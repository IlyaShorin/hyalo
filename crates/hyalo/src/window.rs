//! The launcher window: a borderless popup over a DWM Acrylic backdrop.
//! A global hotkey toggles it. Typing lists the matching apps, files and
//! folders under the bar, the arrows move the selection, Enter opens the
//! selected item (Ctrl+Shift+Enter as administrator), Ctrl+Enter shows a
//! file in its folder, Ctrl+C copies its path, and Esc, Enter or losing
//! focus hides it.
//!
//! The `windows` crate marks Win32 calls `unsafe`. The invariant they rely
//! on: `hwnd` is the live launcher window, owned by the UI thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use hyalo_core::config::{Config, Problem};
use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::hotkey::Hotkey;
use hyalo_core::item::{Action, Item, ItemId, Kind};
use hyalo_core::places::Places;
use hyalo_core::rank::Ranked;
use hyalo_core::text::{Language, Text};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWINDOWATTRIBUTE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
};
use windows::Win32::UI::Shell::{NIN_SELECT, NINF_KEY};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CS_DBLCLKS, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessagePos, GetMessageW,
    GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, HTCLIENT, HWND_TOPMOST, IDC_ARROW,
    IDC_IBEAM, IsWindow, IsWindowVisible, LoadCursorW, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SetCursor,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE, WM_ACTIVATE,
    WM_APP, WM_CHAR, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCHITTEST, WM_SETCURSOR,
    WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_CAPTION, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};
use windows::core::{BOOL, PCWSTR, Result, w};

use crate::apps::Icon;
use crate::autostart::Autostart;
use crate::catalog::{self, Catalog, FileIcons, Found, Provider, Request};
use crate::clipboard;
use crate::field::{self, Edit, Edited, Field};
use crate::files::{self, Excluded, Files, Roots};
use crate::highlight::utf16_ranges;
use crate::hotkeys::{self, Hotkeys};
use crate::instance::Start;
use crate::keys::{self, Command, Held};
use crate::layout::{self, Part, Position, Rect};
use crate::notice::Notice;
use crate::picker::{self, Picker};
use crate::render::{Renderer, Row, Scene};
use crate::results::{self, Listed, MAX_ROWS, Results};
use crate::settings::{self, Loaded};
use crate::tray::{self, Tray};
use crate::{http, launch, rates, store};

mod settings_window;

use settings_window::{SettingsWindow, open_settings};

/// The id the hotkey is registered with; a new hotkey takes the other
/// one, so the old one is freed only once Windows took the new one.
const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_NEXT: i32 = 2;

/// Posted by the provider's thread each time it hands over a piece.
const WM_FOUND: u32 = WM_APP + 1;

/// Posted by a second start of hyalo: show the launcher.
const WM_SHOW: u32 = WM_APP + 2;

/// Posted by a second start with `--settings`, and by the launcher itself
/// for a notice: open the settings window.
const WM_SETTINGS: u32 = WM_APP + 4;

/// Where the launcher shows: its monitor's work area and DPI.
#[derive(Debug, Clone, Copy)]
struct Placement {
    work_area: Rect,
    dpi: u32,
}

impl Placement {
    /// The launcher's rectangle with `rows` results listed, at `position`
    /// if the user dragged it somewhere.
    fn rect(self, rows: usize, position: Option<Position>) -> Rect {
        layout::launcher_rect(self.work_area, layout::scale(self.dpi), rows, position)
    }
}

/// Carries out a picked item's action, as administrator or not.
pub type Runner = Box<dyn Fn(&Action, bool)>;

/// A provider, and how long what it found is trusted.
pub struct Source {
    pub provider: Provider,
    pub trusted: Duration,
    /// What it leaves out of the folders it walks, if it walks any: the
    /// config's to set.
    pub excluded: Option<Excluded>,
}

/// What the window relies on from outside: the real ones in `run`,
/// stand-ins in tests.
pub struct Services {
    /// Find what the launcher searches.
    pub sources: Vec<Source>,
    /// The file index a provider fills.
    pub files: Files,
    pub icons: FileIcons,
    /// The user's folders, whose files rank a little higher.
    pub places: Places,
    pub run: Runner,
    /// Where picks are kept between runs; `None` keeps them in memory.
    pub picks: Option<PathBuf>,
    /// Registers the hotkey with Windows.
    pub hotkeys: Box<dyn Hotkeys>,
    /// The notification area.
    pub tray: Rc<dyn Tray>,
    /// Where the config lives; `None` runs on the defaults.
    pub config: Option<PathBuf>,
    /// Starts the launcher with Windows; `None` where it may not.
    pub autostart: Option<Autostart>,
    /// Asks the user for a folder, for the settings window.
    pub picker: Rc<dyn Picker>,
    /// Where the place the launcher was dragged to is kept; `None` keeps
    /// it in memory.
    pub position: Option<PathBuf>,
    /// The language Windows is shown in: hyalo's own, unless the config
    /// names another.
    pub windows_language: Language,
}

impl Services {
    /// The apps; the files of the fixed drives, with their snapshot in
    /// `%LOCALAPPDATA%\hyalo`; the bank's rates, kept there too; opened by
    /// the shell; picks in `%APPDATA%\hyalo`.
    fn real() -> Self {
        let (files, writer) = Files::new();
        let drives: Roots = Arc::new(files::fixed_drives);
        let excluded = Excluded::default();
        let snapshot = files::default_snapshot();
        let walk = files::provider(writer, drives, snapshot, excluded.clone());
        Self {
            sources: vec![
                Source {
                    provider: Arc::new(catalog::find_apps),
                    trusted: catalog::REFRESH_AFTER,
                    excluded: None,
                },
                Source {
                    provider: walk,
                    trusted: files::REFRESH_AFTER,
                    excluded: Some(excluded),
                },
                Source {
                    provider: rates::provider(
                        Arc::new(|| http::get(&rates::bank())),
                        rates::default_cache(),
                        Arc::new(rates::moscow_today),
                    ),
                    trusted: rates::REFRESH_AFTER,
                    excluded: None,
                },
            ],
            files,
            icons: Arc::new(catalog::file_icons),
            places: files::places(),
            run: Box::new(run_in_background),
            picks: store::default_path(),
            hotkeys: Box::new(hotkeys::System),
            tray: Rc::new(tray::Shell),
            config: settings::default_path(),
            autostart: Autostart::real(),
            picker: Rc::new(picker::System),
            position: store::ours("LOCALAPPDATA", "position.txt"),
            // SAFETY: a plain call.
            windows_language: Language::of_windows(unsafe { GetUserDefaultUILanguage() }),
        }
    }
}

/// Carries out `action` on a thread of its own: the shell can take a
/// while, and the UAC prompt waits for the user.
fn run_in_background(action: &Action, as_admin: bool) {
    let action = action.clone();
    std::thread::spawn(move || {
        if let Err(error) = launch::run(&action, as_admin) {
            eprintln!("hyalo: cannot run {action:?}: {error}");
        }
    });
}

struct App {
    hwnd: HWND,
    query: Field,
    renderer: Option<Renderer>,
    placement: Placement,
    catalog: Catalog,
    results: Results,
    frecency: Frecency,
    services: Services,
    /// When each source's provider last started; `None` before it ever
    /// did.
    found_at: Vec<Option<Instant>>,
    /// Files and folders whose icons were asked for.
    asked: HashSet<ItemId>,
    /// The provider's thread sends here and posts WM_FOUND.
    found: (Sender<Found>, Receiver<Found>),
    /// Where the pointer was last seen, in client pixels; `None` when not
    /// known.
    pointer: Option<POINT>,
    /// The hotkey Windows holds for the launcher, and its id.
    hotkey: Option<(i32, Hotkey)>,
    /// When the launcher last lost the focus while shown.
    deactivated_at: Option<Instant>,
    /// The settings in force: the config's last good ones.
    config: Config,
    /// What is wrong with the config file, if anything.
    problem: Option<Problem>,
    /// The config's hotkey, when Windows refused it.
    refused: Option<Hotkey>,
    /// Where files rank higher or lower: the user's folders and the
    /// config's.
    places: Places,
    /// The settings window, while it is open.
    settings: Option<SettingsWindow>,
    /// What the left button went down on, while it is down.
    pressed: Option<Press>,
    /// When and where the last double-click in the query was, until a
    /// third click uses it.
    double: Option<(Instant, (i32, i32))>,
    /// Where the user dragged the launcher; `None` where it goes at first.
    position: Option<Position>,
}

/// What the left button went down on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    /// The query: moving selects.
    Text,
    /// A row: letting go over it opens it.
    Row(usize),
    /// The glass: the launcher follows the pointer, which holds it at
    /// this point, in client pixels.
    Drag(POINT),
}

impl App {
    /// Makes `hotkey` show and hide the launcher in place of the one it
    /// has, if Windows lets it; when Windows refuses, the one it has stays.
    fn bind(&mut self, hotkey: Hotkey) -> Result<()> {
        if self.hotkey.map(|(_, held)| held) == Some(hotkey) {
            return Ok(());
        }
        let id = match self.hotkey {
            Some((HOTKEY_TOGGLE, _)) => HOTKEY_NEXT,
            _ => HOTKEY_TOGGLE,
        };
        self.services.hotkeys.register(self.hwnd, id, hotkey)?;
        if let Some((old, _)) = self.hotkey.replace((id, hotkey)) {
            self.services.hotkeys.unregister(self.hwnd, old);
        }
        Ok(())
    }

    /// The hotkey that shows the launcher now, if there is one.
    fn hotkey(&self) -> Option<Hotkey> {
        self.hotkey.map(|(_, hotkey)| hotkey)
    }

    /// What hyalo says things in: the config's language, or Windows'.
    fn language(&self) -> Language {
        self.config
            .language
            .unwrap_or(self.services.windows_language)
    }

    /// Shows hyalo's icon in the tray with what it should say.
    fn show_tray(&mut self) {
        let said = self.notice().map(|notice| notice.said(self.language()));
        let tip = tray::tip(self.hotkey(), said.as_deref());
        // Without a taskbar yet, TaskbarCreated brings the icon later.
        let _ = self.services.tray.show(self.hwnd, &tip);
    }

    /// Takes in what the config holds: good settings are put in force; a
    /// problem keeps the ones in force and is told.
    fn configure(&mut self, loaded: Loaded) {
        match loaded {
            Ok(config) => {
                self.problem = None;
                self.apply(config);
            }
            Err(problem) => self.problem = Some(problem),
        }
        self.show_tray();
    }

    /// Puts `config` in force: its hotkey, unless Windows refuses it, and
    /// its places.
    fn apply(&mut self, config: Config) {
        let wanted = config.hotkey;
        self.refused = self.bind(wanted).err().map(|error| {
            eprintln!("hyalo: {wanted} is taken by another app: {error}");
            wanted
        });
        let places = self.services.places.clone();
        self.places = places.with(&config.rank_higher, &config.rank_lower);
        self.leave_out(&config.exclude);
        self.find_if_stale();
        self.config = config;
    }

    /// Tells the sources that walk folders what to leave out; what one of
    /// them found is stale if it left out something else.
    fn leave_out(&mut self, paths: &[String]) {
        for (source, found_at) in self.services.sources.iter().zip(&mut self.found_at) {
            if source
                .excluded
                .as_ref()
                .is_some_and(|excluded| excluded.set(paths))
            {
                *found_at = None;
            }
        }
    }

    /// What to tell the user, if anything: a problem in the config comes
    /// before a refused hotkey.
    fn notice(&self) -> Option<Notice> {
        let problem = self.problem.clone().map(Notice::Config);
        let kept = self.hotkey();
        problem.or_else(|| self.refused.map(|wanted| Notice::Hotkey { wanted, kept }))
    }

    /// Watches the config, if there is one, from what it held at start,
    /// for as long as the window is there.
    fn watch_config(&self, seen: Option<Vec<u8>>) {
        let Some(path) = self.services.config.clone() else {
            return;
        };
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        // SAFETY: IsWindow takes any handle, even one gone since.
        let listening = move || unsafe { IsWindow(Some(HWND(window as *mut _))) }.as_bool();
        let report = move |bytes| hand_over(&sender, window, Found::Config(bytes));
        settings::watch(path, seen, listening, report);
    }

    /// What the tray's menu holds now.
    fn menu(&self) -> Vec<tray::Line> {
        tray::menu(self.hotkey(), self.starts(), self.language())
    }

    /// Starts with Windows if it does not, and stops if it does.
    fn toggle_start(&mut self) {
        if let Some(autostart) = &self.services.autostart
            && let Err(error) = autostart.set(!autostart.starts())
        {
            eprintln!("hyalo: cannot change starting with Windows: {error}");
        }
    }

    /// Opens the config in the editor Windows has for it, or Notepad.
    fn open_config(&mut self) {
        if let Some(path) = &self.services.config {
            let path = path.to_string_lossy().into_owned();
            (self.services.run)(&Action::Edit(path), false);
        }
    }

    /// Before the window goes: the tray icon and the settings window go
    /// too, and the picks are saved.
    fn close(&mut self) {
        self.services.tray.remove(self.hwnd);
        self.close_settings();
        self.save_picks();
    }

    /// The window's rectangle for the rows it lists, where it goes.
    fn rect(&self) -> Rect {
        self.placement
            .rect(self.results.rows().len(), self.position)
    }

    /// Sizes the window for the rows it lists and draws it.
    fn present(&mut self) -> Result<()> {
        let rect = self.rect();
        let (width, height) = (rect.width as u32, rect.height as u32);
        let dpi = self.placement.dpi;
        match &mut self.renderer {
            Some(renderer) => renderer.resize(width, height, dpi)?,
            None => self.renderer = Some(Renderer::new(self.hwnd, width, height, dpi)?),
        }
        self.redraw()?;
        // The picture first, then the window: growing, it never shows
        // glass without the picture for a frame.
        self.put_window()
    }

