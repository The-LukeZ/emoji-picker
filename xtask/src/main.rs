mod data;
mod fetch;

use anyhow::{bail, Result};

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("assets") => assets(),
        _ => bail!("usage: cargo xtask assets"),
    }
}

/// Fetches pinned sources and builds the emoji table. Atlas output comes later.
fn assets() -> Result<()> {
    let svgs = fetch::twemoji_svgs()?;
    let data = fetch::emojibase("en/data.json")?;
    let github = fetch::emojibase("en/shortcodes/github.json")?;
    let iamcal = fetch::emojibase("en/shortcodes/iamcal.json")?;
    let (built, missing) = data::build_table_with_missing(&data, &github, &iamcal, &svgs)?;
    println!("emoji: {}", built.table.emojis.len());
    println!("pages: {}", built.table.pages.len());
    println!("sprites: {}", built.svgs.len());
    println!("missing ({}): {}", missing.len(), missing.join(" "));
    Ok(())
}
