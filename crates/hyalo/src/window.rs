//! The launcher window: a borderless popup over a DWM Acrylic backdrop.
//! A global hotkey toggles it; Esc, Enter or losing focus hides it.
//!
//! The `windows` crate marks Win32 calls `unsafe`. The invariant they rely
//! on: `hwnd` is the live launcher window, owned by the UI thread.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_NOREPEAT, RegisterHotKey, VIRTUAL_KEY, VK_ESCAPE, VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, IsWindowVisible, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW,
    SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SetForegroundWindow, SetWindowPos, ShowWindow,
    TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN,
    WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_CAPTION, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};
use windows::core::{BOOL, Error, Result, w};

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

pub fn run() -> Result<()> {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    let hwnd = create_window()?;
    APP.with_borrow_mut(|app| {
        *app = Some(App {
            hwnd,
            query: Query::default(),
            renderer: None,
        })
    });
    style_window(hwnd)?;
    register_hotkey(hwnd);
    show(hwnd)?;
    message_loop();
    Ok(())
}

fn create_window() -> Result<HWND> {
    let class = w!("hyalo.launcher");
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let window_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassExW(&window_class) == 0 {
            return Err(Error::from_thread());
        }
        // WS_THICKFRAME | WS_CAPTION make DWM treat the popup as a framed
        // window, which is what gets it a shadow and the system backdrop.
        // WM_NCCALCSIZE then removes the frame itself.
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
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
        WM_PAINT => handled(paint(hwnd)),
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

fn paint(hwnd: HWND) -> Result<()> {
    unsafe {
        let _ = ValidateRect(Some(hwnd), None);
    }
    with_app(|app| app.redraw()).unwrap_or(Ok(()))
}

/// The work area and DPI of the monitor under the mouse cursor.
fn placement_under_cursor() -> Result<(Rect, u32)> {
    let mut cursor = POINT::default();
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut dpi_y) = (96, 96);
    unsafe {
        GetCursorPos(&mut cursor)?;
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(monitor, &mut info).ok()?;
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y)?;
    }
    let work = info.rcWork;
    let work_area = Rect {
        x: work.left,
        y: work.top,
        width: work.right - work.left,
        height: work.bottom - work.top,
    };
    Ok((layout::launcher_rect(work_area, dpi as f32 / 96.0), dpi))
}

/// Runs `f` on the app state, unless the state is already borrowed further
/// up the stack: Win32 re-enters the window procedure from inside some calls.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut app = cell.try_borrow_mut().ok()?;
        app.as_mut().map(f)
    })
}
