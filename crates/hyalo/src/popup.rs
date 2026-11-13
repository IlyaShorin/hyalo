//! A popup menu, as the tray's icon and the launcher's rows open them:
//! lines of entries, each standing for a command its caller names, shown
//! by Win32 until the user picks one or clicks away. What a menu holds is
//! decided by its caller, apart from Win32.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, HMENU, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_NOANIMATION, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, TrackPopupMenuEx,
    WM_NULL,
};
use windows::core::{HSTRING, PCWSTR, Result};

/// One entry of a menu: what it stands for and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<C> {
    pub command: C,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
}

/// A line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line<C> {
    Entry(Entry<C>),
    Separator,
}

impl<C> Line<C> {
    /// An entry that says `label` and stands for `command`, neither
    /// checked nor grayed.
    pub fn entry(command: C, label: String) -> Self {
        Line::Entry(Entry {
            command,
            label,
            checked: false,
            enabled: true,
        })
    }
}

/// Which way a menu goes from the point it opens at: up, as from the
/// taskbar, or down, as from a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
}

/// Opens a menu of `lines` for `window` at `at`, in screen pixels, on
/// `side` of it and to the right (Windows moves it to fit the screen),
/// until the user picks an entry or clicks away: what the entry picked
/// stands for, if any. Runs a message loop of its own meanwhile.
pub fn track<C: Clone>(window: HWND, lines: &[Line<C>], at: POINT, side: Side) -> Option<C> {
    let Ok(menu) = build(lines) else {
        eprintln!("hyalo: cannot build a menu");
        return None;
    };
    let side = match side {
        Side::Above => TPM_BOTTOMALIGN,
        Side::Below => TPM_TOPALIGN,
    };
    // The command comes back here rather than as WM_COMMAND. No fade:
    // on Windows 11 26H2 a menu faded in stays blank until the pointer
    // goes over its entries, in every app; one shown at once does not.
    let flags = side | TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY;
    let flags = flags | TPM_NOANIMATION;
    // SAFETY: `menu` is ours and destroyed once. The window must be in
    // the foreground for the menu to close when the user clicks
    // elsewhere; WM_NULL afterwards lets it close on the first click.
    let id = unsafe {
        let _ = SetForegroundWindow(window);
        let id = TrackPopupMenuEx(menu, flags.0, at.x, at.y, window, None);
        // WM_NULL carries nothing in its parameters.
        let none = (WPARAM::default(), LPARAM::default());
        let _ = PostMessageW(Some(window), WM_NULL, none.0, none.1);
        let _ = DestroyMenu(menu);
        id
    };
    picked(lines, id.0 as usize)
}

/// What the entry Win32 gave back as `id` stands for: `build` numbers
/// each line by its place, from 1, as 0 means nothing was picked.
fn picked<C: Clone>(lines: &[Line<C>], id: usize) -> Option<C> {
    match lines.get(id.checked_sub(1)?)? {
        Line::Entry(entry) => Some(entry.command.clone()),
        Line::Separator => None,
    }
}

/// A popup menu holding `lines`, each numbered as `picked` reads it. The
/// caller destroys it.
fn build<C>(lines: &[Line<C>]) -> Result<HMENU> {
    // SAFETY: the menu is new; each label outlives the call that copies it.
    unsafe {
        let menu = CreatePopupMenu()?;
        for (at, line) in lines.iter().enumerate() {
            match line {
                Line::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?,
                Line::Entry(entry) => {
                    let label = HSTRING::from(entry.label.as_str());
                    AppendMenuW(menu, flags(entry), at + 1, &label)?
                }
            }
        }
        Ok(menu)
    }
}

