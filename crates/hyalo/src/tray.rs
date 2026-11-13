//! The icon in the notification area: the way to the launcher besides
//! the hotkey, and the way out. A left click shows or hides the launcher;
//! a right click opens a menu built from a list of entries, so that what
//! the menu holds is decided here, apart from Win32.

use std::sync::OnceLock;

use hyalo_core::hotkey::Hotkey;
use hyalo_core::text::{Language, Text};
use hyalo_core::update::{Progress, Update};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, RegisterWindowMessageW,
    SM_CXSMICON, WM_APP,
};
use windows::core::{PCWSTR, Result, w};

use crate::popup::{self, Side};

/// What a menu entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show,
    Settings,
    StartWithWindows,
    Exit,
    Install,
}

/// One entry of the menu.
pub type Entry = popup::Entry<Command>;

/// A line of the menu.
pub type Line = popup::Line<Command>;

/// The menu: Show (with the hotkey that does it, if one is registered),
/// the install of a newer release, if there is one (grayed while it
/// installs), Settings, Start with Windows (checked when hyalo starts
/// with Windows; grayed when `starts` is `None`, as for a build that may
/// not start with Windows), Exit; in `language`.
pub fn menu(
    hotkey: Option<Hotkey>,
    starts: Option<bool>,
    update: Option<&Update>,
    language: Language,
) -> Vec<Line> {
    let show = language.say(Text::Show);
    let show = match hotkey {
        // A tab puts the keys on the right, as menus show shortcuts.
        Some(hotkey) => format!("{show}\t{hotkey}"),
        None => show.to_string(),
    };
    let start = Line::Entry(Entry {
        command: Command::StartWithWindows,
        label: language.say(Text::StartWithWindows).to_string(),
        checked: starts == Some(true),
        enabled: starts.is_some(),
    });
    let install = update.map(|update| install(update, language));
    let show = Line::entry(Command::Show, show);
    let lines = [Some(show), install, Some(Line::Separator)]
        .into_iter()
        .flatten();
    lines
        .chain([
            Line::entry(Command::Settings, language.say(Text::Settings).to_string()),
            start,
            Line::Separator,
            Line::entry(Command::Exit, language.say(Text::Exit).to_string()),
        ])
        .collect()
}

