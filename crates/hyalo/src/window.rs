//! The launcher window: a borderless popup over a DWM Acrylic backdrop.
//! A global hotkey toggles it; Esc, Enter or losing focus hides it.
//!
//! The `windows` crate marks Win32 calls `unsafe`. The invariant they rely
//! on: `hwnd` is the live launcher window, owned by the UI thread.

use std::cell::RefCell;

use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_NOREPEAT, RegisterHotKey, VIRTUAL_KEY, VK_ESCAPE, VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, IsWindowVisible, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW,
    SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SetForegroundWindow, SetWindowPos, ShowWindow,
    TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN,
    WM_NCCALCSIZE, WM_NCHITTEST, WNDCLASSEXW, WS_CAPTION, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};
use windows::core::{BOOL, Error, PCWSTR, Result, w};

use crate::layout::{self, Rect};
use crate::query::Query;
use crate::render::{Renderer, Scene};

const HOTKEY_TOGGLE: i32 = 1;

struct App {
    hwnd: HWND,
    query: Query,
    renderer: Option<Renderer>,
}

impl App {
    fn present(&mut self, rect: Rect, dpi: u32) -> Result<()> {
        let (width, height) = (rect.width as u32, rect.height as u32);
        match &mut self.renderer {
            Some(renderer) => renderer.resize(width, height, dpi)?,
            None => self.renderer = Some(Renderer::new(self.hwnd, width, height, dpi)?),
        }
        self.redraw()
    }

    fn redraw(&self) -> Result<()> {
        let scene = Scene {
            query: self.query.text(),
        };
        self.renderer
            .as_ref()
            .map_or(Ok(()), |renderer| renderer.draw(&scene))
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Opens the launcher, binds the hotkey and runs until the window closes.
pub fn run() -> Result<()> {
    let hwnd = open()?;
    register_hotkey(hwnd);
    message_loop();
    Ok(())
}

/// Creates the launcher window on this thread and shows it.
fn open() -> Result<HWND> {
    let hwnd = create_window()?;
    APP.with_borrow_mut(|app| {
        *app = Some(App {
            hwnd,
            query: Query::default(),
            renderer: None,
        })
    });
    style_window(hwnd)?;
    show(hwnd)?;
    Ok(hwnd)
}

const WINDOW_CLASS: PCWSTR = w!("hyalo.launcher");

/// Registers the window class. Registering it again is a no-op, so every
/// thread that opens a launcher can call this.
fn register_class(instance: HINSTANCE) -> Result<()> {
    let window_class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };
    match unsafe { RegisterClassExW(&window_class) } {
        0 => already_registered_is_fine(Error::from_thread()),
        _ => Ok(()),
    }
}

fn already_registered_is_fine(error: Error) -> Result<()> {
    match error.code() == ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
        true => Ok(()),
        false => Err(error),
    }
}

fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        register_class(instance)?;
        // WS_THICKFRAME | WS_CAPTION make DWM treat the popup as a framed
        // window, which is what gets it a shadow and the system backdrop.
        // WM_NCCALCSIZE then removes the frame itself.
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            WINDOW_CLASS,
            w!("hyalo"),
            WS_POPUP | WS_THICKFRAME | WS_CAPTION,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

fn style_window(hwnd: HWND) -> Result<()> {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(true))?;
    set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND)?;
    set_attribute(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE)?;
    set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)?;
    let whole_window = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &whole_window) }
}

fn set_attribute<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) -> Result<()> {
    // SAFETY: `value` points to a live `T`, and the size passed is its size.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (value as *const T).cast(),
            size_of::<T>() as u32,
        )
    }
}

fn register_hotkey(hwnd: HWND) {
    let modifiers = MOD_ALT | MOD_NOREPEAT;
    let registered =
        unsafe { RegisterHotKey(Some(hwnd), HOTKEY_TOGGLE, modifiers, u32::from(VK_SPACE.0)) };
    if let Err(error) = registered {
        eprintln!("hyalo: Alt+Space is taken by another app: {error}");
    }
}

