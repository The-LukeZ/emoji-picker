//! Rasterizes Twemoji SVGs into PNG atlas pages and writes the asset bundle and app icon.

use crate::data::Built;
use anyhow::{bail, Context, Result};
use emoji_core::table::{cell_rect, Bundle, ATLAS_COLS, ATLAS_SIZES, BUNDLE_FORMAT};
use rayon::prelude::*;
use resvg::{tiny_skia, usvg};
use serde_bytes::Bytes;
use std::path::Path;

/// The bundle must stay below this size.
const MAX_BUNDLE_BYTES: usize = 30 * 1024 * 1024;
/// Icon sizes written to `app.ico`.
const ICON_SIZES: [u16; 5] = [16, 24, 32, 48, 256];
/// Twemoji stem of the app icon.
const ICON_STEM: &str = "1f600";

/// Rasterizes one SVG to a `size` x `size` pixmap.
fn rasterize(svg: &[u8], size: u16) -> Result<tiny_skia::Pixmap> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default()).context("parsing SVG")?;
    let mut pixmap =
        tiny_skia::Pixmap::new(u32::from(size), u32::from(size)).context("allocating pixmap")?;
    let target = f32::from(size);
    let transform = tiny_skia::Transform::from_scale(
        target / tree.size().width(),
        target / tree.size().height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

/// One atlas page as PNG bytes: `cells` are `(cell index, svg)` pairs drawn at `cell_rect`.
pub fn render_page(cells: &[(u16, &[u8])], size: u16) -> Result<Vec<u8>> {
    let stride = u32::from(size) + 2;
    let rows = cells
        .iter()
        .map(|(c, _)| u32::from(*c) / u32::from(ATLAS_COLS) + 1)
        .max()
        .unwrap_or(1);
    let width = u32::from(ATLAS_COLS) * stride;
    let height = rows * stride;
    let rendered: Vec<(u16, tiny_skia::Pixmap)> = cells
        .par_iter()
        .map(|(cell, svg)| Ok((*cell, rasterize(svg, size)?)))
        .collect::<Result<_>>()?;

    let mut rgba = vec![0u8; usize::try_from(width * height * 4)?];
    for (cell, pixmap) in &rendered {
        let (x0, y0, w, _) = cell_rect(*cell, size);
        for (i, px) in pixmap.pixels().iter().enumerate() {
            let i = u32::try_from(i)?;
            let c = px.demultiply();
            let at = usize::try_from(((y0 + i / w) * width + x0 + i % w) * 4)?;
            rgba[at..at + 4].copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
    }
    encode_png(&rgba, width, height)
}

/// `app.ico` with PNG-compressed entries for `ICON_SIZES`.
fn build_ico(svg: &[u8]) -> Result<Vec<u8>> {
    let pngs: Vec<Vec<u8>> = ICON_SIZES
        .iter()
        .map(|&s| {
            let p = rasterize(svg, s)?;
            let rgba: Vec<u8> = p
                .pixels()
                .iter()
                .flat_map(|px| {
                    let c = px.demultiply();
                    [c.red(), c.green(), c.blue(), c.alpha()]
                })
                .collect();
            encode_png(&rgba, u32::from(s), u32::from(s))
        })
        .collect::<Result<_>>()?;
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&u16::try_from(pngs.len())?.to_le_bytes());
    let mut offset = 6 + 16 * u32::try_from(pngs.len())?;
    for (&s, data) in ICON_SIZES.iter().zip(&pngs) {
        // A width or height byte of 0 means 256.
        let dim = u8::try_from(s).unwrap_or(0);
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&u32::try_from(data.len())?.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += u32::try_from(data.len())?;
    }
    for data in &pngs {
        out.extend_from_slice(data);
    }
    Ok(out)
}

/// Writes through a temporary file so a failed run never leaves a partial output.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))
}

