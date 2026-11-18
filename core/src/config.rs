//! User settings stored as TOML. Loading never fails: a missing file gives
//! defaults and a broken file is backed up and replaced by defaults.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;

/// Picker tile size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeStep {
    Small,
    Medium,
    Large,
    Huge,
}

impl SizeStep {
    /// Tile edge length in pixels.
    pub fn tile_px(self) -> u16 {
        match self {
            SizeStep::Small => 24,
            SizeStep::Medium => 32,
            SizeStep::Large => 40,
            SizeStep::Huge => 48,
        }
    }

    /// Next larger step, clamped at the largest.
    pub fn next(self) -> Self {
        match self {
            SizeStep::Small => SizeStep::Medium,
            SizeStep::Medium => SizeStep::Large,
            SizeStep::Large | SizeStep::Huge => SizeStep::Huge,
        }
    }

    /// Next smaller step, clamped at the smallest.
    pub fn prev(self) -> Self {
        match self {
            SizeStep::Small | SizeStep::Medium => SizeStep::Small,
            SizeStep::Large => SizeStep::Medium,
            SizeStep::Huge => SizeStep::Large,
        }
    }
}

/// How the chosen emoji is delivered to the target window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsertMode {
    Type,
    Paste,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `None` means the first-run setup has not finished.
    pub hotkey: Option<Hotkey>,
    pub blocklist: Vec<String>,
    pub disable_in_fullscreen: bool,
    /// 0 is the default tone, 1 to 5 are the skin tone modifiers.
    pub skin_tone: u8,
    pub size: SizeStep,
    pub start_with_windows: bool,
    pub insert_mode: InsertMode,
    pub paste_exes: Vec<String>,
    pub recents_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: None,
            blocklist: Vec::new(),
            disable_in_fullscreen: true,
            skin_tone: 0,
            size: SizeStep::Medium,
            start_with_windows: true,
            insert_mode: InsertMode::Type,
            paste_exes: Vec::new(),
            recents_enabled: true,
        }
    }
}

/// `path` with `suffix` appended to the file name (`config.toml` -> `config.toml.tmp`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Write `contents` to `path` via a `.tmp` sibling and a rename, creating the parent dir.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = sibling(path, ".tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

impl Config {
    /// Load settings. Returns the config and an optional warning for the user.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) => {
                return (
                    Config::default(),
                    Some(format!(
                        "Could not read the settings file: {e}. Using defaults."
                    )),
                )
            }
        };
        match toml::from_str::<Config>(&text) {
            Ok(mut config) => {
                if config.skin_tone > 5 {
                    config.skin_tone = 0;
                }
                (config, None)
            }
            Err(e) => {
                let bak = sibling(path, ".bak");
                let backup = match fs::rename(path, &bak) {
                    Ok(()) => format!("The old file was saved as {}.", bak.display()),
                    Err(err) => format!("The old file could not be backed up ({err})."),
                };
                (
                    Config::default(),
                    Some(format!(
                        "The settings file could not be read ({}). Using defaults. {backup}",
                        e.message()
                    )),
                )
            }
        }
    }

    /// Save settings, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        write_atomic(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{read_to_string, write};

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (c, w) = Config::load(&dir.path().join("config.toml"));
        assert_eq!(c, Config::default());
        assert!(w.is_none());
    }

    #[test]
    fn defaults_match_brief() {
        let c = Config::default();
        assert_eq!(c.hotkey, None);
        assert!(c.blocklist.is_empty() && c.paste_exes.is_empty());
        assert!(c.disable_in_fullscreen && c.start_with_windows && c.recents_enabled);
        assert_eq!(c.skin_tone, 0);
        assert_eq!(c.size, SizeStep::Medium);
        assert_eq!(c.insert_mode, InsertMode::Type);
    }

    #[test]
    fn unknown_and_missing_keys_keep_known_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write(
            &p,
            "hotkey = \"Win+.\"\nsize = \"huge\"\nfuture_option = 7\n",
        )
        .unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(c.hotkey, Some(Hotkey::WinPeriod));
        assert_eq!(c.size, SizeStep::Huge);
        assert!(c.disable_in_fullscreen);
        assert!(w.is_none());
    }

    #[test]
    fn broken_file_is_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write(&p, "hotkey = [[[").unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(c, Config::default());
        assert!(w.is_some());
        assert_eq!(
            read_to_string(dir.path().join("config.toml.bak")).unwrap(),
            "hotkey = [[["
        );
        assert!(!p.exists());
    }

    #[test]
    fn invalid_hotkey_string_is_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write(&p, "hotkey = \"Bogus+Nope\"\n").unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(c, Config::default());
        assert!(w.is_some());
        assert!(dir.path().join("config.toml.bak").exists());
    }

    #[test]
    fn backup_overwrites_previous_backup() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write(dir.path().join("config.toml.bak"), "old backup").unwrap();
        write(&p, "size = [").unwrap();
        let (_, w) = Config::load(&p);
        assert!(w.is_some());
        assert_eq!(
            read_to_string(dir.path().join("config.toml.bak")).unwrap(),
            "size = ["
        );
    }

    #[test]
    fn skin_tone_above_five_clamps_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        write(&p, "skin_tone = 6\n").unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(c.skin_tone, 0);
        assert!(w.is_none());
        write(&p, "skin_tone = 5\n").unwrap();
        assert_eq!(Config::load(&p).0.skin_tone, 5);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("config.toml");
        let c = Config {
            hotkey: Some(Hotkey::WinPeriod),
            blocklist: vec!["game.exe".into(), "other.exe".into()],
            disable_in_fullscreen: false,
            skin_tone: 3,
            size: SizeStep::Large,
            start_with_windows: false,
            insert_mode: InsertMode::Paste,
            paste_exes: vec!["slack.exe".into()],
            recents_enabled: false,
        };
        c.save(&p).unwrap();
        assert!(!dir.path().join("nested").join("config.toml.tmp").exists());
        let (loaded, w) = Config::load(&p);
        assert_eq!(loaded, c);
        assert!(w.is_none());
    }

    #[test]
    fn default_config_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        Config::default().save(&p).unwrap();
        assert_eq!(Config::load(&p), (Config::default(), None));
    }

    #[test]
    fn size_steps_clamp() {
        assert_eq!(SizeStep::Small.prev(), SizeStep::Small);
        assert_eq!(SizeStep::Huge.next(), SizeStep::Huge);
        assert_eq!(SizeStep::Small.next(), SizeStep::Medium);
        assert_eq!(SizeStep::Huge.prev(), SizeStep::Large);
        let px: Vec<u16> = [
            SizeStep::Small,
            SizeStep::Medium,
            SizeStep::Large,
            SizeStep::Huge,
        ]
        .iter()
        .map(|s| s.tile_px())
        .collect();
        assert_eq!(px, [24, 32, 40, 48]);
    }
}