fn flags<C>(entry: &Entry<C>) -> MENU_ITEM_FLAGS {
    let mut flags = MF_STRING;
    if entry.checked {
        flags |= MF_CHECKED;
    }
    if !entry.enabled {
        flags |= MF_GRAYED;
    }
    flags
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_desktop::take_turn;
    use std::cell::Cell;
    use std::fs::File;
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, FindWindowExW, GWL_EXSTYLE, GetMenuItemCount, GetMenuItemID, GetMenuState,
        GetMenuStringW, GetWindowLongW, GetWindowThreadProcessId, HHOOK, MF_BYPOSITION, MSGF_MENU,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_MSGFILTER, WS_EX_LAYERED,
    };
    use windows::core::w;

    thread_local! {
        /// Whether the menu was fading in at its loop's first message.
        static FADING: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// The turn to show a real menu, held until the file is dropped.
    /// Windows fades one menu at a time in the whole session: a menu that
    /// another test shows meanwhile, in this process or another, would
    /// come up without the fade a test may be looking for.
    pub fn menu_turn() -> File {
        take_turn("menu")
    }

    /// A look at the menu this thread shows next, taken as its own loop
    /// takes its first message, before handling any: whether it is fading
    /// in. Windows makes a menu layered while it fades in, where menus
    /// fade (its default), and plain once it is in. A timer's look comes
    /// only after the messages queued before it are handled, by when the
    /// fade may be over. Holds the menu's turn.
    pub struct FirstLook {
        hook: HHOOK,
        _turn: File,
    }

    impl FirstLook {
        pub fn new() -> Self {
            let turn = menu_turn();
            FADING.set(None);
            // SAFETY: a hook of this thread's alone, on a function of
            // this module; unhooked on drop.
            let hook = unsafe {
                let thread = GetCurrentThreadId();
                SetWindowsHookExW(WH_MSGFILTER, Some(first_message), None, thread)
            };
            let hook = hook.expect("a message filter");
            Self { hook, _turn: turn }
        }

        /// Whether the menu was fading in, if one came up.
        pub fn fading(&self) -> Option<bool> {
            FADING.get()
        }
    }

    impl Drop for FirstLook {
        fn drop(&mut self) {
            let _ = unsafe { UnhookWindowsHookEx(self.hook) };
        }
    }

    /// Called by a menu's loop with each message it takes.
    extern "system" fn first_message(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let menu = (code == MSGF_MENU as i32 && FADING.get().is_none())
            .then(menu_up)
            .flatten();
        if let Some(menu) = menu {
            let style = unsafe { GetWindowLongW(menu, GWL_EXSTYLE) } as u32;
            FADING.set(Some(style & WS_EX_LAYERED.0 != 0));
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// The popup menu this thread has up, if any.
    pub fn menu_up() -> Option<HWND> {
        let mut after = None;
        loop {
            let menu = unsafe { FindWindowExW(None, after, w!("#32768"), PCWSTR::null()) }.ok()?;
            let thread = unsafe { GetWindowThreadProcessId(menu, None) };
            if thread == unsafe { GetCurrentThreadId() } {
                return Some(menu);
            }
            after = Some(menu);
        }
    }

    /// The menu Win32 holds: each item's label, or `None` for a
    /// separator, with its id and whether it is checked and grayed.
    fn read_back(menu: HMENU) -> Vec<(Option<String>, usize, bool, bool)> {
        let count = unsafe { GetMenuItemCount(Some(menu)) };
        (0..count as u32)
            .map(|at| {
                let mut label = [0_u16; 64];
                let length = unsafe { GetMenuStringW(menu, at, Some(&mut label), MF_BYPOSITION) };
                let state = unsafe { GetMenuState(menu, at, MF_BYPOSITION) };
                let id = unsafe { GetMenuItemID(menu, at as i32) } as usize;
                let text =
                    (length > 0).then(|| String::from_utf16_lossy(&label[..length as usize]));
                let checked = state & MF_CHECKED.0 != 0;
                let grayed = state & MF_GRAYED.0 != 0;
                (text, id, checked, grayed)
            })
            .collect()
    }

    /// Show, a line, a checked entry, a grayed one: what tells each
    /// apart.
    fn lines() -> Vec<Line<char>> {
        vec![
            Line::entry('s', "Show hyalo\tAlt+Space".to_string()),
            Line::Separator,
            Line::Entry(Entry {
                command: 'w',
                label: "Start with Windows".to_string(),
                checked: true,
                enabled: true,
            }),
            Line::Entry(Entry {
                command: 'i',
                label: "Installing".to_string(),
                checked: false,
                enabled: false,
            }),
        ]
    }

    #[test]
    fn builds_the_win32_menu_the_lines_describe() {
        let built = build(&lines()).expect("a menu");
        let items = read_back(built);
        unsafe { DestroyMenu(built) }.expect("destroy the menu");
        let show = (Some("Show hyalo\tAlt+Space".to_string()), 1, false, false);
        assert_eq!(items[0], show);
        assert_eq!(items[1].0, None, "a separator");
        assert_eq!(items[1].1, 0, "which sends no command");
        let start = (Some("Start with Windows".to_string()), 3, true, false);
        assert_eq!(items[2], start);
        assert_eq!(items[3], (Some("Installing".to_string()), 4, false, true));
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn the_id_win32_gives_back_names_the_entry_built_with_it() {
        let lines = lines();
        assert_eq!(picked(&lines, 1), Some('s'));
        assert_eq!(picked(&lines, 3), Some('w'));
        assert_eq!(picked(&lines, 4), Some('i'));
        assert_eq!(picked(&lines, 0), None, "nothing picked");
        assert_eq!(picked(&lines, 2), None, "a separator picks nothing");
        assert_eq!(picked(&lines, 5), None, "no such entry");
    }
}
