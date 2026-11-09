//! Finding the best files and folders for a query among millions.
//!
//! Each thread takes a slice of the entries. A name whose letters are not
//! in order is skipped at once (`Pattern::ceiling`); so is one whose best
//! possible score cannot beat what the thread already holds. Only the
//! rest are matched and scored by hyalo-core, as every item is.

use std::num::NonZero;
use std::ops::Range;
use std::thread;

use hyalo_core::item::Kind;
use hyalo_core::matcher::{Ceiling, Match, Pattern};
use hyalo_core::places::{self, Places};
use hyalo_core::rank::{self, Ranked};

use crate::index::Entry;
use crate::{EntryId, Index};

/// The fewest entries worth a thread of their own.
const CHUNK: usize = 4096;

/// What a search found, and how much work it took.
pub struct Searched {
    /// Best first.
    pub rows: Vec<Ranked>,
    /// The names scored in full; their ceilings ruled the others out.
    pub scored: usize,
}

impl Index {
    /// The `limit` best files and folders for `query`, best first, scored
    /// as `hyalo_core::rank` scores items (no picks: the caller ranks
    /// picked files itself). Equal scores keep the order of the index.
    pub fn search(&self, query: &str, places: &Places, limit: usize) -> Searched {
        let pattern = &Pattern::new(query);
        let (mut hits, scored) = thread::scope(|scope| {
            let threads: Vec<_> = self
                .slices()
                .map(|slice| scope.spawn(move || self.best_in(slice, pattern, places, limit)))
                .collect();
            let found = threads.into_iter().map(|thread| thread.join());
            let found = found.map(|scan| scan.expect("a search thread"));
            found.fold((Vec::new(), 0), |(mut hits, scored), (more, also)| {
                hits.extend(more);
                (hits, scored + also)
            })
        });
        // Stable: equal scores stay in the order of the slices.
        hits.sort_by(|one, other| other.score.total_cmp(&one.score));
        hits.truncate(limit);
        let rows = hits.into_iter().map(|hit| self.ranked(hit, pattern));
        Searched {
            rows: rows.collect(),
            scored,
        }
    }

    /// The entries cut into one slice per core, none under `CHUNK`.
    fn slices(&self) -> impl Iterator<Item = Range<usize>> {
        let all = self.entries.len();
        let cores = thread::available_parallelism()
            .unwrap_or(NonZero::<usize>::MIN)
            .get();
        let size = all.div_ceil(cores).max(CHUNK);
        (0..all)
            .step_by(size)
            .map(move |start| start..all.min(start + size))
    }

    /// The best `limit` entries in `slice`.
    fn best_in(
        &self,
        slice: Range<usize>,
        pattern: &Pattern,
        places: &Places,
        limit: usize,
    ) -> (Vec<Hit>, usize) {
        let mut scan = Scan {
            index: self,
            pattern,
            places,
            best: Best {
                limit,
                hits: Vec::new(),
            },
            seen: vec![(u32::MAX, 0.0); SEEN],
            scored: 0,
        };
        for at in slice {
            scan.consider(at as u32);
        }
        (scan.best.hits, scan.scored)
    }

    /// The folded names of the folders from a root down to `folder`.
    fn folders(&self, folder: u32) -> Vec<&str> {
        let above = self.ancestry(folder);
        let mut folders: Vec<&str> = above
            .map(|at| self.folded_of(self.entries[at as usize]))
            .collect();
        folders.reverse();
        folders
    }

    /// The row for a hit, with the letters its name matched.
    fn ranked(&self, hit: Hit, pattern: &Pattern) -> Ranked {
        let item = self.item(EntryId(hit.at)).expect("a hit is in the index");
        let found = hit.found.or_else(|| pattern.find(&item.title));
        Ranked {
            item,
            found: found.expect("an exact ceiling is a match"),
            score: hit.score,
        }
    }
}

/// Folders whose place points one thread remembers: siblings come one
/// after another, so most entries find their folder here.
const SEEN: usize = 256;

/// One thread's look through its slice.
struct Scan<'a> {
    index: &'a Index,
    pattern: &'a Pattern,
    places: &'a Places,
    best: Best,
    /// `(folder, points of its place)`, each folder in slot `folder % SEEN`.
    seen: Vec<(u32, f64)>,
    /// Names that got past their ceilings.
    scored: usize,
}

