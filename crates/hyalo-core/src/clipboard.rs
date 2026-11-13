//! Clipboard history: what was copied while hyalo ran, newest first, kept
//! in memory only, to put back on the clipboard. The launcher reads the
//! clipboard; what it keeps, and how each entry shows as a row, is
//! decided here.

use std::sync::Arc;

pub mod dropped;
pub mod html;
pub mod picture;
mod wanted;

pub use wanted::{Marks, Offered, Wanted, wanted};

use crate::frecency::Timestamp;
use crate::item::{self, Action, Item, ItemId, Kind};
use crate::rank;
use crate::text::{Language, Text};

/// What was copied, as hyalo keeps it to put back.
#[derive(Clone, PartialEq, Eq)]
pub enum Content {
    /// Text, with the HTML (`HTML Format`, UTF-8 with its header) and the
    /// RTF the app put beside it, if any, so a paste keeps the look.
    Text {
        text: String,
        html: Option<Vec<u8>>,
        rtf: Option<Vec<u8>>,
    },
    /// An image, its size in pixels, as PNG.
    Image {
        width: u32,
        height: u32,
        png: Vec<u8>,
    },
    /// Files and folders copied in Explorer, by full path.
    Files(Vec<String>),
}

impl Content {
    /// How many bytes it holds.
    pub fn size(&self) -> usize {
        match self {
            Content::Text { text, html, rtf } => {
                let beside = [html, rtf].into_iter().flatten().map(Vec::len);
                text.len() + beside.sum::<usize>()
            }
            Content::Image { png, .. } => png.len(),
            Content::Files(paths) => paths.iter().map(String::len).sum(),
        }
    }

    /// Whether it is what `other` is, as the user sees it: the same text
    /// whatever came beside it, the same picture, the same files.
    pub fn same(&self, other: &Content) -> bool {
        match (self, other) {
            (Content::Text { text, .. }, Content::Text { text: other, .. }) => text == other,
            _ => self == other,
        }
    }

    /// Whether there is anything in it to show: not a blank text, not an
    /// empty list of files.
    fn shows(&self) -> bool {
        match self {
            Content::Text { text, .. } => !text.trim().is_empty(),
            Content::Image { .. } => true,
            Content::Files(paths) => !paths.is_empty(),
        }
    }
}

/// Says what kind it is and how big, never what it holds: a log is no
/// place for what the user copied.
impl std::fmt::Debug for Content {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        let beside = |name, there: &Option<Vec<u8>>| if there.is_some() { name } else { "" };
        match self {
            Content::Text { text, html, rtf } => {
                let (html, rtf) = (beside(" with HTML", html), beside(" with RTF", rtf));
                write!(formatter, "Text({} bytes{html}{rtf})", text.len())
            }
            Content::Image { width, height, png } => {
                write!(formatter, "Image({width}x{height}, {} bytes)", png.len())
            }
            Content::Files(paths) => write!(formatter, "Files({})", paths.len()),
        }
    }
}

/// Something copied, as the launcher read it: what, from which app (the
/// one in front then; empty when not known), and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub content: Content,
    pub app: String,
    pub at: Timestamp,
}

/// One entry of the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its own for as long as hyalo runs, kept when it is copied again.
    pub id: u64,
    pub content: Arc<Content>,
    pub app: String,
    /// When it was last copied.
    pub at: Timestamp,
    /// How many lines a text has, counted once; `None` for the rest.
    lines: Option<usize>,
}

/// What adding to the history did: the entry's id, and the entries that
/// made room for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub id: u64,
    pub gone: Vec<u64>,
}

/// The most entries the history keeps.
pub const MOST: usize = 25;

/// The most bytes the history keeps in all.
pub const ROOM: usize = 32 << 20;

/// The most bytes one entry may hold; the launcher reads no more.
pub const MOST_BYTES: usize = 4 << 20;

/// What was copied, newest first: at most `MOST` entries and `ROOM`
/// bytes, the oldest going first.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<Entry>,
    /// The id the last new entry got.
    last: u64,
}

