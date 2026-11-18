//! The emoji table and atlas pages, embedded in the executable.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::OnceLock;

use emoji_core::table::{cell_rect, Bundle, SpriteRef, ATLAS_SIZES};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

static BUNDLE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/generated/emoji.bin"
));

/// The embedded bundle, decoded once on first use (checks `BUNDLE_FORMAT`).
pub fn bundle() -> &'static Bundle<'static> {
    static BUNDLE: OnceLock<Bundle<'static>> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        Bundle::decode(BUNDLE_BYTES)
            .expect("embedded emoji.bin is invalid: rerun `cargo xtask assets`")
    })
}

/// Decodes atlas page `page` at cell size `atlas_size` into RGBA pixels.
/// `None` (logged) when the size is unknown or the PNG is not 8-bit RGBA.
pub fn decode_page(page: u16, atlas_size: u16) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let size_index = ATLAS_SIZES.iter().position(|&s| s == atlas_size)?;
    let bytes: &[u8] = bundle().pngs.get(usize::from(page))?.get(size_index)?;
    let decoded = (|| {
        let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
        if reader.output_color_type() != (png::ColorType::Rgba, png::BitDepth::Eight) {
            return None;
        }
        let (w, h) = (reader.info().width, reader.info().height);
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        reader.next_frame(buf.make_mut_bytes()).ok()?;
        Some(buf)
    })();
    if decoded.is_none() {
        crate::log!("atlas page {page} size {atlas_size}: decode failed");
    }
    decoded
}

/// Decoded atlas pages, kept while the picker is open and dropped on hide.
#[derive(Default)]
pub struct AtlasCache {
    pages: HashMap<(u16, u16), Image>,
}

impl AtlasCache {
    /// The page as a Slint image (decoded on first use, then shared).
    pub fn image(&mut self, page: u16, atlas_size: u16) -> Image {
        self.pages
            .entry((page, atlas_size))
            .or_insert_with(|| {
                decode_page(page, atlas_size)
                    .map(Image::from_rgba8)
                    .unwrap_or_default()
            })
            .clone()
    }

    /// Drops every cached page. Images still referenced by the UI stay alive until released there.
    pub fn clear(&mut self) {
        self.pages.clear();
    }

    /// Number of cached pages (for the debug log).
    pub fn len(&self) -> usize {
        self.pages.len()
    }
}

/// Copies `sprites` side by side into one small image (same 1 px padding as the atlas), so a
/// few icons do not keep whole pages alive. Cell `i` is at `cell_rect(i, atlas_size)`.
pub fn sprite_strip(sprites: &[SpriteRef], atlas_size: u16) -> Image {
    let n = sprites.len() as u32;
    let stride = u32::from(atlas_size) + 2;
    let mut strip = SharedPixelBuffer::<Rgba8Pixel>::new(n.max(1) * stride, stride);
    let strip_w = strip.width() as usize;
    let mut order: Vec<usize> = (0..sprites.len()).collect();
    order.sort_by_key(|&i| sprites[i].page);
    let mut current: Option<(u16, SharedPixelBuffer<Rgba8Pixel>)> = None;
    for i in order {
        let sprite = sprites[i];
        if current.as_ref().map(|(p, _)| *p) != Some(sprite.page) {
            // Drop the previous page first so only one decoded page is alive at a time.
            drop(current.take());
            current = decode_page(sprite.page, atlas_size).map(|b| (sprite.page, b));
        }
        let Some((_, page)) = &current else { continue };
        let page_w = page.width() as usize;
        let (sx, sy, w, h) = cell_rect(sprite.cell, atlas_size);
        let (dx, dy) = (i as u32 * stride + 1, 1u32);
        let src = page.as_slice();
        let dst = strip.make_mut_slice();
        for row in 0..h as usize {
            let s = (sy as usize + row) * page_w + sx as usize;
            let d = (dy as usize + row) * strip_w + dx as usize;
            if s + w as usize <= src.len() && d + w as usize <= dst.len() {
                dst[d..d + w as usize].copy_from_slice(&src[s..s + w as usize]);
            }
        }
    }
    Image::from_rgba8(strip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bundle_decodes() {
        assert!(bundle().table.emojis.len() > 1500);
    }

    #[test]
    fn pages_decode_at_every_size() {
        for size in ATLAS_SIZES {
            let page = decode_page(0, size).expect("page 0 decodes");
            assert_eq!(page.width(), 32 * (u32::from(size) + 2));
        }
        assert!(decode_page(0, 33).is_none());
        assert!(decode_page(u16::MAX, 32).is_none());
    }

    #[test]
    fn cache_shares_and_clears() {
        let mut cache = AtlasCache::default();
        let a = cache.image(0, 24);
        let b = cache.image(0, 24);
        assert_eq!(a.size(), b.size());
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn strip_copies_cells() {
        let table = &bundle().table;
        let fire = &table.emojis[table.by_hexcode("1F525").unwrap() as usize];
        let strip = sprite_strip(&[fire.sprite, fire.sprite], 24);
        assert_eq!(strip.size().width, 2 * 26);
        let px = strip.to_rgba8().unwrap();
        // The fire sprite is opaque somewhere in its cell; the padding row stays transparent.
        assert!(px.as_slice().iter().any(|p| p.a > 0));
        assert!(px.as_slice()[..px.width() as usize]
            .iter()
            .all(|p| p.a == 0));
    }
}
