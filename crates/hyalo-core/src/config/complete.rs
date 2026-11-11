//! Writing out what an older config.toml leaves out, so the file shows
//! every setting the settings window does: text in, text out.

use toml_edit::{DocumentMut, Item, Table};

use super::edit::line_endings;
use super::{List, default_file, parse, written};
use crate::text::Language;

/// `text` with the settings it leaves out written in, each with the
/// words the file written at first has on it, Wikipedia in the language
/// the file names, else in `language`, the one Windows speaks; `None`
/// when it leaves out nothing, or does not read. The language goes in as
/// a comment: written,
/// it would stop following the language Windows speaks. A `[search]` the
/// file has stays as it is, even empty: that is no web searches.
pub fn completed(text: &str, language: Language) -> Option<String> {
    let language = parse(text).ok()?.language.unwrap_or(language);
    let mut document: DocumentMut = text.parse().ok()?;
    let first: DocumentMut = default_file(language)
        .parse()
        .expect("the first file reads");
    let mut changed = settings(&mut document, &first);
    changed |= tables(&mut document, &first);
    changed |= said(&mut document, text, &first, language);
    let after = line_endings(text, document.to_string());
    (changed && written(&after).is_ok()).then_some(after)
}

/// Writes in the settings at the top `first` has and `document` leaves
/// out, but the language; whether there were any.
fn settings(document: &mut DocumentMut, first: &DocumentMut) -> bool {
    let missing: Vec<_> = first
        .iter()
        .filter(|(key, item)| item.is_value() && *key != "language" && !document.contains_key(key))
        .filter_map(|(key, _)| first.as_table().get_key_value(key))
        .collect();
    for (key, item) in &missing {
        document.insert_formatted(key, (*item).clone());
    }
    !missing.is_empty()
}

/// Writes in the tables `first` has and `document` leaves out, after its
/// own, and the settings of `[folders]` it leaves out; whether there
/// were any.
fn tables(document: &mut DocumentMut, first: &DocumentMut) -> bool {
    let mut changed = false;
    for (name, item) in first.iter().filter(|(_, item)| item.is_table()) {
        let (key, _) = first.as_table().get_key_value(name).expect("listed");
        match document.get_mut(name).and_then(Item::as_table_mut) {
            Some(table) if name != List::Search.key() => {
                changed |= keys(table, item.as_table().expect("a table"));
            }
            Some(_) => {}
            None => {
                let mut table = item.as_table().expect("a table").clone();
                // Without a place of its own a table follows the one
                // before it, and it comes after the document's tables.
                table.set_position(None);
                document.insert_formatted(key, Item::Table(table));
                changed = true;
            }
        }
    }
    changed
}

/// Writes in the settings of `first` that `table` leaves out, at its end.
fn keys(table: &mut Table, first: &Table) -> bool {
    let missing: Vec<_> = first
        .iter()
        .filter(|(key, _)| !table.contains_key(key))
        .filter_map(|(key, _)| first.get_key_value(key))
        .collect();
    for (key, item) in &missing {
        table.insert_formatted(key, (*item).clone());
    }
    !missing.is_empty()
}

/// Says the language in a comment before the first table, with the words
/// `first` has on it, unless `text` names it or says it in a comment
/// already; whether it did.
fn said(document: &mut DocumentMut, text: &str, first: &DocumentMut, language: Language) -> bool {
    let commented = text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('#') && line.trim_start_matches(['#', ' ']).starts_with("language")
    });
    if document.contains_key("language") || commented {
        return false;
    }
    let (key, _) = first
        .as_table()
        .get_key_value("language")
        .expect("a language");
    let words = key.leaf_decor().prefix().and_then(|prefix| prefix.as_str());
    let words = words.unwrap_or("\n");
    let comment = format!("{words}# language = \"{}\"\n", language.code());
    let mut tables: Vec<&mut Table> = document
        .iter_mut()
        .filter_map(|(_, item)| item.as_table_mut())
        .collect();
    // A table written in has no place and comes after the file's own.
    tables.sort_by_key(|table| table.position().unwrap_or(isize::MAX));
    // `tables` wrote in every table the first file has.
    let table = tables.into_iter().next().expect("a table");
    let before = table.decor().prefix().and_then(|prefix| prefix.as_str());
    let prefix = format!("{comment}{}", before.unwrap_or("\n"));
    table.decor_mut().set_prefix(prefix);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DEFAULT, List, default_file, parse, written};

    /// The file a build of 10.11.2026 wrote, before the language, the
    /// update check and the web searches were settings.
    const OLDER: &str = r#"# hyalo's settings. Saved changes apply at once; a mistake keeps the last
# good settings and shows what is wrong under the launcher's search bar.
# Write paths in single quotes, so that backslashes stay as they are.

# Shows and hides the launcher: Ctrl, Alt, Shift or Win and one key,
# joined by +, such as "Ctrl+Shift+K" or "Win+F1".
hotkey = "F4"

