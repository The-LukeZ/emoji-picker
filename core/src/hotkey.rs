//! Global hotkey model: a platform-free description of a key combination,
//! stored in config as a string such as `Ctrl+Shift+Space`.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// Win32 virtual-key codes as plain constants (no `windows` dependency in core).
const VK_SPACE: u16 = 0x20;
const VK_OEM_PERIOD: u16 = 0xBE;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;

/// Virtual keys that are modifiers themselves and so cannot be the main key.
const MODIFIER_VKS: [u16; 11] = [
    0x10, // VK_SHIFT
    0x11, // VK_CONTROL
    0x12, // VK_MENU (Alt)
    0xA0, // VK_LSHIFT
    0xA1, // VK_RSHIFT
    0xA2, // VK_LCONTROL
    0xA3, // VK_RCONTROL
    0xA4, // VK_LMENU
    0xA5, // VK_RMENU
    0x5B, // VK_LWIN
    0x5C, // VK_RWIN
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    fn is_empty(&self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// The system `Win+.` emoji panel shortcut.
    WinPeriod,
    Combo {
        mods: Mods,
        vk: u16,
    },
}

/// Why a hotkey is rejected for registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyError {
    /// No modifier held: would break normal typing.
    NoModifier,
    /// Shift alone: would break typing capital letters.
    ShiftOnly,
    /// The main key is itself a modifier key.
    ModifierKeyAsKey,
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HotkeyError::NoModifier => "a hotkey needs at least one modifier key",
            HotkeyError::ShiftOnly => "Shift alone is not enough, add Ctrl, Alt or Win",
            HotkeyError::ModifierKeyAsKey => "a modifier key cannot be the main key",
        })
    }
}

impl std::error::Error for HotkeyError {}

/// Error from parsing the string form of a [`Hotkey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseHotkeyError {
    Empty,
    /// An empty segment, as in `Ctrl+` or `+A`.
    EmptyPart,
    UnknownKey(String),
    DuplicateModifier(String),
    /// More than one non-modifier key.
    MultipleKeys,
    /// Only modifiers, no main key.
    MissingKey,
}

impl fmt::Display for ParseHotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseHotkeyError::Empty => f.write_str("empty hotkey"),
            ParseHotkeyError::EmptyPart => f.write_str("empty key name in hotkey"),
            ParseHotkeyError::UnknownKey(k) => write!(f, "unknown key name {k:?}"),
            ParseHotkeyError::DuplicateModifier(m) => write!(f, "duplicate modifier {m:?}"),
            ParseHotkeyError::MultipleKeys => f.write_str("more than one main key"),
            ParseHotkeyError::MissingKey => f.write_str("hotkey has only modifiers"),
        }
    }
}

impl std::error::Error for ParseHotkeyError {}

impl Hotkey {
    /// Ready-made choices offered in settings.
    pub fn presets() -> [Hotkey; 2] {
        [
            Hotkey::WinPeriod,
            Hotkey::Combo {
                mods: Mods {
                    ctrl: true,
                    shift: true,
                    ..Mods::default()
                },
                vk: VK_SPACE,
            },
        ]
    }

