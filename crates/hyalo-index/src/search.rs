//! Finding the best files and folders for a query among millions.
//!
//! Each thread takes a slice of the entries. A name whose letters are not
//! in order is skipped at once (`Pattern::ceiling`); so is one whose best
//! possible score cannot beat what the thread already holds. Only the
//! rest are matched and scored by hyalo-core, as every item is.

use std::collections::HashSet;
use std::num::NonZero;
use std::ops::Range;
use std::thread;

use hyalo_core::filter::Filters;
use hyalo_core::item::Kind;
use hyalo_core::matcher::{self, Ceiling, Match, Pattern};
use hyalo_core::places::{self, Places};
use hyalo_core::rank::{self, Ranked};

use crate::index::{Entry, offset, same};
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
        self.search_with(query, &Filters::default(), places, limit)
    }

    /// The same, for the files and folders `filters` lets through. With no
    /// words what it lets through is ranked by its place alone, the newest
    /// first among equals; neither words nor filters list nothing.
    pub fn search_with(
        &self,
        query: &str,
        filters: &Filters,
        places: &Places,
        limit: usize,
    ) -> Searched {
        if query.is_empty() && filters.is_empty() {
            return Searched {
                rows: Vec::new(),
                scored: 0,
            };
        }
        let pattern = &Pattern::new(query);
        let (narrowing, unmatched) = (&self.narrowing(filters), query.is_empty());
        let (mut hits, scored) = thread::scope(|scope| {
            let threads: Vec<_> = self
                .slices()
                .map(|slice| {
                    let search =
                        move || self.best_in(slice, pattern, narrowing, places, limit, unmatched);
                    scope.spawn(search)
                })
                .collect();
            let found = threads.into_iter().map(|thread| thread.join());
            let found = found.map(|scan| scan.expect("a search thread"));
            found.fold((Vec::new(), 0), |(mut hits, scored), (more, also)| {
                hits.extend(more);
                (hits, scored + also)
            })
        });
        // Stable: equal scores stay in the order of the slices.
        hits.sort_by(|one, other| {
            let newer = other.newer.cmp(&one.newer);
            other.score.total_cmp(&one.score).then(newer)
        });
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

    /// The best `limit` entries in `slice`; `unmatched`, by their place
    /// alone.
    fn best_in(
        &self,
        slice: Range<usize>,
        pattern: &Pattern,
        narrowing: &Narrowing,
        places: &Places,
        limit: usize,
        unmatched: bool,
    ) -> (Vec<Hit>, usize) {
        let mut scan = Scan {
            index: self,
            pattern,
            narrowing,
            unmatched,
            places,
            best: Best {
                limit,
                hits: Vec::new(),
            },
            seen: vec![(u32::MAX, 0.0); SEEN],
            under: vec![None; SEEN],
            scored: 0,
        };
        for at in slice {
            scan.consider(at as u32);
        }
        (scan.best.hits, scan.scored)
    }

    /// The folders `folders` names: by a full path, the one there; by a
    /// name, every entry so named (a file or an entry removed is no one's
    /// folder, so taking them in changes nothing).
    fn folders_named(&self, folders: &[String]) -> HashSet<u32> {
        let mut found = HashSet::new();
        for folder in folders {
            if folder.contains(['\\', ':']) {
                let there = self
                    .find(folder)
                    .filter(|&id| self.is_folder(id) == Some(true));
                found.extend(there.map(|id| id.0));
                continue;
            }
            let folded = matcher::folded(folder);
            let named = |entry: &Entry| {
                self.folded_of(*entry) == folded && same(self.name_of(*entry), folder)
            };
            let all = self.entries.iter().enumerate();
            found.extend(
                all.filter(|(_, entry)| named(entry))
                    .map(|(at, _)| offset(at)),
            );
        }
        found
    }

    /// What `filters` lets through, made ready to ask of one path after
    /// another.
    pub fn sieve<'a>(&'a self, filters: &'a Filters) -> Sieve<'a> {
        Sieve {
            index: self,
            narrowing: self.narrowing(filters),
        }
    }

    /// `filters` made ready: the folders `in:` names found.
    fn narrowing<'a>(&self, filters: &'a Filters) -> Narrowing<'a> {
        let named = !filters.folders.is_empty();
        Narrowing {
            filters,
            within: named.then(|| self.folders_named(&filters.folders)),
        }
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
            found: found.expect("a hit with points is a match"),
            score: hit.score,
        }
    }
}