[folders]
# Left out of the file index with everything in them: full paths.
# WinSxS keeps a copy of every system file each update brought, hundreds
# of thousands of them, and none is worth opening from here.
exclude = ['C:\Windows\WinSxS']

# Files in these folders rank lower, as in caches, AppData and
# node_modules: a folder name, wherever it is, or a full path.
# Build output of Cargo and Maven lives in target folders.
rank_lower = ['target']

# Files in these folders rank higher, as in Desktop, Documents, Downloads
# and the project folders in your profile: a folder name or a full path.
rank_higher = []
"#;

    fn complete(text: &str) -> String {
        completed(text, Language::Russian).expect("something to add")
    }

    /// Whether the lines of `inner` are lines of `outer`, in their order.
    fn kept(inner: &str, outer: &str) -> bool {
        let mut lines = outer.lines();
        inner.lines().all(|line| lines.any(|other| other == line))
    }

    #[test]
    fn an_older_file_gets_what_it_leaves_out_and_keeps_the_rest_as_it_was() {
        let after = complete(OLDER);
        assert!(kept(OLDER, &after), "{after}");
        assert!(after.contains("\n# Looks for a new version of hyalo on GitHub once a day"));
        assert!(after.contains("\ncheck_for_updates = true\n"), "{after}");
        let at = |text: &str| after.find(text).expect(text);
        // On the user's file it went before [search], the table written in
        // without a place, which sorted first.
        assert!(
            at("\n# language = \"ru\"\n") < at("\n[folders]\n"),
            "among the settings at the top: {after}"
        );
        let config = parse(&after).expect("good settings");
        assert_eq!(config.hotkey.to_string(), "F4");
        assert_eq!(config.language, None, "still the language Windows speaks");
        assert!(config.check_for_updates);
        let searches = written(&after).expect("good settings");
        let defaults = written(&default_file(Language::Russian)).expect("the first file");
        assert_eq!(searches.list(List::Search), defaults.list(List::Search));
    }

    #[test]
    fn a_file_with_every_setting_is_left_alone() {
        assert_eq!(completed(DEFAULT, Language::English), None);
        let russian = default_file(Language::Russian);
        assert_eq!(completed(&russian, Language::Russian), None);
        let after = complete(OLDER);
        assert_eq!(completed(&after, Language::Russian), None, "once is enough");
    }

    #[test]
    fn an_empty_search_table_stays_empty_and_a_missing_folder_list_is_written() {
        let after = complete("hotkey = 'F4'\n\n[folders]\nexclude = []\n\n[search]\n");
        let written = written(&after).expect("good settings");
        assert_eq!(written.list(List::Search), [] as [String; 0], "{after}");
        assert_eq!(written.list(List::Exclude), [] as [String; 0], "as written");
        assert!(after.contains("\nrank_lower = ['target']\n"), "{after}");
        assert!(after.contains("\nrank_higher = []\n"), "{after}");
    }

    #[test]
    fn a_language_written_or_said_in_a_comment_is_not_added_again() {
        let named = complete("language = 'en'\n");
        assert!(!named.contains("# language"), "{named}");
        let wikipedia = "\nw = \"https://en.wikipedia.org/";
        assert!(named.contains(wikipedia), "the file's language: {named}");
        let commented = complete("# language = \"en\"\nhotkey = 'F4'\n");
        assert_eq!(commented.matches("language =").count(), 1, "{commented}");
    }

    #[test]
    fn a_file_that_does_not_read_is_left_alone() {
        assert_eq!(completed("hotkye = 'F4'\n", Language::English), None);
        assert_eq!(completed("hotkey = \n", Language::English), None);
    }

    #[test]
    fn a_file_with_windows_line_endings_keeps_them() {
        let after = complete(&OLDER.replace('\n', "\r\n"));
        assert!(!after.replace("\r\n", "").contains('\n'), "only CRLF");
    }

    #[test]
    fn tables_written_in_follow_the_file_s_own_in_the_first_file_s_order() {
        let after = complete("hotkey = 'F4'\n");
        let at = |header: &str| after.find(header).expect(header);
        assert!(at("\n[folders]\n") < at("\n[search]\n"), "{after}");
    }

    #[test]
    fn a_file_that_leaves_out_only_its_web_searches_gets_them() {
        let russian = default_file(Language::Russian);
        let (without, _) = russian.split_once("\n# Web searches").expect("the table");
        let after = complete(&format!("{without}\n"));
        assert!(after.contains("\n[search]\ng = "), "{after}");
    }

    #[test]
    fn a_file_that_leaves_out_only_the_language_gets_it_in_a_comment() {
        let language = "# What the settings window, the tray menu and the launcher's rows say:
# \"en\" for English or \"ru\" for Russian. Left out, the language Windows
# is shown in.
language = \"en\"

";
        let without = DEFAULT.replacen(language, "", 1);
        assert_ne!(without, DEFAULT, "the language taken out");
        let after = complete(&without);
        assert!(after.contains("\n# language = \"ru\"\n"), "{after}");
    }
}