fn message_loop() {
    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        // The client area covers the whole window: no visible frame.
        WM_NCCALCSIZE if wparam.0 != 0 => LRESULT(0),
        // No resize edges.
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_HOTKEY => handled(toggle(hwnd)),
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            hide(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            on_key(hwnd, VIRTUAL_KEY(wparam.0 as u16));
            LRESULT(0)
        }
        WM_CHAR => handled(on_char(wparam.0 as u16)),
        // No WM_PAINT: the picture lives in the DirectComposition swap chain
        // and survives without one; DefWindowProc validates the window.
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The message was handled; log a failure instead of unwinding into Win32.
fn handled(result: Result<()>) -> LRESULT {
    if let Err(error) = result {
        eprintln!("hyalo: {error}");
    }
    LRESULT(0)
}

fn toggle(hwnd: HWND) -> Result<()> {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        hide(hwnd);
        return Ok(());
    }
    show(hwnd)
}

fn show(hwnd: HWND) -> Result<()> {
    let (rect, dpi) = placement_under_cursor()?;
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE,
        )?;
    }
    with_app(|app| app.present(rect, dpi)).unwrap_or(Ok(()))?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    with_app(|app| app.query.clear());
}

fn on_key(hwnd: HWND, key: VIRTUAL_KEY) {
    if matches!(key, VK_ESCAPE | VK_RETURN) {
        hide(hwnd);
    }
}

fn on_char(unit: u16) -> Result<()> {
    with_app(|app| match app.query.type_unit(unit) {
        true => app.redraw(),
        false => Ok(()),
    })
    .unwrap_or(Ok(()))
}

/// Where the launcher goes on the monitor under the mouse cursor.
///
/// Only a thread on the desktop that receives input may read the cursor
/// (not, say, while the UAC secure desktop is up). Without it, use the
/// primary monitor: it always contains (0, 0).
fn placement_under_cursor() -> Result<(Rect, u32)> {
    let cursor = cursor_position().unwrap_or_default();
    placement_on(unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) })
}

fn cursor_position() -> Result<POINT> {
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor)? };
    Ok(cursor)
}

/// The launcher's rectangle on `monitor`, and that monitor's DPI.
fn placement_on(monitor: HMONITOR) -> Result<(Rect, u32)> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut dpi_y) = (96, 96);
    unsafe {
        GetMonitorInfoW(monitor, &mut info).ok()?;
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y)?;
    }
    let scale = dpi as f32 / 96.0;
    Ok((layout::launcher_rect(to_rect(info.rcWork), scale), dpi))
}

fn to_rect(rect: RECT) -> Rect {
    Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    }
}