    /// Puts the window where it goes, at the size of what it lists.
    fn put_window(&self) -> Result<()> {
        let rect = self.rect();
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOACTIVATE,
            )
        }
    }

    /// Moves the launcher with the pointer, at `point` in client pixels,
    /// that holds it at `grab`, as far as it stays within its work area.
    fn drag(&mut self, point: POINT, grab: POINT) -> Result<()> {
        let mut rect = RECT::default();
        // SAFETY: a plain call into a live RECT.
        unsafe { GetWindowRect(self.hwnd, &mut rect)? };
        let corner = (rect.left + point.x - grab.x, rect.top + point.y - grab.y);
        let (work_area, scale) = (self.placement.work_area, layout::scale(self.placement.dpi));
        self.position = Some(layout::position_at(corner, work_area, scale));
        self.put_window()
    }

    /// The launcher goes back where it goes at first, and stays there.
    fn put_back(&mut self) -> Result<()> {
        self.position = None;
        self.keep_position();
        self.put_window()
    }

    /// Keeps where the launcher was dragged to in the position file, if
    /// there is one; with no position, the file goes.
    fn keep_position(&self) {
        let Some(path) = &self.services.position else {
            return;
        };
        let kept = match self.position {
            Some(position) => store::replace(path, position.written().as_bytes()),
            None => std::fs::remove_file(path).or_else(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(error),
            }),
        };
        if let Err(error) = kept {
            eprintln!(
                "hyalo: cannot keep the launcher's place in {}: {error}",
                path.display()
            );
        }
    }

    fn redraw(&mut self) -> Result<()> {
        let scene = scene(
            &self.query,
            &self.results,
            &self.catalog.icons,
            self.language(),
        );
        self.renderer
            .as_mut()
            .map_or(Ok(()), |renderer| renderer.draw(&scene))
    }

    /// Moves the selection with `step` and shows where it went.
    fn move_selection(&mut self, step: fn(&mut Results)) -> Result<()> {
        step(&mut self.results);
        self.redraw()
    }

    /// The row under `point`, in client pixels.
    fn row_under(&self, point: POINT) -> Option<usize> {
        let y = point.y as f32 / layout::scale(self.placement.dpi);
        layout::row_at(y, self.results.rows().len())
    }

    /// Selects the row the pointer moved onto. A move to where the
    /// pointer already was is not the user's: Windows sends one when the
    /// window changes under a still pointer, as when the list grows while
    /// typing, and that must not take the selection from the keyboard.
    fn hover(&mut self, point: POINT) -> Result<()> {
        let moved = self.pointer.replace(point) != Some(point);
        let row = self.row_under(point).filter(|_| moved);
        match row.is_some_and(|row| self.results.select(row)) {
            true => self.redraw(),
            false => Ok(()),
        }
    }

    /// What the launcher has under `point`, in client pixels.
    fn part_at(&self, point: POINT) -> Part {
        let scale = layout::scale(self.placement.dpi);
        let rows = self.results.rows().len();
        let width = self.rect().width as f32 / scale;
        let (x, y) = (point.x as f32 / scale, point.y as f32 / scale);
        layout::part_at(x, y, width, rows)
    }

    /// The character a click at `point` puts the caret before; `None`
    /// without a picture to tell.
    fn char_at(&self, point: POINT) -> Option<usize> {
        let x = point.x as f32 / layout::scale(self.placement.dpi);
        let text = self.query.text();
        let unit = self.renderer.as_ref()?.unit_at(text, x).ok()?;
        Some(field::char_index(text, unit))
    }

    /// Puts the caret at `point`, or with `extend` selects up to there.
    fn place(&mut self, point: POINT, extend: bool) -> Edited {
        self.char_at(point)
            .map_or(Edited::Nothing, |at| self.query.place(at, extend))
    }

    /// The left button went down at `point`, with Shift held when
    /// `shift`: in the query it places the caret, or selects to there,
    /// or after a double-click selects everything; on a row, the row is
    /// opened when the button comes up over it.
    fn press(&mut self, point: POINT, shift: bool) -> Result<()> {
        let (pressed, edited) = match self.part_at(point) {
            Part::Text if self.third(point) => (None, self.query.apply(Edit::SelectAll)),
            Part::Text => (Some(Press::Text), self.place(point, shift)),
            Part::Row(row) => (Some(Press::Row(row)), Edited::Nothing),
            Part::Glass => (Some(Press::Drag(point)), Edited::Nothing),
        };
        self.pressed = pressed;
        self.refresh(edited)
    }

    /// Whether a click at `point` is the third of a triple-click.
    fn third(&mut self, point: POINT) -> bool {
        let double = self.double.take();
        field::third_click(
            double,
            Instant::now(),
            (point.x, point.y),
            double_click_time(),
            double_click_room(),
        )
    }

    /// A double-click at `point`: in the query it selects the word there;
    /// on the glass it puts the launcher back where it goes at first; on a
    /// row it is a press like any other, `shift` held or not.
    fn double_click(&mut self, point: POINT, shift: bool) -> Result<()> {
        match self.part_at(point) {
            Part::Text => self.select_word(point),
            Part::Glass => self.put_back(),
            Part::Row(_) => self.press(point, shift),
        }
    }

    /// Selects the word at `point` in the query, and notes the
    /// double-click for a third click.
    fn select_word(&mut self, point: POINT) -> Result<()> {
        self.double = Some((Instant::now(), (point.x, point.y)));
        self.pressed = None;
        let edited = self
            .char_at(point)
            .map_or(Edited::Nothing, |at| self.query.select_word(at));
        self.refresh(edited)
    }

    /// The pointer moved to `point`: with the button down in the query,
    /// the selection follows it; else the row under it is selected.
    fn pointer_moved(&mut self, point: POINT) -> Result<()> {
        match self.pressed {
            Some(Press::Text) => {
                let edited = self.place(point, true);
                self.refresh(edited)
            }
            Some(Press::Drag(grab)) => self.drag(point, grab),
            _ => self.hover(point),
        }
    }

    /// The left button came up at `point`: over the row it went down on,
    /// that row opens, as Enter would; after a drag, where the launcher
    /// went is kept. Whether a row opened.
    fn release(&mut self, point: POINT) -> bool {
        match self.pressed.take() {
            Some(Press::Row(row)) => self.open(row, point),
            Some(Press::Drag(_)) => {
                self.keep_position();
                false
            }
            _ => false,
        }
    }

    /// Opens row `row` if the button came up over it at `point`; whether
    /// it did.
    fn open(&mut self, row: usize, point: POINT) -> bool {
        if self.row_under(point) != Some(row) {
            return false;
        }
        self.results.select(row);
        self.act(Command::Open { as_admin: false });
        true
    }

    /// Carries out what `command` does with the selected row and notes
    /// the pick; whether it did anything.
    fn act(&mut self, command: Command) -> bool {
        let Some(item) = self.results.picked().map(|row| row.item.clone()) else {
            return false;
        };
        let Some((action, as_admin)) = keys::action(command, &item) else {
            return false;
        };
        match action {
            // Later, from the message loop: opening a window here, with
            // the app state borrowed, would lose its first messages.
            Action::Settings => post(self.hwnd, WM_SETTINGS),
            action => (self.services.run)(&action, as_admin),
        }
        self.note(item);
        true
    }

    /// Carries out a key that edits the query or copies from it, and
    /// shows what came of it; whether the launcher hides: Ctrl+C with
    /// nothing selected copied the selected row's path.
    fn edit(&mut self, key: field::Key) -> Result<bool> {
        let edited = match key {
            field::Key::Copy => return Ok(self.copy()),
            field::Key::Edit(edit) => self.query.apply(edit),
            field::Key::Cut => self.cut(),
            field::Key::Paste => {
                let text = clipboard::text().unwrap_or_default();
                text_edited(self.query.paste(&text))
            }
        };
        self.refresh(edited).map(|()| false)
    }

    /// Copies the text selected in the bar; with none selected, the
    /// selected row's path, as Ctrl+C always did. Whether it was the path.
    fn copy(&mut self) -> bool {
        match self.query.selected() {
            "" => self.act(Command::Copy),
            selected => {
                (self.services.run)(&Action::CopyText(selected.to_string()), false);
                false
            }
        }
    }

    /// Takes the selection out of the query onto the clipboard.
    fn cut(&mut self) -> Edited {
        let Some(text) = self.query.cut() else {
            return Edited::Nothing;
        };
        (self.services.run)(&Action::CopyText(text), false);
        Edited::Text
    }

    /// Shows what an edit changed: a new query searches again; a caret or
    /// a selection moved is drawn.
    fn refresh(&mut self, edited: Edited) -> Result<()> {
        match edited {
            Edited::Nothing => Ok(()),
            Edited::Caret => self.redraw(),
            Edited::Text => {
                self.search();
                self.present()
            }
        }
    }

    /// Notes a pick, on disk too when there is a picks file; a file or
    /// folder is kept to rank with the apps from now on. A calculator
    /// answer is not noted: it already comes first, and each sum would
    /// leave a pick behind.
    fn note(&mut self, item: Item) {
        if matches!(item.kind, Kind::Calculator | Kind::Notice) {
            return;
        }
        self.frecency.record(&item.id, self.query.text(), now());
        self.save_picks();
        if item.path().is_some() {
            self.catalog.pick(item);
        }
    }

    /// Writes the picks to the picks file, if there is one.
    fn save_picks(&self) {
        if let Some(path) = &self.services.picks
            && let Err(error) = store::save(path, &self.frecency)
        {
            eprintln!("hyalo: cannot save picks to {}: {error}", path.display());
        }
    }

    /// Lists what the query finds, best first, the first row selected.
    fn search(&mut self) {
        self.results = self.listed();
        self.fetch_icons();
    }

    /// The rows for the query: its answer, if it has one, then the best
    /// items and files.
    fn listed(&self) -> Results {
        let query = self.query.text();
        let items = self.catalog.items.iter().chain(&self.catalog.picked);
        let (frecency, places) = (&self.frecency, &self.places);
        let found = Results::search(query, items, self.file_hits(), frecency, places, now());
        let answer = match query.is_empty() {
            true => self.notice_row(),
            false => results::answer(query, self.catalog.rates.as_ref(), self.language()),
        };
        found.answered(answer)
    }

    /// The notice's row, when there is a config the settings window can
    /// change.
    fn notice_row(&self) -> Option<Item> {
        self.services.config.as_ref()?;
        Some(self.notice()?.item(self.language()))
    }

    /// The files and folders the index finds for the query: enough to
    /// fill the list even when every picked file is among them.
    fn file_hits(&self) -> Vec<Ranked> {
        let query = self.query.text();
        if query.is_empty() {
            return Vec::new();
        }
        let enough = MAX_ROWS + self.catalog.picked.len();
        self.services.files.search(query, &self.places, enough)
    }

    /// Asks for the icons of the files and folders listed that have none
    /// yet, once each, on a thread of its own.
    fn fetch_icons(&mut self) {
        let rows = self.results.rows().iter();
        let files = rows.filter(|row| row.item.path().is_some());
        let wanted: Vec<ItemId> = files
            .map(|row| row.item.id.clone())
            .filter(|id| !self.catalog.icons.contains_key(id) && self.asked.insert(id.clone()))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let size = layout::icon_pixels(self.placement.dpi);
        let (icons, sender) = (self.services.icons.clone(), self.found.0.clone());
        let window = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            icons(&wanted, size, &|found| hand_over(&sender, window, found))
        });
    }

    /// Starts each source's provider on a thread of its own, unless what
    /// it last found is still fresh.
    fn find_if_stale(&mut self) {
        let now = Instant::now();
        for (source, found_at) in self.services.sources.iter().zip(&mut self.found_at) {
            if !catalog::stale(*found_at, now, source.trusted) {
                continue;
            }
            *found_at = Some(now);
            let request = Request {
                icon_size: layout::icon_pixels(self.placement.dpi),
                known: self.catalog.icons.keys().cloned().collect(),
            };
            let (provider, sender) = (source.provider.clone(), self.found.0.clone());
            let window = self.hwnd.0 as isize;
            let find = move || provider(&request, &|found| hand_over(&sender, window, found));
            std::thread::spawn(find);
        }
    }

    /// Looks, on a thread of its own, for the picked files and folders
    /// that are still there, so they rank with the apps.
    fn find_picked(&self) {
        let ids: Vec<ItemId> = self.frecency.ids().cloned().collect();
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || hand_over(&sender, window, Found::Picked(files::picked(&ids))));
    }

    /// Takes in what the providers handed over, searches again (keeping
    /// the selected item selected), and shows the result if the window is
    /// up. After an icon the search finds the same rows; it costs less
    /// than the redraw that follows.
    fn take_found(&mut self) -> Result<()> {
        let found: Vec<Found> = self.found.1.try_iter().collect();
        for found in found {
            match found {
                Found::Config(bytes) => {
                    self.configure(settings::read(bytes.clone()));
                    self.show_written(bytes);
                }
                found => self.catalog.take(found),
            }
        }
        self.results = self.results.kept(self.listed());
        self.fetch_icons();
        match unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            true => self.present(),
            false => Ok(()),
        }
    }
}

/// Sends a piece to the window and wakes it up. The window travels as a
/// number: a window handle may not cross threads.
fn hand_over(sender: &Sender<Found>, window: isize, found: Found) {
    let _ = sender.send(found);
    let hwnd = HWND(window as *mut _);
    // WM_FOUND carries nothing: the piece went through the channel.
    // Posting to a window that is already gone fails, and nothing waits.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_FOUND, WPARAM::default(), LPARAM::default()) };
}

/// What the window shows: the query and a row per result.
fn scene<'a>(
    query: &'a Field,
    results: &'a Results,
    icons: &'a HashMap<ItemId, Icon>,
    language: Language,
) -> Scene<'a> {
    let row = |listed: &'a Listed| Row {
        id: &listed.item.id,
        title: &listed.item.title,
        subtitle: results::subtitle(&listed.item, language),
        highlights: utf16_ranges(&listed.item.title, &listed.matched),
        icon: icons.get(&listed.item.id),
    };
    Scene {
        query: query.text(),
        placeholder: language.say(Text::Placeholder),
        caret: query.caret_utf16(),
        selection: query.selection_utf16(),
        rows: results.rows().iter().map(row).collect(),
        selected: results.selected(),
    }
}

