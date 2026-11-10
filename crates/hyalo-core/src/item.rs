//! An item: one row in the results list.
//!
//! The UI calls these results; the type is `Item` so it does not clash with
//! `std::result::Result`.

use serde::{Deserialize, Serialize};

use crate::system_commands::Command;
use crate::text::Text;

/// One row in the results list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stays the same across runs, so frecency can find the item again.
    pub id: ItemId,
    /// The name the query is matched against, such as "Visual Studio Code".
    pub title: String,
    /// The second line, such as a path. Empty when there is none.
    pub subtitle: String,
    /// Other names the query finds the item by, never shown: a Windows
    /// setting's name in the other language and words people search it
    /// by.
    pub keywords: Vec<String>,
    pub kind: Kind,
    /// What Enter does.
    pub action: Action,
}

/// A stable key for an item: an app's AppUserModelID or shortcut path, a
/// file's full path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    App,
    File,
    Folder,
    Calculator,
    /// One of Windows' own settings: a page of the Settings app or a
    /// Control Panel item.
    Setting,
    /// A system command, such as Restart.
    Command,
    /// A web search for what was typed after a prefix: never ranked,
    /// never remembered as a pick.
    Web,
    /// What hyalo tells the user, such as a mistake in the config: never
    /// ranked, never remembered as a pick.
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Hand the target to the shell: a path, or a `shell:AppsFolder\…` URI
    /// for a Store app.
    Open(String),
    /// Start `program` with `arguments`, such as
    /// `rundll32.exe sysdm.cpl,EditEnvironmentVariables`.
    Launch { program: String, arguments: String },
    /// Carry out a system command, such as restarting.
    System(Command),
    /// Open the folder a file or folder is in, with it selected.
    Reveal(String),
    /// Put the text on the clipboard, such as a calculator answer.
    CopyText(String),
    /// Open hyalo's settings window: the launcher does it itself.
    Settings,
    /// Open a text file, such as the config, in the app Windows has for
    /// its kind, or in Notepad when there is none.
    Edit(String),
}

impl Action {
    /// What the row says after the first Enter, for an action that asks
    /// before it runs (a system command such as Restart); `None` for one
    /// that runs at once.
    pub fn asking(&self) -> Option<Text> {
        match self {
            Action::System(command) => command.asking(),
            _ => None,
        }
    }
}

impl Item {
    /// The file or folder at the full path `path`: its own name is the
    /// title, the folder it is in the subtitle, and Enter opens it.
    pub fn file(path: String, folder: bool) -> Item {
        let (parent, name) = path.rsplit_once('\\').unwrap_or(("", &path));
        // A drive's root is `C:\`; `C:` alone is the current folder there.
        let mut subtitle = parent.to_string();
        if subtitle.ends_with(':') {
            subtitle.push('\\');
        }
        Item {
            id: ItemId(path.clone()),
            title: name.to_string(),
            subtitle,
            keywords: Vec::new(),
            kind: match folder {
                true => Kind::Folder,
                false => Kind::File,
            },
            action: Action::Open(path),
        }
    }

    /// The full path of a file or folder; `None` for anything else.
    pub fn path(&self) -> Option<&str> {
        let file = matches!(self.kind, Kind::File | Kind::Folder);
        file.then_some(self.id.0.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_titled_by_its_name_and_lives_in_its_folder() {
        let file = Item::file(r"C:\Users\Me\notes.txt".to_string(), false);
        assert_eq!(file.id, ItemId(r"C:\Users\Me\notes.txt".to_string()));
        assert_eq!(file.title, "notes.txt");
        assert_eq!(file.subtitle, r"C:\Users\Me");
        assert_eq!(file.kind, Kind::File);
        assert_eq!(
            file.action,
            Action::Open(r"C:\Users\Me\notes.txt".to_string())
        );
    }

    #[test]
    fn a_folder_is_a_folder() {
        let folder = Item::file(r"D:\Work\hyalo".to_string(), true);
        assert_eq!(folder.kind, Kind::Folder);
        assert_eq!(folder.title, "hyalo");
    }

    #[test]
    fn something_at_the_root_of_a_drive_lives_in_the_drive_itself() {
        let file = Item::file(r"C:\pagefile.sys".to_string(), false);
        assert_eq!(file.subtitle, r"C:\");
    }

    #[test]
    fn only_a_system_command_that_asks_first_asks() {
        let restart = Action::System(Command::Restart);
        assert_eq!(restart.asking(), Some(Text::AskRestart));
        assert_eq!(Action::System(Command::Lock).asking(), None);
        assert_eq!(
            Action::Open("ms-settings:display".to_string()).asking(),
            None
        );
    }

    #[test]
    fn files_and_folders_have_a_path_and_nothing_else_does() {
        let file = Item::file(r"C:\a.txt".to_string(), false);
        assert_eq!(file.path(), Some(r"C:\a.txt"));
        let folder = Item::file(r"C:\a".to_string(), true);
        assert_eq!(folder.path(), Some(r"C:\a"));
        let app = Item {
            kind: Kind::App,
            ..Item::file(r"C:\app.exe".to_string(), false)
        };
        assert_eq!(app.path(), None);
    }
}
