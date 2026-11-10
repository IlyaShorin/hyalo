//! The icon in the notification area: the way to the launcher besides
//! the hotkey, and the way out. A left click shows or hides the launcher;
//! a right click opens a menu built from a list of entries, so that what
//! the menu holds is decided here, apart from Win32.

use std::sync::OnceLock;

use hyalo_core::hotkey::Hotkey;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, HICON, HMENU, IMAGE_ICON,
    LR_DEFAULTCOLOR, LoadImageW, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    PostMessageW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_APP, WM_NULL,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

/// What a menu entry does. Its number is the id Win32 gives back for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show = 1,
    Settings = 2,
    StartWithWindows = 3,
    Exit = 4,
}

impl Command {
    /// The command a menu sent as `id`, if it is one of these.
    pub fn from_id(id: usize) -> Option<Command> {
        let all = [
            Command::Show,
            Command::Settings,
            Command::StartWithWindows,
            Command::Exit,
        ];
        all.into_iter().find(|&command| command as usize == id)
    }

    fn entry(self, label: String) -> Line {
        Line::Entry(Entry {
            command: self,
            label,
            checked: false,
            enabled: true,
        })
    }
}

/// One entry of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub command: Command,
    pub label: String,
    pub checked: bool,
    pub enabled: bool,
}

/// A line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Entry(Entry),
    Separator,
}

/// The menu: Show (with the hotkey that does it, if one is registered),
/// Settings, Start with Windows (checked when hyalo starts with Windows;
/// grayed when `starts` is `None`, as for a build that may not start with
/// Windows), Exit.
pub fn menu(hotkey: Option<Hotkey>, starts: Option<bool>) -> Vec<Line> {
    let show = match hotkey {
        // A tab puts the keys on the right, as menus show shortcuts.
        Some(hotkey) => format!("Show hyalo\t{hotkey}"),
        None => "Show hyalo".to_string(),
    };
    let start = Line::Entry(Entry {
        command: Command::StartWithWindows,
        label: "Start with Windows".to_string(),
        checked: starts == Some(true),
        enabled: starts.is_some(),
    });
    vec![
        Command::Show.entry(show),
        Line::Separator,
        Command::Settings.entry("Settings".to_string()),
        start,
        Line::Separator,
        Command::Exit.entry("Exit".to_string()),
    ]
}

/// The longest tooltip the notification area shows, in UTF-16 units.
const TIP_UNITS: usize = 127;

/// The tooltip: hyalo and its hotkey, or what is wrong, if anything is.
pub fn tip(hotkey: Option<Hotkey>, problem: Option<&str>) -> String {
    let said = match (problem, hotkey) {
        (Some(problem), _) => format!("hyalo · {problem}"),
        (None, Some(hotkey)) => format!("hyalo · {hotkey}"),
        (None, None) => "hyalo".to_string(),
    };
    if said.encode_utf16().count() <= TIP_UNITS {
        return said;
    }
    // Room for the ellipsis, one unit.
    let mut units = 0;
    let room = |letter: &char| {
        units += letter.len_utf16();
        units < TIP_UNITS
    };
    said.chars().take_while(room).collect::<String>() + "…"
}

/// What the notification area tells the window, in `WM_TRAY`.
pub const WM_TRAY: u32 = WM_APP + 3;

/// The message Explorer broadcasts when the taskbar is (back) up: every
/// icon must be added again.
pub fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    // SAFETY: a plain call with a constant string.
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

/// What the window asks of the notification area: the real one in
/// `Shell`, a stand-in in tests.
pub trait Tray {
    /// Shows hyalo's icon with `tip`, adding it when the taskbar has none
    /// yet: at start, after Explorer restarts. Whether the taskbar took
    /// it: at logon it may not be up yet, and TaskbarCreated says when.
    fn show(&self, window: HWND, tip: &str) -> bool;
    /// Takes the icon away.
    fn remove(&self, window: HWND);
    /// Opens the menu at `at`, in screen pixels, until the user picks an
    /// entry or clicks away: what was picked, if anything.
    fn menu(&self, window: HWND, lines: &[Line], at: POINT) -> Option<Command>;
}

/// The notification area of the taskbar.
pub struct Shell;

/// The one icon hyalo shows.
const ICON_ID: u32 = 1;

