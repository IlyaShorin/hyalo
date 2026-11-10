//! The config: hyalo's settings as the user writes them in
//! `config.toml`. Text in, settings or the first problem out, with the
//! line it is on. Reading and watching the file is the launcher's.

use std::fmt;
use std::ops::Range;

use serde::Deserialize;
use toml::Spanned;

use crate::hotkey::{self, Hotkey};
use crate::places::{self, Place};
use crate::text::{Language, Text};

mod edit;

pub use edit::{Change, Refusal, edit};

/// What the file holds when hyalo writes it the first time: every
/// setting at its default, with a word on each.
pub const DEFAULT: &str = r#"# hyalo's settings. Saved changes apply at once; a mistake keeps the last
# good settings and shows what is wrong under the launcher's search bar.
# Write paths in single quotes, so that backslashes stay as they are.

# Shows and hides the launcher: Ctrl, Alt, Shift or Win and one key,
# joined by +, such as "Ctrl+Shift+K" or "Win+F1".
hotkey = "Alt+Space"

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

/// The settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Shows and hides the launcher.
    pub hotkey: Hotkey,
    /// Full paths the walk leaves out, with everything under them: with
    /// backslashes, without one at the end.
    pub exclude: Vec<String>,
    /// Places whose files rank lower, besides the built-in ones.
    pub rank_lower: Vec<Place>,
    /// Places whose files rank higher, besides the built-in ones.
    pub rank_higher: Vec<Place>,
}

impl Default for Config {
    fn default() -> Self {
        let place = |text: &&str| Place::parse(text).expect("a built-in place");
        let strings = |list: List| list.defaults().iter().map(|text| text.to_string());
        let places = |list: List| list.defaults().iter().map(place).collect();
        Config {
            hotkey: Hotkey::default(),
            exclude: strings(List::Exclude).collect(),
            rank_lower: places(List::RankLower),
            rank_higher: places(List::RankHigher),
        }
    }
}

/// A list of folders in `[folders]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    Exclude,
    RankLower,
    RankHigher,
}

impl List {
    /// Every list, in the order the file has them.
    pub const ALL: [List; 3] = [List::Exclude, List::RankLower, List::RankHigher];

    /// Its key in `[folders]`.
    pub fn key(self) -> &'static str {
        match self {
            List::Exclude => "exclude",
            List::RankLower => "rank_lower",
            List::RankHigher => "rank_higher",
        }
    }

    /// What it holds when the file leaves it out.
    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            List::Exclude => &[r"C:\Windows\WinSxS"],
            List::RankLower => &["target"],
            List::RankHigher => &[],
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
    lists: [Vec<String>; 3],
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
        }
    }
}

/// The settings `text` gives: a setting left out keeps its default; an
/// unknown one is a problem, so a typo does not go unseen.
pub fn parse(text: &str) -> Result<Config, Problem> {
    read(text).map(|(config, _)| config)
}

/// The settings `text` gives, and each list's entries as written.
fn read(text: &str) -> Result<(Config, [Vec<String>; 3]), Problem> {
    let file: File = toml::from_str(text).map_err(|error| unreadable(text, &error))?;
    let folders = file.folders.unwrap_or_default();
    let lists = [&folders.exclude, &folders.rank_lower, &folders.rank_higher];
    let written = List::ALL.map(|list| as_written(lists[list.index()].as_deref(), list));
    let defaults = Config::default();
    let at = Lines(text);
    let config = Config {
        hotkey: at.one(file.hotkey, defaults.hotkey, |text| {
            Hotkey::parse(text).map_err(Why::Hotkey)
        })?,
        exclude: at.each(folders.exclude, defaults.exclude, excluded)?,
        rank_lower: at.each(folders.rank_lower, defaults.rank_lower, place)?,
        rank_higher: at.each(folders.rank_higher, defaults.rank_higher, place)?,
    };
    Ok((config, written))
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
    folders: Option<Folders>,
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

fn place(text: &str) -> Result<Place, Why> {
    Place::parse(text).ok_or_else(|| Why::Value(Wanted::Place, text.to_string()))
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
        assert_eq!(config, Config::default());
        assert_eq!(config.hotkey, Hotkey::default());
        assert_eq!(config.exclude, [r"C:\Windows\WinSxS"]);
        assert_eq!(config.rank_lower, [place("target")]);
        assert_eq!(config.rank_higher, []);
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

    fn entries(written: &Written, list: List) -> Vec<&str> {
        written.list(list).iter().map(String::as_str).collect()
    }

    #[test]
    fn the_lists_go_by_their_keys_in_folders() {
        let keys = List::ALL.map(List::key);
        assert_eq!(keys, ["exclude", "rank_lower", "rank_higher"]);
    }

    #[test]
    fn the_file_written_at_first_is_written_as_the_defaults() {
        let written = written(DEFAULT).expect("the default file");
        assert_eq!(written.hotkey, Hotkey::default());
        assert_eq!(entries(&written, List::Exclude), [r"C:\Windows\WinSxS"]);
        assert_eq!(entries(&written, List::RankLower), ["target"]);
        assert!(entries(&written, List::RankHigher).is_empty());
        for list in List::ALL {
            assert_eq!(entries(&written, list), list.defaults());
        }
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
