//! Changing one setting in the config's text and nothing else: the
//! user's comments, order and layout stay as they were. Text in, text
//! out; reading and writing the file is the launcher's.

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use super::{List, Problem, Wanted, Why, Written, engine_of, written};
use crate::hotkey::Hotkey;
use crate::text::Language;

/// One change to the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The hotkey becomes this one.
    Hotkey(Hotkey),
    /// hyalo speaks this language.
    Language(Language),
    /// hyalo looks for a newer release, or stops looking.
    CheckForUpdates(bool),
    /// An entry is added at the end of a list, unless it is there.
    Add(List, String),
    /// An entry, as written, leaves a list; one not there is gone already.
    Remove(List, String),
}

/// Why a change was not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The text has a problem of its own: nothing changes until it is
    /// fixed.
    Broken(Problem),
    /// The change would make the settings wrong, for this reason.
    Wrong(Why),
}

/// `text` with `change` made in it, and only there; `text` as it is
/// when the change makes no difference.
pub fn edit(text: &str, change: &Change) -> Result<String, Refusal> {
    let before = written(text).map_err(Refusal::Broken)?;
    // Both read TOML with the same parser: text whose settings read
    // well parses here too.
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error: toml_edit::TomlError| Refusal::Wrong(Why::Toml(error.to_string())))?;
    if !make(&mut document, &before, change).map_err(Refusal::Wrong)? {
        return Ok(text.to_string());
    }
    let after = line_endings(text, document.to_string());
    written(&after).map_err(|problem| Refusal::Wrong(problem.why))?;
    Ok(after)
}

/// Makes `change` in `document`, whose settings are `before`; whether it
/// changed anything, or why it cannot be made.
fn make(document: &mut DocumentMut, before: &Written, change: &Change) -> Result<bool, Why> {
    match change {
        Change::Add(List::Search, entry) => add_engine(document, entry),
        Change::Remove(List::Search, entry) => Ok(remove_engine(document, before, entry)),
        change => Ok(make_setting(document, before, change)),
    }
}

/// Makes a change to a setting or a list of folders in `document`.
fn make_setting(document: &mut DocumentMut, before: &Written, change: &Change) -> bool {
    match change {
        Change::Hotkey(hotkey) => {
            let differs = *hotkey != before.hotkey;
            differs && set(document, "hotkey", hotkey.to_string().into())
        }
        Change::Language(language) => {
            let differs = Some(*language) != before.language;
            differs && set(document, "language", language.code().into())
        }
        Change::CheckForUpdates(on) => {
            let differs = *on != before.check_for_updates;
            differs && set(document, "check_for_updates", (*on).into())
        }
        Change::Add(list, entry) => add(document, before, *list, entry),
        Change::Remove(list, entry) => remove(document, before, *list, entry),
    }
}

/// `after` with the line endings of `before`: toml_edit ends every line
/// it writes with a line feed, and a file saved on Windows may end its
/// lines with a carriage return too.
fn line_endings(before: &str, after: String) -> String {
    match before.contains("\r\n") {
        true => after.replace("\r\n", "\n").replace('\n', "\r\n"),
        false => after,
    }
}

/// Puts `value` as the setting `key` at the file's top in the place of
/// the one written, keeping what is around it; adds the setting when the
/// file leaves it out. Always a change.
fn set(document: &mut DocumentMut, key: &str, mut value: Value) -> bool {
    match document.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            document.insert(key, Item::Value(value));
        }
    }
    true
}

/// Writes the web search `entry` (`prefix address`) in `[search]`, which
/// is added when the file has none; a prefix there already, in any case,
/// takes the new address.
fn add_engine(document: &mut DocumentMut, entry: &str) -> Result<bool, Why> {
    let wrong = || Why::Value(Wanted::Engine, entry.trim().to_string());
    let (prefix, url) = engine_of(entry).ok_or_else(wrong)?;
    let search = document
        .entry(List::Search.key())
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(wrong)?;
    let written = search
        .iter()
        .map(|(key, _)| key.to_string())
        .find(|key| key.to_lowercase() == prefix.to_lowercase());
    let key = written.unwrap_or_else(|| prefix.to_string());
    let mut value = Value::from(url);
    match search.get_mut(&key).and_then(Item::as_value_mut) {
        Some(old) if old.as_str() == Some(url) => return Ok(false),
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            search.insert(&key, Item::Value(value));
        }
    }
    Ok(true)
}

