//! The open windows for the window switcher, found the way Alt+Tab finds
//! them, and switching to one. Which window counts is
//! `hyalo_core::switcher`'s to decide; this reads what Windows says.
//!
//! Reading a window's title, styles and process sends it no message, so
//! a window that hangs cannot hold the launcher up.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use hyalo_core::item::ItemId;
use hyalo_core::switcher::{self, Seen};
use windows::Win32::Foundation::{
    CloseHandle, E_ACCESSDENIED, ERROR_INVALID_WINDOW_HANDLE, HWND, LPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx,
    GW_OWNER, GWL_EXSTYLE, GetShellWindow, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    InternalGetWindowText, IsIconic, IsWindow, IsWindowVisible, SW_RESTORE, SetForegroundWindow,
    ShowWindowAsync, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, HSTRING, PWSTR, Result};

use crate::apps::{self, Icon};
use crate::catalog::{Found, Provider, Request};
use crate::com::Com;

/// An open window the switcher lists.
#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    /// Its handle, as a number: a handle may not cross threads.
    pub window: isize,
    pub title: String,
    /// The program it belongs to, when Windows says.
    pub exe: Option<String>,
}

/// The windows provider: the open windows, front first, then the icons
/// of their programs the launcher lacks.
pub fn provider() -> Provider {
    // SAFETY: a plain call.
    let ours = unsafe { GetCurrentProcessId() };
    Arc::new(move |request, report| find(ours, request, report))
}

/// Reports the open windows not of process `ours`.
fn find(ours: u32, request: &Request, report: &dyn Fn(Found)) {
    report_windows(&windows(ours), request, report);
}

/// Reports `open` as `Found::Windows`, then each one's icon the request
/// does not know, its program's, as `Found::Icon`.
fn report_windows(open: &[Open], request: &Request, report: &dyn Fn(Found)) {
    let items = open.iter().map(|open| {
        let app = open.exe.as_deref().map(app_name).unwrap_or_default();
        switcher::item(open.window, &open.title, &app)
    });
    let items: Vec<_> = items.collect();
    report(Found::Windows(items.clone()));
    let Ok(_com) = Com::init() else {
        return;
    };
    let mut icons: HashMap<&str, Option<Icon>> = HashMap::new();
    for (open, item) in open.iter().zip(items) {
        let Some(exe) = open.exe.as_deref() else {
            continue;
        };
        if request.known.contains(&item.id) {
            continue;
        }
        let icon = icons
            .entry(exe)
            .or_insert_with(|| apps::icon_at(exe, request.icon_size).ok());
        if let Some(icon) = icon {
            report(Found::Icon(ItemId(item.id.0), icon.clone()));
        }
    }
}

/// The top-level windows of this thread's desktop Alt+Tab would show,
/// front first, leaving out process `ours`'s.
pub fn windows(ours: u32) -> Vec<Open> {
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes to `all`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut all as *mut Vec<HWND> as isize)) };
    // SAFETY: a plain call.
    let desktop = unsafe { GetShellWindow() };
    all.into_iter()
        .filter_map(|window| open(window, ours, desktop))
        .collect()
}

extern "system" fn each(window: HWND, all: LPARAM) -> BOOL {
    // SAFETY: `all` is the `Vec` `windows` passed, alive for the call.
    let all = unsafe { &mut *(all.0 as *mut Vec<HWND>) };
    all.push(window);
    BOOL::from(true)
}

/// `window` as the switcher lists it, if it lists it.
fn open(window: HWND, ours: u32, desktop: HWND) -> Option<Open> {
    let title = title(window);
    let process = process_of(window);
    let seen = seen(
        window,
        !title.is_empty(),
        process == ours || window == desktop,
    );
    if !switcher::switchable(seen) {
        return None;
    }
    Some(Open {
        window: window.0 as isize,
        title,
        exe: program(window, process),
    })
}

/// What Windows says of `window`.
fn seen(window: HWND, titled: bool, ours: bool) -> Seen {
    // SAFETY: plain calls on a handle EnumWindows gave; a window gone
    // meanwhile reads as hidden, unowned, with no styles.
    unsafe {
        let styles = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
        Seen {
            visible: IsWindowVisible(window).as_bool(),
            cloaked: cloaked(window),
            tool: styles & WS_EX_TOOLWINDOW.0 != 0,
            app: styles & WS_EX_APPWINDOW.0 != 0,
            owned: GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()),
            titled,
            ours,
        }
    }
}

