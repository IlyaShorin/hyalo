//! The config: hyalo's settings as the user writes them in
//! `config.toml`. Text in, settings or the first problem out, with the
//! line it is on. Reading and watching the file is the launcher's.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use serde::Deserialize;
use toml::Spanned;

use crate::hotkey::{self, Hotkey};
use crate::places::{self, Place};
use crate::text::{Language, Text};
use crate::web::{self, Engine};

mod complete;
mod edit;

pub use complete::completed;
pub use edit::{Change, Refusal, edit};

/// What the file holds when hyalo writes it the first time: every
/// setting at its default, with a word on each.
pub const DEFAULT: &str = r#"# hyalo's settings. Saved changes apply at once; a mistake keeps the last
# good settings and shows what is wrong under the launcher's search bar.
# Write paths in single quotes, so that backslashes stay as they are.

# Shows and hides the launcher: Ctrl, Alt, Shift or Win and one key,
# joined by +, such as "Ctrl+Shift+K" or "Win+F1".
hotkey = "Alt+Space"

# What the settings window, the tray menu and the launcher's rows say:
# "en" for English or "ru" for Russian. Left out, the language Windows
# is shown in.
language = "en"

# Looks for a new version of hyalo on GitHub once a day and tells you of
# it under the search bar and in the tray menu; nothing is installed
# until you click Install. false stops looking.
check_for_updates = true

# Lists what you copy under the empty search bar, newest first: text,
# pictures and files, kept in memory alone, never on disk, and nothing
# password managers mark private. false stops it and forgets the list.
clipboard_history = true

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

# Web searches: a prefix, then the address with {query} where the words
# go. Type the prefix, a space and the words, such as g rust lifetimes.
# Add your own, such as so = "https://stackoverflow.com/search?q={query}",
# change an address, or take a line out to drop that search.
[search]
g = "https://www.google.com/search?q={query}"
gh = "https://github.com/search?q={query}"
w = "https://en.wikipedia.org/w/index.php?search={query}"
ya = "https://yandex.ru/search/?text={query}"
yt = "https://www.youtube.com/results?search_query={query}"
"#;

/// The file written the first time, in `language`: the defaults, with the
/// language Windows is shown in, and Wikipedia in it.
pub fn default_file(language: Language) -> String {
    let named = format!("language = \"{}\"", language.code());
    let wikipedia = format!("https://{}.wikipedia.org/", language.code());
    DEFAULT.replacen("language = \"en\"", &named, 1).replacen(
        "https://en.wikipedia.org/",
        &wikipedia,
        1,
    )
}

/// The settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Shows and hides the launcher.
    pub hotkey: Hotkey,
    /// What hyalo says things in; `None` follows Windows.
    pub language: Option<Language>,
    /// Whether to look for a newer release.
    pub check_for_updates: bool,
    /// Whether to keep what is copied, to list it under the empty bar.
    pub clipboard_history: bool,
    /// Full paths the walk leaves out, with everything under them: with
    /// backslashes, without one at the end.
    pub exclude: Vec<String>,
    /// Places whose files rank lower, besides the built-in ones.
    pub rank_lower: Vec<Place>,
    /// Places whose files rank higher, besides the built-in ones.
    pub rank_higher: Vec<Place>,
    /// The web searches `[search]` lists; `None` when the file has no
    /// `[search]`, which searches the defaults.
    pub search: Option<Vec<Engine>>,
}

impl Config {
    /// The web searches a query may ask: those the file lists, or, when it
    /// lists none, the defaults, Wikipedia in `language`.
    pub fn engines(&self, language: Language) -> Vec<Engine> {
        self.search
            .clone()
            .unwrap_or_else(|| web::defaults(language))
    }
}

impl Default for Config {
    fn default() -> Self {
        let place = |text: &&str| Place::parse(text).expect("a built-in place");
        let strings = |list: List| list.defaults().iter().map(|text| text.to_string());
        let places = |list: List| list.defaults().iter().map(place).collect();
        Config {
            hotkey: Hotkey::default(),
            language: None,
            check_for_updates: true,
            clipboard_history: true,
            exclude: strings(List::Exclude).collect(),
            rank_lower: places(List::RankLower),
            rank_higher: places(List::RankHigher),
            search: None,
        }
    }
}

