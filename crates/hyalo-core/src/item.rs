//! An item: one row in the results list.
//!
//! The UI calls these results; the type is `Item` so it does not clash with
//! `std::result::Result`.

/// One row in the results list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stays the same across runs, so frecency can find the item again.
    pub id: ItemId,
    /// The name the query is matched against, such as "Visual Studio Code".
    pub title: String,
    /// The second line, such as a path. Empty when there is none.
    pub subtitle: String,
    pub kind: Kind,
    /// What Enter does.
    pub action: Action,
}

/// A stable key for an item: an app's AppUserModelID or shortcut path, a
/// file's full path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    App,
    File,
    Folder,
    Calculator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Hand the target to the shell: a path, or a `shell:AppsFolder\…` URI
    /// for a Store app.
    Open(String),
    /// Put the text on the clipboard, such as a calculator answer.
    CopyText(String),
}
