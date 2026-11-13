//! Ranking: which items a query shows, best first.
//!
//! An item's score is how well its title matches, plus a weight for its
//! kind (apps above every file and folder, folders above files), plus a
//! boost from frecency.

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
            let (found, points) = find(&pattern, item)?;
            let picks = frecency.score(&item.id, query, now);
            let place = item.path().map_or(0.0, |path| places.of_path(path));
            Some(Ranked {
                score: score(points, item.kind, place, picks),
                item: item.clone(),
                found,
            })
        })
        .collect();
    ranked.sort_by(|one, other| other.score.total_cmp(&one.score));
    ranked
}

/// Points a match on one of an item's other names loses: a tenth of a
/// kind of match, so its title wins when both match as well.
const OTHER_NAME: f64 = 2.0;

/// How the query best matches `item`, by its title or one of its other
/// names, and the points that is worth. A match on another name
/// highlights nothing: the title is what is shown.
fn find(pattern: &matcher::Pattern, item: &Item) -> Option<(Match, f64)> {
    let title = pattern.find(&item.title).map(|found| {
        let points = found.points();
        (found, points)
    });
    let others = item.keywords.iter().filter_map(|keyword| {
        let found = pattern.find(keyword)?;
        let points = found.points() - OTHER_NAME;
        let positions = Vec::new();
        Some((Match { positions, ..found }, points))
    });
    let best = |one: &(Match, f64), other: &(Match, f64)| one.1.total_cmp(&other.1);
    title.into_iter().chain(others).max_by(best)
}

/// Points a folder has over a file, and an answer over an app: a little
/// more than half a kind of match.
const KIND_STEP: f64 = 12.0;

/// Points an app has over a file: more than an exact match of a folder in
/// a favoured place (100 + 12 + 3) has over the weakest match of an app
/// (a substring of another name in the wrong layout: 20 - 2 - 5). What
/// matches among the apps comes first, unless a file or folder was picked
/// for the query.
const LAUNCHED: f64 = 104.0;

/// An item's score: the `points` its match is worth, its kind, the
/// points of the place it lives in (see `places`) and its frecency score.
pub fn score(points: f64, kind: Kind, place: f64, picks: f64) -> f64 {
    points + kind_weight(kind) + place + boost(picks)
}

/// How much less a match in an item's other words is worth than the
/// same match in its title: one kind of match.
const BY_OTHER: f64 = 16.0;

/// The `items` `filter` finds, best first, each with the letters of its
/// title it matched: by its title, or, a little weaker and with nothing
/// to highlight, by the best of the `other` words it gives for an item.
/// For lists ranked only among themselves, such as the open windows:
/// equal ones keep their order; an empty filter keeps every item.
pub fn listed<'a>(
    filter: &str,
    items: &'a [Item],
    other: impl Fn(&'a Item) -> Vec<&'a str>,
) -> Vec<(Item, Vec<usize>)> {
    if filter.is_empty() {
        return items
            .iter()
            .map(|item| (item.clone(), Vec::new()))
            .collect();
    }
    let pattern = matcher::Pattern::new(filter);
    let by_other = |item: &'a Item| {
        let points = other(item)
            .into_iter()
            .filter_map(|words| pattern.find(words));
        let best = points.map(|found| found.points()).max_by(f64::total_cmp)?;
        Some((best - BY_OTHER, Vec::new()))
    };
    let found = |item: &'a Item| match pattern.find(&item.title) {
        Some(found) => Some((found.points(), found.positions)),
        None => by_other(item),
    };
    let mut ranked: Vec<(f64, Item, Vec<usize>)> = items
        .iter()
        .filter_map(|item| {
            let (points, positions) = found(item)?;
            Some((points, item.clone(), positions))
        })
        .collect();
    ranked.sort_by(|one, other| other.0.total_cmp(&one.0));
    ranked
        .into_iter()
        .map(|(_, item, positions)| (item, positions))
        .collect()
}

