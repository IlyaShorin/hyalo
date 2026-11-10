//! What the launcher can list: items and their icons, found off the UI
//! thread by a provider and handed over piece by piece.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hyalo_core::item::{Item, ItemId};
use hyalo_core::rates::Rates;
use windows::core::Result;

use crate::apps::{self, Icon};
use crate::com::Com;

/// One piece of what a provider found.
pub enum Found {
    /// Everything there is to list, replacing what was listed before.
    Items(Vec<Item>),
    /// The icon of one item.
    Icon(ItemId, Icon),
    /// The file index changed: search it again.
    Files,
    /// Picked files and folders that are still there.
    Picked(Vec<Item>),
    /// The newest exchange rates.
    Rates(Rates),
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
    pub icons: HashMap<ItemId, Icon>,
    /// Files and folders picked before, ranked with the items so that
    /// frecency lifts them however many files match as well.
    pub picked: Vec<Item>,
    /// What currency answers convert with; `None` until found.
    pub rates: Option<Rates>,
}

impl Catalog {
    /// Takes in one piece.
    pub fn take(&mut self, found: Found) {
        match found {
            Found::Items(items) => self.items = items,
            Found::Icon(id, icon) => {
                self.icons.insert(id, icon);
            }
            Found::Files => {}
            Found::Picked(items) => items.into_iter().for_each(|item| self.pick(item)),
            Found::Rates(rates) => self.rates = Some(rates),
        }
    }

    /// Remembers a picked file or folder, once.
    pub fn pick(&mut self, item: Item) {
        if !self.picked.iter().any(|picked| picked.id == item.id) {
            self.picked.push(item);
        }
    }
}

/// The apps provider: lists the Applications folder, reports the items,
/// then the icons the window lacks, one at a time, so search works while
/// icons still come in (listing takes about 0.5 s, icons as long again).
pub fn find_apps(request: &Request, report: &dyn Fn(Found)) {
    if let Err(error) = report_apps(request, report) {
        eprintln!("hyalo: cannot list the apps: {error}");
    }
}

fn report_apps(request: &Request, report: &dyn Fn(Found)) -> Result<()> {
    let _com = Com::init()?;
    let entries = apps::find()?;
    report(Found::Items(
        entries.iter().map(|entry| entry.item.clone()).collect(),
    ));
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
    use hyalo_core::item::{Action, Kind};
    use std::cell::RefCell;

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: String::new(),
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
        let found = RefCell::new(Vec::new());
        file_icons(&ids, 32, &|piece| found.borrow_mut().push(piece));
        let found = found.into_inner();
        assert_eq!(found.len(), 1, "one icon, none for what is not there");
        let icon = icon_of(&found, &ids[0].0).expect("explorer's icon");
        assert_eq!((icon.width, icon.height), (32, 32));
    }

    const EXPLORER: &str = "Microsoft.Windows.Explorer";

    /// Everything `find_apps` reports for `request`.
    fn found(request: &Request) -> Vec<Found> {
        let found = RefCell::new(Vec::new());
        find_apps(request, &|piece| found.borrow_mut().push(piece));
        found.into_inner()
    }

    fn icon_of<'a>(found: &'a [Found], id: &str) -> Option<&'a Icon> {
        found.iter().find_map(|piece| match piece {
            Found::Icon(of, icon) if of.0 == id => Some(icon),
            _ => None,
        })
    }

    #[test]
    fn the_apps_provider_reports_the_apps_then_only_the_icons_not_known() {
        let mut request = Request {
            icon_size: 32,
            known: HashSet::new(),
        };
        let first = found(&request);
        let Some(Found::Items(items)) = first.first() else {
            panic!("the items come first");
        };
        assert!(items.iter().any(|item| item.id.0 == EXPLORER));
        let icon = icon_of(&first, EXPLORER).expect("Explorer's icon");
        assert_eq!((icon.width, icon.height), (32, 32));
        request.known = items.iter().map(|item| item.id.clone()).collect();
        assert_eq!(found(&request).len(), 1, "the items again, no icons");
    }
}