/// The entry that installs `update`, grayed while it installs.
fn install(update: &Update, language: Language) -> Line {
    let installing = update.progress == Progress::Installing;
    let label = match installing {
        true => Text::Installing,
        false => Text::InstallUpdate,
    };
    Line::Entry(Entry {
        command: Command::Install,
        label: language.fill(label, &[&update.release.version.to_string()]),
        checked: false,
        enabled: !installing,
    })
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
        // Above the point: the taskbar is at the bottom.
        popup::track(window, lines, at, Side::Above)
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

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::update::{Progress, Release, Update, Version};

    fn update(progress: Progress) -> Update {
        Update {
            release: Release {
                version: Version::parse("0.0.2").expect("a version"),
                notes: String::new(),
                exe: String::new(),
                sum: String::new(),
            },
            progress,
        }
    }

    #[test]
    fn a_newer_release_puts_its_install_under_show() {
        let offered = update(Progress::Offered);
        let lines = menu(None, None, Some(&offered), Language::English);
        let install = entry(&lines, Command::Install);
        assert_eq!(install.label, "Install hyalo 0.0.2");
        assert!(install.enabled);
        assert!(!install.checked, "a command, not a choice");
        assert_eq!(lines[1], Line::Entry(install), "right under Show");
        assert_eq!(lines.len(), 7);
        let installing = update(Progress::Installing);
        let lines = menu(None, None, Some(&installing), Language::Russian);
        let install = entry(&lines, Command::Install);
        assert_eq!(install.label, "Устанавливается hyalo 0.0.2…");
        assert!(!install.enabled, "once is enough");
        let lines = menu(None, None, None, Language::English);
        let commands = lines.iter().filter_map(|line| match line {
            Line::Entry(entry) => Some(entry.command),
            Line::Separator => None,
        });
        assert!(!commands.collect::<Vec<_>>().contains(&Command::Install));
    }
    use crate::popup::tests::{FirstLook, menu_up};
    use crate::test_desktop::on_hidden_desktop;
    use std::cell::Cell;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
        ReleaseDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, EndMenu, GetIconInfo, ICONINFO, KillTimer, SetTimer,
        WINDOW_EX_STYLE, WINDOW_STYLE,
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
        let lines = menu(alt_space(), Some(false), None, Language::English);
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
    fn the_menu_speaks_russian_in_russian() {
        let lines = menu(alt_space(), Some(false), None, Language::Russian);
        let labels: Vec<&str> = lines
            .iter()
            .filter_map(|line| match line {
                Line::Entry(entry) => Some(entry.label.as_str()),
                Line::Separator => None,
            })
            .collect();
        let expected = [
            "Показать hyalo\tAlt+Space",
            "Параметры",
            "Запускать вместе с Windows",
            "Выход",
        ];
        assert_eq!(labels, expected);
        let show = entry(&menu(None, None, None, Language::Russian), Command::Show);
        assert_eq!(show.label, "Показать hyalo");
    }

    #[test]
    fn show_names_no_hotkey_when_none_is_registered() {
        assert_eq!(
            entry(&menu(None, None, None, Language::English), Command::Show).label,
            "Show hyalo"
        );
        let ctrl_k = Hotkey::parse("Ctrl+K").ok();
        let show = entry(&menu(ctrl_k, None, None, Language::English), Command::Show);
        assert_eq!(show.label, "Show hyalo\tCtrl+K");
    }

    #[test]
    fn start_with_windows_is_checked_when_it_does_and_grayed_when_it_may_not() {
        let on = entry(
            &menu(alt_space(), Some(true), None, Language::English),
            Command::StartWithWindows,
        );
        assert!(on.checked && on.enabled);
        let off = entry(
            &menu(alt_space(), Some(false), None, Language::English),
            Command::StartWithWindows,
        );
        assert!(!off.checked && off.enabled);
        let unavailable = entry(
            &menu(alt_space(), None, None, Language::English),
            Command::StartWithWindows,
        );
        assert!(!unavailable.checked && !unavailable.enabled);
        let others = [Command::Show, Command::Settings, Command::Exit];
        for command in others {
            assert!(
                !entry(
                    &menu(alt_space(), Some(true), None, Language::English),
                    command
                )
                .checked
            );
        }
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

    thread_local! {
        static TICKS: Cell<u32> = const { Cell::new(0) };
    }

    /// Ticks inside the real menu's own loop: closes the menu once it is
    /// up, picking nothing; gives up after a second.
    extern "system" fn close_the_menu(window: HWND, _: u32, id: usize, _: u32) {
        TICKS.set(TICKS.get() + 1);
        if menu_up().is_some() || TICKS.get() > 100 {
            let _ = unsafe { KillTimer(Some(window), id) };
            let _ = unsafe { EndMenu() };
        }
    }

    /// Reported on 0.2.7: the tray's menu came up as a blank light-grey
    /// rectangle, each entry showing only once the pointer went over it.
    /// Windows fades menus in unless told not to, and on Windows 11 26H2
    /// (build 26300) a menu faded in never shows what it drew, in any
    /// app's menu (PowerShell's too); one shown at once shows it all.
    #[test]
    fn the_tray_menu_comes_up_whole_rather_than_fading_in() {
        on_hidden_desktop(|| {
            let window = window();
            let look = FirstLook::new();
            unsafe { SetTimer(Some(window), 1, 10, Some(close_the_menu)) };
            let lines = menu(alt_space(), Some(false), None, Language::English);
            let picked = Shell.menu(window, &lines, POINT { x: 400, y: 400 });
            unsafe { DestroyWindow(window) }.expect("destroy");
            assert_eq!(picked, None, "closed with nothing picked");
            assert_eq!(look.fading(), Some(false), "up at once, not fading in");
        });
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