/// A list of entries the settings window shows: the three of folders in
/// `[folders]`, and the web searches in `[search]`, each written there as
/// `prefix = "address"` and listed as an entry `prefix address`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Exclude,
    RankLower,
    RankHigher,
    Search,
}

impl List {
    /// Every list, in the order the file has them.
    pub const ALL: [List; 4] = [
        List::Exclude,
        List::RankLower,
        List::RankHigher,
        List::Search,
    ];

    /// The lists of folders, in the order `[folders]` has them.
    pub const FOLDERS: [List; 3] = [List::Exclude, List::RankLower, List::RankHigher];

    /// Its key in `[folders]`; the web searches have the table `[search]`.
    pub fn key(self) -> &'static str {
        match self {
            List::Exclude => "exclude",
            List::RankLower => "rank_lower",
            List::RankHigher => "rank_higher",
            List::Search => "search",
        }
    }

    /// What it holds when the file leaves it out.
    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            List::Exclude => &[r"C:\Windows\WinSxS"],
            List::RankLower => &["target"],
            List::RankHigher | List::Search => &[],
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The settings as the file writes them, for showing and changing them:
/// the hotkey, and each list's entries as they are written.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub hotkey: Hotkey,
    pub language: Option<Language>,
    pub check_for_updates: bool,
    pub clipboard_history: bool,
    lists: [Vec<String>; 4],
}

impl Written {
    /// The entries of `list`, as written.
    pub fn list(&self, list: List) -> &[String] {
        &self.lists[list.index()]
    }
}

/// The settings `text` writes, if it holds good settings.
pub fn written(text: &str) -> Result<Written, Problem> {
    let (config, lists) = read(text)?;
    Ok(Written {
        hotkey: config.hotkey,
        language: config.language,
        check_for_updates: config.check_for_updates,
        clipboard_history: config.clipboard_history,
        lists,
    })
}

/// What is wrong with a config, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Counted from 1.
    pub line: usize,
    pub why: Why,
}

impl Problem {
    /// Its line and what is wrong there, in `language`.
    pub fn said(&self, language: Language) -> String {
        let line = self.line.to_string();
        language.fill(Text::ProblemAt, &[&line, &self.why.said(language)])
    }
}

impl fmt::Display for Problem {
    /// Its line and what is wrong there, in English.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.said(Language::English))
    }
}

/// What is wrong with a config, or with a change to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The hotkey's name, or the keys pressed for one.
    Hotkey(hotkey::Wrong),
    /// A value that does not read as what was wanted.
    Value(Wanted, String),
    /// A backslash in double quotes, as in a Windows path.
    Escape,
    /// What TOML's own reader says, in English.
    Toml(String),
    /// The file is not UTF-8.
    NotUtf8,
    /// The file cannot be read at all.
    Unreadable,
    /// Windows refused this hotkey: another app holds it.
    Taken(Hotkey),
    /// The file could not be written, for the reason Windows gives.
    Unsaved(String),
}

/// What a value of the config should have been.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// A full path below a drive: an excluded folder.
    FullPath,
    /// A folder name or a full path: a folder that ranks.
    Place,
    Language,
    /// One word: a web search's prefix.
    Prefix,
    /// A web address with `{query}`: a web search's.
    Address,
    /// A prefix, a space and an address: a web search as the settings
    /// window adds one.
    Engine,
}

impl Why {
    /// What is wrong, in `language`; TOML's own words stay English.
    pub fn said(&self, language: Language) -> String {
        match self {
            Why::Hotkey(wrong) => wrong.said(language),
            Why::Value(wanted, value) => language.fill(wanted.text(), &[value]),
            Why::Escape => language.say(Text::Escape).to_string(),
            Why::Toml(message) => message.clone(),
            Why::NotUtf8 => language.say(Text::NotUtf8).to_string(),
            Why::Unreadable => language.say(Text::Unreadable).to_string(),
            Why::Taken(hotkey) => language.fill(Text::Taken, &[&hotkey.to_string()]),
            Why::Unsaved(error) => language.fill(Text::Unsaved, &[error]),
        }
    }
}