impl Tray for Shell {
    fn show(&self, window: HWND, tip: &str) -> bool {
        let icon = icon(small_icon()).ok();
        let data = data(window, icon, tip);
        // SAFETY: `data` is a live NOTIFYICONDATAW sized as it says; the
        // shell copies the icon, so it is destroyed afterwards.
        unsafe {
            let shown = Shell_NotifyIconW(NIM_MODIFY, &data).as_bool()
                || Shell_NotifyIconW(NIM_ADD, &data).as_bool()
                    && Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool();
            let _ = icon.map(|icon| DestroyIcon(icon));
            shown
        }
    }

    fn remove(&self, window: HWND) {
        // SAFETY: as in `show`; an icon that is not there is not removed.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data(window, None, "")) };
    }

    fn menu(&self, window: HWND, lines: &[Line], at: POINT) -> Option<Command> {
        let Ok(menu) = build(lines) else {
            eprintln!("hyalo: cannot build the tray menu");
            return None;
        };
        // SAFETY: `menu` is ours and destroyed once. The window must be in
        // the foreground for the menu to close when the user clicks
        // elsewhere; WM_NULL afterwards lets it close on the first click.
        unsafe {
            let _ = SetForegroundWindow(window);
            // Above and to the right of the point, which Windows moves to
            // fit the screen; the command comes back here rather than as
            // WM_COMMAND.
            let flags =
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RETURNCMD | TPM_NONOTIFY;
            let picked = TrackPopupMenuEx(menu, flags.0, at.x, at.y, window, None);
            let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            Command::from_id(picked.0 as usize)
        }
    }
}

/// The notification data for hyalo's icon in `window`.
fn data(window: HWND, icon: Option<HICON>, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    if let Some(icon) = icon {
        data.uFlags |= NIF_ICON;
        data.hIcon = icon;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let units = tip.encode_utf16().take(data.szTip.len() - 1);
    for (slot, unit) in data.szTip.iter_mut().zip(units) {
        *slot = unit;
    }
    data
}

/// The side of a small icon, as the taskbar shows them.
fn small_icon() -> i32 {
    // SAFETY: plain calls.
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) }
}

/// hyalo's icon, `side` pixels square, from the executable's resources.
/// The caller destroys it.
pub fn icon(side: i32) -> Result<HICON> {
    // SAFETY: the module is this executable; the resource is named by
    // number, as MAKEINTRESOURCE does.
    unsafe {
        let module = GetModuleHandleW(None)?;
        let name = PCWSTR(ICON_ID as usize as *const u16);
        let image = LoadImageW(
            Some(module.into()),
            name,
            IMAGE_ICON,
            side,
            side,
            LR_DEFAULTCOLOR,
        )?;
        Ok(HICON(image.0))
    }
}

/// A popup menu holding `lines`. The caller destroys it.
fn build(lines: &[Line]) -> Result<HMENU> {
    // SAFETY: the menu is new; each label outlives the call that copies it.
    unsafe {
        let menu = CreatePopupMenu()?;
        for line in lines {
            match line {
                Line::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?,
                Line::Entry(entry) => {
                    let label = HSTRING::from(entry.label.as_str());
                    AppendMenuW(menu, flags(entry), entry.command as usize, &label)?
                }
            }
        }
        Ok(menu)
    }
}

