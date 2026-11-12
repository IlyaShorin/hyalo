//! What the launcher can list: items and their icons, found off the UI
//! thread by a provider and handed over piece by piece.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hyalo_core::item::{Item, ItemId};
use hyalo_core::rates::Rates;
use hyalo_core::system_settings;
use hyalo_core::uninstall::{self, Uninstall};
use hyalo_core::update::{Failure, Release};
use windows::core::Result;

use crate::apps::{self, Icon};
use crate::com::Com;

/// One piece of what a provider found.
pub enum Found {
    /// Everything there is to list, replacing what was listed before.
    Items(Vec<Item>),
    /// The icon of one item.
    Icon(ItemId, Icon),
    /// Each app's uninstaller, where hyalo can tell it, in place of those
    /// found before.
    Uninstallers(HashMap<ItemId, Uninstall>),
    /// The file index changed: search it again.
    Files,
    /// Picked files and folders that are still there.
    Picked(Vec<Item>),
    /// The newest exchange rates.
    Rates(Rates),
    /// The open windows, front first, in place of those found before.
    Windows(Vec<Item>),
    /// What the config file holds since it was saved, as bytes: the
    /// launcher reads the settings from them, the settings window what
    /// they write.
    Config(Vec<u8>),
    /// GitHub's answer about the latest release: the release, if it is
    /// one hyalo can install, or why GitHub could not be asked.
    Latest(std::result::Result<Option<Release>, String>),
    /// How installing an update went.
    Installed(std::result::Result<(), Failure>),
    /// An uninstaller ended, whether it uninstalled or was called off:
    /// the apps may have changed.
    UninstallEnded,
}

/// What the window asks a provider for.
pub struct Request {
    /// Icon side in pixels.
    pub icon_size: u32,
    /// Items whose icons the window already has.
    pub known: HashSet<ItemId>,
}

/// A provider: finds items on a thread of its own, reporting each piece
/// as it goes.
pub type Provider = Arc<dyn Fn(&Request, &dyn Fn(Found)) + Send + Sync>;

/// Finds the icons of the files and folders `ids` names, `size` pixels
/// square, on a thread of its own, reporting each as `Found::Icon`.
pub type FileIcons = Arc<dyn Fn(&[ItemId], u32, &dyn Fn(Found)) + Send + Sync>;

/// The items to search and the icons found for them so far.
#[derive(Default)]
pub struct Catalog {
    /// What the apps provider lists.
    pub items: Vec<Item>,
    /// Windows' own settings and the system commands, in the language in
    /// force.
    pub system: Vec<Item>,
    pub icons: HashMap<ItemId, Icon>,
    /// What uninstalls each app that can be uninstalled.
    pub uninstallers: HashMap<ItemId, Uninstall>,
    /// Files and folders picked before, ranked with the items so that
    /// frecency lifts them however many files match as well.
    pub picked: Vec<Item>,
    /// What currency answers convert with; `None` until found.
    pub rates: Option<Rates>,
    /// The open windows the switcher lists, front first.
    pub windows: Vec<Item>,
}

impl Catalog {
    /// Takes in one piece.
    pub fn take(&mut self, found: Found) {
        match found {
            Found::Items(items) => self.items = items,
            Found::Icon(id, icon) => {
                self.icons.insert(id, icon);
            }
            Found::Uninstallers(uninstallers) => self.uninstallers = uninstallers,
            Found::Files => {}
            Found::Picked(items) => items.into_iter().for_each(|item| self.pick(item)),
            Found::Rates(rates) => self.rates = Some(rates),
            Found::Windows(windows) => self.open(windows),
            // The window puts settings in force and updates hyalo; there
            // is nothing to list.
            Found::Config(_) | Found::Latest(_) | Found::Installed(_) | Found::UninstallEnded => {}
        }
    }

    /// Takes in the open windows; the icons of those closed since go, so
    /// that windows coming and going leave none behind.
    fn open(&mut self, windows: Vec<Item>) {
        let closed: Vec<ItemId> = self
            .windows
            .iter()
            .filter(|old| !windows.iter().any(|open| open.id == old.id))
            .map(|old| old.id.clone())
            .collect();
        for id in closed {
            self.icons.remove(&id);
        }
        self.windows = windows;
    }

    /// Remembers a picked file or folder, once.
    pub fn pick(&mut self, item: Item) {
        if !self.picked.iter().any(|picked| picked.id == item.id) {
            self.picked.push(item);
        }
    }
}