/// The time frecency counts in.
fn now() -> Timestamp {
    let since_1970 = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    Timestamp(since_1970.unwrap_or_default().as_secs())
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Opens the launcher, shown, in the tray, or with its settings window
/// open, and runs until the window closes.
pub fn run(start: Start) -> Result<()> {
    begin(Services::real(), start)?;
    message_loop();
    Ok(())
}

/// Opens the launcher with `services`, in the tray, and asks it, as a
/// second start would, to show or to open its settings when `start`
/// says so: through the message loop, where showing a window has every
/// message it needs.
fn begin(services: Services, start: Start) -> Result<HWND> {
    let hwnd = open(services, false)?;
    ask(hwnd, start);
    Ok(hwnd)
}

/// Creates the launcher window on this thread, with the picks saved
/// before, puts the config in force (the hotkey, the folders), puts the
/// icon in the tray, sets the providers looking and, if `shown`, shows
/// the window.
fn open(services: Services, shown: bool) -> Result<HWND> {
    let hwnd = create_window()?;
    let placement = placement_under_cursor()?;
    let frecency = services
        .picks
        .as_deref()
        .map(store::load)
        .unwrap_or_default();
    let sources = services.sources.len();
    let places = services.places.clone();
    let language = services.windows_language;
    let seen = services
        .config
        .as_deref()
        .and_then(|path| settings::contents(path, language));
    let position = services.position.as_deref().and_then(read_position);
    APP.with_borrow_mut(|app| {
        let mut opened = App {
            hwnd,
            query: Field::default(),
            renderer: None,
            placement,
            catalog: Catalog::default(),
            results: Results::default(),
            frecency,
            services,
            found_at: vec![None; sources],
            asked: HashSet::new(),
            found: channel(),
            pointer: None,
            hotkey: None,
            deactivated_at: None,
            config: Config::default(),
            problem: None,
            refused: None,
            places,
            settings: None,
            pressed: None,
            double: None,
            position,
        };
        opened.find_picked();
        let loaded = seen.clone().map_or(Ok(Config::default()), settings::read);
        // A file broken at start keeps the last good settings: the
        // defaults.
        if loaded.is_err() {
            opened.apply(Config::default());
        }
        opened.configure(loaded);
        opened.watch_config(seen);
        *app = Some(opened);
    });
    style_window(hwnd)?;
    if shown {
        show(hwnd)?;
    }
    Ok(hwnd)
}

const WINDOW_CLASS: PCWSTR = w!("hyalo.launcher");

/// Registers the window class, so every thread that opens a launcher can
/// call this. Its result is not checked: the second registration fails
/// because the class exists, and if registering failed for any other
/// reason, CreateWindowExW reports the missing class.
fn register_class(instance: HINSTANCE) -> Result<()> {
    let window_class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        // Double-clicks select words in the query.
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&window_class) };
    Ok(())
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
            // show() places and sizes the window.
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
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
    // No DwmExtendFrameIntoClientArea: the window has no redirection
    // bitmap, so nothing covers the backdrop in the client area anyway.
    set_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)
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
        // No WM_PAINT: the picture lives in the DirectComposition swap chain
        // and survives without one; DefWindowProc validates the window.
        WM_DESTROY => {
            with_app(App::close);
            unsafe { PostQuitMessage(0) };
            handled(Ok(()))
        }
        _ => match on_message(hwnd, message, wparam, lparam) {
            Some(result) => handled(result),
            None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        },
    }
}

/// What the launcher does with a message; `None` for messages it leaves
/// to Windows.
fn on_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<Result<()>> {
    match message {
        WM_HOTKEY => Some(on_hotkey(hwnd)),
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            on_focus_lost(hwnd);
            Some(Ok(()))
        }
        WM_KEYDOWN | WM_CHAR | WM_SYSCHAR | WM_SYSKEYUP => Some(on_keyboard(hwnd, message, wparam)),
        WM_SYSKEYDOWN => on_alt_key(hwnd, VIRTUAL_KEY(wparam.0 as u16)),
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONUP => {
            Some(on_mouse(hwnd, message, wparam, client_point(lparam)))
        }
        // Always over the client area: WM_NCHITTEST above says so.
        WM_SETCURSOR => Some(on_set_cursor(hwnd)),
        _ => on_app_message(hwnd, message, wparam, lparam),
    }
}

/// What the launcher does with the messages its own threads, other
/// processes and the shell send it; `None` for the rest.
fn on_app_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<Result<()>> {
    match message {
        WM_FOUND => Some(with_app(App::take_found).unwrap_or(Ok(()))),
        WM_SHOW => Some(show(hwnd)),
        WM_SETTINGS => Some(open_settings()),
        tray::WM_TRAY => Some(on_tray(hwnd, wparam, lparam)),
        _ if message == tray::taskbar_created() => {
            with_app(App::show_tray);
            Some(Ok(()))
        }
        _ => None,
    }
}

/// How long after the launcher lost the focus a click on the tray icon
/// counts as the click that took it.
const FOCUS_CLICK: Duration = Duration::from_millis(500);

/// Losing the focus hides the launcher, and notes when, if it was shown.
fn on_focus_lost(hwnd: HWND) {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        with_app(|app| app.deactivated_at = Some(Instant::now()));
    }
    hide(hwnd);
}

/// The icon chosen with the keyboard: Enter or Space on it.
const KEY_SELECT: u32 = NIN_SELECT | NINF_KEY;

/// What the tray icon tells: a click, or Enter on the icon, shows or
/// hides the launcher; a right click, or the menu key, opens the menu.
fn on_tray(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Result<()> {
    match (lparam.0 & 0xFFFF) as u32 {
        NIN_SELECT | KEY_SELECT => on_tray_click(hwnd),
        // Where the menu opens, in screen pixels, comes as a mouse
        // message's point does, in wParam.
        WM_CONTEXTMENU => on_tray_menu(hwnd, client_point(LPARAM(wparam.0 as isize))),
        _ => Ok(()),
    }
}

/// A click on the tray icon hides a shown launcher and shows a hidden
/// one. While the launcher is up, the click takes the focus from it
/// first, which hides it: that click must not show it again.
fn on_tray_click(hwnd: HWND) -> Result<()> {
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let lost = with_app(|app| app.deactivated_at.take()).flatten();
    match visible || lost.is_some_and(|at| at.elapsed() < FOCUS_CLICK) {
        true => {
            hide(hwnd);
            Ok(())
        }
        false => show(hwnd),
    }
}

/// Opens the tray's menu at `at` and carries out what is picked. The
/// menu runs a message loop of its own, so the app state is not held
/// while it is open.
fn on_tray_menu(hwnd: HWND, at: POINT) -> Result<()> {
    let Some((tray, lines)) = with_app(|app| (Rc::clone(&app.services.tray), app.menu())) else {
        return Ok(());
    };
    match tray.menu(hwnd, &lines, at) {
        Some(command) => on_command(hwnd, command),
        None => Ok(()),
    }
}

/// Carries out a command from the tray's menu. Exit destroys the window,
/// which saves the picks, takes the icon away and ends the message loop.
fn on_command(hwnd: HWND, command: tray::Command) -> Result<()> {
    match command {
        tray::Command::Show => show(hwnd),
        tray::Command::Settings => open_settings(),
        tray::Command::StartWithWindows => {
            with_app(App::toggle_start);
            Ok(())
        }
        tray::Command::Exit => unsafe { DestroyWindow(hwnd) },
    }
}

/// From a second start: asks the launcher that runs already to show, or
/// to open its settings.
pub fn ask_running(start: Start) {
    // SAFETY: a plain call.
    if let Ok(hwnd) = unsafe { FindWindowW(WINDOW_CLASS, PCWSTR::null()) } {
        ask(hwnd, start);
    }
}

/// Asks the launcher `hwnd`, perhaps in another process, to show, or to
/// open its settings. The start was the user's, so it may hand the
/// foreground on; the launcher, in the background, could not take it on
/// its own.
fn ask(hwnd: HWND, start: Start) {
    let message = match start {
        Start::Shown => WM_SHOW,
        Start::Settings => WM_SETTINGS,
        // Started with Windows: the tray icon is all there is to see.
        Start::Hidden => return,
    };
    // SAFETY: plain calls; the window may be gone by the time a call
    // reaches it, and then the call fails and nothing waits.
    unsafe {
        let mut process = u32::default();
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        let _ = AllowSetForegroundWindow(process);
    }
    post(hwnd, message);
}

/// Posts `message`, which carries nothing, to `hwnd`.
fn post(hwnd: HWND, message: u32) {
    // SAFETY: a plain call; posting to a window that is gone fails, and
    // nothing waits.
    let _ = unsafe { PostMessageW(Some(hwnd), message, WPARAM::default(), LPARAM::default()) };
}

/// The point a mouse message carries: x in the low word of `lparam`, y in
/// the high one, each signed, in client pixels.
fn client_point(lparam: LPARAM) -> POINT {
    let word = |shift: u32| i32::from((lparam.0 >> shift) as u16 as i16);
    POINT {
        x: word(0),
        y: word(16),
    }
}

/// The Shift key, held while a mouse message was sent (`MK_SHIFT`).
const MOUSE_SHIFT: usize = 0x0004;

/// What the mouse does. While the button is down in the query, the
/// window has the mouse, so selecting goes on outside it; a click that
/// opens a row hides the launcher.
fn on_mouse(hwnd: HWND, message: u32, wparam: WPARAM, point: POINT) -> Result<()> {
    let shift = wparam.0 & MOUSE_SHIFT != 0;
    match message {
        WM_LBUTTONDOWN => {
            let pressed = with_app(|app| app.press(point, shift).map(|()| app.pressed));
            capture_while(hwnd, pressed)
        }
        WM_LBUTTONDBLCLK => with_app(|app| app.double_click(point, shift)).unwrap_or(Ok(())),
        WM_LBUTTONUP => {
            // SAFETY: a plain call; without the mouse it does nothing.
            let _ = unsafe { ReleaseCapture() };
            if with_app(|app| app.release(point)) == Some(true) {
                hide(hwnd);
            }
            Ok(())
        }
        _ => with_app(|app| app.pointer_moved(point)).unwrap_or(Ok(())),
    }
}

/// The pointer's look over the launcher, where the message that moved
/// it put it.
fn on_set_cursor(hwnd: HWND) -> Result<()> {
    // SAFETY: a plain call.
    let at = unsafe { GetMessagePos() };
    set_cursor_over(hwnd, client_point(LPARAM(at as isize)))
}

/// Shows the pointer over the point `at` of the screen as an I-beam over
/// the query, an arrow elsewhere.
fn set_cursor_over(hwnd: HWND, at: POINT) -> Result<()> {
    let mut point = at;
    // SAFETY: a plain call into a live POINT.
    let _ = unsafe { ScreenToClient(hwnd, &mut point) };
    let shape = match with_app(|app| app.part_at(point)) {
        Some(Part::Text) => IDC_IBEAM,
        _ => IDC_ARROW,
    };
    // SAFETY: plain calls; a shared system cursor is never destroyed.
    unsafe { SetCursor(Some(LoadCursorW(None, shape)?)) };
    Ok(())
}

/// Takes the mouse for `hwnd` while the button is down on something, so
/// selecting and dragging go on outside it.
fn capture_while(hwnd: HWND, pressed: Option<Result<Option<Press>>>) -> Result<()> {
    match pressed {
        Some(Ok(Some(_))) => {
            // SAFETY: a plain call on a window of this thread.
            unsafe { SetCapture(hwnd) };
            Ok(())
        }
        Some(Err(error)) => Err(error),
        _ => Ok(()),
    }
}

/// How soon a click must follow a double-click to make a triple-click:
/// as soon as the second click of a double-click.
fn double_click_time() -> Duration {
    // SAFETY: a plain call.
    Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }))
}

/// The box a double-click's clicks fall in, in pixels.
fn double_click_room() -> (i32, i32) {
    // SAFETY: plain calls.
    unsafe {
        (
            GetSystemMetrics(SM_CXDOUBLECLK),
            GetSystemMetrics(SM_CYDOUBLECLK),
        )
    }
}

/// The message was handled: answer 0, as Windows expects, and log a
/// failure instead of unwinding into Win32.
fn handled(result: Result<()>) -> LRESULT {
    if let Err(error) = result {
        eprintln!("hyalo: {error}");
    }
    LRESULT(0)
}

/// The hotkey: taken by the settings window's recorder while it listens,
/// else it shows or hides the launcher.
fn on_hotkey(hwnd: HWND) -> Result<()> {
    let taken = with_app(|app| {
        let window = app.settings.as_mut();
        window.is_some_and(SettingsWindow::hotkey_pressed)
    });
    match taken {
        Some(true) => Ok(()),
        _ => toggle(hwnd),
    }
}

fn toggle(hwnd: HWND) -> Result<()> {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        hide(hwnd);
        return Ok(());
    }
    show(hwnd)
}

fn show(hwnd: HWND) -> Result<()> {
    let placement = placement_under_cursor()?;
    with_app(|app| -> Result<()> {
        app.placement = placement;
        app.find_if_stale();
        app.search();
        app.present()?;
        app.pointer = pointer_over(hwnd);
        Ok(())
    })
    .unwrap_or(Ok(()))?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

/// Hides the window and forgets the query and its results.
fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    with_app(|app| {
        app.query.clear();
        app.results = Results::default();
    });
}

/// Moves the selection, acts on the selected row, or edits the query.
/// Enter hides the launcher even with nothing to open; Ctrl+Enter hides
/// it only when the row has a folder to show.
fn on_key(hwnd: HWND, key: VIRTUAL_KEY) -> Result<()> {
    let held = held();
    let moved = match keys::command(key, held) {
        Some(Command::Up) => with_app(|app| app.move_selection(Results::up)),
        Some(Command::Down) => with_app(|app| app.move_selection(Results::down)),
        Some(Command::Hide) => {
            hide(hwnd);
            None
        }
        Some(command @ Command::Open { .. }) => {
            with_app(|app| app.act(command));
            hide(hwnd);
            None
        }
        Some(command) => {
            if with_app(|app| app.act(command)) == Some(true) {
                hide(hwnd);
            }
            None
        }
        None => return on_field_key(hwnd, field::key(key, held)),
    };
    moved.unwrap_or(Ok(()))
}

/// A key that edits the query or copies from it; Ctrl+C that copied a
/// row's path hides the launcher.
fn on_field_key(hwnd: HWND, key: Option<field::Key>) -> Result<()> {
    let Some(key) = key else {
        return Ok(());
    };
    match with_app(|app| app.edit(key)) {
        Some(Ok(true)) => {
            hide(hwnd);
            Ok(())
        }
        Some(Err(error)) => Err(error),
        _ => Ok(()),
    }
}

/// A key pressed with Alt: Alt+F4 hides the launcher; `None` leaves the
/// others to Windows.
fn on_alt_key(hwnd: HWND, key: VIRTUAL_KEY) -> Option<Result<()>> {
    keys::alt_command(key)?;
    hide(hwnd);
    Some(Ok(()))
}