impl History {
    /// The entries, newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Puts `copied` first, within `MOST_BYTES` (see `fitted`), unless
    /// there is nothing in it to show. What is the same as an entry
    /// already there takes that entry's place and keeps its id.
    pub fn add(&mut self, copied: Copied) -> Option<Added> {
        let content = fitted(copied.content).filter(Content::shows)?;
        let copied = Copied { content, ..copied };
        let same = |entry: &Entry| entry.content.same(&copied.content);
        let id = match self.entries.iter().position(same) {
            Some(at) => self.entries.remove(at).id,
            None => {
                self.last += 1;
                self.last
            }
        };
        let lines = match &copied.content {
            Content::Text { text, .. } => Some(text.lines().count()),
            _ => None,
        };
        let entry = Entry {
            id,
            content: Arc::new(copied.content),
            app: copied.app,
            at: copied.at,
            lines,
        };
        self.entries.insert(0, entry);
        let gone = self.make_room();
        Some(Added { id, gone })
    }

    /// Lets the oldest entries go until what is left fits; their ids.
    fn make_room(&mut self) -> Vec<u64> {
        let mut gone = Vec::new();
        while self.entries.len() > MOST || self.size() > ROOM {
            gone.extend(self.entries.pop().map(|entry| entry.id));
        }
        gone
    }

    /// The bytes all entries hold.
    fn size(&self) -> usize {
        self.entries.iter().map(|entry| entry.content.size()).sum()
    }

    /// Takes out the entry `id`; whether there was one.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() < before
    }

    /// Forgets every entry; their ids.
    pub fn clear(&mut self) -> Vec<u64> {
        self.entries.drain(..).map(|entry| entry.id).collect()
    }
}

/// `content` within `MOST_BYTES`: a text too big leaves its RTF behind,
/// else its HTML, else both, keeping all it can; anything else too big,
/// or a text too big alone, is not kept.
fn fitted(content: Content) -> Option<Content> {
    if content.size() <= MOST_BYTES {
        return Some(content);
    }
    let Content::Text { text, html, rtf } = content else {
        return None;
    };
    let fits =
        |beside: &Option<Vec<u8>>| text.len() + beside.as_ref().map_or(0, Vec::len) <= MOST_BYTES;
    let (html, rtf) = match () {
        () if fits(&html) => (html, None),
        () if fits(&rtf) => (None, rtf),
        () if fits(&None) => (None, None),
        () => return None,
    };
    Some(Content::Text { text, html, rtf })
}

/// What a query starts with to look through the whole history: no file
/// name holds one, so it takes nothing from the search.
pub const PREFIX: char = '"';

/// What follows `"` in `query`, trimmed: the filter, empty for every
/// entry; `None` when the query does not look through the history.
pub fn filter(query: &str) -> Option<&str> {
    query.trim_start().strip_prefix(PREFIX).map(str::trim)
}

/// The history's `rows` `filter` finds, best first, each with the letters
/// of its title it matched: by the title, or, a little weaker and with
/// nothing to highlight, by its text or its app. Equal ones keep their
/// order, newest first; an empty filter keeps every row.
pub fn ranked(filter: &str, rows: &[Item]) -> Vec<(Item, Vec<usize>)> {
    rank::listed(filter, rows, |row| {
        row.keywords.iter().map(String::as_str).collect()
    })
}

/// What a row's item id starts with, before its entry's id.
const ROW: &str = "hyalo:clip:";

/// The most letters of a text's first line a title keeps: more than a
/// row ever shows.
pub const TITLE: usize = 200;

/// The most letters of a text the query looks through.
pub const HEAD: usize = 4096;

/// The item id of the row of the entry `entry`.
pub fn row_id(entry: u64) -> ItemId {
    ItemId(format!("{ROW}{entry}"))
}

/// The entry a row of the history stands for; `None` for any other row.
pub fn entry_of(id: &ItemId) -> Option<u64> {
    id.0.strip_prefix(ROW)?.parse().ok()
}

/// The forms of a count of lines.
const LINES: [Text; 3] = [Text::LinesOne, Text::LinesFew, Text::LinesMany];