impl fmt::Display for Why {
    /// What is wrong, in English.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.said(Language::English))
    }
}

impl Wanted {
    /// The sentence that says a value is not what was wanted.
    fn text(self) -> Text {
        match self {
            Wanted::FullPath => Text::NotFullPath,
            Wanted::Place => Text::NotPlace,
            Wanted::Language => Text::NotLanguage,
            Wanted::Prefix => Text::NotPrefix,
            Wanted::Address => Text::NotAddress,
            Wanted::Engine => Text::NotEngine,
        }
    }
}

/// The settings `text` gives: a setting left out keeps its default; an
/// unknown one is a problem, so a typo does not go unseen.
pub fn parse(text: &str) -> Result<Config, Problem> {
    read(text).map(|(config, _)| config)
}

/// The settings `text` gives, and each list's entries as written.
fn read(text: &str) -> Result<(Config, [Vec<String>; 4]), Problem> {
    let file: File = toml::from_str(text).map_err(|error| unreadable(text, &error))?;
    let folders = file.folders.unwrap_or_default();
    let lists = [&folders.exclude, &folders.rank_lower, &folders.rank_higher];
    let [exclude, rank_lower, rank_higher] =
        List::FOLDERS.map(|list| as_written(lists[list.index()].as_deref(), list));
    let engines = file.search.iter().flatten();
    let engines = engines.map(|(prefix, url)| entry(prefix, url.get_ref()));
    let written = [exclude, rank_lower, rank_higher, engines.collect()];
    let defaults = Config::default();
    let at = Lines(text);
    let config = Config {
        hotkey: at.one(file.hotkey, defaults.hotkey, |text| {
            Hotkey::parse(text).map_err(Why::Hotkey)
        })?,
        language: file
            .language
            .map(|value| at.read(value, &language))
            .transpose()?,
        check_for_updates: file.check_for_updates.unwrap_or(defaults.check_for_updates),
        clipboard_history: file.clipboard_history.unwrap_or(defaults.clipboard_history),
        exclude: at.each(folders.exclude, defaults.exclude, excluded)?,
        rank_lower: at.each(folders.rank_lower, defaults.rank_lower, place)?,
        rank_higher: at.each(folders.rank_higher, defaults.rank_higher, place)?,
        search: file
            .search
            .map(|search| {
                search
                    .into_iter()
                    .map(|(prefix, url)| at.read(url, &|url| engine(&prefix, url)))
                    .collect::<Result<_, _>>()
            })
            .transpose()?,
    };
    Ok((config, written))
}

/// A web search as the settings window lists it: its prefix, a space,
/// its address.
pub fn entry(prefix: &str, url: &str) -> String {
    format!("{prefix} {url}")
}

/// The prefix and the address of a web search listed as `entry`; `None`
/// without both.
pub fn engine_of(entry: &str) -> Option<(&str, &str)> {
    let (prefix, url) = entry.trim().split_once(char::is_whitespace)?;
    Some((prefix, url.trim_start()))
}

/// A list's entries as the file writes them, or its defaults when the
/// file leaves it out.
fn as_written(values: Option<&[Spanned<String>]>, list: List) -> Vec<String> {
    match values {
        Some(values) => values.iter().map(|value| value.get_ref().clone()).collect(),
        None => list
            .defaults()
            .iter()
            .map(|text| text.to_string())
            .collect(),
    }
}

/// The file as TOML has it; every value keeps where it was written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    hotkey: Option<Spanned<String>>,
    language: Option<Spanned<String>>,
    check_for_updates: Option<bool>,
    clipboard_history: Option<bool>,
    folders: Option<Folders>,
    /// Each prefix and its address, the prefixes in order.
    search: Option<BTreeMap<String, Spanned<String>>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Folders {
    exclude: Option<Vec<Spanned<String>>>,
    rank_lower: Option<Vec<Spanned<String>>>,
    rank_higher: Option<Vec<Spanned<String>>>,
}

