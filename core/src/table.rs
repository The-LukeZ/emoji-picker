//! Emoji table types and the asset bundle format shared by the xtask builder and the app.

use serde::{Deserialize, Serialize};
use serde_bytes::Bytes;
use std::fmt;

/// Picker tab. People are merged into the smileys tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    SmileysPeople,
    AnimalsNature,
    FoodDrink,
    Activities,
    TravelPlaces,
    Objects,
    Symbols,
    Flags,
}

impl Category {
    /// Tab order: smileys, animals, food, activity, travel, objects, symbols, flags.
    pub const ALL: [Category; 8] = [
        Category::SmileysPeople,
        Category::AnimalsNature,
        Category::FoodDrink,
        Category::Activities,
        Category::TravelPlaces,
        Category::Objects,
        Category::Symbols,
        Category::Flags,
    ];

    /// Maps an Emojibase `group` number to a tab. Group 2 (components) and unknown groups give `None`.
    pub fn from_emojibase_group(g: u8) -> Option<Category> {
        match g {
            0 | 1 => Some(Category::SmileysPeople),
            3 => Some(Category::AnimalsNature),
            4 => Some(Category::FoodDrink),
            5 => Some(Category::TravelPlaces),
            6 => Some(Category::Activities),
            7 => Some(Category::Objects),
            8 => Some(Category::Symbols),
            9 => Some(Category::Flags),
            _ => None,
        }
    }
}

/// One sprite page: a category plus a tone set.
/// `tone_set` 0 is shared (not tone-capable); 1..=6 is tone 0..=5 of tone-capable emoji.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageKey {
    pub category: Category,
    pub tone_set: u8,
}

/// Location of an emoji sprite. `page` indexes `EmojiTable::pages`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpriteRef {
    pub page: u16,
    pub cell: u16,
}

/// A skin tone variant of a tone-capable emoji.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    pub glyph: String,
    pub sprite: SpriteRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Emoji {
    pub hexcode: String,
    pub glyph: String,
    pub category: Category,
    pub order: u32,
    pub name: String,
    pub keywords: Vec<String>,
    pub shortcodes: Vec<String>,
    pub sprite: SpriteRef,
    /// Tones 1..=5 (light to dark) when the emoji is tone-capable.
    pub tones: Option<Box<[Variant; 5]>>,
}

impl Emoji {
    /// Glyph and sprite for `tone`. Tone 0, or an emoji without tones, gives the base.
    pub fn variant(&self, tone: u8) -> (&str, SpriteRef) {
        match (&self.tones, tone) {
            (Some(tones), 1..=5) => {
                let v = &tones[usize::from(tone) - 1];
                (v.glyph.as_str(), v.sprite)
            }
            _ => (self.glyph.as_str(), self.sprite),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiTable {
    /// In Emojibase order.
    pub emojis: Vec<Emoji>,
    pub pages: Vec<PageKey>,
}

impl EmojiTable {
    pub fn by_hexcode(&self, hex: &str) -> Option<u32> {
        self.emojis
            .iter()
            .position(|e| e.hexcode == hex)
            .map(|i| i as u32)
    }

    /// Indices into `emojis` of the given category, in Emojibase order.
    pub fn in_category(&self, c: Category) -> impl Iterator<Item = u32> + '_ {
        self.emojis
            .iter()
            .enumerate()
            .filter(move |(_, e)| e.category == c)
            .map(|(i, _)| i as u32)
    }
}

/// Atlas cell sizes in pixels; `Bundle::pngs[page][size_index]` follows this order.
pub const ATLAS_SIZES: [u16; 4] = [24, 32, 40, 48];
/// Cells per atlas row.
pub const ATLAS_COLS: u16 = 32;

/// Pixel rect `(x, y, w, h)` of `cell` in an atlas of cell size `size`, with 1 px padding.
pub fn cell_rect(cell: u16, size: u16) -> (u32, u32, u32, u32) {
    let stride = u32::from(size) + 2;
    let cols = u32::from(ATLAS_COLS);
    let cell = u32::from(cell);
    let x = (cell % cols) * stride + 1;
    let y = (cell / cols) * stride + 1;
    (x, y, u32::from(size), u32::from(size))
}

pub const BUNDLE_FORMAT: u32 = 1;

#[derive(Debug)]
pub enum BundleError {
    Format { found: u32, expected: u32 },
    Decode(postcard::Error),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::Format { found, expected } => {
                write!(f, "unsupported bundle format {found}, expected {expected}")
            }
            BundleError::Decode(e) => write!(f, "bundle decode failed: {e}"),
        }
    }
}