/// What a row shows of an entry's content: its title, where it lives
/// (files' folder; empty for the rest, whose app says where they came
/// from), and the words it is found by, if any.
struct Shown {
    title: String,
    place: String,
    words: Option<String>,
}

impl Entry {
    /// The entry's row, in `language`, as of `now`: what it is as the
    /// title; where it came from, how long ago, and how many lines a text
    /// has, as the second line; Enter puts it back on the clipboard.
    pub fn item(&self, now: Timestamp, language: Language) -> Item {
        let shown = match &*self.content {
            Content::Text { text, .. } => text_shown(text),
            Content::Image { width, height, .. } => Shown {
                title: language.fill(Text::ClipImage, &[&width.to_string(), &height.to_string()]),
                place: String::new(),
                words: None,
            },
            Content::Files(paths) => files_shown(paths, language),
        };
        let many = self.lines.filter(|&lines| lines > 1);
        let lines = many.map(|lines| language.count(lines as u64, LINES));
        let app = Some(self.app.clone()).filter(|app| !app.is_empty());
        let from = Some(shown.place)
            .filter(|place| !place.is_empty())
            .or(app.clone());
        let parts = [from, Some(ago(self.at, now, language)), lines];
        Item {
            id: row_id(self.id),
            title: shown.title,
            subtitle: parts.into_iter().flatten().collect::<Vec<_>>().join(" · "),
            keywords: shown.words.into_iter().chain(app).collect(),
            kind: Kind::Clip,
            action: Action::PutBack(Arc::clone(&self.content)),
        }
    }
}

/// A text as a row: its first line that is not blank as the title, its
/// start as the words to find it by, each with its blanks squeezed.
fn text_shown(text: &str) -> Shown {
    let head: String = text.chars().take(HEAD * 2).collect();
    let first = head.lines().find(|line| !line.trim().is_empty());
    Shown {
        title: squeezed(first.unwrap_or_default())
            .chars()
            .take(TITLE)
            .collect(),
        place: String::new(),
        words: Some(squeezed(&head).chars().take(HEAD).collect()),
    }
}

/// Files as a row: the first one's name and how many more as the title,
/// the folder it is in as its place, their names as the words to find
/// them by.
fn files_shown(paths: &[String], language: Language) -> Shown {
    let files: Vec<(&str, String)> = paths.iter().map(|path| item::split(path)).collect();
    let (name, folder) = &files[0];
    let title = match files.len() {
        1 => name.to_string(),
        n => language.fill(Text::ClipFiles, &[name, &(n - 1).to_string()]),
    };
    let names: Vec<&str> = files.iter().map(|&(name, _)| name).collect();
    Shown {
        title,
        place: folder.clone(),
        words: Some(names.join(" ")),
    }
}

