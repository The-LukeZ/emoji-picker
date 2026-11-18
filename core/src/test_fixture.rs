//! Small hand-built emoji table shared by tests in this crate.

use crate::table::{Category, Emoji, EmojiTable, PageKey, SpriteRef, Variant};

const TONES: [char; 5] = [
    '\u{1F3FB}',
    '\u{1F3FC}',
    '\u{1F3FD}',
    '\u{1F3FE}',
    '\u{1F3FF}',
];

/// Page layout: 0 = smileys shared, 1..=6 = smileys tone sets, 7 = travel shared.
fn pages() -> Vec<PageKey> {
    let mut pages = vec![PageKey {
        category: Category::SmileysPeople,
        tone_set: 0,
    }];
    for tone_set in 1..=6 {
        pages.push(PageKey {
            category: Category::SmileysPeople,
            tone_set,
        });
    }
    pages.push(PageKey {
        category: Category::TravelPlaces,
        tone_set: 0,
    });
    pages
}

#[allow(clippy::too_many_arguments)]
fn plain(
    order: u32,
    hexcode: &str,
    glyph: &str,
    category: Category,
    name: &str,
    keywords: &[&str],
    shortcodes: &[&str],
    sprite: SpriteRef,
) -> Emoji {
    Emoji {
        hexcode: hexcode.to_string(),
        glyph: glyph.to_string(),
        category,
        order,
        name: name.to_string(),
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        shortcodes: shortcodes.iter().map(|s| s.to_string()).collect(),
        sprite,
        tones: None,
    }
}

/// Tone-capable emoji. Base sprite lives in tone set 1 (page 1), tone `i` in page `2 + i`.
fn toned(mut e: Emoji, cell: u16, tone_glyph: impl Fn(char) -> String) -> Emoji {
    e.sprite = SpriteRef { page: 1, cell };
    let variants: Vec<Variant> = TONES
        .iter()
        .enumerate()
        .map(|(i, t)| Variant {
            glyph: tone_glyph(*t),
            sprite: SpriteRef {
                page: 2 + i as u16,
                cell,
            },
        })
        .collect();
    e.tones = Some(Box::new(
        <[Variant; 5]>::try_from(variants).expect("five tones"),
    ));
    e
}

pub fn fixture() -> EmojiTable {
    use Category::{SmileysPeople as S, TravelPlaces as T};
    let shared = |cell| SpriteRef { page: 0, cell };

    let thumbs_up = toned(
        plain(
            3,
            "1F44D",
            "👍",
            S,
            "thumbs up",
            &[],
            &["+1", "thumbsup"],
            shared(0),
        ),
        0,
        |t| format!("👍{t}"),
    );
    let thumbs_down = toned(
        plain(
            4,
            "1F44E",
            "👎",
            S,
            "thumbs down",
            &[],
            &["-1", "thumbsdown"],
            shared(0),
        ),
        1,
        |t| format!("👎{t}"),
    );
    let holding_hands = toned(
        plain(
            7,
            "1F9D1-200D-1F91D-200D-1F9D1",
            "🧑\u{200D}🤝\u{200D}🧑",
            S,
            "people holding hands",
            &[],
            &[],
            shared(0),
        ),
        2,
        |t| format!("🧑{t}\u{200D}🤝\u{200D}🧑{t}"),
    );

    EmojiTable {
        emojis: vec![
            plain(
                0,
                "1F600",
                "😀",
                S,
                "grinning face",
                &[],
                &["grinning"],
                shared(0),
            ),
            plain(
                1,
                "1F602",
                "😂",
                S,
                "face with tears of joy",
                &[],
                &["joy"],
                shared(1),
            ),
            plain(
                2,
                "1F639",
                "😹",
                S,
                "cat with tears of joy",
                &[],
                &["joy_cat"],
                shared(2),
            ),
            thumbs_up,
            thumbs_down,
            plain(
                5,
                "2764-FE0F",
                "❤\u{FE0F}",
                S,
                "red heart",
                &["love"],
                &["heart"],
                shared(3),
            ),
            plain(
                6,
                "1F525",
                "🔥",
                T,
                "fire",
                &[],
                &["fire"],
                SpriteRef { page: 7, cell: 0 },
            ),
            holding_hands,
        ],
        pages: pages(),
    }
}