/// Renders every page at every atlas size, then writes `emoji.bin` and `app.ico` into `out_dir`.
pub fn write_assets(
    built: &Built,
    twemoji: &std::collections::HashMap<String, Vec<u8>>,
    out_dir: &Path,
) -> Result<()> {
    let pages = built.table.pages.len();
    let mut by_page: Vec<Vec<(u16, &[u8])>> = vec![Vec::new(); pages];
    for (sprite, svg) in &built.svgs {
        by_page[usize::from(sprite.page)].push((sprite.cell, svg));
    }
    let jobs: Vec<(usize, usize)> = (0..pages)
        .flat_map(|p| (0..ATLAS_SIZES.len()).map(move |s| (p, s)))
        .collect();
    let rendered: Vec<Vec<u8>> = jobs
        .par_iter()
        .map(|&(p, s)| render_page(&by_page[p], ATLAS_SIZES[s]))
        .collect::<Result<_>>()?;

    let sizes = ATLAS_SIZES.len();
    let pngs: Vec<Vec<&Bytes>> = (0..pages)
        .map(|p| {
            (0..sizes)
                .map(|s| Bytes::new(&rendered[p * sizes + s]))
                .collect()
        })
        .collect();
    let bundle = Bundle {
        format: BUNDLE_FORMAT,
        table: built.table.clone(),
        pngs,
    };
    let bytes = bundle.encode();
    if bytes.len() > MAX_BUNDLE_BYTES {
        bail!(
            "bundle is {} bytes, over the {MAX_BUNDLE_BYTES} byte limit",
            bytes.len()
        );
    }

    let icon_svg = twemoji
        .get(ICON_STEM)
        .with_context(|| format!("Twemoji {ICON_STEM} missing for the app icon"))?;
    let ico = build_ico(icon_svg)?;

    std::fs::create_dir_all(out_dir)?;
    write_atomic(&out_dir.join("emoji.bin"), &bytes)?;
    write_atomic(&out_dir.join("app.ico"), &ico)?;
    println!(
        "bundle: {} bytes ({:.2} MB), {} atlas PNGs",
        bytes.len(),
        bytes.len() as f64 / (1024.0 * 1024.0),
        rendered.len()
    );
    println!("icon: {} bytes", ico.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Img {
        width: u32,
        height: u32,
        data: Vec<u8>,
    }

    impl Img {
        fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
            let at = ((y * self.width + x) * 4) as usize;
            self.data[at..at + 4].try_into().unwrap()
        }
    }

    fn decode(png: &[u8]) -> Img {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut data).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        Img {
            width: info.width,
            height: info.height,
            data,
        }
    }

    const RED: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36"><rect width="36" height="36" fill="#f00"/></svg>"##;

    #[test]
    fn page_dimensions_and_padding() {
        let png = render_page(&[(0, RED), (33, RED)], 32).unwrap();
        let img = decode(&png);
        assert_eq!((img.width, img.height), (32 * 34, 2 * 34));
        assert_eq!(img.pixel(0, 0)[3], 0); // padding transparent
        assert_eq!(img.pixel(1, 1), [255, 0, 0, 255]); // straight alpha, opaque red
        assert_eq!(img.pixel(35, 35), [255, 0, 0, 255]);
    }

    #[test]
    fn half_transparent_pixels_are_straight_alpha() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36"><rect width="36" height="36" fill="#ff0000" fill-opacity="0.5"/></svg>"##;
        let img = decode(&render_page(&[(0, &svg[..])], 24).unwrap());
        let p = img.pixel(1, 1);
        assert_eq!(p[0], 255, "red must not be premultiplied: {p:?}");
        assert!((126..=129).contains(&p[3]), "{p:?}");
    }

    #[test]
    fn ico_has_five_png_entries() {
        let ico = build_ico(RED).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 5, 0]);
        // Entry 4 is 256 px, stored as 0.
        assert_eq!(ico[6 + 16 * 4], 0);
        assert_eq!(ico[6], 16);
        let off = u32::from_le_bytes(ico[6 + 12..6 + 16].try_into().unwrap()) as usize;
        let img = decode(&ico[off..]);
        assert_eq!((img.width, img.height), (16, 16));
    }
}
