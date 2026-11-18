//! Recently used emoji, stored one base hexcode per line, newest first.

use std::fs;
use std::io;
use std::path::Path;

use crate::config::write_atomic;

/// Maximum number of remembered emoji.
pub const RECENTS_CAP: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recents {
    /// Base hexcodes, newest first.
    pub items: Vec<String>,
}

impl Recents {
    /// Record a use: move `hex` to the front (or add it) and enforce the cap.
    pub fn push(&mut self, hex: &str) {
        self.items.retain(|h| h != hex);
        self.items.insert(0, hex.to_string());
        self.items.truncate(RECENTS_CAP);
    }

    /// Load recents. A missing or unreadable file gives an empty list.
    pub fn load(path: &Path) -> Recents {
        let Ok(text) = fs::read_to_string(path) else {
            return Recents::default();
        };
        let mut items: Vec<String> = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if !items.iter().any(|h| h == line) {
                items.push(line.to_string());
            }
        }
        items.truncate(RECENTS_CAP);
        Recents { items }
    }

    /// Save recents, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut text = self.items.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        write_atomic(path, &text)
    }

    /// Position of `hex` in the list (0 is the most recent).
    pub fn rank(&self, hex: &str) -> Option<usize> {
        self.items.iter().position(|h| h == hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recents_move_to_front_and_cap() {
        let mut r = Recents::default();
        for i in 0..40 {
            r.push(&format!("{i:X}"));
        }
        r.push("5");
        assert_eq!(r.items.len(), 32);
        assert_eq!(r.items[0], "5");
        assert_eq!(r.rank("5"), Some(0));
        assert_eq!(r.rank("0"), None);
    }

    #[test]
    fn push_existing_does_not_duplicate() {
        let mut r = Recents::default();
        r.push("A");
        r.push("B");
        r.push("A");
        assert_eq!(r.items, ["A", "B"]);
        assert_eq!(r.rank("B"), Some(1));
    }

    #[test]
    fn save_then_load_roundtrips_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("recents.txt");
        let mut r = Recents::default();
        r.push("1F600");
        r.push("1F44D");
        r.save(&p).unwrap();
        assert!(!dir.path().join("nested").join("recents.txt.tmp").exists());
        assert_eq!(Recents::load(&p), r);
    }

    #[test]
    fn missing_or_bad_file_gives_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Recents::load(&dir.path().join("recents.txt")),
            Recents::default()
        );
        let p = dir.path().join("bad.txt");
        fs::write(&p, [0xFF, 0xFE, 0x00]).unwrap();
        assert_eq!(Recents::load(&p), Recents::default());
    }

    #[test]
    fn load_skips_blanks_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("recents.txt");
        let mut text = String::from("\n  A  \n\nA\n");
        for i in 0..40 {
            text.push_str(&format!("{i:X}X\n"));
        }
        fs::write(&p, text).unwrap();
        let r = Recents::load(&p);
        assert_eq!(r.items.len(), RECENTS_CAP);
        assert_eq!(r.items[0], "A");
        assert_eq!(r.items[1], "0X");
    }
}
