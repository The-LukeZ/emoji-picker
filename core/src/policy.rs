//! Decides whether the global hotkey may be registered for the current foreground window.

use crate::config::Config;

/// What the service thread knows about the foreground window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    pub exe: Option<String>,
    pub fullscreen: bool,
}

/// Lowercased file name of `path`; accepts full paths with `\` or `/` separators or a bare name.
pub fn exe_file_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
}

/// False when paused, when the foreground exe is unknown or blocklisted (compared by file
/// name), or when it is fullscreen and `disable_in_fullscreen` is set.
pub fn hotkey_allowed(fg: &Foreground, cfg: &Config, paused: bool) -> bool {
    if paused {
        return false;
    }
    let Some(exe) = &fg.exe else {
        return false;
    };
    let name = exe_file_name(exe);
    if cfg
        .blocklist
        .iter()
        .any(|entry| exe_file_name(entry) == name)
    {
        return false;
    }
    !(fg.fullscreen && cfg.disable_in_fullscreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocklist_matches_file_name_case_insensitive() {
        let cfg = Config {
            blocklist: vec!["VALORANT-Win64-Shipping.exe".into()],
            ..Default::default()
        };
        let fg = |e: &str| Foreground {
            exe: Some(e.into()),
            fullscreen: false,
        };
        assert!(!hotkey_allowed(
            &fg(r"C:\Riot\valorant-win64-shipping.exe"),
            &cfg,
            false
        ));
        assert!(hotkey_allowed(&fg(r"C:\Windows\notepad.exe"), &cfg, false));
        assert!(!hotkey_allowed(
            &Foreground {
                exe: None,
                fullscreen: false
            },
            &cfg,
            false
        ));
        assert!(!hotkey_allowed(&fg(r"C:\Windows\notepad.exe"), &cfg, true));
    }

    #[test]
    fn blocklist_entry_may_be_full_path() {
        let cfg = Config {
            blocklist: vec![r"D:\Games\Game.EXE".into()],
            ..Default::default()
        };
        let fg = Foreground {
            exe: Some("C:/other/game.exe".into()),
            fullscreen: false,
        };
        assert!(!hotkey_allowed(&fg, &cfg, false));
    }

    #[test]
    fn exe_file_name_forms() {
        assert_eq!(exe_file_name(r"C:\Windows\Notepad.EXE"), "notepad.exe");
        assert_eq!(exe_file_name("C:/Windows/Notepad.exe"), "notepad.exe");
        assert_eq!(exe_file_name("Notepad.exe"), "notepad.exe");
    }

    #[test]
    fn fullscreen_respects_setting() {
        let fg = Foreground {
            exe: Some(r"C:\Windows\notepad.exe".into()),
            fullscreen: true,
        };
        let blocked = Config::default();
        assert!(blocked.disable_in_fullscreen);
        assert!(!hotkey_allowed(&fg, &blocked, false));
        let allowed = Config {
            disable_in_fullscreen: false,
            ..Default::default()
        };
        assert!(hotkey_allowed(&fg, &allowed, false));
    }
}