    pub fn validate(&self) -> Result<(), HotkeyError> {
        match self {
            Hotkey::WinPeriod => Ok(()),
            Hotkey::Combo { mods, vk } => {
                if MODIFIER_VKS.contains(vk) {
                    Err(HotkeyError::ModifierKeyAsKey)
                } else if mods.is_empty() {
                    Err(HotkeyError::NoModifier)
                } else if mods.shift && !(mods.ctrl || mods.alt || mods.win) {
                    Err(HotkeyError::ShiftOnly)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Ctrl+Alt is how AltGr is reported on many keyboard layouts, so the
    /// combination can swallow typed characters. Allowed, but worth a warning.
    pub fn altgr_risk(&self) -> bool {
        matches!(self, Hotkey::Combo { mods, .. } if mods.ctrl && mods.alt)
    }
}

fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        VK_F1..=VK_F24 => format!("F{}", vk - VK_F1 + 1),
        VK_SPACE => "Space".to_string(),
        VK_OEM_PERIOD => ".".to_string(),
        _ => format!("VK_0x{vk:02X}"),
    }
}

fn parse_key(token: &str) -> Result<u16, ParseHotkeyError> {
    let unknown = || ParseHotkeyError::UnknownKey(token.to_string());
    let upper = token.to_ascii_uppercase();
    if upper == "SPACE" {
        return Ok(VK_SPACE);
    }
    if upper == "." {
        return Ok(VK_OEM_PERIOD);
    }
    if let Some(hex) = upper.strip_prefix("VK_0X") {
        if hex.is_empty() || hex.len() > 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(unknown());
        }
        return match u16::from_str_radix(hex, 16) {
            Ok(0) | Err(_) => Err(unknown()),
            Ok(vk) => Ok(vk),
        };
    }
    if let Some(num) = upper.strip_prefix('F') {
        if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
            return match num.parse::<u16>() {
                Ok(n @ 1..=24) => Ok(VK_F1 + n - 1),
                _ => Err(unknown()),
            };
        }
    }
    match upper.as_bytes() {
        [b] if b.is_ascii_uppercase() || b.is_ascii_digit() => Ok(u16::from(*b)),
        _ => Err(unknown()),
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Hotkey::WinPeriod => f.write_str("Win+."),
            Hotkey::Combo { mods, vk } => {
                for (on, name) in [
                    (mods.ctrl, "Ctrl"),
                    (mods.alt, "Alt"),
                    (mods.shift, "Shift"),
                    (mods.win, "Win"),
                ] {
                    if on {
                        write!(f, "{name}+")?;
                    }
                }
                f.write_str(&key_name(*vk))
            }
        }
    }
}

impl FromStr for Hotkey {
    type Err = ParseHotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(ParseHotkeyError::Empty);
        }
        let mut mods = Mods::default();
        let mut vk: Option<u16> = None;
        for part in s.split('+') {
            let token = part.trim();
            if token.is_empty() {
                return Err(ParseHotkeyError::EmptyPart);
            }
            let slot = match token.to_ascii_lowercase().as_str() {
                "ctrl" => Some(&mut mods.ctrl),
                "alt" => Some(&mut mods.alt),
                "shift" => Some(&mut mods.shift),
                "win" => Some(&mut mods.win),
                _ => None,
            };
            match slot {
                Some(flag) => {
                    if *flag {
                        return Err(ParseHotkeyError::DuplicateModifier(token.to_string()));
                    }
                    *flag = true;
                }
                None => {
                    let key = parse_key(token)?;
                    if vk.replace(key).is_some() {
                        return Err(ParseHotkeyError::MultipleKeys);
                    }
                }
            }
        }
        let vk = vk.ok_or(ParseHotkeyError::MissingKey)?;
        let win_only = Mods {
            win: true,
            ..Mods::default()
        };
        if mods == win_only && vk == VK_OEM_PERIOD {
            return Ok(Hotkey::WinPeriod);
        }
        Ok(Hotkey::Combo { mods, vk })
    }
}

