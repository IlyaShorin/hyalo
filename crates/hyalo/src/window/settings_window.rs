//! The settings window: a window of its own with a title bar and a
//! taskbar button, over Mica (the backdrop Windows gives windows that
//! stay open; the launcher's Acrylic is for windows that come and go).
//! It shows the page the panel builds and carries out what the panel
//! says with the launcher's services: the config file, the hotkeys,
//! starting with Windows, the folder picker. One at a time; Esc or its
//! close button closes it.
//!
//! Its state lives in the launcher's `App`, beside those services. The
//! invariant the `unsafe` calls rely on: `hwnd` is the live settings
//! window, owned by the UI thread.

use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use hyalo_core::config::{self, Change, List, Problem, Refusal, Why};
use hyalo_core::item::Action;
use hyalo_core::text::{Language, Text};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmExtendFrameIntoClientArea,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_LWIN,
    VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, IDC_ARROW, IsIconic,
    LoadCursorW, MINMAXINFO, RegisterClassExW, SM_CXICON, SM_CXSMICON, SW_RESTORE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow,
    WA_INACTIVE, WM_ACTIVATE, WM_CHAR, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SIZE,
    WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, Result, w};

use super::{
    App, MOUSE_SHIFT, Placement, client_point, double_click_room, double_click_time, down,
    placement_under_cursor, set_attribute, with_app,
};
use crate::field;
use crate::layout;
use crate::page::{self, Laid, Target, What};
use crate::paint::Painter;
use crate::panel::{Effect, Panel, Versions};
use crate::recorder::Mods;
use crate::{clipboard, settings, tray};

const CLASS: PCWSTR = w!("hyalo.settings");

/// The settings window as the launcher keeps it.
pub(super) struct SettingsWindow {
    pub(super) hwnd: HWND,
    pub(super) panel: Panel,
    painter: Painter,
    dpi: u32,
    /// The page as last drawn, for what is under the pointer and where
    /// Tab goes.
    pub(super) laid: Laid,
    /// Whether Windows tells the window when the pointer leaves it.
    tracking: bool,
    /// Whether the recorder took the last key pressed with Alt: the
    /// character it makes is the recorder's too.
    recorded: bool,
    /// When and where the last double-click in a field was, until a
    /// third click uses it.
    double: Option<(Instant, (i32, i32))>,
    /// The title the window has, in the page's language.
    title: &'static str,
}

/// Opens the settings window, or brings the one open forward. Called
/// with no app state borrowed: creating and showing a window sends it
/// messages, which must reach it.
pub(super) fn open_settings() -> Result<()> {
    let open = with_app(|app| app.settings.as_ref().map(|window| window.hwnd)).flatten();
    if let Some(hwnd) = open {
        bring_forward(hwnd);
        return Ok(());
    }
    let Some(panel) = with_app(|app| app.fresh_panel()).flatten() else {
        return Ok(());
    };
    let hwnd = create(placement_under_cursor()?)?;
    // A window that cannot be drawn is not left behind, hidden.
    // SAFETY: the window was made above, on this thread.
    let window = SettingsWindow::new(hwnd, panel)
        .inspect_err(|_| unsafe { DestroyWindow(hwnd) }.unwrap_or_default())?;
    with_app(|app| app.settings = Some(window));
    bring_forward(hwnd);
    Ok(())
}

/// Shows `hwnd`, restored if it was minimized, in front.
fn bring_forward(hwnd: HWND) {
    // SAFETY: plain calls on a window of this thread.
    unsafe {
        let show = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, show);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Registers the class once per process; see `super::register_class`
/// for why its result is not checked.
fn register_class(instance: HINSTANCE) -> Result<()> {
    // SAFETY: the icons and the cursor are the system's or this
    // executable's and live as long as the class.
    unsafe {
        let dpi = windows::Win32::UI::HiDpi::GetDpiForSystem();
        let big = tray::icon(GetSystemMetricsForDpi(SM_CXICON, dpi)).unwrap_or_default();
        let small = tray::icon(GetSystemMetricsForDpi(SM_CXSMICON, dpi)).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            // Double-clicks select words in the name fields.
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hIcon: big,
            hIconSm: small,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
    Ok(())
}

/// The window, framed and resizable, in the middle of the monitor the
/// launcher would show on, dark over Mica; not shown yet.
fn create(placement: Placement) -> Result<HWND> {
    let rect = layout::settings_rect(placement.work_area, layout::scale(placement.dpi));
    // SAFETY: plain calls; the class is registered first.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?.into();
        register_class(instance)?;
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            w!("hyalo settings"),
            WS_OVERLAPPEDWINDOW,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            None,
            None,
            Some(instance),
            None,
        )?
    };
    // Where DWM refuses the backdrop (a window station of its own), the
    // window still opens, on a plain background.
    if let Err(error) = style(hwnd) {
        eprintln!("hyalo: the settings window goes without its backdrop: {error}");
    }
    Ok(hwnd)
}

fn style(hwnd: HWND) -> Result<()> {
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(true))?;
    set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_MAINWINDOW)?;
    // The backdrop under the whole client area, which the swap chain
    // leaves clear wherever it draws nothing.
    let sheet = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: a plain call with a live MARGINS.
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &sheet) }
}

/// The size of `hwnd`'s client area, in pixels: a client rectangle
/// starts at 0, 0.
fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT::default();
    // SAFETY: a plain call into a live RECT.
    let _ = unsafe { GetClientRect(hwnd, &mut rect) };
    (rect.right as u32, rect.bottom as u32)
}