/// Folders whose place points one thread remembers: siblings come one
/// after another, so most entries find their folder here.
const SEEN: usize = 256;

/// Filters made ready to ask whether this file or that passes them.
pub struct Sieve<'a> {
    index: &'a Index,
    narrowing: Narrowing<'a>,
}

impl Sieve<'_> {
    /// Whether the file or folder at the full path `path` is in the index
    /// and passes the filters.
    pub fn lets_through(&self, path: &str) -> bool {
        let index = self.index;
        let Some(id) = index.find(path) else {
            return false;
        };
        let entry = index.entries[id.0 as usize];
        let under =
            |within: &HashSet<u32>| index.ancestry(entry.parent).any(|at| within.contains(&at));
        let within = self.narrowing.within.as_ref();
        !entry.is_root() && self.narrowing.admits(index, id.0, entry) && within.is_none_or(under)
    }
}

/// Filters made ready for one search.
struct Narrowing<'a> {
    filters: &'a Filters,
    /// The folders `in:` names, when it names any: what is listed is
    /// under one of them.
    within: Option<HashSet<u32>>,
}

impl Narrowing<'_> {
    /// Whether the entry at `at` is of a kind, extension, size and time
    /// the filters let through.
    fn admits(&self, index: &Index, at: u32, entry: Entry) -> bool {
        let filters = self.filters;
        if entry.is_folder() && !filters.take_folders() {
            return false;
        }
        let at = at as usize;
        let sized = filters.sized_and_dated(index.sizes[at], i64::from(index.times[at]));
        sized && self.typed(index, entry)
    }

    /// Whether the entry's extension is one asked for, if any are: its
    /// name is looked at only then, which a search without one spares.
    fn typed(&self, index: &Index, entry: Entry) -> bool {
        let wanted = &self.filters.extensions;
        if wanted.is_empty() {
            return true;
        }
        let extension = index
            .folded_of(entry)
            .rsplit_once('.')
            .map(|(_, extension)| extension);
        wanted
            .iter()
            .any(|wanted| Some(wanted.as_str()) == extension)
    }
}

/// One thread's look through its slice.
struct Scan<'a> {
    index: &'a Index,
    pattern: &'a Pattern,
    narrowing: &'a Narrowing<'a>,
    /// No words: what the filters let through is listed unmatched.
    unmatched: bool,
    places: &'a Places,
    best: Best,
    /// `(folder, points of its place)`, each folder in slot `folder % SEEN`.
    seen: Vec<(u32, f64)>,
    /// `(folder, whether it is under a folder `in:` names)`, the same way.
    under: Vec<Option<(u32, bool)>>,
    /// Names that got past their ceilings.
    scored: usize,
}