impl Serialize for Hotkey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        assert_eq!("Win+.".parse::<Hotkey>().unwrap(), Hotkey::WinPeriod);
        let h: Hotkey = "Ctrl+Shift+Space".parse().unwrap();
        assert_eq!(h.to_string(), "Ctrl+Shift+Space");
        assert_eq!("shift+ctrl+space".parse::<Hotkey>().unwrap(), h);
        assert_eq!(
            "Ctrl+VK_0xBA".parse::<Hotkey>().unwrap().to_string(),
            "Ctrl+VK_0xBA"
        );
    }

    #[test]
    fn rejects_typing_breakers() {
        assert_eq!(
            "A".parse::<Hotkey>().unwrap().validate(),
            Err(HotkeyError::NoModifier)
        );
        assert_eq!(
            "Shift+A".parse::<Hotkey>().unwrap().validate(),
            Err(HotkeyError::ShiftOnly)
        );
        assert!("Ctrl+Shift+Space"
            .parse::<Hotkey>()
            .unwrap()
            .validate()
            .is_ok());
    }

    #[test]
    fn modifier_key_as_key_checked_first() {
        for vk in [
            0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C,
        ] {
            let none = Hotkey::Combo {
                mods: Mods::default(),
                vk,
            };
            assert_eq!(
                none.validate(),
                Err(HotkeyError::ModifierKeyAsKey),
                "{vk:#x}"
            );
            let ctrl = Hotkey::Combo {
                mods: Mods {
                    ctrl: true,
                    ..Mods::default()
                },
                vk,
            };
            assert_eq!(
                ctrl.validate(),
                Err(HotkeyError::ModifierKeyAsKey),
                "{vk:#x}"
            );
        }
        let via_str = "Ctrl+VK_0x11".parse::<Hotkey>().unwrap();
        assert_eq!(via_str.validate(), Err(HotkeyError::ModifierKeyAsKey));
    }

    #[test]
    fn win_period_valid_and_other_win_combos_valid() {
        assert!(Hotkey::WinPeriod.validate().is_ok());
        assert!("Win+E".parse::<Hotkey>().unwrap().validate().is_ok());
    }

    #[test]
    fn altgr_warning() {
        assert!("Ctrl+Alt+E".parse::<Hotkey>().unwrap().altgr_risk());
        assert!(!Hotkey::presets().iter().any(|h| h.altgr_risk()));
        assert!(!Hotkey::WinPeriod.altgr_risk());
    }

    #[test]
    fn presets_are_valid_and_fixed() {
        let p = Hotkey::presets();
        assert_eq!(p[0], Hotkey::WinPeriod);
        assert_eq!(p[1].to_string(), "Ctrl+Shift+Space");
        assert!(p.iter().all(|h| h.validate().is_ok()));
    }

    #[test]
    fn canonical_modifier_order() {
        let h: Hotkey = "win+shift+alt+ctrl+f13".parse().unwrap();
        assert_eq!(h.to_string(), "Ctrl+Alt+Shift+Win+F13");
        assert_eq!(
            "Win+Shift+.".parse::<Hotkey>().unwrap().to_string(),
            "Shift+Win+."
        );
        assert_ne!("Win+Shift+.".parse::<Hotkey>().unwrap(), Hotkey::WinPeriod);
    }

    #[test]
    fn key_names_round_trip() {
        for s in [
            "Ctrl+A",
            "Ctrl+Z",
            "Ctrl+0",
            "Ctrl+9",
            "Ctrl+F1",
            "Ctrl+F24",
            "Ctrl+Space",
            "Ctrl+.",
        ] {
            assert_eq!(s.parse::<Hotkey>().unwrap().to_string(), s);
        }
        assert_eq!(
            "Ctrl+VK_0x41".parse::<Hotkey>().unwrap(),
            "Ctrl+A".parse::<Hotkey>().unwrap()
        );
        assert_eq!(
            "Ctrl+F1".parse::<Hotkey>().unwrap(),
            Hotkey::Combo {
                mods: Mods {
                    ctrl: true,
                    ..Mods::default()
                },
                vk: 0x70
            }
        );
    }

    #[test]
    fn rejects_bad_strings() {
        for s in [
            "",
            "  ",
            "Ctrl+",
            "+A",
            "Ctrl+Ctrl+A",
            "A+B",
            "A+A",
            "Ctrl+Shift",
            "Ctrl",
            "Win",
            "Ctrl+Foo",
            "Ctrl+F0",
            "Ctrl+F25",
            "Ctrl+VK_0x",
            "Ctrl+VK_0xZZ",
            "Ctrl+VK_0x0",
            "Ctrl+VK_0x10000",
            "Ctrl++",
        ] {
            assert!(s.parse::<Hotkey>().is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn serde_uses_string_form() {
        let h: Hotkey = "Ctrl+Alt+E".parse().unwrap();
        let bytes = postcard::to_allocvec(&h).unwrap();
        let as_string = postcard::to_allocvec(&"Ctrl+Alt+E".to_string()).unwrap();
        assert_eq!(bytes, as_string);
        assert_eq!(postcard::from_bytes::<Hotkey>(&bytes).unwrap(), h);
        let wp = postcard::to_allocvec(&Hotkey::WinPeriod).unwrap();
        assert_eq!(
            postcard::from_bytes::<Hotkey>(&wp).unwrap(),
            Hotkey::WinPeriod
        );
    }

    #[test]
    fn serde_rejects_invalid_string() {
        let bad = postcard::to_allocvec(&"Ctrl+Nope".to_string()).unwrap();
        assert!(postcard::from_bytes::<Hotkey>(&bad).is_err());
        let empty = postcard::to_allocvec(&String::new()).unwrap();
        assert!(postcard::from_bytes::<Hotkey>(&empty).is_err());
    }
}