/// The modifier keys held, as of the key message being handled.
fn mods() -> Mods {
    Mods {
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

impl SettingsWindow {
    /// The window's state, with its first frame drawn.
    fn new(hwnd: HWND, panel: Panel) -> Result<Self> {
        // SAFETY: a plain call on a live window.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let (width, height) = client_size(hwnd);
        let mut window = Self {
            hwnd,
            panel,
            painter: Painter::new(hwnd, width, height, dpi)?,
            dpi,
            laid: Laid::default(),
            tracking: false,
            recorded: false,
            double: None,
            title: "",
        };
        window.redraw()?;
        Ok(window)
    }

    /// Lays the page out for the window's width, keeps the scroll within
    /// it, and draws it.
    pub(super) fn redraw(&mut self) -> Result<()> {
        let painter = &self.painter;
        let (width, height) = painter.view();
        self.laid = page::lay_out(&self.panel.page(), width, &|text| painter.measure(text));
        self.panel.fit(self.laid.height, height);
        let panel = &self.panel;
        let drawn = painter.draw(&self.laid, panel.scroll(), &|target| panel.look(target));
        self.retitle();
        drawn
    }

    /// Names the window in the page's language, when that changed.
    fn retitle(&mut self) {
        let title = self.panel.title();
        if title != self.title {
            // SAFETY: a plain call on a live window; the WM_SETTEXT it
            // sends finds the app state borrowed and goes to Windows.
            let _ = unsafe { SetWindowTextW(self.hwnd, &HSTRING::from(title)) };
            self.title = title;
        }
    }

    /// Redraws, telling of a failure rather than handing it on: a frame
    /// that failed leaves the last one up, and the next may succeed.
    pub(super) fn redraw_or_say(&mut self) {
        if let Err(error) = self.redraw() {
            eprintln!("hyalo: cannot draw the settings: {error}");
        }
    }

    /// The hotkey was pressed; whether the recorder took it.
    pub(super) fn hotkey_pressed(&mut self) -> bool {
        let taken = self.panel.hotkey_pressed();
        if taken {
            self.redraw_or_say();
        }
        taken
    }

    /// The control under the point a mouse message carries.
    fn hit(&self, lparam: LPARAM) -> Option<Target> {
        let (x, y) = self.on_page(lparam);
        self.laid.hit(x, y)
    }

    /// The point a mouse message carries, in DIPs from the page's top.
    fn on_page(&self, lparam: LPARAM) -> (f32, f32) {
        let point = client_point(lparam);
        let scale = layout::scale(self.dpi);
        let (x, y) = (point.x as f32 / scale, point.y as f32 / scale);
        (x, y + self.panel.scroll())
    }

    /// The button went down at `lparam`, with Shift held when `shift`: the
    /// control there takes the focus; in a name field the caret goes
    /// where it was clicked, or Shift selects up to there, or a click
    /// after a double-click selects everything.
    fn press(&mut self, lparam: LPARAM, shift: bool) {
        let target = self.hit(lparam);
        self.panel.press(target);
        let Some(Target::Name(list)) = target else {
            return;
        };
        let point = client_point(lparam);
        let double = self.double.take();
        let (soon, room) = (double_click_time(), double_click_room());
        if field::third_click(double, Instant::now(), (point.x, point.y), soon, room) {
            self.panel.select_all();
        } else if let Some(at) = self.char_at(list, lparam) {
            self.panel.place(at, shift);
        }
    }

    /// A double-click at `lparam`: in a name field it selects the word
    /// there; elsewhere it is a press like any other.
    fn double_click(&mut self, lparam: LPARAM, shift: bool) {
        let target = self.hit(lparam);
        let Some(Target::Name(list)) = target else {
            return self.press(lparam, shift);
        };
        self.panel.press(target);
        let point = client_point(lparam);
        self.double = Some((Instant::now(), (point.x, point.y)));
        if let Some(at) = self.char_at(list, lparam) {
            self.panel.select_word(at);
        }
    }

    /// With the button down in a name field, the selection follows the
    /// pointer to `lparam`; whether it moved.
    fn drag(&mut self, lparam: LPARAM) -> bool {
        let dragging = self.panel.dragging();
        let at = dragging.and_then(|list| self.char_at(list, lparam));
        at.is_some_and(|at| self.panel.place(at, true))
    }

    /// The character a click at `lparam` puts the caret before in the
    /// name field of `list`, as it was last drawn.
    fn char_at(&self, list: List, lparam: LPARAM) -> Option<usize> {
        let target = Some(Target::Name(list));
        let mark = self
            .laid
            .marks
            .iter()
            .find(|mark| mark.what.target() == target)?;
        let What::Field { text, caret, .. } = &mark.what else {
            return None;
        };
        let (x, _) = self.on_page(lparam);
        let unit = self.painter.field_hit(text, *caret, mark.at, x).ok()?;
        Some(field::char_index(text, unit))
    }

    /// Asks Windows to say when the pointer leaves, once per visit.
    fn track(&mut self) {
        if self.tracking {
            return;
        }
        let mut leave = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            ..Default::default()
        };
        // SAFETY: a plain call with a live TRACKMOUSEEVENT.
        self.tracking = unsafe { TrackMouseEvent(&mut leave) }.is_ok();
    }

    /// What the window does with a message: `None` for those it leaves
    /// to Windows, else what the panel says to do, if anything. Nothing
    /// here sends the window a message that must reach it: the app state
    /// is borrowed.
    fn input(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<Option<Effect>> {
        let outcome = match message {
            WM_SIZE => self.resize(),
            _ => match self.pointer(message, wparam, lparam) {
                Outcome::Unhandled => self.keyboard(message, wparam),
                handled => handled,
            },
        };
        match outcome {
            Outcome::Unhandled => None,
            Outcome::Same => Some(None),
            Outcome::Changed(effect) => {
                self.redraw_or_say();
                Some(effect)
            }
        }
    }

    /// The window took a new size: the picture takes it too.
    fn resize(&mut self) -> Outcome {
        let (width, height) = client_size(self.hwnd);
        if let Err(error) = self.painter.resize(width, height, self.dpi) {
            eprintln!("hyalo: cannot resize the settings: {error}");
        }
        Outcome::Changed(None)
    }

    /// What the mouse does.
    fn pointer(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Outcome {
        match message {
            WM_MOUSEMOVE => {
                self.track();
                let target = self.hit(lparam);
                let hovered = self.panel.hover(target);
                changed(self.drag(lparam) | hovered)
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                changed(self.panel.hover(None))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                // SAFETY: a plain call on a window of this thread.
                unsafe { SetCapture(self.hwnd) };
                let shift = wparam.0 & MOUSE_SHIFT != 0;
                match message {
                    WM_LBUTTONDOWN => self.press(lparam, shift),
                    _ => self.double_click(lparam, shift),
                }
                Outcome::Changed(None)
            }
            WM_LBUTTONUP => {
                // SAFETY: as above.
                let _ = unsafe { ReleaseCapture() };
                Outcome::Changed(self.panel.release(self.hit(lparam)))
            }
            WM_MOUSEWHEEL => {
                let delta = i32::from((wparam.0 >> 16) as u16 as i16);
                let view = self.painter.view().1;
                self.panel.wheel(delta, self.laid.height, view);
                Outcome::Changed(None)
            }
            _ => Outcome::Unhandled,
        }
    }

    /// What the keyboard does. Keys let go only matter to the recorder;
    /// Alt or F10 let go would start the window's menu mode, and there is
    /// no menu bar, so it is kept from Windows.
    fn keyboard(&mut self, message: u32, wparam: WPARAM) -> Outcome {
        let key = wparam.0 as u16;
        match message {
            WM_KEYDOWN => {
                let view = self.painter.view().1;
                Outcome::Changed(self.panel.key(key, mods(), &self.laid, view))
            }
            WM_SYSKEYDOWN => self.system_key(key),
            WM_KEYUP | WM_SYSKEYUP => {
                let listening = self.panel.listening();
                self.panel.key_up(mods());
                changed(listening)
            }
            WM_SYSCHAR => self.system_char(key),
            WM_CHAR => changed(self.panel.char(key)),
            _ => Outcome::Unhandled,
        }
    }

    /// A key pressed with Alt, or F10: the recorder's while it listens, a
    /// key like any other; else Windows' (Alt+F4 closes the window).
    fn system_key(&mut self, key: u16) -> Outcome {
        self.recorded = self.panel.listening();
        match self.recorded {
            true => Outcome::Changed(self.panel.record(key, mods())),
            false => Outcome::Unhandled,
        }
    }

    /// The character a key pressed with Alt makes. Windows would look for
    /// it in a menu bar there is none of, and beep: it is kept from
    /// Windows, all but Alt+Space, which opens the window menu, unless the
    /// recorder took the key that made it.
    fn system_char(&mut self, unit: u16) -> Outcome {
        let recorded = std::mem::take(&mut self.recorded);
        match recorded || unit != u16::from(b' ') {
            true => Outcome::Same,
            false => Outcome::Unhandled,
        }
    }
}

/// What the window made of a message.
enum Outcome {
    /// Not the window's: Windows handles it.
    Unhandled,
    /// Handled, and nothing on the page changed.
    Same,
    /// Handled, the page changed, and this is what the panel says to do,
    /// if anything.
    Changed(Option<Effect>),
}

fn changed(changed: bool) -> Outcome {
    match changed {
        true => Outcome::Changed(None),
        false => Outcome::Same,
    }
}

impl App {
    /// A panel for the config as it is now, if there is a config to
    /// change.
    fn fresh_panel(&self) -> Option<Panel> {
        let path = self.services.config.as_deref()?;
        let language = self.services.windows_language;
        let file = settings::contents(path, language).map_or_else(
            || {
                Err(Problem {
                    line: 1,
                    why: Why::Unreadable,
                })
            },
            settings::written,
        );
        let mut panel = Panel::new(file, self.starts(), self.services.windows_language);
        panel.set_updates(self.versions());
        Some(panel)
    }

    /// The versions the settings window shows, where hyalo updates
    /// itself.
    fn versions(&self) -> Option<Versions> {
        let updates = self.services.updates.as_ref()?;
        Some(Versions {
            current: updates.current,
            update: self.offered().cloned(),
        })
    }

    /// Shows the versions as they are now in the settings window, if it
    /// is open.
    pub(super) fn show_versions(&mut self) {
        let versions = self.versions();
        if let Some(window) = &mut self.settings {
            window.panel.set_updates(versions);
            window.redraw_or_say();
        }
    }

    /// Whether hyalo starts with Windows; `None` where it may not.
    pub(super) fn starts(&self) -> Option<bool> {
        self.services
            .autostart
            .as_ref()
            .map(|autostart| autostart.starts())
    }

    /// Shows whether hyalo starts with Windows as it does now: Windows'
    /// own switch may have changed it.
    fn refresh_starts(&mut self) {
        let starts = self.starts();
        if let Some(window) = &mut self.settings {
            window.panel.set_starts(starts);
            window.redraw_or_say();
        }
    }

    /// Starts with Windows, or stops, and shows which it does now.
    fn start_with_windows(&mut self, on: bool) {
        if let Some(autostart) = &self.services.autostart
            && let Err(error) = autostart.set(on)
        {
            eprintln!("hyalo: cannot change starting with Windows: {error}");
        }
        self.refresh_starts();
    }

    /// Makes `change` in the config file, and shows what came of it. The
    /// watcher puts it in force, as it does an edit in a text editor.
    fn change(&mut self, change: Change) {
        let Some(path) = self.services.config.clone() else {
            return;
        };
        let made = self.write_change(&path, &change);
        let Some(window) = &mut self.settings else {
            return;
        };
        match made {
            Ok(text) => {
                let written = text.and_then(|text| config::written(&text).ok());
                window.panel.done(&change, written);
            }
            Err(Refusal::Broken(problem)) => window.panel.reload(Err(problem)),
            Err(Refusal::Wrong(why)) => window.panel.refused(&change, why),
        }
    }

    /// Writes `change` into the file at `path` as it is now; a new hotkey
    /// is registered first, so one Windows refuses is never written, and
    /// the old one is given back if the file cannot be written. The text
    /// written, if anything changed.
    fn write_change(
        &mut self,
        path: &Path,
        change: &Change,
    ) -> std::result::Result<Option<String>, Refusal> {
        let language = self.services.windows_language;
        let Some(text) = settings::edited(path, change, language)? else {
            return Ok(None);
        };
        let before = self.hotkey();
        if let Change::Hotkey(hotkey) = change {
            let taken = |_| Refusal::Wrong(Why::Taken(*hotkey));
            self.bind(*hotkey).map_err(taken)?;
        }
        if let Err(error) = settings::write(path, &text) {
            if let Some(before) = before {
                let _ = self.bind(before);
            }
            return Err(Refusal::Wrong(Why::Unsaved(error.to_string())));
        }
        Ok(Some(text))
    }

    /// Takes in what the config file holds now.
    pub(super) fn show_written(&mut self, bytes: Vec<u8>) {
        // Checking may have been turned on or off: what is offered too.
        let versions = self.versions();
        if let Some(window) = &mut self.settings {
            window.panel.reload(settings::written(bytes));
            window.panel.set_updates(versions);
            window.redraw_or_say();
        }
    }

    /// Draws the settings window again, if it is open.
    fn redraw_settings(&mut self) {
        if let Some(window) = &mut self.settings {
            window.redraw_or_say();
        }
    }

    /// Before the launcher goes, its settings window goes.
    pub(super) fn close_settings(&mut self) {
        if let Some(window) = self.settings.take() {
            // SAFETY: the window is live; its WM_DESTROY finds no state.
            let _ = unsafe { DestroyWindow(window.hwnd) };
        }
    }
}

/// The title of the folder picker for a list, in `language`; `None` for
/// the web searches, which take no folder.
fn picking(list: List, language: Language) -> Option<&'static str> {
    let title = match list {
        List::Exclude => Text::PickExclude,
        List::RankLower => Text::PickLower,
        List::RankHigher => Text::PickHigher,
        List::Search => return None,
    };
    Some(language.say(title))
}

/// Carries out what the panel said to do. Called with no app state
/// borrowed: the folder picker runs a message loop of its own, and
/// closing destroys the window.
fn carry_out(hwnd: HWND, effect: Effect) {
    match effect {
        Effect::Close => {
            // SAFETY: the settings window, live.
            let _ = unsafe { DestroyWindow(hwnd) };
            return;
        }
        Effect::Pick(list) => pick(hwnd, list),
        Effect::Change(change) => {
            with_app(|app| app.change(change));
        }
        Effect::StartWithWindows(on) => {
            with_app(|app| app.start_with_windows(on));
        }
        Effect::OpenConfig => {
            with_app(App::open_config);
        }
        Effect::Install => {
            with_app(App::install);
        }
        Effect::Copy(text) => {
            with_app(|app| (app.services.run)(&Action::CopyText(text), false));
        }
        Effect::Paste => {
            let text = clipboard::text().unwrap_or_default();
            ours(hwnd, |window| window.panel.paste(&text));
        }
    }
    with_app(App::redraw_settings);
}

/// Opens the folder picker for `list`, and adds what is picked to it.
/// Called with no app state borrowed: the picker runs a message loop of
/// its own.
fn pick(hwnd: HWND, list: List) {
    let Some(picker) = with_app(|app| Rc::clone(&app.services.picker)) else {
        return;
    };
    let language = ours(hwnd, |window| window.panel.language()).unwrap_or_default();
    let Some(title) = picking(list, language) else {
        return;
    };
    if let Some(path) = picker.pick(hwnd, title) {
        with_app(|app| app.change(Change::Add(list, path)));
    }
}

/// Runs `f` on the window's state, when `hwnd` is the settings window
/// the app has.
fn ours<R>(hwnd: HWND, f: impl FnOnce(&mut SettingsWindow) -> R) -> Option<R> {
    with_app(|app| {
        app.settings
            .as_mut()
            .filter(|window| window.hwnd == hwnd)
            .map(f)
    })
    .flatten()
}

extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(answer) = framing(hwnd, message, wparam, lparam) {
        return answer;
    }
    match ours(hwnd, |window| window.input(message, wparam, lparam)).flatten() {
        Some(effect) => {
            if let Some(effect) = effect {
                carry_out(hwnd, effect);
            }
            LRESULT(0)
        }
        // SAFETY: Windows' own handling of the rest.
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// What the window does with the messages about the window itself: its
/// end, its DPI, its least size, its coming to the front. `None` hands
/// the message on.
fn framing(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match message {
        WM_DESTROY => forget(hwnd),
        WM_DPICHANGED => on_dpi_changed(hwnd, wparam, lparam),
        WM_GETMINMAXINFO => least_size(hwnd, lparam),
        // Windows' own switch may have changed starting with Windows;
        // Windows still gives the window the focus.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE => {
            with_app(App::refresh_starts);
            return None;
        }
        _ => return None,
    }
    Some(LRESULT(0))
}

/// The window is going: the app forgets it.
fn forget(hwnd: HWND) {
    with_app(|app| {
        if app
            .settings
            .as_ref()
            .is_some_and(|window| window.hwnd == hwnd)
        {
            app.settings = None;
        }
    });
}

/// The window moved to a monitor of another DPI: it takes the size
/// Windows suggests, and the WM_SIZE that follows draws it at the new
/// DPI.
fn on_dpi_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    let dpi = (wparam.0 & 0xFFFF) as u32;
    ours(hwnd, |window| window.dpi = dpi);
    // SAFETY: with WM_DPICHANGED, lParam points to the suggested RECT.
    let suggested = unsafe { *(lparam.0 as *const RECT) };
    // SAFETY: a plain call on a window of this thread; no app state is
    // borrowed, so the WM_SIZE it sends is handled.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            suggested.left,
            suggested.top,
            suggested.right - suggested.left,
            suggested.bottom - suggested.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// Keeps the window from being made too small to use.
fn least_size(hwnd: HWND, lparam: LPARAM) {
    // SAFETY: a plain call on a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let (width, height) = layout::settings_least(layout::scale(dpi));
    // SAFETY: with WM_GETMINMAXINFO, lParam points to a MINMAXINFO.
    let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
    info.ptMinTrackSize.x = width;
    info.ptMinTrackSize.y = height;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use hyalo_core::config::DEFAULT;
    use hyalo_core::hotkey::Hotkey;
    use hyalo_core::item::Action;
    use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VIRTUAL_KEY, VK_A, VK_C, VK_CONTROL, VK_ESCAPE, VK_HOME, VK_K, VK_RETURN, VK_SHIFT,
        VK_SPACE, VK_TAB, VK_V, VK_X,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowTextW, IsWindow, IsWindowVisible,
        SendMessageW, WM_HOTKEY, WS_CAPTION, WS_EX_TOOLWINDOW,
    };

    use super::super::tests::{
        Chooser, Keyboard, Launcher, Ran, Taskbar, config_file, hold, listing, named,
        pump_until_closed, release_json, releasing, services,
    };
    use super::super::{HOTKEY_TOGGLE, MOUSE_SHIFT, tray};
    use super::*;
    use crate::autostart::{self, Autostart};
    use crate::scratch::Scratch;
    use crate::surface::Frame;
    use crate::test_desktop::{in_own_window_station, on_hidden_desktop};
    use crate::update::Updates;

    /// A launcher on a config of its own, with the stand-ins a test looks
    /// at. The launcher goes first, then its folder.
    struct Setup {
        launcher: Launcher,
        taskbar: Rc<Taskbar>,
        keyboard: Rc<Keyboard>,
        chooser: Rc<Chooser>,
        ran: Ran,
        path: PathBuf,
        _scratch: Scratch,
    }

    /// A launcher whose config holds `text`, starting with Windows as
    /// `autostart` has it.
    fn set_up(test: &str, text: &str, autostart: Option<Autostart>) -> Setup {
        set_up_updating(test, text, autostart, None)
    }

    /// The same, updating itself with `updates`.
    fn set_up_updating(
        test: &str,
        text: &str,
        autostart: Option<Autostart>,
        updates: Option<Updates>,
    ) -> Setup {
        let (scratch, path) = config_file(test, text);
        let (mut services, ran) = services(listing(&["Notepad"]).0, None);
        let (taskbar, keyboard) = (Rc::new(Taskbar::default()), Rc::new(Keyboard::default()));
        let chooser = Rc::new(Chooser::default());
        services.tray = taskbar.clone();
        services.hotkeys = Box::new(keyboard.clone());
        services.picker = chooser.clone();
        services.config = Some(path.clone());
        services.autostart = autostart;
        services.updates = updates;
        let launcher = Launcher::serving(services);
        Setup {
            launcher,
            taskbar,
            keyboard,
            chooser,
            ran,
            path,
            _scratch: scratch,
        }
    }

    impl Setup {
        /// Opens the settings window from the tray's menu.
        fn settings(&self) -> Settings {
            self.launcher.pick(&self.taskbar, tray::Command::Settings);
            Settings::find()
        }

        fn file(&self) -> String {
            fs::read_to_string(&self.path).expect("the config")
        }

        /// Handles messages until `done` holds for the settings window.
        fn until(&self, done: impl Fn(&SettingsWindow) -> bool) {
            self.launcher
                .pump_until(|app| app.settings.as_ref().is_some_and(&done));
        }
    }

    /// The settings window as a test drives it: through the messages
    /// Windows would send.
    struct Settings(HWND);

    impl Settings {
        fn find() -> Self {
            let open = with_app(|app| app.settings.as_ref().map(|window| window.hwnd));
            Self(open.flatten().expect("a settings window"))
        }

        fn state<R>(&self, f: impl FnOnce(&SettingsWindow) -> R) -> R {
            let window = |app: &mut App| f(app.settings.as_ref().expect("open"));
            with_app(window).expect("app state")
        }

        fn send(&self, message: u32, wparam: usize, lparam: isize) -> LRESULT {
            unsafe { SendMessageW(self.0, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))) }
        }

        /// Where `target` is, in the client pixels a mouse message
        /// carries.
        fn point(&self, target: Target) -> isize {
            let (area, scroll, dpi) = self.state(|window| {
                let area = window.laid.area(target).expect("on the page");
                (area, window.panel.scroll(), window.dpi)
            });
            let scale = layout::scale(dpi);
            let (x, y) = area.middle();
            let (x, y) = ((x * scale) as isize, ((y - scroll) * scale) as isize);
            (y & 0xFFFF) << 16 | (x & 0xFFFF)
        }

        fn click(&self, target: Target) {
            let at = self.point(target);
            self.send(WM_LBUTTONDOWN, 0, at);
            self.send(WM_LBUTTONUP, 0, at);
        }

        /// The point `x` DIPs right of where the text of the field
        /// `target` starts, in its middle, in client pixels.
        fn point_in(&self, target: Target, x: f32) -> isize {
            let (area, scroll, dpi) = self.state(|window| {
                let area = window.laid.area(target).expect("on the page");
                (area, window.panel.scroll(), window.dpi)
            });
            let scale = layout::scale(dpi);
            let x = page::field_inside(area).x + x;
            let y = area.middle().1 - scroll;
            let (x, y) = ((x * scale) as isize, (y * scale) as isize);
            (y & 0xFFFF) << 16 | (x & 0xFFFF)
        }

        /// The selection in the name field of `list`, in UTF-16 units.
        fn selection(&self, list: List) -> std::ops::Range<u32> {
            let marks = self.state(|window| window.laid.marks.clone());
            let mut fields = marks.into_iter().filter_map(|mark| match mark.what {
                What::Field {
                    target, selection, ..
                } if target == Target::Name(list) => Some(selection),
                _ => None,
            });
            fields.next().expect("the field")
        }

        fn press(&self, key: VIRTUAL_KEY) {
            self.send(WM_KEYDOWN, usize::from(key.0), 0);
        }

        fn type_text(&self, text: &str) {
            for unit in text.encode_utf16() {
                self.send(WM_CHAR, usize::from(unit), 0);
            }
        }

        /// The texts on the page: warnings marked `!`, what fields hold in
        /// brackets, the recorder's keys joined by `+` in braces.
        fn texts(&self) -> Vec<String> {
            let marks = self.state(|window| window.laid.marks.clone());
            marks
                .into_iter()
                .filter_map(|mark| match mark.what {
                    What::Name(text) | What::Entry(text) | What::Line(text) => Some(text),
                    What::Help { text, alarm: true } => Some(format!("! {text}")),
                    What::Field { text, .. } if !text.is_empty() => Some(format!("[{text}]")),
                    What::Keys { caps, .. } => Some(format!("{{{}}}", caps.join("+"))),
                    _ => None,
                })
                .collect()
        }

        /// Whether the window keeps `message` from Windows' own handling.
        /// Asked of the window's input directly: a test must never send
        /// what Windows would answer with a beep, which the hidden desktop
        /// plays on the user's speakers.
        fn takes(&self, message: u32, wparam: usize) -> bool {
            let window = |app: &mut App| {
                let window = app.settings.as_mut().expect("open");
                window.input(message, WPARAM(wparam), LPARAM(0)).is_some()
            };
            with_app(window).expect("app state")
        }

        fn shows(&self, text: &str) -> bool {
            self.texts().iter().any(|shown| shown == text)
        }

        fn open(&self) -> bool {
            unsafe { IsWindow(Some(self.0)) }.as_bool()
        }

        fn frame(&self) -> Frame {
            self.state(|window| window.painter.read_frame())
                .expect("read the frame back")
        }

        fn dwm(&self, attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE) -> i32 {
            let mut value = 0_i32;
            let size = size_of::<i32>() as u32;
            unsafe {
                DwmGetWindowAttribute(self.0, attribute, (&raw mut value).cast(), size)
                    .expect("read a DWM attribute");
            }
            value
        }
    }

    fn default_with(old: &str, new: &str) -> String {
        assert!(DEFAULT.contains(old), "{old}");
        DEFAULT.replace(old, new)
    }

    #[test]
    fn the_tray_opens_one_settings_window_with_a_title_bar_and_esc_closes_it() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-open", DEFAULT, None);
            let settings = setup.settings();
            assert!(unsafe { IsWindowVisible(settings.0) }.as_bool());
            let style = unsafe { GetWindowLongPtrW(settings.0, GWL_STYLE) } as u32;
            assert_eq!(style & WS_CAPTION.0, WS_CAPTION.0, "a title bar");
            let ex = unsafe { GetWindowLongPtrW(settings.0, GWL_EXSTYLE) } as u32;
            assert_eq!(ex & WS_EX_TOOLWINDOW.0, 0, "a taskbar button");
            assert_eq!(
                setup.settings().0,
                settings.0,
                "the same one, brought forward"
            );
            settings.press(VK_ESCAPE);
            assert!(!settings.open());
            assert!(with_app(|app| app.settings.is_none()).expect("app state"));
            assert!(setup.settings().open(), "opens again");
        });
    }

    #[test]
    fn it_is_dark_over_mica() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-mica", DEFAULT, None);
            let settings = setup.settings();
            assert_eq!(settings.dwm(DWMWA_USE_IMMERSIVE_DARK_MODE), 1);
            assert_eq!(settings.dwm(DWMWA_SYSTEMBACKDROP_TYPE), DWMSBT_MAINWINDOW.0);
        });
    }

    #[test]
    fn it_shows_the_settings_the_file_writes_and_draws_them() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-shows", "hotkey = 'F4'\n", None);
            let settings = setup.settings();
            assert!(settings.shows("{F4}"));
            assert!(settings.shows(r"C:\Windows\WinSxS"));
            assert!(settings.shows("target"));
            let frame = settings.frame();
            let (width, height) = client_size(settings.0);
            assert_eq!(
                (frame.width, frame.height),
                (width as usize, height as usize)
            );
            let lit = (0..frame.height).filter(|&y| frame.pixel(frame.width / 2, y)[3] > 0);
            assert!(lit.count() > 100, "cards drawn");
        });
    }

    #[test]
    fn a_recorded_hotkey_is_registered_and_written_with_the_comments_kept() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-record", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            assert!(settings.state(|window| window.panel.listening()));
            hold(&[VK_CONTROL, VK_SHIFT]);
            settings.press(VK_K);
            hold(&[]);
            assert_eq!(setup.keyboard.held(), ["Ctrl+Shift+K"]);
            let expected = default_with("\"Alt+Space\"", "\"Ctrl+Shift+K\"");
            assert_eq!(setup.file(), expected);
            assert!(settings.shows("{Ctrl+Shift+K}"), "at once");
            setup
                .launcher
                .pump_until(|app| app.config.hotkey.to_string() == "Ctrl+Shift+K");
            assert_eq!(setup.keyboard.held(), ["Ctrl+Shift+K"], "in force once");
            let tip = setup.taskbar.tips.borrow().last().cloned();
            assert_eq!(tip.as_deref(), Some("hyalo · Ctrl+Shift+K"));
        });
    }

    #[test]
    fn a_hotkey_windows_refuses_is_told_and_neither_registered_nor_written() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-refused", DEFAULT, None);
            setup
                .keyboard
                .taken
                .borrow_mut()
                .push(Hotkey::parse("Ctrl+K").expect("Ctrl+K"));
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            hold(&[VK_CONTROL]);
            settings.press(VK_K);
            hold(&[]);
            assert!(settings.shows("! Ctrl+K is taken by another app"));
            assert!(settings.shows("{Alt+Space}"));
            assert_eq!(setup.keyboard.held(), ["Alt+Space"]);
            assert_eq!(setup.file(), DEFAULT);
        });
    }

    #[test]
    fn the_hotkey_pressed_while_recording_is_the_one_there_is() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-own-hotkey", DEFAULT, None);
            setup.launcher.press(VK_ESCAPE);
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            setup.launcher.send(WM_HOTKEY, HOTKEY_TOGGLE as usize);
            assert!(!setup.launcher.visible(), "taken by the recorder");
            assert!(!settings.state(|window| window.panel.listening()));
            assert_eq!(setup.file(), DEFAULT);
            setup.launcher.send(WM_HOTKEY, HOTKEY_TOGGLE as usize);
            assert!(setup.launcher.visible(), "the launcher's again");
        });
    }

    #[test]
    fn esc_while_recording_keeps_the_hotkey_and_the_window() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-cancel", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            settings.press(VK_ESCAPE);
            assert!(settings.open());
            assert!(!settings.state(|window| window.panel.listening()));
            settings.press(VK_ESCAPE);
            assert!(!settings.open());
        });
    }

    #[test]
    fn alt_while_recording_is_a_key_not_the_window_menu() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-alt", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            hold(&[windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU]);
            let menu = windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
            assert_eq!(
                settings.send(WM_SYSKEYDOWN, usize::from(menu.0), 0),
                LRESULT(0)
            );
            assert!(settings.shows("{Alt}"));
            settings.send(WM_SYSKEYDOWN, usize::from(VK_K.0), 0);
            assert!(
                settings.takes(WM_SYSCHAR, usize::from(b'k')),
                "the K the recorder took"
            );
            hold(&[]);
            assert!(settings.takes(WM_SYSKEYUP, usize::from(menu.0)));
            assert_eq!(setup.keyboard.held(), ["Alt+K"]);
        });
    }

    #[test]
    fn an_alt_key_never_looks_for_a_menu_bar_there_is_none_of() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-no-menu", DEFAULT, None);
            let settings = setup.settings();
            let menu = windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
            assert!(
                !settings.takes(WM_SYSCHAR, usize::from(b' ')),
                "Alt+Space opens the window menu, from the start"
            );
            assert!(
                settings.takes(WM_SYSCHAR, usize::from(b'k')),
                "Alt+K, nothing recording"
            );
            assert!(
                settings.takes(WM_SYSKEYUP, usize::from(menu.0)),
                "Alt let go: no menu mode"
            );
            settings.click(Target::Hotkey);
            hold(&[menu]);
            settings.send(WM_SYSKEYDOWN, usize::from(VK_SPACE.0), 0);
            hold(&[]);
            assert!(!settings.state(|window| window.panel.listening()), "heard");
            assert!(
                settings.takes(WM_SYSCHAR, usize::from(b' ')),
                "the recorder's Alt+Space opens no menu"
            );
            assert!(
                !settings.takes(WM_SYSCHAR, usize::from(b' ')),
                "the next Alt+Space does"
            );
        });
    }

    /// The window's title, as Windows has it.
    fn title(hwnd: HWND) -> String {
        let mut title = [0_u16; 64];
        let length = unsafe { GetWindowTextW(hwnd, &mut title) };
        String::from_utf16_lossy(&title[..length as usize])
    }

    #[test]
    fn choosing_russian_writes_it_and_the_window_and_the_tray_speak_it() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-language", DEFAULT, None);
            let settings = setup.settings();
            assert_eq!(title(settings.0), "hyalo settings");
            assert!(settings.shows("Hotkey"));
            settings.click(Target::Language(Language::Russian));
            let russian = DEFAULT.replacen("language = \"en\"", "language = \"ru\"", 1);
            assert_eq!(setup.file(), russian, "that line alone");
            assert!(settings.shows("Сочетание клавиш"));
            assert_eq!(title(settings.0), "Параметры hyalo");
            setup
                .launcher
                .pump_until(|app| app.language() == Language::Russian);
            setup.launcher.tray(super::super::WM_CONTEXTMENU, (0, 0));
            let menus = setup.taskbar.menus.borrow();
            let (lines, _) = menus.last().expect("a menu");
            let exit = lines.iter().any(|line| match line {
                tray::Line::Entry(entry) => entry.label == "Выход",
                tray::Line::Separator => false,
            });
            assert!(exit);
        });
    }

    #[test]
    fn the_picker_asks_in_the_pages_language() {
        on_hidden_desktop(|| {
            let text = config::default_file(Language::Russian);
            let setup = set_up("settings-window-picker-russian", &text, None);
            let settings = setup.settings();
            settings.click(Target::Pick(List::RankHigher));
            let asked = setup.chooser.asked.borrow().clone();
            assert_eq!(asked, ["Папка, файлы из которой показывать выше"]);
        });
    }

    #[test]
    fn alt_f4_closes_the_settings_window_and_leaves_the_launcher() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-alt-f4-closes", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Name(List::RankLower));
            super::super::tests::alt_f4(settings.0);
            // Windows posts the close Alt+F4 asks for.
            super::super::tests::drain();
            assert!(!settings.open());
            assert!(unsafe { IsWindow(Some(setup.launcher.0)) }.as_bool());
        });
    }

    #[test]
    fn alt_f4_while_recording_closes_the_window_and_takes_no_hotkey() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-alt-f4", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Hotkey);
            let menu = windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
            hold(&[menu]);
            let f4 = windows::Win32::UI::Input::KeyboardAndMouse::VK_F4;
            settings.send(WM_SYSKEYDOWN, usize::from(f4.0), 0);
            hold(&[]);
            assert!(!settings.open());
            assert_eq!(setup.keyboard.held(), ["Alt+Space"]);
            assert_eq!(setup.file(), DEFAULT);
        });
    }

    #[test]
    fn the_switch_starts_hyalo_with_windows_and_stops_it_by_mouse_and_keyboard() {
        on_hidden_desktop(|| {
            let keys = autostart::tests::Keys::new("settings-window");
            let exe = r"C:\Programs\hyalo\hyalo.exe";
            let setup = set_up("settings-window-start", DEFAULT, Some(keys.autostart(exe)));
            let settings = setup.settings();
            settings.click(Target::StartWithWindows);
            assert_eq!(keys.run_value(), Some(format!("\"{exe}\" --hidden")));
            let on = settings.state(|window| {
                window
                    .laid
                    .marks
                    .iter()
                    .any(|mark| matches!(mark.what, What::Toggle { on: true, .. }))
            });
            assert!(on, "shown on");
            settings.press(VK_SPACE);
            assert_eq!(
                keys.run_value(),
                None,
                "Space on the switch the click focused"
            );
            hold(&[VK_SHIFT]);
            settings.press(VK_TAB);
            hold(&[]);
            assert!(settings.state(|window| window.panel.look(Target::Hotkey).ring));
            settings.press(VK_TAB);
            settings.press(VK_RETURN);
            assert!(keys.run_value().is_some(), "Enter on the switch Tab rang");
        });
    }

    #[test]
    fn a_debug_build_has_no_switch() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-no-switch", DEFAULT, None);
            let settings = setup.settings();
            assert!(!settings.shows("Start with Windows"));
        });
    }

    #[test]
    fn a_folder_picked_is_written_and_its_cross_takes_it_out_again() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-pick", DEFAULT, None);
            let settings = setup.settings();
            *setup.chooser.next.borrow_mut() = Some(r"D:\old".to_string());
            settings.click(Target::Pick(List::Exclude));
            assert_eq!(
                *setup.chooser.asked.borrow(),
                ["Leave a folder out of the index"]
            );
            let added = default_with(r"['C:\Windows\WinSxS']", r"['C:\Windows\WinSxS', 'D:\old']");
            assert_eq!(setup.file(), added);
            assert!(settings.shows(r"D:\old"));
            settings.click(Target::Remove(List::Exclude, 1));
            assert_eq!(setup.file(), DEFAULT, "the comments survive both");
            assert!(!settings.shows(r"D:\old"));
        });
    }

    #[test]
    fn a_pick_cancelled_changes_nothing() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-cancelled", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Pick(List::RankHigher));
            assert_eq!(*setup.chooser.asked.borrow(), ["Rank a folder higher"]);
            assert_eq!(setup.file(), DEFAULT);
            settings.click(Target::Pick(List::RankLower));
            assert_eq!(setup.chooser.asked.borrow()[1], "Rank a folder lower");
        });
    }

    #[test]
    fn a_folder_name_typed_is_added_and_leaves_its_field() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-name", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Name(List::RankLower));
            settings.type_text("vendor");
            assert!(settings.shows("[vendor]"));
            settings.press(VK_RETURN);
            assert_eq!(
                setup.file(),
                default_with("['target']", "['target', 'vendor']")
            );
            assert!(settings.shows("vendor"));
            assert!(!settings.shows("[vendor]"));
        });
    }

    #[test]
    fn a_web_search_typed_is_written_in_the_search_table_and_its_cross_takes_it_out() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-search", DEFAULT, None);
            let settings = setup.settings();
            let so = "so https://stackoverflow.com/search?q={query}";
            settings.click(Target::Name(List::Search));
            settings.type_text(so);
            settings.click(Target::AddName(List::Search));
            let line = "so = \"https://stackoverflow.com/search?q={query}\"\n";
            assert_eq!(setup.file(), format!("{DEFAULT}{line}"));
            assert!(settings.shows(so));
            let written = config::written(&setup.file()).expect("good settings");
            let listed = written.list(List::Search);
            let at = listed.iter().position(|entry| entry == so).expect("listed");
            settings.click(Target::Remove(List::Search, at));
            assert_eq!(setup.file(), DEFAULT, "the comments survive both");
        });
    }

    #[test]
    fn a_click_in_a_name_field_places_the_caret_and_dragging_selects() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-field-mouse", DEFAULT, None);
            let settings = setup.settings();
            let name = Target::Name(List::RankLower);
            settings.click(name);
            settings.type_text("vendor");
            let start = settings.point_in(name, 0.0);
            settings.send(WM_LBUTTONDOWN, 0, start);
            settings.send(WM_LBUTTONUP, 0, start);
            settings.type_text("X");
            assert!(settings.shows("[Xvendor]"));
            let past = settings.point_in(name, 300.0);
            settings.send(WM_LBUTTONDOWN, 0, start);
            settings.send(WM_MOUSEMOVE, 0, past);
            settings.send(WM_LBUTTONUP, 0, past);
            assert_eq!(settings.selection(List::RankLower), 0..7);
            settings.send(WM_MOUSEMOVE, 0, start);
            assert_eq!(
                settings.selection(List::RankLower),
                0..7,
                "the button is up"
            );
            settings.type_text("y");
            assert!(settings.shows("[y]"));
        });
    }

    #[test]
    fn a_click_in_a_name_field_scrolled_to_its_end_lands_where_it_was_clicked() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-field-scrolled-click", DEFAULT, None);
            let settings = setup.settings();
            let name = Target::Name(List::RankLower);
            settings.click(name);
            let long = "a_folder_name_longer_than_the_field_shows_".repeat(3);
            settings.type_text(&long);
            let area = settings.state(|window| window.laid.area(name).expect("on the page"));
            let near_the_end = settings.point_in(name, page::field_inside(area).width - 4.0);
            settings.send(WM_LBUTTONDOWN, 0, near_the_end);
            settings.send(WM_LBUTTONUP, 0, near_the_end);
            let caret = settings.selection(List::RankLower);
            let length = long.len() as u32;
            assert!(
                caret.is_empty() && caret.start + 3 >= length,
                "{caret:?} of {length}"
            );
        });
    }

    #[test]
    fn shift_double_and_triple_clicks_select_in_a_name_field() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-field-clicks", DEFAULT, None);
            let settings = setup.settings();
            let name = Target::Name(List::RankHigher);
            settings.click(name);
            settings.type_text("my vendor");
            settings.press(VK_HOME);
            let past = settings.point_in(name, 300.0);
            settings.send(WM_LBUTTONDOWN, MOUSE_SHIFT, past);
            settings.send(WM_LBUTTONUP, MOUSE_SHIFT, past);
            assert_eq!(settings.selection(List::RankHigher), 0..9);
            let on_vendor = settings.state(|window| window.painter.measure("my ven"));
            let on_vendor = settings.point_in(name, on_vendor);
            settings.send(WM_LBUTTONDOWN, 0, on_vendor);
            settings.send(WM_LBUTTONUP, 0, on_vendor);
            settings.send(WM_LBUTTONDBLCLK, 0, on_vendor);
            settings.send(WM_LBUTTONUP, 0, on_vendor);
            assert_eq!(settings.selection(List::RankHigher), 3..9);
            settings.send(WM_LBUTTONDOWN, 0, on_vendor);
            settings.send(WM_LBUTTONUP, 0, on_vendor);
            assert_eq!(settings.selection(List::RankHigher), 0..9);
        });
    }

    #[test]
    fn a_double_click_elsewhere_is_a_click() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-double-switch", DEFAULT, None);
            let settings = setup.settings();
            let pick = settings.point(Target::Pick(List::Exclude));
            settings.send(WM_LBUTTONDOWN, 0, pick);
            settings.send(WM_LBUTTONUP, 0, pick);
            settings.send(WM_LBUTTONDBLCLK, 0, pick);
            settings.send(WM_LBUTTONUP, 0, pick);
            let asked = setup.chooser.asked.borrow().len();
            assert_eq!(asked, 2, "two clicks, two pickers");
        });
    }

    #[test]
    fn ctrl_v_pastes_into_a_name_field_and_ctrl_c_copies_from_it() {
        let name = "window::settings_window::tests::ctrl_v_pastes_into_a_name_field_and_ctrl_c_copies_from_it";
        in_own_window_station(name, || {
            on_hidden_desktop(|| {
                clipboard::copy("vendor\r\n").expect("copy");
                let setup = set_up("settings-window-paste", DEFAULT, None);
                let settings = setup.settings();
                settings.click(Target::Name(List::RankLower));
                hold(&[VK_CONTROL]);
                settings.press(VK_V);
                assert!(settings.shows("[vendor]"));
                settings.press(VK_A);
                settings.press(VK_C);
                let copied = (Action::CopyText("vendor".to_string()), false);
                assert_eq!(setup.ran.borrow().last(), Some(&copied));
                settings.press(VK_X);
                assert!(!settings.shows("[vendor]"));
                assert_eq!(setup.ran.borrow().len(), 2);
            });
        });
    }

    #[test]
    fn a_name_that_is_no_place_is_told_and_stays_in_its_field() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-wrong", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Name(List::RankHigher));
            settings.type_text(r"old\stuff");
            settings.click(Target::AddName(List::RankHigher));
            assert_eq!(setup.file(), DEFAULT);
            assert!(settings.shows(r"[old\stuff]"));
            let told = settings
                .texts()
                .into_iter()
                .find(|text| text.starts_with('!'));
            assert!(told.expect("a warning").contains(r"`old\stuff`"));
        });
    }

    #[test]
    fn edits_saved_in_the_file_show_up_and_what_is_typed_stays() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-follow", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Name(List::RankLower));
            settings.type_text("ven");
            let edited = default_with("\"Alt+Space\"", "\"F4\"").replace("['target']", "['build']");
            fs::write(&setup.path, &edited).expect("save");
            setup.until(|window| {
                window
                    .laid
                    .marks
                    .iter()
                    .any(|mark| mark.what == What::Entry("build".to_string()))
            });
            assert!(settings.shows("{F4}"));
            assert!(!settings.shows("target"));
            assert!(settings.shows("[ven]"));
            settings.type_text("dor");
            assert!(settings.shows("[vendor]"), "still typing there");
        });
    }

    #[test]
    fn a_file_broken_in_an_editor_is_shown_and_never_written() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-broken", DEFAULT, None);
            let settings = setup.settings();
            let broken = "\nhotkye = 'F4'\n";
            fs::write(&setup.path, broken).expect("save");
            setup.until(|window| window.laid.area(Target::Fix).is_some());
            assert!(settings.shows("config.toml has a problem"));
            assert!(
                settings
                    .texts()
                    .iter()
                    .any(|text| text.starts_with("Line 2: unknown field `hotkye`"))
            );
            settings.click(Target::Remove(List::Exclude, 0));
            assert_eq!(setup.file(), broken);
            settings.click(Target::Fix);
            let opened = Action::Edit(setup.path.to_string_lossy().into_owned());
            assert_eq!(*setup.ran.borrow(), [(opened.clone(), false)]);
            settings.click(Target::OpenConfig);
            assert_eq!(setup.ran.borrow().len(), 2);
        });
    }

    #[test]
    fn a_change_made_as_the_file_breaks_is_refused_with_its_problem() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-race", DEFAULT, None);
            let settings = setup.settings();
            let broken = "hotkey = 'F4'\n[folders\n";
            fs::write(&setup.path, broken).expect("save");
            // Before the watcher has told the window.
            settings.click(Target::Remove(List::RankLower, 0));
            assert_eq!(setup.file(), broken);
            assert!(settings.shows("config.toml has a problem"));
        });
    }

    #[test]
    fn its_own_writes_come_back_without_a_change_on_screen() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-echo", DEFAULT, None);
            let settings = setup.settings();
            settings.click(Target::Remove(List::RankLower, 0));
            let before = settings.frame();
            setup
                .launcher
                .pump_until(|app| app.config.rank_lower.is_empty());
            let after = settings.frame();
            let same = (0..before.height)
                .all(|y| (0..before.width).all(|x| before.pixel(x, y) == after.pixel(x, y)));
            assert!(same, "the same frame");
            assert_eq!(setup.file(), default_with("['target']", "[]"));
        });
    }

    #[test]
    fn the_wheel_scrolls_a_page_taller_than_the_window() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-wheel", DEFAULT, None);
            let settings = setup.settings();
            unsafe {
                SetWindowPos(
                    settings.0,
                    None,
                    0,
                    0,
                    600,
                    420,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            }
            .expect("make it small");
            assert_eq!(settings.state(|window| window.panel.scroll()), 0.0);
            let down = (-120_i16 as u16 as usize) << 16;
            settings.send(WM_MOUSEWHEEL, down, 0);
            assert_eq!(settings.state(|window| window.panel.scroll()), 48.0);
            let (_, height) = settings.state(|window| window.painter.view());
            assert!(height < 420.0, "drawn at the new size: {height}");
        });
    }

    /// Makes the settings window small, so the page outgrows it.
    fn shrink(settings: &Settings) {
        let flags = SWP_NOZORDER | SWP_NOACTIVATE;
        unsafe { SetWindowPos(settings.0, None, 0, 0, 600, 420, flags) }.expect("make it small");
    }

    #[test]
    fn the_wheel_stops_at_the_bottom_and_a_click_finds_the_control_scrolled_under_it() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-scrolled", DEFAULT, None);
            let settings = setup.settings();
            shrink(&settings);
            let far = (-12000_i16 as u16 as usize) << 16;
            settings.send(WM_MOUSEWHEEL, far, 0);
            let (scroll, page, view) = settings.state(|window| {
                (
                    window.panel.scroll(),
                    window.laid.height,
                    window.painter.view().1,
                )
            });
            assert_eq!(scroll, page - view, "the bottom of the page");
            settings.click(Target::Pick(List::RankHigher));
            assert_eq!(*setup.chooser.asked.borrow(), ["Rank a folder higher"]);
        });
    }

    #[test]
    fn tab_brings_a_control_below_into_a_short_window() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-reveal", DEFAULT, None);
            let settings = setup.settings();
            shrink(&settings);
            hold(&[VK_SHIFT]);
            settings.press(VK_TAB);
            hold(&[]);
            let (scroll, bottom, view) = settings.state(|window| {
                let last = window
                    .laid
                    .area(Target::OpenConfig)
                    .expect("the last control");
                (
                    window.panel.scroll(),
                    last.bottom(),
                    window.painter.view().1,
                )
            });
            assert_eq!(scroll, bottom + 12.0 - view);
        });
    }

    #[test]
    fn either_windows_key_is_win() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-win", DEFAULT, None);
            let settings = setup.settings();
            let f = |n: u16| VIRTUAL_KEY(0x6F + n);
            for (win, key, name) in [(VK_LWIN, f(2), "Win+F2"), (VK_RWIN, f(3), "Win+F3")] {
                settings.click(Target::Hotkey);
                hold(&[win]);
                settings.press(key);
                hold(&[]);
                assert_eq!(setup.keyboard.held(), [name]);
            }
        });
    }

    #[test]
    fn a_config_that_cannot_be_read_is_shown_as_a_problem() {
        on_hidden_desktop(|| {
            let scratch =
                Scratch::new("settings-window-unreadable").with(&["hyalo\\config.toml\\"]);
            let (mut services, _) = services(listing(&[]).0, None);
            let taskbar = Rc::new(Taskbar::default());
            services.tray = taskbar.clone();
            services.config = Some(scratch.0.join("hyalo").join("config.toml"));
            let launcher = Launcher::serving(services);
            launcher.pick(&taskbar, tray::Command::Settings);
            let settings = Settings::find();
            assert!(settings.shows("config.toml has a problem"));
            assert!(settings.shows("Line 1: config.toml cannot be read"));
        });
    }

    #[test]
    fn coming_to_the_front_shows_whether_hyalo_starts_with_windows_now() {
        on_hidden_desktop(|| {
            let keys = autostart::tests::Keys::new("settings-window-activate");
            let exe = r"C:\Programs\hyalo\hyalo.exe";
            let setup = set_up(
                "settings-window-activate",
                DEFAULT,
                Some(keys.autostart(exe)),
            );
            let settings = setup.settings();
            let on = || {
                let marks = settings.state(|window| window.laid.marks.clone());
                marks
                    .iter()
                    .any(|mark| matches!(mark.what, What::Toggle { on: true, .. }))
            };
            keys.autostart(exe).set(true).expect("on, elsewhere");
            settings.send(WM_ACTIVATE, WA_INACTIVE as usize, 0);
            assert!(!on(), "losing the focus asks nothing");
            settings.send(
                WM_ACTIVATE,
                windows::Win32::UI::WindowsAndMessaging::WA_ACTIVE as usize,
                0,
            );
            assert!(on(), "coming to the front asks again");
        });
    }

    #[test]
    fn the_pointer_is_watched_for_leaving_on_every_visit() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-leave", DEFAULT, None);
            let settings = setup.settings();
            let target = Target::Pick(List::RankHigher);
            let left = |app: &App| {
                let window = app.settings.as_ref();
                window.is_some_and(|window| !window.panel.look(target).hovered)
            };
            // The pointer is never over a window of the hidden desktop: once
            // asked to watch, Windows says at once that it left.
            for _visit in 0..2 {
                settings.send(WM_MOUSEMOVE, 0, settings.point(target));
                assert!(settings.state(|window| window.panel.look(target).hovered));
                setup.launcher.pump_until(left);
            }
        });
    }

    #[test]
    fn the_pointer_lights_the_control_it_is_over() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-hover", DEFAULT, None);
            let settings = setup.settings();
            let target = Target::Pick(List::RankHigher);
            settings.send(WM_MOUSEMOVE, 0, settings.point(target));
            assert!(settings.state(|window| window.panel.look(target).hovered));
            settings.send(WM_MOUSELEAVE, 0, 0);
            assert!(!settings.state(|window| window.panel.look(target).hovered));
        });
    }

    #[test]
    fn a_monitor_of_another_dpi_draws_it_anew_at_its_size() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-dpi", DEFAULT, None);
            let settings = setup.settings();
            let suggested = RECT {
                left: 10,
                top: 20,
                right: 10 + 900,
                bottom: 20 + 1200,
            };
            let dpi = 144 | 144 << 16;
            let answer = settings.send(WM_DPICHANGED, dpi, (&raw const suggested) as isize);
            assert_eq!(answer, LRESULT(0), "handled");
            assert_eq!(settings.state(|window| window.dpi), 144);
            let mut placed = RECT::default();
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindowRect(settings.0, &mut placed)
            }
            .expect("its rect");
            assert_eq!(placed, suggested, "where Windows suggests");
            let (width, height) = client_size(settings.0);
            let frame = settings.frame();
            assert_eq!(
                (frame.width, frame.height),
                (width as usize, height as usize)
            );
            let (view, _) = settings.state(|window| window.painter.view());
            assert_eq!(view, width as f32 / 1.5);
        });
    }

    #[test]
    fn it_may_not_be_made_smaller_than_its_least_size() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-least", DEFAULT, None);
            let settings = setup.settings();
            let mut info = MINMAXINFO::default();
            let answer = settings.send(WM_GETMINMAXINFO, 0, (&raw mut info) as isize);
            assert_eq!(answer, LRESULT(0), "handled");
            let dpi = settings.state(|window| window.dpi);
            let (width, height) = layout::settings_least(layout::scale(dpi));
            assert_eq!(
                (info.ptMinTrackSize.x, info.ptMinTrackSize.y),
                (width, height)
            );
        });
    }

    #[test]
    fn a_newer_release_comes_first_and_install_swaps_it_in_and_closes_hyalo() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", "Faster.\\nSmaller."));
            let (updates, releases) = releasing("settings-update-install", latest, b"new build");
            let setup = set_up_updating("settings-update", DEFAULT, None, Some(updates));
            setup.launcher.pump_until(|app| app.update.is_some());
            setup.launcher.search_for("", 1);
            setup.launcher.press(VK_RETURN);
            setup.launcher.pump_until(|app| app.settings.is_some());
            let settings = Settings::find();
            let shown = settings.texts();
            assert_eq!(
                shown[..4],
                [
                    "hyalo 0.0.2 is out",
                    "Faster.",
                    "Smaller.",
                    "Check for updates"
                ]
            );
            settings.click(Target::Install);
            pump_until_closed(setup.launcher.0);
            assert!(!settings.open(), "gone with the launcher");
            assert_eq!(
                releases.files(),
                named(&[("hyalo.exe", "new build"), ("hyalo.exe.old", "old build")])
            );
            assert_eq!(releases.started(), std::slice::from_ref(&releases.exe));
        });
    }

    #[test]
    fn the_switch_turns_checking_off_in_the_file_and_the_release_leaves_the_page() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", ""));
            let (updates, _releases) = releasing("settings-update-off", latest, b"new build");
            let setup = set_up_updating("settings-update-switch", DEFAULT, None, Some(updates));
            setup.launcher.pump_until(|app| app.update.is_some());
            let settings = setup.settings();
            assert!(settings.shows("hyalo 0.0.2 is out"));
            settings.click(Target::CheckUpdates);
            let off = DEFAULT.replace("check_for_updates = true", "check_for_updates = false");
            assert_eq!(setup.file(), off);
            setup.until(|window| {
                window.laid.area(Target::Install).is_none()
                    && window.laid.area(Target::CheckUpdates).is_some()
            });
            assert!(settings.shows("Check for updates"));
            let tip = setup.taskbar.tips.borrow().last().cloned().expect("a tip");
            assert_eq!(tip, "hyalo · Alt+Space");
            settings.click(Target::CheckUpdates);
            assert_eq!(setup.file(), DEFAULT);
            setup.until(|window| window.laid.area(Target::Install).is_some());
        });
    }

    #[test]
    fn a_build_that_does_not_install_is_told_by_install_and_the_window_stays() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", ""));
            let (updates, releases) = releasing("settings-update-refused", latest, b"another");
            let setup = set_up_updating("settings-update-bad", DEFAULT, None, Some(updates));
            setup.launcher.pump_until(|app| app.update.is_some());
            let settings = setup.settings();
            settings.click(Target::Install);
            let said = "! Not installed: the download is not the build its SHA-256 names";
            setup.until(|window| {
                let laid = &window.laid;
                laid.marks.iter().any(|mark| {
                    matches!(&mark.what, What::Help { text, alarm: true } if format!("! {text}") == said)
                })
            });
            assert!(settings.open());
            assert_eq!(releases.files(), named(&[("hyalo.exe", "old build")]));
        });
    }

    #[test]
    fn with_no_newer_release_the_page_ends_with_the_switch_and_the_file() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.1", ""));
            let (updates, _releases) = releasing("settings-update-none", latest, b"");
            let setup = set_up_updating("settings-update-newest", DEFAULT, None, Some(updates));
            setup
                .launcher
                .pump_until(|app| matches!(app.checked, hyalo_core::update::Checked::Answered(_)));
            let settings = setup.settings();
            let shown = settings.texts();
            let last = &shown[shown.len() - 2..];
            assert_eq!(last, ["Check for updates", "Edit the file instead"]);
            assert!(settings.state(|window| window.laid.area(Target::Install).is_none()));
        });
    }

    #[test]
    fn exit_takes_the_settings_window_with_the_launcher() {
        on_hidden_desktop(|| {
            let setup = set_up("settings-window-exit", DEFAULT, None);
            let settings = setup.settings();
            setup.launcher.pick(&setup.taskbar, tray::Command::Exit);
            assert!(!settings.open());
        });
    }
}
