//! The launcher window: a borderless popup over a DWM Acrylic backdrop.
//! A global hotkey toggles it. Typing lists the matching apps, files and
//! folders under the bar, the arrows move the selection, Enter opens the
//! selected item (Ctrl+Shift+Enter as administrator), Ctrl+Enter shows a
//! file in its folder, Ctrl+C copies its path, and Esc, Enter or losing
//! focus hides it. A right click, the Menu key or Shift+F10 opens a row's
//! menu of the same.
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
use hyalo_core::filter::{Clock, Filters, Query};
use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::hotkey::Hotkey;
use hyalo_core::item::{Action, Item, ItemId, Kind};
use hyalo_core::places::Places;
use hyalo_core::rank::Ranked;
use hyalo_core::text::{Language, Text};
use hyalo_core::update::{Checked, Failure, Progress, Release, Update};
use hyalo_core::{system_commands, system_settings};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DWMWINDOWATTRIBUTE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
};
use windows::Win32::UI::Shell::{NIN_SELECT, NINF_KEY};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CS_DBLCLKS, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessagePos, GetMessageW,
    GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, HTCLIENT, IDC_ARROW, IDC_IBEAM,
    IsWindow, IsWindowVisible, LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    SM_CXDOUBLECLK, SM_CYDOUBLECLK, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SetCursor,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WA_INACTIVE, WM_ACTIVATE,
    WM_APP, WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDBLCLK,
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
use crate::row_menu::{self, RowMenu};
use crate::settings::{self, Loaded};
use crate::surface::Behind;
use crate::switcher;
use crate::tray::{self, Tray};
use crate::update::Updates;
use crate::{http, launch, rates, store, system, uninstall, update};

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

/// Carries out a picked item's action, as administrator or not, then
/// calls its `Done`, on the thread that carried it out.
pub type Runner = Box<dyn Fn(&Action, bool, Done)>;

/// What to do once an action was carried out.
pub type Done = Box<dyn FnOnce() + Send>;

/// Nothing to do once an action was carried out.
fn nothing() -> Done {
    Box::new(|| ())
}

/// A provider, and how long what it found is trusted.
pub struct Source {
    pub provider: Provider,
    pub trusted: Duration,
    /// What it leaves out of the folders it walks, if it walks any: the
    /// config's to set.
    pub excluded: Option<Excluded>,
    /// Whether it lists the apps, which an uninstall changes.
    apps: bool,
}

impl Source {
    /// A source of what `provider` finds, trusted for `trusted`, leaving
    /// `excluded` out of the folders it walks, if it walks any.
    pub fn new(provider: Provider, trusted: Duration, excluded: Option<Excluded>) -> Source {
        Source {
            provider,
            trusted,
            excluded,
            apps: false,
        }
    }

    /// The source that lists the apps, which an uninstall changes.
    pub fn apps(provider: Provider, trusted: Duration) -> Source {
        Source {
            apps: true,
            ..Source::new(provider, trusted, None)
        }
    }
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
    /// Opens a row's menu.
    pub row_menu: Rc<dyn RowMenu>,
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
    /// Where newer releases come from and how one is installed; `None`
    /// where hyalo does not update itself, as in a debug build.
    pub updates: Option<Updates>,
    /// Whether Windows has hibernation turned on, so Hibernate is listed.
    pub hibernates: bool,
    /// Asks DWM for a window's look.
    pub dwm: Dwm,
}

/// Sets one of a window's DWM attributes, each a 32-bit value, and says
/// whether DWM took it: `set_attribute`, or in tests a stand-in that
/// answers as an older Windows does.
pub type Dwm = fn(HWND, DWMWINDOWATTRIBUTE, u32) -> Result<()>;

/// The icons of Windows' settings. They never change, but the shell may
/// give none at start, so the ones still missing are asked for again as
/// often as the apps.
fn settings_icons() -> Source {
    Source::new(
        Arc::new(catalog::find_system_icons),
        catalog::REFRESH_AFTER,
        None,
    )
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
                Source::apps(
                    catalog::apps(Arc::new(uninstall::entries)),
                    catalog::REFRESH_AFTER,
                ),
                settings_icons(),
                Source::new(walk, files::REFRESH_AFTER, Some(excluded)),
                Source::new(
                    rates::provider(
                        Arc::new(|| http::get(&rates::bank())),
                        rates::default_cache(),
                        Arc::new(rates::moscow_today),
                    ),
                    rates::REFRESH_AFTER,
                    None,
                ),
                // Looked for at every show: windows come and go.
                Source::new(switcher::provider(), Duration::ZERO, None),
            ],
            files,
            icons: Arc::new(catalog::file_icons),
            places: files::places(),
            run: Box::new(run_in_background),
            picks: store::default_path(),
            hotkeys: Box::new(hotkeys::System),
            tray: Rc::new(tray::Shell),
            row_menu: Rc::new(row_menu::Popup),
            config: settings::default_path(),
            autostart: Autostart::real(),
            picker: Rc::new(picker::System),
            position: store::ours("LOCALAPPDATA", "position.txt"),
            // SAFETY: a plain call.
            windows_language: Language::of_windows(unsafe { GetUserDefaultUILanguage() }),
            updates: Updates::real(),
            hibernates: system::hibernates(),
            dwm: set_attribute,
        }
    }
}

/// Carries out `action` on a thread of its own: the shell can take a
/// while, and the UAC prompt waits for the user. Switching to a window
/// happens at once, while the launcher is still in front: Windows lets
/// the foreground hand the foreground on, and the launcher hides next.
fn run_in_background(action: &Action, as_admin: bool, done: Done) {
    let action = action.clone();
    if launch::at_once(&action) {
        return carry_out(&action, as_admin, done);
    }
    std::thread::spawn(move || carry_out(&action, as_admin, done));
}

/// Carries out `action`, telling the log when it cannot, then `done`.
fn carry_out(action: &Action, as_admin: bool, done: Done) {
    if let Err(error) = launch::run(action, as_admin) {
        eprintln!("hyalo: cannot run {action:?}: {error}");
    }
    done();
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
    /// How the last check for a newer release went.
    checked: Checked,
    /// The newer release GitHub named, and how far installing it came.
    update: Option<Update>,
    /// What the launcher is drawn over: the glass DWM gave it, or not.
    behind: Behind,
}

/// A row's menu, made ready to open: what opens it, its lines, where it
/// opens, in screen pixels, and the item of the row it is for.
struct Opening {
    menu: Rc<dyn RowMenu>,
    lines: Vec<row_menu::Line>,
    at: POINT,
    item: ItemId,
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
        self.check_for_update();
        self.catalog.system = system_rows(self.language(), self.services.hibernates);
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
    /// before a refused hotkey, and both before a newer release.
    fn notice(&self) -> Option<Notice> {
        let problem = self.problem.clone().map(Notice::Config);
        let kept = self.hotkey();
        let refused = || self.refused.map(|wanted| Notice::Hotkey { wanted, kept });
        let update = || {
            let current = self.services.updates.as_ref()?.current;
            let update = self.offered()?.clone();
            Some(Notice::Update { update, current })
        };
        problem.or_else(refused).or_else(update)
    }

    /// The newer release hyalo tells of: none while checking is off.
    fn offered(&self) -> Option<&Update> {
        self.update
            .as_ref()
            .filter(|_| self.config.check_for_updates)
    }