impl Scan<'_> {
    /// Offers the entry at `at` to `best`, unless it cannot beat what is
    /// there: judged roughly from the folded name, then from the name's
    /// words; only a possible acronym needs `find` to know its points.
    fn consider(&mut self, at: u32) {
        let index = self.index;
        let entry = index.entries[at as usize];
        if !entry.is_live() || entry.is_root() {
            return;
        }
        let (folded, name, kind) = (index.folded_of(entry), index.name_of(entry), kind_of(entry));
        let floor = self.best.floor();
        let beats = |points: f64| rank::score(points, kind, places::BOOST, 0.0) > floor;
        let rough = self
            .pattern
            .rough(folded)
            .filter(|rough| beats(rough.points()));
        let ceiling = rough.and_then(|rough| rough.ceiling(name));
        let Some(ceiling) = ceiling.filter(|ceiling| beats(ceiling.points())) else {
            return;
        };
        self.scored += 1;
        let Some((points, found)) = matched(self.pattern, name, ceiling) else {
            return;
        };
        let score = rank::score(points, kind, self.place(entry.parent), 0.0);
        self.best.offer(Hit { at, score, found });
    }

    /// The points of the place of what is in `folder`.
    fn place(&mut self, folder: u32) -> f64 {
        let slot = folder as usize % SEEN;
        if self.seen[slot].0 != folder {
            let points = self.places.points(&self.index.folders(folder));
            self.seen[slot] = (folder, points);
        }
        self.seen[slot].1
    }
}

/// The points a name's match is worth, with the match itself when finding
/// it was the only way to know them.
fn matched(pattern: &Pattern, name: &str, ceiling: Ceiling) -> Option<(f64, Option<Match>)> {
    match ceiling {
        Ceiling::Exactly(points) => Some((points, None)),
        Ceiling::AtMost(_) => {
            let found = pattern.find(name)?;
            Some((found.points(), Some(found)))
        }
    }
}

fn kind_of(entry: Entry) -> Kind {
    match entry.is_folder() {
        true => Kind::Folder,
        false => Kind::File,
    }
}

/// An entry that matched, and its score.
struct Hit {
    at: u32,
    score: f64,
    /// Found while searching only when needed; the rows shown find it.
    found: Option<Match>,
}

/// The best hits so far, best first, at most `limit`.
struct Best {
    limit: usize,
    hits: Vec<Hit>,
}

impl Best {
    /// The score a hit must beat to be kept.
    fn floor(&self) -> f64 {
        match self.hits.len() < self.limit {
            true => f64::NEG_INFINITY,
            false => self.hits.last().map_or(f64::INFINITY, |hit| hit.score),
        }
    }

    /// Keeps `hit` if it is among the best; after any equal ones, which
    /// came first.
    fn offer(&mut self, hit: Hit) {
        let place = self.hits.partition_point(|kept| kept.score >= hit.score);
        self.hits.insert(place, hit);
        self.hits.truncate(self.limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryId, synthetic};
    use hyalo_core::frecency::{Frecency, Timestamp};
    use hyalo_core::item::{Item, Kind};
    use hyalo_core::rank::rank;

    /// C: holding the names given, each in its own folder `C:\in`; a name
    /// ending in a backslash is a folder.
    fn holding(names: &[&str]) -> Index {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let folder = index.add(c, "in", true).unwrap();
        for name in names {
            let bare = name.trim_end_matches('\\');
            index.add(folder, bare, bare != *name).unwrap();
        }
        index
    }

    fn titles(index: &Index, query: &str, limit: usize) -> Vec<String> {
        let found = index.search(query, &Places::default(), limit);
        found
            .rows
            .into_iter()
            .map(|ranked| ranked.item.title)
            .collect()
    }

    #[test]
    fn lists_the_matching_files_and_folders_best_first() {
        let index = holding(&["my notes.md", "notes.txt", "Notes\\", "report.pdf"]);
        assert_eq!(
            titles(&index, "notes", 10),
            ["Notes", "notes.txt", "my notes.md"]
        );
    }

    #[test]
    fn a_row_is_the_entry_with_its_path_and_the_letters_matched() {
        let index = holding(&["Visual Studio Code\\"]);
        let found = index.search("vsc", &Places::default(), 10);
        let row = &found.rows[0];
        assert_eq!(
            row.item,
            Item::file(r"C:\in\Visual Studio Code".into(), true)
        );
        assert_eq!(row.item.kind, Kind::Folder);
        assert_eq!(row.found.positions, [0, 7, 14]);
    }

    #[test]
    fn lists_at_most_the_limit_keeping_the_best() {
        let index = holding(&["ab", "a", "abc"]);
        assert_eq!(titles(&index, "a", 2), ["a", "ab"]);
        assert_eq!(titles(&index, "a", 0), Vec::<String>::new());
    }

    #[test]
    fn equal_scores_keep_the_order_of_the_index() {
        let index = holding(&["note b", "note a", "note c"]);
        assert_eq!(titles(&index, "note", 2), ["note b", "note a"]);
    }

    #[test]
    fn roots_and_removed_entries_are_not_found() {
        let mut index = holding(&["cat.txt", "cow.txt"]);
        index.remove(EntryId(2)).unwrap();
        assert_eq!(titles(&index, "c", 10), ["cow.txt"]);
    }

    #[test]
    fn an_entry_in_the_first_place_is_found() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let d = index.add_root("D:").unwrap();
        index.remove(c).unwrap();
        let file = index.add(d, "found.txt", false).unwrap();
        assert_eq!(file, EntryId(0));
        assert_eq!(titles(&index, "found", 5), ["found.txt"]);
    }

