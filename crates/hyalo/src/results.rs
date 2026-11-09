//! The rows the window lists for a query, and which one Enter picks.

use hyalo_core::calculator;
use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::item::Item;
use hyalo_core::rank::rank;

/// The most rows the window lists.
pub const MAX_ROWS: usize = 8;

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
}

impl Results {
    /// The calculator's answer if the query is arithmetic, then the best
    /// items for `query`: `MAX_ROWS` in all, the first one selected. An
    /// empty query lists nothing: it would match everything equally.
    pub fn search<'a>(
        query: &str,
        items: impl IntoIterator<Item = &'a Item>,
        frecency: &Frecency,
        now: Timestamp,
    ) -> Self {
        if query.is_empty() {
            return Self::default();
        }
        let answer = calculator::answer(query).map(|item| Listed {
            item,
            matched: Vec::new(),
        });
        let ranked = rank(query, items, frecency, now)
            .into_iter()
            .map(|ranked| Listed {
                item: ranked.item,
                matched: ranked.found.positions,
            });
        let rows = answer.into_iter().chain(ranked).take(MAX_ROWS).collect();
        Self { rows, selected: 0 }
    }

    /// The rows for `query` in items that changed, such as after the
    /// provider looked again. The item selected before stays selected
    /// where it moved, so what Enter opens does not change under the
    /// user's fingers; if it is gone, the first row is.
    pub fn refreshed<'a>(
        &self,
        query: &str,
        items: impl IntoIterator<Item = &'a Item>,
        frecency: &Frecency,
        now: Timestamp,
    ) -> Self {
        let mut fresh = Self::search(query, items, frecency, now);
        let kept = self.picked().and_then(|picked| {
            let mut rows = fresh.rows.iter();
            rows.position(|row| row.item.id == picked.item.id)
        });
        fresh.selected = kept.unwrap_or(fresh.selected);
        fresh
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
        }
        moved
    }

    /// One row down; from the last row back to the first. With no rows
    /// there is nowhere to go.
    pub fn down(&mut self) {
        let next = (self.selected + 1).checked_rem(self.rows.len());
        self.selected = next.unwrap_or(self.selected);
    }

    /// One row up; from the first row round to the last.
    pub fn up(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.checked_sub(1).unwrap_or(last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::item::{Action, ItemId, Kind};

    const NOW: Timestamp = Timestamp(1_700_000_000);

    fn app(title: &str) -> Item {
        Item {
            id: ItemId(title.to_string()),
            title: title.to_string(),
            subtitle: String::new(),
            kind: Kind::App,
            action: Action::Open(title.to_string()),
        }
    }

    fn apps(titles: &[&str]) -> Vec<Item> {
        titles.iter().map(|title| app(title)).collect()
    }

    fn search(query: &str, items: &[Item]) -> Results {
        Results::search(query, items, &Frecency::default(), NOW)
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

    #[test]
    fn an_arithmetic_query_lists_its_answer_first() {
        let results = search("2+2", &apps(&["2+2 Notes", "Paint"]));
        assert_eq!(titles(&results), ["4", "2+2 Notes"]);
        assert_eq!(picked(&results), Some("4"));
    }

    #[test]
    fn the_answer_takes_one_of_the_eight_rows() {
        let many: Vec<String> = (1..=10).map(|n| format!("1+1 App {n:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let results = search("1+1", &apps(&many));
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
        results.refreshed(query, items, &Frecency::default(), NOW)
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

    #[test]
    fn moving_through_no_rows_picks_nothing() {
        let mut results = search("", &apps(&["a1"]));
        results.down();
        results.up();
        assert_eq!(picked(&results), None);
    }
}
