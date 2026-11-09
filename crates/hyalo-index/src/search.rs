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
use hyalo_core::matcher::{Match, Pattern};
use hyalo_core::places::{self, Places};
use hyalo_core::rank::{self, Ranked};

use crate::index::Entry;
use crate::{EntryId, Index};

/// The fewest entries worth a thread of their own.
const CHUNK: usize = 4096;

impl Index {
    /// The `limit` best files and folders for `query`, best first, scored
    /// as `hyalo_core::rank` scores items (no picks: the caller ranks
    /// picked files itself). Equal scores keep the order of the index.
    pub fn search(&self, query: &str, places: &Places, limit: usize) -> Vec<Ranked> {
        let pattern = &Pattern::new(query);
        let mut hits: Vec<Hit> = thread::scope(|scope| {
            let threads: Vec<_> = self
                .slices()
                .map(|slice| scope.spawn(move || self.best_in(slice, pattern, places, limit)))
                .collect();
            let found = threads.into_iter().map(|thread| thread.join());
            found
                .flat_map(|hits| hits.expect("a search thread"))
                .collect()
        });
        // Stable: equal scores stay in the order of the slices.
        hits.sort_by(|one, other| other.score.total_cmp(&one.score));
        hits.truncate(limit);
        hits.into_iter().map(|hit| self.ranked(hit)).collect()
    }

    /// The entries cut into one slice per core, none under `CHUNK`.
    fn slices(&self) -> impl Iterator<Item = Range<usize>> {
        let all = self.entries.len();
        let cores = thread::available_parallelism().map_or(1, NonZero::get);
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
    ) -> Vec<Hit> {
        let mut best = Best {
            limit,
            hits: Vec::new(),
        };
        for at in slice {
            self.consider(at as u32, pattern, places, &mut best);
        }
        best.hits
    }

    fn consider(&self, at: u32, pattern: &Pattern, places: &Places, best: &mut Best) {
        let entry = self.entries[at as usize];
        if !entry.is_live() || entry.is_root() {
            return;
        }
        let Some(ceiling) = pattern.ceiling(self.folded_of(entry)) else {
            return;
        };
        let kind = kind_of(entry);
        if rank::score(ceiling, kind, places::BOOST, 0.0) <= best.floor() {
            return;
        }
        let Some(found) = pattern.find(self.name_of(entry)) else {
            return;
        };
        let place = places.points(&self.folders(entry));
        let score = rank::score(found.points(), kind, place, 0.0);
        best.offer(Hit { at, score, found });
    }

    /// The folded names of the folders `entry` is in, from its root down.
    fn folders(&self, entry: Entry) -> Vec<&str> {
        let above = self.ancestry(entry.parent);
        let mut folders: Vec<&str> = above
            .map(|at| self.folded_of(self.entries[at as usize]))
            .collect();
        folders.reverse();
        folders
    }

    fn ranked(&self, hit: Hit) -> Ranked {
        Ranked {
            item: self.item(EntryId(hit.at)).expect("a hit is in the index"),
            found: hit.found,
            score: hit.score,
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
    found: Match,
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
        if place < self.limit {
            self.hits.insert(place, hit);
            self.hits.truncate(self.limit);
        }
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
        found.into_iter().map(|ranked| ranked.item.title).collect()
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
        let row = &found[0];
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
        let paths: Vec<&str> = found.iter().map(|row| row.item.id.0.as_str()).collect();
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
                .into_iter()
                .map(|ranked| (ranked.item.id.0, ranked.score))
                .collect();
            assert_eq!(found, expected, "{query}");
        }
    }
}