/// Whether `key` is down, as of the key message being handled.
fn down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: a plain call.
    unsafe { GetKeyState(i32::from(key.0)) < 0 }
}

/// What the keys do. The launcher has no menu, so an Alt key's letter,
/// or Alt let go, is kept from Windows, which would only look for one
/// and beep.
fn on_keyboard(hwnd: HWND, message: u32, wparam: WPARAM) -> Result<()> {
    match message {
        WM_KEYDOWN => on_key(hwnd, VIRTUAL_KEY(wparam.0 as u16)),
        WM_CHAR => on_char(wparam.0 as u16),
        _ => Ok(()),
    }
}

/// Which modifiers are down, as of the key message being handled.
fn held() -> Held {
    Held {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
    }
}

/// Types one UTF-16 unit; a change to the query searches again.
fn on_char(unit: u16) -> Result<()> {
    with_app(|app| {
        let typed = app.query.type_unit(unit);
        app.refresh(text_edited(typed))
    })
    .unwrap_or(Ok(()))
}

/// A change to the text, or none.
fn text_edited(changed: bool) -> Edited {
    match changed {
        true => Edited::Text,
        false => Edited::Nothing,
    }
}

/// The position the file at `path` keeps, if it keeps one.
fn read_position(path: &Path) -> Option<Position> {
    Position::parse(&std::fs::read_to_string(path).ok()?)
}

/// Where the launcher goes: the monitor under the mouse cursor.
///
/// Only a thread on the desktop that receives input may read the cursor
/// (not, say, while the UAC secure desktop is up). Without it, use the
/// primary monitor: it always contains (0, 0).
fn placement_under_cursor() -> Result<Placement> {
    let cursor = cursor_position().unwrap_or_default();
    placement_on(unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) })
}

/// Where the mouse pointer is, in `hwnd`'s client pixels; `None` where it
/// cannot be read (see `placement_under_cursor`).
fn pointer_over(hwnd: HWND) -> Option<POINT> {
    let mut point = cursor_position().ok()?;
    unsafe { ScreenToClient(hwnd, &mut point) }
        .as_bool()
        .then_some(point)
}

fn cursor_position() -> Result<POINT> {
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor)? };
    Ok(cursor)
}

