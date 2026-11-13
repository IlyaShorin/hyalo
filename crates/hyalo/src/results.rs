//! The rows the window lists for a query, and which one Enter picks.

use std::borrow::Cow;
use std::collections::HashSet;

use hyalo_core::clipboard::{self, Content};
use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::item::{Action, Item, Kind};
use hyalo_core::matcher::Match;
use hyalo_core::places::Places;
use hyalo_core::rank::{self, Ranked, rank};
use hyalo_core::rates::Rates;
use hyalo_core::system_commands::Command;
use hyalo_core::text::{Language, Text};
use hyalo_core::web::{self, Engine};
use hyalo_core::{calculator, currency, switcher};

/// The most rows the window lists.
pub const MAX_ROWS: usize = 8;

/// The answer for `query`, listed first: a web search when the query
/// starts with a prefix of the user's `engines` or a built-in one, else
/// the calculator's when the query is arithmetic, else a currency
/// conversion at `rates`, once there are any.
pub fn answer(
    query: &str,
    rates: Option<&Rates>,
    engines: &[Engine],
    language: Language,
) -> Option<Item> {
    let searched = web::answer(query, engines, language);
    let calculated = || calculator::answer(query, language);
    searched
        .or_else(calculated)
        .or_else(|| currency::answer(query, rates?, language))
}

/// The second line of a row, in `language`: what the item says, or, for
/// an app with no path to show, that it is an app.
pub fn subtitle(item: &Item, language: Language) -> &str {
    match (item.kind, item.subtitle.is_empty()) {
        (Kind::App, true) => language.say(Text::App),
        _ => &item.subtitle,
    }
}

/// The symbol a row of `kind` shows where an icon goes, from Segoe Fluent
/// Icons: a calculator for answers, a globe for web searches, a warning
/// for a notice, a system command's own, an entry of the clipboard
/// history's; `None` for the rest, which have icons (apps, files,
/// settings pages, windows).
pub fn glyph(kind: Kind, action: &Action) -> Option<char> {
    match (kind, action) {
        (Kind::Calculator, _) => Some('\u{E8EF}'),
        (Kind::Web, _) => Some('\u{E774}'),
        (Kind::Notice, _) => Some('\u{E7BA}'),
        (_, Action::System(command)) => Some(command_glyph(*command)),
        (_, Action::PutBack(content)) => Some(copied_glyph(content)),
        _ => None,
    }
}

/// The symbol of an entry of the clipboard history: a clipboard for text;
/// a picture or a page until the thumbnail or the first file's icon
/// comes, which takes its place.
fn copied_glyph(content: &Content) -> char {
    match content {
        Content::Text { .. } => '\u{E77F}',
        Content::Image { .. } => '\u{E91B}',
        Content::Files(_) => '\u{E8A5}',
    }
}

/// A system command's symbol.
fn command_glyph(command: Command) -> char {
    match command {
        Command::Lock => '\u{E72E}',
        Command::Sleep => '\u{E708}',
        Command::Hibernate => '\u{E823}',
        Command::Restart => '\u{E777}',
        Command::ShutDown => '\u{E7E8}',
        Command::SignOut => '\u{F3B1}',
        Command::EmptyRecycleBin => '\u{E74D}',
    }
}

/// One row of the list: an item, and the letters of its title the query
/// matched, as ascending `char` indices, to highlight.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub item: Item,
    pub matched: Vec<usize>,
}

/// The rows for one query and the selected row.
#[derive(Debug, Default)]
pub struct Results {
    rows: Vec<Listed>,
    /// Within `rows` whenever there are any.
    selected: usize,
    /// What the selected row asks to press Enter for, if it asks: its own
    /// action that asks first (a system command such as Restart), after
    /// the first Enter, or uninstalling its app, after Shift+Delete.
    asking: Option<Action>,
}