/// The text a config came from, to tell the line a value is on.
struct Lines<'a>(&'a str);

impl Lines<'_> {
    /// The line the byte at `offset` is on.
    fn line(&self, offset: usize) -> usize {
        let before = &self.0.as_bytes()[..offset.min(self.0.len())];
        before.iter().filter(|&&byte| byte == b'\n').count() + 1
    }

    fn problem(&self, span: Range<usize>, why: Why) -> Problem {
        Problem {
            line: self.line(span.start),
            why,
        }
    }

    /// `value` read as `read` reads it; a refusal is a problem on its line.
    fn read<T>(
        &self,
        value: Spanned<String>,
        read: &impl Fn(&str) -> Result<T, Why>,
    ) -> Result<T, Problem> {
        read(value.get_ref()).map_err(|why| self.problem(value.span(), why))
    }

    /// The setting `value` reads as, or `default` when it was left out.
    fn one<T>(
        &self,
        value: Option<Spanned<String>>,
        default: T,
        read: impl Fn(&str) -> Result<T, Why>,
    ) -> Result<T, Problem> {
        value.map_or(Ok(default), |value| self.read(value, &read))
    }

    /// Every entry of a list, or `default` when the list was left out.
    fn each<T>(
        &self,
        values: Option<Vec<Spanned<String>>>,
        default: Vec<T>,
        read: impl Fn(&str) -> Result<T, Why>,
    ) -> Result<Vec<T>, Problem> {
        let Some(values) = values else {
            return Ok(default);
        };
        values
            .into_iter()
            .map(|value| self.read(value, &read))
            .collect()
    }
}

/// A file TOML cannot read, or whose values are of the wrong kind, as a
/// problem on one line. A backslash in double quotes is the usual slip
/// with Windows paths, so it gets advice instead of TOML's escapes.
fn unreadable(text: &str, error: &toml::de::Error) -> Problem {
    let message = error.message().lines().next().unwrap_or_default();
    let why = match message.contains("escape") {
        true => Why::Escape,
        false => Why::Toml(message.to_string()),
    };
    let span = error.span().unwrap_or_default();
    Lines(text).problem(span, why)
}

/// An excluded path: a full path below a drive, with backslashes and no
/// backslash at the end. A drive alone, `C:\`, is `C:` once its
/// backslash is gone, which is no full path.
fn excluded(text: &str) -> Result<String, Why> {
    let path = text.trim().replace('/', "\\");
    let path = path.trim_end_matches('\\');
    match places::full_path(path) {
        true => Ok(path.to_string()),
        false => Err(Why::Value(Wanted::FullPath, text.to_string())),
    }
}

fn language(text: &str) -> Result<Language, Why> {
    Language::parse(text).ok_or_else(|| Why::Value(Wanted::Language, text.to_string()))
}

fn place(text: &str) -> Result<Place, Why> {
    Place::parse(text).ok_or_else(|| Why::Value(Wanted::Place, text.to_string()))
}