/// The work area and DPI of `monitor`.
fn placement_on(monitor: HMONITOR) -> Result<Placement> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut dpi_y) = (96, 96);
    unsafe {
        GetMonitorInfoW(monitor, &mut info).ok()?;
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y)?;
    }
    Ok(Placement {
        work_area: to_rect(info.rcWork),
        dpi,
    })
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
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use hyalo_core::item::{Action, Item, Kind};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
    use windows::Win32::Graphics::Gdi::MonitorFromWindow;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SetKeyboardState, VK_A, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
        VK_RETURN, VK_RIGHT, VK_UP, VK_V, VK_X,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetCursor, GetWindowRect, IsWindow, PM_QS_POSTMESSAGE, PM_REMOVE,
        PeekMessageW, SendMessageW, WA_ACTIVE, WM_CLOSE, WM_NULL, WM_QUIT,
    };

    use super::*;
    use crate::autostart;
    use crate::render::Frame;
    use crate::scratch::Scratch;
    use crate::test_desktop::{in_own_window_station, on_hidden_desktop};
    use std::path::Path;

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: "App".to_string(),
            kind: Kind::App,
            action: Action::Open(format!("shell:AppsFolder\\{title}")),
        }
    }

    /// An opaque red square, `side` pixels wide.
    fn red(side: u32) -> Icon {
        let pixels = (side * side) as usize;
        Icon {
            width: side,
            height: side,
            bgra: [0, 0, 255, 255].repeat(pixels),
        }
    }

    /// A provider that lists apps with these titles, each with a red icon,
    /// and counts how often it runs.
    pub(super) fn listing(titles: &[&str]) -> (Provider, Arc<AtomicUsize>) {
        let items: Vec<Item> = titles.iter().map(|title| app(title)).collect();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let provider: Provider = Arc::new(move |request: &Request, report: &dyn Fn(Found)| {
            counted.fetch_add(1, Ordering::SeqCst);
            report(Found::Items(items.clone()));
            for item in &items {
                report(Found::Icon(item.id.clone(), red(request.icon_size)));
            }
        });
        (provider, runs)
    }

    /// Folders as a test picks them: what the next pick gives, and the
    /// titles of the pickers opened.
    #[derive(Default)]
    pub(super) struct Chooser {
        pub(super) next: RefCell<Option<String>>,
        pub(super) asked: RefCell<Vec<String>>,
    }

    impl Picker for Chooser {
        fn pick(&self, _: HWND, title: &str) -> Option<String> {
            self.asked.borrow_mut().push(title.to_string());
            self.next.borrow_mut().take()
        }
    }

    /// What the launcher was asked to run, and whether as administrator.
    pub(super) type Ran = Rc<RefCell<Vec<(Action, bool)>>>;

    /// Hotkeys as a test keeps them: what the launcher holds, by id, and
    /// the keys other apps hold.
    #[derive(Default)]
    pub(super) struct Keyboard {
        pub(super) held: RefCell<Vec<(i32, Hotkey)>>,
        pub(super) taken: RefCell<Vec<Hotkey>>,
    }

    impl Hotkeys for Rc<Keyboard> {
        fn register(&self, _: HWND, id: i32, hotkey: Hotkey) -> Result<()> {
            let held = self.held.borrow().iter().any(|&(_, held)| held == hotkey);
            if held || self.taken.borrow().contains(&hotkey) {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x80070581_u32 as i32,
                )));
            }
            self.held.borrow_mut().push((id, hotkey));
            Ok(())
        }

        fn unregister(&self, _: HWND, id: i32) {
            self.held.borrow_mut().retain(|&(held, _)| held != id);
        }
    }

    /// The notification area as a test keeps it: the tips shown, how
    /// often the icon was taken away, the menus opened and where, and
    /// what the user will pick from the next menu.
    #[derive(Default)]
    pub(super) struct Taskbar {
        pub(super) tips: RefCell<Vec<String>>,
        pub(super) removed: RefCell<usize>,
        pub(super) menus: RefCell<Vec<(Vec<tray::Line>, POINT)>>,
        pub(super) pick: RefCell<Option<tray::Command>>,
    }

    impl Tray for Taskbar {
        fn show(&self, _: HWND, tip: &str) -> bool {
            self.tips.borrow_mut().push(tip.to_string());
            true
        }

        fn remove(&self, _: HWND) {
            *self.removed.borrow_mut() += 1;
        }

        fn menu(&self, _: HWND, lines: &[tray::Line], at: POINT) -> Option<tray::Command> {
            self.menus.borrow_mut().push((lines.to_vec(), at));
            self.pick.borrow_mut().take()
        }
    }

    impl Keyboard {
        /// The hotkeys the launcher holds now.
        pub(super) fn held(&self) -> Vec<String> {
            let held = self.held.borrow();
            held.iter().map(|(_, hotkey)| hotkey.to_string()).collect()
        }
    }

    /// Services for a test: `provider`, no files, red icons for files, a
    /// runner that only notes what it is asked to run, and `picks` as the
    /// picks file.
    pub(super) fn services(provider: Provider, picks: Option<PathBuf>) -> (Services, Ran) {
        let ran = Ran::default();
        let noted = ran.clone();
        let icons: FileIcons = Arc::new(|ids: &[ItemId], size, report: &dyn Fn(Found)| {
            for id in ids {
                report(Found::Icon(id.clone(), red(size)));
            }
        });
        let services = Services {
            sources: vec![Source {
                provider,
                trusted: catalog::REFRESH_AFTER,
                excluded: None,
            }],
            files: Files::default(),
            icons,
            places: Places::default(),
            run: Box::new(move |action: &Action, as_admin| {
                noted.borrow_mut().push((action.clone(), as_admin));
            }),
            picks,
            hotkeys: Box::new(Rc::new(Keyboard::default())),
            tray: Rc::new(Taskbar::default()),
            config: None,
            autostart: None,
            picker: Rc::new(Chooser::default()),
            position: None,
            windows_language: Language::English,
        };
        (services, ran)
    }

    /// The same, finding files by walking what `roots` gives.
    pub(super) fn walking(roots: Roots, picks: Option<PathBuf>) -> (Services, Ran) {
        let (mut services, ran) = services(listing(&[]).0, picks);
        let (files, writer) = Files::new();
        services.files = files;
        let excluded = Excluded::default();
        services.sources.push(Source {
            provider: files::provider(writer, roots, None, excluded.clone()),
            trusted: files::REFRESH_AFTER,
            excluded: Some(excluded),
        });
        (services, ran)
    }

    /// Roots that are just `folder`.
    pub(super) fn just(folder: &Path) -> Roots {
        let folder = folder.to_path_buf();
        Arc::new(move || vec![folder.clone()])
    }

    fn titles(app: &App) -> Vec<String> {
        let rows = app.results.rows().iter();
        rows.map(|row| row.item.title.clone()).collect()
    }

    fn opened(title: &str, as_admin: bool) -> (Action, bool) {
        (app(title).action, as_admin)
    }

    /// Holds these keys down for the messages this thread handles next.
    pub(super) fn hold(keys: &[VIRTUAL_KEY]) {
        let mut state = [0_u8; 256];
        for key in keys {
            state[usize::from(key.0)] = 0x80;
        }
        unsafe { SetKeyboardState(&state) }.expect("set the keyboard state");
    }

    /// A picks file of its own for one test, removed afterwards.
    struct PicksFile(PathBuf);

    impl PicksFile {
        fn new(test: &str) -> Self {
            let name = format!("hyalo-{}-{test}.json", std::process::id());
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for PicksFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// A provider that lists apps titled `first` at once, then apps
    /// titled `then` once the test lets it go.
    fn gated(first: &[&str], then: &[&str]) -> (Provider, Sender<()>) {
        let apps = |titles: &[&str]| titles.iter().map(|title| app(title)).collect::<Vec<_>>();
        let (first, then) = (apps(first), apps(then));
        let (release, gate) = channel::<()>();
        let gate = Mutex::new(gate);
        let provider: Provider = Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
            report(Found::Items(first.clone()));
            let _ = gate.lock().expect("the gate").recv();
            report(Found::Items(then.clone()));
        });
        (provider, release)
    }

    /// The launcher as a test drives it: through the messages Windows
    /// would send. Destroyed while the thread's app state is still alive.
    pub(super) struct Launcher(pub(super) HWND);

    impl Launcher {
        /// A launcher that finds nothing to list.
        pub(super) fn open() -> Self {
            Self::with(listing(&[]).0)
        }

        pub(super) fn with(provider: Provider) -> Self {
            Self::serving(services(provider, None).0)
        }

        /// Starts with no key held or toggled: a fresh thread may inherit
        /// toggle bits from the user's session, and a toggled Ctrl reads
        /// as a key state that is not 0.
        pub(super) fn serving(services: Services) -> Self {
            hold(&[]);
            Self(open(services, true).expect("open the launcher"))
        }

        /// A launcher that lists apps with these titles, once it has them
        /// all, icons too.
        pub(super) fn listing(titles: &[&str]) -> Self {
            Self::listing_with(titles, None).0
        }

        /// A launcher that finds the files and folders in `folder`, and
        /// what it was asked to run.
        pub(super) fn finding(folder: &Path, picks: Option<PathBuf>) -> (Self, Ran) {
            let (services, ran) = walking(just(folder), picks);
            (Self::serving(services), ran)
        }

        /// Types `query` and waits for `rows` rows.
        pub(super) fn search_for(&self, query: &str, rows: usize) {
            self.type_text(query);
            self.pump_until(|app| app.results.rows().len() == rows);
        }

        /// The same, keeping picks in `picks`; also what it was asked to run.
        pub(super) fn listing_with(titles: &[&str], picks: Option<PathBuf>) -> (Self, Ran) {
            let (services, ran) = services(listing(titles).0, picks);
            let launcher = Self::serving(services);
            let all = titles.len();
            launcher.pump_until(|app| app.catalog.icons.len() == all);
            (launcher, ran)
        }

        /// Handles queued messages until `done` holds, as the message loop
        /// would; fails after five seconds.
        pub(super) fn pump_until(&self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !with_app(|app| done(app)).expect("app state") {
                assert!(Instant::now() < deadline, "timed out waiting");
                let mut message = MSG::default();
                while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                    unsafe {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        pub(super) fn placement(&self) -> Placement {
            with_app(|app| app.placement).expect("app state")
        }

        /// Which of `rows` rows is lit, read off the frame: the left
        /// margin, clear of icons and text, is only glass, plus the
        /// highlight where the selected row is.
        pub(super) fn lit_row(&self, rows: usize) -> usize {
            let frame = self.frame();
            let scale = layout::scale(self.placement().dpi);
            let x = (9.0 * scale) as usize;
            let alpha = |row| frame.pixel(x, (middle_of(row) * scale) as usize)[3];
            (0..rows).max_by_key(|&row| alpha(row)).expect("rows")
        }

        /// The pixel at (x, y) in DIPs.
        pub(super) fn pixel_at(&self, x: f32, y: f32) -> [u8; 4] {
            let scale = layout::scale(self.placement().dpi);
            self.frame()
                .pixel((x * scale) as usize, (y * scale) as usize)
        }

        pub(super) fn send(&self, message: u32, wparam: usize) -> LRESULT {
            unsafe { SendMessageW(self.0, message, Some(WPARAM(wparam)), Some(LPARAM(0))) }
        }

        /// A mouse message for the point (x, y) in DIPs.
        pub(super) fn mouse(&self, message: u32, x: f32, y: f32) {
            self.mouse_with(message, x, y, 0);
        }

        /// The same, with the keys `keys` (`MK_*`) held.
        pub(super) fn mouse_with(&self, message: u32, x: f32, y: f32, keys: usize) {
            let scale = layout::scale(self.placement().dpi);
            let (x, y) = ((x * scale) as isize, (y * scale) as isize);
            let point = LPARAM((y & 0xFFFF) << 16 | (x & 0xFFFF));
            unsafe { SendMessageW(self.0, message, Some(WPARAM(keys)), Some(point)) };
        }

        /// The left button down and up at (x, y) in DIPs.
        pub(super) fn click(&self, x: f32, y: f32) {
            self.mouse(WM_LBUTTONDOWN, x, y);
            self.mouse(WM_LBUTTONUP, x, y);
        }

        /// The same in pixels of the frame, at the bar's middle.
        fn click_pixel(&self, x: usize) {
            let scale = layout::scale(self.placement().dpi);
            self.click(x as f32 / scale, layout::BAR_HEIGHT / 2.0);
        }

        /// What is selected in the search bar.
        fn selected(&self) -> String {
            with_app(|app| app.query.selected().to_string()).expect("app state")
        }

        pub(super) fn post(&self, message: u32, wparam: usize) {
            unsafe { PostMessageW(Some(self.0), message, WPARAM(wparam), LPARAM(0)) }
                .expect("post a message");
        }

        pub(super) fn type_text(&self, text: &str) {
            for unit in text.encode_utf16() {
                self.send(WM_CHAR, usize::from(unit));
            }
        }

        pub(super) fn press(&self, key: VIRTUAL_KEY) {
            self.send(WM_KEYDOWN, usize::from(key.0));
        }

        pub(super) fn hotkey(&self) {
            self.send(WM_HOTKEY, HOTKEY_TOGGLE as usize);
        }

        /// What the tray icon tells the window: `event`, with the point
        /// (x, y) in screen pixels where a menu would open.
        pub(super) fn tray(&self, event: u32, (x, y): (i32, i32)) {
            let at = WPARAM(((y as usize) << 16) | (x as usize & 0xFFFF));
            let event = LPARAM((1 << 16) | event as isize);
            unsafe { SendMessageW(self.0, tray::WM_TRAY, Some(at), Some(event)) };
        }

        /// Right-clicks the tray icon, and picks `command` from the menu.
        pub(super) fn pick(&self, taskbar: &Taskbar, command: tray::Command) {
            *taskbar.pick.borrow_mut() = Some(command);
            self.tray(WM_CONTEXTMENU, (0, 0));
        }

        pub(super) fn visible(&self) -> bool {
            unsafe { IsWindowVisible(self.0) }.as_bool()
        }

        pub(super) fn rect(&self) -> Rect {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(self.0, &mut rect) }.expect("window rect");
            to_rect(rect)
        }

        pub(super) fn client_size(&self) -> (i32, i32) {
            let mut rect = RECT::default();
            unsafe { GetClientRect(self.0, &mut rect) }.expect("client rect");
            (rect.right - rect.left, rect.bottom - rect.top)
        }

        /// What the launcher shows now, read back from its swap chain.
        /// No repaint first: the test must see what the code put on screen.
        pub(super) fn frame(&self) -> Frame {
            with_app(|app| app.renderer.as_ref().expect("a renderer").read_frame())
                .expect("app state")
                .expect("read the frame back")
        }

        /// The first column of the caret, in pixels, on the bar's middle
        /// row.
        pub(super) fn caret_x(&self) -> usize {
            let scale = layout::scale(self.placement().dpi);
            let middle = (layout::BAR_HEIGHT / 2.0 * scale) as usize;
            caret_x(&self.frame(), middle).expect("a caret on screen")
        }

        /// What the search bar holds.
        pub(super) fn query(&self) -> String {
            with_app(|app| app.query.text().to_string()).expect("app state")
        }

        /// A window attribute as DWM holds it, read back from DWM.
        pub(super) fn dwm(&self, attribute: DWMWINDOWATTRIBUTE) -> i32 {
            let mut value = 0_i32;
            let size = size_of::<i32>() as u32;
            // SAFETY: `value` is a live i32 and `size` is its size.
            unsafe {
                DwmGetWindowAttribute(self.0, attribute, (&raw mut value).cast(), size)
                    .expect("read a DWM attribute");
            }
            value
        }
    }

    impl Drop for Launcher {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    /// The middle of row `row` from the top, in DIPs.
    fn middle_of(row: usize) -> f32 {
        layout::row_top(row) + layout::ROW_HEIGHT / 2.0
    }

    /// The first column of the blue caret on the row `y`.
    fn caret_x(frame: &Frame, y: usize) -> Option<usize> {
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
            let placement = placement_on(monitor).expect("placement");
            assert_eq!(launcher.rect(), placement.rect(0, None));
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
    fn dwm_draws_dark_glass_with_round_corners() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            assert_eq!(launcher.dwm(DWMWA_USE_IMMERSIVE_DARK_MODE), 1);
            assert_eq!(
                launcher.dwm(DWMWA_SYSTEMBACKDROP_TYPE),
                DWMSBT_TRANSIENTWINDOW.0
            );
            assert_eq!(launcher.dwm(DWMWA_WINDOW_CORNER_PREFERENCE), DWMWCP_ROUND.0);
        });
    }

    #[test]
    fn opening_takes_alt_space_as_the_hotkey() {
        on_hidden_desktop(|| {
            let keyboard = Rc::new(Keyboard::default());
            let (mut services, _) = services(listing(&[]).0, None);
            services.hotkeys = Box::new(keyboard.clone());
            let _launcher = Launcher::serving(services);
            assert_eq!(keyboard.held(), ["Alt+Space"]);
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
    fn a_second_start_asks_it_to_show() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.press(VK_ESCAPE);
            ask(launcher.0, Start::Hidden);
            for asked in [WM_SHOW, WM_SETTINGS] {
                let mut message = MSG::default();
                let queued = unsafe {
                    PeekMessageW(&mut message, Some(launcher.0), asked, asked, PM_REMOVE)
                };
                assert!(!queued.as_bool(), "started hidden, nothing to ask");
            }
            ask(launcher.0, Start::Shown);
            launcher.pump_until(|_| unsafe { IsWindowVisible(launcher.0) }.as_bool());
        });
    }

    /// Handles every message queued for this thread, as the message loop
    /// would, until none is left.
    pub(super) fn drain() {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    fn a_first_start_shows_stays_in_the_tray_or_opens_the_settings_as_asked() {
        let starts = [
            (Start::Shown, true, false),
            (Start::Hidden, false, false),
            (Start::Settings, false, true),
        ];
        for (start, shown, settings) in starts {
            on_hidden_desktop(move || {
                let (_scratch, config) = config_file(&format!("window-begin-{start:?}"), "");
                let (mut services, _) = services(listing(&[]).0, None);
                services.tray = Rc::new(Taskbar::default());
                services.config = Some(config);
                hold(&[]);
                let launcher = Launcher(begin(services, start).expect("begin"));
                assert!(
                    !launcher.visible(),
                    "{start:?}: nothing shown before the loop"
                );
                drain();
                assert_eq!(launcher.visible(), shown, "{start:?}");
                let open = with_app(|app| app.settings.is_some()).expect("app state");
                assert_eq!(open, settings, "{start:?}");
            });
        }
    }

    #[test]
    fn a_second_start_with_settings_asks_it_to_open_them() {
        on_hidden_desktop(|| {
            let (_scratch, config) = config_file("window-ask-settings", "");
            let (launcher, _, _) = in_the_tray(Some(config), None);
            launcher.press(VK_ESCAPE);
            ask(launcher.0, Start::Settings);
            launcher.pump_until(|app| app.settings.is_some());
            assert!(!launcher.visible(), "the settings, not the launcher");
        });
    }

    #[test]
    fn a_second_start_shows_it_and_keeps_it_shown() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.press(VK_ESCAPE);
            launcher.send(WM_SHOW, 0);
            assert!(launcher.visible());
            launcher.send(WM_SHOW, 0);
            assert!(launcher.visible(), "shown, not toggled");
        });
    }

    /// A launcher with a stand-in taskbar, the config at `config` and
    /// picks in `picks`; also the taskbar and what it was asked to run.
    pub(super) fn in_the_tray(
        config: Option<PathBuf>,
        picks: Option<PathBuf>,
    ) -> (Launcher, Rc<Taskbar>, Ran) {
        let taskbar = Rc::new(Taskbar::default());
        let (mut services, ran) = services(listing(&["Notepad"]).0, picks);
        services.tray = taskbar.clone();
        services.config = config;
        let launcher = Launcher::serving(services);
        launcher.pump_until(|app| !app.catalog.items.is_empty());
        (launcher, taskbar, ran)
    }

    #[test]
    fn opening_puts_the_icon_in_the_tray_with_the_hotkey_in_its_tip() {
        on_hidden_desktop(|| {
            let (_launcher, taskbar, _) = in_the_tray(None, None);
            assert_eq!(*taskbar.tips.borrow(), ["hyalo · Alt+Space"]);
        });
    }

    #[test]
    fn a_click_on_the_tray_icon_shows_a_hidden_launcher_and_hides_a_shown_one() {
        on_hidden_desktop(|| {
            let (launcher, _, _) = in_the_tray(None, None);
            launcher.press(VK_ESCAPE);
            launcher.tray(NIN_SELECT, (0, 0));
            assert!(launcher.visible());
            launcher.tray(NIN_SELECT, (0, 0));
            assert!(!launcher.visible());
            launcher.tray(KEY_SELECT, (0, 0));
            assert!(launcher.visible(), "Enter on the icon");
        });
    }

    #[test]
    fn the_click_that_took_the_focus_leaves_it_hidden() {
        on_hidden_desktop(|| {
            let (launcher, _, _) = in_the_tray(None, None);
            launcher.send(WM_ACTIVATE, WA_INACTIVE as usize);
            launcher.tray(NIN_SELECT, (0, 0));
            assert!(!launcher.visible());
            launcher.tray(NIN_SELECT, (0, 0));
            assert!(launcher.visible(), "the next click shows it");
        });
    }

    #[test]
    fn a_right_click_opens_the_menu_where_the_tray_says() {
        on_hidden_desktop(|| {
            let (launcher, taskbar, _) = in_the_tray(None, None);
            launcher.tray(WM_CONTEXTMENU, (1500, 1040));
            let menus = taskbar.menus.borrow();
            let (lines, at) = &menus[0];
            assert_eq!(
                *lines,
                tray::menu(Some(Hotkey::default()), None, Language::English)
            );
            assert_eq!((at.x, at.y), (1500, 1040));
            assert!(launcher.visible(), "nothing picked");
        });
    }

    #[test]
    fn show_from_the_menu_shows_it() {
        on_hidden_desktop(|| {
            let (launcher, taskbar, _) = in_the_tray(None, None);
            launcher.press(VK_ESCAPE);
            launcher.pick(&taskbar, tray::Command::Show);
            assert!(launcher.visible());
        });
    }

    #[test]
    fn settings_from_the_menu_opens_the_settings_window() {
        on_hidden_desktop(|| {
            let (_scratch, config) = config_file("window-settings", "");
            let (launcher, taskbar, ran) = in_the_tray(Some(config), None);
            launcher.pick(&taskbar, tray::Command::Settings);
            assert!(with_app(|app| app.settings.is_some()).expect("app state"));
            assert!(ran.borrow().is_empty(), "no editor");
        });
    }

    #[test]
    fn without_a_config_there_are_no_settings_to_open() {
        on_hidden_desktop(|| {
            let (launcher, taskbar, _) = in_the_tray(None, None);
            launcher.pick(&taskbar, tray::Command::Settings);
            assert!(with_app(|app| app.settings.is_none()).expect("app state"));
        });
    }

    #[test]
    fn exit_takes_the_icon_away_saves_the_picks_and_quits() {
        let picks = PicksFile::new("exit");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let (launcher, taskbar, _) = in_the_tray(None, Some(path.clone()));
            launcher.type_text("note");
            launcher.press(VK_RETURN);
            std::fs::remove_file(&path).expect("the pick was saved");
            launcher.pick(&taskbar, tray::Command::Exit);
            assert!(
                !unsafe { IsWindow(Some(launcher.0)) }.as_bool(),
                "destroyed"
            );
            assert_eq!(*taskbar.removed.borrow(), 1);
            assert!(path.exists(), "saved again");
            let mut message = MSG::default();
            let quit = unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) };
            assert!(quit.as_bool());
        });
    }

    #[test]
    fn start_with_windows_from_the_menu_turns_it_on_and_off() {
        on_hidden_desktop(|| {
            let keys = autostart::tests::Keys::new("window-menu");
            let exe = r"C:\Programs\hyalo\hyalo.exe";
            let taskbar = Rc::new(Taskbar::default());
            let (mut services, _) = services(listing(&[]).0, None);
            services.tray = taskbar.clone();
            services.autostart = Some(keys.autostart(exe));
            let launcher = Launcher::serving(services);
            launcher.pick(&taskbar, tray::Command::StartWithWindows);
            let value = keys.run_value().expect("a Run value");
            assert_eq!(value, format!("\"{exe}\" --hidden"));
            launcher.pick(&taskbar, tray::Command::StartWithWindows);
            assert_eq!(keys.run_value(), None);
            launcher.pick(&taskbar, tray::Command::StartWithWindows);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            let menus = taskbar.menus.borrow();
            let lines = &menus.last().expect("a menu").0;
            assert_eq!(
                *lines,
                tray::menu(Some(Hotkey::default()), Some(true), Language::English)
            );
        });
    }

    #[test]
    fn opening_in_the_tray_looks_for_apps_without_showing() {
        on_hidden_desktop(|| {
            let (provider, runs) = listing(&["Notepad"]);
            let hwnd = open(services(provider, None).0, false).expect("open the launcher");
            let launcher = Launcher(hwnd);
            launcher.pump_until(|app| !app.catalog.items.is_empty());
            assert!(!launcher.visible());
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            launcher.hotkey();
            assert_eq!(runs.load(Ordering::SeqCst), 1, "still fresh when shown");
        });
    }

    /// A config file of its own for one test, holding `text`.
    pub(super) fn config_file(test: &str, text: &str) -> (Scratch, PathBuf) {
        let scratch = Scratch::new(test).with(&["hyalo\\"]);
        let path = scratch.0.join("hyalo").join("config.toml");
        std::fs::write(&path, text).expect("write the config");
        (scratch, path)
    }

    /// A launcher listing Notepad, on the config at `path`, with
    /// `keyboard` and a stand-in taskbar.
    pub(super) fn configured(path: &Path, keyboard: &Rc<Keyboard>) -> (Launcher, Rc<Taskbar>, Ran) {
        let taskbar = Rc::new(Taskbar::default());
        let (mut services, ran) = services(listing(&["Notepad"]).0, None);
        services.tray = taskbar.clone();
        services.hotkeys = Box::new(keyboard.clone());
        services.config = Some(path.to_path_buf());
        let launcher = Launcher::serving(services);
        launcher.pump_until(|app| !app.catalog.items.is_empty());
        (launcher, taskbar, ran)
    }

    pub(super) fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("a hotkey")
    }

    /// The titles listed now.
    fn listed() -> Vec<String> {
        with_app(|app| titles(app)).expect("app state")
    }

    #[test]
    fn a_missing_config_is_written_with_the_defaults() {
        on_hidden_desktop(|| {
            let scratch = Scratch::new("window-config-missing");
            let path = scratch.0.join("hyalo").join("config.toml");
            let keyboard = Rc::new(Keyboard::default());
            let _launcher = configured(&path, &keyboard);
            let written = std::fs::read_to_string(&path).expect("written");
            assert_eq!(written, hyalo_core::config::DEFAULT);
            assert_eq!(keyboard.held(), ["Alt+Space"]);
        });
    }

    #[test]
    fn the_hotkey_comes_from_the_config() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-hotkey", "hotkey = 'Ctrl+Shift+K'\n");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, taskbar, _) = configured(&path, &keyboard);
            assert_eq!(keyboard.held(), ["Ctrl+Shift+K"]);
            assert_eq!(*taskbar.tips.borrow(), ["hyalo · Ctrl+Shift+K"]);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            let menus = taskbar.menus.borrow();
            assert_eq!(
                menus[0].0,
                tray::menu(Some(hotkey("Ctrl+Shift+K")), None, Language::English)
            );
        });
    }

    #[test]
    fn a_hotkey_saved_applies_at_once_and_frees_the_old_one() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-save", "");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, taskbar, _) = configured(&path, &keyboard);
            std::fs::write(&path, "hotkey = 'Win+F2'\n").expect("save");
            launcher.pump_until(|_| keyboard.held() == ["Win+F2"]);
            assert_eq!(
                taskbar.tips.borrow().last().expect("a tip"),
                "hyalo · Win+F2"
            );
        });
    }

    #[test]
    fn a_hotkey_windows_refuses_keeps_the_old_one_and_says_so() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-refused", "");
            let keyboard = Rc::new(Keyboard::default());
            keyboard.taken.borrow_mut().push(hotkey("Ctrl+K"));
            let (launcher, taskbar, _) = configured(&path, &keyboard);
            std::fs::write(&path, "hotkey = 'Ctrl+K'\n").expect("save");
            launcher.pump_until(|app| app.results.rows().len() == 1);
            let said = "Ctrl+K is taken: Alt+Space still shows hyalo";
            assert_eq!(first_row().0, said);
            assert_eq!(keyboard.held(), ["Alt+Space"]);
            let tip = taskbar.tips.borrow().last().expect("a tip").clone();
            assert_eq!(tip, format!("hyalo · {said}"));
        });
    }

    #[test]
    fn a_broken_config_keeps_the_settings_in_force_and_says_where() {
        let picks = PicksFile::new("broken-config");
        let picked = picks.0.clone();
        on_hidden_desktop(move || {
            let text = "hotkey = 'Ctrl+Shift+K'\n";
            let (_scratch, path) = config_file("window-config-broken", text);
            let keyboard = Rc::new(Keyboard::default());
            let taskbar = Rc::new(Taskbar::default());
            let (mut services, ran) = services(listing(&[]).0, Some(picked.clone()));
            services.tray = taskbar.clone();
            services.hotkeys = Box::new(keyboard.clone());
            services.config = Some(path.clone());
            let launcher = Launcher::serving(services);
            std::fs::write(&path, "\nhotkye = 1\n").expect("save");
            launcher.pump_until(|app| app.results.rows().len() == 1);
            let (title, subtitle) = first_row();
            assert!(
                title.starts_with("config.toml, line 2: unknown field `hotkye`"),
                "{title}"
            );
            assert!(subtitle.contains("last good settings"), "{subtitle}");
            assert_eq!(keyboard.held(), ["Ctrl+Shift+K"]);
            assert!(
                taskbar
                    .tips
                    .borrow()
                    .last()
                    .expect("a tip")
                    .contains("line 2")
            );
            launcher.press(VK_RETURN);
            assert!(!launcher.visible());
            launcher.pump_until(|app| app.settings.is_some());
            assert!(
                ran.borrow().is_empty(),
                "the settings window, not an editor"
            );
            assert!(!picked.exists(), "a notice is not a pick");
        });
    }

    #[test]
    fn fixing_the_config_takes_the_notice_away() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-fixed", "hotkey = 5\n");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, _, _) = configured(&path, &keyboard);
            assert_eq!(
                launcher.rect(),
                launcher.placement().rect(1, None),
                "broken at start"
            );
            std::fs::write(&path, "hotkey = 'Ctrl+Alt+H'\n").expect("save");
            launcher.pump_until(|app| app.results.rows().is_empty());
            assert_eq!(launcher.rect(), launcher.placement().rect(0, None));
            assert_eq!(keyboard.held(), ["Ctrl+Alt+H"]);
        });
    }

    #[test]
    fn a_config_broken_at_start_runs_on_the_defaults() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-broken-start", "hotkey = 5\n");
            let keyboard = Rc::new(Keyboard::default());
            let _launcher = configured(&path, &keyboard);
            assert_eq!(keyboard.held(), ["Alt+Space"]);
            let leaves_out = with_app(|app| app.config.exclude.clone()).expect("app state");
            assert_eq!(leaves_out, Config::default().exclude);
        });
    }

    #[test]
    fn the_notice_is_not_listed_while_typing() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-typing", "hotkey = 5\n");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, _, ran) = configured(&path, &keyboard);
            launcher.type_text("note");
            assert_eq!(listed(), ["Notepad"]);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
        });
    }

    #[test]
    fn folders_the_config_ranks_higher_rise_and_change_when_saved() {
        on_hidden_desktop(|| {
            let higher = "[folders]\nrank_higher = ['high']\n";
            let (_scratch, path) = config_file("window-config-places", higher);
            let tree = Scratch::new("window-places").with(&[
                "high\\",
                "low\\",
                "high\\notes.txt",
                "low\\notes.txt",
            ]);
            let (mut services, _) = walking(just(&tree.0), None);
            services.config = Some(path.clone());
            let launcher = Launcher::serving(services);
            launcher.search_for("notes", 2);
            let first =
                || with_app(|app| app.results.rows()[0].item.id.0.clone()).expect("app state");
            assert_eq!(first(), tree.path("high\\notes.txt"));
            std::fs::write(&path, "[folders]\nrank_higher = ['low']\n").expect("save");
            let low = tree.path("low\\notes.txt");
            launcher.pump_until(|app| app.results.rows()[0].item.id.0 == low);
        });
    }

    #[test]
    fn folders_the_config_leaves_out_are_not_listed_and_change_when_saved() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-excluded").with(&[
                "keep\\",
                "drop\\",
                "keep\\notes.txt",
                "drop\\notes.txt",
            ]);
            let excluding = |path: &str| format!("[folders]\nexclude = ['{path}']\n");
            let (_scratch, config) =
                config_file("window-config-exclude", &excluding(&tree.path("drop")));
            let (mut services, _) = walking(just(&tree.0), None);
            services.config = Some(config.clone());
            let launcher = Launcher::serving(services);
            launcher.search_for("notes", 1);
            assert_eq!(listed(), ["notes.txt"]);
            let kept = tree.path("keep\\notes.txt");
            assert_eq!(
                with_app(|app| app.results.rows()[0].item.id.0.clone()),
                Some(kept)
            );
            std::fs::write(&config, excluding(&tree.path("keep"))).expect("save");
            let dropped = tree.path("drop\\notes.txt");
            launcher.pump_until(|app| {
                let rows = app.results.rows();
                rows.len() == 1 && rows[0].item.id.0 == dropped
            });
        });
    }

    #[test]
    fn the_icon_comes_back_when_explorer_restarts() {
        on_hidden_desktop(|| {
            let (launcher, taskbar, _) = in_the_tray(None, None);
            launcher.send(tray::taskbar_created(), 0);
            assert_eq!(taskbar.tips.borrow().len(), 2);
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
    fn an_alt_key_in_the_launcher_looks_for_no_menu() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let menu = windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
            // Asked of the launcher's own handling, never sent: Windows
            // answers an Alt key's letter with a beep when there is no
            // menu, on the user's speakers.
            let keys = [
                (WM_SYSCHAR, usize::from(b'k')),
                (WM_SYSKEYUP, usize::from(menu.0)),
            ];
            for (message, key) in keys {
                let handled = on_message(launcher.0, message, WPARAM(key), LPARAM(0));
                assert!(handled.is_some(), "{message:#x}");
            }
            assert!(launcher.visible());
        });
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
    fn the_arrows_home_and_end_move_the_caret_through_the_query() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let start = launcher.caret_x();
            launcher.type_text("notepad");
            let end = launcher.caret_x();
            launcher.press(VK_LEFT);
            let left = launcher.caret_x();
            assert!(start < left && left < end, "{start} < {left} < {end}");
            launcher.press(VK_HOME);
            assert_eq!(launcher.caret_x(), start);
            launcher.press(VK_RIGHT);
            assert!(launcher.caret_x() > start);
            launcher.press(VK_END);
            assert_eq!(launcher.caret_x(), end);
        });
    }

    #[test]
    fn a_letter_typed_in_the_middle_goes_in_at_the_caret_and_searches() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad", "Paint"]);
            launcher.type_text("ntepad");
            launcher.press(VK_HOME);
            launcher.press(VK_RIGHT);
            launcher.type_text("o");
            assert_eq!(launcher.query(), "notepad");
            launcher.pump_until(|app| titles(app) == ["Notepad"]);
        });
    }

    #[test]
    fn shift_selects_and_what_is_typed_takes_the_selections_place() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text("paint");
            hold(&[VK_SHIFT]);
            launcher.press(VK_HOME);
            hold(&[]);
            launcher.type_text("note");
            assert_eq!(launcher.query(), "note");
        });
    }

    #[test]
    fn ctrl_a_selects_everything_and_delete_takes_it_out() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.search_for("note", 1);
            hold(&[VK_CONTROL]);
            launcher.press(VK_A);
            hold(&[]);
            launcher.press(VK_DELETE);
            assert_eq!(launcher.query(), "");
            launcher.pump_until(|app| app.results.rows().is_empty());
            let bar = launcher.placement().rect(0, None);
            assert_eq!(launcher.rect(), bar);
        });
    }

    #[test]
    fn ctrl_c_with_text_selected_copies_the_text_and_keeps_it_open() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.search_for("note", 1);
            hold(&[VK_SHIFT]);
            launcher.press(VK_LEFT);
            launcher.press(VK_LEFT);
            hold(&[VK_CONTROL]);
            launcher.press(VK_C);
            assert_eq!(*ran.borrow(), [(Action::CopyText("te".to_string()), false)]);
            assert!(launcher.visible());
            assert_eq!(launcher.query(), "note");
        });
    }

    #[test]
    fn ctrl_x_cuts_the_selection_to_the_clipboard_and_searches_again() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad", "Note"], None);
            launcher.search_for("notepad", 1);
            hold(&[VK_SHIFT, VK_CONTROL]);
            launcher.press(VK_LEFT);
            hold(&[VK_CONTROL]);
            launcher.press(VK_X);
            assert_eq!(
                *ran.borrow(),
                [(Action::CopyText("notepad".to_string()), false)]
            );
            assert_eq!(launcher.query(), "");
            launcher.press(VK_X);
            assert_eq!(ran.borrow().len(), 1, "nothing selected, nothing cut");
        });
    }

    #[test]
    fn ctrl_v_pastes_the_clipboards_text_as_one_line() {
        let name = "window::tests::ctrl_v_pastes_the_clipboards_text_as_one_line";
        in_own_window_station(name, || {
            on_hidden_desktop(|| {
                crate::clipboard::copy("pad\r\nplus\r\n").expect("copy");
                let launcher = Launcher::listing(&["Notepad", "Paint"]);
                launcher.type_text("note");
                hold(&[VK_CONTROL]);
                launcher.press(VK_V);
                assert_eq!(launcher.query(), "notepad plus");
                assert_eq!(crate::clipboard::text().as_deref(), Some("pad\r\nplus\r\n"));
            });
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
    fn the_message_loop_takes_every_posted_message_off_the_queue() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.post(WM_NULL, 0);
            unsafe { PostQuitMessage(0) };
            message_loop();
            let mut message = MSG::default();
            let posted = PM_REMOVE | PM_QS_POSTMESSAGE;
            let left = unsafe { PeekMessageW(&mut message, None, 0, 0, posted) };
            assert!(!left.as_bool(), "message {:#x} left", message.message);
        });
    }

    #[test]
    fn answers_zero_to_the_messages_it_handles() {
        let handled = [
            (WM_NCCALCSIZE, 1),
            (WM_CHAR, usize::from(b'a')),
            (WM_KEYDOWN, usize::from(VK_A.0)),
            (WM_ACTIVATE, WA_INACTIVE as usize),
            (WM_HOTKEY, HOTKEY_TOGGLE as usize),
            (WM_FOUND, 0),
            (WM_SHOW, 0),
            (tray::WM_TRAY, 0),
        ];
        on_hidden_desktop(move || {
            let launcher = Launcher::open();
            for (message, wparam) in handled {
                let answer = launcher.send(message, wparam);
                assert_eq!(answer, LRESULT(0), "message {message:#x}");
            }
        });
    }

    #[test]
    fn destroying_the_window_quits_the_app_with_exit_code_zero() {
        on_hidden_desktop(|| {
            let hwnd = open(services(listing(&[]).0, None).0, true).expect("open the launcher");
            unsafe { DestroyWindow(hwnd) }.expect("destroy the window");
            let mut message = MSG::default();
            let quit = unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) };
            assert!(quit.as_bool());
            assert_eq!(message.wParam.0, 0, "exit code");
        });
    }

    /// Alt+F4 as Windows sends it to the window with the focus: F4 down
    /// with Alt held, which the context bit of lParam says too.
    pub(super) fn alt_f4(hwnd: HWND) -> LRESULT {
        let menu = windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU;
        let f4 = windows::Win32::UI::Input::KeyboardAndMouse::VK_F4;
        hold(&[menu]);
        let alt = LPARAM(1 << 29);
        let answer =
            unsafe { SendMessageW(hwnd, WM_SYSKEYDOWN, Some(WPARAM(f4.0.into())), Some(alt)) };
        hold(&[]);
        answer
    }

    #[test]
    fn alt_f4_hides_the_launcher_and_hyalo_runs_on() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text("note");
            alt_f4(launcher.0);
            // Windows posts the close Alt+F4 asks for.
            drain();
            assert!(!launcher.visible());
            assert!(
                unsafe { IsWindow(Some(launcher.0)) }.as_bool(),
                "not closed"
            );
            launcher.hotkey();
            assert!(launcher.visible());
            assert_eq!(launcher.query(), "", "hidden as Esc hides it");
        });
    }

    #[test]
    fn other_keys_with_alt_are_windows_own() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let space = windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE;
            // Asked of the launcher's own handling, never sent.
            for key in [space, VK_RETURN, VK_UP] {
                let answer = on_message(launcher.0, WM_SYSKEYDOWN, WPARAM(key.0.into()), LPARAM(0));
                assert!(answer.is_none(), "{key:?}");
            }
        });
    }

    #[test]
    fn being_asked_to_close_still_closes_hyalo() {
        on_hidden_desktop(|| {
            let hwnd = open(services(listing(&[]).0, None).0, true).expect("open the launcher");
            unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) };
            assert!(!unsafe { IsWindow(Some(hwnd)) }.as_bool());
            let mut message = MSG::default();
            let quit = unsafe { PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE) };
            assert!(quit.as_bool());
        });
    }

    #[test]
    fn typing_lists_the_matching_apps_and_grows_the_window_down() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad", "Paint", "Sticky Notes"]);
            let bar = launcher.rect();
            launcher.type_text("note");
            let list = launcher.rect();
            assert_eq!((list.x, list.y, list.width), (bar.x, bar.y, bar.width));
            assert_eq!(list, launcher.placement().rect(2, None));
            assert_eq!(launcher.frame().height, list.height as usize);
        });
    }

    #[test]
    fn emptying_the_query_shrinks_the_window_to_the_bar() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.type_text("no");
            launcher.type_text("\u{8}\u{8}");
            assert_eq!(launcher.rect(), launcher.placement().rect(0, None));
        });
    }

    #[test]
    fn a_row_shows_its_icon_and_marks_the_letters_typed() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.type_text("note");
            let top = layout::row_top(0);
            assert_eq!(launcher.pixel_at(28.0, top + 24.0), [0, 0, 255, 255]);
            let frame = launcher.frame();
            let scale = layout::scale(launcher.placement().dpi);
            let band = (top * scale) as usize..((top + 26.0) * scale) as usize;
            let accent = band
                .flat_map(|y| (0..frame.width).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let [blue, _, red, _] = frame.pixel(x, y);
                    blue > red.saturating_add(80)
                })
                .count();
            assert!(accent > 20, "{accent} accent pixels in the title");
        });
    }

    #[test]
    fn the_arrows_move_the_selection_and_wrap() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["a1", "a2", "a3"]);
            launcher.type_text("a");
            assert_eq!(launcher.lit_row(3), 0);
            launcher.press(VK_DOWN);
            assert_eq!(launcher.lit_row(3), 1);
            launcher.press(VK_UP);
            launcher.press(VK_UP);
            assert_eq!(launcher.lit_row(3), 2);
        });
    }

    #[test]
    fn reopening_shows_the_bar_alone() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.type_text("note");
            launcher.press(VK_ESCAPE);
            launcher.hotkey();
            assert_eq!(launcher.rect(), launcher.placement().rect(0, None));
        });
    }

    #[test]
    fn showing_again_soon_does_not_list_the_apps_again() {
        on_hidden_desktop(|| {
            let (provider, runs) = listing(&["Notepad"]);
            let launcher = Launcher::with(provider);
            launcher.pump_until(|app| !app.catalog.items.is_empty());
            launcher.hotkey();
            launcher.hotkey();
            assert_eq!(runs.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn apps_found_while_the_query_is_typed_are_listed_at_once() {
        on_hidden_desktop(|| {
            let (provider, release) = gated(&[], &["Notepad"]);
            let launcher = Launcher::with(provider);
            launcher.type_text("note");
            assert_eq!(launcher.rect(), launcher.placement().rect(0, None));
            release.send(()).expect("release the provider");
            launcher.pump_until(|app| !app.catalog.items.is_empty());
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
        });
    }

    #[test]
    fn apps_found_while_hidden_wait_for_the_next_show() {
        on_hidden_desktop(|| {
            let (provider, release) = gated(&[], &["Notepad"]);
            let launcher = Launcher::with(provider);
            launcher.press(VK_ESCAPE);
            release.send(()).expect("release the provider");
            launcher.pump_until(|app| !app.catalog.items.is_empty());
            assert!(!launcher.visible());
            launcher.hotkey();
            launcher.type_text("note");
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
        });
    }

    #[test]
    fn a_refresh_keeps_the_selected_app_selected() {
        on_hidden_desktop(|| {
            let (provider, release) = gated(&["Notes", "Notepad"], &["Note", "Notes", "Notepad"]);
            let (services, ran) = services(provider, None);
            let launcher = Launcher::serving(services);
            launcher.pump_until(|app| app.catalog.items.len() == 2);
            launcher.type_text("note");
            launcher.press(VK_DOWN);
            release.send(()).expect("release the provider");
            launcher.pump_until(|app| app.catalog.items.len() == 3);
            assert_eq!(launcher.lit_row(3), 2);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
        });
    }

    #[test]
    fn hovering_a_row_selects_it() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["a1", "a2", "a3"]);
            launcher.type_text("a");
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(2));
            assert_eq!(launcher.lit_row(3), 2);
            launcher.mouse(WM_MOUSEMOVE, 200.0, 30.0);
            assert_eq!(launcher.lit_row(3), 2, "the bar selects nothing");
        });
    }

    #[test]
    fn a_move_to_where_the_pointer_already_was_keeps_the_selection() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["a1", "a2", "a3"]);
            launcher.type_text("a");
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(2));
            launcher.press(VK_UP);
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(2));
            assert_eq!(
                launcher.lit_row(3),
                1,
                "the same spot: Windows, not the user"
            );
            launcher.mouse(WM_MOUSEMOVE, 201.0, middle_of(2));
            assert_eq!(launcher.lit_row(3), 2, "a real move");
        });
    }

    #[test]
    fn showing_again_forgets_where_the_pointer_was() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["a1", "a2", "a3"]);
            launcher.type_text("a");
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(2));
            launcher.press(VK_ESCAPE);
            launcher.hotkey();
            launcher.type_text("a");
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(2));
            assert_eq!(launcher.lit_row(3), 2);
        });
    }

    #[test]
    fn clicking_a_row_opens_it_and_hides() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notes", "Notepad"], None);
            launcher.type_text("note");
            launcher.click(200.0, middle_of(1));
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn letting_go_over_another_row_than_the_one_pressed_opens_nothing() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notes", "Notepad"], None);
            launcher.type_text("note");
            launcher.mouse(WM_LBUTTONDOWN, 200.0, middle_of(0));
            launcher.mouse(WM_LBUTTONUP, 200.0, middle_of(1));
            launcher.mouse(WM_LBUTTONUP, 200.0, middle_of(1));
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
        });
    }

    #[test]
    fn a_click_in_the_query_puts_the_caret_where_it_was_clicked() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let start = launcher.caret_x();
            launcher.type_text("notepad notes");
            let end = launcher.caret_x();
            let middle = (start + end) / 2;
            launcher.click_pixel(middle);
            let caret = launcher.caret_x();
            assert!(
                caret.abs_diff(middle) < 10,
                "clicked {middle}, caret at {caret}"
            );
            launcher.click_pixel(1);
            assert_eq!(launcher.caret_x(), caret, "the icon is not the query");
            launcher.click_pixel(end + 100);
            assert_eq!(launcher.caret_x(), end, "past the text is its end");
        });
    }

    #[test]
    fn a_click_in_a_query_scrolled_to_its_end_lands_where_it_was_clicked() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text(&"notepad notes ".repeat(8));
            let end = launcher.caret_x();
            launcher.click_pixel(end - 40);
            let caret = launcher.caret_x();
            assert!(
                caret.abs_diff(end - 40) < 10,
                "clicked {}, caret at {caret}",
                end - 40
            );
        });
    }

    #[test]
    fn the_query_takes_clicks_up_to_its_room_on_the_right_and_low_in_the_bar() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text("note");
            launcher.press(VK_HOME);
            let scale = layout::scale(launcher.placement().dpi);
            let (width, _) = launcher.client_size();
            let room = width as f32 / scale - layout::TEXT_RIGHT_PAD;
            let low = layout::BAR_HEIGHT - 8.0;
            launcher.click(room + 4.0, low);
            launcher.type_text("x");
            assert_eq!(launcher.query(), "xnote", "right of the room is glass");
            launcher.click(room - 4.0, low);
            launcher.type_text("y");
            assert_eq!(launcher.query(), "xnotey", "the query's own room");
        });
    }

    #[test]
    fn dragging_across_the_query_selects_what_it_passes_over() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("note");
            let scale = layout::scale(launcher.placement().dpi);
            let end = launcher.caret_x() as f32 / scale;
            let middle = layout::BAR_HEIGHT / 2.0;
            launcher.mouse(WM_LBUTTONDOWN, layout::TEXT_LEFT, middle);
            launcher.mouse(WM_MOUSEMOVE, end + 30.0, middle);
            assert_eq!(launcher.selected(), "note");
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(0));
            launcher.mouse(WM_LBUTTONUP, 200.0, middle_of(0));
            assert!(
                ran.borrow().is_empty(),
                "letting go over a row opens nothing"
            );
            assert_eq!(launcher.selected(), "note");
            launcher.mouse(WM_MOUSEMOVE, layout::TEXT_LEFT, middle);
            assert_eq!(
                launcher.selected(),
                "note",
                "the button is up: nothing changes"
            );
            launcher.type_text("x");
            assert_eq!(launcher.query(), "x");
        });
    }

    #[test]
    fn a_shift_click_selects_from_the_caret_to_where_it_was_clicked() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text("notepad");
            let end = launcher.caret_x();
            launcher.press(VK_HOME);
            let scale = layout::scale(launcher.placement().dpi);
            let middle = layout::BAR_HEIGHT / 2.0;
            launcher.mouse_with(
                WM_LBUTTONDOWN,
                end as f32 / scale + 5.0,
                middle,
                MOUSE_SHIFT,
            );
            launcher.mouse(WM_LBUTTONUP, end as f32 / scale + 5.0, middle);
            assert_eq!(launcher.selected(), "notepad");
        });
    }

    /// Drags the launcher by its search icon by (x, y) DIPs; how far it
    /// moved, in pixels, as the mouse messages carry it.
    fn drag_by(launcher: &Launcher, (x, y): (f32, f32)) -> (i32, i32) {
        let scale = layout::scale(launcher.placement().dpi);
        let icon = (20.0, layout::BAR_HEIGHT / 2.0);
        launcher.mouse(WM_LBUTTONDOWN, icon.0, icon.1);
        launcher.mouse(WM_MOUSEMOVE, icon.0 + x, icon.1 + y);
        launcher.mouse(WM_LBUTTONUP, icon.0 + x, icon.1 + y);
        let pixels = |dips: f32| (dips * scale) as i32;
        (
            pixels(icon.0 + x) - pixels(icon.0),
            pixels(icon.1 + y) - pixels(icon.1),
        )
    }

    #[test]
    fn dragging_the_glass_moves_the_launcher_and_it_shows_there_again() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            let before = launcher.rect();
            let (x, y) = drag_by(&launcher, (100.0, 50.0));
            let moved = launcher.rect();
            assert_eq!((moved.x, moved.y), (before.x + x, before.y + y));
            assert_eq!((moved.width, moved.height), (before.width, before.height));
            launcher.press(VK_ESCAPE);
            launcher.hotkey();
            assert_eq!(launcher.rect(), moved);
            launcher.search_for("note", 1);
            let listed = launcher.rect();
            assert_eq!(
                (listed.x, listed.y),
                (moved.x, moved.y),
                "the list grows down"
            );
        });
    }

    #[test]
    fn pressing_the_query_or_a_row_moves_nothing() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.search_for("note", 1);
            let before = launcher.rect();
            for y in [layout::BAR_HEIGHT / 2.0, middle_of(0)] {
                launcher.mouse(WM_LBUTTONDOWN, 200.0, y);
                launcher.mouse(WM_MOUSEMOVE, 300.0, y + 20.0);
                assert_eq!(launcher.rect(), before, "{y}");
                launcher.mouse(WM_LBUTTONUP, 300.0, y + 20.0);
            }
        });
    }

    #[test]
    fn a_double_click_on_the_glass_puts_it_back_in_its_first_place() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let first = launcher.rect();
            drag_by(&launcher, (100.0, 50.0));
            assert_ne!(launcher.rect(), first);
            launcher.mouse(WM_LBUTTONDBLCLK, 20.0, layout::BAR_HEIGHT / 2.0);
            launcher.mouse(WM_LBUTTONUP, 20.0, layout::BAR_HEIGHT / 2.0);
            assert_eq!(launcher.rect(), first);
        });
    }

    #[test]
    fn it_stops_where_its_tallest_list_still_fits_its_work_area() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            drag_by(&launcher, (3000.0, 3000.0));
            let placement = launcher.placement();
            let (work, tallest) = (placement.work_area, placement.rect(MAX_ROWS, None));
            let rect = launcher.rect();
            assert_eq!(rect.x + rect.width, work.x + work.width);
            assert_eq!(rect.y + tallest.height, work.y + work.height);
        });
    }

    #[test]
    fn where_it_was_dragged_to_is_kept_across_starts_until_put_back() {
        on_hidden_desktop(|| {
            let folder = Scratch::new("window-position");
            let file = PathBuf::from(folder.path("position.txt"));
            let start = || {
                let (mut services, _) = services(listing(&[]).0, None);
                services.position = Some(file.clone());
                Launcher::serving(services)
            };
            let first = start();
            let at_first = first.rect();
            drag_by(&first, (-100.0, 40.0));
            let moved = first.rect();
            drop(first);
            assert!(file.exists());
            let second = start();
            assert_eq!(second.rect(), moved);
            second.mouse(WM_LBUTTONDBLCLK, 20.0, layout::BAR_HEIGHT / 2.0);
            assert!(!file.exists(), "put back, nothing to keep");
            drop(second);
            assert_eq!(start().rect(), at_first);
        });
    }

    #[test]
    fn the_pointer_is_an_i_beam_over_the_query_and_an_arrow_elsewhere() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            let rect = launcher.rect();
            let scale = layout::scale(launcher.placement().dpi);
            let on = |x: f32| POINT {
                x: rect.x + (x * scale) as i32,
                y: rect.y + (layout::BAR_HEIGHT / 2.0 * scale) as i32,
            };
            let shape = |name| unsafe { LoadCursorW(None, name) }.expect("a system cursor");
            set_cursor_over(launcher.0, on(200.0)).expect("set the pointer");
            assert_eq!(unsafe { GetCursor() }, shape(IDC_IBEAM));
            set_cursor_over(launcher.0, on(20.0)).expect("set the pointer");
            assert_eq!(unsafe { GetCursor() }, shape(IDC_ARROW));
        });
    }

    #[test]
    fn a_double_click_selects_a_word_and_a_third_click_everything() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.type_text("open notes");
            let scale = layout::scale(launcher.placement().dpi);
            let on_notes = (launcher.caret_x() as f32 - 10.0) / scale;
            let middle = layout::BAR_HEIGHT / 2.0;
            launcher.click(on_notes, middle);
            launcher.mouse(WM_LBUTTONDBLCLK, on_notes, middle);
            launcher.mouse(WM_LBUTTONUP, on_notes, middle);
            assert_eq!(launcher.selected(), "notes");
            launcher.click(on_notes, middle);
            assert_eq!(launcher.selected(), "open notes");
            launcher.click(on_notes, middle);
            assert_eq!(launcher.selected(), "", "a fourth click places the caret");
        });
    }

    #[test]
    fn clicking_the_bar_opens_nothing() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("note");
            launcher.mouse(WM_LBUTTONUP, 200.0, 30.0);
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
        });
    }

    #[test]
    fn enter_opens_the_selected_app_and_hides() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notes", "Notepad"], None);
            launcher.type_text("note");
            launcher.press(VK_DOWN);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn ctrl_shift_enter_opens_it_as_administrator() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("note");
            hold(&[VK_CONTROL, VK_SHIFT]);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", true)]);
        });
    }

    #[test]
    fn enter_with_nothing_listed_only_hides() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("xyz");
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty());
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn arithmetic_lists_its_answer_on_top_and_enter_copies_it() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("2+2*2");
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
            launcher.press(VK_RETURN);
            let copy = Action::CopyText("6".to_string());
            assert_eq!(*ran.borrow(), [(copy, false)]);
            assert!(!launcher.visible());
        });
    }

    /// A rates source that hands over the fixture's rates, of 10.11.2026,
    /// once the test lets it go.
    fn rating() -> (Source, Sender<()>) {
        let (release, gate) = channel::<()>();
        let gate = Mutex::new(gate);
        let provider: Provider = Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
            let _ = gate.lock().expect("the gate").recv();
            report(Found::Rates(rates::tests::fixture()));
        });
        let source = Source {
            provider,
            trusted: rates::REFRESH_AFTER,
            excluded: None,
        };
        (source, release)
    }

    /// The first row: what it shows, and its second line.
    fn first_row() -> (String, String) {
        let first = with_app(|app| app.results.rows()[0].item.clone()).expect("app state");
        (first.title, first.subtitle)
    }

    /// The labels of the menu the tray opened last.
    fn menu_labels(taskbar: &Taskbar) -> Vec<String> {
        let menus = taskbar.menus.borrow();
        let (lines, _) = menus.last().expect("a menu");
        let entries = lines.iter().filter_map(|line| match line {
            tray::Line::Entry(entry) => Some(entry.label.clone()),
            tray::Line::Separator => None,
        });
        entries.collect()
    }

    const RUSSIAN_MENU: [&str; 4] = [
        "Показать hyalo\tAlt+Space",
        "Параметры",
        "Запускать вместе с Windows",
        "Выход",
    ];

    #[test]
    fn a_config_in_russian_speaks_russian_in_the_tray_and_the_rows() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-russian", "language = 'ru'\n");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, taskbar, _) = configured(&path, &keyboard);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(menu_labels(&taskbar), RUSSIAN_MENU);
            launcher.search_for("2+2", 1);
            let calculator = "Калькулятор · Enter — скопировать".to_string();
            assert_eq!(first_row(), ("4".to_string(), calculator));
        });
    }

    #[test]
    fn a_config_without_a_language_speaks_the_one_windows_is_shown_in() {
        on_hidden_desktop(|| {
            let text = "hotkey = 'Alt+Space'\n";
            let (_scratch, path) = config_file("window-config-windows-language", text);
            let taskbar = Rc::new(Taskbar::default());
            let (mut services, _) = services(listing(&[]).0, None);
            services.tray = taskbar.clone();
            services.config = Some(path);
            services.windows_language = Language::Russian;
            let launcher = Launcher::serving(services);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(menu_labels(&taskbar), RUSSIAN_MENU);
        });
    }

    #[test]
    fn a_language_saved_applies_at_once_and_a_broken_file_keeps_it() {
        on_hidden_desktop(|| {
            let (_scratch, path) = config_file("window-config-language-saved", "language = 'ru'\n");
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, taskbar, _) = configured(&path, &keyboard);
            std::fs::write(&path, "language = 'en'\n").expect("save");
            launcher.pump_until(|app| app.language() == Language::English);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(menu_labels(&taskbar)[3], "Exit");
            std::fs::write(&path, "language = 'ru'\n").expect("save");
            launcher.pump_until(|app| app.language() == Language::Russian);
            std::fs::write(&path, "\nhotkye = 1\n").expect("save");
            launcher.pump_until(|app| app.results.rows().len() == 1);
            let (title, subtitle) = first_row();
            assert!(title.starts_with("config.toml, строка 2: "), "{title}");
            let advice = "Действуют последние исправные настройки · Enter — открыть параметры";
            assert_eq!(subtitle, advice);
            let tips = taskbar.tips.borrow();
            let tip = tips.last().expect("a tip");
            assert!(tip.starts_with("hyalo · config.toml, строка 2: "), "{tip}");
        });
    }

    #[test]
    fn a_missing_config_is_written_in_the_language_windows_is_shown_in() {
        on_hidden_desktop(|| {
            let scratch = Scratch::new("window-config-missing-russian");
            let path = scratch.0.join("hyalo").join("config.toml");
            let (mut services, _) = services(listing(&[]).0, None);
            services.config = Some(path.clone());
            services.windows_language = Language::Russian;
            let _launcher = Launcher::serving(services);
            let written = std::fs::read_to_string(&path).expect("written");
            assert_eq!(written, hyalo_core::config::default_file(Language::Russian));
        });
    }

    #[test]
    fn a_currency_lists_its_conversion_on_top_and_enter_copies_it() {
        on_hidden_desktop(|| {
            let (source, release) = rating();
            let (mut services, ran) = services(listing(&["Notepad"]).0, None);
            services.sources.push(source);
            let launcher = Launcher::serving(services);
            release.send(()).expect("let the rates go");
            launcher.pump_until(|app| app.catalog.rates.is_some());
            launcher.type_text("100 usd");
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
            let (title, subtitle) = first_row();
            assert_eq!(title, "8\u{A0}275.36\u{A0}₽");
            assert!(subtitle.contains("10.11.2026"), "{subtitle}");
            launcher.press(VK_RETURN);
            let copy = Action::CopyText("8275.36".to_string());
            assert_eq!(*ran.borrow(), [(copy, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn rates_found_while_a_currency_is_typed_answer_it_at_once() {
        on_hidden_desktop(|| {
            let (source, release) = rating();
            let (mut services, _) = services(listing(&[]).0, None);
            services.sources.push(source);
            let launcher = Launcher::serving(services);
            launcher.type_text("400 rub");
            let listed = with_app(|app| titles(app)).expect("app state");
            assert_eq!(listed, Vec::<String>::new());
            release.send(()).expect("let the rates go");
            launcher.pump_until(|app| app.results.rows().len() == 1);
            assert_eq!(first_row().0, "4.83\u{A0}$");
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
        });
    }

    #[test]
    fn copying_an_answer_is_not_remembered_as_a_pick() {
        let picks = PicksFile::new("answer");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let (launcher, ran) = Launcher::listing_with(&[], Some(path.clone()));
            launcher.type_text("2+2");
            launcher.press(VK_RETURN);
            assert_eq!(ran.borrow().len(), 1, "the answer was copied");
            assert!(!path.exists(), "no picks saved");
        });
    }

    #[test]
    fn typing_lists_files_and_folders_with_their_icons() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-files").with(&["notes.txt", "Notes\\", "other.md"]);
            let (launcher, _) = Launcher::finding(&tree.0, None);
            launcher.search_for("notes", 2);
            launcher.pump_until(|app| app.catalog.icons.len() == 2);
            assert_eq!(
                with_app(|app| titles(app)).expect("app state"),
                ["Notes", "notes.txt"]
            );
            assert_eq!(launcher.rect(), launcher.placement().rect(2, None));
            let red = [0, 0, 255, 255];
            assert_eq!(launcher.pixel_at(28.0, layout::row_top(1) + 24.0), red);
        });
    }

    #[test]
    fn files_found_while_the_query_is_typed_are_listed_at_once() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-later").with(&["report.pdf"]);
            let (release, gate) = channel::<()>();
            let (gate, root) = (Mutex::new(gate), tree.0.clone());
            let roots: Roots = Arc::new(move || {
                let _ = gate.lock().expect("the gate").recv();
                vec![root.clone()]
            });
            let launcher = Launcher::serving(walking(roots, None).0);
            launcher.type_text("report");
            assert_eq!(launcher.rect(), launcher.placement().rect(0, None));
            release.send(()).expect("release the walk");
            launcher.pump_until(|app| !app.results.rows().is_empty());
            assert_eq!(launcher.rect(), launcher.placement().rect(1, None));
        });
    }

    #[test]
    fn enter_opens_a_file() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-open").with(&["report.pdf"]);
            let (launcher, ran) = Launcher::finding(&tree.0, None);
            launcher.search_for("report", 1);
            launcher.press(VK_RETURN);
            let opened = Action::Open(tree.path("report.pdf"));
            assert_eq!(*ran.borrow(), [(opened, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn ctrl_enter_shows_a_file_in_its_folder() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-reveal").with(&["report.pdf"]);
            let (launcher, ran) = Launcher::finding(&tree.0, None);
            launcher.search_for("report", 1);
            hold(&[VK_CONTROL]);
            launcher.press(VK_RETURN);
            let shown = Action::Reveal(tree.path("report.pdf"));
            assert_eq!(*ran.borrow(), [(shown, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn ctrl_c_copies_the_path_of_a_folder() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-copy").with(&["Reports\\"]);
            let (launcher, ran) = Launcher::finding(&tree.0, None);
            launcher.search_for("reports", 1);
            hold(&[VK_CONTROL]);
            launcher.press(VK_C);
            let copied = Action::CopyText(tree.path("Reports"));
            assert_eq!(*ran.borrow(), [(copied, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn ctrl_enter_and_ctrl_c_with_nothing_listed_keep_it_open() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("xyz");
            hold(&[VK_CONTROL]);
            launcher.press(VK_C);
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
        });
    }

    #[test]
    fn a_picked_file_among_the_files_found_still_leaves_eight_rows() {
        let picks = PicksFile::new("eight");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let names: Vec<String> = (1..=9).map(|n| format!("notes {n}.txt")).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let tree = Scratch::new("window-eight").with(&names);
            let (first, _) = Launcher::finding(&tree.0, Some(path.clone()));
            first.search_for("notes", MAX_ROWS);
            first.press(VK_RETURN);
            drop(first);
            let (second, _) = Launcher::finding(&tree.0, Some(path));
            second.pump_until(|app| !app.catalog.picked.is_empty());
            second.search_for("notes", MAX_ROWS);
        });
    }

    #[test]
    fn a_files_icon_is_asked_for_once() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-icon-once").with(&["report.pdf"]);
            let (mut services, _) = walking(just(&tree.0), None);
            let asked = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&asked);
            services.icons = Arc::new(move |ids: &[ItemId], size, report: &dyn Fn(Found)| {
                for id in ids {
                    counted.fetch_add(1, Ordering::SeqCst);
                    report(Found::Icon(id.clone(), red(size)));
                }
            });
            let launcher = Launcher::serving(services);
            launcher.search_for("report", 1);
            launcher.pump_until(|app| !app.catalog.icons.is_empty());
            launcher.type_text("\u{8}t");
            // A second request would come from a thread of its own.
            std::thread::sleep(Duration::from_millis(300));
            launcher.pump_until(|_| true);
            assert_eq!(asked.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn ctrl_enter_and_ctrl_c_leave_an_app_alone() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("note");
            hold(&[VK_CONTROL]);
            launcher.press(VK_RETURN);
            launcher.press(VK_C);
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
        });
    }

    #[test]
    fn a_picked_file_rises_above_an_equal_one_after_a_restart() {
        let picks = PicksFile::new("file");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let tree = Scratch::new("window-picked").with(&["notes a.txt", "notes b.txt"]);
            let (first, _) = Launcher::finding(&tree.0, Some(path.clone()));
            first.search_for("notes", 2);
            first.press(VK_DOWN);
            first.press(VK_RETURN);
            drop(first);
            let (second, ran) = Launcher::finding(&tree.0, Some(path));
            // As at a real start: the picks are back long before typing.
            second.pump_until(|app| !app.catalog.picked.is_empty());
            second.search_for("notes", 2);
            second.press(VK_RETURN);
            let opened = Action::Open(tree.path("notes b.txt"));
            assert_eq!(*ran.borrow(), [(opened, false)]);
        });
    }

    #[test]
    fn a_pick_survives_a_restart_and_lifts_its_app() {
        let picks = PicksFile::new("restart");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let titles = ["Notes", "Notepad"];
            let (first, _) = Launcher::listing_with(&titles, Some(path.clone()));
            first.type_text("note");
            first.press(VK_DOWN);
            first.press(VK_RETURN);
            drop(first);
            let (second, ran) = Launcher::listing_with(&titles, Some(path));
            second.type_text("note");
            second.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
        });
    }
}
