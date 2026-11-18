//! Turns Emojibase JSON and Twemoji SVGs into the emoji table.

use anyhow::{bail, Context, Result};
use emoji_core::table::{Category, Emoji, EmojiTable, PageKey, SpriteRef, Variant};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// Most emoji allowed to lack a Twemoji SVG before the build fails.
const MAX_MISSING: usize = 20;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Tone {
    One(u8),
    Many(Vec<u8>),
}

#[derive(Debug, Deserialize)]
pub struct RawSkin {
    pub hexcode: String,
    pub emoji: String,
    tone: Tone,
}

#[derive(Debug, Deserialize)]
struct RawEmoji {
    label: String,
    hexcode: String,
    emoji: String,
    group: Option<u8>,
    #[serde(default)]
    order: u32,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    skins: Vec<RawSkin>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

type Shortcodes = HashMap<String, OneOrMany>;

pub struct Built {
    pub table: EmojiTable,
    pub svgs: Vec<(SpriteRef, Vec<u8>)>,
}

/// Twemoji file stem for an Emojibase hexcode, if one exists in `available`.
pub fn twemoji_file(hexcode: &str, available: &HashSet<String>) -> Option<String> {
    let lower = hexcode.to_lowercase();
    let segments: Vec<String> = lower
        .split('-')
        .map(|s| {
            let t = s.trim_start_matches('0');
            if t.is_empty() { "0" } else { t }.to_string()
        })
        .collect();
    let join = |segs: &[&str]| segs.join("-");
    let all: Vec<&str> = segments.iter().map(String::as_str).collect();
    let no_fe0f: Vec<&str> = all.iter().copied().filter(|s| *s != "fe0f").collect();

    let first = if all.contains(&"200d") {
        join(&all)
    } else {
        join(&no_fe0f)
    };
    [first, join(&no_fe0f), join(&all)]
        .into_iter()
        .find(|c| available.contains(c))
}

/// The skin entry for `tone`: a plain number equal to it, or a list whose values all equal it.
pub fn pick_tone(skins: &[RawSkin], tone: u8) -> Option<&RawSkin> {
    skins.iter().find(|s| match &s.tone {
        Tone::One(t) => *t == tone,
        Tone::Many(ts) => !ts.is_empty() && ts.iter().all(|t| *t == tone),
    })
}

fn merge_shortcodes(hexcode: &str, presets: &[&Shortcodes]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for preset in presets {
        let names: &[String] = match preset.get(hexcode) {
            Some(OneOrMany::One(s)) => std::slice::from_ref(s),
            Some(OneOrMany::Many(v)) => v,
            None => &[],
        };
        for n in names {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
    }
    out
}

/// An emoji that survived the SVG check, before cells are assigned.
struct Plan<'a> {
    raw: &'a RawEmoji,
    category: Category,
    base_file: String,
    /// Tone 1..=5: glyph and SVG stem, or `None` to reuse the base.
    tones: [Option<(&'a str, String)>; 5],
    tone_capable: bool,
}

#[allow(dead_code)] // wrapper for the atlas step; the CLI uses the missing-aware variant
pub fn build_table(
    data: &[u8],
    github: &[u8],
    iamcal: &[u8],
    svgs: &HashMap<String, Vec<u8>>,
) -> Result<Built> {
    build_table_with_missing(data, github, iamcal, svgs).map(|(built, _)| built)
}

/// Like `build_table`, also returning the hexcodes dropped for lack of an SVG.
pub fn build_table_with_missing(
    data: &[u8],
    github: &[u8],
    iamcal: &[u8],
    svgs: &HashMap<String, Vec<u8>>,
) -> Result<(Built, Vec<String>)> {
    let mut raws: Vec<RawEmoji> = serde_json::from_slice(data).context("parsing emojibase data")?;
    raws.sort_by_key(|r| r.order);
    let github: Shortcodes = serde_json::from_slice(github).context("parsing github shortcodes")?;
    let iamcal: Shortcodes = serde_json::from_slice(iamcal).context("parsing iamcal shortcodes")?;
    let available: HashSet<String> = svgs.keys().cloned().collect();

    let mut plans = Vec::new();
    let mut missing = Vec::new();
    for raw in &raws {
        let Some(category) = raw.group.and_then(Category::from_emojibase_group) else {
            continue;
        };
        let Some(base_file) = twemoji_file(&raw.hexcode, &available) else {
            missing.push(raw.hexcode.clone());
            continue;
        };
        let mut tones: [Option<(&str, String)>; 5] = Default::default();
        for (i, slot) in tones.iter_mut().enumerate() {
            let tone = u8::try_from(i + 1).expect("tone index fits u8");
            *slot = pick_tone(&raw.skins, tone)
                .and_then(|s| twemoji_file(&s.hexcode, &available).map(|f| (s.emoji.as_str(), f)));
        }
        plans.push(Plan {
            raw,
            category,
            base_file,
            tones,
            tone_capable: !raw.skins.is_empty(),
        });
    }
    if missing.len() > MAX_MISSING {
        bail!(
            "{} emoji have no Twemoji SVG (limit {MAX_MISSING}): {}",
            missing.len(),
            missing.join(" ")
        );
    }

    // Pages in category then tone set order, only those in use.
    let mut used: HashSet<PageKey> = HashSet::new();
    for p in &plans {
        if p.tone_capable {
            used.insert(PageKey {
                category: p.category,
                tone_set: 1,
            });
            for (i, t) in p.tones.iter().enumerate() {
                if t.is_some() {
                    used.insert(PageKey {
                        category: p.category,
                        tone_set: u8::try_from(i + 2).expect("tone set fits u8"),
                    });
                }
            }
        } else {
            used.insert(PageKey {
                category: p.category,
                tone_set: 0,
            });
        }
    }
    let mut pages: Vec<PageKey> = Vec::new();
    for category in Category::ALL {
        for tone_set in 0..=6u8 {
            let key = PageKey { category, tone_set };
            if used.contains(&key) {
                pages.push(key);
            }
        }
    }
    let page_index: HashMap<PageKey, u16> = pages
        .iter()
        .enumerate()
        .map(|(i, k)| Ok((*k, u16::try_from(i)?)))
        .collect::<Result<_>>()?;

    let mut next_cell = vec![0u16; pages.len()];
    let mut sprites: Vec<(SpriteRef, Vec<u8>)> = Vec::new();
    let mut alloc = |key: PageKey, file: &str| -> Result<SpriteRef> {
        let page = page_index[&key];
        let cell = next_cell[usize::from(page)];
        next_cell[usize::from(page)] = cell.checked_add(1).context("page cell overflow")?;
        let sprite = SpriteRef { page, cell };
        sprites.push((sprite, svgs[file].clone()));
        Ok(sprite)
    };

    let mut emojis = Vec::with_capacity(plans.len());
    for p in &plans {
        let raw = p.raw;
        let base_set = u8::from(p.tone_capable);
        let sprite = alloc(
            PageKey {
                category: p.category,
                tone_set: base_set,
            },
            &p.base_file,
        )?;
        let tones = if p.tone_capable {
            let mut variants = Vec::with_capacity(5);
            for (i, t) in p.tones.iter().enumerate() {
                variants.push(match t {
                    Some((glyph, file)) => Variant {
                        glyph: (*glyph).to_string(),
                        sprite: alloc(
                            PageKey {
                                category: p.category,
                                tone_set: u8::try_from(i + 2).expect("tone set fits u8"),
                            },
                            file,
                        )?,
                    },
                    None => Variant {
                        glyph: raw.emoji.clone(),
                        sprite,
                    },
                });
            }
            let boxed: Box<[Variant; 5]> = variants
                .into_boxed_slice()
                .try_into()
                .expect("exactly five tone variants");
            Some(boxed)
        } else {
            None
        };
        emojis.push(Emoji {
            hexcode: raw.hexcode.clone(),
            glyph: raw.emoji.clone(),
            category: p.category,
            order: raw.order,
            name: raw.label.clone(),
            keywords: raw.tags.clone(),
            shortcodes: merge_shortcodes(&raw.hexcode, &[&github, &iamcal]),
            sprite,
            tones,
        });
    }

    let built = Built {
        table: EmojiTable { emojis, pages },
        svgs: sprites,
    };
    Ok((built, missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn set<const N: usize>(names: [&str; N]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn svgs_for(stems: &[&str]) -> HashMap<String, Vec<u8>> {
        stems
            .iter()
            .map(|s| (s.to_string(), format!("<svg id=\"{s}\"/>").into_bytes()))
            .collect()
    }

    #[test]
    fn twemoji_names() {
        let a = set(["1f600", "2764", "23-20e3", "1f3f3-fe0f-200d-1f308", "a9"]);
        assert_eq!(twemoji_file("1F600", &a).unwrap(), "1f600");
        assert_eq!(twemoji_file("2764-FE0F", &a).unwrap(), "2764");
        assert_eq!(twemoji_file("0023-FE0F-20E3", &a).unwrap(), "23-20e3");
        assert_eq!(
            twemoji_file("1F3F3-FE0F-200D-1F308", &a).unwrap(),
            "1f3f3-fe0f-200d-1f308"
        );
        assert_eq!(twemoji_file("00A9", &a).unwrap(), "a9");
        assert!(twemoji_file("1F9CC", &a).is_none());
    }

    #[test]
    fn twemoji_falls_back_to_fe0f_kept() {
        let a = set(["2764-fe0f"]);
        assert_eq!(twemoji_file("2764-FE0F", &a).unwrap(), "2764-fe0f");
    }

    fn skins() -> Vec<RawSkin> {
        serde_json::from_str(
            r#"[
                {"hexcode":"A-1-1","emoji":"a","tone":[1,1]},
                {"hexcode":"A-1-2","emoji":"b","tone":[1,2]},
                {"hexcode":"B-2","emoji":"c","tone":2},
                {"hexcode":"C-3-3","emoji":"d","tone":[3,3]}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn tone_pick_requires_all_equal() {
        let skins = skins();
        assert_eq!(pick_tone(&skins, 1).unwrap().hexcode, "A-1-1");
        assert_eq!(pick_tone(&skins, 2).unwrap().hexcode, "B-2");
        assert_eq!(pick_tone(&skins, 3).unwrap().hexcode, "C-3-3");
        assert!(pick_tone(&skins, 4).is_none());
    }

    const DATA: &str = r#"[
        {"label":"grinning face","hexcode":"1F600","emoji":"😀","group":0,"order":1,"tags":["face","grin"]},
        {"label":"light skin tone","hexcode":"1F3FB","emoji":"🏻","group":2,"order":2}
    ]"#;

    #[test]
    fn build_drops_components_and_merges_shortcodes() {
        let svgs = svgs_for(&["1f600", "1f3fb"]);
        let b = build_table(
            DATA.as_bytes(),
            br#"{"1F600":"grinning"}"#,
            br#"{"1F600":["grinning","grinning_face"]}"#,
            &svgs,
        )
        .unwrap();
        assert_eq!(b.table.emojis.len(), 1);
        let e = &b.table.emojis[0];
        assert_eq!(e.hexcode, "1F600");
        assert_eq!(e.glyph, "\u{1F600}");
        assert_eq!(e.name, "grinning face");
        assert_eq!(e.keywords, ["face", "grin"]);
        assert_eq!(e.shortcodes, ["grinning", "grinning_face"]);
        assert_eq!(e.category, Category::SmileysPeople);
        assert!(e.tones.is_none());
        assert_eq!(b.table.pages.len(), 1);
        assert_eq!(
            b.table.pages[0],
            PageKey {
                category: Category::SmileysPeople,
                tone_set: 0
            }
        );
        assert_eq!(b.svgs.len(), 1);
        assert_eq!(b.svgs[0].0, e.sprite);
    }

    const TONED: &str = r#"[
        {"label":"thumbs up","hexcode":"1F44D","emoji":"👍","group":1,"order":2,
         "skins":[
           {"hexcode":"1F44D-1F3FB","emoji":"👍🏻","tone":1},
           {"hexcode":"1F44D-1F3FC","emoji":"👍🏼","tone":2},
           {"hexcode":"1F44D-1F3FD","emoji":"👍🏽","tone":3},
           {"hexcode":"1F44D-1F3FE","emoji":"👍🏾","tone":4}
         ]},
        {"label":"grinning face","hexcode":"1F600","emoji":"😀","group":0,"order":1}
    ]"#;

    #[test]
    fn tone_capable_emoji_use_tone_pages_and_base_fallback() {
        // Tone 4 has a skin entry but no SVG; tone 5 has no skin entry at all.
        let svgs = svgs_for(&[
            "1f600",
            "1f44d",
            "1f44d-1f3fb",
            "1f44d-1f3fc",
            "1f44d-1f3fd",
        ]);
        let b = build_table(TONED.as_bytes(), b"{}", b"{}", &svgs).unwrap();
        // Emojibase order: grinning (order 1) first.
        assert_eq!(b.table.emojis[0].hexcode, "1F600");
        let up = &b.table.emojis[1];
        let tones = up.tones.as_ref().unwrap();
        let page_of = |s: SpriteRef| b.table.pages[usize::from(s.page)];
        assert_eq!(page_of(up.sprite).tone_set, 1);
        assert_eq!(page_of(tones[0].sprite).tone_set, 2);
        assert_eq!(page_of(tones[1].sprite).tone_set, 3);
        assert_eq!(page_of(tones[2].sprite).tone_set, 4);
        assert_eq!(tones[0].glyph, "\u{1F44D}\u{1F3FB}");
        assert_eq!(tones[3].sprite, up.sprite);
        assert_eq!(tones[3].glyph, up.glyph);
        assert_eq!(tones[4].sprite, up.sprite);
        // Pages follow category then tone set order, and each cell is unique per page.
        let keys: Vec<(usize, u8)> = b
            .table
            .pages
            .iter()
            .map(|p| {
                let c = Category::ALL.iter().position(|c| *c == p.category).unwrap();
                (c, p.tone_set)
            })
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        let mut seen = HashSet::new();
        for (s, _) in &b.svgs {
            assert!(seen.insert(*s), "duplicate sprite {s:?}");
        }
        // 1 grinning + base + 3 tone SVGs.
        assert_eq!(b.svgs.len(), 5);
    }

    #[test]
    fn cells_follow_emojibase_order_per_page() {
        let data = r#"[
            {"label":"b","hexcode":"1F601","emoji":"😁","group":0,"order":20},
            {"label":"a","hexcode":"1F600","emoji":"😀","group":0,"order":10}
        ]"#;
        let svgs = svgs_for(&["1f600", "1f601"]);
        let b = build_table(data.as_bytes(), b"{}", b"{}", &svgs).unwrap();
        assert_eq!(b.table.emojis[0].hexcode, "1F600");
        assert_eq!(b.table.emojis[0].sprite.cell, 0);
        assert_eq!(b.table.emojis[1].sprite.cell, 1);
    }

    #[test]
    fn missing_svg_drops_emoji_and_is_listed() {
        let svgs = svgs_for(&["1f600"]);
        let data = r#"[
            {"label":"a","hexcode":"1F600","emoji":"😀","group":0,"order":1},
            {"label":"melting","hexcode":"1FAE0","emoji":"🫠","group":0,"order":2}
        ]"#;
        let (b, missing) = build_table_with_missing(data.as_bytes(), b"{}", b"{}", &svgs).unwrap();
        assert_eq!(b.table.emojis.len(), 1);
        assert_eq!(missing, ["1FAE0"]);
    }

    #[test]
    fn too_many_missing_svgs_fails() {
        let entries: Vec<String> = (0..21)
            .map(|i| {
                format!(
                    r#"{{"label":"e{i}","hexcode":"1F9{i:02X}","emoji":"x","group":0,"order":{i}}}"#
                )
            })
            .collect();
        let data = format!("[{}]", entries.join(","));
        let err = build_table(data.as_bytes(), b"{}", b"{}", &HashMap::new())
            .err()
            .expect("must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("21"), "{msg}");
        assert!(msg.contains("1F900"), "{msg}");
        assert!(msg.contains("1F914"), "{msg}");
    }
}