/// Whether DWM hides `window`: on another virtual desktop, or suspended.
fn cloaked(window: HWND) -> bool {
    let mut cloaked = 0_u32;
    // SAFETY: `cloaked` is the u32 DWMWA_CLOAKED writes.
    let read = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    read.is_ok() && cloaked != 0
}

/// `window`'s title as Windows keeps it, without asking the window.
fn title(window: HWND) -> String {
    let mut text = [0_u16; 512];
    // SAFETY: the buffer is ours; the call writes at most its length.
    let length = unsafe { InternalGetWindowText(window, &mut text) };
    String::from_utf16_lossy(&text[..length.max(0) as usize])
}

fn process_of(window: HWND) -> u32 {
    let mut process = 0_u32;
    // SAFETY: `process` is the u32 the call writes.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
    process
}

/// The program `window` of `process` belongs to. A Store app's window is
/// a frame of ApplicationFrameHost round the app's own window, a child of
/// another process: that one is the app.
fn program(window: HWND, process: u32) -> Option<String> {
    let exe = image(process)?;
    Some(belongs_to(exe, || {
        child_of_another(window, process).and_then(image)
    }))
}

/// The program a window of `exe` belongs to: `exe`, unless that hosts
/// Store apps' frames; then the app `inside` finds in the frame, while
/// it is there.
fn belongs_to(exe: String, inside: impl FnOnce() -> Option<String>) -> String {
    match frame_host(&exe) {
        true => inside().unwrap_or(exe),
        false => exe,
    }
}

/// Whether `exe` is the host of Store apps' frames.
fn frame_host(exe: &str) -> bool {
    let name = Path::new(exe).file_name().and_then(|name| name.to_str());
    name.is_some_and(|name| name.eq_ignore_ascii_case("ApplicationFrameHost.exe"))
}

/// The process of the first child of `window` that another process than
/// `process` owns.
fn child_of_another(window: HWND, process: u32) -> Option<u32> {
    let mut found = (process, None::<u32>);
    // SAFETY: the callback only writes to `found`, alive for the call.
    unsafe {
        let _ = EnumChildWindows(
            Some(window),
            Some(child),
            LPARAM(&mut found as *mut (u32, Option<u32>) as isize),
        );
    }
    found.1
}

extern "system" fn child(window: HWND, found: LPARAM) -> BOOL {
    // SAFETY: `found` is the pair `child_of_another` passed.
    let found = unsafe { &mut *(found.0 as *mut (u32, Option<u32>)) };
    let process = process_of(window);
    if process != found.0 {
        found.1 = Some(process);
    }
    BOOL::from(found.1.is_none())
}

/// The full path of the program process `process` runs.
fn image(process: u32) -> Option<String> {
    // SAFETY: the handle is ours, closed once read.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process).ok()?;
        let mut path = [0_u16; 1024];
        let mut length = path.len() as u32;
        let read = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(handle);
        read.ok()?;
        Some(String::from_utf16_lossy(&path[..length as usize]))
    }
}

/// What the switcher calls the program at `exe`: the description its
/// file gives (`Visual Studio Code`), or else its name without `.exe`.
pub fn app_name(exe: &str) -> String {
    named(description(exe), exe)
}

/// `description`, when there is one, or `exe`'s name without its
/// extension.
fn named(description: Option<String>, exe: &str) -> String {
    let described = description.filter(|text| !text.trim().is_empty());
    let stem = || {
        let stem = Path::new(exe).file_stem().and_then(|stem| stem.to_str());
        stem.unwrap_or(exe).to_string()
    };
    described.map_or_else(stem, |text| text.trim().to_string())
}

/// The `FileDescription` in `exe`'s version information, in its first
/// language.
fn description(exe: &str) -> Option<String> {
    let path = HSTRING::from(exe);
    // SAFETY: the buffer is as long as the call said; the values read
    // from it point into it and are read while it lives.
    unsafe {
        // A file with no version information measures 0.
        let size = NonZeroU32::new(GetFileVersionInfoSizeW(&path, None))?.get();
        let mut block = vec![0_u8; size as usize];
        GetFileVersionInfoW(&path, None, size, block.as_mut_ptr().cast()).ok()?;
        let (pointer, length) = value(&block, "\\VarFileInfo\\Translation")?;
        if length < 4 {
            return None;
        }
        let words = pointer as *const u16;
        let (language, page) = (*words, *words.add(1));
        let name = format!("\\StringFileInfo\\{language:04x}{page:04x}\\FileDescription");
        let (pointer, length) = value(&block, &name)?;
        let text = std::slice::from_raw_parts(pointer as *const u16, length as usize);
        let text = text.split(|&unit| unit == 0).next().unwrap_or_default();
        Some(String::from_utf16_lossy(text))
    }
}