/// The web search `prefix` asks at `url`.
fn engine(prefix: &str, url: &str) -> Result<Engine, Why> {
    Engine::custom(prefix, url).map_err(|wrong| match wrong {
        web::Wrong::Prefix => Why::Value(Wanted::Prefix, prefix.to_string()),
        web::Wrong::Url => Why::Value(Wanted::Address, url.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(text: &str) -> Problem {
        parse(text).expect_err("a problem")
    }

    fn place(text: &str) -> Place {
        Place::parse(text).expect("a place")
    }

    #[test]
    fn the_file_written_at_first_reads_as_the_defaults() {
        let config = parse(DEFAULT).expect("the default file");
        let english = Some(Language::English);
        assert_eq!(
            config,
            Config {
                language: english,
                search: Some(web::defaults(Language::English)),
                ..Config::default()
            }
        );
        assert_eq!(config.hotkey, Hotkey::default());
        assert_eq!(config.exclude, [r"C:\Windows\WinSxS"]);
        assert_eq!(config.rank_lower, [place("target")]);
        assert_eq!(config.rank_higher, []);
    }

    #[test]
    fn checking_for_updates_is_on_unless_the_file_turns_it_off() {
        assert!(parse(DEFAULT).expect("the default file").check_for_updates);
        assert!(parse("").expect("a config").check_for_updates);
        let off = parse(
            "check_for_updates = false
",
        )
        .expect("a config");
        assert!(!off.check_for_updates);
        assert!(
            !written(
                "check_for_updates = false
"
            )
            .expect("written")
            .check_for_updates
        );
        assert!(written("").expect("written").check_for_updates);
        let wrong = problem(
            "
check_for_updates = 'no'
",
        );
        assert_eq!(wrong.line, 2);
    }

    #[test]
    fn the_clipboard_history_is_on_unless_the_file_turns_it_off() {
        assert!(parse(DEFAULT).expect("the default file").clipboard_history);
        assert!(parse("").expect("a config").clipboard_history);
        let off = "clipboard_history = false
";
        assert!(!parse(off).expect("a config").clipboard_history);
        assert!(!written(off).expect("written").clipboard_history);
        assert!(written("").expect("written").clipboard_history);
        assert_eq!(
            problem(
                "
clipboard_history = 1
"
            )
            .line,
            2
        );
    }

    #[test]
    fn a_setting_left_out_keeps_its_default() {
        assert_eq!(parse(""), Ok(Config::default()));
        let config = parse("hotkey = \"Ctrl+K\"\n").expect("a config");
        assert_eq!(config.hotkey.to_string(), "Ctrl+K");
        assert_eq!(config.exclude, Config::default().exclude);
        let config = parse("[folders]\nrank_higher = ['notes']\n").expect("a config");
        assert_eq!(config.rank_higher, [place("notes")]);
        assert_eq!(config.rank_lower, Config::default().rank_lower);
    }

    #[test]
    fn reads_every_setting() {
        let text = r#"
hotkey = "win+shift+f1"
[folders]
exclude = ['D:\Old', "C:\\Temp"]
rank_lower = ['vendor', 'D:\Archive']
rank_higher = ['notes']
"#;
        let config = parse(text).expect("a config");
        assert_eq!(config.hotkey.to_string(), "Win+Shift+F1");
        assert_eq!(config.exclude, [r"D:\Old", r"C:\Temp"]);
        assert_eq!(config.rank_lower, [place("vendor"), place(r"D:\Archive")]);
        assert_eq!(config.rank_higher, [place("notes")]);
    }

    #[test]
    fn a_list_written_empty_is_empty() {
        let config = parse("[folders]\nexclude = []\nrank_lower = []\n").expect("a config");
        assert_eq!(config.exclude, Vec::<String>::new());
        assert_eq!(config.rank_lower, []);
    }

    #[test]
    fn an_excluded_path_is_kept_with_backslashes_and_no_backslash_at_the_end() {
        let config = parse("[folders]\nexclude = [' c:/Windows/WinSxS/ ']\n").expect("a config");
        assert_eq!(config.exclude, [r"c:\Windows\WinSxS"]);
    }

    #[test]
    fn an_unknown_setting_is_named_with_its_line() {
        let found = problem("\nhotkye = \"Alt+Space\"\n");
        assert_eq!(found.line, 2);
        assert!(found.why.to_string().contains("`hotkye`"), "{}", found.why);
        let found = problem("[folders]\nexclude = []\nhide = []\n");
        assert_eq!(found.line, 3);
        assert!(found.why.to_string().contains("`hide`"), "{}", found.why);
    }

    #[test]
    fn broken_syntax_is_found_on_its_line() {
        let found = problem("hotkey = \"Alt+Space\"\n[folders]\nexclude = [\n");
        assert!(found.line >= 3, "{found}");
        let found = problem("hotkey = \"Alt+Space\n");
        assert_eq!(found.line, 1);
    }

    #[test]
    fn a_setting_of_the_wrong_kind_is_found_on_its_line() {
        let found = problem("\n\nhotkey = 5\n");
        assert_eq!(found.line, 3);
        let found = problem("[folders]\nexclude = 'C:\\Windows'\n");
        assert_eq!(found.line, 2);
    }

    #[test]
    fn a_hotkey_it_cannot_read_is_found_on_its_line() {
        let found = problem("# keys\nhotkey = \"Ctrl+Kay\"\n");
        assert_eq!(found.line, 2);
        assert!(found.why.to_string().contains("`Kay`"), "{}", found.why);
    }

    #[test]
    fn an_excluded_path_must_be_a_full_path_below_a_drive() {
        for path in ["'Windows'", r"'\Windows'", r"'C:\'", "''"] {
            let text = format!("[folders]\nexclude = [\n  'D:\\ok',\n  {path},\n]\n");
            let found = problem(&text);
            assert_eq!(found.line, 4, "{path}");
            assert!(found.why.to_string().contains("full path"), "{}", found.why);
        }
    }

    #[test]
    fn a_place_must_be_a_folder_name_or_a_full_path() {
        let found = problem("[folders]\nrank_lower = ['target', 'old\\stuff']\n");
        assert_eq!(found.line, 2);
        assert!(
            found.why.to_string().contains(r"`old\stuff`"),
            "{}",
            found.why
        );
        let found = problem("[folders]\nrank_higher = ['']\n");
        assert_eq!(found.line, 2);
    }

    #[test]
    fn a_backslash_in_double_quotes_gets_a_hint_about_single_quotes() {
        let found = problem("[folders]\nexclude = [\"C:\\Windows\"]\n");
        assert_eq!(found.line, 2);
        assert!(
            found.why.to_string().contains("single quotes"),
            "{}",
            found.why
        );
    }

    #[test]
    fn a_problem_reads_as_its_line_and_what_is_wrong() {
        let found = Problem {
            line: 3,
            why: Why::Hotkey(hotkey::Wrong::Unknown("Kay".to_string())),
        };
        assert_eq!(found.to_string(), "line 3: `Kay` is not a key hyalo knows");
        assert_eq!(
            found.said(Language::Russian),
            "строка 3: hyalo не знает клавишу `Kay`"
        );
    }

    #[test]
    fn the_file_written_at_first_names_the_language_windows_speaks() {
        assert_eq!(default_file(Language::English), DEFAULT);
        let russian = default_file(Language::Russian);
        assert!(russian.contains("\nlanguage = \"ru\"\n"), "{russian}");
        let config = parse(&russian).expect("the default file");
        let expected = Config {
            language: Some(Language::Russian),
            search: Some(web::defaults(Language::Russian)),
            ..Config::default()
        };
        assert_eq!(config, expected);
    }

    #[test]
    fn a_language_is_read_by_its_code_and_left_out_follows_windows() {
        let russian = parse("language = 'RU'\n").expect("a config");
        assert_eq!(russian.language, Some(Language::Russian));
        assert_eq!(parse("").expect("a config").language, None);
        let written = written("language = \"en\"\n").expect("good settings");
        assert_eq!(written.language, Some(Language::English));
        let found = problem("hotkey = 'F4'\nlanguage = 'de'\n");
        assert_eq!(found.line, 2);
        assert_eq!(found.why, Why::Value(Wanted::Language, "de".to_string()));
    }

    fn entries(written: &Written, list: List) -> Vec<&str> {
        written.list(list).iter().map(String::as_str).collect()
    }

    #[test]
    fn the_lists_go_by_their_keys_in_folders() {
        let keys = List::ALL.map(List::key);
        assert_eq!(keys, ["exclude", "rank_lower", "rank_higher", "search"]);
        assert_eq!(List::FOLDERS, List::ALL[..3]);
    }

    const SEARCH: &str = "[search]
so = 'https://stackoverflow.com/search?q={query}'
g = \"https://duckduckgo.com/?q={query}\"
";

    #[test]
    fn web_searches_are_read_from_the_search_table_by_prefix() {
        let config = parse(SEARCH).expect("a config");
        let engines: Vec<(&str, &str, &str)> = config
            .search
            .iter()
            .flatten()
            .map(|engine| (&*engine.prefix, &*engine.name, &*engine.url))
            .collect();
        assert_eq!(
            engines,
            [
                ("g", "duckduckgo.com", "https://duckduckgo.com/?q={query}"),
                (
                    "so",
                    "stackoverflow.com",
                    "https://stackoverflow.com/search?q={query}"
                ),
            ]
        );
        assert_eq!(parse("").expect("a config").search, None);
    }

    /// The five hyalo knew without being told are written in the file
    /// like any other, so they can be seen, changed and taken out.
    #[test]
    fn the_file_written_at_first_lists_five_web_searches_wikipedia_in_its_language() {
        let engines = |text: &str, language| {
            let config = parse(text).expect("a config");
            let engines = config.engines(language).into_iter();
            engines
                .map(|engine| (engine.prefix, engine.url))
                .collect::<Vec<_>>()
        };
        let english = engines(DEFAULT, Language::English);
        let prefixes: Vec<&str> = english.iter().map(|(prefix, _)| prefix.as_str()).collect();
        assert_eq!(prefixes, ["g", "gh", "w", "ya", "yt"]);
        let russian = engines(&default_file(Language::Russian), Language::Russian);
        let wikipedia = russian.iter().find(|(prefix, _)| prefix == "w");
        let url = wikipedia.map(|(_, url)| url.as_str());
        assert_eq!(
            url,
            Some("https://ru.wikipedia.org/w/index.php?search={query}")
        );
    }

    #[test]
    fn a_file_without_web_searches_searches_the_five_and_an_empty_table_none() {
        let left_out = parse("hotkey = 'F4'\n").expect("a config");
        assert_eq!(
            left_out.engines(Language::Russian),
            web::defaults(Language::Russian)
        );
        let emptied = parse("[search]\n").expect("a config");
        assert_eq!(emptied.engines(Language::Russian), []);
    }

    #[test]
    fn web_searches_are_listed_as_their_prefix_and_address() {
        let written = written(SEARCH).expect("a config");
        assert_eq!(
            entries(&written, List::Search),
            [
                "g https://duckduckgo.com/?q={query}",
                "so https://stackoverflow.com/search?q={query}"
            ]
        );
        assert!(entries(&written, List::Exclude).len() == 1, "the defaults");
    }

    #[test]
    fn an_entry_reads_back_as_its_prefix_and_address() {
        let listed = entry("so", "https://a.io/{query}");
        assert_eq!(engine_of(&listed), Some(("so", "https://a.io/{query}")));
        assert_eq!(
            engine_of("  so 	 https://a.io/{query} "),
            Some(("so", "https://a.io/{query}"))
        );
        assert_eq!(engine_of("so"), None);
        assert_eq!(engine_of(" so "), None);
        assert_eq!(engine_of(""), None);
    }

    #[test]
    fn a_web_search_that_is_wrong_is_found_on_its_line() {
        let found = problem(
            "[search]

so = 'https://stackoverflow.com/?q='
",
        );
        assert_eq!(found.line, 3);
        let url = "https://stackoverflow.com/?q=".to_string();
        assert_eq!(found.why, Why::Value(Wanted::Address, url));
        let found = problem(
            "[search]
'a b' = 'https://a.io/{query}'
",
        );
        assert_eq!(found.line, 2);
        assert_eq!(found.why, Why::Value(Wanted::Prefix, "a b".to_string()));
        assert_eq!(
            problem(
                "[search]
so = 5
"
            )
            .line,
            2
        );
        assert_eq!(
            problem(
                "search = 'x'
"
            )
            .line,
            1
        );
    }

    #[test]
    fn what_is_wrong_with_a_web_search_is_said_in_either_language() {
        let said = |wanted, value: &str| {
            let why = Why::Value(wanted, value.to_string());
            (why.said(Language::English), why.said(Language::Russian))
        };
        let (english, russian) = said(Wanted::Address, "https://a.io/");
        assert_eq!(
            english,
            "`https://a.io/` is not a web address with {query} where the words go"
        );
        assert_eq!(
            russian,
            "`https://a.io/` — не веб-адрес с {query} на месте слов"
        );
        let (english, _) = said(Wanted::Prefix, ">w");
        assert_eq!(
            english,
            "`>w` is not a prefix: one word, not starting with >"
        );
        let (english, russian) = said(Wanted::Engine, "so");
        assert!(
            english.starts_with("`so` is not a prefix and an address, such as `so https://"),
            "{english}"
        );
        assert!(
            russian.starts_with("`so` — не префикс с адресом"),
            "{russian}"
        );
    }

    #[test]
    fn the_file_written_at_first_is_written_as_the_defaults() {
        let written = written(DEFAULT).expect("the default file");
        assert_eq!(written.hotkey, Hotkey::default());
        assert_eq!(entries(&written, List::Exclude), [r"C:\Windows\WinSxS"]);
        assert_eq!(entries(&written, List::RankLower), ["target"]);
        assert!(entries(&written, List::RankHigher).is_empty());
        for list in List::FOLDERS {
            assert_eq!(entries(&written, list), list.defaults());
        }
        let searches = entries(&written, List::Search);
        assert_eq!(searches.len(), 5, "{searches:?}");
        assert_eq!(searches[0], "g https://www.google.com/search?q={query}");
    }

    #[test]
    fn a_list_left_out_is_written_as_its_defaults() {
        let written = written("hotkey = 'F4'\n").expect("a config");
        assert_eq!(written.hotkey.to_string(), "F4");
        for list in List::ALL {
            assert_eq!(entries(&written, list), list.defaults());
        }
    }

    #[test]
    fn entries_are_kept_as_the_file_writes_them() {
        let text = "[folders]\nexclude = [' c:/Windows/WinSxS/ ']\nrank_lower = ['Vendor', 'D:\\Archive']\nrank_higher = []\n";
        let written = written(text).expect("a config");
        assert_eq!(entries(&written, List::Exclude), [" c:/Windows/WinSxS/ "]);
        assert_eq!(
            entries(&written, List::RankLower),
            ["Vendor", r"D:\Archive"]
        );
        assert!(entries(&written, List::RankHigher).is_empty());
    }

    #[test]
    fn a_broken_file_has_nothing_written_but_its_problem() {
        let found = written("\n\nhotkey = 5\n").expect_err("a problem");
        assert_eq!(found.line, 3);
        assert_eq!(
            written("[folders]\nexclude = ['x']\n").map(|_| ()),
            Err(problem("[folders]\nexclude = ['x']\n"))
        );
    }

    #[test]
    fn what_is_wrong_is_said_in_either_language_but_tomls_own_words() {
        let saids = [
            (
                Why::Value(Wanted::FullPath, "C:".into()),
                "`C:` — не полный путь вроде `D:\\old`",
            ),
            (
                Why::Value(Wanted::Place, "a\\b".into()),
                "`a\\b` — не имя папки и не полный путь вроде `D:\\old`",
            ),
            (
                Why::Value(Wanted::Language, "de".into()),
                "hyalo не знает языка `de`: только \"en\" или \"ru\"",
            ),
            (Why::Toml("unknown field".into()), "unknown field"),
            (
                Why::NotUtf8,
                "это не текст в UTF-8: сохраните файл в кодировке UTF-8",
            ),
            (Why::Unreadable, "не удаётся прочитать config.toml"),
            (
                Why::Taken(Hotkey::default()),
                "Alt+Space уже занято другим приложением",
            ),
            (
                Why::Unsaved("Отказано в доступе.".into()),
                "не удалось сохранить config.toml: Отказано в доступе.",
            ),
        ];
        for (why, said) in saids {
            assert_eq!(why.said(Language::Russian), said, "{why:?}");
        }
        assert!(
            Why::Escape
                .said(Language::Russian)
                .contains("одинарных кавычках")
        );
        assert!(Why::Escape.to_string().contains("single quotes"));
    }

    #[test]
    fn a_problem_is_one_line() {
        let found = problem("hotkey = \"Alt+Space\n");
        assert!(!found.why.to_string().contains('\n'), "{:?}", found.why);
    }
}
