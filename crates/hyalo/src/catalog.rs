//! What the launcher can list: items and their icons, found off the UI
//! thread by a provider and handed over piece by piece.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hyalo_core::item::{Item, ItemId};
use windows::core::Result;

use crate::apps::{self, Icon};
use crate::com::Com;

/// One piece of what a provider found.
pub enum Found {
    /// Everything there is to list, replacing what was listed before.
    Items(Vec<Item>),
    /// The icon of one item.
    Icon(ItemId, Icon),
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

/// The items to search and the icons found for them so far.
#[derive(Default)]
pub struct Catalog {
    pub items: Vec<Item>,
    pub icons: HashMap<ItemId, Icon>,
}

impl Catalog {
    /// Takes in one piece. Returns whether the items changed, the only
    /// change that calls for searching again.
    pub fn take(&mut self, found: Found) -> bool {
        match found {
            Found::Items(items) => {
                let changed = items != self.items;
                self.items = items;
                changed
            }
            Found::Icon(id, icon) => {
                self.icons.insert(id, icon);
                false
            }
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

/// How long a list is trusted before showing the launcher looks again.
/// Listing the apps costs half a second of a core, too much for every
/// show.
const REFRESH_AFTER: Duration = Duration::from_secs(5 * 60);

/// Whether a list found at `found_at` (`None`: never) is due for
/// another look at `now`.
pub fn stale(found_at: Option<Instant>, now: Instant) -> bool {
    found_at.is_none_or(|at| now.duration_since(at) >= REFRESH_AFTER)
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
    fn new_items_replace_the_old_and_call_for_a_search() {
        let mut catalog = Catalog::default();
        assert!(catalog.take(Found::Items(vec![app("Paint")])));
        assert!(catalog.take(Found::Items(vec![app("Notepad")])));
        assert_eq!(catalog.items, [app("Notepad")]);
    }

    #[test]
    fn the_same_items_again_change_nothing() {
        let mut catalog = Catalog::default();
        catalog.take(Found::Items(vec![app("Paint")]));
        assert!(!catalog.take(Found::Items(vec![app("Paint")])));
    }

    #[test]
    fn an_icon_is_kept_for_its_item_without_a_search() {
        let mut catalog = Catalog::default();
        let paint = ItemId("Paint".to_string());
        assert!(!catalog.take(Found::Icon(paint.clone(), dot())));
        assert_eq!(catalog.icons.get(&paint), Some(&dot()));
    }

    #[test]
    fn a_list_never_found_is_stale() {
        assert!(stale(None, Instant::now()));
    }

    #[test]
    fn a_list_is_trusted_for_five_minutes() {
        let found = Instant::now();
        let later = |seconds| found + Duration::from_secs(seconds);
        assert!(!stale(Some(found), found));
        assert!(!stale(Some(found), later(299)));
        assert!(stale(Some(found), later(300)));
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
