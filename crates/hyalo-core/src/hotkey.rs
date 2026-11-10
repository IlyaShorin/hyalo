//! The hotkey that shows and hides the launcher, as the config names it:
//! modifiers and one key joined by `+`, such as `Ctrl+Shift+K` or
//! `Win+F1`.
//!
//! Pure: the numbers it hands out are the ones `RegisterHotKey` takes,
//! the `MOD_*` bits and a virtual-key code.

use std::fmt;

/// The `MOD_*` bits of `RegisterHotKey`.
const ALT: u8 = 0x1;
const CTRL: u8 = 0x2;
const SHIFT: u8 = 0x4;
const WIN: u8 = 0x8;

/// Modifiers by name, in the order a hotkey's name lists them.
const MODIFIERS: [(&str, u8); 6] = [
    ("Win", WIN),
    ("Windows", WIN),
    ("Ctrl", CTRL),
    ("Control", CTRL),
    ("Alt", ALT),
    ("Shift", SHIFT),
];

/// Keys with names of their own, by virtual-key code. The first name of
/// a code is the one shown; the others are read too. Punctuation is named
/// by where it sits on a US keyboard.
const NAMED: [(&str, u16); 31] = [
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Return", 0x0D),
    ("Tab", 0x09),
    ("Esc", 0x1B),
    ("Escape", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Pause", 0x13),
    ("PrintScreen", 0x2C),
    ("`", 0xC0),
    ("-", 0xBD),
    ("=", 0xBB),
    ("[", 0xDB),
    ("]", 0xDD),
    ("\\", 0xDC),
    (";", 0xBA),
    ("'", 0xDE),
    (",", 0xBC),
    (".", 0xBE),
    ("/", 0xBF),
    ("Del", 0x2E),
];

/// The virtual-key code of F1; F24 is 23 further on.
const F1: u16 = 0x70;

/// Modifiers and one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// `MOD_*` bits.
    modifiers: u8,
    /// A virtual-key code.
    key: u16,
}

impl Default for Hotkey {
    /// Alt+Space.
    fn default() -> Self {
        Hotkey {
            modifiers: ALT,
            key: 0x20,
        }
    }
}

impl Hotkey {
    /// Reads a name such as `Ctrl+Shift+K`: modifiers in any order and
    /// any case, then or among them one key. A hotkey needs Ctrl, Alt or
    /// Win, unless its key is F1 to F24: Shift alone, or no modifier,
    /// would take a key the user types.
    pub fn parse(text: &str) -> Result<Hotkey, String> {
        let parts = text
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty());
        let (mut modifiers, mut key) = (0, None);
        for part in parts {
            match modifier(part) {
                Some(bit) if modifiers & bit != 0 => return Err(format!("`{part}` twice")),
                Some(bit) => modifiers |= bit,
                None => key = Some(one_key(key, part)?),
            }
        }
        checked(modifiers, key, text)
    }

    /// The `MOD_*` bits `RegisterHotKey` takes.
    pub fn modifiers(self) -> u32 {
        u32::from(self.modifiers)
    }

    /// The virtual-key code `RegisterHotKey` takes.
    pub fn key(self) -> u16 {
        self.key
    }
}

impl fmt::Display for Hotkey {
    /// The name the hotkey goes by: `Win+Ctrl+Alt+Shift+K` with the
    /// modifiers it has.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let shown = [MODIFIERS[0], MODIFIERS[2], MODIFIERS[4], MODIFIERS[5]];
        for (name, bit) in shown {
            if self.modifiers & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.key))
    }
}

/// The modifier bit a part names, if it names one.
fn modifier(part: &str) -> Option<u8> {
    let named = MODIFIERS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(part));
    named.map(|&(_, bit)| bit)
}

/// The key `part` names, when no key came before it.
fn one_key(before: Option<u16>, part: &str) -> Result<u16, String> {
    let key = key_code(part).ok_or_else(|| format!("`{part}` is not a key hyalo knows"))?;
    match before {
        Some(first) => Err(format!(
            "two keys, `{}` and `{part}`: a hotkey has one",
            key_name(first)
        )),
        None => Ok(key),
    }
}

/// The hotkey made of `modifiers` and `key`, if it is one a user can have.
fn checked(modifiers: u8, key: Option<u16>, text: &str) -> Result<Hotkey, String> {
    if text.trim().is_empty() {
        return Err("the hotkey is empty".to_string());
    }
    let key = key.ok_or_else(|| format!("no key after the modifiers in `{text}`"))?;
    if modifiers & (CTRL | ALT | WIN) == 0 && function_key(key).is_none() {
        return Err(format!(
            "`{text}` needs Ctrl, Alt or Win: only F1 to F24 do without"
        ));
    }
    Ok(Hotkey { modifiers, key })
}

/// The virtual-key code of a key's name, in any case.
fn key_code(name: &str) -> Option<u16> {
    let named = NAMED
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name));
    named.map(|&(_, code)| code).or_else(|| {
        let upper = name.to_ascii_uppercase();
        match upper.as_bytes() {
            [one] if one.is_ascii_alphanumeric() => Some(u16::from(*one)),
            [b'F', number @ ..] => function_code(std::str::from_utf8(number).ok()?),
            _ => None,
        }
    })
}

