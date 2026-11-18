//! Pinned downloads with a SHA-256 check and an on-disk cache.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Latest jdecked/twemoji release when pinned (Twemoji 17.0.3, Emoji 17.0).
pub const TWEMOJI_TAG: &str = "v17.0.3";
/// `emojibase-data` release for Unicode Emoji 17.0, matching the Twemoji release above.
pub const EMOJIBASE_VERSION: &str = "17.0.0";

const TWEMOJI_SHA256: &str = "a0855654b633045ae2337537e77f1bb4361162f7fcd910e613eaab1d6d9c5fca";
const EMOJIBASE_SHA256: &[(&str, &str)] = &[
    (
        "en/data.json",
        "ed014f1049bd370c5794f815850156196ac382850f51c3e9f6a9e83553fb3f01",
    ),
    (
        "en/shortcodes/github.json",
        "d6bb7101e7bab1e52b30f4b3562da2319b484a6266352a1119f0d1ba52db23db",
    ),
    (
        "en/shortcodes/iamcal.json",
        "b1952bd90cb8580224434eaf8f1911265d23a78c7ed36f39f8590c90847443eb",
    ),
];

const USER_AGENT: &str = "emoji-picker-xtask";
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

fn cache_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/xtask-cache")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn download(url: &str) -> Result<Vec<u8>> {
    let mut resp = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?;
    resp.body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .with_context(|| format!("reading body of {url}"))
}

/// Returns the bytes for `url`, from the cache when its hash matches, else downloaded and verified.
fn cached(cache_name: &str, url: &str, sha256: &str) -> Result<Vec<u8>> {
    let path = cache_dir().join(cache_name);
    if let Ok(bytes) = std::fs::read(&path) {
        if sha256_hex(&bytes) == sha256 {
            return Ok(bytes);
        }
    }
    eprintln!("downloading {url}");
    let bytes = download(url)?;
    let got = sha256_hex(&bytes);
    if got != sha256 {
        bail!("SHA-256 mismatch for {url}: expected {sha256}, got {got}");
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(bytes)
}

/// File stem to SVG bytes for every `*/assets/svg/*.svg` in the pinned Twemoji release.
pub fn twemoji_svgs() -> Result<HashMap<String, Vec<u8>>> {
    let url = format!("https://github.com/jdecked/twemoji/archive/refs/tags/{TWEMOJI_TAG}.tar.gz");
    let name = format!("twemoji-{TWEMOJI_TAG}.tar.gz");
    let bytes = cached(&name, &url, TWEMOJI_SHA256)?;
    svgs_from_targz(&bytes)
}

fn svgs_from_targz(bytes: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut out = HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let parts: Vec<_> = path.components().collect();
        let in_svg_dir =
            parts.len() == 4 && parts[1].as_os_str() == "assets" && parts[2].as_os_str() == "svg";
        if !in_svg_dir || path.extension().and_then(|e| e.to_str()) != Some("svg") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let mut svg = Vec::new();
        entry.read_to_end(&mut svg)?;
        out.insert(stem, svg);
    }
    if out.is_empty() {
        bail!("no SVG files found in the Twemoji archive");
    }
    Ok(out)
}

/// One pinned Emojibase file, e.g. `en/data.json`.
pub fn emojibase(file: &str) -> Result<Vec<u8>> {
    let Some((_, sha)) = EMOJIBASE_SHA256.iter().find(|(f, _)| *f == file) else {
        bail!("no pinned hash for emojibase file {file}");
    };
    let url = format!("https://cdn.jsdelivr.net/npm/emojibase-data@{EMOJIBASE_VERSION}/{file}");
    let name = format!("emojibase-{EMOJIBASE_VERSION}-{}", file.replace('/', "_"));
    cached(&name, &url, sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn extracts_only_asset_svgs() {
        let gz = targz(&[
            ("twemoji-1/assets/svg/1f600.svg", b"<svg a/>"),
            ("twemoji-1/assets/svg/23-20e3.svg", b"<svg b/>"),
            ("twemoji-1/assets/72x72/1f600.png", b"png"),
            ("twemoji-1/assets/svg/readme.txt", b"txt"),
            ("twemoji-1/other/svg/1f601.svg", b"no"),
        ]);
        let m = svgs_from_targz(&gz).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["1f600"], b"<svg a/>");
        assert!(m.contains_key("23-20e3"));
    }

    #[test]
    fn hash_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