/// `text` with each run of blanks, line breaks among them, one space,
/// and none at either end.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long before `now` `then` was, in `language`: just now, then in
/// minutes, hours, days.
pub fn ago(then: Timestamp, now: Timestamp, language: Language) -> String {
    let seconds = now.0.saturating_sub(then.0);
    let (text, unit) = match seconds {
        0..60 => return language.say(Text::JustNow).to_string(),
        60..3600 => (Text::MinutesAgo, 60),
        3600..86_400 => (Text::HoursAgo, 3600),
        _ => (Text::DaysAgo, 86_400),
    };
    language.fill(text, &[&(seconds / unit).to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Action, ItemId, Kind};
    use crate::text::Language;

    fn text(text: &str) -> Content {
        Content::Text {
            text: text.to_string(),
            html: None,
            rtf: None,
        }
    }

    fn copied(content: Content, at: u64) -> Copied {
        Copied {
            content,
            app: "Notepad".to_string(),
            at: Timestamp(at),
        }
    }

    fn add(history: &mut History, copied: Copied) -> Added {
        history.add(copied).expect("kept")
    }

    fn texts(history: &History) -> Vec<String> {
        let entries = history.entries().iter();
        let text = |entry: &Entry| match &*entry.content {
            Content::Text { text, .. } => text.clone(),
            other => format!("{other:?}"),
        };
        entries.map(text).collect()
    }

    #[test]
    fn what_is_copied_comes_first_each_with_an_id_of_its_own() {
        let mut history = History::default();
        let one = add(&mut history, copied(text("one"), 10));
        let two = add(&mut history, copied(text("two"), 20));
        assert_eq!(texts(&history), ["two", "one"]);
        assert_ne!(one.id, two.id);
        assert_eq!(history.entries()[0].id, two.id);
        assert_eq!(history.entries()[0].at, Timestamp(20));
        assert_eq!(history.entries()[0].app, "Notepad");
        assert!(one.gone.is_empty() && two.gone.is_empty());
    }

    #[test]
    fn the_same_text_copied_again_moves_up_keeping_its_id_and_takes_the_newer_look() {
        let mut history = History::default();
        let first = add(&mut history, copied(text("one"), 10));
        add(&mut history, copied(text("two"), 20));
        let styled = Content::Text {
            text: "one".to_string(),
            html: Some(b"<b>one</b>".to_vec()),
            rtf: None,
        };
        let again = add(
            &mut history,
            Copied {
                app: "Word".to_string(),
                ..copied(styled.clone(), 30)
            },
        );
        assert_eq!(
            again,
            Added {
                id: first.id,
                gone: Vec::new()
            }
        );
        assert_eq!(texts(&history), ["one", "two"]);
        let entry = &history.entries()[0];
        assert_eq!((&*entry.content, entry.app.as_str()), (&styled, "Word"));
        assert_eq!(entry.at, Timestamp(30));
    }

    #[test]
    fn pictures_and_files_are_the_same_only_when_all_of_them_is() {
        let picture = |png: &[u8]| Content::Image {
            width: 2,
            height: 1,
            png: png.to_vec(),
        };
        assert!(picture(b"ab").same(&picture(b"ab")));
        assert!(!picture(b"ab").same(&picture(b"ac")));
        let files =
            |paths: &[&str]| Content::Files(paths.iter().map(|path| path.to_string()).collect());
        assert!(files(&[r"C:\a"]).same(&files(&[r"C:\a"])));
        assert!(!files(&[r"C:\a"]).same(&files(&[r"C:\a", r"C:\b"])));
        assert!(!text("C:\\a").same(&files(&[r"C:\a"])));
    }

    #[test]
    fn it_keeps_the_newest_25_and_says_which_went() {
        let mut history = History::default();
        let ids: Vec<u64> = (0..MOST as u64)
            .map(|at| add(&mut history, copied(text(&at.to_string()), at)).id)
            .collect();
        let added = add(&mut history, copied(text("last"), 99));
        assert_eq!(added.gone, [ids[0]]);
        assert_eq!(history.entries().len(), MOST);
        assert_eq!(history.entries()[MOST - 1].id, ids[1]);
    }

    #[test]
    fn it_keeps_no_more_than_32_megabytes_letting_the_oldest_go_first() {
        let mut history = History::default();
        let big = |letter: &str| text(&letter.repeat(MOST_BYTES));
        let ids: Vec<u64> = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .into_iter()
            .map(|letter| add(&mut history, copied(big(letter), 1)).id)
            .collect();
        assert!(history.entries().len() == 8, "8 × 4 MB fit in 32 MB");
        let added = add(&mut history, copied(text("one more byte"), 2));
        assert_eq!(added.gone, [ids[0]]);
        let added = add(&mut history, copied(big("i"), 3));
        assert_eq!(added.gone, [ids[1]], "{:?}", texts(&history).len());
    }

    #[test]
    fn an_entry_holds_its_text_and_what_came_beside_it_or_its_png_or_its_paths() {
        let styled = Content::Text {
            text: "abc".to_string(),
            html: Some(vec![0; 10]),
            rtf: Some(vec![0; 100]),
        };
        assert_eq!(styled.size(), 113);
        assert_eq!(text("абв").size(), 6);
        let picture = Content::Image {
            width: 100,
            height: 100,
            png: vec![0; 7],
        };
        assert_eq!(picture.size(), 7);
        let files = Content::Files(vec![r"C:\a".to_string(), r"C:\bb".to_string()]);
        assert_eq!(files.size(), 9);
    }

    #[test]
    fn what_was_copied_is_never_written_to_a_log() {
        let styled = Content::Text {
            text: "hunter2".to_string(),
            html: Some(b"<b>hunter2</b>".to_vec()),
            rtf: None,
        };
        assert_eq!(format!("{styled:?}"), "Text(7 bytes with HTML)");
        let both = Content::Text {
            text: String::new(),
            html: Some(Vec::new()),
            rtf: Some(Vec::new()),
        };
        assert_eq!(format!("{both:?}"), "Text(0 bytes with HTML with RTF)");
        assert_eq!(format!("{:?}", text("a")), "Text(1 bytes)");
        let picture = Content::Image {
            width: 3,
            height: 2,
            png: vec![0; 9],
        };
        assert_eq!(format!("{picture:?}"), "Image(3x2, 9 bytes)");
        let files = Content::Files(vec![r"C:\secret.txt".to_string()]);
        assert_eq!(format!("{files:?}"), "Files(1)");
    }

    #[test]
    fn an_entry_taken_out_or_all_forgotten_are_gone() {
        let mut history = History::default();
        let one = add(&mut history, copied(text("one"), 1)).id;
        let two = add(&mut history, copied(text("two"), 2)).id;
        assert!(history.remove(one));
        assert!(!history.remove(one), "already gone");
        assert_eq!(texts(&history), ["two"]);
        let three = add(&mut history, copied(text("three"), 3)).id;
        assert_eq!(history.clear(), [three, two]);
        assert!(history.entries().is_empty());
        let again = add(&mut history, copied(text("one"), 4)).id;
        assert!(again > three, "ids are never given twice");
    }

    fn listed(content: Content, app: &str, at: u64) -> Entry {
        let mut history = History::default();
        let copied = Copied {
            content,
            app: app.to_string(),
            at: Timestamp(at),
        };
        history.add(copied).expect("kept");
        history.entries()[0].clone()
    }

    const NOW: Timestamp = Timestamp(1_000_000);

    #[test]
    fn a_text_is_titled_by_its_first_line_and_says_where_from_when_and_how_long() {
        let content = text("\r\n  \t\r\n  Dear  \tsir,\r\nthe report\r\n\r\nis late\r\n");
        let entry = listed(content.clone(), "Outlook", NOW.0 - 300);
        let item = entry.item(NOW, Language::English);
        assert_eq!(item.title, "Dear sir,");
        assert_eq!(item.subtitle, "Outlook · 5 min ago · 6 lines");
        assert_eq!(item.kind, Kind::Clip);
        assert_eq!(item.action, Action::PutBack(Arc::new(content)));
        assert_eq!(entry_of(&item.id), Some(entry.id));
        let russian = entry.item(NOW, Language::Russian);
        assert_eq!(russian.subtitle, "Outlook · 5 мин назад · 6 строк");
    }

    #[test]
    fn a_line_alone_says_no_count_and_an_app_not_known_is_left_out() {
        let entry = listed(text("hello"), "", NOW.0);
        let item = entry.item(NOW, Language::English);
        let shown = (item.title.as_str(), item.subtitle.as_str());
        assert_eq!(shown, ("hello", "just now"));
        let entry = listed(text("hello\n"), "cmd", NOW.0);
        let item = entry.item(NOW, Language::English);
        assert_eq!(item.subtitle, "cmd · just now");
    }

    #[test]
    fn a_long_first_line_is_cut_where_no_row_would_show_it_anyway() {
        let entry = listed(text(&"й".repeat(1000)), "", NOW.0);
        let title = entry.item(NOW, Language::English).title;
        assert_eq!(title, "й".repeat(TITLE));
    }

    #[test]
    fn an_entry_over_4_megabytes_leaves_its_rtf_then_its_html_behind_or_is_not_kept() {
        let mut history = History::default();
        let styled = |text_size: usize, html: usize, rtf: usize| Content::Text {
            text: "a".repeat(text_size),
            html: Some(vec![b'h'; html]),
            rtf: Some(vec![b'r'; rtf]),
        };
        let sizes = |history: &History| match &*history.entries()[0].content {
            Content::Text { text, html, rtf } => (
                text.len(),
                html.as_ref().map(Vec::len),
                rtf.as_ref().map(Vec::len),
            ),
            _ => panic!("a text"),
        };
        let half = MOST_BYTES / 2;
        add(&mut history, copied(styled(half, half / 2, half / 2), 1));
        assert_eq!(
            sizes(&history),
            (half, Some(half / 2), Some(half / 2)),
            "all of it fits"
        );
        add(&mut history, copied(styled(half - 1, half / 2, half), 2));
        assert_eq!(sizes(&history), (half - 1, Some(half / 2), None));
        add(&mut history, copied(styled(half + 1, half, 1), 3));
        assert_eq!(
            sizes(&history),
            (half + 1, None, Some(1)),
            "the HTML alone would not fit"
        );
        add(&mut history, copied(styled(half + 2, half - 2, 1), 4));
        assert_eq!(
            sizes(&history),
            (half + 2, Some(half - 2), None),
            "just fits"
        );
        add(&mut history, copied(styled(MOST_BYTES, 1, 1), 5));
        assert_eq!(
            sizes(&history),
            (MOST_BYTES, None, None),
            "the text alone just fits"
        );
        let too_long = text(&"a".repeat(MOST_BYTES + 1));
        assert!(history.add(copied(too_long, 4)).is_none());
        let picture = Content::Image {
            width: 1,
            height: 1,
            png: vec![0; MOST_BYTES + 1],
        };
        assert!(history.add(copied(picture, 5)).is_none());
        let long = Content::Files(vec!["a".repeat(MOST_BYTES + 1)]);
        assert!(history.add(copied(long, 6)).is_none());
        assert_eq!(history.entries().len(), 5);
    }

    #[test]
    fn a_blank_text_or_no_files_are_not_kept() {
        let mut history = History::default();
        for blank in ["", " ", "\r\n\t "] {
            assert_eq!(history.add(copied(text(blank), 1)), None, "{blank:?}");
        }
        let none = Content::Files(Vec::new());
        assert_eq!(history.add(copied(none, 1)), None);
        assert!(history.entries().is_empty());
    }

    #[test]
    fn how_long_ago_goes_in_minutes_then_hours_then_days() {
        let said = |seconds: u64, language| ago(Timestamp(NOW.0 - seconds), NOW, language);
        let english = [
            (0, "just now"),
            (59, "just now"),
            (60, "1 min ago"),
            (3599, "59 min ago"),
            (3600, "1 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 d ago"),
            (3 * 86_400 + 5, "3 d ago"),
        ];
        for (seconds, english) in english {
            assert_eq!(said(seconds, Language::English), english);
        }
        assert_eq!(said(7200, Language::Russian), "2 ч назад");
        assert_eq!(said(86_400 * 2, Language::Russian), "2 дн. назад");
        let later = ago(Timestamp(NOW.0 + 10), NOW, Language::English);
        assert_eq!(later, "just now", "a clock set back");
    }

    #[test]
    fn an_image_is_titled_by_its_size() {
        let picture = Content::Image {
            width: 1920,
            height: 1080,
            png: vec![1],
        };
        let entry = listed(picture, "Snipping Tool", NOW.0 - 7200);
        let item = entry.item(NOW, Language::English);
        assert_eq!(item.title, "Image 1920 × 1080");
        assert_eq!(item.subtitle, "Snipping Tool · 2 h ago");
        let russian = entry.item(NOW, Language::Russian);
        assert_eq!(russian.title, "Изображение 1920 × 1080");
    }

    #[test]
    fn files_are_titled_by_the_first_ones_name_and_say_their_folder() {
        let paths = [
            r"C:\Users\me\Documents\report.pdf",
            r"C:\Users\me\Documents\notes",
            r"C:\Users\me\Documents\a.txt",
            r"C:\Users\me\Documents\b.txt",
        ];
        let files = Content::Files(paths.iter().map(|path| path.to_string()).collect());
        let entry = listed(files, "Explorer", NOW.0 - 86_400);
        let item = entry.item(NOW, Language::English);
        assert_eq!(item.title, "report.pdf and 3 more");
        assert_eq!(item.subtitle, r"C:\Users\me\Documents · 1 d ago");
        let russian = entry.item(NOW, Language::Russian);
        assert_eq!(russian.title, "report.pdf и ещё 3");
        let one = Content::Files(vec![r"D:\film.mkv".to_string()]);
        let item = listed(one, "Explorer", NOW.0).item(NOW, Language::English);
        let shown = (item.title.as_str(), item.subtitle.as_str());
        assert_eq!(shown, ("film.mkv", r"D:\ · just now"));
    }

    #[test]
    fn a_row_is_found_by_its_text_or_names_and_its_app_never_shown() {
        let long = format!("first\n  second   line\n{}", "x".repeat(10_000));
        let entry = listed(text(&long), "Code", NOW.0);
        let keywords = entry.item(NOW, Language::English).keywords;
        assert_eq!(keywords.len(), 2);
        assert!(
            keywords[0].starts_with("first second line xxx"),
            "{keywords:?}"
        );
        assert_eq!(keywords[0].chars().count(), HEAD);
        assert_eq!(keywords[1], "Code");
        let paths = vec![r"C:\a\one.txt".to_string(), r"C:\a\two.txt".to_string()];
        let entry = listed(Content::Files(paths), "Explorer", NOW.0);
        let keywords = entry.item(NOW, Language::English).keywords;
        assert_eq!(keywords, ["one.txt two.txt", "Explorer"]);
        let picture = Content::Image {
            width: 1,
            height: 1,
            png: vec![1],
        };
        let entry = listed(picture, "", NOW.0);
        assert!(entry.item(NOW, Language::English).keywords.is_empty());
    }

    #[test]
    fn a_query_starting_with_a_double_quote_looks_through_the_history() {
        assert_eq!(filter("\""), Some(""));
        assert_eq!(filter("\"report"), Some("report"));
        assert_eq!(filter("  \"  the report  "), Some("the report"));
        assert_eq!(filter("report"), None);
        assert_eq!(filter("a\"b"), None);
        assert_eq!(filter(""), None);
    }

    #[test]
    fn the_history_is_found_by_title_then_weaker_by_text_or_app() {
        let mut history = History::default();
        let copies = [
            ("dear sir", "Outlook"),
            ("pad\nthe report", ""),
            ("report: late", ""),
        ];
        for (at, (text_copied, app)) in copies.into_iter().enumerate() {
            let copy = Copied {
                content: text(text_copied),
                app: app.to_string(),
                at: Timestamp(at as u64),
            };
            add(&mut history, copy);
        }
        let rows: Vec<Item> = history
            .entries()
            .iter()
            .map(|entry| entry.item(NOW, Language::English))
            .collect();
        let titles = |found: &[(Item, Vec<usize>)]| -> Vec<String> {
            found.iter().map(|(item, _)| item.title.clone()).collect()
        };
        assert_eq!(
            titles(&ranked("", &rows)),
            ["report: late", "pad", "dear sir"]
        );
        let found = ranked("report", &rows);
        assert_eq!(titles(&found), ["report: late", "pad"]);
        assert_eq!(found[0].1, [0, 1, 2, 3, 4, 5]);
        assert!(found[1].1.is_empty(), "found in its text, not its title");
        assert_eq!(titles(&ranked("outlook", &rows)), ["dear sir"]);
        assert!(ranked("zzz", &rows).is_empty());
    }

    #[test]
    fn only_a_clipboard_row_names_an_entry() {
        assert_eq!(entry_of(&ItemId("hyalo:clip:7".to_string())), Some(7));
        assert_eq!(entry_of(&row_id(12)), Some(12));
        assert_eq!(entry_of(&ItemId("hyalo:window:7".to_string())), None);
        assert_eq!(entry_of(&ItemId("hyalo:clip:x".to_string())), None);
        assert_eq!(entry_of(&ItemId(r"C:\7".to_string())), None);
    }
}
