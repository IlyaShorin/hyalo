//! Global hotkeys, which Windows holds for the whole session: one taken on
//! any desktop is taken on all of them, the hidden test desktop too. So the
//! window registers through `Hotkeys`, and tests hand it a stand-in rather
//! than take the user's keys.

use hyalo_core::hotkey::Hotkey;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::core::Result;

/// Registers and frees global hotkeys for a window.
pub trait Hotkeys {
    /// Makes `hotkey` send `window` `WM_HOTKEY` with `id`. Windows refuses
    /// a hotkey another app holds, and some it keeps for itself.
    fn register(&self, window: HWND, id: i32, hotkey: Hotkey) -> Result<()>;
    /// Frees the hotkey registered as `id`.
    fn unregister(&self, window: HWND, id: i32);
}

/// The hotkeys Windows holds.
pub struct System;

impl Hotkeys for System {
    fn register(&self, window: HWND, id: i32, hotkey: Hotkey) -> Result<()> {
        // No repeats: holding the keys down toggles the launcher once.
        let modifiers = HOT_KEY_MODIFIERS(hotkey.modifiers()) | MOD_NOREPEAT;
        let key = u32::from(hotkey.key());
        // SAFETY: a plain call for a window of this thread.
        unsafe { RegisterHotKey(Some(window), id, modifiers, key) }
    }

    fn unregister(&self, window: HWND, id: i32) {
        // SAFETY: as above; freeing what is not registered does nothing.
        let _ = unsafe { UnregisterHotKey(Some(window), id) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_desktop::on_hidden_desktop;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
    };
    use windows::core::w;

    /// A plain window that is never shown.
    fn window() -> HWND {
        let (ex, style) = (WINDOW_EX_STYLE(0), WINDOW_STYLE(0));
        unsafe {
            CreateWindowExW(
                ex,
                w!("STATIC"),
                w!(""),
                style,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .expect("a window")
    }

    #[test]
    fn a_hotkey_one_window_holds_is_refused_to_another_until_freed() {
        on_hidden_desktop(|| {
            // Keys no one presses: the test holds them for the whole session
            // for a moment.
            let rare = Hotkey::parse("Ctrl+Alt+Shift+Win+F24").expect("a hotkey");
            let (one, other) = (window(), window());
            System.register(one, 1, rare).expect("register");
            assert!(System.register(other, 1, rare).is_err(), "taken");
            System.unregister(one, 1);
            System.register(other, 1, rare).expect("freed");
            System.unregister(other, 1);
            unsafe {
                DestroyWindow(one).expect("destroy");
                DestroyWindow(other).expect("destroy");
            }
        });
    }
}