    /// Asks GitHub for the latest release, on a thread of its own, when
    /// hyalo updates itself, checking is on and a check is due.
    fn check_for_update(&mut self) {
        let Some(updates) = &self.services.updates else {
            return;
        };
        if !self.config.check_for_updates || !self.checked.due(now().0) {
            return;
        }
        self.checked = Checked::Asking;
        let latest = updates.latest.clone();
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            let answer = latest().map(|json| Release::parse(&json));
            hand_over(&sender, window, Found::Latest(answer));
        });
    }

    /// Takes in GitHub's answer: a release newer than this build is
    /// offered; a check that failed is tried again later.
    fn take_latest(&mut self, answer: std::result::Result<Option<Release>, String>) {
        let at = now().0;
        self.checked = match &answer {
            Ok(_) => Checked::Answered(at),
            Err(error) => {
                eprintln!("hyalo: cannot ask GitHub for the latest release: {error}");
                Checked::Failed(at)
            }
        };
        if let (Ok(Some(latest)), Some(updates)) = (answer, &self.services.updates) {
            self.update = Update::after_check(self.update.take(), latest, updates.current);
        }
        self.tell_update();
    }

    /// Installs the newer release on a thread of its own, unless it is
    /// being installed already.
    fn install(&mut self) {
        let (Some(updates), Some(update)) = (&self.services.updates, &mut self.update) else {
            return;
        };
        if update.progress == Progress::Installing {
            return;
        }
        update.progress = Progress::Installing;
        let (release, installer) = (update.release.clone(), updates.installer.clone());
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            let installed = update::install(&release, &installer);
            hand_over(&sender, window, Found::Installed(installed));
        });
        self.tell_update();
    }

    /// How installing went: once the new build is in place and started,
    /// this one closes, and the new one, which waits for that, takes
    /// over; a failure is told, and this build runs on.
    fn take_installed(&mut self, installed: std::result::Result<(), Failure>) {
        let Err(failure) = installed else {
            return post(self.hwnd, WM_CLOSE);
        };
        eprintln!("hyalo: the update did not install: {failure:?}");
        if let Some(update) = &mut self.update {
            update.progress = Progress::Failed(failure);
        }
        self.tell_update();
    }

    /// Tells what is known of a newer release: in the tray, and in the
    /// settings window if it is open.
    fn tell_update(&mut self) {
        self.show_tray();
        self.show_versions();
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
        tray::menu(
            self.hotkey(),
            self.starts(),
            self.offered(),
            self.language(),
        )
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
            self.run_action(&Action::Edit(path));
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
            None => {
                let renderer = Renderer::new(self.hwnd, width, height, dpi, self.behind)?;
                self.renderer = Some(renderer);
            }
        }
        self.redraw()?;
        // The picture first, then the window: growing, it never shows
        // glass without the picture for a frame.
        self.put_window()
    }

    /// Puts the window where it goes, at the size of what it lists,
    /// leaving it where it is among the windows: lifted on every refresh,
    /// it would cover a row's menu (see `show`).
    fn put_window(&self) -> Result<()> {
        let rect = self.rect();
        unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOACTIVATE | SWP_NOZORDER,
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
        let part = self.part_at(point);
        if !matches!(part, Part::Row(_)) {
            self.calm()?;
        }
        let (pressed, edited) = match part {
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
    fn release(&mut self, point: POINT) -> Result<bool> {
        match self.pressed.take() {
            Some(Press::Row(row)) => self.open(row, point),
            Some(Press::Drag(_)) => {
                self.keep_position();
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Opens row `row` if the button came up over it at `point`; whether
    /// it did.
    fn open(&mut self, row: usize, point: POINT) -> Result<bool> {
        if self.row_under(point) != Some(row) {
            return Ok(false);
        }
        self.results.select(row);
        let asked = self.enter(Command::Open { as_admin: false })?;
        Ok(!asked)
    }

    /// Enter on the selected row: carries out what the row asked about,
    /// if it asks; else what `command` does with it (opens it), unless
    /// that asks first: then the row asks, in place. Whether it asked: the
    /// launcher stays, waiting for the next Enter.
    fn enter(&mut self, command: Command) -> Result<bool> {
        let Some((item, (wanted, as_admin))) = self.wanted(command) else {
            return Ok(false);
        };
        match self.results.confirm(wanted) {
            Some(action) => {
                self.perform(item, action, as_admin);
                Ok(false)
            }
            None => self.redraw().map(|()| true),
        }
    }

    /// Shift+Delete: cuts the text selected in the bar, as in any field;
    /// with none, the selected app asks to be uninstalled.
    fn uninstall(&mut self) -> Result<()> {
        if !self.query.selected().is_empty() {
            return self.edit(field::Key::Cut).map(drop);
        }
        self.ask_to_uninstall()
    }

    /// The selected app asks, in place, to be uninstalled, if hyalo knows
    /// what uninstalls it. Only the next Enter uninstalls.
    fn ask_to_uninstall(&mut self) -> Result<()> {
        let picked = self.results.picked().map(|row| &row.item.id);
        let found = picked
            .and_then(|id| self.catalog.uninstallers.get(id))
            .cloned();
        match found.is_some_and(|found| self.results.ask(Action::Uninstall(found))) {
            true => self.redraw(),
            false => Ok(()),
        }
    }

    /// Makes ready the menu of the row at `clicked`, a point of the
    /// screen, or with `None`, from the keys, of the selected row: selects
    /// the row and calls off its asking, as any key or click but Enter
    /// does. `None` off the rows, or with none.
    fn row_menu(&mut self, clicked: Option<POINT>) -> Result<Option<Opening>> {
        let Some((row, at)) = self.menu_row(clicked) else {
            return Ok(None);
        };
        self.results.select(row);
        self.results.calm();
        self.redraw()?;
        let item = &self.results.rows()[row].item;
        let uninstalls = self.catalog.uninstallers.contains_key(&item.id);
        Ok(Some(Opening {
            menu: Rc::clone(&self.services.row_menu),
            lines: row_menu::lines(item, uninstalls, self.language()),
            at,
            item: item.id.clone(),
        }))
    }

    /// The row a menu opens for, and where, in screen pixels: the row at
    /// `clicked`, there; with `None`, the selected row, under it.
    fn menu_row(&self, clicked: Option<POINT>) -> Option<(usize, POINT)> {
        match clicked {
            Some(at) => Some((self.row_under(to_client(self.hwnd, at))?, at)),
            None => {
                let row = self.results.picked().map(|_| self.results.selected())?;
                let scale = layout::scale(self.placement.dpi);
                let (x, y) = layout::menu_spot(row);
                let spot = POINT {
                    x: (x * scale) as i32,
                    y: (y * scale) as i32,
                };
                Some((row, to_screen(self.hwnd, spot)))
            }
        }
    }

    /// The menu of `item`'s row closed, leaving the pointer at `pointer`
    /// on the screen, if known. It went there over the menu, unseen, and
    /// Windows tells of it next as a move: no move of the user's (see
    /// `hover`), so it must not take the selection, nor call the asking
    /// off. Whether the selected row is still `item`'s.
    fn menu_closed(&mut self, item: &ItemId, pointer: Option<POINT>) -> bool {
        self.pointer = pointer.map(|at| to_client(self.hwnd, at));
        self.results
            .picked()
            .is_some_and(|row| row.item.id == *item)
    }

    /// Stops the selected row asking, as anything but Enter does, and
    /// shows it.
    fn calm(&mut self) -> Result<()> {
        match self.results.calm() {
            true => self.redraw(),
            false => Ok(()),
        }
    }

    /// Carries out what `command` does with the selected row and notes
    /// the pick; whether it did anything.
    fn act(&mut self, command: Command) -> bool {
        let Some((item, (action, as_admin))) = self.wanted(command) else {
            return false;
        };
        self.perform(item, action, as_admin);
        true
    }

    /// The selected row's item, what `command` does with it and whether
    /// as administrator; `None` with no row, or when `command` does
    /// nothing with it.
    fn wanted(&self, command: Command) -> Option<(Item, (Action, bool))> {
        let item = self.results.picked()?.item.clone();
        let wanted = keys::action(command, &item)?;
        Some((item, wanted))
    }

    /// Carries out `action` for `item`, as administrator or not, and notes
    /// the pick.
    fn perform(&mut self, item: Item, action: Action, as_admin: bool) {
        match action {
            // Later, from the message loop: opening a window here, with
            // the app state borrowed, would lose its first messages.
            Action::Settings => post(self.hwnd, WM_SETTINGS),
            action => (self.services.run)(&action, as_admin, self.after(&action)),
        }
        self.note(item);
    }

    /// What to do once `action` was carried out: after an uninstall, tell
    /// the window it ended, so it looks for the apps again; after anything
    /// else, nothing.
    fn after(&self, action: &Action) -> Done {
        if !matches!(action, Action::Uninstall(_)) {
            return nothing();
        }
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        Box::new(move || hand_over(&sender, window, Found::UninstallEnded))
    }

    /// Carries out `action` on the runner, not as administrator, with
    /// nothing to do once it is done.
    fn run_action(&self, action: &Action) {
        (self.services.run)(action, false, nothing());
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
                self.run_action(&Action::CopyText(selected.to_string()));
                false
            }
        }
    }

    /// Takes the selection out of the query onto the clipboard.
    fn cut(&mut self) -> Edited {
        let Some(text) = self.query.cut() else {
            return Edited::Nothing;
        };
        self.run_action(&Action::CopyText(text));
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
    /// leave a pick behind; nor is a web search, for the same reasons,
    /// nor a window, whose handle means nothing once it closes.
    fn note(&mut self, item: Item) {
        let kind = item.kind;
        if matches!(
            kind,
            Kind::Calculator | Kind::Web | Kind::Window | Kind::Notice
        ) {
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

    /// The rows for the query: the open windows after `>`; else its
    /// answer, if it has one, then the best items and files. The words
    /// match apps and names; the filters among them narrow the files and
    /// folders alone, picked ones too. A query of filters alone lists
    /// files and folders by place. The answer reads the whole text.
    fn listed(&self) -> Results {
        let text = self.query.text();
        if let Some(filter) = hyalo_core::switcher::filter(text) {
            return Results::windows(filter, &self.catalog.windows);
        }
        let query = Query::parse(text, clock());
        let picked = self.picked(&query.filters);
        let (frecency, places, files) = (&self.frecency, &self.places, self.file_hits(&query));
        let found = match query.words.is_empty() && !query.filters.is_empty() {
            true => Results::filtered(text, picked, files, frecency, places, now()),
            false => {
                let catalog = &self.catalog;
                let items = catalog.items.iter().chain(&catalog.system).chain(picked);
                Results::search(&query.words, items, files, frecency, places, now())
            }
        };
        let answer = match text.is_empty() {
            true => self.notice_row(),
            false => {
                let rates = self.catalog.rates.as_ref();
                let engines = self.config.engines(self.language());
                results::answer(text, rates, &engines, self.language())
            }
        };
        found.answered(answer)
    }

    /// The picked files and folders `filters` lets through.
    fn picked(&self, filters: &Filters) -> Vec<&Item> {
        match filters.is_empty() {
            true => self.catalog.picked.iter().collect(),
            false => self
                .services
                .files
                .let_through(filters, &self.catalog.picked),
        }
    }

    /// The notice's row, when there is a config the settings window can
    /// change.
    fn notice_row(&self) -> Option<Item> {
        self.services.config.as_ref()?;
        Some(self.notice()?.item(self.language()))
    }

    /// The files and folders the index finds for the query: enough to
    /// fill the list even when every picked file is among them.
    fn file_hits(&self, query: &Query) -> Vec<Ranked> {
        if query.words.is_empty() && query.filters.is_empty() {
            return Vec::new();
        }
        let enough = MAX_ROWS + self.catalog.picked.len();
        let files = &self.services.files;
        files.search(&query.words, &query.filters, &self.places, enough)
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
        for index in 0..self.services.sources.len() {
            let trusted = self.services.sources[index].trusted;
            if catalog::stale(self.found_at[index], now, trusted) {
                self.found_at[index] = Some(now);
                self.find(index);
            }
        }
    }

    /// Looks for the apps again now, an uninstaller having ended, and once
    /// more at the next show: an uninstaller may hand its work to a copy
    /// of itself and end before the app is gone.
    fn look_again_for_apps(&mut self) {
        for index in 0..self.services.sources.len() {
            if self.services.sources[index].apps {
                self.found_at[index] = None;
                self.find(index);
            }
        }
    }

    /// Starts source `index`'s provider on a thread of its own.
    fn find(&self, index: usize) {
        let request = Request {
            icon_size: layout::icon_pixels(self.placement.dpi),
            known: self.catalog.icons.keys().cloned().collect(),
        };
        let provider = self.services.sources[index].provider.clone();
        let (sender, window) = (self.found.0.clone(), self.hwnd.0 as isize);
        let find = move || provider(&request, &|found| hand_over(&sender, window, found));
        std::thread::spawn(find);
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
                Found::Latest(answer) => self.take_latest(answer),
                Found::Installed(installed) => self.take_installed(installed),
                Found::UninstallEnded => self.look_again_for_apps(),
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
    let row = |(index, listed): (usize, &'a Listed)| Row {
        id: &listed.item.id,
        title: &listed.item.title,
        subtitle: results.subtitle(index, language),
        highlights: utf16_ranges(&listed.item.title, &listed.matched),
        icon: icons.get(&listed.item.id),
        glyph: results::glyph(listed.item.kind, &listed.item.action),
    };
    Scene {
        query: query.text(),
        placeholder: language.say(Text::Placeholder),
        caret: query.caret_utf16(),
        selection: query.selection_utf16(),
        rows: results.rows().iter().enumerate().map(row).collect(),
        selected: results.selected(),
        asking: results.asking(),
    }
}

/// Windows' own settings and the system commands, in `language`; Hibernate
/// only when Windows `hibernates`.
fn system_rows(language: Language, hibernates: bool) -> Vec<Item> {
    let mut rows = system_settings::items(language);
    rows.extend(system_commands::items(language, hibernates));
    rows
}

/// What `GetTimeZoneInformation` answers in summer time
/// (`TIME_ZONE_ID_DAYLIGHT`, in a module of its own in the windows crate).
const DAYLIGHT: u32 = 2;

/// Now, and how far local time is ahead of UTC, for filters such as
/// `mod:today`.
fn clock() -> Clock {
    let mut zone = TIME_ZONE_INFORMATION::default();
    // SAFETY: Windows fills the struct, which is ours.
    let daylight = unsafe { GetTimeZoneInformation(&mut zone) } == DAYLIGHT;
    let extra = if daylight {
        zone.DaylightBias
    } else {
        zone.StandardBias
    };
    Clock {
        now: now().0 as i64,
        // UTC is local time plus the bias, in minutes.
        offset: -i64::from(zone.Bias + extra) * 60,
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
/// open, and runs until the window closes. Once it knows whether the
/// window opened, it tidies up after an update, or, when it is a new
/// build that could not start, goes back to the old one.
pub fn run(start: Start, replace: bool) -> Result<()> {
    let opened = begin(Services::real(), start);
    update::settle_running(opened.is_ok(), replace);
    opened?;
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
        .and_then(|path| settings::at_start(path, language));
    let position = services.position.as_deref().and_then(read_position);
    let behind = style_window(hwnd, services.dwm);
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
            checked: Checked::Never,
            update: None,
            behind,
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

/// Asks DWM for the launcher's look: dark, round corners, no border,
/// glass behind it; says what it is drawn over. An older Windows refuses
/// what it does not know (Windows 10 all but the dark, Windows 11 before
/// 22H2 the glass), and the launcher does without.
fn style_window(hwnd: HWND, dwm: Dwm) -> Behind {
    let _ = dwm(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        BOOL::from(true).0 as u32,
    );
    let _ = dwm(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0 as u32);
    let _ = dwm(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
    // No DwmExtendFrameIntoClientArea: the window has no redirection
    // bitmap, so nothing covers the backdrop in the client area anyway.
    Behind::of(dwm(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        DWMSBT_TRANSIENTWINDOW.0 as u32,
    ))
}

/// DWM's own `Dwm`.
fn set_attribute(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: u32) -> Result<()> {
    // SAFETY: `value` is a live u32, and the size passed is its size.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
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
            // The app goes with its window, its renderer too. Left on the
            // thread until the process exits, the D3D device would be
            // released only after Windows has stopped every other thread,
            // and that release ends the process with 0x87A instead of 0.
            if let Some(mut app) = take_app() {
                app.close();
            }
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
        // Windows makes it of a right click (WM_RBUTTONUP), Shift+F10 and
        // the Menu key.
        WM_CONTEXTMENU => Some(on_context_menu(hwnd, client_point(lparam))),
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

/// Where `WM_CONTEXTMENU` says it came from the keys, not a click.
const FROM_KEYS: POINT = POINT { x: -1, y: -1 };

/// A right click at `at`, a point of the screen, or the Menu key or
/// Shift+F10 (`FROM_KEYS`): opens the menu of the row clicked, or of the
/// selected row, and carries out what is picked. The menu runs a message
/// loop of its own, so the app state is not held while it is open; the
/// list may change meanwhile, so what is picked is done only to the row
/// the menu was for, if it is still selected.
fn on_context_menu(hwnd: HWND, at: POINT) -> Result<()> {
    let clicked = Some(at).filter(|&at| at != FROM_KEYS);
    let opening = with_app(|app| app.row_menu(clicked)).transpose()?;
    let Some(opening) = opening.flatten() else {
        return Ok(());
    };
    let closed = opening.menu.open(hwnd, &opening.lines, opening.at);
    let still = with_app(|app| app.menu_closed(&opening.item, closed.pointer)) == Some(true);
    match closed.picked.filter(|_| still) {
        Some(command) => on_picked(hwnd, command),
        None => Ok(()),
    }
}

/// Carries out what was picked from a row's menu: what its keys do, but
/// Uninstall… only asks, even with text selected in the bar, which
/// Shift+Delete would cut.
fn on_picked(hwnd: HWND, command: Command) -> Result<()> {
    match command {
        Command::Open { .. } => on_enter(hwnd, command),
        Command::Uninstall => with_app(App::ask_to_uninstall).unwrap_or(Ok(())),
        command => {
            act_on_row(hwnd, command);
            Ok(())
        }
    }
}

/// Carries out what `command` does with the selected row, hiding the
/// launcher if it did anything.
fn act_on_row(hwnd: HWND, command: Command) {
    if with_app(|app| app.act(command)) == Some(true) {
        hide(hwnd);
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
        tray::Command::Install => {
            with_app(App::install);
            Ok(())
        }
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

/// Asks the launcher that runs, perhaps another build in another process,
/// to end, as an update's old build does once it has started the new
/// one: it saves the picks and takes its icon away. Not tested: on the
/// hidden desktop it would reach other tests' launchers.
pub fn close_running() {
    // SAFETY: a plain call.
    if let Ok(hwnd) = unsafe { FindWindowW(WINDOW_CLASS, PCWSTR::null()) } {
        post(hwnd, WM_CLOSE);
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
            if with_app(|app| app.release(point)).transpose()? == Some(true) {
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
    let shape = match with_app(|app| app.part_at(to_client(hwnd, at))) {
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
        app.check_for_update();
        app.search();
        app.present()?;
        app.pointer = pointer_over(hwnd);
        Ok(())
    })
    .unwrap_or(Ok(()))?;
    // Shown and in front, it is on top of the windows that stay on top.
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
    match keys::command(key, held) {
        Some(command @ Command::Open { .. }) => on_enter(hwnd, command),
        command => {
            with_app(App::calm).unwrap_or(Ok(()))?;
            on_other_key(hwnd, (key, held), command)
        }
    }
}

/// Any key but Enter, once the selected row stopped asking: moves the
/// selection, hides, acts on the row, or edits the query.
fn on_other_key(
    hwnd: HWND,
    (key, held): (VIRTUAL_KEY, Held),
    command: Option<Command>,
) -> Result<()> {
    let moved = match command {
        Some(Command::Up) => with_app(|app| app.move_selection(Results::up)),
        Some(Command::Down) => with_app(|app| app.move_selection(Results::down)),
        Some(Command::Hide) => {
            hide(hwnd);
            None
        }
        Some(Command::Uninstall) => with_app(App::uninstall),
        Some(command) => {
            act_on_row(hwnd, command);
            None
        }
        None => return on_field_key(hwnd, field::key(key, held)),
    };
    moved.unwrap_or(Ok(()))
}

/// Enter: opens the selected row and hides the launcher, even with
/// nothing to open, unless the row asked first.
fn on_enter(hwnd: HWND, command: Command) -> Result<()> {
    let asked = with_app(|app| app.enter(command)).transpose()?;
    if asked != Some(true) {
        hide(hwnd);
    }
    Ok(())
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

/// `at`, a point of the screen, in `hwnd`'s client pixels.
fn to_client(hwnd: HWND, at: POINT) -> POINT {
    let mut point = at;
    // SAFETY: a plain call into a live POINT.
    let _ = unsafe { ScreenToClient(hwnd, &mut point) };
    point
}

/// `at`, in `hwnd`'s client pixels, on the screen.
fn to_screen(hwnd: HWND, at: POINT) -> POINT {
    let mut point = at;
    // SAFETY: a plain call into a live POINT.
    let _ = unsafe { ClientToScreen(hwnd, &mut point) };
    point
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

/// Takes the app state off the thread, unless it is borrowed further up
/// the stack or already gone.
fn take_app() -> Option<App> {
    APP.try_with(|cell| cell.try_borrow_mut().ok()?.take())
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use hyalo_core::config::DEFAULT;
    use hyalo_core::item::{Action, Item, Kind};
    use hyalo_core::uninstall::Uninstall;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
    use windows::Win32::Graphics::Gdi::MonitorFromWindow;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SetKeyboardState, VK_A, VK_BACK, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F10,
        VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP, VK_V, VK_X,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EndMenu, FindWindowExW, GW_HWNDPREV, GetClientRect, GetCursor, GetWindow, GetWindowRect,
        IsWindow, KillTimer, PM_QS_POSTMESSAGE, PM_REMOVE, PeekMessageW, SW_SHOWNOACTIVATE,
        SendMessageW, SetTimer, WA_ACTIVE, WM_CLOSE, WM_NULL, WM_QUIT, WM_RBUTTONDOWN,
        WM_RBUTTONUP,
    };

    use super::*;
    use crate::autostart;
    use crate::render::Frame;
    use crate::scratch::Scratch;
    use crate::test_desktop::{
        exit_code_on_hidden_desktop, in_own_window_station, on_hidden_desktop,
    };
    use std::path::Path;

    /// A hyalo started as the screen locked showed Windows' settings
    /// without icons all morning: the shell gave none at start, and the
    /// provider was trusted for ever. It skips the icons it has, so asking
    /// again as often as for the apps costs next to nothing.
    #[test]
    fn icons_of_settings_the_shell_did_not_give_are_asked_for_again_later() {
        let trusted = settings_icons().trusted;
        let found = Instant::now();
        let soon = found + Duration::from_secs(60);
        assert!(
            !catalog::stale(Some(found), soon, trusted),
            "not at every show"
        );
        let later = found + catalog::REFRESH_AFTER;
        assert!(catalog::stale(Some(found), later, trusted), "asked again");
    }

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: "App".to_string(),
            keywords: Vec::new(),
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

    /// Rows' menus as a test keeps them: the lines of each menu opened
    /// and where, what happens while the next one is open, what the user
    /// picks from it, and where on the screen they leave the pointer.
    #[derive(Default)]
    pub(super) struct Choices {
        pub(super) opened: RefCell<Vec<(Vec<row_menu::Line>, POINT)>>,
        pub(super) meanwhile: RefCell<Option<Box<dyn FnOnce()>>>,
        pub(super) pick: RefCell<Option<Command>>,
        pub(super) left_at: RefCell<Option<POINT>>,
    }

    impl RowMenu for Choices {
        fn open(&self, _: HWND, lines: &[row_menu::Line], at: POINT) -> row_menu::Closed {
            self.opened.borrow_mut().push((lines.to_vec(), at));
            if let Some(meanwhile) = self.meanwhile.borrow_mut().take() {
                meanwhile();
            }
            row_menu::Closed {
                picked: self.pick.borrow_mut().take(),
                pointer: *self.left_at.borrow(),
            }
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
            sources: vec![Source::apps(provider, catalog::REFRESH_AFTER)],
            files: Files::default(),
            icons,
            places: Places::default(),
            run: Box::new(move |action: &Action, as_admin, done: Done| {
                noted.borrow_mut().push((action.clone(), as_admin));
                done();
            }),
            picks,
            hotkeys: Box::new(Rc::new(Keyboard::default())),
            tray: Rc::new(Taskbar::default()),
            row_menu: Rc::new(Choices::default()),
            config: None,
            autostart: None,
            picker: Rc::new(Chooser::default()),
            position: None,
            windows_language: Language::English,
            updates: None,
            hibernates: true,
            dwm: set_attribute,
        };
        (services, ran)
    }

    /// The same, finding files by walking what `roots` gives.
    pub(super) fn walking(roots: Roots, picks: Option<PathBuf>) -> (Services, Ran) {
        let (mut services, ran) = services(listing(&[]).0, picks);
        let (files, writer) = Files::new();
        services.files = files;
        let excluded = Excluded::default();
        services.sources.push(Source::new(
            files::provider(writer, roots, None, excluded.clone()),
            files::REFRESH_AFTER,
            Some(excluded),
        ));
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

    /// How long a test waits for what the launcher's threads hand over.
    /// Under mutation testing eight suites run at once beside other
    /// sessions' runs, four times slower than alone, and five seconds
    /// were once not enough for a check and an install on threads.
    const PATIENCE: Duration = Duration::from_secs(15);

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
        /// would; fails after `PATIENCE`, naming the line that waited.
        #[track_caller]
        pub(super) fn pump_until(&self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + PATIENCE;
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

        /// The right button down and up at (x, y) in DIPs, which Windows
        /// turns into `WM_CONTEXTMENU` at that point of the screen.
        pub(super) fn right_click(&self, x: f32, y: f32) {
            self.mouse(WM_RBUTTONDOWN, x, y);
            self.mouse(WM_RBUTTONUP, x, y);
        }

        /// The Menu key: what Windows sends for it, `WM_CONTEXTMENU` at
        /// (-1, -1). Windows makes that of the key pressed for real only,
        /// not of a `WM_KEYUP` sent or posted.
        pub(super) fn menu_key(&self) {
            let hwnd = Some(WPARAM(self.0.0 as usize));
            unsafe { SendMessageW(self.0, WM_CONTEXTMENU, hwnd, Some(LPARAM(-1))) };
        }

        /// Shift+F10, which Windows turns into the same `WM_CONTEXTMENU`.
        pub(super) fn shift_f10(&self) {
            hold(&[VK_SHIFT]);
            self.send(WM_SYSKEYDOWN, usize::from(VK_F10.0));
            hold(&[]);
        }

        /// The point (x, y) in DIPs of the launcher, in screen pixels.
        pub(super) fn on_screen(&self, x: f32, y: f32) -> POINT {
            let scale = layout::scale(self.placement().dpi);
            let rect = self.rect();
            POINT {
                x: rect.x + (x * scale) as i32,
                y: rect.y + (y * scale) as i32,
            }
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

        /// A pixel of the glass right of the query, where nothing but the
        /// launcher's tint is drawn: blue, green, red and alpha.
        pub(super) fn glass_pixel(&self) -> [u8; 4] {
            let frame = self.frame();
            frame.pixel(frame.width * 19 / 20, frame.height / 2)
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
            let [.., alpha] = launcher.glass_pixel();
            assert!(alpha < 255, "the glass shows through: {alpha}");
        });
    }

    /// DWM as Windows 10 answers: it refuses the attributes Windows 11
    /// brought, round corners, the border's colour and the system
    /// backdrop, as it refuses any attribute it does not know.
    pub(super) fn windows_10(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: u32) -> Result<()> {
        match attribute {
            DWMWA_WINDOW_CORNER_PREFERENCE | DWMWA_BORDER_COLOR | DWMWA_SYSTEMBACKDROP_TYPE => {
                Err(windows::Win32::Foundation::E_INVALIDARG.into())
            }
            _ => set_attribute(hwnd, attribute, value),
        }
    }

    #[test]
    fn where_dwm_gives_no_glass_it_opens_on_a_solid_background() {
        on_hidden_desktop(|| {
            let (mut services, _) = services(listing(&[]).0, None);
            services.dwm = windows_10;
            let launcher = Launcher::serving(services);
            assert_eq!(launcher.dwm(DWMWA_USE_IMMERSIVE_DARK_MODE), 1);
            let [.., alpha] = launcher.glass_pixel();
            assert_eq!(alpha, 255, "the desktop does not show through");
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
    fn it_shows_on_top_of_the_windows_that_stay_on_top() {
        on_hidden_desktop(|| {
            let launcher = Launcher::open();
            launcher.hotkey();
            let (ex, style) = (WS_EX_TOPMOST | WS_EX_TOOLWINDOW, WS_POPUP);
            let other = unsafe {
                CreateWindowExW(
                    ex,
                    w!("STATIC"),
                    w!(""),
                    style,
                    0,
                    0,
                    50,
                    50,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .expect("a window that stays on top");
            let _ = unsafe { ShowWindow(other, SW_SHOWNOACTIVATE) };
            assert!(above(other, launcher.0), "on top of the hidden launcher");
            launcher.hotkey();
            assert!(above(launcher.0, other), "the launcher shows over it");
            unsafe { DestroyWindow(other) }.expect("destroy it");
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
                tray::menu(Some(Hotkey::default()), None, None, Language::English)
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
                tray::menu(Some(Hotkey::default()), Some(true), None, Language::English)
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
                tray::menu(Some(hotkey("Ctrl+Shift+K")), None, None, Language::English)
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
            // Letters nothing is found by, so the bar stays alone.
            launcher.type_text("zqx");
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
    fn hyalo_exits_with_zero_once_its_shown_window_has_closed() {
        let name = "window::tests::hyalo_exits_with_zero_once_its_shown_window_has_closed";
        let code = exit_code_on_hidden_desktop(name, || {
            // Shown: drawn, so it has a renderer.
            let hwnd = open(services(listing(&[]).0, None).0, true).expect("open the launcher");
            unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) };
        });
        assert_eq!(code, 0, "exit code {code:#X}");
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
            // Titles no Windows setting matches as well, so three rows.
            let launcher = Launcher::listing(&["zq1", "zq2", "zq3"]);
            launcher.type_text("zq");
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

    /// A windows source that lists `titles` as open windows of Notepad,
    /// front first, with handles 1, 2, ..., each with an icon; and how
    /// many times it looked.
    fn windowing(titles: &[&str]) -> (Source, Arc<AtomicUsize>) {
        let items: Vec<Item> = titles
            .iter()
            .zip(1..)
            .map(|(title, window)| hyalo_core::switcher::item(window, title, "Notepad"))
            .collect();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let provider: Provider = Arc::new(move |request: &Request, report: &dyn Fn(Found)| {
            counted.fetch_add(1, Ordering::SeqCst);
            report(Found::Windows(items.clone()));
            for item in &items {
                report(Found::Icon(item.id.clone(), red(request.icon_size)));
            }
        });
        let source = Source::new(provider, Duration::ZERO, None);
        (source, runs)
    }

    const WINDOWS: [&str; 3] = ["notes.txt - Notepad", "todo.txt - Notepad", "Notepad tips"];

    /// A launcher listing Notepad and the open `WINDOWS`, once it has
    /// them; what it was asked to run, and how often it looked for them.
    fn switching(picks: Option<PathBuf>) -> (Launcher, Ran, Arc<AtomicUsize>) {
        let (mut services, ran) = services(listing(&["Notepad"]).0, picks);
        let (source, runs) = windowing(&WINDOWS);
        services.sources.push(source);
        let launcher = Launcher::serving(services);
        launcher.pump_until(|app| app.catalog.icons.len() == 1 + WINDOWS.len());
        (launcher, ran, runs)
    }

    #[test]
    fn the_prefix_lists_the_open_windows_front_first_with_their_icons() {
        on_hidden_desktop(|| {
            let (launcher, _, _) = switching(None);
            launcher.type_text(">");
            assert_eq!(listed(), WINDOWS);
            assert_eq!(first_row(), (WINDOWS[0].to_string(), "Notepad".to_string()));
            let icons = with_app(|app| {
                let rows = app.results.rows().iter();
                rows.filter(|row| app.catalog.icons.contains_key(&row.item.id))
                    .count()
            });
            assert_eq!(icons, Some(WINDOWS.len()));
            launcher.type_text(" tips");
            assert_eq!(listed(), ["Notepad tips"], "the windows alone, not the app");
        });
    }

    #[test]
    fn enter_switches_to_the_window_the_filter_found_and_remembers_no_pick() {
        on_hidden_desktop(|| {
            let picks = PicksFile::new("window-switcher");
            let (launcher, ran, _) = switching(Some(picks.0.clone()));
            launcher.type_text(">todo");
            assert_eq!(listed(), ["todo.txt - Notepad"]);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [(Action::Switch(2), false)]);
            assert!(!launcher.visible());
            assert!(!picks.0.exists(), "a window is not a pick");
        });
    }

    #[test]
    fn what_reads_as_a_filter_after_the_prefix_finds_windows_by_title() {
        on_hidden_desktop(|| {
            let (mut services, _) = services(listing(&["Notepad"]).0, None);
            let titles = ["ext:pdf - Search", "notes.txt - Notepad"];
            services.sources.push(windowing(&titles).0);
            let launcher = Launcher::serving(services);
            launcher.pump_until(|app| app.catalog.windows.len() == titles.len());
            launcher.type_text("> ext:pdf");
            assert_eq!(listed(), ["ext:pdf - Search"]);
        });
    }

    #[test]
    fn the_windows_are_looked_for_again_at_every_show() {
        on_hidden_desktop(|| {
            let (launcher, _, runs) = switching(None);
            let before = runs.load(Ordering::SeqCst);
            launcher.hotkey();
            launcher.hotkey();
            launcher.pump_until(|_| runs.load(Ordering::SeqCst) == before + 1);
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

    /// The first row's title and second line, as the window shows them.
    fn first_shown() -> (String, String) {
        let shown = with_app(|app| {
            let row = &app.results.rows()[0];
            let said = app.results.subtitle(0, app.language());
            (row.item.title.clone(), said.to_string())
        });
        shown.expect("app state")
    }

    /// Whether the first row's second line shows in amber, the asking
    /// colour, in the frame on screen: some of its pixels much redder
    /// than blue.
    fn asks_in_amber(launcher: &Launcher) -> bool {
        let frame = launcher.frame();
        let scale = layout::scale(launcher.placement().dpi);
        let top = layout::row_top(0) + 25.0;
        let ys = (top * scale) as usize..((top + 18.0) * scale) as usize;
        let xs = (60.0 * scale) as usize..(300.0 * scale) as usize;
        ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
            .any(|(x, y)| {
                let [blue, _, red, _] = frame.pixel(x, y);
                red > blue.saturating_add(80)
            })
    }

    fn restart() -> (Action, bool) {
        (Action::System(system_commands::Command::Restart), false)
    }

    #[test]
    fn a_windows_setting_is_listed_by_name_and_enter_opens_it() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("bluetooth");
            let bluetooth = ("Bluetooth & devices".to_string(), "Settings".to_string());
            assert_eq!(first_shown(), bluetooth);
            launcher.press(VK_RETURN);
            let opened = Action::Open("ms-settings:bluetooth".to_string());
            assert_eq!(*ran.borrow(), [(opened, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn a_control_panel_item_is_found_by_its_russian_name_too() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("переменные среды");
            let names = (
                "Environment Variables".to_string(),
                "Control Panel".to_string(),
            );
            assert_eq!(first_shown(), names);
            launcher.press(VK_RETURN);
            let variables = Action::Launch {
                program: "rundll32.exe".to_string(),
                arguments: "sysdm.cpl,EditEnvironmentVariables".to_string(),
            };
            assert_eq!(*ran.borrow(), [(variables, false)]);
        });
    }

    #[test]
    fn enter_on_restart_asks_in_place_and_the_next_enter_restarts() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("restart");
            assert_eq!(first_shown().1, "System command");
            assert!(!asks_in_amber(&launcher));
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty(), "not yet");
            assert!(launcher.visible());
            assert_eq!(first_shown().1, "Press Enter again to restart");
            assert!(asks_in_amber(&launcher), "the question, in amber");
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [restart()]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn lock_runs_at_the_first_enter() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("lock");
            launcher.press(VK_RETURN);
            let lock = Action::System(system_commands::Command::Lock);
            assert_eq!(*ran.borrow(), [(lock, false)]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn any_key_but_enter_stops_the_asking() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("restart");
            for key in [VK_DOWN, VK_UP, VK_LEFT, VK_BACK] {
                launcher.press(VK_RETURN);
                assert_eq!(first_shown().1, "Press Enter again to restart", "{key:?}");
                launcher.press(key);
                assert_eq!(first_shown().1, "System command", "{key:?}");
            }
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty(), "each Enter asked anew");
            assert!(launcher.visible());
        });
    }

    #[test]
    fn typing_or_hiding_stops_the_asking() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("restar");
            launcher.press(VK_RETURN);
            launcher.type_text("t");
            assert_eq!(first_shown().1, "System command");
            launcher.press(VK_RETURN);
            launcher.press(VK_ESCAPE);
            launcher.hotkey();
            launcher.type_text("restart");
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty());
            assert_eq!(first_shown().1, "Press Enter again to restart");
        });
    }

    #[test]
    fn a_click_on_restart_asks_and_a_second_click_restarts() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&[], None);
            launcher.type_text("restart");
            launcher.click(200.0, middle_of(0));
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
            launcher.click(200.0, middle_of(0));
            assert_eq!(*ran.borrow(), [restart()]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn a_click_in_the_query_stops_the_asking() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&[]);
            launcher.type_text("restart");
            launcher.press(VK_RETURN);
            launcher.click(60.0, layout::BAR_HEIGHT / 2.0);
            assert_eq!(first_shown().1, "System command");
        });
    }

    /// What uninstalls the app `title` in these tests: an MSI, named as
    /// Everything's is.
    fn uninstaller(title: &str) -> Uninstall {
        Uninstall::Program {
            name: format!("{title} 1.4.1.1032 (x64)"),
            program: "msiexec.exe".to_string(),
            arguments: format!("/x {{{title}}}"),
        }
    }

    fn uninstalled(title: &str) -> (Action, bool) {
        (Action::Uninstall(uninstaller(title)), false)
    }

    /// A launcher that lists apps titled `titles`, icons too, and knows
    /// what uninstalls those `uninstallable` names, one at least; and
    /// what it was asked to run.
    fn uninstalling(titles: &[&str], uninstallable: &[&str]) -> (Launcher, Ran) {
        let (services, ran) = uninstallers(titles, uninstallable);
        (knowing(services, uninstallable.len()), ran)
    }

    /// Services that list apps titled `titles`, icons too, and know what
    /// uninstalls those `uninstallable` names; and what the launcher was
    /// asked to run.
    fn uninstallers(titles: &[&str], uninstallable: &[&str]) -> (Services, Ran) {
        let listed = listing(titles).0;
        let found: HashMap<ItemId, Uninstall> = uninstallable
            .iter()
            .map(|title| (app(title).id, uninstaller(title)))
            .collect();
        let provider: Provider = Arc::new(move |request: &Request, report: &dyn Fn(Found)| {
            listed(request, report);
            report(Found::Uninstallers(found.clone()));
        });
        services(provider, None)
    }

    /// A launcher on `services`, once it knows what uninstalls `all` apps.
    fn knowing(services: Services, all: usize) -> Launcher {
        let launcher = Launcher::serving(services);
        launcher.pump_until(|app| app.catalog.uninstallers.len() == all);
        launcher
    }

    /// Shift+Delete, then no key held.
    fn shift_delete(launcher: &Launcher) {
        hold(&[VK_SHIFT]);
        launcher.press(VK_DELETE);
        hold(&[]);
    }

    #[test]
    fn shift_delete_asks_to_uninstall_the_app_and_enter_uninstalls_it() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything", "Notepad"], &["Everything"]);
            launcher.type_text("every");
            assert_eq!(first_shown().1, "App");
            assert!(!asks_in_amber(&launcher));
            shift_delete(&launcher);
            let asks = "Press Enter to uninstall Everything 1.4.1.1032 (x64)";
            assert_eq!(first_shown().1, asks, "names what it uninstalls");
            assert!(asks_in_amber(&launcher), "the question, in amber");
            assert!(ran.borrow().is_empty(), "not yet");
            assert!(launcher.visible());
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [uninstalled("Everything")]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn a_click_on_the_row_asking_to_uninstall_uninstalls() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything"], &["Everything"]);
            launcher.type_text("every");
            shift_delete(&launcher);
            launcher.click(200.0, middle_of(0));
            assert_eq!(*ran.borrow(), [uninstalled("Everything")]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn any_other_key_typing_or_a_click_elsewhere_calls_the_uninstall_off() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything"], &["Everything"]);
            launcher.type_text("every");
            for key in [VK_DOWN, VK_UP, VK_END] {
                shift_delete(&launcher);
                launcher.press(key);
                assert_eq!(first_shown().1, "App", "{key:?}");
            }
            shift_delete(&launcher);
            launcher.type_text("t");
            assert_eq!(first_shown().1, "App", "typing");
            shift_delete(&launcher);
            launcher.click(60.0, layout::BAR_HEIGHT / 2.0);
            assert_eq!(first_shown().1, "App", "a click in the query");
            launcher.press(VK_RETURN);
            assert_eq!(
                *ran.borrow(),
                [opened("Everything", false)],
                "Enter opens it"
            );
        });
    }

    #[test]
    fn shift_delete_with_text_selected_cuts_it_as_in_any_field() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything"], &["Everything"]);
            launcher.search_for("every", 1);
            hold(&[VK_CONTROL]);
            launcher.press(VK_A);
            shift_delete(&launcher);
            let cut = (Action::CopyText("every".to_string()), false);
            assert_eq!(*ran.borrow(), [cut]);
            assert_eq!(launcher.query(), "");
            assert!(!with_app(|app| app.results.asking()).expect("app state"));
        });
    }

    #[test]
    fn shift_delete_where_hyalo_knows_no_uninstaller_does_nothing() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything", "Notepad"], &["Everything"]);
            launcher.type_text("notepad");
            shift_delete(&launcher);
            assert_eq!(first_shown().1, "App");
            launcher.type_text(" xyz");
            shift_delete(&launcher);
            assert!(ran.borrow().is_empty());
            assert!(launcher.visible());
            assert_eq!(launcher.query(), "notepad xyz");
        });
    }

    #[test]
    fn shift_delete_on_a_row_asking_to_restart_asks_nothing_more() {
        on_hidden_desktop(|| {
            let (launcher, ran) = uninstalling(&["Everything"], &["Everything"]);
            launcher.type_text("restart");
            launcher.press(VK_RETURN);
            shift_delete(&launcher);
            assert_eq!(first_shown().1, "System command", "called off");
            launcher.press(VK_RETURN);
            assert!(ran.borrow().is_empty(), "asks anew");
        });
    }

    /// Apps as the shell lists them while an uninstaller works: Everything,
    /// with its uninstaller, and Notepad, until `gone` is set, then
    /// Notepad alone; and how often they were looked for.
    fn installed(gone: Arc<AtomicBool>) -> (Provider, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let provider: Provider = Arc::new(move |request: &Request, report: &dyn Fn(Found)| {
            counted.fetch_add(1, Ordering::SeqCst);
            let gone = gone.load(Ordering::SeqCst);
            let titles: &[&str] = match gone {
                true => &["Notepad"],
                false => &["Everything", "Notepad"],
            };
            let items: Vec<Item> = titles.iter().map(|title| app(title)).collect();
            report(Found::Items(items.clone()));
            let everything = (!gone).then(|| (app("Everything").id, uninstaller("Everything")));
            report(Found::Uninstallers(everything.into_iter().collect()));
            for item in &items {
                report(Found::Icon(item.id.clone(), red(request.icon_size)));
            }
        });
        (provider, runs)
    }

    #[test]
    fn once_the_uninstaller_ends_the_apps_are_looked_for_at_once_and_at_the_next_show() {
        on_hidden_desktop(|| {
            let gone = Arc::new(AtomicBool::new(false));
            let (provider, runs) = installed(gone.clone());
            let (mut services, ran) = services(provider, None);
            // Another source, which lists no apps: an uninstall leaves it.
            let others = Arc::new(AtomicUsize::new(0));
            let counted = others.clone();
            let other: Provider = Arc::new(move |_: &Request, _: &dyn Fn(Found)| {
                counted.fetch_add(1, Ordering::SeqCst);
            });
            services
                .sources
                .push(Source::new(other, catalog::REFRESH_AFTER, None));
            // The runner keeps what to do once it is done: the uninstaller
            // is still at work until the test says it ended.
            let working: Rc<RefCell<Vec<Done>>> = Rc::default();
            let (noted, kept) = (ran.clone(), working.clone());
            services.run = Box::new(move |action: &Action, as_admin, done: Done| {
                noted.borrow_mut().push((action.clone(), as_admin));
                kept.borrow_mut().push(done);
            });
            let launcher = knowing(services, 1);
            launcher.type_text("every");
            shift_delete(&launcher);
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [uninstalled("Everything")]);
            launcher.hotkey();
            launcher.type_text("every");
            assert_eq!(first_shown().0, "Everything", "still there while it works");
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            launcher.press(VK_ESCAPE);
            gone.store(true, Ordering::SeqCst);
            let ended = working.borrow_mut().pop().expect("the uninstall's done");
            ended();
            let listed = |app: &App| {
                !app.catalog
                    .items
                    .iter()
                    .any(|item| item.title == "Everything")
            };
            launcher.pump_until(listed);
            assert_eq!(runs.load(Ordering::SeqCst), 2, "at once, while hidden");
            assert!(with_app(|app| app.catalog.uninstallers.is_empty()).expect("app state"));
            launcher.hotkey();
            launcher.pump_until(|_| runs.load(Ordering::SeqCst) == 3);
            launcher.type_text("every");
            assert!(with_app(|app| app.results.rows().is_empty()).expect("app state"));
            assert_eq!(others.load(Ordering::SeqCst), 1, "the other source, once");
        });
    }

    #[test]
    fn opening_an_app_leaves_the_apps_as_they_were_found() {
        on_hidden_desktop(|| {
            let (launcher, ran) = Launcher::listing_with(&["Notepad"], None);
            launcher.type_text("note");
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [opened("Notepad", false)]);
            drain();
            let found = with_app(|app| app.found_at[0].is_some()).expect("app state");
            assert!(found, "not to be looked for again");
        });
    }

    #[test]
    fn the_runner_is_done_once_it_carried_the_action_out_even_when_that_failed() {
        let (sender, done) = channel();
        let missing = Action::Open(r"shell:AppsFolder\hyalo.test.no-such-app".to_string());
        run_in_background(
            &missing,
            false,
            Box::new(move || sender.send(()).expect("told")),
        );
        done.recv_timeout(PATIENCE)
            .expect("done, on a thread of its own");
        let (sender, done) = channel();
        let gone = Action::Switch(0x7FFF_FFF0);
        run_in_background(
            &gone,
            false,
            Box::new(move || sender.send(()).expect("told")),
        );
        done.try_recv().expect("done at once, on this thread");
    }

    /// A launcher that lists apps titled `titles`, knows what uninstalls
    /// those `uninstallable` names, one at least, and opens rows' menus
    /// through `Choices`; what it was asked to run, and the menus.
    fn with_menus(titles: &[&str], uninstallable: &[&str]) -> (Launcher, Ran, Rc<Choices>) {
        let (mut services, ran) = uninstallers(titles, uninstallable);
        let choices = Rc::new(Choices::default());
        services.row_menu = choices.clone();
        (knowing(services, uninstallable.len()), ran, choices)
    }

    /// The row titled `title`.
    fn row_of(title: &str) -> usize {
        let rows = with_app(|app| titles(app)).expect("app state");
        rows.iter().position(|row| row == title).expect("listed")
    }

    /// The menu of the app `title`'s row, `uninstalls` or not.
    fn menu_of(title: &str, uninstalls: bool) -> Vec<row_menu::Line> {
        row_menu::lines(&app(title), uninstalls, Language::English)
    }

    #[test]
    fn a_right_click_on_a_row_selects_it_and_opens_its_menu_where_it_was_clicked() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything", "Evernote"], &["Everything"]);
            launcher.search_for("ever", 2);
            let (first, second) = (row_of("Everything"), row_of("Evernote"));
            launcher.right_click(200.0, middle_of(second));
            assert_eq!(with_app(|app| app.results.selected()), Some(second));
            let at = launcher.on_screen(200.0, middle_of(second));
            assert_eq!(*choices.opened.borrow(), [(menu_of("Evernote", false), at)]);
            launcher.right_click(300.0, middle_of(first));
            let at = launcher.on_screen(300.0, middle_of(first));
            let opened = choices.opened.borrow()[1].clone();
            assert_eq!(opened, (menu_of("Everything", true), at), "it uninstalls");
            assert_eq!(launcher.lit_row(2), first);
            assert!(ran.borrow().is_empty(), "nothing picked");
            assert!(launcher.visible());
        });
    }

    #[test]
    fn the_menu_key_and_shift_f10_open_the_selected_rows_menu_under_it() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything", "Evernote"], &["Everything"]);
            launcher.search_for("ever", 2);
            launcher.press(VK_DOWN);
            let (x, y) = layout::menu_spot(1);
            let under = launcher.on_screen(x, y);
            let title = with_app(|app| titles(app)[1].clone()).expect("app state");
            let menu = menu_of(&title, title == "Everything");
            launcher.menu_key();
            launcher.shift_f10();
            let opened = [(menu.clone(), under), (menu, under)];
            assert_eq!(*choices.opened.borrow(), opened);
            assert!(ran.borrow().is_empty());
        });
    }

    #[test]
    fn a_right_click_off_the_rows_or_the_keys_with_no_row_open_no_menu() {
        on_hidden_desktop(|| {
            let (launcher, _, choices) = with_menus(&["Everything"], &["Everything"]);
            launcher.menu_key();
            launcher.shift_f10();
            launcher.search_for("every", 1);
            launcher.right_click(100.0, layout::BAR_HEIGHT / 2.0);
            launcher.right_click(8.0, 8.0);
            launcher.right_click(200.0, middle_of(1));
            assert!(choices.opened.borrow().is_empty());
            assert!(launcher.visible());
        });
    }

    #[test]
    fn open_and_run_as_administrator_from_the_menu_do_what_enter_does() {
        on_hidden_desktop(|| {
            for as_admin in [false, true] {
                let (launcher, ran, choices) = with_menus(&["Everything"], &["Everything"]);
                launcher.search_for("every", 1);
                *choices.pick.borrow_mut() = Some(Command::Open { as_admin });
                launcher.right_click(200.0, middle_of(0));
                assert_eq!(*ran.borrow(), [opened("Everything", as_admin)]);
                assert!(!launcher.visible(), "hidden, as after Enter");
            }
        });
    }

    #[test]
    fn show_in_folder_and_copy_path_from_the_menu_do_what_their_keys_do() {
        on_hidden_desktop(|| {
            let path = r"C:\Users\Me\notes.txt";
            let notes = Item::file(path.to_string(), false);
            let picks = [
                (Command::Reveal, Action::Reveal(path.to_string())),
                (Command::Copy, Action::CopyText(path.to_string())),
            ];
            for (pick, done) in picks {
                let listed = vec![notes.clone()];
                let provider: Provider = Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
                    report(Found::Items(listed.clone()));
                });
                let (mut services, ran) = services(provider, None);
                let choices = Rc::new(Choices::default());
                services.row_menu = choices.clone();
                let launcher = Launcher::serving(services);
                launcher.search_for("notes", 1);
                *choices.pick.borrow_mut() = Some(pick);
                launcher.menu_key();
                let menu = row_menu::lines(&notes, false, Language::English);
                assert_eq!(choices.opened.borrow()[0].0, menu);
                assert_eq!(*ran.borrow(), [(done, false)]);
                assert!(!launcher.visible());
            }
        });
    }

    #[test]
    fn uninstall_from_the_menu_asks_in_place_and_only_enter_uninstalls() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything"], &["Everything"]);
            launcher.search_for("every", 1);
            *choices.pick.borrow_mut() = Some(Command::Uninstall);
            launcher.right_click(200.0, middle_of(0));
            let asks = "Press Enter to uninstall Everything 1.4.1.1032 (x64)";
            assert_eq!(first_shown().1, asks);
            assert!(asks_in_amber(&launcher), "the question, in amber");
            assert!(ran.borrow().is_empty(), "never straight from the menu");
            assert!(launcher.visible());
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [uninstalled("Everything")]);
            assert!(!launcher.visible());
        });
    }

    #[test]
    fn uninstall_from_the_menu_asks_even_with_text_selected_in_the_bar() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything"], &["Everything"]);
            launcher.search_for("every", 1);
            hold(&[VK_CONTROL]);
            launcher.press(VK_A);
            hold(&[]);
            *choices.pick.borrow_mut() = Some(Command::Uninstall);
            launcher.menu_key();
            assert!(with_app(|app| app.results.asking()).expect("app state"));
            assert_eq!(launcher.query(), "every", "nothing cut");
            assert_eq!(launcher.selected(), "every");
            assert!(ran.borrow().is_empty());
        });
    }

    #[test]
    fn opening_the_menu_of_a_row_that_asks_calls_it_off_so_open_opens() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything"], &["Everything"]);
            launcher.search_for("every", 1);
            shift_delete(&launcher);
            launcher.menu_key();
            assert_eq!(first_shown().1, "App", "called off, nothing picked");
            shift_delete(&launcher);
            *choices.pick.borrow_mut() = Some(Command::Open { as_admin: false });
            launcher.right_click(200.0, middle_of(0));
            assert_eq!(
                *ran.borrow(),
                [opened("Everything", false)],
                "not uninstalled"
            );
        });
    }

    #[test]
    fn what_is_picked_is_done_only_to_the_row_the_menu_was_for() {
        on_hidden_desktop(|| {
            let (launcher, ran, choices) = with_menus(&["Everything", "Evernote"], &["Everything"]);
            launcher.search_for("ever", 2);
            let hwnd = launcher.0;
            // The list changes under the open menu: typing leaves only
            // Everything listed, and selected.
            *choices.meanwhile.borrow_mut() = Some(Box::new(move || {
                unsafe { SendMessageW(hwnd, WM_CHAR, Some(WPARAM(usize::from(b'y'))), None) };
            }));
            *choices.pick.borrow_mut() = Some(Command::Open { as_admin: false });
            launcher.right_click(200.0, middle_of(row_of("Evernote")));
            assert_eq!(titles_listed(), ["Everything"]);
            assert!(
                ran.borrow().is_empty(),
                "Everything was not the row clicked"
            );
            // The launcher hides under the open menu: the hotkey.
            *choices.meanwhile.borrow_mut() = Some(Box::new(move || {
                unsafe {
                    SendMessageW(hwnd, WM_HOTKEY, Some(WPARAM(HOTKEY_TOGGLE as usize)), None)
                };
            }));
            *choices.pick.borrow_mut() = Some(Command::Open { as_admin: false });
            launcher.menu_key();
            assert!(!launcher.visible());
            assert!(ran.borrow().is_empty(), "the row went with the launcher");
        });
    }

    fn titles_listed() -> Vec<String> {
        with_app(|app| titles(app)).expect("app state")
    }

    /// Reported on 0.2.5: Uninstall… picked from the menu uninstalled
    /// nothing. The pointer goes over the menu to pick the entry, below
    /// where the row was clicked; once the menu is gone, Windows tells
    /// the launcher the pointer is there, over another row, and that
    /// must not take the selection, nor call the asking off.
    #[test]
    fn the_pointer_left_where_uninstall_was_picked_calls_nothing_off() {
        on_hidden_desktop(|| {
            let titles = ["Everything", "Evernote", "Everest"];
            let (launcher, ran, choices) = with_menus(&titles, &["Everything"]);
            launcher.search_for("ever", 3);
            let row = row_of("Everything");
            let below = (row + 1) % 3;
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(row));
            *choices.left_at.borrow_mut() = Some(launcher.on_screen(200.0, middle_of(below)));
            *choices.pick.borrow_mut() = Some(Command::Uninstall);
            launcher.right_click(200.0, middle_of(row));
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(below));
            assert_eq!(with_app(|app| app.results.selected()), Some(row));
            assert!(with_app(|app| app.results.asking()).expect("app state"));
            launcher.press(VK_RETURN);
            assert_eq!(*ran.borrow(), [uninstalled("Everything")]);
            launcher.hotkey();
            launcher.search_for("ever", 3);
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(row));
            *choices.left_at.borrow_mut() = None;
            launcher.right_click(200.0, middle_of(row));
            launcher.mouse(WM_MOUSEMOVE, 200.0, middle_of(below));
            let selected = with_app(|app| app.results.selected());
            assert_eq!(
                selected,
                Some(below),
                "where the pointer is not known, a move is the user's"
            );
        });
    }

    thread_local! {
        /// The gate a provider waits at while the real menu is open, and
        /// what the menu saw: whether it was above the launcher when it
        /// opened, and once the list had grown, whether it was still up
        /// and above it.
        static GATE: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
        static OVER: RefCell<Option<bool>> = const { RefCell::new(None) };
        static AFTER: RefCell<Option<(bool, bool)>> = const { RefCell::new(None) };
    }

    /// Whether `upper` is above `lower` among the desktop's windows.
    fn above(upper: HWND, lower: HWND) -> bool {
        let mut at = unsafe { GetWindow(lower, GW_HWNDPREV) }.ok();
        while let Some(window) = at {
            if window == upper {
                return true;
            }
            at = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
        }
        false
    }

    /// Ticks inside the real menu's own loop: once it is up, notes
    /// whether it is above the launcher and lets the provider hand over
    /// more; once the list grew from that, notes whether the menu is still
    /// up and above, and closes it. Gives up after a few seconds.
    extern "system" fn refresh_under_the_menu(hwnd: HWND, _: u32, id: usize, _: u32) {
        let ticks = TICKS.with(|ticks| {
            *ticks.borrow_mut() += 1;
            *ticks.borrow()
        });
        let menu = menu_up();
        let grown = with_app(|app| app.results.rows().len() == 2) == Some(true);
        if let (Some(menu), None) = (menu, OVER.with(|over| *over.borrow())) {
            OVER.with(|over| *over.borrow_mut() = Some(above(menu, hwnd)));
            let gate = GATE.with(|gate| gate.borrow_mut().take()).expect("a gate");
            gate.send(()).expect("let the provider go");
        } else if grown || ticks > 500 {
            let still = menu.map(|menu| (true, above(menu, hwnd)));
            AFTER.with(|after| *after.borrow_mut() = Some(still.unwrap_or((false, false))));
            let _ = unsafe { KillTimer(Some(hwnd), id) };
            let _ = unsafe { EndMenu() };
        }
    }

    /// Reported on 0.2.5: every row's menu went away within about a
    /// second. A refresh arrives while the menu is open and draws the
    /// launcher again (here a provider's next piece; on a live machine
    /// the files provider's, up to twice a second while the index
    /// changes, or a folder's icon): that must leave the menu up and
    /// above it.
    #[test]
    fn a_refresh_while_the_real_menu_is_open_leaves_the_menu_up_above_the_launcher() {
        on_hidden_desktop(|| {
            let (provider, gate) = gated(&["Everything"], &["Everything", "Evernote"]);
            let (mut services, _) = services(provider, None);
            services.row_menu = Rc::new(row_menu::Popup);
            let launcher = Launcher::serving(services);
            launcher.search_for("ever", 1);
            GATE.with(|held| *held.borrow_mut() = Some(gate));
            unsafe { SetTimer(Some(launcher.0), 1, 10, Some(refresh_under_the_menu)) };
            launcher.menu_key();
            assert_eq!(
                OVER.with(|over| *over.borrow()),
                Some(true),
                "above at first"
            );
            let after = AFTER.with(|after| after.borrow_mut().take());
            assert_eq!(titles_listed().len(), 2, "the list grew under the menu");
            assert_eq!(
                after,
                Some((true, true)),
                "still up, and above the launcher"
            );
        });
    }

    thread_local! {
        /// What the real menu saw once it was up: whether the launcher
        /// was still shown, and where the menu was.
        static IN_MENU: RefCell<Option<(bool, RECT)>> = const { RefCell::new(None) };
        static TICKS: RefCell<u32> = const { RefCell::new(0) };
    }

    /// The popup menu this thread has up, if any.
    fn menu_up() -> Option<HWND> {
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

    /// Ticks inside the real menu's own message loop: once the menu is
    /// up, notes what it sees and picks the second entry, as Down, Down
    /// and Enter would; gives up after a second.
    extern "system" fn pick_second(hwnd: HWND, _: u32, id: usize, _: u32) {
        let ticks = TICKS.with(|ticks| {
            *ticks.borrow_mut() += 1;
            *ticks.borrow()
        });
        let Some(menu) = menu_up() else {
            if ticks > 100 {
                let _ = unsafe { KillTimer(Some(hwnd), id) };
                let _ = unsafe { EndMenu() };
            }
            return;
        };
        let _ = unsafe { KillTimer(Some(hwnd), id) };
        let mut rect = RECT::default();
        unsafe { GetWindowRect(menu, &mut rect) }.expect("the menu's place");
        let shown = unsafe { IsWindowVisible(hwnd) }.as_bool();
        IN_MENU.with(|seen| *seen.borrow_mut() = Some((shown, rect)));
        for key in [VK_DOWN, VK_DOWN, VK_RETURN] {
            let key = WPARAM(usize::from(key.0));
            unsafe { PostMessageW(Some(menu), WM_KEYDOWN, key, LPARAM(0)) }.expect("a key");
        }
    }

    #[test]
    fn the_real_menu_opens_under_the_row_keeps_the_launcher_up_and_does_what_is_picked() {
        on_hidden_desktop(|| {
            let (mut services, ran) = uninstallers(&["Everything"], &[]);
            services.row_menu = Rc::new(row_menu::Popup);
            let launcher = Launcher::serving(services);
            launcher.search_for("every", 1);
            unsafe { SetTimer(Some(launcher.0), 1, 10, Some(pick_second)) };
            launcher.menu_key();
            let (shown, rect) = IN_MENU
                .with(|seen| seen.borrow_mut().take())
                .expect("a menu");
            assert!(shown, "losing no focus to its menu, the launcher stays");
            let (x, y) = layout::menu_spot(0);
            let under = launcher.on_screen(x, y);
            assert_eq!((rect.left, rect.top), (under.x, under.y), "under the row");
            assert_eq!(
                *ran.borrow(),
                [opened("Everything", true)],
                "as administrator"
            );
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

    #[test]
    fn a_prefix_and_words_list_a_web_search_first_and_enter_opens_it() {
        on_hidden_desktop(|| {
            let picks = PicksFile::new("window-web-search");
            let (launcher, ran) = Launcher::listing_with(&["Git Bash"], Some(picks.0.clone()));
            launcher.type_text("git");
            assert_eq!(listed(), ["Git Bash"], "a word that starts like gh");
            hold(&[VK_CONTROL]);
            launcher.press(VK_A);
            hold(&[]);
            launcher.type_text("g привет мир");
            let title = "Search Google for “привет мир”";
            assert_eq!(
                first_row(),
                (
                    title.to_string(),
                    "Web search · Enter to open in the browser".to_string()
                )
            );
            launcher.press(VK_RETURN);
            let url = "https://www.google.com/search?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82%20%D0%BC%D0%B8%D1%80";
            assert_eq!(*ran.borrow(), [(Action::Open(url.to_string()), false)]);
            assert!(!launcher.visible());
            assert!(!picks.0.exists(), "a web search is not a pick");
        });
    }

    #[test]
    fn what_reads_as_a_filter_after_a_web_prefix_is_searched_for_too() {
        on_hidden_desktop(|| {
            let launcher = Launcher::listing(&["Notepad"]);
            launcher.type_text("g rust ext:pdf");
            let title = "Search Google for “rust ext:pdf”";
            assert_eq!(first_row().0, title);
        });
    }

    #[test]
    fn a_web_search_of_the_configs_is_listed_and_changes_when_saved() {
        on_hidden_desktop(|| {
            let so = "[search]\nso = 'https://stackoverflow.com/search?q={query}'\n";
            let (_scratch, path) = config_file("window-config-search", so);
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, _, _) = configured(&path, &keyboard);
            launcher.type_text("so borrow");
            assert_eq!(first_row().0, "Search stackoverflow.com for “borrow”");
            std::fs::write(
                &path,
                "[search]\nso = 'https://sourcegraph.com/search?q={query}'\n",
            )
            .expect("save");
            launcher.pump_until(|app| {
                app.results.rows()[0].item.title == "Search sourcegraph.com for “borrow”"
            });
        });
    }

    #[test]
    fn an_older_config_has_what_it_leaves_out_written_in_at_start() {
        on_hidden_desktop(|| {
            let older = "hotkey = 'F4'\n\n[folders]\nexclude = []\n";
            let (_scratch, path) = config_file("window-config-older", older);
            let keyboard = Rc::new(Keyboard::default());
            let (launcher, _, _) = configured(&path, &keyboard);
            let text = std::fs::read_to_string(&path).expect("the file");
            assert!(text.starts_with("hotkey = 'F4'\n"), "{text}");
            assert!(text.contains("\n[folders]\nexclude = []\n"), "kept: {text}");
            assert!(text.contains("\ncheck_for_updates = true\n"), "{text}");
            assert!(text.contains("\ng = \"https://www.google.com/"), "{text}");
            launcher.type_text("gh hyalo");
            assert_eq!(first_row().0, "Search GitHub for “hyalo”");
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
        let source = Source::new(provider, rates::REFRESH_AFTER, None);
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
    fn filters_narrow_the_files_listed_and_alone_list_files_by_themselves() {
        on_hidden_desktop(|| {
            let tree = Scratch::new("window-filters").with(&[
                "report.pdf",
                "report.txt",
                "deep\\",
                "deep\\scan.pdf",
            ]);
            let (launcher, _) = Launcher::finding(&tree.0, None);
            launcher.search_for("report", 2);
            launcher.type_text(" ext:pdf");
            launcher.pump_until(|app| titles(app) == ["report.pdf"]);
            drop(launcher);
            let (launcher, _) = Launcher::finding(&tree.0, None);
            launcher.search_for("in:deep", 1);
            assert_eq!(listed(), ["scan.pdf"]);
        });
    }

    #[test]
    fn the_clock_is_now_and_as_far_ahead_of_utc_as_windows_local_time_is() {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
        let clock = clock();
        let utc = (clock.now + 11_644_473_600) * 10_000_000;
        let as_filetime = |time: i64| FILETIME {
            dwLowDateTime: time as u32,
            dwHighDateTime: (time >> 32) as u32,
        };
        let mut local = FILETIME::default();
        // SAFETY: both structs are ours.
        unsafe { FileTimeToLocalFileTime(&as_filetime(utc), &mut local) }.expect("local time");
        let local = i64::from(local.dwHighDateTime) << 32 | i64::from(local.dwLowDateTime);
        assert_eq!(clock.offset, (local - utc) / 10_000_000);
        assert!((clock.now - now().0 as i64).abs() <= 1);
    }

    #[test]
    fn a_picked_file_the_filters_leave_out_is_not_listed() {
        let picks = PicksFile::new("filtered");
        let path = picks.0.clone();
        on_hidden_desktop(move || {
            let tree = Scratch::new("window-picked-filtered").with(&["notes.txt", "notes.md"]);
            let (first, _) = Launcher::finding(&tree.0, Some(path.clone()));
            first.search_for("notes.txt", 1);
            first.press(VK_RETURN);
            drop(first);
            let (second, _) = Launcher::finding(&tree.0, Some(path));
            second.pump_until(|app| !app.catalog.picked.is_empty());
            assert_eq!(
                listed(),
                Vec::<String>::new(),
                "nothing typed, nothing listed"
            );
            second.search_for("notes", 2);
            second.type_text(" ext:md");
            second.pump_until(|app| titles(app) == ["notes.md"]);
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

    /// GitHub's answer about a release tagged `tag`, with `notes` as its
    /// JSON writes them.
    pub(super) fn release_json(tag: &str, notes: &str) -> Vec<u8> {
        let url = format!("https://github.com/IlyaShorin/hyalo/releases/download/{tag}");
        format!(
            r#"{{"tag_name": "{tag}", "draft": false, "prerelease": false, "body": "{notes}",
                "assets": [{{"name": "hyalo.exe", "browser_download_url": "{url}/hyalo.exe"}},
                {{"name": "hyalo.exe.sha256", "browser_download_url": "{url}/hyalo.exe.sha256"}}]}}"#
        )
        .into_bytes()
    }

    /// Releases as a test keeps them: how often GitHub was asked, the exe
    /// an update replaces (`old build`, in a scratch folder of its own),
    /// and the exes started in its place.
    pub(super) struct Releases {
        pub(super) asked: Arc<AtomicUsize>,
        pub(super) exe: PathBuf,
        pub(super) started: Arc<Mutex<Vec<PathBuf>>>,
        pub(super) _folder: Scratch,
    }

    impl Releases {
        /// What the exe's folder holds: each file's name and text.
        pub(super) fn files(&self) -> Vec<(String, String)> {
            let folder = self.exe.parent().expect("a folder");
            let mut files: Vec<(String, String)> = std::fs::read_dir(folder)
                .expect("the folder")
                .map(|entry| {
                    let entry = entry.expect("an entry");
                    let text = std::fs::read_to_string(entry.path()).expect("the file");
                    (entry.file_name().to_string_lossy().into_owned(), text)
                })
                .collect();
            files.sort();
            files
        }

        pub(super) fn started(&self) -> Vec<PathBuf> {
            self.started.lock().expect("started").clone()
        }
    }

    /// The files of a folder, by name, with what they hold.
    pub(super) fn named(files: &[(&str, &str)]) -> Vec<(String, String)> {
        let named = files
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()));
        named.collect()
    }

    /// Updates for hyalo 0.0.1 from a GitHub that answers `latest`, and
    /// serves `new build` with the sum of `summed`.
    pub(super) fn releasing(
        test: &str,
        latest: std::result::Result<Vec<u8>, String>,
        summed: &[u8],
    ) -> (Updates, Releases) {
        let folder = Scratch::new(test);
        let exe = folder.0.join("hyalo.exe");
        std::fs::write(&exe, b"old build").expect("the old build");
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = asked.clone();
        let sum: String = update::sha256(summed)
            .0
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let download = move |url: &str| match url.rsplit('/').next() {
            Some("hyalo.exe") => Ok(b"new build".to_vec()),
            Some("hyalo.exe.sha256") => Ok(format!("{sum}  hyalo.exe\n").into_bytes()),
            _ => Err(format!("nothing at {url}")),
        };
        let started = Arc::new(Mutex::new(Vec::new()));
        let noted = started.clone();
        let updates = Updates {
            current: hyalo_core::update::Version::parse("0.0.1").expect("a version"),
            latest: Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                latest.clone()
            }),
            installer: Arc::new(update::Installer {
                download: Arc::new(download),
                exe: exe.clone(),
                start: Arc::new(move |exe: &Path| {
                    noted.lock().expect("started").push(exe.to_path_buf());
                    Ok(())
                }),
            }),
        };
        let releases = Releases {
            asked,
            exe,
            started,
            _folder: folder,
        };
        (updates, releases)
    }

    /// A launcher on a config holding `text`, with `updates` and a
    /// stand-in taskbar.
    fn updating(test: &str, text: &str, updates: Updates) -> (Launcher, Rc<Taskbar>, Scratch) {
        let (scratch, path) = config_file(test, text);
        let taskbar = Rc::new(Taskbar::default());
        let (mut services, _) = services(listing(&[]).0, None);
        services.tray = taskbar.clone();
        services.config = Some(path);
        services.updates = Some(updates);
        (Launcher::serving(services), taskbar, scratch)
    }

    /// Handles messages as the message loop would until the window is
    /// gone; fails after `PATIENCE`.
    #[track_caller]
    pub(super) fn pump_until_closed(hwnd: HWND) {
        let deadline = Instant::now() + PATIENCE;
        while unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for it to close"
            );
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

    #[test]
    fn a_newer_release_is_told_under_the_empty_bar_in_the_tray_and_its_menu() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", "Faster."));
            let (updates, releases) = releasing("update-told", latest, b"new build");
            let (launcher, taskbar, _scratch) = updating("update-told-config", DEFAULT, updates);
            launcher.pump_until(|app| app.update.is_some());
            assert_eq!(releases.asked.load(Ordering::SeqCst), 1);
            launcher.search_for("", 1);
            let (title, subtitle) = first_row();
            assert_eq!(title, "hyalo 0.0.2 is out");
            assert!(subtitle.starts_with("See what is new"), "{subtitle}");
            let tip = taskbar.tips.borrow().last().cloned().expect("a tip");
            assert_eq!(tip, "hyalo · hyalo 0.0.2 is out");
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(menu_labels(&taskbar)[1], "Install hyalo 0.0.2");
            launcher.press(VK_RETURN);
            launcher.pump_until(|app| app.settings.is_some());
        });
    }

    #[test]
    fn github_is_asked_once_at_start_not_on_every_show() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.1", ""));
            let (updates, releases) = releasing("update-once", latest, b"new build");
            let (launcher, taskbar, _scratch) = updating("update-once-config", DEFAULT, updates);
            launcher.pump_until(|app| matches!(app.checked, Checked::Answered(_)));
            launcher.hotkey();
            launcher.hotkey();
            launcher.pump_until(|app| app.checked != Checked::Asking);
            assert_eq!(releases.asked.load(Ordering::SeqCst), 1);
            assert!(with_app(|app| app.update.is_none()).expect("app state"));
            launcher.search_for("", 0);
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(menu_labels(&taskbar).len(), 4, "no install");
        });
    }

    #[test]
    fn a_check_that_fails_tells_nothing_and_is_tried_again_later() {
        on_hidden_desktop(|| {
            let (updates, releases) = releasing("update-offline", Err("offline".into()), b"");
            let (launcher, taskbar, _scratch) = updating("update-offline-config", DEFAULT, updates);
            launcher.pump_until(|app| matches!(app.checked, Checked::Failed(_)));
            assert_eq!(releases.asked.load(Ordering::SeqCst), 1);
            launcher.search_for("", 0);
            let tip = taskbar.tips.borrow().last().cloned().expect("a tip");
            assert_eq!(tip, "hyalo · Alt+Space");
        });
    }

    #[test]
    fn with_checking_off_github_is_not_asked_until_it_is_turned_on() {
        on_hidden_desktop(|| {
            let off = DEFAULT.replace("check_for_updates = true", "check_for_updates = false");
            let latest = Ok(release_json("v0.0.2", ""));
            let (updates, releases) = releasing("update-off", latest, b"new build");
            let (launcher, _taskbar, scratch) = updating("update-off-config", &off, updates);
            launcher.hotkey();
            launcher.hotkey();
            assert_eq!(
                with_app(|app| app.checked).expect("app state"),
                Checked::Never
            );
            let path = scratch.0.join("hyalo").join("config.toml");
            std::fs::write(&path, DEFAULT).expect("turn it on");
            launcher.pump_until(|app| app.update.is_some());
            assert_eq!(releases.asked.load(Ordering::SeqCst), 1);
            std::fs::write(&path, &off).expect("turn it off");
            launcher.pump_until(|app| app.notice().is_none());
            assert!(
                with_app(|app| app.update.is_some()).expect("app state"),
                "known still"
            );
        });
    }

    #[test]
    fn install_from_the_tray_swaps_the_new_build_in_starts_it_and_closes_hyalo() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", ""));
            let (updates, releases) = releasing("update-install", latest, b"new build");
            let (launcher, taskbar, _scratch) = updating("update-install-config", DEFAULT, updates);
            launcher.pump_until(|app| app.update.is_some());
            launcher.pick(&taskbar, tray::Command::Install);
            pump_until_closed(launcher.0);
            assert_eq!(
                releases.files(),
                named(&[("hyalo.exe", "new build"), ("hyalo.exe.old", "old build")])
            );
            assert_eq!(releases.started(), std::slice::from_ref(&releases.exe));
        });
    }

    #[test]
    fn a_build_its_sum_does_not_name_is_refused_told_and_hyalo_runs_on() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", ""));
            let (updates, releases) = releasing("update-refused", latest, b"another build");
            let (launcher, taskbar, _scratch) = updating("update-refused-config", DEFAULT, updates);
            launcher.pump_until(|app| app.update.is_some());
            launcher.pick(&taskbar, tray::Command::Install);
            launcher.pump_until(|app| {
                app.update
                    .as_ref()
                    .is_some_and(|update| matches!(update.progress, Progress::Failed(_)))
            });
            launcher.search_for("", 1);
            let (title, subtitle) = first_row();
            let said =
                "hyalo 0.0.2 is not installed: the download is not the build its SHA-256 names";
            assert_eq!(title, said);
            assert!(
                subtitle.starts_with("hyalo 0.0.1 keeps running"),
                "{subtitle}"
            );
            assert!(launcher.visible());
            assert_eq!(releases.files(), named(&[("hyalo.exe", "old build")]));
            assert!(releases.started().is_empty());
            launcher.tray(WM_CONTEXTMENU, (0, 0));
            assert_eq!(
                menu_labels(&taskbar)[1],
                "Install hyalo 0.0.2",
                "to try again"
            );
        });
    }

    #[test]
    fn install_asked_again_while_it_installs_downloads_nothing_more() {
        on_hidden_desktop(|| {
            let latest = Ok(release_json("v0.0.2", ""));
            let (mut updates, releases) = releasing("update-twice", latest, b"new build");
            // The sum's download waits until the test lets it go.
            let (go, gate) = channel::<()>();
            let gate = Mutex::new(gate);
            let asked = Arc::new(AtomicUsize::new(0));
            let counted = asked.clone();
            let download = updates.installer.download.clone();
            let installer = update::Installer {
                download: Arc::new(move |url: &str| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = gate.lock().expect("the gate").recv();
                    download(url)
                }),
                exe: releases.exe.clone(),
                start: updates.installer.start.clone(),
            };
            updates.installer = Arc::new(installer);
            let (launcher, taskbar, _scratch) = updating("update-twice-config", DEFAULT, updates);
            launcher.pump_until(|app| app.update.is_some());
            launcher.pick(&taskbar, tray::Command::Install);
            launcher.pick(&taskbar, tray::Command::Install);
            assert!(
                with_app(|app| {
                    app.update
                        .as_ref()
                        .is_some_and(|update| update.progress == Progress::Installing)
                })
                .expect("app state")
            );
            go.send(()).expect("let the sum go");
            go.send(()).expect("let the exe go");
            pump_until_closed(launcher.0);
            assert_eq!(asked.load(Ordering::SeqCst), 2, "the sum and the exe, once");
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