/// Takes the web search listed as `entry` out of `[search]`.
fn remove_engine(document: &mut DocumentMut, before: &Written, entry: &str) -> bool {
    let listed = before.list(List::Search).iter().any(|old| old == entry);
    let prefix = engine_of(entry).map(|(prefix, _)| prefix);
    let search = document
        .get_mut(List::Search.key())
        .and_then(Item::as_table_like_mut);
    match (listed, prefix, search) {
        (true, Some(prefix), Some(search)) => search.remove(prefix).is_some(),
        _ => false,
    }
}

/// Adds `entry` at the end of `list`, unless an entry the same but for
/// case is there: paths and folder names compare that way on Windows.
fn add(document: &mut DocumentMut, before: &Written, list: List, entry: &str) -> bool {
    let entries = before.list(list);
    if entries
        .iter()
        .any(|old| old.to_lowercase() == entry.to_lowercase())
    {
        return false;
    }
    match array(document, list) {
        Some(array) => push(array, entry),
        None => {
            let mut all = entries.to_vec();
            all.push(entry.to_string());
            set_array(document, list, &all);
        }
    }
    true
}

/// Takes `entry`, as written, out of `list`.
fn remove(document: &mut DocumentMut, before: &Written, list: List, entry: &str) -> bool {
    let entries = before.list(list);
    let Some(at) = entries.iter().position(|old| old == entry) else {
        return false;
    };
    match array(document, list) {
        Some(array) => take(array, at),
        None => {
            let mut all = entries.to_vec();
            all.remove(at);
            set_array(document, list, &all);
        }
    }
    true
}

/// The array `list` is written as, if the file writes it.
fn array(document: &mut DocumentMut, list: List) -> Option<&mut Array> {
    let folders = document.get_mut("folders")?.as_table_like_mut()?;
    folders.get_mut(list.key())?.as_array_mut()
}

/// Writes `list` with `entries`, in `[folders]`, which is added when the
/// file has none.
fn set_array(document: &mut DocumentMut, list: List, entries: &[String]) {
    let mut array = Array::new();
    for entry in entries {
        push(&mut array, entry);
    }
    let folders = document
        .entry("folders")
        .or_insert_with(|| Item::Table(Table::new()));
    if let Some(folders) = folders.as_table_like_mut() {
        folders.insert(list.key(), Item::Value(Value::Array(array)));
    }
}

/// Adds `entry` at the end of `array`, on a line of its own when the
/// entries before it have one each.
fn push(array: &mut Array, entry: &str) {
    let mut value = string(entry);
    // The new line is indented as the last entry's, without the comments
    // that may be above it.
    let own_line = array.iter().last().and_then(|last| {
        let prefix = last.decor().prefix()?.as_str()?;
        let (_, indent) = prefix.rsplit_once('\n')?;
        Some(format!("\n{indent}"))
    });
    if let Some(prefix) = own_line {
        value.decor_mut().set_prefix(prefix);
    }
    array.push_formatted(value);
}

/// Takes the entry at `at` out of `array`. The entry after the first
/// takes its place at the start, and an array left empty closes up.
fn take(array: &mut Array, at: usize) {
    let removed = array.remove(at);
    let lead = removed.decor().prefix().cloned().unwrap_or_default();
    if let Some(first) = array.get_mut(0).filter(|_| at == 0) {
        let own_line = first.decor().prefix().and_then(|prefix| prefix.as_str());
        if !own_line.is_some_and(|prefix| prefix.contains('\n')) {
            first.decor_mut().set_prefix(lead);
        }
    }
    let blank = array
        .trailing()
        .as_str()
        .is_some_and(|rest| rest.trim().is_empty());
    // toml_edit writes no trailing comma in an empty array.
    if array.is_empty() && blank {
        array.set_trailing("");
    }
}