fn kind_weight(kind: Kind) -> f64 {
    match kind {
        Kind::File => 0.0,
        Kind::Folder => KIND_STEP,
        // Windows' settings rank like apps: a page should not drown
        // under files.
        Kind::App | Kind::Setting | Kind::Command => LAUNCHED,
        // A notice or a web search is never ranked; it goes first, as an
        // answer does. Windows and what was copied are ranked only among
        // themselves.
        Kind::Calculator | Kind::Web | Kind::Window | Kind::Notice | Kind::Clip => {
            LAUNCHED + KIND_STEP
        }
    }
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
            keywords: Vec::new(),
            kind,
            action: Action::Open(id.to_string()),
        }
    }

    fn known_as(title: &str, keywords: &[&str]) -> Item {
        Item {
            keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
            ..item(title, title, Kind::App)
        }
    }

    #[test]
    fn an_item_is_found_by_its_other_names_with_nothing_to_highlight() {
        let items = [
            known_as("Дисплей", &["Display", "brightness"]),
            file("Notes"),
        ];
        let ranked = rank(
            "bright",
            &items,
            &Frecency::default(),
            &Places::default(),
            NOW,
        );
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].item.title, "Дисплей");
        assert_eq!(ranked[0].found.strength, matcher::Strength::Prefix);
        assert_eq!(ranked[0].found.positions, Vec::<usize>::new());
    }

    #[test]
    fn the_best_of_its_names_counts_and_the_title_wins_a_tie() {
        let items = [known_as("Sound", &["volume", "sounds"])];
        let ranked = rank(
            "sound",
            &items,
            &Frecency::default(),
            &Places::default(),
            NOW,
        );
        assert_eq!(ranked[0].found.strength, matcher::Strength::Exact);
        assert_eq!(ranked[0].found.positions, [0, 1, 2, 3, 4], "the title's");
        let items = [known_as("Volume mixer", &["volume"])];
        let ranked = rank(
            "volume",
            &items,
            &Frecency::default(),
            &Places::default(),
            NOW,
        );
        assert_eq!(ranked[0].found.strength, matcher::Strength::Exact);
    }

    #[test]
    fn a_title_outranks_the_same_match_on_another_name() {
        let items = [known_as("Mixer", &["sound"]), known_as("Sound", &[])];
        assert_eq!(
            order("sound", &items, &Frecency::default()),
            ["Sound", "Mixer"]
        );
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
    fn a_windows_setting_and_a_system_command_rank_like_an_app() {
        let items = [
            item("file", "Sound", Kind::File),
            item("setting", "Sound", Kind::Setting),
            item("command", "Sound", Kind::Command),
            item("folder", "Sound", Kind::Folder),
            item("app", "Sound", Kind::App),
        ];
        assert_eq!(
            order("sound", &items, &Frecency::default()),
            ["setting", "command", "app", "folder", "file"]
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

    /// `docker` listed five folders of that name above Docker Desktop:
    /// people open the app far more often than such a folder.
    #[test]
    fn a_matching_app_outranks_every_file_and_folder_never_picked_however_well_they_match() {
        let items = [
            Item::file(r"C:\Users\Me\Desktop\docker".to_string(), true),
            at(r"C:\Users\Me\Desktop\docker"),
            known_as("Containers", &["undockered"]),
            item("app", "Docker Desktop", Kind::App),
        ];
        assert_eq!(
            order_in("docker", &items, &Frecency::default(), &desktop()),
            [
                "app",
                "Containers",
                r"C:\Users\Me\Desktop\docker",
                r"C:\Users\Me\Desktop\docker",
            ]
        );
        let ranked = rank("docker", &items, &Frecency::default(), &desktop(), NOW);
        let kinds: Vec<Kind> = ranked.iter().map(|ranked| ranked.item.kind).collect();
        assert_eq!(kinds, [Kind::App, Kind::App, Kind::Folder, Kind::File]);
    }

    #[test]
    fn a_folder_picked_for_the_query_rises_above_an_app_that_only_contains_it() {
        let folder = Item::file(r"D:\docker".to_string(), true);
        let items = [item("app", "Undockered", Kind::App), folder.clone()];
        assert_eq!(
            order("docker", &items, &picked(&folder.id.0, "docker", 1)),
            [r"D:\docker", "app"]
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
