mod atlas;
mod data;
mod fetch;

use anyhow::{bail, Result};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("assets") => assets(),
        _ => bail!("usage: cargo xtask assets"),
    }
}

/// Fetches pinned sources, builds the emoji table, and writes `assets/generated`.
fn assets() -> Result<()> {
    let started = Instant::now();
    let svgs = fetch::twemoji_svgs()?;
    let data = fetch::emojibase("en/data.json")?;
    let github = fetch::emojibase("en/shortcodes/github.json")?;
    let iamcal = fetch::emojibase("en/shortcodes/iamcal.json")?;
    let (built, missing) = data::build_table_with_missing(&data, &github, &iamcal, &svgs)?;
    println!("emoji: {}", built.table.emojis.len());
    println!("pages: {}", built.table.pages.len());
    println!("sprites: {}", built.svgs.len());
    println!("missing ({}): {}", missing.len(), missing.join(" "));
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/generated");
    atlas::write_assets(&built, &svgs, &out_dir)?;
    println!("done in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}
