//! The hotkey recorder: a field that, once clicked, takes the next
//! combination pressed as the new hotkey. Pure: the window hands it each
//! key and the modifiers held, and it says what it heard.

use hyalo_core::hotkey::Hotkey;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_ESCAPE, VK_F4, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};

/// The modifier keys held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    /// The `MOD_*` bits `RegisterHotKey` takes.
    fn bits(self) -> u32 {
        let bit = |held: bool, bit: u32| if held { bit } else { 0 };
        bit(self.alt, 0x1) | bit(self.ctrl, 0x2) | bit(self.shift, 0x4) | bit(self.win, 0x8)
    }

    /// Their names, in the order a hotkey's name lists them.
    pub fn names(self) -> Vec<String> {
        let held = [
            (self.win, "Win"),
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ];
        let names = held.into_iter().filter(|&(held, _)| held);
        names.map(|(_, name)| name.to_string()).collect()
    }
}

/// Whether the recorder listens, and to which modifiers so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Recorder {
    #[default]
    Idle,
    Listening(Mods),
}

/// What the recorder made of a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// Nothing yet: a modifier alone, or the recorder does not listen.
    Nothing,
    /// Esc alone: the hotkey stays as it is.
    Cancelled,
    /// Alt+F4 alone: the window closes, as Alt+F4 closes windows, rather
    /// than Alt+F4 being taken from every window.
    Close,
    /// A combination that may be a hotkey; the recorder stops.
    Hotkey(Hotkey),
    /// A combination that may not be one, and why; it listens on.
    Wrong(String),
}

impl Recorder {
    pub fn listen(&mut self) {
        *self = Recorder::Listening(Mods::default());
    }

    pub fn stop(&mut self) {
        *self = Recorder::Idle;
    }

    pub fn listening(self) -> bool {
        matches!(self, Recorder::Listening(_))
    }

    /// The key `key`, a virtual-key code, went down with `mods` held.
    pub fn key(&mut self, key: u16, mods: Mods) -> Heard {
        if !self.listening() {
            return Heard::Nothing;
        }
        if modifier(key) {
            *self = Recorder::Listening(mods);
            return Heard::Nothing;
        }
        if let Some(heard) = alone(key, mods) {
            self.stop();
            return heard;
        }
        match Hotkey::from_keys(mods.bits(), key) {
            Ok(hotkey) => {
                self.stop();
                Heard::Hotkey(hotkey)
            }
            Err(why) => Heard::Wrong(why),
        }
    }

    /// A key went up, and `mods` are what is held now.
    pub fn held(&mut self, mods: Mods) {
        if self.listening() {
            *self = Recorder::Listening(mods);
        }
    }

    /// The keys the field shows: those of `hotkey`, or the modifiers held
    /// while it listens.
    pub fn caps(self, hotkey: Hotkey) -> Vec<String> {
        match self {
            Recorder::Idle => caps(hotkey),
            Recorder::Listening(mods) => mods.names(),
        }
    }
}

/// The keys of `hotkey`, each by its name: `Ctrl`, `Shift`, `K`.
pub fn caps(hotkey: Hotkey) -> Vec<String> {
    // No key is named `+`: the one there is goes by `=`.
    hotkey.to_string().split('+').map(String::from).collect()
}

/// What the keys that keep their meaning while recording mean: Esc alone
/// cancels, Alt+F4 alone closes.
fn alone(key: u16, mods: Mods) -> Option<Heard> {
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    match (key, mods) {
        (key, mods) if key == VK_ESCAPE.0 && mods == Mods::default() => Some(Heard::Cancelled),
        (key, mods) if key == VK_F4.0 && mods == alt => Some(Heard::Close),
        _ => None,
    }
}