/// `entry` as a TOML string: in single quotes, as the file writes its
/// entries, so that a path's backslashes stay as they are; as toml_edit
/// writes it when single quotes cannot hold it.
fn string(entry: &str) -> Value {
    let literal = !entry.contains(|letter: char| letter == '\'' || letter.is_control());
    let quoted = format!("'{entry}'");
    match literal {
        true => quoted.parse().unwrap_or_else(|_| Value::from(entry)),
        false => Value::from(entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT;

    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("a hotkey")
    }

    fn edited(text: &str, change: Change) -> String {
        edit(text, &change).expect("changed")
    }

    fn add(list: List, entry: &str) -> Change {
        Change::Add(list, entry.to_string())
    }

    fn remove(list: List, entry: &str) -> Change {
        Change::Remove(list, entry.to_string())
    }

    #[test]
    fn a_new_hotkey_changes_its_value_and_nothing_else() {
        let after = edited(DEFAULT, Change::Hotkey(hotkey("F4")));
        let expected = DEFAULT.replace("hotkey = \"Alt+Space\"", "hotkey = \"F4\"");
        assert_eq!(after, expected);
        assert_ne!(after, DEFAULT);
    }

    #[test]
    fn the_hotkey_keeps_the_spaces_and_the_comment_around_it() {
        let text = "# mine\nhotkey   =   'Ctrl+K'   # since May\n\n[folders]\n";
        let after = edited(text, Change::Hotkey(hotkey("win+f2")));
        assert_eq!(
            after,
            "# mine\nhotkey   =   \"Win+F2\"   # since May\n\n[folders]\n"
        );
    }

    #[test]
    fn a_hotkey_the_file_leaves_out_is_added_before_the_folders() {
        let text = "[folders]\nexclude = []\n";
        let after = edited(text, Change::Hotkey(hotkey("F4")));
        assert_eq!(after, "hotkey = \"F4\"\n[folders]\nexclude = []\n");
    }

    #[test]
    fn the_hotkey_it_has_changes_nothing() {
        let text = "hotkey = 'alt+space' # as typed\n";
        assert_eq!(edited(text, Change::Hotkey(Hotkey::default())), text);
        assert_eq!(edited("", Change::Hotkey(Hotkey::default())), "");
    }

    #[test]
    fn an_entry_is_added_at_the_end_in_single_quotes() {
        let after = edited(DEFAULT, add(List::Exclude, r"D:\old"));
        let expected = DEFAULT.replace(
            r"exclude = ['C:\Windows\WinSxS']",
            r"exclude = ['C:\Windows\WinSxS', 'D:\old']",
        );
        assert_eq!(after, expected);
    }

    #[test]
    fn an_entry_added_to_an_empty_list_is_its_only_one() {
        let after = edited(DEFAULT, add(List::RankHigher, "notes"));
        let expected = DEFAULT.replace("rank_higher = []", "rank_higher = ['notes']");
        assert_eq!(after, expected);
    }

    #[test]
    fn an_entry_added_to_a_list_of_lines_gets_a_line_of_its_own() {
        let text = "[folders]\nexclude = [\n  # copies\n  'C:\\Windows\\WinSxS',\n] # done\n";
        let after = edited(text, add(List::Exclude, r"D:\old"));
        let expected = "[folders]\nexclude = [\n  # copies\n  'C:\\Windows\\WinSxS',\n  'D:\\old',\n] # done\n";
        assert_eq!(after, expected);
    }

    #[test]
    fn a_list_the_file_leaves_out_starts_from_its_defaults() {
        let text = "[folders]\nexclude = []\n";
        let after = edited(text, add(List::RankLower, "vendor"));
        assert_eq!(
            after,
            "[folders]\nexclude = []\nrank_lower = ['target', 'vendor']\n"
        );
        let after = edited(text, remove(List::RankLower, "target"));
        assert_eq!(after, "[folders]\nexclude = []\nrank_lower = []\n");
    }

    #[test]
    fn folders_the_file_leaves_out_are_added_at_its_end() {
        let text = "hotkey = \"F4\" # mine\n";
        let after = edited(text, add(List::RankHigher, r"D:\notes"));
        assert_eq!(
            after,
            "hotkey = \"F4\" # mine\n\n[folders]\nrank_higher = ['D:\\notes']\n"
        );
    }

    #[test]
    fn folders_written_inline_or_dotted_are_changed_where_they_are() {
        let inline = "folders = { exclude = [] }\n";
        let after = edited(inline, add(List::Exclude, r"D:\old"));
        assert_eq!(after, "folders = { exclude = ['D:\\old'] }\n");
        let dotted = "folders.rank_higher = ['notes']\n";
        let after = edited(dotted, remove(List::RankHigher, "notes"));
        assert_eq!(after, "folders.rank_higher = []\n");
    }

    #[test]
    fn an_entry_removed_takes_nothing_else_with_it() {
        let after = edited(DEFAULT, remove(List::Exclude, r"C:\Windows\WinSxS"));
        let expected = DEFAULT.replace(r"exclude = ['C:\Windows\WinSxS']", "exclude = []");
        assert_eq!(after, expected);
        let text = "[folders]\nrank_lower = ['a', 'b', 'c'] # three\n";
        assert_eq!(
            edited(text, remove(List::RankLower, "b")),
            "[folders]\nrank_lower = ['a', 'c'] # three\n"
        );
        assert_eq!(
            edited(text, remove(List::RankLower, "a")),
            "[folders]\nrank_lower = ['b', 'c'] # three\n"
        );
        assert_eq!(
            edited(text, remove(List::RankLower, "c")),
            "[folders]\nrank_lower = ['a', 'b'] # three\n"
        );
    }

    #[test]
    fn an_entry_removed_from_a_list_of_lines_takes_its_own_comment() {
        let text = "[folders]\nexclude = [\n  # copies\n  'C:\\Windows\\WinSxS',\n  # old\n  'D:\\old',\n]\n";
        let after = edited(text, remove(List::Exclude, r"D:\old"));
        assert_eq!(
            after,
            "[folders]\nexclude = [\n  # copies\n  'C:\\Windows\\WinSxS',\n]\n"
        );
        let after = edited(text, remove(List::Exclude, r"C:\Windows\WinSxS"));
        assert_eq!(after, "[folders]\nexclude = [\n  # old\n  'D:\\old',\n]\n");
    }

    #[test]
    fn a_list_of_lines_emptied_closes_up() {
        let text = "[folders]\nexclude = [\n  'D:\\old',\n]\n";
        let after = edited(text, remove(List::Exclude, r"D:\old"));
        assert_eq!(after, "[folders]\nexclude = []\n");
        let noted = "[folders]\nexclude = [\n  'D:\\old',\n  # keep this\n]\n";
        let after = edited(noted, remove(List::Exclude, r"D:\old"));
        assert_eq!(after, "[folders]\nexclude = [\n  # keep this\n]\n");
    }

    #[test]
    fn what_is_not_there_is_not_removed_and_what_is_there_not_added_again() {
        assert_eq!(edited(DEFAULT, remove(List::Exclude, r"D:\never")), DEFAULT);
        assert_eq!(edited(DEFAULT, remove(List::RankLower, "Target")), DEFAULT);
        assert_eq!(
            edited(DEFAULT, add(List::Exclude, r"c:\windows\winsxs")),
            DEFAULT
        );
        assert_eq!(edited(DEFAULT, add(List::RankLower, "TARGET")), DEFAULT);
    }

    #[test]
    fn a_path_single_quotes_cannot_hold_is_written_in_triple_quotes() {
        let after = edited(DEFAULT, add(List::RankHigher, r"D:\it's"));
        assert!(after.contains(r"rank_higher = ['''D:\it's''']"), "{after}");
        let back = written(&after).expect("good settings");
        assert_eq!(back.list(List::RankHigher), [r"D:\it's"]);
    }

    #[test]
    fn a_name_in_quotes_of_its_own_reads_back_as_it_was_typed() {
        // In single quotes it would be `'''notes'''`, which TOML reads as
        // a string of three quotes around `notes`, without the quotes.
        let after = edited(DEFAULT, add(List::RankHigher, "''notes''"));
        let back = written(&after).expect("good settings");
        assert_eq!(back.list(List::RankHigher), ["''notes''"]);
    }

    #[test]
    fn what_is_written_reads_back_as_the_change() {
        let after = edited(DEFAULT, add(List::RankLower, r"D:\Archive"));
        let back = written(&after).expect("good settings");
        assert_eq!(back.list(List::RankLower), ["target", r"D:\Archive"]);
        assert_eq!(back.list(List::Exclude), [r"C:\Windows\WinSxS"]);
        assert_eq!(back.hotkey, Hotkey::default());
    }

    #[test]
    fn lines_ending_in_carriage_returns_keep_them() {
        let text = DEFAULT.replace('\n', "\r\n");
        let after = edited(&text, Change::Hotkey(hotkey("F4")));
        assert_eq!(after, text.replace("\"Alt+Space\"", "\"F4\""));
    }

    #[test]
    fn a_broken_file_is_not_changed_and_its_problem_is_told() {
        for change in [Change::Hotkey(hotkey("F4")), add(List::Exclude, r"D:\old")] {
            let refused = edit("hotkey = 'F4'\n[folders\n", &change);
            let Err(Refusal::Broken(problem)) = refused else {
                panic!("{refused:?}");
            };
            assert_eq!(problem.line, 2);
        }
        let refused = edit("\nhotkye = 'F4'\n", &remove(List::Exclude, "x"));
        assert!(matches!(
            refused,
            Err(Refusal::Broken(Problem { line: 2, .. }))
        ));
    }

    #[test]
    fn a_language_takes_the_place_of_the_one_written_and_keeps_its_comment() {
        let after = edited(DEFAULT, Change::Language(Language::Russian));
        assert_eq!(after, crate::config::default_file(Language::Russian));
        assert_eq!(edited(&after, Change::Language(Language::Russian)), after);
    }

    #[test]
    fn a_language_left_out_is_added_among_the_settings_at_the_top() {
        let text = "hotkey = 'F4'\n\n[folders]\nexclude = []\n";
        let after = edited(text, Change::Language(Language::Russian));
        assert_eq!(
            after,
            "hotkey = 'F4'\nlanguage = \"ru\"\n\n[folders]\nexclude = []\n"
        );
        assert_eq!(
            edited("", Change::Language(Language::English)),
            "language = \"en\"\n"
        );
    }

    const SO: &str = "so https://stackoverflow.com/search?q={query}";

    #[test]
    fn a_web_search_is_added_to_the_search_table_under_its_comments() {
        let after = edited(DEFAULT, add(List::Search, SO));
        let expected = format!(
            "{DEFAULT}so = \"https://stackoverflow.com/search?q={{query}}\"
"
        );
        assert_eq!(after, expected);
        let back = written(&after).expect("good settings");
        assert_eq!(back.list(List::Search), [SO]);
    }

    #[test]
    fn a_search_table_the_file_leaves_out_is_added_at_its_end() {
        let text = "hotkey = 'F4'
";
        let after = edited(text, add(List::Search, "  so   https://a.io/?q={query} "));
        assert_eq!(
            after,
            "hotkey = 'F4'

[search]
so = \"https://a.io/?q={query}\"
"
        );
    }

    #[test]
    fn a_prefix_written_already_takes_the_new_address_in_its_place() {
        let text = "[search]
SO = 'https://old.io/{query}' # mine
w = 'https://w.io/{query}'
";
        let after = edited(text, add(List::Search, SO));
        assert_eq!(
            after,
            "[search]
SO = \"https://stackoverflow.com/search?q={query}\" # mine
w = 'https://w.io/{query}'
"
        );
        assert_eq!(
            edited(
                &after,
                add(
                    List::Search,
                    "SO https://stackoverflow.com/search?q={query}"
                )
            ),
            after
        );
    }

    #[test]
    fn a_web_search_removed_takes_only_its_own_line() {
        let text = "[search]
# mine
so = 'https://stackoverflow.com/search?q={query}'
w = 'https://w.io/{query}'
";
        let after = edited(text, remove(List::Search, SO));
        assert_eq!(
            after,
            "[search]
w = 'https://w.io/{query}'
"
        );
        assert_eq!(
            edited(text, remove(List::Search, "so https://other.io/{query}")),
            text
        );
        assert_eq!(edited(text, remove(List::Search, "so")), text);
        assert_eq!(edited("", remove(List::Search, SO)), "");
        let inline = "search = { so = 'https://stackoverflow.com/search?q={query}' }
";
        assert_eq!(
            edited(inline, remove(List::Search, SO)),
            "search = {}
"
        );
    }

    #[test]
    fn a_web_search_without_an_address_or_with_a_wrong_one_is_refused_with_why() {
        let refused = edit(DEFAULT, &add(List::Search, " so "));
        assert_eq!(
            refused,
            Err(Refusal::Wrong(Why::Value(Wanted::Engine, "so".to_string())))
        );
        let refused = edit(DEFAULT, &add(List::Search, "so https://a.io/"));
        assert_eq!(
            refused,
            Err(Refusal::Wrong(Why::Value(
                Wanted::Address,
                "https://a.io/".to_string()
            )))
        );
        let refused = edit(DEFAULT, &add(List::Search, ">w https://a.io/{query}"));
        assert_eq!(
            refused,
            Err(Refusal::Wrong(Why::Value(Wanted::Prefix, ">w".to_string())))
        );
        let refused = edit(
            "search = 5
",
            &add(List::Search, SO),
        );
        assert!(matches!(refused, Err(Refusal::Broken(_))), "{refused:?}");
    }

    #[test]
    fn checking_for_updates_turned_off_is_written_in_its_place_and_on_again() {
        let off = edited(DEFAULT, Change::CheckForUpdates(false));
        let expected = DEFAULT.replace("check_for_updates = true", "check_for_updates = false");
        assert_ne!(expected, DEFAULT);
        assert_eq!(off, expected);
        assert_eq!(edited(&off, Change::CheckForUpdates(false)), off);
        assert_eq!(edited(&off, Change::CheckForUpdates(true)), DEFAULT);
    }

    #[test]
    fn checking_for_updates_left_out_is_on_and_turned_off_among_the_settings_at_the_top() {
        let text = "hotkey = 'F4'

[folders]
exclude = []
";
        assert_eq!(edited(text, Change::CheckForUpdates(true)), text);
        assert_eq!(
            edited(text, Change::CheckForUpdates(false)),
            "hotkey = 'F4'
check_for_updates = false

[folders]
exclude = []
"
        );
    }

    #[test]
    fn an_entry_that_would_be_wrong_is_refused_with_why() {
        let refused = edit(DEFAULT, &add(List::RankLower, r"old\stuff"));
        let Err(Refusal::Wrong(why)) = refused else {
            panic!("{refused:?}");
        };
        assert!(why.to_string().contains(r"`old\stuff`"), "{why}");
        let refused = edit(DEFAULT, &add(List::Exclude, r"C:\"));
        let Err(Refusal::Wrong(why)) = refused else {
            panic!("{refused:?}");
        };
        assert!(why.to_string().contains("full path"), "{why}");
        assert!(matches!(
            edit(DEFAULT, &add(List::RankHigher, "")),
            Err(Refusal::Wrong(_))
        ));
    }
}