    #[test]
    fn a_name_that_can_at_best_tie_the_rows_kept_is_not_scored() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let favoured = index.add(c, "fav", true).unwrap();
        let other = index.add(c, "other", true).unwrap();
        index.add(favoured, "note.txt", false).unwrap();
        // A prefix, at best in a favoured place: a tie with note.txt.
        index.add(other, "note.md", false).unwrap();
        let places = Places::new(&[r"C:\fav"]);
        let one = index.search("note", &places, 1);
        assert_eq!(one.rows[0].item.title, "note.txt");
        assert_eq!(one.scored, 1);
        assert_eq!(index.search("note", &places, 2).scored, 2);
    }

    #[test]
    fn a_word_hidden_in_a_hump_is_not_passed_over() {
        let index = holding(&["a shell", "PowerShell\\"]);
        assert_eq!(titles(&index, "shell", 1), ["PowerShell"]);
    }

    #[test]
    fn a_query_in_the_wrong_layout_finds_too() {
        let index = holding(&["notes.txt", "Блокнот.lnk"]);
        assert_eq!(titles(&index, "тщеуы", 10), ["notes.txt"]);
        assert_eq!(titles(&index, ",kjr", 10), ["Блокнот.lnk"]);
    }

    #[test]
    fn where_a_file_lives_moves_it_up_or_down() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let modules = index.add(c, "node_modules", true).unwrap();
        let desktop = index.add(c, "Desktop", true).unwrap();
        let other = index.add(c, "other", true).unwrap();
        for folder in [modules, other, desktop] {
            index.add(folder, "index.js", false).unwrap();
        }
        let places = Places::new(&[r"C:\Desktop"]);
        let found = index.search("index", &places, 10);
        let paths: Vec<&str> = found
            .rows
            .iter()
            .map(|row| row.item.id.0.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                r"C:\Desktop\index.js",
                r"C:\other\index.js",
                r"C:\node_modules\index.js"
            ]
        );
    }

    /// Every file and folder of `index`, in its order, as rows.
    fn rows(index: &Index) -> Vec<Item> {
        (0..index.entries.len() as u32)
            .map(EntryId)
            .filter(|&id| index.path(id).is_some_and(|path| path.contains('\\')))
            .filter_map(|id| index.item(id))
            .collect()
    }

    #[test]
    fn finds_what_ranking_every_file_finds_on_a_large_drive() {
        let index = synthetic::drive(20_000, 5);
        let rows = rows(&index);
        let places = Places::new(&[r"C:\Desktop"]);
        let now = Timestamp(0);
        for query in ["no", "report", "rpt", "отчет", "мысщву", "x", "e"] {
            let all = rank(query, &rows, &Frecency::default(), &places, now);
            let expected: Vec<(String, f64)> = all
                .into_iter()
                .take(20)
                .map(|ranked| (ranked.item.id.0, ranked.score))
                .collect();
            let found: Vec<(String, f64)> = index
                .search(query, &places, 20)
                .rows
                .into_iter()
                .map(|ranked| (ranked.item.id.0, ranked.score))
                .collect();
            assert_eq!(found, expected, "{query}");
        }
    }
}
