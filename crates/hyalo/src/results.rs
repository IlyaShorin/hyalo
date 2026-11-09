//! The rows the window lists for a query, and which one Enter picks.

use hyalo_core::frecency::{Frecency, Timestamp};
use hyalo_core::item::Item;
use hyalo_core::rank::{Ranked, rank};

/// The most rows the window lists.
pub const MAX_ROWS: usize = 8;

/// The ranked rows for one query and the selected row.
#[derive(Debug, Default)]
pub struct Results {
    rows: Vec<Ranked>,
    /// Within `rows` whenever there are any.
    selected: usize,
}

impl Results {
    /// The best `MAX_ROWS` items for `query`, the first one selected.
    /// An empty query lists nothing: it would match everything equally.
    pub fn search<'a>(
        query: &str,
        items: impl IntoIterator<Item = &'a Item>,
        frecency: &Frecency,
        now: Timestamp,
    ) -> Self {
        if query.is_empty() {
            return Self::default();
        }
        let mut rows = rank(query, items, frecency, now);
        rows.truncate(MAX_ROWS);
        Self { rows, selected: 0 }
    }

    pub fn rows(&self) -> &[Ranked] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The selected row: what Enter opens.
    pub fn picked(&self) -> Option<&Ranked> {
        self.rows.get(self.selected)
    }

    /// One row down; from the last row back to the first.
    pub fn down(&mut self) {
        self.selected = (self.selected + 1)
            .checked_rem(self.rows.len())
            .unwrap_or(0);
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

    #[test]
    fn moving_through_no_rows_picks_nothing() {
        let mut results = search("", &apps(&["a1"]));
        results.down();
        results.up();
        assert_eq!(picked(&results), None);
    }
}
