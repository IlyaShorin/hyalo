//! The rows the window lists for a query, and which one Enter picks.

use std::collections::HashSet;

use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::item::{Action, Item, Kind};
use hyalo_core::matcher::Match;
use hyalo_core::places::Places;
use hyalo_core::rank::{self, Ranked, rank};
use hyalo_core::rates::Rates;
use hyalo_core::system_commands::Command;
use hyalo_core::text::{Language, Text};
use hyalo_core::{calculator, currency};

/// The most rows the window lists.
pub const MAX_ROWS: usize = 8;

/// The answer for `query`, listed first: the calculator's when the query
/// is arithmetic, else a currency conversion at `rates`, once there are
/// any.
pub fn answer(query: &str, rates: Option<&Rates>, language: Language) -> Option<Item> {
    let calculated = calculator::answer(query, language);
    calculated.or_else(|| currency::answer(query, rates?, language))
}

/// The second line of a row, in `language`: what the item says, or, for
/// an app with no path to show, that it is an app.
pub fn subtitle(item: &Item, language: Language) -> &str {
    match (item.kind, item.subtitle.is_empty()) {
        (Kind::App, true) => language.say(Text::App),
        _ => &item.subtitle,
    }
}

/// The symbol a row shows where an icon goes: a system command's, from
/// Segoe Fluent Icons; `None` for the rest, which have icons.
pub fn glyph(action: &Action) -> Option<char> {
    let Action::System(command) = action else {
        return None;
    };
    Some(match command {
        Command::Lock => '\u{E72E}',
        Command::Sleep => '\u{E708}',
        Command::Hibernate => '\u{E823}',
        Command::Restart => '\u{E777}',
        Command::ShutDown => '\u{E7E8}',
        Command::SignOut => '\u{F3B1}',
        Command::EmptyRecycleBin => '\u{E74D}',
    })
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
    /// Whether the selected row asked to press Enter again: only one
    /// that asks first, after the first Enter.
    asking: bool,
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
            asking: false,
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
        fresh.asking = self.asking && kept.is_some();
        fresh
    }

    /// Whether Enter may run the selected row now: at once, unless the
    /// row asks first (a system command such as Restart) and is not
    /// asking yet; then it starts asking, and the next Enter runs it.
    pub fn confirm(&mut self) -> bool {
        let asks = self
            .picked()
            .is_some_and(|row| row.item.action.asking().is_some());
        let confirmed = self.asking || !asks;
        self.asking = !confirmed;
        confirmed
    }

    /// Whether the selected row asks to press Enter again.
    pub fn asking(&self) -> bool {
        self.asking
    }

    /// Stops asking, as anything but Enter does; whether it was.
    pub fn calm(&mut self) -> bool {
        std::mem::take(&mut self.asking)
    }

    /// The second line of row `row` in `language`: what the asking row
    /// asks, else what its item says.
    pub fn subtitle(&self, row: usize, language: Language) -> &str {
        let item = &self.rows[row].item;
        let asked = (self.asking && row == self.selected).then(|| item.action.asking());
        match asked.flatten() {
            Some(text) => language.say(text),
            None => subtitle(item, language),
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
            self.asking = false;
        }
        moved
    }

    /// One row down; from the last row back to the first. With no rows
    /// there is nowhere to go.
    pub fn down(&mut self) {
        let next = (self.selected + 1).checked_rem(self.rows.len());
        self.selected = next.unwrap_or(self.selected);
        self.asking = false;
    }

    /// One row up; from the first row round to the last.
    pub fn up(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.checked_sub(1).unwrap_or(last);
        self.asking = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::item::{ItemId, Kind};
    use hyalo_core::system_commands;

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

    #[test]
    fn files_found_join_the_items_by_score() {
        let files = [file(r"C:\notes"), file(r"C:\my notes.md")];
        let results = found("notes", &apps(&["Sticky Notes", "Notes"]), &files);
        assert_eq!(
            titles(&results),
            ["Notes", "notes", "Sticky Notes", "my notes.md"]
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
        answer(query, rates, Language::English).map(|item| item.title)
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

    fn said(results: &Results) -> Vec<&str> {
        let rows = 0..results.rows().len();
        rows.map(|row| results.subtitle(row, Language::English))
            .collect()
    }

    #[test]
    fn enter_on_a_row_that_asks_first_asks_and_the_next_enter_runs_it() {
        let mut results = commands("restart");
        assert_eq!(picked(&results), Some("Restart"));
        assert!(!results.asking());
        assert!(!results.confirm(), "asks");
        assert!(results.asking());
        assert_eq!(said(&results)[0], "Press Enter again to restart");
        assert!(results.confirm(), "runs");
        assert!(!results.asking());
    }

    #[test]
    fn a_row_that_does_not_ask_runs_at_the_first_enter() {
        let mut results = commands("lock");
        assert_eq!(picked(&results), Some("Lock"));
        assert!(results.confirm());
        assert!(!results.asking());
        let mut results = search("note", &apps(&["Notepad"]));
        assert!(results.confirm());
        assert!(Results::default().confirm(), "nothing to ask about");
    }

    #[test]
    fn only_the_asking_row_says_so_and_the_others_say_what_they_are() {
        let mut results = commands("s");
        assert!(!results.confirm());
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
            results.confirm();
            step(&mut results);
            assert!(!results.asking());
            assert!(!results.confirm(), "a new row asks again");
        }
    }

    #[test]
    fn down_and_up_stop_the_asking_even_on_the_only_row() {
        let mut results = commands("restart");
        results.confirm();
        results.down();
        assert!(!results.asking());
        results.confirm();
        results.up();
        assert!(!results.asking());
    }

    #[test]
    fn pointing_at_the_asking_row_again_keeps_it_asking() {
        let mut results = commands("s");
        results.confirm();
        assert!(!results.select(0));
        assert!(results.asking());
    }

    #[test]
    fn calming_says_whether_it_was_asking() {
        let mut results = commands("restart");
        assert!(!results.calm());
        results.confirm();
        assert!(results.calm());
        assert!(!results.asking());
    }

    #[test]
    fn a_refresh_keeps_the_asking_row_asking_and_one_without_it_does_not() {
        let items = system_commands::items(Language::English, true);
        let mut results = commands("restart");
        results.confirm();
        let fresh = results.kept(search("restart", &items));
        assert!(fresh.asking());
        let gone = results.kept(search("restart", &apps(&["Restart"])));
        assert!(!gone.asking(), "another item, though named the same");
    }

    #[test]
    fn each_system_command_shows_a_symbol_of_its_own_and_nothing_else_does() {
        let glyphs: std::collections::HashSet<char> = Command::ALL
            .into_iter()
            .filter_map(|command| glyph(&Action::System(command)))
            .collect();
        assert_eq!(glyphs.len(), Command::ALL.len());
        assert!(
            glyphs
                .iter()
                .all(|glyph| ('\u{E000}'..='\u{F8FF}').contains(glyph))
        );
        assert_eq!(glyph(&Action::System(Command::Lock)), Some('\u{E72E}'));
        assert_eq!(
            glyph(&Action::Open("ms-settings:display".to_string())),
            None
        );
    }

    #[test]
    fn moving_through_no_rows_picks_nothing() {
        let mut results = search("", &apps(&["a1"]));
        results.down();
        results.up();
        assert_eq!(picked(&results), None);
    }
}
