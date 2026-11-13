//! The window switcher: a query that starts with `>` lists the open
//! windows, front first as Alt+Tab has them, filtered by what follows;
//! Enter brings the selected one to the front.

use crate::item::{Action, Item, ItemId, Kind};
use crate::rank;

/// What a query starts with to list the windows.
pub const PREFIX: char = '>';

/// What follows `>` in `query`, trimmed: the filter, empty for every
/// window; `None` when the query does not list windows.
pub fn filter(query: &str) -> Option<&str> {
    query.trim_start().strip_prefix(PREFIX).map(str::trim)
}

/// What Windows says of a top-level window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seen {
    pub visible: bool,
    /// Hidden by DWM though visible: on another virtual desktop, or a
    /// Store app that is suspended.
    pub cloaked: bool,
    /// `WS_EX_TOOLWINDOW`: a palette, a tray's hidden window.
    pub tool: bool,
    /// `WS_EX_APPWINDOW`: wants a taskbar button even when owned.
    pub app: bool,
    /// Has an owner: a dialog or a popup of another window.
    pub owned: bool,
    pub titled: bool,
    /// hyalo's own, or the desktop behind every window.
    pub ours: bool,
}

/// Whether Alt+Tab would list a window Windows says `seen` of: shown,
/// titled, not hyalo's, not a tool window, and owned by none unless it
/// asks for a taskbar button.
pub fn switchable(seen: Seen) -> bool {
    let shown = seen.visible && !seen.cloaked && seen.titled;
    let alone = !seen.owned || seen.app;
    shown && alone && !seen.tool && !seen.ours
}

/// The row of an open window: its title, the app it belongs to as the
/// second line; Enter switches to it. `window` is its handle as a number.
pub fn item(window: isize, title: &str, app: &str) -> Item {
    Item {
        id: ItemId(format!("hyalo:window:{window:x}")),
        title: title.to_string(),
        subtitle: app.to_string(),
        keywords: Vec::new(),
        kind: Kind::Window,
        action: Action::Switch(window),
    }
}

/// The windows `filter` finds, best first, each with the letters of its
/// title it matched: by its title, or, a little weaker and with nothing
/// to highlight, by its app's name. Equal ones keep their order, front
/// first; an empty filter keeps every window.
pub fn ranked(filter: &str, windows: &[Item]) -> Vec<(Item, Vec<usize>)> {
    rank::listed(filter, windows, |window| vec![window.subtitle.as_str()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_starting_with_the_prefix_lists_windows_filtered_by_the_rest() {
        assert_eq!(filter(">"), Some(""));
        assert_eq!(filter("> "), Some(""));
        assert_eq!(filter(">code"), Some("code"));
        assert_eq!(filter("  >  main.rs  "), Some("main.rs"));
        assert_eq!(filter("code"), None);
        assert_eq!(filter("a>b"), None);
        assert_eq!(filter(""), None);
    }

    const WINDOW: Seen = Seen {
        visible: true,
        cloaked: false,
        tool: false,
        app: false,
        owned: false,
        titled: true,
        ours: false,
    };

    #[test]
    fn a_shown_titled_window_of_its_own_is_switched_to() {
        assert!(switchable(WINDOW));
        let owned_app = Seen {
            owned: true,
            app: true,
            ..WINDOW
        };
        assert!(switchable(owned_app), "an owned window that wants a button");
        assert!(switchable(Seen {
            app: true,
            ..WINDOW
        }));
    }

    #[test]
    fn hidden_cloaked_untitled_owned_tool_and_hyalos_windows_are_not() {
        let not = [
            Seen {
                visible: false,
                ..WINDOW
            },
            Seen {
                cloaked: true,
                ..WINDOW
            },
            Seen {
                titled: false,
                ..WINDOW
            },
            Seen {
                owned: true,
                ..WINDOW
            },
            Seen {
                tool: true,
                ..WINDOW
            },
            Seen {
                tool: true,
                app: true,
                ..WINDOW
            },
            Seen {
                ours: true,
                ..WINDOW
            },
        ];
        for seen in not {
            assert!(!switchable(seen), "{seen:?}");
        }
    }

    #[test]
    fn a_windows_row_is_its_title_and_app_and_enter_switches_to_it() {
        let item = item(0x1A2B, "main.rs - hyalo", "Visual Studio Code");
        assert_eq!(item.id, ItemId("hyalo:window:1a2b".to_string()));
        assert_eq!(item.title, "main.rs - hyalo");
        assert_eq!(item.subtitle, "Visual Studio Code");
        assert_eq!(item.kind, Kind::Window);
        assert_eq!(item.action, Action::Switch(0x1A2B));
        assert_eq!(item.path(), None, "nothing to show in a folder");
    }

    fn windows() -> Vec<Item> {
        vec![
            item(1, "Downloads", "Windows Explorer"),
            item(2, "notes.txt - Notepad", "Notepad"),
            item(
                3,
                "README.md - hyalo - Visual Studio Code",
                "Visual Studio Code",
            ),
            item(4, "Notepad tips - YouTube", "Google Chrome"),
        ]
    }

    fn titles(ranked: &[(Item, Vec<usize>)]) -> Vec<&str> {
        ranked.iter().map(|(item, _)| item.title.as_str()).collect()
    }

    #[test]
    fn with_nothing_after_the_prefix_every_window_is_listed_front_first() {
        let all = ranked("", &windows());
        let expected: Vec<String> = windows().into_iter().map(|item| item.title).collect();
        assert_eq!(titles(&all), expected);
        assert!(all.iter().all(|(_, matched)| matched.is_empty()));
    }

    #[test]
    fn the_filter_matches_titles_best_first_and_equal_ones_front_first() {
        let found = ranked("note", &windows());
        assert_eq!(
            titles(&found),
            ["notes.txt - Notepad", "Notepad tips - YouTube"]
        );
        assert_eq!(found[0].1, [0, 1, 2, 3]);
        let found = ranked("readme", &windows());
        assert_eq!(titles(&found), ["README.md - hyalo - Visual Studio Code"]);
        assert!(ranked("zzz", &windows()).is_empty());
    }

    #[test]
    fn a_window_is_found_by_its_apps_name_below_one_found_by_its_title() {
        let found = ranked("explorer", &windows());
        assert_eq!(titles(&found), ["Downloads"]);
        assert!(found[0].1.is_empty(), "nothing in the title to mark");
        let both = vec![
            item(1, "Inbox", "Visual Studio Code"),
            item(2, "visual notes", "Paint"),
        ];
        assert_eq!(titles(&ranked("visual", &both)), ["visual notes", "Inbox"]);
    }
}
