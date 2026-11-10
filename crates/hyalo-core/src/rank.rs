//! Ranking: which items a query shows, best first.
//!
//! An item's score is how well its title matches, plus a weight for its
//! kind (apps above folders above files), plus a boost from frecency.

use crate::frecency::{Frecency, Timestamp};
use crate::item::{Item, Kind};
use crate::matcher::{self, Match};
use crate::places::Places;

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
    places: &Places,
    now: Timestamp,
) -> Vec<Ranked> {
    let pattern = matcher::Pattern::new(query);
    let mut ranked: Vec<Ranked> = items
        .into_iter()
        .filter_map(|item| {
            let found = pattern.find(&item.title)?;
            let picks = frecency.score(&item.id, query, now);
            let place = item.path().map_or(0.0, |path| places.of_path(path));
            Some(Ranked {
                score: score(found.points(), item.kind, place, picks),
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

/// An item's score: the `points` its match is worth, its kind, the
/// points of the place it lives in (see `places`) and its frecency score.
pub fn score(points: f64, kind: Kind, place: f64, picks: f64) -> f64 {
    points + kind_weight(kind) + place + boost(picks)
}

fn kind_weight(kind: Kind) -> f64 {
    let step = match kind {
        Kind::File => 0,
        Kind::Folder => 1,
        Kind::App => 2,
        // A notice is never ranked; it goes first, as an answer does.
        Kind::Calculator | Kind::Notice => 3,
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
        order_in(query, items, frecency, &Places::default())
    }

    fn order_in(query: &str, items: &[Item], frecency: &Frecency, places: &Places) -> Vec<String> {
        rank(query, items, frecency, places, NOW)
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
        let ranked = rank("vsc", &items, &Frecency::default(), &Places::default(), NOW);
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

    fn at(path: &str) -> Item {
        Item::file(path.to_string(), false)
    }

    fn desktop() -> Places {
        Places::new(&[r"C:\Users\Me\Desktop"])
    }

    #[test]
    fn a_file_in_a_favoured_folder_ranks_above_an_equal_one_elsewhere() {
        let items = [at(r"D:\notes.txt"), at(r"C:\Users\Me\Desktop\notes.txt")];
        assert_eq!(
            order_in("notes", &items, &Frecency::default(), &desktop()),
            [r"C:\Users\Me\Desktop\notes.txt", r"D:\notes.txt"]
        );
    }

    #[test]
    fn a_file_in_a_buried_folder_ranks_below_an_equal_one() {
        let items = [at(r"C:\app\node_modules\index.js"), at(r"C:\app\index.js")];
        assert_eq!(
            order_in("index", &items, &Frecency::default(), &desktop()),
            [r"C:\app\index.js", r"C:\app\node_modules\index.js"]
        );
    }

    #[test]
    fn an_app_outranks_a_favoured_file_that_matches_one_kind_better() {
        let items = [
            at(r"C:\Users\Me\Desktop\code.txt"),
            item("vscode", "Visual Studio Code", Kind::App),
        ];
        assert_eq!(
            order_in("code", &items, &Frecency::default(), &desktop()),
            ["vscode", r"C:\Users\Me\Desktop\code.txt"]
        );
    }

    #[test]
    fn a_ranked_score_is_the_score_of_its_parts() {
        let file = at(r"C:\Users\Me\Desktop\Notes\code.txt");
        let frecency = picked(&file.id.0, "code", 1);
        let ranked = rank("co", [&file], &frecency, &desktop(), NOW);
        let picks = frecency.score(&file.id, "co", NOW);
        let parts = score(ranked[0].found.points(), Kind::File, 3.0, picks);
        assert_eq!(ranked[0].score, parts);
        assert!(score(80.0, Kind::Folder, 3.0, 0.0) > score(80.0, Kind::File, 3.0, 0.0));
        assert!(score(80.0, Kind::File, 3.0, 0.0) > score(80.0, Kind::File, 0.0, 0.0));
        assert!(score(80.0, Kind::File, 0.0, 1.0) > score(80.0, Kind::File, 0.0, 0.0));
        assert!(score(81.0, Kind::File, 0.0, 0.0) > score(80.0, Kind::File, 0.0, 0.0));
    }
}
