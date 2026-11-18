//! Needs `cargo xtask assets` output; run with `cargo test -p emoji-core --release -- --ignored`.

use emoji_core::recents::Recents;
use emoji_core::search::SearchIndex;
use emoji_core::table::Bundle;
use std::time::Instant;

#[test]
#[ignore]
fn search_under_5ms_on_real_data() {
    let bytes = std::fs::read("../assets/generated/emoji.bin").unwrap();
    let b = Bundle::decode(&bytes).unwrap();
    let idx = SearchIndex::new(&b.table);
    for q in ["s", "sm", "smile", "face with", "heart"] {
        let t = Instant::now();
        idx.search(q, &Recents::default(), &b.table);
        let elapsed = t.elapsed();
        eprintln!("search {q:?}: {elapsed:?}");
        assert!(elapsed.as_millis() < 5, "{q}");
    }
}