/// The code of F1 to F24, from the number after the F.
fn function_code(number: &str) -> Option<u16> {
    let number: u16 = number.parse().ok()?;
    (1..=24).contains(&number).then(|| F1 + number - 1)
}

/// Which of F1 to F24 `key` is, if it is one.
fn function_key(key: u16) -> Option<u16> {
    (F1..F1 + 24).contains(&key).then(|| key - F1 + 1)
}

/// The name a key is shown by.
fn key_name(key: u16) -> String {
    let named = NAMED.iter().find(|&&(_, code)| code == key);
    match (named, function_key(key)) {
        (Some((name, _)), _) => name.to_string(),
        (None, Some(number)) => format!("F{number}"),
        (None, None) => char::from(key as u8).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(text: &str) -> String {
        Hotkey::parse(text).expect("a hotkey").to_string()
    }

    fn refused(text: &str) -> String {
        Hotkey::parse(text).expect_err("not a hotkey")
    }

    #[test]
    fn the_default_is_alt_space() {
        let hotkey = Hotkey::default();
        assert_eq!(hotkey.to_string(), "Alt+Space");
        assert_eq!((hotkey.modifiers(), hotkey.key()), (0x1, 0x20));
        assert_eq!(Hotkey::parse("Alt+Space"), Ok(hotkey));
    }

    #[test]
    fn hands_out_the_numbers_register_hot_key_takes() {
        let hotkey = Hotkey::parse("Ctrl+Shift+K").expect("a hotkey");
        assert_eq!((hotkey.modifiers(), hotkey.key()), (0x2 | 0x4, 0x4B));
        let hotkey = Hotkey::parse("Win+F1").expect("a hotkey");
        assert_eq!((hotkey.modifiers(), hotkey.key()), (0x8, 0x70));
        let hotkey = Hotkey::parse("Alt+Ctrl+Win+Shift+7").expect("a hotkey");
        assert_eq!((hotkey.modifiers(), hotkey.key()), (0xF, 0x37));
    }

    #[test]
    fn reads_any_case_and_order_and_names_it_one_way() {
        assert_eq!(named("shift+ctrl+k"), "Ctrl+Shift+K");
        assert_eq!(named("SPACE+alt"), "Alt+Space");
        assert_eq!(named("Control + Windows + f12"), "Win+Ctrl+F12");
        assert_eq!(named("Alt+Shift+Win+Ctrl+Z"), "Win+Ctrl+Alt+Shift+Z");
    }

    #[test]
    fn knows_the_keys_by_their_usual_names() {
        let names = [
            ("Ctrl+Return", "Ctrl+Enter"),
            ("Ctrl+Escape", "Ctrl+Esc"),
            ("Ctrl+Del", "Ctrl+Delete"),
            ("Alt+pageup", "Alt+PageUp"),
            ("Alt+`", "Alt+`"),
            ("Ctrl+\\", "Ctrl+\\"),
            ("Ctrl+0", "Ctrl+0"),
            ("Ctrl+9", "Ctrl+9"),
            ("Ctrl+a", "Ctrl+A"),
            ("Ctrl+F24", "Ctrl+F24"),
        ];
        for (text, name) in names {
            assert_eq!(named(text), name, "{text}");
        }
        let codes = [("Alt+A", 0x41), ("Alt+Z", 0x5A), ("Alt+F24", 0x87)];
        for (text, code) in codes {
            assert_eq!(Hotkey::parse(text).expect("a hotkey").key(), code, "{text}");
        }
    }

    #[test]
    fn a_function_key_needs_no_modifier() {
        assert_eq!(named("F13"), "F13");
        assert_eq!(named("Shift+F1"), "Shift+F1");
    }

    #[test]
    fn a_key_the_user_types_needs_ctrl_alt_or_win() {
        for text in ["K", "Shift+K", "Space", "Shift+Space"] {
            assert!(refused(text).contains("needs Ctrl, Alt or Win"), "{text}");
        }
    }

    #[test]
    fn says_what_is_wrong_with_a_name_it_cannot_read() {
        assert!(refused("").contains("empty"));
        assert!(refused("Ctrl+Alt").contains("no key"));
        assert!(refused("Ctrl+Ctrl+K").contains("`Ctrl` twice"));
        assert!(refused("Ctrl+Control+K").contains("`Control` twice"));
        assert!(refused("Ctrl+K+L").contains("two keys"));
        assert!(refused("Ctrl+Kay").contains("`Kay`"));
        assert!(refused("Ctrl+F25").contains("`F25`"));
        assert!(refused("Ctrl+F0").contains("`F0`"));
        assert!(refused("Ctrl+").contains("no key"));
    }

    #[test]
    fn spare_plus_signs_and_spaces_do_no_harm() {
        assert_eq!(named("Ctrl++K"), "Ctrl+K");
        assert_eq!(named(" Ctrl + K "), "Ctrl+K");
    }
}