fn flags(entry: &Entry) -> MENU_ITEM_FLAGS {
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
mod tests {
    use super::*;
    use crate::test_desktop::on_hidden_desktop;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
        ReleaseDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetIconInfo, GetMenuItemCount, GetMenuItemID, GetMenuState,
        GetMenuStringW, ICONINFO, MF_BYPOSITION, WINDOW_EX_STYLE, WINDOW_STYLE,
    };

    fn alt_space() -> Option<Hotkey> {
        Some(Hotkey::default())
    }

    fn entry(lines: &[Line], command: Command) -> Entry {
        let entries = lines.iter().filter_map(|line| match line {
            Line::Entry(entry) => Some(entry),
            Line::Separator => None,
        });
        let mut found = entries.filter(|entry| entry.command == command);
        found.next().expect("an entry").clone()
    }

    #[test]
    fn the_menu_shows_settings_start_with_windows_and_exit() {
        let lines = menu(alt_space(), Some(false));
        let labels: Vec<Option<&str>> = lines
            .iter()
            .map(|line| match line {
                Line::Entry(entry) => Some(entry.label.as_str()),
                Line::Separator => None,
            })
            .collect();
        let expected = [
            Some("Show hyalo\tAlt+Space"),
            None,
            Some("Settings"),
            Some("Start with Windows"),
            None,
            Some("Exit"),
        ];
        assert_eq!(labels, expected);
        let enabled = lines.iter().all(|line| match line {
            Line::Entry(entry) => entry.enabled,
            Line::Separator => true,
        });
        assert!(enabled);
    }

    #[test]
    fn show_names_no_hotkey_when_none_is_registered() {
        assert_eq!(entry(&menu(None, None), Command::Show).label, "Show hyalo");
        let ctrl_k = Hotkey::parse("Ctrl+K").ok();
        let show = entry(&menu(ctrl_k, None), Command::Show);
        assert_eq!(show.label, "Show hyalo\tCtrl+K");
    }

    #[test]
    fn start_with_windows_is_checked_when_it_does_and_grayed_when_it_may_not() {
        let on = entry(&menu(alt_space(), Some(true)), Command::StartWithWindows);
        assert!(on.checked && on.enabled);
        let off = entry(&menu(alt_space(), Some(false)), Command::StartWithWindows);
        assert!(!off.checked && off.enabled);
        let unavailable = entry(&menu(alt_space(), None), Command::StartWithWindows);
        assert!(!unavailable.checked && !unavailable.enabled);
        let others = [Command::Show, Command::Settings, Command::Exit];
        for command in others {
            assert!(!entry(&menu(alt_space(), Some(true)), command).checked);
        }
    }

    #[test]
    fn a_command_comes_back_from_its_number() {
        let all = [
            Command::Show,
            Command::Settings,
            Command::StartWithWindows,
            Command::Exit,
        ];
        for command in all {
            assert_eq!(Command::from_id(command as usize), Some(command));
        }
        assert_eq!(Command::from_id(0), None);
        assert_eq!(Command::from_id(5), None);
    }

    #[test]
    fn the_tip_names_the_hotkey_or_what_is_wrong() {
        assert_eq!(tip(alt_space(), None), "hyalo · Alt+Space");
        assert_eq!(tip(None, None), "hyalo");
        let problem = "config.toml, line 3: unknown field `hotkye`";
        assert_eq!(
            tip(alt_space(), Some(problem)),
            format!("hyalo · {problem}")
        );
    }

    #[test]
    fn a_long_tip_is_cut_short_to_fit_with_an_ellipsis() {
        let problem = "ж".repeat(300);
        let shown = tip(alt_space(), Some(&problem));
        assert_eq!(shown.encode_utf16().count(), TIP_UNITS);
        assert!(shown.starts_with("hyalo · жж") && shown.ends_with('…'));
        let room = TIP_UNITS - "hyalo · ".encode_utf16().count();
        let exact = "x".repeat(room);
        assert_eq!(
            tip(None, Some(&exact)),
            format!("hyalo · {exact}"),
            "just fits"
        );
        let over = "x".repeat(room + 1);
        assert!(tip(None, Some(&over)).ends_with('…'), "one more does not");
    }

    /// The menu Win32 holds: each item's label, or `None` for a
    /// separator, with whether it is checked and grayed.
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

    /// A plain window that is never shown.
    fn window() -> HWND {
        let (ex, style) = (WINDOW_EX_STYLE(0), WINDOW_STYLE(0));
        let none = (None, None, None, None);
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
                none.0,
                none.1,
                none.2,
                none.3,
            )
        }
        .expect("a window")
    }

    #[test]
    fn a_desktop_with_no_taskbar_takes_no_icon() {
        on_hidden_desktop(|| {
            let window = window();
            assert!(!Shell.show(window, "hyalo · Alt+Space"));
            Shell.remove(window);
            unsafe { DestroyWindow(window) }.expect("destroy");
        });
    }

    #[test]
    fn the_icon_tells_its_window_with_the_tray_message_and_shows_its_tip() {
        let icon = icon(16).expect("the icon");
        let data = data(HWND::default(), Some(icon), "hyalo · Alt+Space");
        unsafe { DestroyIcon(icon) }.expect("destroy the icon");
        assert_eq!(data.uCallbackMessage, WM_TRAY);
        assert_eq!(data.uID, ICON_ID);
        assert_eq!(data.uFlags, NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP | NIF_ICON);
        assert_eq!(unsafe { data.Anonymous.uVersion }, NOTIFYICON_VERSION_4);
        let tip: Vec<u16> = "hyalo · Alt+Space\0".encode_utf16().collect();
        assert_eq!(&data.szTip[..tip.len()], &tip[..]);
        let bare = data_without_icon();
        assert_eq!(bare.uFlags, NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP);
    }

    fn data_without_icon() -> NOTIFYICONDATAW {
        data(HWND::default(), None, "")
    }

    #[test]
    fn a_tip_too_long_for_the_shell_is_cut_and_still_ends() {
        let long = "x".repeat(300);
        let data = data(HWND::default(), None, &long);
        assert_eq!(data.szTip[data.szTip.len() - 1], 0);
        assert_eq!(data.szTip[data.szTip.len() - 2], u16::from(b'x'));
    }

    #[test]
    fn builds_the_win32_menu_the_lines_describe() {
        let built = |lines: &[Line]| {
            let built = build(lines).expect("a menu");
            let items = read_back(built);
            unsafe { DestroyMenu(built) }.expect("destroy the menu");
            items
        };
        let items = built(&menu(alt_space(), Some(true)));
        let show = (Some("Show hyalo	Alt+Space".to_string()), 1, false, false);
        assert_eq!(items[0], show);
        assert_eq!(items[1].0, None, "a separator");
        assert_eq!(items[1].1, 0, "which sends no command");
        let start = (Some("Start with Windows".to_string()), 3, true, false);
        assert_eq!(items[3], start);
        assert_eq!(items[5], (Some("Exit".to_string()), 4, false, false));
        assert!(built(&menu(alt_space(), None))[3].3, "grayed");
    }

    /// The pixel at (x, y) of `icon`, as BGRA.
    fn pixel(icon: HICON, x: usize, y: usize) -> (i32, [u8; 4]) {
        let mut info = ICONINFO::default();
        unsafe { GetIconInfo(icon, &mut info) }.expect("icon info");
        let mut header = BITMAPINFO::default();
        header.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        let dc = unsafe { GetDC(None) };
        unsafe { GetDIBits(dc, info.hbmColor, 0, 0, None, &mut header, DIB_RGB_COLORS) };
        let side = header.bmiHeader.biWidth;
        header.bmiHeader.biBitCount = 32;
        header.bmiHeader.biCompression = BI_RGB.0;
        header.bmiHeader.biHeight = -side;
        let mut bgra = vec![0_u8; (side * side * 4) as usize];
        let lines = side as u32;
        let buffer = Some(bgra.as_mut_ptr().cast());
        unsafe {
            GetDIBits(
                dc,
                info.hbmColor,
                0,
                lines,
                buffer,
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        unsafe {
            ReleaseDC(None, dc);
            let _ = DeleteObject(info.hbmColor.into());
            let _ = DeleteObject(info.hbmMask.into());
        }
        let at = (y * side as usize + x) * 4;
        (side, bgra[at..at + 4].try_into().expect("4 bytes"))
    }

    #[test]
    fn the_executable_carries_the_glass_lens_at_every_size_asked() {
        for side in [16, 20, 24, 32, 48, 256] {
            let icon = icon(side).expect("the icon resource");
            // The middle of the lens: blue glass, solid.
            let middle = side as usize * 110 / 256;
            let (loaded, [blue, _, red, alpha]) = pixel(icon, middle, middle);
            unsafe { DestroyIcon(icon) }.expect("destroy the icon");
            assert_eq!(loaded, side);
            assert!(
                blue > 200 && red < 120 && alpha > 240,
                "{side}: {blue} {red} {alpha}"
            );
        }
    }

    #[test]
    fn the_icon_is_clear_in_its_corners() {
        let icon = icon(32).expect("the icon resource");
        let (_, [.., corner]) = pixel(icon, 1, 30);
        unsafe { DestroyIcon(icon) }.expect("destroy the icon");
        assert_eq!(corner, 0);
    }
}