impl std::error::Error for BundleError {}

impl From<postcard::Error> for BundleError {
    fn from(e: postcard::Error) -> Self {
        BundleError::Decode(e)
    }
}

/// Emoji table plus atlas PNGs. `pngs[page][size_index]` indexes `ATLAS_SIZES`.
/// PNG bytes borrow from the input buffer on decode, so an embedded bundle is not copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle<'a> {
    pub format: u32,
    pub table: EmojiTable,
    #[serde(borrow)]
    pub pngs: Vec<Vec<&'a Bytes>>,
}

impl<'a> Bundle<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Bundle<'a>, BundleError> {
        let b: Bundle<'a> = postcard::from_bytes(bytes)?;
        if b.format != BUNDLE_FORMAT {
            return Err(BundleError::Format {
                found: b.format,
                expected: BUNDLE_FORMAT,
            });
        }
        Ok(b)
    }

    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("bundle serialization is infallible for in-memory data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::fixture;
    use serde_bytes::Bytes;

    #[test]
    fn cell_rect_has_padding() {
        assert_eq!(cell_rect(0, 32), (1, 1, 32, 32));
        assert_eq!(cell_rect(33, 32), (35, 35, 32, 32));
    }

    #[test]
    fn bundle_roundtrip_borrows() {
        let png = vec![1u8, 2, 3];
        let b = Bundle {
            format: BUNDLE_FORMAT,
            table: fixture(),
            pngs: vec![vec![Bytes::new(&png); 4]],
        };
        let bytes = b.encode();
        let d = Bundle::decode(&bytes).unwrap();
        assert_eq!(d.table.emojis.len(), 8);
        assert_eq!(d.table, b.table);
        assert_eq!(&d.pngs[0][3][..], &[1, 2, 3]);
        // Borrowed, not copied: the slice points into the encoded buffer.
        let p = d.pngs[0][3].as_ptr() as usize;
        let start = bytes.as_ptr() as usize;
        assert!(p >= start && p < start + bytes.len());
    }

    #[test]
    fn bundle_rejects_other_format() {
        let b = Bundle {
            format: 99,
            table: fixture(),
            pngs: vec![],
        };
        let bytes = b.encode();
        assert!(matches!(
            Bundle::decode(&bytes),
            Err(BundleError::Format { found: 99, .. })
        ));
    }

    #[test]
    fn bundle_rejects_garbage() {
        assert!(Bundle::decode(&[0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn variant_falls_back_to_base() {
        let t = fixture();
        let fire = &t.emojis[t.by_hexcode("1F525").unwrap() as usize];
        assert_eq!(fire.variant(3).0, "🔥");
        let up = &t.emojis[t.by_hexcode("1F44D").unwrap() as usize];
        assert_eq!(up.variant(2).0, "👍🏼");
        assert_eq!(up.variant(0).0, "👍");
    }

    #[test]
    fn emojibase_group_mapping() {
        assert_eq!(
            Category::from_emojibase_group(1),
            Some(Category::SmileysPeople)
        );
        assert_eq!(Category::from_emojibase_group(2), None);
    }

    #[test]
    fn in_category_is_ordered() {
        let t = fixture();
        let travel: Vec<u32> = t.in_category(Category::TravelPlaces).collect();
        assert_eq!(travel, vec![6]);
        assert_eq!(t.in_category(Category::SmileysPeople).count(), 7);
    }
}