impl Scan<'_> {
    /// Offers the entry at `at` to `best`, unless it cannot beat what is
    /// there: judged roughly from the folded name, then from the name's
    /// words; a possible acronym is checked word by word only then.
    fn consider(&mut self, at: u32) {
        let entry = self.index.entries[at as usize];
        let listed = entry.is_live() && !entry.is_root();
        if !listed || !self.narrowing.admits(self.index, at, entry) || !self.inside(entry.parent) {
            return;
        }
        match self.unmatched {
            true => self.list(at, entry),
            false => self.match_name(at, entry),
        }
    }

    /// Offers the entry at `at` by its place alone, the newer first.
    fn list(&mut self, at: u32, entry: Entry) {
        let score = rank::score(0.0, kind_of(entry), self.place(entry.parent), 0.0);
        let newer = self.index.times[at as usize];
        self.best.offer(Hit {
            at,
            score,
            found: Some(Match::none()),
            newer: Some(newer),
        });
    }

    /// Offers the entry at `at` by how its name matches, unless that cannot
    /// beat what is kept.
    fn match_name(&mut self, at: u32, entry: Entry) {
        let index = self.index;
        let (folded, name, kind) = (index.folded_of(entry), index.name_of(entry), kind_of(entry));
        let floor = self.best.floor();
        let beats = |points: f64| rank::score(points, kind, places::BOOST, 0.0) > floor;
        let rough = self.pattern.rough(folded);
        let Some(rough) = rough.filter(|rough| beats(rough.points())) else {
            return;
        };
        let ceiling = rough.ceiling(name);
        let Some(ceiling) = ceiling.filter(|ceiling| beats(ceiling.points())) else {
            return;
        };
        self.scored += 1;
        let points = match ceiling {
            Ceiling::Exactly(points) => Some(points),
            Ceiling::AtMost(_) => rough.exactly(name),
        };
        let Some(points) = points else {
            return;
        };
        let score = rank::score(points, kind, self.place(entry.parent), 0.0);
        self.best.offer(Hit {
            at,
            score,
            found: None,
            newer: None,
        });
    }

    /// Whether what is in `folder` is under a folder `in:` names, if it
    /// names any.
    fn inside(&mut self, folder: u32) -> bool {
        let Some(within) = &self.narrowing.within else {
            return true;
        };
        let slot = folder as usize % SEEN;
        match self.under[slot] {
            Some((seen, under)) if seen == folder => under,
            _ => {
                let under = self.index.ancestry(folder).any(|at| within.contains(&at));
                self.under[slot] = Some((folder, under));
                under
            }
        }
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
    /// No letters, for a row listed by filters alone; a match's letters
    /// are found for the rows shown alone.
    found: Option<Match>,
    /// When it was written, for ties among rows listed unmatched; none
    /// for a match, whose ties keep the order of the index.
    newer: Option<u32>,
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
        let ahead = |kept: &Hit| (kept.score, kept.newer) >= (hit.score, hit.newer);
        let place = self.hits.partition_point(ahead);
        self.hits.insert(place, hit);
        self.hits.truncate(self.limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryId, synthetic};
    use hyalo_core::filter::{Clock, Query};
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

    /// 2026-11-10 14:30 in Moscow.
    const CLOCK: Clock = Clock {
        now: 1_794_310_200,
        offset: 3 * 60 * 60,
    };
    const DAY: i64 = 24 * 60 * 60;

    /// C: with Desktop, Downloads\old and Work\deep, files in each: a
    /// name, its folder, its size and how many days ago it was written;
    /// a name ending in a backslash is a folder.
    fn drive() -> Index {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let desktop = index.add(c, "Desktop", true).unwrap();
        let downloads = index.add(c, "Downloads", true).unwrap();
        let old = index.add(downloads, "old", true).unwrap();
        let work = index.add(c, "Work", true).unwrap();
        let deep = index.add(work, "deep", true).unwrap();
        let files = [
            (desktop, "report.pdf", 2_000_000, 0),
            (desktop, "report.docx", 30_000, 1),
            (downloads, "manual.PDF", 500, 3),
            (old, "scan.pdf", 9_000_000, 400),
            (work, "notes.txt", 100, 2),
            (deep, "report.pdf", 1_000, 10),
            (deep, "pdf\\", 0, 5),
        ];
        for (folder, name, size, days) in files {
            let bare = name.trim_end_matches('\\');
            let entry = index.add(folder, bare, bare != name).unwrap();
            index.set_stat(entry, size, CLOCK.now - days * DAY).unwrap();
        }
        index
    }

    fn found(index: &Index, query: &str) -> Vec<String> {
        let query = Query::parse(query, CLOCK);
        let places = Places::new(&[r"C:\Desktop"]);
        let rows = index
            .search_with(&query.words, &query.filters, &places, 10)
            .rows;
        rows.into_iter().map(|row| row.item.id.0).collect()
    }

    #[test]
    fn filters_alone_list_what_they_let_through_best_place_first_then_newest() {
        let index = drive();
        assert_eq!(
            found(&index, "ext:pdf"),
            [
                r"C:\Desktop\report.pdf",
                r"C:\Downloads\manual.PDF",
                r"C:\Work\deep\report.pdf",
                r"C:\Downloads\old\scan.pdf",
            ]
        );
    }

    #[test]
    fn words_and_filters_go_together() {
        let index = drive();
        assert_eq!(
            found(&index, "report ext:pdf"),
            [r"C:\Desktop\report.pdf", r"C:\Work\deep\report.pdf"]
        );
        assert_eq!(
            found(&index, "ext:docx,txt rep"),
            [r"C:\Desktop\report.docx"]
        );
    }

    #[test]
    fn a_folder_is_named_or_given_by_its_path_and_holds_what_is_under_it_at_any_depth() {
        let index = drive();
        // Folders first, then the newest: deep was never given a time.
        assert_eq!(
            found(&index, "in:work"),
            [
                r"C:\Work\deep\pdf",
                r"C:\Work\deep",
                r"C:\Work\notes.txt",
                r"C:\Work\deep\report.pdf"
            ]
        );
        assert_eq!(
            found(&index, r"report in:c:\WORK\"),
            [r"C:\Work\deep\report.pdf"]
        );
        assert_eq!(
            found(&index, "ext:pdf in:Downloads"),
            [r"C:\Downloads\manual.PDF", r"C:\Downloads\old\scan.pdf"]
        );
        assert_eq!(
            found(&index, "in:old in:deep ext:pdf"),
            [r"C:\Work\deep\report.pdf", r"C:\Downloads\old\scan.pdf"]
        );
        assert!(found(&index, "in:nowhere").is_empty());
        assert!(found(&index, r"in:C:\Nowhere").is_empty());
        assert!(
            found(&index, r"in:C:\Work\notes.txt").is_empty(),
            "a file holds nothing"
        );
    }

    #[test]
    fn sizes_and_times_narrow_files_and_folders_have_no_size() {
        let index = drive();
        assert_eq!(
            found(&index, "size:>1mb"),
            [r"C:\Desktop\report.pdf", r"C:\Downloads\old\scan.pdf"]
        );
        assert_eq!(
            found(&index, "size:<=1000 ext:pdf"),
            [r"C:\Downloads\manual.PDF", r"C:\Work\deep\report.pdf"]
        );
        assert_eq!(found(&index, "mod:today"), [r"C:\Desktop\report.pdf"]);
        assert_eq!(
            found(&index, "in:deep mod:7d"),
            [r"C:\Work\deep\pdf"],
            "a folder has a time"
        );
        assert_eq!(
            found(&index, "pdf mod:<2026-01-01"),
            [r"C:\Downloads\old\scan.pdf"]
        );
    }

    #[test]
    fn neither_words_nor_filters_list_nothing() {
        let index = drive();
        assert!(found(&index, "").is_empty());
        let none = index.search("", &Places::default(), 10);
        assert!(none.rows.is_empty());
        assert_eq!(none.scored, 0);
    }

    #[test]
    fn a_row_listed_by_filters_alone_matched_no_letters() {
        let index = drive();
        let query = Query::parse("ext:docx", CLOCK);
        let rows = index
            .search_with(&query.words, &query.filters, &Places::default(), 10)
            .rows;
        assert_eq!(rows[0].item.title, "report.docx");
        assert!(rows[0].found.positions.is_empty());
    }

    #[test]
    fn a_folder_is_named_letter_by_letter_in_any_case_but_not_ё_as_е() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        for folder in ["Ёлка", "Елка"] {
            let folder = index.add(c, folder, true).unwrap();
            index.add(folder, "toy.txt", false).unwrap();
        }
        let query = Query::parse("in:ЁЛКА", CLOCK);
        let rows = index
            .search_with("", &query.filters, &Places::default(), 10)
            .rows;
        let paths: Vec<String> = rows.into_iter().map(|row| row.item.id.0).collect();
        assert_eq!(paths, [r"C:\Ёлка\toy.txt"]);
    }

    #[test]
    fn folders_that_share_a_place_in_a_threads_memory_are_told_apart() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let target = index.add(c, "target", true).unwrap();
        // Six hundred folders, every third in target, a file in each:
        // folders 128 apart fall in the same of the 256 places remembered,
        // and every third pair of them is one in target, one not.
        for n in 0..600 {
            let parent = if n % 3 == 0 { target } else { c };
            let folder = index.add(parent, &format!("folder {n}"), true).unwrap();
            index.add(folder, "file.txt", false).unwrap();
        }
        let query = Query::parse("in:target", CLOCK);
        let found = index.search_with("", &query.filters, &Places::default(), 2000);
        assert_eq!(found.rows.len(), 400, "200 folders and their files");
    }

    #[test]
    fn a_sieve_lets_through_the_paths_the_filters_do() {
        let index = drive();
        let sieve = |text: &str| -> Vec<bool> {
            let query = Query::parse(text, CLOCK);
            let sieve = index.sieve(&query.filters);
            let paths = [
                r"C:\Desktop\report.pdf",
                r"c:\work\DEEP\report.pdf",
                r"C:\Work\deep\pdf",
                r"C:\Nowhere\report.pdf",
                "C:",
            ];
            paths.iter().map(|path| sieve.lets_through(path)).collect()
        };
        assert_eq!(sieve("ext:pdf"), [true, true, false, false, false]);
        assert_eq!(sieve("in:work"), [false, true, true, false, false]);
        assert_eq!(sieve("mod:today"), [true, false, false, false, false]);
        assert_eq!(
            sieve(r"in:c:\desktop size:>1mb"),
            [true, false, false, false, false]
        );
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