/// Where the value `name` lies in the version information `block`, and
/// its length.
///
/// # Safety
/// `block` holds what `GetFileVersionInfoW` wrote.
unsafe fn value(block: &[u8], name: &str) -> Option<(*const core::ffi::c_void, u32)> {
    let mut pointer = std::ptr::null_mut();
    let mut length = 0_u32;
    let name = HSTRING::from(name);
    // SAFETY: the caller's; the call writes a pointer into `block`.
    let found = unsafe { VerQueryValueW(block.as_ptr().cast(), &name, &mut pointer, &mut length) };
    found.as_bool().then_some((pointer.cast_const(), length))
}

/// Brings `window` to the front, restoring it if it is minimized. Call it
/// while hyalo is in front: Windows lets the foreground process hand the
/// foreground on. When Windows refuses even so, the window's taskbar
/// button flashes until the user goes to it, and the refusal comes back.
pub fn switch_to(window: isize) -> Result<()> {
    let window = HWND(window as *mut _);
    // SAFETY: plain calls; Windows checks the handle.
    unsafe {
        if !IsWindow(Some(window)).as_bool() {
            return Err(ERROR_INVALID_WINDOW_HANDLE.to_hresult().into());
        }
        if IsIconic(window).as_bool() {
            // Async: a window that hangs must not hang the launcher.
            let _ = ShowWindowAsync(window, SW_RESTORE);
        }
        if SetForegroundWindow(window).as_bool() {
            return Ok(());
        }
        let flash = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: window,
            // Until the window comes forward, at the cursor's blink rate.
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            ..Default::default()
        };
        let _ = FlashWindowEx(&flash);
    }
    Err(E_ACCESSDENIED.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::tests::shell;
    use crate::test_desktop::on_hidden_desktop;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, HWND_TOP, MSG,
        PM_REMOVE, PeekMessageW, SW_MINIMIZE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
        ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
        WS_VISIBLE,
    };
    use windows::core::w;

    /// A top-level window this test made, destroyed when dropped.
    struct Made(HWND);

    impl Made {
        /// A window titled `title`, with `extra` styles, shown when
        /// `shown`.
        fn new(title: &str, extra: WINDOW_EX_STYLE, shown: bool) -> Made {
            let visible = if shown {
                WS_VISIBLE
            } else {
                Default::default()
            };
            Made::under(title, extra, WS_OVERLAPPEDWINDOW | visible, None)
        }

        fn shown(title: &str) -> Made {
            Made::new(title, WINDOW_EX_STYLE::default(), true)
        }

        /// A shown window owned by `owner`, with `extra` styles.
        fn owned(title: &str, owner: &Made, extra: WINDOW_EX_STYLE) -> Made {
            let style = WS_OVERLAPPEDWINDOW | WS_VISIBLE;
            Made::under(title, extra, style, Some(owner.0))
        }

        /// A child window of `parent`.
        fn child_of(parent: &Made) -> Made {
            Made::under("", WINDOW_EX_STYLE::default(), WS_CHILD, Some(parent.0))
        }

        /// A window with these styles: owned by `above`, or its child for
        /// `WS_CHILD`.
        fn under(
            title: &str,
            extra: WINDOW_EX_STYLE,
            style: WINDOW_STYLE,
            above: Option<HWND>,
        ) -> Made {
            let window = unsafe {
                CreateWindowExW(
                    extra,
                    w!("STATIC"),
                    &HSTRING::from(title),
                    style,
                    0,
                    0,
                    200,
                    100,
                    above,
                    None,
                    None,
                    None,
                )
            };
            Made(window.expect("a window"))
        }

        fn id(&self) -> isize {
            self.0.0 as isize
        }
    }

    impl Drop for Made {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    /// The titles of `made`'s windows the switcher lists, in its order.
    fn listed(made: &[&Made], ours: u32) -> Vec<String> {
        let ids: Vec<isize> = made.iter().map(|made| made.id()).collect();
        let open = windows(ours).into_iter();
        let mine = open.filter(|open| ids.contains(&open.window));
        mine.map(|open| open.title).collect()
    }

    /// Handles what this thread was posted, as a message loop would.
    fn pump() {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe { DispatchMessageW(&message) };
        }
    }

    #[test]
    fn the_desktops_windows_are_listed_front_first_and_only_the_real_ones() {
        on_hidden_desktop(|| {
            let back = Made::shown("hyalo switcher back");
            let front = Made::shown("hyalo switcher front");
            let tool = Made::new("hyalo switcher tool", WS_EX_TOOLWINDOW, true);
            let hidden = Made::new("hyalo switcher hidden", WINDOW_EX_STYLE::default(), false);
            let untitled = Made::shown("");
            let all = [&back, &front, &tool, &hidden, &untitled];
            assert_eq!(
                listed(&all, 0),
                ["hyalo switcher front", "hyalo switcher back"]
            );
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            unsafe { SetWindowPos(back.0, Some(HWND_TOP), 0, 0, 0, 0, flags) }.expect("raise");
            assert_eq!(
                listed(&all, 0),
                ["hyalo switcher back", "hyalo switcher front"]
            );
            let ours = unsafe { GetCurrentProcessId() };
            assert!(listed(&all, ours).is_empty(), "hyalo's own windows");
            let open = windows(0);
            let mine = open.iter().find(|open| open.window == front.id());
            let exe = std::env::current_exe().expect("this test's program");
            let exe = exe.to_string_lossy().to_string();
            assert_eq!(mine.and_then(|open| open.exe.clone()), Some(exe));
        });
    }

    #[test]
    fn an_owned_window_is_listed_only_when_it_asks_to_be_an_app_window() {
        on_hidden_desktop(|| {
            let owner = Made::shown("hyalo switcher owner");
            let dialog = Made::owned("hyalo switcher dialog", &owner, WINDOW_EX_STYLE::default());
            let app = Made::owned("hyalo switcher app window", &owner, WS_EX_APPWINDOW);
            let listed = listed(&[&owner, &dialog, &app], 0);
            assert_eq!(listed.len(), 2, "{listed:?}");
            assert!(listed.contains(&"hyalo switcher owner".to_string()));
            assert!(listed.contains(&"hyalo switcher app window".to_string()));
        });
    }

    #[test]
    fn a_child_counts_when_another_process_than_the_one_named_owns_it() {
        on_hidden_desktop(|| {
            let ours = unsafe { GetCurrentProcessId() };
            let lone = Made::shown("hyalo switcher lone");
            assert_eq!(child_of_another(lone.0, 0), None, "no child at all");
            let parent = Made::shown("hyalo switcher parent");
            let _child = Made::child_of(&parent);
            assert_eq!(child_of_another(parent.0, ours), None, "our own child");
            assert_eq!(child_of_another(parent.0, 0), Some(ours));
        });
    }

    #[test]
    fn a_store_apps_frame_is_named_by_the_app_inside_and_any_other_by_itself() {
        let host = r"C:\Windows\System32\ApplicationFrameHost.exe";
        let calculator = r"C:\Program Files\WindowsApps\Calculator\CalculatorApp.exe";
        let explorer = r"C:\Windows\explorer.exe";
        let inside = |exe: &str| {
            let exe = exe.to_string();
            move || Some(exe)
        };
        assert_eq!(belongs_to(host.to_string(), inside(calculator)), calculator);
        assert_eq!(belongs_to(host.to_string(), || None), host, "app gone");
        assert_eq!(
            belongs_to(explorer.to_string(), inside(calculator)),
            explorer
        );
    }

    #[test]
    fn the_provider_reports_the_windows_then_each_programs_icon_once() {
        let windows = std::env::var("SystemRoot").expect("SystemRoot is set");
        let explorer = format!(r"{windows}\explorer.exe");
        let open = |window: isize, exe: &Option<String>| Open {
            window,
            title: format!("window {window}"),
            exe: exe.clone(),
        };
        let exe = Some(explorer.clone());
        let opened = vec![open(1, &exe), open(2, &None), open(3, &exe)];
        let found = shell(move || {
            let request = Request {
                icon_size: 32,
                known: HashSet::from([switcher::item(3, "", "").id]),
            };
            let found = RefCell::new(Vec::new());
            report_windows(&opened, &request, &|piece| found.borrow_mut().push(piece));
            found.into_inner()
        });
        let [Found::Windows(items), Found::Icon(id, icon)] = &found[..] else {
            panic!("the windows, then the one icon not known");
        };
        let titles: Vec<&str> = items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["window 1", "window 2", "window 3"]);
        assert_eq!(items[0].subtitle, app_name(&explorer));
        assert_eq!(items[1].subtitle, "", "no program to name");
        assert_eq!(items[0].action, hyalo_core::item::Action::Switch(1));
        assert_eq!(*id, items[0].id);
        assert_eq!((icon.width, icon.height), (32, 32));
    }

    #[test]
    fn windows_whose_icons_are_known_are_reported_without_them() {
        on_hidden_desktop(|| {
            let made = Made::shown("hyalo switcher known");
            let id = switcher::item(made.id(), "", "").id;
            let found = RefCell::new(Vec::new());
            let request = Request {
                icon_size: 32,
                known: windows(0)
                    .iter()
                    .map(|open| switcher::item(open.window, "", "").id)
                    .collect(),
            };
            find(0, &request, &|piece| found.borrow_mut().push(piece));
            let found = found.into_inner();
            let Some(Found::Windows(items)) = found.first() else {
                panic!("the windows first");
            };
            assert!(items.iter().any(|item| item.id == id));
            // Other tests' windows may have opened meanwhile, with icons.
            let icon = |piece: &Found| matches!(piece, Found::Icon(of, _) if *of == id);
            assert!(!found.iter().any(icon), "its icon is known");
        });
    }

    /// A test process is never the foreground one (its console is, or
    /// the lock screen), so Windows refuses it the foreground on the
    /// hidden desktop; where it may, the window must be in front.
    #[test]
    fn switching_restores_a_minimized_window_and_brings_it_to_the_front_if_windows_lets_it() {
        on_hidden_desktop(|| {
            let back = Made::shown("hyalo switcher minimized");
            let _front = Made::shown("hyalo switcher in front");
            let _ = unsafe { ShowWindow(back.0, SW_MINIMIZE) };
            assert!(unsafe { IsIconic(back.0) }.as_bool());
            match switch_to(back.id()) {
                Ok(()) => assert_eq!(unsafe { GetForegroundWindow() }, back.0),
                Err(error) => assert_eq!(error.code(), E_ACCESSDENIED),
            }
            pump();
            assert!(!unsafe { IsIconic(back.0) }.as_bool(), "restored");
        });
    }

    #[test]
    fn a_window_that_is_gone_is_not_switched_to() {
        on_hidden_desktop(|| {
            let gone = Made::shown("hyalo switcher gone").id();
            let error = switch_to(gone).expect_err("no such window");
            assert_eq!(error.code(), ERROR_INVALID_WINDOW_HANDLE.to_hresult());
        });
    }

    #[test]
    fn an_app_is_called_by_its_files_description_or_else_its_name() {
        let exe = r"C:\Program Files\Code\Code.exe";
        let described = Some("Visual Studio Code".to_string());
        assert_eq!(named(described, exe), "Visual Studio Code");
        assert_eq!(named(Some("  Notepad ".to_string()), exe), "Notepad");
        assert_eq!(named(None, exe), "Code");
        assert_eq!(named(Some("   ".to_string()), exe), "Code");
        assert_eq!(named(None, "tool"), "tool");
    }

    #[test]
    fn explorer_describes_itself() {
        let windows = std::env::var("SystemRoot").expect("SystemRoot is set");
        let explorer = format!(r"{windows}\explorer.exe");
        let name = app_name(&explorer);
        assert!(
            ["Windows Explorer", "Проводник"].contains(&name.as_str()),
            "{name}: its description in the language Windows speaks"
        );
        let missing = format!(r"{windows}\no such program.exe");
        assert_eq!(app_name(&missing), "no such program");
    }

    #[test]
    fn store_apps_are_framed_by_their_host() {
        let host = r"C:\WINDOWS\system32\ApplicationFrameHost.exe";
        assert!(frame_host(host));
        assert!(frame_host(&host.to_lowercase()));
        assert!(!frame_host(r"C:\Windows\explorer.exe"));
        assert!(!frame_host(""));
    }
}
