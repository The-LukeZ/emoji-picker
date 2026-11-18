//! The emoji table and atlas pages, embedded in the executable.

use std::sync::OnceLock;

use emoji_core::table::Bundle;

static BUNDLE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/generated/emoji.bin"
));

/// The embedded bundle, decoded once on first use (checks `BUNDLE_FORMAT`).
#[cfg_attr(not(test), allow(dead_code))] // first used by the picker window (Task 12)
pub fn bundle() -> &'static Bundle<'static> {
    static BUNDLE: OnceLock<Bundle<'static>> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        Bundle::decode(BUNDLE_BYTES)
            .expect("embedded emoji.bin is invalid: rerun `cargo xtask assets`")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_bundle_decodes() {
        assert!(bundle().table.emojis.len() > 1500);
    }
}