/// What the Uninstall keys list: the registry's (`uninstall::entries`),
/// or a test's stand-in.
pub type Listing = Arc<dyn Fn() -> Vec<uninstall::Entry> + Send + Sync>;

/// The apps provider: lists the Applications folder, reports the items,
/// then each app's uninstaller, of those `listing` gives, then the icons
/// the window lacks, one at a time, so search works while icons still
/// come in (listing takes about 0.5 s, icons as long again).
pub fn apps(listing: Listing) -> Provider {
    Arc::new(move |request: &Request, report: &dyn Fn(Found)| {
        if let Err(error) = report_apps(request, &listing, report) {
            eprintln!("hyalo: cannot list the apps: {error}");
        }
    })
}

fn report_apps(request: &Request, listing: &Listing, report: &dyn Fn(Found)) -> Result<()> {
    let _com = Com::init()?;
    let entries = apps::find()?;
    report(Found::Items(
        entries.iter().map(|entry| entry.item.clone()).collect(),
    ));
    report(Found::Uninstallers(uninstallers(&entries, &listing())));
    let missing = entries
        .iter()
        .filter(|entry| !request.known.contains(&entry.item.id));
    for entry in missing {
        match entry.icon(request.icon_size) {
            Ok(icon) => report(Found::Icon(entry.item.id.clone(), icon)),
            Err(error) => eprintln!("hyalo: no icon for {}: {error}", entry.item.title),
        }
    }
    Ok(())
}

/// What uninstalls each of the apps `entries` lists, where it can be told
/// from what the Uninstall keys list in `listed`.
fn uninstallers(
    entries: &[apps::Entry],
    listed: &[uninstall::Entry],
) -> HashMap<ItemId, Uninstall> {
    let folders = crate::uninstall::folders();
    let found = entries.iter().filter_map(|entry| {
        let uninstaller = uninstall::find(&entry.app, listed, &folders)?;
        Some((entry.item.id.clone(), uninstaller))
    });
    found.collect()
}

/// The icons of Windows' settings the window lacks, from the shell: the
/// Settings app's for each of its pages, found once, and each Control
/// Panel item's own.
pub fn find_system_icons(request: &Request, report: &dyn Fn(Found)) {
    if let Err(error) = Com::init() {
        eprintln!("hyalo: no icons for Windows' settings: {error}");
        return;
    }
    let mut found: HashMap<&str, Icon> = HashMap::new();
    for setting in system_settings::SETTINGS {
        let id = setting.id();
        if request.known.contains(&id) {
            continue;
        }
        let icon = match found.get(setting.icon) {
            Some(icon) => Ok(icon.clone()),
            None => apps::icon_at(setting.icon, request.icon_size),
        };
        match icon {
            Ok(icon) => {
                found.insert(setting.icon, icon.clone());
                report(Found::Icon(id, icon));
            }
            Err(error) => eprintln!("hyalo: no icon for {}: {error}", id.0),
        }
    }
}

/// The shell's icons for files and folders, one by one.
pub fn file_icons(ids: &[ItemId], size: u32, report: &dyn Fn(Found)) {
    let com = Com::init();
    if let Err(error) = com {
        eprintln!("hyalo: no icons for files: {error}");
        return;
    }
    for id in ids {
        match apps::icon_at(&id.0, size) {
            Ok(icon) => report(Found::Icon(id.clone(), icon)),
            Err(error) => eprintln!("hyalo: no icon for {}: {error}", id.0),
        }
    }
}

/// How long the apps are trusted before showing the launcher looks again.
/// Listing them costs half a second of a core, too much for every show.
pub const REFRESH_AFTER: Duration = Duration::from_secs(5 * 60);

