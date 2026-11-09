//! Ranking: which items a query shows, best first.
//!
//! An item's score is how well its title matches, plus a weight for its
//! kind (apps above folders above files), plus a boost from frecency.

use crate::frecency::{Frecency, Timestamp};
use crate::item::{Item, Kind};
use crate::matcher::{self, Match};

/// One row of the ranked list.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub item: Item,
    /// How the query matched the title, with positions to highlight.
    pub found: Match,
    pub score: f64,
}

/// The items whose titles match `query`, best first; equal scores keep
/// the order they came in. Only the items that match are cloned.
pub fn rank<'a>(
    query: &str,
    items: impl IntoIterator<Item = &'a Item>,
    frecency: &Frecency,
    now: Timestamp,
) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = items
        .into_iter()
        .filter_map(|item| {
            let found = matcher::find(query, &item.title)?;
            let picks = frecency.score(&item.id, query, now);
            Some(Ranked {
                score: found.points() + kind_weight(item.kind) + boost(picks),
                item: item.clone(),
                found,
            })
        })
        .collect();
    ranked.sort_by(|one, other| other.score.total_cmp(&one.score));
    ranked
}

/// Points per step up the kind ladder (file, folder, app, calculator): a
/// little more than one kind of match, so an app outranks a file that
/// matches one kind better, but not two.
const KIND_STEP: f64 = 12.0;

fn kind_weight(kind: Kind) -> f64 {
    let step = match kind {
        Kind::File => 0,
        Kind::Folder => 1,
        Kind::App => 2,
        Kind::Calculator => 3,
    };
    f64::from(step) * KIND_STEP
}

/// The most frecency can add: four kinds of match, so the weakest match
/// picked any number of times stays below an exact one.
const BOOST_LIMIT: f64 = 80.0;

/// The frecency score that earns half the limit. One pick earns less than
/// a kind of match (16 points); two earn more (26.7).
const BOOST_HALF: f64 = 4.0;

/// Frecency in points: rises with every pick, ever more slowly, and never
/// reaches the limit.
fn boost(picks: f64) -> f64 {
    BOOST_LIMIT * picks / (picks + BOOST_HALF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Action, ItemId};

    const NOW: Timestamp = Timestamp(1_700_000_000);

    fn item(id: &str, title: &str, kind: Kind) -> Item {
        Item {
            id: ItemId(id.to_string()),
            title: title.to_string(),
            subtitle: String::new(),
            kind,
            action: Action::Open(id.to_string()),
        }
    }

    fn file(title: &str) -> Item {
        item(title, title, Kind::File)
    }

    fn order(query: &str, items: &[Item], frecency: &Frecency) -> Vec<String> {
        rank(query, items, frecency, NOW)
            .into_iter()
            .map(|ranked| ranked.item.id.0)
            .collect()
    }

    fn picked(id: &str, query: &str, times: usize) -> Frecency {
        let mut frecency = Frecency::default();
        for _ in 0..times {
            frecency.record(&ItemId(id.to_string()), query, NOW);
        }
        frecency
    }

    #[test]
    fn items_that_do_not_match_are_left_out() {
        let items = [file("Code"), file("Chrome")];
        assert_eq!(order("code", &items, &Frecency::default()), ["Code"]);
    }

    #[test]
    fn stronger_matches_rank_first() {
        let items = [file("Sticky Notes"), file("Notepad++"), file("Note")];
        assert_eq!(
            order("note", &items, &Frecency::default()),
            ["Note", "Notepad++", "Sticky Notes"]
        );
    }

    #[test]
    fn a_ranked_item_carries_its_match() {
        let items = [file("Visual Studio Code")];
        let ranked = rank("vsc", &items, &Frecency::default(), NOW);
        assert_eq!(ranked[0].found.positions, [0, 7, 14]);
    }

    #[test]
    fn the_wrong_layout_finds_items_too() {
        let items = [file("Visual Studio Code"), file("Chrome")];
        assert_eq!(
            order("мысщву", &items, &Frecency::default()),
            ["Visual Studio Code"]
        );
    }

    #[test]
    fn for_the_same_match_calculator_then_apps_then_folders_then_files() {
        let items = [
            item("file", "Code", Kind::File),
            item("folder", "Code", Kind::Folder),
            item("app", "Code", Kind::App),
            item("calculator", "Code", Kind::Calculator),
        ];
        assert_eq!(
            order("code", &items, &Frecency::default()),
            ["calculator", "app", "folder", "file"]
        );
    }

    #[test]
    fn an_app_outranks_a_file_that_matches_one_kind_better() {
        let items = [
            item("code.txt", "code.txt", Kind::File),
            item("vscode", "Visual Studio Code", Kind::App),
        ];
        assert_eq!(
            order("code", &items, &Frecency::default()),
            ["vscode", "code.txt"]
        );
    }

    #[test]
    fn a_file_that_matches_two_kinds_better_outranks_an_app() {
        let items = [
            item("vscode", "Visual Studio Code", Kind::App),
            item("vsc.log", "vsc.log", Kind::File),
        ];
        assert_eq!(
            order("vsc", &items, &Frecency::default()),
            ["vsc.log", "vscode"]
        );
    }

    #[test]
    fn a_pick_lifts_an_item_above_an_equal_one() {
        let items = [file("Notes"), file("Notepad")];
        assert_eq!(
            order("note", &items, &picked("Notepad", "note", 1)),
            ["Notepad", "Notes"]
        );
    }

    #[test]
    fn one_pick_does_not_lift_past_a_better_kind_of_match() {
        let items = [file("Visual Studio Code"), file("code.txt")];
        assert_eq!(
            order("code", &items, &picked("Visual Studio Code", "code", 1)),
            ["code.txt", "Visual Studio Code"]
        );
    }

    #[test]
    fn two_picks_lift_past_a_match_one_kind_better() {
        let items = [file("code.txt"), file("Visual Studio Code")];
        assert_eq!(
            order("code", &items, &picked("Visual Studio Code", "code", 2)),
            ["Visual Studio Code", "code.txt"]
        );
    }

    #[test]
    fn no_number_of_picks_lifts_a_substring_past_an_exact_match() {
        let items = [file("Encode"), file("Code")];
        assert_eq!(
            order("code", &items, &picked("Encode", "code", 1000)),
            ["Code", "Encode"]
        );
    }

    #[test]
    fn equal_scores_keep_the_order_they_came_in() {
        let items = [file("Notes b"), file("Notes a")];
        assert_eq!(
            order("notes", &items, &Frecency::default()),
            ["Notes b", "Notes a"]
        );
    }
}
