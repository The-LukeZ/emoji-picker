//! English search over the emoji table with tiered ranking.

use crate::recents::Recents;
use crate::table::EmojiTable;

/// Precomputed lowercase search strings for one emoji.
struct Entry {
    name: String,
    name_words: Vec<String>,
    keywords: Vec<String>,
    shortcodes: Vec<String>,
}

impl Entry {
    /// All searchable strings: name, keywords, shortcodes.
    fn fields(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str())
            .chain(self.keywords.iter().map(String::as_str))
            .chain(self.shortcodes.iter().map(String::as_str))
    }

    /// Strings a term may be a prefix of: name words, keywords, shortcodes.
    fn prefix_targets(&self) -> impl Iterator<Item = &str> {
        self.name_words
            .iter()
            .chain(&self.keywords)
            .chain(&self.shortcodes)
            .map(String::as_str)
    }

    /// Match tier (0 is best), or `None` when some term does not match.
    fn tier(&self, whole: &str, terms: &[&str]) -> Option<u8> {
        if !terms.iter().all(|t| self.fields().any(|f| f.contains(t))) {
            return None;
        }
        if self.name == whole || self.shortcodes.iter().any(|s| s == whole) {
            return Some(0);
        }
        if self.name.starts_with(whole) {
            return Some(1);
        }
        if self.shortcodes.iter().any(|s| s.starts_with(whole)) {
            return Some(2);
        }
        if terms
            .iter()
            .all(|t| self.prefix_targets().any(|f| f.starts_with(t)))
        {
            return Some(3);
        }
        Some(4)
    }
}

/// Lowercase, drop surrounding whitespace and `:` (so `:joy:` works), collapse spaces.
fn normalize(query: &str) -> String {
    let lower = query.to_lowercase();
    let trimmed = lower.trim_matches(|c: char| c == ':' || c.is_whitespace());
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub struct SearchIndex {
    entries: Vec<Entry>,
}

impl SearchIndex {
    pub fn new(table: &EmojiTable) -> SearchIndex {
        let entries = table
            .emojis
            .iter()
            .map(|e| {
                let name = e.name.to_lowercase();
                let name_words = name.split_whitespace().map(str::to_string).collect();
                Entry {
                    name,
                    name_words,
                    keywords: e.keywords.iter().map(|k| k.to_lowercase()).collect(),
                    shortcodes: e.shortcodes.iter().map(|s| s.to_lowercase()).collect(),
                }
            })
            .collect();
        SearchIndex { entries }
    }

    /// Matching emoji indices, best first. `None` for a blank query (show categories).
    ///
    /// Sort key is `(tier, not in recents, order)`: recents only break ties within a tier.
    pub fn search(&self, query: &str, recents: &Recents, table: &EmojiTable) -> Option<Vec<u32>> {
        let whole = normalize(query);
        if whole.is_empty() {
            return None;
        }
        let terms: Vec<&str> = whole.split(' ').collect();

        let mut hits: Vec<(u8, bool, u32, u32)> = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(tier) = entry.tier(&whole, &terms) {
                let e = &table.emojis[i];
                hits.push((tier, recents.rank(&e.hexcode).is_none(), e.order, i as u32));
            }
        }
        hits.sort_unstable();
        Some(hits.into_iter().map(|(_, _, _, i)| i).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::fixture;

    fn s(q: &str) -> Option<Vec<u32>> {
        s_with(q, &Recents::default())
    }

    fn s_with(q: &str, recents: &Recents) -> Option<Vec<u32>> {
        let t = fixture();
        SearchIndex::new(&t).search(q, recents, &t)
    }

    fn glyphs(v: Option<Vec<u32>>) -> Vec<String> {
        let t = fixture();
        v.expect("query is not blank")
            .into_iter()
            .map(|i| t.emojis[i as usize].glyph.clone())
            .collect()
    }

    #[test]
    fn exact_shortcode_first() {
        assert_eq!(glyphs(s("joy")), ["😂", "😹"]);
    }

    #[test]
    fn colons_and_spaces_ignored() {
        assert_eq!(s(":joy:"), s("joy"));
        assert_eq!(s("  :joy:  "), s("joy"));
        assert_eq!(s(" : joy : "), s("joy"));
        assert_eq!(glyphs(s("  thumbs   up ")), ["👍"]);
    }

    #[test]
    fn name_prefix_ties_by_order() {
        assert_eq!(glyphs(s("thumbs")), ["👍", "👎"]);
    }

    #[test]
    fn recents_boost_within_tier() {
        let mut r = Recents::default();
        r.push("1F44E");
        assert_eq!(glyphs(s_with("thumbs", &r)), ["👎", "👍"]);
    }

    #[test]
    fn recents_do_not_cross_tiers() {
        let mut r = Recents::default();
        r.push("1F639");
        assert_eq!(glyphs(s_with("joy", &r)), ["😂", "😹"]);
    }

    #[test]
    fn all_terms_must_match() {
        assert_eq!(glyphs(s("heart red")), ["❤\u{FE0F}"]);
        assert!(s("heart fire").unwrap().is_empty());
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(glyphs(s("FIRE")), ["🔥"]);
    }

    #[test]
    fn keywords_match() {
        assert!(glyphs(s("love")).contains(&"❤\u{FE0F}".to_string()));
    }

    #[test]
    fn blank_query_is_none() {
        assert!(s("   ").is_none());
        assert!(s("").is_none());
        assert!(s(" :: ").is_none());
    }

    #[test]
    fn no_match_is_empty() {
        assert!(s("xyzzy").unwrap().is_empty());
    }

    #[test]
    fn substring_ranks_after_prefix() {
        // "hum" is only a substring of "thumbs ..." (tier 4); "ire" of "fire".
        assert_eq!(glyphs(s("umb")), ["👍", "👎"]);
        assert_eq!(glyphs(s("ire")), ["🔥"]);
    }
}