/// Whether a list found at `found_at` (`None`: never) is due for
/// another look at `now`, when it is trusted for `trusted`.
pub fn stale(found_at: Option<Instant>, now: Instant, trusted: Duration) -> bool {
    found_at.is_none_or(|at| now.duration_since(at) >= trusted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::tests::shell;
    use hyalo_core::item::{Action, Kind};
    use std::cell::RefCell;

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: String::new(),
            keywords: Vec::new(),
            kind: Kind::App,
            action: Action::Open(title.to_string()),
        }
    }

    fn dot() -> Icon {
        Icon {
            width: 1,
            height: 1,
            bgra: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn new_items_replace_the_old() {
        let mut catalog = Catalog::default();
        catalog.take(Found::Items(vec![app("Paint")]));
        catalog.take(Found::Items(vec![app("Notepad")]));
        assert_eq!(catalog.items, [app("Notepad")]);
    }

    #[test]
    fn an_icon_is_kept_for_its_item() {
        let mut catalog = Catalog::default();
        let paint = ItemId("Paint".to_string());
        catalog.take(Found::Icon(paint.clone(), dot()));
        assert_eq!(catalog.icons.get(&paint), Some(&dot()));
    }

    #[test]
    fn the_rates_found_last_are_kept() {
        let mut catalog = Catalog::default();
        catalog.take(Found::Rates(crate::rates::tests::dated("09.11.2026")));
        catalog.take(Found::Rates(crate::rates::tests::fixture()));
        assert_eq!(catalog.rates, Some(crate::rates::tests::fixture()));
    }

    #[test]
    fn open_windows_replace_those_before_and_the_closed_ones_lose_their_icons() {
        let mut catalog = Catalog::default();
        let one = hyalo_core::switcher::item(1, "One", "App");
        let two = hyalo_core::switcher::item(2, "Two", "App");
        catalog.take(Found::Items(vec![app("Paint")]));
        catalog.take(Found::Windows(vec![one.clone(), two.clone()]));
        for id in [&one.id, &two.id, &app("Paint").id] {
            catalog.take(Found::Icon(id.clone(), dot()));
        }
        catalog.take(Found::Windows(vec![two.clone()]));
        assert_eq!(catalog.windows, std::slice::from_ref(&two));
        assert!(!catalog.icons.contains_key(&one.id), "closed");
        assert!(catalog.icons.contains_key(&two.id), "still open");
        assert!(catalog.icons.contains_key(&app("Paint").id), "not a window");
        assert_eq!(catalog.items, [app("Paint")]);
    }

    #[test]
    fn a_list_never_found_is_stale() {
        assert!(stale(None, Instant::now(), REFRESH_AFTER));
    }

    #[test]
    fn a_list_is_trusted_as_long_as_it_says() {
        let found = Instant::now();
        let later = |seconds| found + Duration::from_secs(seconds);
        let five_minutes = Duration::from_secs(300);
        assert!(!stale(Some(found), found, five_minutes));
        assert!(!stale(Some(found), later(299), five_minutes));
        assert!(stale(Some(found), later(300), five_minutes));
        assert_eq!(REFRESH_AFTER, five_minutes, "the apps");
    }

    #[test]
    fn picked_files_are_kept_once() {
        let mut catalog = Catalog::default();
        let file = Item::file(r"C:\a.txt".to_string(), false);
        catalog.pick(file.clone());
        catalog.take(Found::Picked(vec![
            file.clone(),
            Item::file(r"C:\b".to_string(), true),
        ]));
        catalog.take(Found::Files);
        assert_eq!(
            catalog.picked,
            [file, Item::file(r"C:\b".to_string(), true)]
        );
    }

    #[test]
    fn a_files_icon_comes_from_the_shell() {
        let windows = std::env::var("SystemRoot").expect("SystemRoot is set");
        let ids = [
            ItemId(format!(r"{windows}\explorer.exe")),
            ItemId(format!(r"{windows}\no such file.exe")),
        ];
        let asked = ids.clone();
        let found = reported(move |report| file_icons(&asked, 32, report));
        assert_eq!(found.len(), 1, "one icon, none for what is not there");
        let icon = icon_of(&found, &ids[0].0).expect("explorer's icon");
        assert_eq!((icon.width, icon.height), (32, 32));
    }

    const EXPLORER: &str = "Microsoft.Windows.Explorer";

    /// Everything `find` reports, found with the shell, which has a
    /// deadline (`apps::tests::shell`).
    #[track_caller]
    fn reported(find: impl FnOnce(&dyn Fn(Found)) + Send + 'static) -> Vec<Found> {
        shell(move || {
            let found = RefCell::new(Vec::new());
            find(&|piece| found.borrow_mut().push(piece));
            found.into_inner()
        })
    }

    /// Everything the apps provider reports when the window has the icons
    /// of `known` and the Uninstall keys list `listed`.
    #[track_caller]
    fn found_with(known: HashSet<ItemId>, listed: Vec<uninstall::Entry>) -> Vec<Found> {
        let request = Request {
            icon_size: 32,
            known,
        };
        let listing: Listing = Arc::new(move || listed.clone());
        reported(move |report| apps(listing)(&request, report))
    }

    /// The same, the Uninstall keys listing nothing.
    #[track_caller]
    fn found(known: HashSet<ItemId>) -> Vec<Found> {
        found_with(known, Vec::new())
    }

    fn icon_of<'a>(found: &'a [Found], id: &str) -> Option<&'a Icon> {
        found.iter().find_map(|piece| match piece {
            Found::Icon(of, icon) if of.0 == id => Some(icon),
            _ => None,
        })
    }

    #[test]
    fn every_windows_setting_gets_an_icon_once_and_none_it_has_already() {
        let request = Request {
            icon_size: 32,
            known: HashSet::new(),
        };
        let first = reported(move |report| find_system_icons(&request, report));
        let settings = system_settings::SETTINGS;
        assert_eq!(first.len(), settings.len(), "one icon each");
        let display = icon_of(&first, "ms-settings:display").expect("a page's icon");
        assert_eq!((display.width, display.height), (32, 32));
        let sound = icon_of(&first, "ms-settings:sound").expect("another page's");
        assert_eq!(display, sound, "the Settings app's, both");
        let devices = "shell:::{74246bfc-4c96-11d0-abef-0020af6b0b7a}";
        let devices = icon_of(&first, devices).expect("Device Manager's own");
        assert_ne!(devices, display);
        let known = settings
            .iter()
            .skip(1)
            .map(|setting| setting.id())
            .collect();
        let request = Request {
            icon_size: 32,
            known,
        };
        let again = reported(move |report| find_system_icons(&request, report));
        assert_eq!(again.len(), 1, "only the one not known");
    }

    #[test]
    fn the_apps_provider_reports_the_apps_then_only_the_icons_not_known() {
        let first = found(HashSet::new());
        let Some(Found::Items(items)) = first.first() else {
            panic!("the items come first");
        };
        assert!(items.iter().any(|item| item.id.0 == EXPLORER));
        let icon = icon_of(&first, EXPLORER).expect("Explorer's icon");
        assert_eq!((icon.width, icon.height), (32, 32));
        let known = items.iter().map(|item| item.id.clone()).collect();
        let again = found(known).len();
        assert_eq!(again, 2, "the items and the uninstallers again, no icons");
    }

    #[test]
    fn after_the_apps_comes_each_ones_uninstaller_of_those_the_keys_list() {
        let setup = r"C:\Program Files (x86)\Microsoft\Edge\Application\setup.exe";
        let edge = uninstall::Entry {
            key: "Microsoft Edge".to_string(),
            name: "Microsoft Edge".to_string(),
            command: format!("\"{setup}\" --uninstall"),
            ..uninstall::Entry::default()
        };
        let found = found_with(HashSet::new(), vec![edge]);
        let Some(Found::Uninstallers(uninstallers)) = found.get(1) else {
            panic!("the uninstallers come after the items");
        };
        let of = |id: &str| uninstallers.get(&ItemId(id.to_string()));
        let edge = of("MSEdge").expect("Edge's, by its name");
        let removes = Uninstall::Program {
            name: "Microsoft Edge".to_string(),
            program: setup.to_string(),
            arguments: "--uninstall".to_string(),
        };
        assert_eq!(edge, &removes);
        let store = of("Microsoft.WindowsStore_8wekyb3d8bbwe!App");
        let Some(Uninstall::Package { full_name, .. }) = store else {
            panic!("the Store's package: {store:?}");
        };
        assert!(
            full_name.starts_with("Microsoft.WindowsStore_"),
            "{full_name}"
        );
        assert_eq!(of(EXPLORER), None, "Windows' own");
        let settings =
            "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
        assert_eq!(of(settings), None, "a package of Windows' own");
    }

    #[test]
    fn uninstallers_found_replace_those_before() {
        let mut catalog = Catalog::default();
        let paint = ItemId("Paint".to_string());
        let package = |full_name: &str| Uninstall::Package {
            name: "Paint".to_string(),
            full_name: full_name.to_string(),
        };
        let found = |full_name| HashMap::from([(paint.clone(), package(full_name))]);
        catalog.take(Found::Uninstallers(found("old")));
        catalog.take(Found::Uninstallers(found("new")));
        assert_eq!(catalog.uninstallers, found("new"));
        catalog.take(Found::Uninstallers(HashMap::new()));
        assert!(catalog.uninstallers.is_empty(), "uninstalled since");
    }
}