/// Runs `f` on the app state, unless the state is already borrowed further
/// up the stack (Win32 re-enters the window procedure from inside some
/// calls) or already gone (the thread is exiting).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.try_with(|cell| {
        let mut app = cell.try_borrow_mut().ok()?;
        app.as_mut().map(f)
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::MonitorFromWindow;
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_A;
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyWindow, GetClientRect, GetWindowRect, PM_REMOVE, PeekMessageW, PostMessageW,
        SendMessageW, WA_ACTIVE, WM_QUIT,
    };

    use super::*;
    use crate::render::Frame;
    use crate::test_desktop::on_hidden_desktop;

    /// The launcher as a test drives it: through the messages Windows
    /// would send. Destroyed while the thread's app state is still alive.
    struct Launcher(HWND);

    impl Launcher {
        fn open() -> Self {
            Self(open().expect("open the launcher"))
        }

        fn send(&self, message: u32, wparam: usize) -> LRESULT {
            unsafe { SendMessageW(self.0, message, Some(WPARAM(wparam)), Some(LPARAM(0))) }
        }

        fn post(&self, message: u32, wparam: usize) {
            unsafe { PostMessageW(Some(self.0), message, WPARAM(wparam), LPARAM(0)) }
                .expect("post a message");
        }

        fn type_text(&self, text: &str) {
            for unit in text.encode_utf16() {
                self.send(WM_CHAR, usize::from(unit));
            }
        }

        fn press(&self, key: VIRTUAL_KEY) {
            self.send(WM_KEYDOWN, usize::from(key.0));
        }

        fn hotkey(&self) {
            self.send(WM_HOTKEY, HOTKEY_TOGGLE as usize);
        }

        fn visible(&self) -> bool {
            unsafe { IsWindowVisible(self.0) }.as_bool()
        }

        fn rect(&self) -> Rect {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(self.0, &mut rect) }.expect("window rect");
            to_rect(rect)
        }

        fn client_size(&self) -> (i32, i32) {
            let mut rect = RECT::default();
            unsafe { GetClientRect(self.0, &mut rect) }.expect("client rect");
            (rect.right - rect.left, rect.bottom - rect.top)
        }

        /// What the launcher shows now, read back from its swap chain.
        /// No repaint first: the test must see what the code put on screen.
        fn frame(&self) -> Frame {
            with_app(|app| app.renderer.as_ref().expect("a renderer").read_frame())
                .expect("app state")
                .expect("read the frame back")
        }

        fn caret_x(&self) -> usize {
            caret_x(&self.frame()).expect("a caret on screen")
        }
    }

    impl Drop for Launcher {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    /// The first column of the blue caret on the middle row.
    fn caret_x(frame: &Frame) -> Option<usize> {
        let y = frame.height / 2;
        (0..frame.width).find(|&x| {
            let [blue, _, red, alpha] = frame.pixel(x, y);
            blue > 220 && red < 160 && alpha > 220
        })
    }

    /// The brightest red value in columns well inside the placeholder text
    /// but past where a one-letter query ends.
    fn placeholder_glare(frame: &Frame) -> u8 {
        let columns = frame.width * 30 / 100..frame.width * 45 / 100;
        let rows = frame.height * 35 / 100..frame.height * 65 / 100;
        rows.flat_map(|y| columns.clone().map(move |x| (x, y)))
            .map(|(x, y)| frame.pixel(x, y)[2])
            .max()
            .expect("a non-empty region")
    }

    #[test]
    fn opens_visible_where_the_layout_puts_it_on_its_monitor() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            assert!(launcher.visible());
            let monitor = unsafe { MonitorFromWindow(launcher.0, MONITOR_DEFAULTTONEAREST) };
            let (expected, _) = placement_on(monitor).expect("placement");
            assert_eq!(launcher.rect(), expected);
        });
    }

    #[test]
    fn has_no_visible_frame_and_no_resize_edges() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let rect = launcher.rect();
            assert_eq!(launcher.client_size(), (rect.width, rect.height));
            let hit = launcher.send(WM_NCHITTEST, 0);
            assert_eq!(hit, LRESULT(HTCLIENT as isize));
        });
    }

    #[test]
    fn the_hotkey_hides_it_and_brings_it_back() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.hotkey();
            assert!(!launcher.visible());
            launcher.hotkey();
            assert!(launcher.visible());
        });
    }

    #[test]
    fn escape_enter_and_losing_focus_each_hide_it() {
        let hiders = [
            (WM_KEYDOWN, usize::from(VK_ESCAPE.0)),
            (WM_KEYDOWN, usize::from(VK_RETURN.0)),
            (WM_ACTIVATE, WA_INACTIVE as usize),
        ];
        for (message, wparam) in hiders {
            on_hidden_desktop(move || {
                let launcher = Launcher::open();
                launcher.send(message, wparam);
                assert!(!launcher.visible(), "message {message:#x} {wparam:#x}");
            });
        }
    }

    #[test]
    fn other_keys_and_gaining_focus_keep_it_open() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.press(VK_A);
            launcher.send(WM_ACTIVATE, WA_ACTIVE as usize);
            assert!(launcher.visible());
        });
    }

    #[test]
    fn shows_the_placeholder_until_something_is_typed() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let empty = placeholder_glare(&launcher.frame());
            launcher.type_text("a");
            let typed = placeholder_glare(&launcher.frame());
            assert!(
                empty > typed.saturating_add(40),
                "placeholder {empty}, typed {typed}"
            );
        });
    }

    #[test]
    fn typing_moves_the_caret_right() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let start = launcher.caret_x();
            launcher.type_text("notepad");
            assert!(launcher.caret_x() > start + 40);
        });
    }

    #[test]
    fn reopening_starts_with_an_empty_query() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let start = launcher.caret_x();
            launcher.type_text("notepad");
            launcher.press(VK_ESCAPE);
            launcher.hotkey();
            assert_eq!(launcher.caret_x(), start);
        });
    }

    #[test]
    fn the_message_loop_delivers_typing_and_stops_on_quit() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let start = launcher.caret_x();
            for unit in "abc".encode_utf16() {
                launcher.post(WM_CHAR, usize::from(unit));
            }
            unsafe { PostQuitMessage(0) };
            message_loop();
            assert!(launcher.caret_x() > start);
        });
    }

    #[test]
    fn destroying_the_window_quits_the_app() {
        on_hidden_desktop(|| {
            let hwnd = open().expect("open the launcher");
            unsafe { DestroyWindow(hwnd) }.expect("destroy the window");
            let mut message = MSG::default();
            let quit = unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) };
            assert!(quit.as_bool());
        });
    }
}
