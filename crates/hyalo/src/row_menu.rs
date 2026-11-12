//! The menu a row of the launcher opens on a right click, the Menu key or
//! Shift+F10: what the row can do, each entry with the keys that do the
//! same, so the mouse shows the way to the keys. Which entries a row gets
//! is decided here, apart from Win32; an entry picked does what its keys
//! do (`keys::Command`).

use hyalo_core::item::{Action, Item, Kind};
use hyalo_core::text::{Language, Text};
use windows::Win32::Foundation::{HWND, POINT};

use crate::keys::{self, Command};
use crate::popup::{self, Side};

/// A line of a row's menu.
pub type Line = popup::Line<Command>;

/// The menu of `item`'s row, in `language`: what Enter does with it,
/// named for what that is; Run as administrator for an app; Show in
/// folder and Copy path for a file or folder; then, after a line,
/// Uninstall… for an app hyalo knows the uninstaller of (`uninstalls`).
pub fn lines(item: &Item, uninstalls: bool, language: Language) -> Vec<Line> {
    let app = item.kind == Kind::App;
    let file = item.path().is_some();
    let entries = [
        Some((Command::Open { as_admin: false }, enter(&item.action))),
        app.then_some((Command::Open { as_admin: true }, Text::RunAsAdministrator)),
        file.then_some((Command::Reveal, Text::ShowInFolder)),
        file.then_some((Command::Copy, Text::CopyPath)),
    ];
    let entry = |(command, text): (Command, Text)| {
        // A tab puts the keys on the right, as menus show shortcuts.
        let label = format!("{}\t{}", language.say(text), keys::shortcut(command));
        Line::entry(command, label)
    };
    let mut lines: Vec<Line> = entries.into_iter().flatten().map(entry).collect();
    if uninstalls {
        lines.push(Line::Separator);
        lines.push(entry((Command::Uninstall, Text::Uninstall)));
    }
    lines
}

/// What Enter does with a row whose action is `action`, as its menu
/// names it.
fn enter(action: &Action) -> Text {
    match action {
        Action::CopyText(_) => Text::Copy,
        Action::Switch(_) => Text::SwitchTo,
        Action::System(_) => Text::Run,
        _ => Text::Open,
    }
}

/// What the window asks of a row's menu: the real one in `Popup`, a
/// stand-in in tests.
pub trait RowMenu {
    /// Opens a menu of `lines` for `window` at `at`, in screen pixels,
    /// until the user picks an entry or clicks away: what was picked, if
    /// anything.
    fn open(&self, window: HWND, lines: &[Line], at: POINT) -> Option<Command>;
}

/// The menu Windows shows.
pub struct Popup;

impl RowMenu for Popup {
    fn open(&self, window: HWND, lines: &[Line], at: POINT) -> Option<Command> {
        popup::track(window, lines, at, Side::Below)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::item::ItemId;
    use hyalo_core::system_commands;

    fn item(kind: Kind, action: Action) -> Item {
        Item {
            id: ItemId("an item".to_string()),
            title: "An item".to_string(),
            subtitle: String::new(),
            keywords: Vec::new(),
            kind,
            action,
        }
    }

    fn app() -> Item {
        let open = Action::Open(r"shell:AppsFolder\Everything".to_string());
        item(Kind::App, open)
    }

    /// Each line's label, or `None` for a separator.
    fn labels(lines: &[Line]) -> Vec<Option<&str>> {
        lines.iter().map(label).collect()
    }

    fn label(line: &Line) -> Option<&str> {
        match line {
            Line::Entry(entry) => Some(entry.label.as_str()),
            Line::Separator => None,
        }
    }

    /// What each entry does.
    fn commands(lines: &[Line]) -> Vec<Command> {
        let command = |line: &Line| match line {
            Line::Entry(entry) => Some(entry.command),
            Line::Separator => None,
        };
        lines.iter().filter_map(command).collect()
    }

    const OPEN: Command = Command::Open { as_admin: false };
    const AS_ADMIN: Command = Command::Open { as_admin: true };

    #[test]
    fn an_app_opens_runs_as_administrator_and_uninstalls_after_a_line_if_hyalo_knows_how() {
        let lines = lines(&app(), true, Language::English);
        let expected = [
            Some("Open\tEnter"),
            Some("Run as administrator\tCtrl+Shift+Enter"),
            None,
            Some("Uninstall…\tShift+Delete"),
        ];
        assert_eq!(labels(&lines), expected);
        assert_eq!(commands(&lines), [OPEN, AS_ADMIN, Command::Uninstall]);
        let unknown = super::lines(&app(), false, Language::English);
        let expected = [
            Some("Open\tEnter"),
            Some("Run as administrator\tCtrl+Shift+Enter"),
        ];
        assert_eq!(labels(&unknown), expected, "no line, no uninstall");
    }

    #[test]
    fn a_file_or_folder_opens_shows_in_its_folder_and_copies_its_path() {
        for folder in [false, true] {
            let file = Item::file(r"C:\Users\Me\notes".to_string(), folder);
            let lines = lines(&file, false, Language::English);
            let expected = [
                Some("Open\tEnter"),
                Some("Show in folder\tCtrl+Enter"),
                Some("Copy path\tCtrl+C"),
            ];
            assert_eq!(labels(&lines), expected, "folder: {folder}");
            let done = [OPEN, Command::Reveal, Command::Copy];
            assert_eq!(commands(&lines), done);
        }
    }

    #[test]
    fn other_rows_offer_what_enter_does_named_for_what_it_does() {
        let rows = [
            (item(Kind::Calculator, Action::CopyText("6".into())), "Copy"),
            (item(Kind::Window, Action::Switch(42)), "Switch to"),
            (
                system_commands::Command::Restart.item(Language::English),
                "Run",
            ),
            (item(Kind::Web, Action::Open("https://x".into())), "Open"),
            (
                item(Kind::Setting, Action::Open("ms-settings:".into())),
                "Open",
            ),
            (item(Kind::Notice, Action::Settings), "Open"),
        ];
        for (row, said) in rows {
            let lines = lines(&row, false, Language::English);
            let expected = format!("{said}\tEnter");
            assert_eq!(labels(&lines), [Some(expected.as_str())], "{row:?}");
            assert_eq!(commands(&lines), [OPEN]);
        }
    }

    #[test]
    fn the_menu_speaks_russian_in_russian_and_names_the_keys_as_they_are() {
        let lines = lines(&app(), true, Language::Russian);
        let expected = [
            Some("Открыть\tEnter"),
            Some("Запуск от имени администратора\tCtrl+Shift+Enter"),
            None,
            Some("Удалить…\tShift+Delete"),
        ];
        assert_eq!(labels(&lines), expected);
        let file = Item::file(r"C:\notes.txt".to_string(), false);
        let lines = super::lines(&file, false, Language::Russian);
        let expected = [
            Some("Открыть\tEnter"),
            Some("Показать в папке\tCtrl+Enter"),
            Some("Копировать путь\tCtrl+C"),
        ];
        assert_eq!(labels(&lines), expected);
    }
}