/// Whether `key` is a modifier: alone, it makes no hotkey.
fn modifier(key: u16) -> bool {
    let modifiers = [
        VK_SHIFT,
        VK_CONTROL,
        VK_MENU,
        VK_LWIN,
        VK_RWIN,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
    ];
    modifiers.iter().any(|modifier| modifier.0 == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_K, VK_NUMPAD0, VK_SPACE};

    const CTRL_SHIFT: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: true,
        win: false,
    };

    fn listening() -> Recorder {
        let mut recorder = Recorder::default();
        recorder.listen();
        recorder
    }

    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("a hotkey")
    }

    #[test]
    fn idle_it_hears_nothing() {
        let mut recorder = Recorder::default();
        assert!(!recorder.listening());
        assert_eq!(recorder.key(VK_K.0, CTRL_SHIFT), Heard::Nothing);
        assert_eq!(recorder.key(VK_ESCAPE.0, Mods::default()), Heard::Nothing);
        recorder.held(CTRL_SHIFT);
        assert_eq!(recorder, Recorder::Idle);
    }

    #[test]
    fn a_combination_pressed_is_the_hotkey_and_it_stops_listening() {
        let mut recorder = listening();
        assert!(recorder.listening());
        assert_eq!(
            recorder.key(VK_K.0, CTRL_SHIFT),
            Heard::Hotkey(hotkey("Ctrl+Shift+K"))
        );
        assert!(!recorder.listening());
    }

    #[test]
    fn a_function_key_alone_is_a_hotkey() {
        let mut recorder = listening();
        assert_eq!(
            recorder.key(VK_F4.0, Mods::default()),
            Heard::Hotkey(hotkey("F4"))
        );
    }

    #[test]
    fn modifiers_alone_are_shown_while_it_listens_on() {
        let mut recorder = listening();
        assert_eq!(recorder.caps(Hotkey::default()), Vec::<String>::new());
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        for key in [VK_CONTROL, VK_LCONTROL, VK_RCONTROL] {
            assert_eq!(recorder.key(key.0, ctrl), Heard::Nothing);
        }
        assert_eq!(recorder.caps(Hotkey::default()), ["Ctrl"]);
        let all = Mods {
            ctrl: true,
            alt: true,
            shift: true,
            win: true,
        };
        for key in [
            VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN, VK_LSHIFT, VK_RSHIFT, VK_LMENU, VK_RMENU,
        ] {
            assert_eq!(recorder.key(key.0, all), Heard::Nothing, "{key:?}");
        }
        assert_eq!(
            recorder.caps(Hotkey::default()),
            ["Win", "Ctrl", "Alt", "Shift"]
        );
        recorder.held(ctrl);
        assert_eq!(recorder.caps(Hotkey::default()), ["Ctrl"]);
        assert!(recorder.listening());
    }

    #[test]
    fn esc_alone_cancels_and_with_a_modifier_is_a_combination() {
        let mut recorder = listening();
        assert_eq!(recorder.key(VK_ESCAPE.0, Mods::default()), Heard::Cancelled);
        assert!(!recorder.listening());
        let mut recorder = listening();
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(
            recorder.key(VK_ESCAPE.0, ctrl),
            Heard::Hotkey(hotkey("Ctrl+Esc"))
        );
    }

    #[test]
    fn alt_f4_alone_closes_and_with_more_held_is_a_combination() {
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        let mut recorder = listening();
        assert_eq!(recorder.key(VK_F4.0, alt), Heard::Close);
        assert!(!recorder.listening());
        let mut recorder = listening();
        let shift_alt = Mods { shift: true, ..alt };
        assert_eq!(
            recorder.key(VK_F4.0, shift_alt),
            Heard::Hotkey(hotkey("Alt+Shift+F4"))
        );
        let mut recorder = listening();
        assert_eq!(
            recorder.key(VK_F4.0, Mods::default()),
            Heard::Hotkey(hotkey("F4"))
        );
    }

    #[test]
    fn a_combination_that_may_not_be_a_hotkey_says_why_and_listens_on() {
        let mut recorder = listening();
        let Heard::Wrong(why) = recorder.key(VK_SPACE.0, Mods::default()) else {
            panic!("refused");
        };
        assert!(why.contains("needs Ctrl, Alt or Win"), "{why}");
        assert!(recorder.listening());
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert!(matches!(recorder.key(VK_NUMPAD0.0, ctrl), Heard::Wrong(_)));
        assert!(recorder.listening());
    }

    #[test]
    fn idle_the_field_shows_the_hotkey_key_by_key() {
        let recorder = Recorder::default();
        assert_eq!(
            recorder.caps(hotkey("Ctrl+Shift+K")),
            ["Ctrl", "Shift", "K"]
        );
        assert_eq!(recorder.caps(hotkey("F4")), ["F4"]);
        assert_eq!(caps(hotkey("Ctrl+=")), ["Ctrl", "="]);
    }

    #[test]
    fn stopping_goes_back_to_idle() {
        let mut recorder = listening();
        recorder.stop();
        assert_eq!(recorder, Recorder::Idle);
    }
}