impl Results {
    /// The best of the items ranked for `query` and of the `files` the
    /// index found for it: `MAX_ROWS` in all, the first one selected.
    /// Equal scores list items first; something found both ways is listed
    /// once. An empty query lists nothing: it would match everything
    /// equally.
    pub fn search<'a>(
        query: &str,
        items: impl IntoIterator<Item = &'a Item>,
        files: Vec<Ranked>,
        frecency: &Frecency,
        places: &Places,
        now: Timestamp,
    ) -> Self {
        if query.is_empty() {
            return Self::default();
        }
        let mut ranked = rank(query, items, frecency, places, now);
        ranked.extend(files);
        Self::merged(ranked)
    }

    /// The rows for a query of filters alone, `text` as typed: the `files`
    /// the index let through, by kind and place, the newest first, and
    /// the `picked` ones (that the filters let through too), lifted by how
    /// often they were picked for `text`. Something listed both ways is
    /// listed once.
    pub fn filtered<'a>(
        text: &str,
        picked: impl IntoIterator<Item = &'a Item>,
        files: Vec<Ranked>,
        frecency: &Frecency,
        places: &Places,
        now: Timestamp,
    ) -> Self {
        let lifted = picked.into_iter().map(|item| {
            let place = item.path().map_or(0.0, |path| places.of_path(path));
            let picks = frecency.score(&item.id, text, now);
            Ranked {
                score: rank::score(0.0, item.kind, place, picks),
                item: item.clone(),
                found: Match::none(),
            }
        });
        Self::merged(files.into_iter().chain(lifted).collect())
    }

    /// `ranked` best first, equal scores in their order, each item once,
    /// `MAX_ROWS` at most, the first selected.
    fn merged(mut ranked: Vec<Ranked>) -> Self {
        ranked.sort_by(|one, other| other.score.total_cmp(&one.score));
        let mut listed = HashSet::new();
        let ranked = ranked
            .into_iter()
            .filter(|ranked| listed.insert(ranked.item.id.clone()))
            .map(|ranked| Listed {
                item: ranked.item,
                matched: ranked.found.positions,
            });
        let rows = ranked.take(MAX_ROWS).collect();
        Self {
            rows,
            selected: 0,
            asking: None,
        }
    }

    /// The open `windows` the switcher's `filter` finds, best first, at
    /// most `MAX_ROWS`, the first one selected.
    pub fn windows(filter: &str, windows: &[Item]) -> Self {
        Self::ranked(switcher::ranked(filter, windows))
    }

    /// The `rows` of the clipboard history `filter` finds, best first, at
    /// most `MAX_ROWS`, the first one selected.
    pub fn copied(filter: &str, rows: &[Item]) -> Self {
        Self::ranked(clipboard::ranked(filter, rows))
    }

    /// The first `MAX_ROWS` of what was `ranked`, each with the letters
    /// its title matched, the first one selected.
    fn ranked(ranked: Vec<(Item, Vec<usize>)>) -> Self {
        let rows = ranked
            .into_iter()
            .map(|(item, matched)| Listed { item, matched });
        Self {
            rows: rows.take(MAX_ROWS).collect(),
            ..Self::default()
        }
    }

    /// These rows under `answer`, if there is one, still `MAX_ROWS` in
    /// all: the answer, such as the calculator's, comes first.
    pub fn answered(mut self, answer: Option<Item>) -> Self {
        let answer = answer.map(|item| Listed {
            item,
            matched: Vec::new(),
        });
        self.rows = answer.into_iter().chain(self.rows).take(MAX_ROWS).collect();
        self
    }

    /// `fresh` rows for the same query, such as after the provider looked
    /// again, with the item selected here still selected where it moved,
    /// so what Enter opens does not change under the user's fingers; if
    /// it is gone, the first row is.
    pub fn kept(&self, mut fresh: Self) -> Self {
        let kept = self.picked().and_then(|picked| {
            let mut rows = fresh.rows.iter();
            rows.position(|row| row.item.id == picked.item.id)
        });
        fresh.selected = kept.unwrap_or(fresh.selected);
        fresh.asking = self.asking.clone().filter(|_| kept.is_some());
        fresh
    }

    /// What Enter carries out now, `wanted` being what it does with the
    /// selected row: what the row asked about, if it asks; else `wanted`
    /// at once, unless that asks first (a system command such as
    /// Restart): then the row asks about it, and the next Enter runs it.
    pub fn confirm(&mut self, wanted: Action) -> Option<Action> {
        if let Some(asked) = self.asking.take() {
            return Some(asked);
        }
        match wanted.asks() {
            true => {
                self.ask(wanted);
                None
            }
            false => Some(wanted),
        }
    }

    /// The selected row asks to press Enter for `action`, if there is a
    /// row and `action` asks first, such as uninstalling the row's app;
    /// whether it asks.
    pub fn ask(&mut self, action: Action) -> bool {
        let asks = action.asks() && self.picked().is_some();
        self.asking = asks.then_some(action);
        asks
    }

    /// Whether the selected row asks to press Enter.
    pub fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// Stops asking, as anything but Enter does; whether it was.
    pub fn calm(&mut self) -> bool {
        self.asking.take().is_some()
    }

    /// The second line of row `row` in `language`: what the asking row
    /// asks, else what its item says.
    pub fn subtitle(&self, row: usize, language: Language) -> Cow<'_, str> {
        let asking = self.asking.as_ref().filter(|_| row == self.selected);
        match asking.and_then(|action| action.asking(language)) {
            Some(asked) => Cow::Owned(asked),
            None => Cow::Borrowed(subtitle(&self.rows[row].item, language)),
        }
    }

    pub fn rows(&self) -> &[Listed] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The selected row: what Enter opens.
    pub fn picked(&self) -> Option<&Listed> {
        self.rows.get(self.selected)
    }

    /// Selects row `row`, if there is one; whether the selection moved.
    pub fn select(&mut self, row: usize) -> bool {
        let moved = row < self.rows.len() && row != self.selected;
        if moved {
            self.selected = row;
            self.asking = None;
        }
        moved
    }

    /// One row down; from the last row back to the first. With no rows
    /// there is nowhere to go.
    pub fn down(&mut self) {
        let next = (self.selected + 1).checked_rem(self.rows.len());
        self.selected = next.unwrap_or(self.selected);
        self.asking = None;
    }

    /// One row up; from the first row round to the last.
    pub fn up(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.checked_sub(1).unwrap_or(last);
        self.asking = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::clipboard::{Content, Copied, History};
    use hyalo_core::item::{ItemId, Kind};
    use hyalo_core::system_commands;
    use hyalo_core::uninstall::Uninstall;

    const NOW: Timestamp = Timestamp(1_700_000_000);

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: String::new(),
            keywords: Vec::new(),
            kind: Kind::App,
            action: Action::Open(title.to_string()),
        }
    }

    fn apps(titles: &[&str]) -> Vec<Item> {
        titles.iter().map(|title| app(title)).collect()
    }

    fn search(query: &str, items: &[Item]) -> Results {
        found(query, items, &[])
    }

    /// The rows for `query` when the index finds `files`.
    fn found(query: &str, items: &[Item], files: &[Item]) -> Results {
        let ranking = (&Frecency::default(), &Places::default());
        let files = rank(query, files, ranking.0, ranking.1, NOW);
        Results::search(query, items, files, ranking.0, ranking.1, NOW)
    }

    fn file(path: &str) -> Item {
        Item::file(path.to_string(), false)
    }

    /// By score, which puts the apps that match above every file never
    /// picked, even one that matches better.
    #[test]
    fn files_found_join_the_items_by_score() {
        let files = [file(r"C:\notes"), file(r"C:\my notes.md")];
        let results = found("notes", &apps(&["Sticky Notes", "Notes"]), &files);
        assert_eq!(
            titles(&results),
            ["Notes", "Sticky Notes", "notes", "my notes.md"]
        );
    }

    /// A row the index lists for filters alone, scored `score`.
    fn unmatched(path: &str, score: f64) -> Ranked {
        Ranked {
            item: file(path),
            found: Match::none(),
            score,
        }
    }

    #[test]
    fn filters_alone_list_the_files_let_through_in_their_order_picked_ones_lifted() {
        let files = vec![
            unmatched(r"C:\new.pdf", 0.0),
            unmatched(r"C:\old.pdf", 0.0),
            unmatched(r"C:\picked.pdf", 0.0),
        ];
        let picked = file(r"C:\picked.pdf");
        let mut frecency = Frecency::default();
        frecency.record(&picked.id, "ext:pdf", NOW);
        let places = Places::default();
        let listed = |text: &str| {
            let picked = std::slice::from_ref(&picked);
            Results::filtered(text, picked, files.clone(), &frecency, &places, NOW)
        };
        assert_eq!(
            titles(&listed("ext:pdf")),
            ["picked.pdf", "new.pdf", "old.pdf"]
        );
        assert_eq!(
            titles(&listed("ext:docx")),
            ["new.pdf", "old.pdf", "picked.pdf"],
            "picked for another query"
        );
        let none = Results::filtered("ext:pdf", [], Vec::new(), &frecency, &places, NOW);
        assert!(none.rows().is_empty());
    }

    #[test]
    fn a_file_found_both_picked_and_by_the_index_is_listed_once() {
        let picked = file(r"C:\notes.txt");
        let both = std::slice::from_ref(&picked);
        let results = found("notes", both, both);
        assert_eq!(titles(&results), ["notes.txt"]);
    }

    fn titles(results: &Results) -> Vec<&str> {
        results
            .rows()
            .iter()
            .map(|row| row.item.title.as_str())
            .collect()
    }

    fn picked(results: &Results) -> Option<&str> {
        results.picked().map(|row| row.item.title.as_str())
    }

    #[test]
    fn an_empty_query_lists_nothing() {
        let results = search("", &apps(&["Notepad", "Paint"]));
        assert_eq!(titles(&results), Vec::<&str>::new());
        assert_eq!(picked(&results), None);
    }

    #[test]
    fn lists_matches_best_first() {
        let results = search("note", &apps(&["Sticky Notes", "Paint", "Notepad"]));
        assert_eq!(titles(&results), ["Notepad", "Sticky Notes"]);
    }

    #[test]
    fn lists_at_most_eight_rows() {
        let many: Vec<String> = (1..=10).map(|n| format!("App {n:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let results = search("app", &apps(&many));
        assert_eq!(titles(&results), &many[..MAX_ROWS]);
    }

    /// The rows for `query` under the calculator's answer.
    fn answered(query: &str, items: &[Item]) -> Results {
        search(query, items).answered(calculator::answer(query, Language::English))
    }

    #[test]
    fn an_app_with_no_path_says_it_is_an_app_in_the_language() {
        let mut app = apps(&["Calculator"]).remove(0);
        app.subtitle = String::new();
        assert_eq!(subtitle(&app, Language::English), "App");
        assert_eq!(subtitle(&app, Language::Russian), "Приложение");
        app.subtitle = r"C:\Tools\calc.exe".to_string();
        assert_eq!(subtitle(&app, Language::Russian), r"C:\Tools\calc.exe");
        let folder = Item::file(r"C:\Users".to_string(), true);
        assert_eq!(subtitle(&folder, Language::Russian), r"C:\");
        let mut nothing = Item::file(r"C:\Users".to_string(), true);
        nothing.subtitle = String::new();
        assert_eq!(subtitle(&nothing, Language::Russian), "", "not an app");
    }

    #[test]
    fn an_answer_is_listed_first() {
        let results = answered("2+2", &apps(&["2+2 Notes", "Paint"]));
        assert_eq!(titles(&results), ["4", "2+2 Notes"]);
        assert_eq!(picked(&results), Some("4"));
    }

    fn answer_title(query: &str, rates: Option<&Rates>) -> Option<String> {
        answer(query, rates, &[], Language::English).map(|item| item.title)
    }

    #[test]
    fn a_prefix_and_words_are_answered_with_a_web_search_and_the_rows_follow() {
        let query = "g it";
        let engines = web::defaults(Language::English);
        let answer = |query| answer(query, None, &engines, Language::English);
        let rows = search(query, &apps(&["Git Bash", "Paint"]));
        let results = rows.answered(answer(query));
        assert_eq!(titles(&results), ["Search Google for “it”"]);
        let git = search("git", &apps(&["Git Bash"]));
        let results = git.answered(answer("git"));
        assert_eq!(titles(&results), ["Git Bash"], "no web row for a word");
        let notes = search("w notes", &apps(&["w notes app"]));
        let results = notes.answered(answer("w notes"));
        assert_eq!(
            titles(&results),
            ["Search Wikipedia for “notes”", "w notes app"]
        );
    }

    #[test]
    fn the_switcher_lists_at_most_eight_windows_marking_what_the_filter_matched() {
        let windows: Vec<Item> = (1..=10)
            .map(|n| switcher::item(n, &format!("Window {n:02}"), "App"))
            .collect();
        let all = Results::windows("", &windows);
        assert_eq!(titles(&all).len(), MAX_ROWS);
        assert_eq!(titles(&all)[0], "Window 01", "front first");
        let found = Results::windows("10", &windows);
        assert_eq!(titles(&found), ["Window 10"]);
        assert_eq!(found.rows()[0].matched, [7, 8]);
        assert_eq!(picked(&found), Some("Window 10"));
    }

    #[test]
    fn the_history_lists_at_most_eight_entries_marking_what_the_filter_matched() {
        let mut history = History::default();
        for n in 1..=10 {
            let copied = Copied {
                content: Content::Text {
                    text: format!("copy {n:02}"),
                    html: None,
                    rtf: None,
                },
                app: String::new(),
                at: NOW,
            };
            history.add(copied).expect("kept");
        }
        let entries = history.entries().iter();
        let rows: Vec<Item> = entries
            .map(|entry| entry.item(NOW, Language::English))
            .collect();
        let all = Results::copied("", &rows);
        assert_eq!(titles(&all).len(), MAX_ROWS);
        assert_eq!(titles(&all)[0], "copy 10", "newest first");
        let found = Results::copied("01", &rows);
        assert_eq!(titles(&found), ["copy 01"]);
        assert_eq!(found.rows()[0].matched, [5, 6]);
        assert_eq!(picked(&found), Some("copy 01"));
    }

    #[test]
    fn a_web_search_is_answered_before_the_calculator() {
        let ddg = Engine::custom("g", "https://duckduckgo.com/?q={query}").expect("an engine");
        let item = answer("g 2+2", None, &[ddg], Language::English).expect("a search");
        assert_eq!(item.title, "Search duckduckgo.com for “2+2”");
        assert_eq!(
            answer_title("2+2", None),
            Some("4".to_string()),
            "no prefix"
        );
    }

    #[test]
    fn arithmetic_is_answered_by_the_calculator() {
        let rates = crate::rates::tests::fixture();
        assert_eq!(answer_title("2+2*2", Some(&rates)), Some("6".to_string()));
        assert_eq!(answer_title("2+2*2", None), Some("6".to_string()));
    }

    #[test]
    fn a_currency_is_converted_at_the_rates() {
        let rates = crate::rates::tests::fixture();
        let title = answer_title("100 usd", Some(&rates));
        assert_eq!(title, Some("8\u{A0}275.36\u{A0}₽".to_string()));
    }

    #[test]
    fn without_rates_a_currency_has_no_answer() {
        assert_eq!(answer_title("100 usd", None), None);
        assert_eq!(answer_title("notepad", None), None);
    }

    #[test]
    fn no_answer_leaves_the_rows_as_they_are() {
        let results = search("note", &apps(&["Notes"])).answered(None);
        assert_eq!(titles(&results), ["Notes"]);
    }

    #[test]
    fn the_answer_takes_one_of_the_eight_rows() {
        let many: Vec<String> = (1..=10).map(|n| format!("1+1 App {n:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let results = answered("1+1", &apps(&many));
        assert_eq!(titles(&results)[0], "2");
        assert_eq!(titles(&results)[1..], many[..MAX_ROWS - 1]);
    }

    #[test]
    fn a_row_carries_the_letters_its_title_matched() {
        let results = search("vsc", &apps(&["Visual Studio Code"]));
        assert_eq!(results.rows()[0].matched, [0, 7, 14]);
    }

    #[test]
    fn the_best_row_starts_selected() {
        let results = search("note", &apps(&["Sticky Notes", "Notepad"]));
        assert_eq!(results.selected(), 0);
        assert_eq!(picked(&results), Some("Notepad"));
    }

    #[test]
    fn down_moves_the_selection_and_wraps_to_the_top() {
        let mut results = search("a", &apps(&["a1", "a2", "a3"]));
        results.down();
        assert_eq!(picked(&results), Some("a2"));
        results.down();
        results.down();
        assert_eq!(picked(&results), Some("a1"));
    }

    #[test]
    fn up_moves_the_selection_and_wraps_to_the_bottom() {
        let mut results = search("a", &apps(&["a1", "a2", "a3"]));
        results.up();
        assert_eq!(picked(&results), Some("a3"));
        results.up();
        assert_eq!(picked(&results), Some("a2"));
    }

    fn refreshed(results: &Results, query: &str, items: &[Item]) -> Results {
        results.kept(search(query, items))
    }

    #[test]
    fn a_refresh_keeps_the_selected_item_selected_where_it_moved() {
        let mut results = search("note", &apps(&["Notes", "Notepad"]));
        results.down();
        let results = refreshed(&results, "note", &apps(&["Note", "Notes", "Notepad"]));
        assert_eq!(titles(&results), ["Note", "Notes", "Notepad"]);
        assert_eq!(picked(&results), Some("Notepad"));
    }

    #[test]
    fn a_refresh_that_drops_the_selected_item_selects_the_first_row() {
        let mut results = search("note", &apps(&["Notes", "Notepad"]));
        results.down();
        let results = refreshed(&results, "note", &apps(&["Notes", "Note"]));
        assert_eq!(picked(&results), Some("Note"));
    }

    #[test]
    fn a_refresh_lists_what_the_query_finds_in_the_new_items() {
        let results = refreshed(&Results::default(), "note", &apps(&["Notepad"]));
        assert_eq!(picked(&results), Some("Notepad"));
    }

    #[test]
    fn select_moves_the_selection_to_a_row_and_says_so() {
        let mut results = search("a", &apps(&["a1", "a2", "a3"]));
        assert!(results.select(2));
        assert_eq!(picked(&results), Some("a3"));
        assert!(!results.select(2), "already there");
    }

    #[test]
    fn select_past_the_last_row_changes_nothing() {
        let mut results = search("a", &apps(&["a1", "a2", "a3"]));
        assert!(!results.select(3));
        assert_eq!(picked(&results), Some("a1"));
    }

    fn commands(query: &str) -> Results {
        let items = system_commands::items(Language::English, true);
        search(query, &items)
    }

    fn said(results: &Results) -> Vec<String> {
        let rows = 0..results.rows().len();
        let said = rows.map(|row| results.subtitle(row, Language::English));
        said.map(|line| line.into_owned()).collect()
    }

    /// Enter on the selected row: what it carries out now, if anything.
    fn enter(results: &mut Results) -> Option<Action> {
        let wanted = results.picked()?.item.action.clone();
        results.confirm(wanted)
    }

    fn restart() -> Option<Action> {
        Some(Action::System(Command::Restart))
    }

    #[test]
    fn enter_on_a_row_that_asks_first_asks_and_the_next_enter_runs_it() {
        let mut results = commands("restart");
        assert_eq!(picked(&results), Some("Restart"));
        assert!(!results.asking());
        assert_eq!(enter(&mut results), None, "asks");
        assert!(results.asking());
        assert_eq!(said(&results)[0], "Press Enter again to restart");
        assert_eq!(enter(&mut results), restart(), "runs");
        assert!(!results.asking());
    }

    #[test]
    fn a_row_that_does_not_ask_runs_at_the_first_enter() {
        let mut results = commands("lock");
        assert_eq!(picked(&results), Some("Lock"));
        let lock = Some(Action::System(Command::Lock));
        assert_eq!(enter(&mut results), lock);
        assert!(!results.asking());
        let mut results = search("note", &apps(&["Notepad"]));
        let open = Some(Action::Open("Notepad".to_string()));
        assert_eq!(enter(&mut results), open);
    }

    /// Uninstalling the app `title`.
    fn uninstalling(title: &str) -> Action {
        Action::Uninstall(Uninstall::Package {
            name: title.to_string(),
            full_name: format!("{title}_1.0.0.0_x64__0000000000000"),
        })
    }

    #[test]
    fn a_row_asked_to_uninstall_its_app_says_so_and_enter_uninstalls_it() {
        let mut results = search("note", &apps(&["Notepad", "Notes"]));
        assert!(results.ask(uninstalling("Notepad")));
        assert!(results.asking());
        assert_eq!(
            said(&results),
            ["Press Enter to uninstall Notepad", "App"],
            "the selected row asks, the others are apps"
        );
        assert_eq!(enter(&mut results), Some(uninstalling("Notepad")));
        assert!(!results.asking());
        assert_eq!(said(&results)[0], "App");
        let open = Some(Action::Open("Notepad".to_string()));
        assert_eq!(enter(&mut results), open, "the next Enter opens it");
    }

    #[test]
    fn only_a_row_asks_and_only_about_what_asks_first() {
        let mut nothing = Results::default();
        assert!(!nothing.ask(uninstalling("Notepad")), "no row");
        assert!(!nothing.asking());
        let mut results = search("note", &apps(&["Notepad"]));
        assert!(!results.ask(Action::Open("Notepad".to_string())));
        assert!(!results.asking());
        results.ask(uninstalling("Notepad"));
        assert!(!results.ask(Action::Open("Notepad".to_string())));
        assert!(!results.asking(), "asked about something else since");
    }

    #[test]
    fn only_the_asking_row_says_so_and_the_others_say_what_they_are() {
        let mut results = commands("s");
        assert_eq!(enter(&mut results), None);
        let lines = said(&results);
        assert_eq!(lines[0], "Press Enter again to sleep");
        assert!(
            lines[1..].iter().all(|line| *line == "System command"),
            "{lines:?}"
        );
        results.calm();
        assert_eq!(said(&results)[0], "System command");
    }

    #[test]
    fn moving_the_selection_stops_the_asking() {
        let moves: [fn(&mut Results); 3] = [Results::down, Results::up, |results| {
            results.select(1);
        }];
        for step in moves {
            let mut results = commands("s");
            enter(&mut results);
            step(&mut results);
            assert!(!results.asking());
            assert_eq!(enter(&mut results), None, "a new row asks again");
        }
    }

    #[test]
    fn down_and_up_stop_the_asking_even_on_the_only_row() {
        let mut results = commands("restart");
        enter(&mut results);
        results.down();
        assert!(!results.asking());
        enter(&mut results);
        results.up();
        assert!(!results.asking());
    }

    #[test]
    fn pointing_at_the_asking_row_again_keeps_it_asking() {
        let mut results = commands("s");
        enter(&mut results);
        assert!(!results.select(0));
        assert!(results.asking());
    }

    #[test]
    fn calming_says_whether_it_was_asking() {
        let mut results = commands("restart");
        assert!(!results.calm());
        enter(&mut results);
        assert!(results.calm());
        assert!(!results.asking());
    }

    #[test]
    fn a_refresh_keeps_the_asking_row_asking_and_one_without_it_does_not() {
        let items = system_commands::items(Language::English, true);
        let mut results = commands("restart");
        enter(&mut results);
        let fresh = results.kept(search("restart", &items));
        assert!(fresh.asking());
        let mut fresh = fresh;
        assert_eq!(enter(&mut fresh), restart(), "about the same");
        let gone = results.kept(search("restart", &apps(&["Restart"])));
        assert!(!gone.asking(), "another item, though named the same");
        let mut listed = search("note", &apps(&["Notepad"]));
        listed.ask(uninstalling("Notepad"));
        let mut kept = listed.kept(search("note", &apps(&["Notes", "Notepad"])));
        assert_eq!(enter(&mut kept), Some(uninstalling("Notepad")), "moved");
    }

    #[test]
    fn each_system_command_shows_a_symbol_of_its_own() {
        let glyphs: std::collections::HashSet<char> = Command::ALL
            .into_iter()
            .filter_map(|command| glyph(Kind::Command, &Action::System(command)))
            .collect();
        assert_eq!(glyphs.len(), Command::ALL.len());
        assert!(
            glyphs
                .iter()
                .all(|glyph| ('\u{E000}'..='\u{F8FF}').contains(glyph))
        );
        let lock = glyph(Kind::Command, &Action::System(Command::Lock));
        assert_eq!(lock, Some('\u{E72E}'));
    }

    #[test]
    fn rows_without_an_icon_of_their_own_show_a_symbol_and_the_rest_their_icons() {
        let copy = Action::CopyText("4".to_string());
        assert_eq!(glyph(Kind::Calculator, &copy), Some('\u{E8EF}'));
        let search = Action::Open("https://www.google.com/search?q=x".to_string());
        assert_eq!(glyph(Kind::Web, &search), Some('\u{E774}'));
        assert_eq!(glyph(Kind::Notice, &Action::Settings), Some('\u{E7BA}'));
        let page = Action::Open("ms-settings:display".to_string());
        assert_eq!(glyph(Kind::Setting, &page), None);
        assert_eq!(glyph(Kind::App, &Action::Open("x".to_string())), None);
        let back = |content| Action::PutBack(std::sync::Arc::new(content));
        let text = back(Content::Text {
            text: "a".to_string(),
            html: None,
            rtf: None,
        });
        assert_eq!(glyph(Kind::Clip, &text), Some('\u{E77F}'));
        let picture = back(Content::Image {
            width: 1,
            height: 1,
            png: Vec::new(),
        });
        let until = "until its thumbnail comes";
        assert_eq!(glyph(Kind::Clip, &picture), Some('\u{E91B}'), "{until}");
        let files = back(Content::Files(Vec::new()));
        let until = "until the first one's icon comes";
        assert_eq!(glyph(Kind::Clip, &files), Some('\u{E8A5}'), "{until}");
    }

    #[test]
    fn moving_through_no_rows_picks_nothing() {
        let mut results = search("", &apps(&["a1"]));
        results.down();
        results.up();
        assert_eq!(picked(&results), None);
    }
}
