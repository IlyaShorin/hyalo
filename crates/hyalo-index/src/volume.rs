//! Volumes whose files come by record number: what the indexer service
//! reads of an NTFS volume's master file table and journal. Each entry of
//! such a volume knows its file's record; the volume maps records back to
//! entries, so a change, which names a record and not a path, finds the
//! entries it changes. A map may point at an entry since removed, or given
//! to another file: an entry is taken for a record's only while it still
//! says it is that record's and lies under that volume's root.

use std::collections::HashMap;

use hyalo_core::volume::{Change, File, Position, ROOT};

use crate::{EntryId, Error, Index};

/// The record of an entry that came by no record: a walk's.
pub(crate) const NO_FILE: u32 = u32::MAX;

/// A record with no entry.
const NONE: u32 = u32::MAX;

/// Records beyond any real volume's: a change naming one is passed over,
/// so a map never takes more than a gigabyte.
const MOST_RECORDS: u32 = 1 << 28;

/// A volume whose files come by record number.
#[derive(Debug, Clone, Default)]
pub(crate) struct Volume {
    root: u32,
    /// For each record, the entry of its first name.
    records: Vec<u32>,
    /// For a record with more names, its hard links, their entries.
    links: HashMap<u32, Vec<u32>>,
    position: Option<Position>,
}

impl Volume {
    pub(crate) fn shrink_to_fit(&mut self) {
        self.records.shrink_to_fit();
        self.links.shrink_to_fit();
    }

    /// The bytes it holds on the heap, roughly for its links.
    pub(crate) fn memory(&self) -> usize {
        let links: usize = self.links.values().map(|links| 8 + 4 * links.len()).sum();
        self.records.capacity() * size_of::<u32>() + links
    }
}

impl Index {
    /// Makes the volume whose root is `root` hold what `change` says of a
    /// file: none of it, if it is gone; else an entry for each of its
    /// names whose folder the index holds, each with its size and last
    /// write. An entry it had keeps its place where it can, so a folder
    /// moved or renamed keeps what it holds. Whether it brought in a
    /// folder the index did not hold, whose files must be asked for.
    pub fn settle(&mut self, root: EntryId, change: &Change) -> Result<bool, Error> {
        self.root(root)?;
        let record = change.record;
        if record >= MOST_RECORDS {
            return Ok(false);
        }
        let held = self.held(root.0, record);
        let (now, came) = match &change.file {
            None => {
                self.drop_entries(record, &held, &[]);
                (Vec::new(), false)
            }
            Some(file) => self.place_file(root.0, record, file, held),
        };
        self.remember(root.0, record, &now);
        Ok(came)
    }

    /// Adds what a listing says of a file to the volume whose root is
    /// `root`, which holds nothing of it yet, its folders listed before
    /// what they hold: each name an entry, in a folder the index holds,
    /// as a walk adds them. Unlike `settle`, it looks for no entry of the
    /// file to move and no other by the same name to replace, which in a
    /// folder of thousands costs a look through them all for each.
    pub fn list(&mut self, root: EntryId, change: &Change) -> Result<(), Error> {
        self.root(root)?;
        let (record, Some(file)) = (change.record, &change.file) else {
            return Ok(());
        };
        if record >= MOST_RECORDS {
            return Ok(());
        }
        let mut now = Vec::new();
        for name in &file.names {
            let Some(folder) = self.folder_of(root.0, name.parent) else {
                continue;
            };
            if let Ok(entry) = self.add(EntryId(folder), &name.text, file.folder) {
                self.files[entry.0 as usize] = record;
                // Added just now, so it is there.
                let _ = self.set_stat(entry, file.size, file.written);
                now.push(entry.0);
            }
        }
        self.remember(root.0, record, &now);
        Ok(())
    }

    /// How far the journal of the volume whose root is `root` was taken in.
    pub fn position(&self, root: EntryId) -> Option<Position> {
        self.volume(root.0)?.position
    }

    /// Notes how far the journal of the volume whose root is `root` was
    /// taken in.
    pub fn set_position(&mut self, root: EntryId, position: Position) -> Result<(), Error> {
        self.root(root)?;
        self.volume_mut(root.0).position = Some(position);
        Ok(())
    }

    /// Gives `file`, of the record `record`, an entry for each name whose
    /// folder is held: those `held` already in place stay, the others move
    /// to the names left, and new entries take the rest. The entries it
    /// has now, and whether a folder came in.
    fn place_file(
        &mut self,
        root: u32,
        record: u32,
        file: &File,
        held: Vec<u32>,
    ) -> (Vec<u32>, bool) {
        let held = self.same_kind(record, file.folder, held);
        let wanted = file.names.iter().filter_map(|name| {
            let folder = self.folder_of(root, name.parent)?;
            Some((folder, name.text.as_str()))
        });
        let wanted: Vec<(u32, &str)> = wanted.collect();
        let (mut now, left, mut held) = self.in_place(held, wanted);
        let mut came = false;
        for (folder, name) in left {
            let old = held.pop();
            let (entry, fresh) = self.place_name(record, old, folder, name, file.folder);
            came |= file.folder && fresh && entry.is_some();
            now.extend(entry);
        }
        self.drop_entries(record, &held, &now);
        for &at in &now {
            // Each is in the index: placed just now.
            let _ = self.set_stat(EntryId(at), file.size, file.written);
        }
        (now, came)
    }

    /// The entries `held` of the record `record`, unless it changed from a
    /// file to a folder or the other way: then none of them stays.
    fn same_kind(&mut self, record: u32, folder: bool, held: Vec<u32>) -> Vec<u32> {
        let other = |at: &u32| self.entries[*at as usize].is_folder() != folder;
        if !held.iter().any(other) {
            return held;
        }
        self.drop_entries(record, &held, &[]);
        Vec::new()
    }

    /// Those of `held` already named as one of `wanted` says, the names
    /// left, and the entries left.
    fn in_place<'a>(
        &self,
        mut held: Vec<u32>,
        wanted: Vec<(u32, &'a str)>,
    ) -> (Vec<u32>, Vec<(u32, &'a str)>, Vec<u32>) {
        let mut now = Vec::new();
        let mut left = Vec::new();
        for (folder, name) in wanted {
            let in_place = held.iter().position(|&at| self.is_at(at, folder, name));
            match in_place {
                Some(found) => now.push(held.swap_remove(found)),
                None => left.push((folder, name)),
            }
        }
        (now, left, held)
    }

    /// Gives the record `record` an entry named `name` in `folder`: the
    /// entry `old` moved there, or one that cannot be, gone, and a new
    /// one. The entry, unless the name is refused, and whether it is new.
    fn place_name(
        &mut self,
        record: u32,
        old: Option<u32>,
        folder: u32,
        name: &str,
        is_folder: bool,
    ) -> (Option<u32>, bool) {
        if let Some(at) = old {
            if self.rename(EntryId(at), EntryId(folder), name).is_ok() {
                return (Some(at), false);
            }
            self.drop_entries(record, &[at], &[]);
        }
        (self.fresh(record, folder, name, is_folder), true)
    }

    /// Whether the entry at `at` is named `name` in the folder `folder`,
    /// letter for letter.
    fn is_at(&self, at: u32, folder: u32, name: &str) -> bool {
        self.entries[at as usize].parent == folder && self.name_at(at) == name
    }

    /// A new entry of the record `record` named `name` in `folder`, in
    /// place of any other there by that name, unless the name is refused.
    fn fresh(&mut self, record: u32, folder: u32, name: &str, is_folder: bool) -> Option<u32> {
        if let Some(there) = self.child(folder, name) {
            // Found among its entries just now.
            let _ = self.remove(EntryId(there));
        }
        let entry = self.add(EntryId(folder), name, is_folder).ok()?;
        self.files[entry.0 as usize] = record;
        Some(entry.0)
    }

    /// Removes the entries among `entries` still the record `record`'s,
    /// but for those it `keeps`: another may have taken a place since.
    fn drop_entries(&mut self, record: u32, entries: &[u32], keeps: &[u32]) {
        for &at in entries {
            if !keeps.contains(&at) && self.files[at as usize] == record {
                // Still its own, so still in the index.
                let _ = self.remove(EntryId(at));
            }
        }
    }

    /// The folder the record `record` is on the volume whose root is
    /// `root`: the root itself for the root's record.
    fn folder_of(&self, root: u32, record: u32) -> Option<u32> {
        if record == ROOT {
            return Some(root);
        }
        let held = self.held(root, record);
        held.into_iter()
            .find(|&at| self.entries[at as usize].is_folder())
    }

    /// The entries the record `record` has on the volume whose root is
    /// `root`.
    fn held(&self, root: u32, record: u32) -> Vec<u32> {
        let Some(volume) = self.volume(root) else {
            return Vec::new();
        };
        let first = volume.records.get(record as usize).copied();
        let links = volume.links.get(&record).into_iter().flatten().copied();
        let held = first.into_iter().chain(links);
        held.filter(|&at| self.holds(root, record, at)).collect()
    }

    /// Whether the entry at `at` is still the record `record`'s, under
    /// `root`.
    fn holds(&self, root: u32, record: u32, at: u32) -> bool {
        let live = self.live(EntryId(at)).is_ok();
        live && self.files[at as usize] == record && self.ancestry(at).last() == Some(root)
    }

    /// Notes that the record `record` has the entries `now` on the volume
    /// whose root is `root`.
    fn remember(&mut self, root: u32, record: u32, now: &[u32]) {
        let volume = self.volume_mut(root);
        let at = record as usize;
        if volume.records.len() <= at {
            volume.records.resize(at + 1, NONE);
        }
        volume.records[at] = now.first().copied().unwrap_or(NONE);
        match now.get(1..).filter(|links| !links.is_empty()) {
            Some(links) => volume.links.insert(record, links.to_vec()),
            None => volume.links.remove(&record),
        };
    }

    /// The entry `id`, if it is a root.
    fn root(&self, id: EntryId) -> Result<(), Error> {
        match self.live(id)?.is_root() {
            true => Ok(()),
            false => Err(Error::NoSuchEntry),
        }
    }

    fn volume(&self, root: u32) -> Option<&Volume> {
        self.volumes.iter().find(|volume| volume.root == root)
    }

    fn volume_mut(&mut self, root: u32) -> &mut Volume {
        let at = match self.volumes.iter().position(|volume| volume.root == root) {
            Some(at) => at,
            None => {
                self.volumes.push(Volume {
                    root,
                    ..Volume::default()
                });
                self.volumes.len() - 1
            }
        };
        &mut self.volumes[at]
    }

    /// Lets go of what the root `root`'s volume was, as the root goes.
    pub(crate) fn forget(&mut self, root: u32) {
        self.volumes.retain(|volume| volume.root != root);
    }

    /// Maps the records of every entry under the root `root` back to them,
    /// and notes `position`; nothing if none came by record.
    pub(crate) fn map_volume(&mut self, root: u32, position: Option<Position>) {
        let mut found: Vec<(u32, u32)> = Vec::new();
        let mut waiting = vec![root];
        while let Some(at) = waiting.pop() {
            for child in self.children(at) {
                waiting.push(child);
                let record = self.files[child as usize];
                if record != NO_FILE {
                    found.push((record, child));
                }
            }
        }
        if found.is_empty() && position.is_none() {
            return;
        }
        found.sort_unstable();
        for links in found.chunk_by(|one, other| one.0 == other.0) {
            let entries: Vec<u32> = links.iter().map(|&(_, at)| at).collect();
            self.remember(root, links[0].0, &entries);
        }
        self.volume_mut(root).position = position;
    }

    /// Maps every root's records, with the positions saved for them;
    /// whether each record is one a volume can have and each position a
    /// root's.
    pub(crate) fn map_volumes(&mut self, positions: &[(u32, Position)]) -> bool {
        let known = |record: &u32| *record < MOST_RECORDS || *record == NO_FILE;
        if !self.files.iter().all(known) {
            return false;
        }
        let roots: Vec<u32> = self.children(u32::MAX).collect();
        for &root in &roots {
            let position = positions.iter().find(|&&(of, _)| of == root);
            self.map_volume(root, position.map(|&(_, position)| position));
        }
        positions.iter().all(|(root, _)| roots.contains(root))
    }

    /// Each volume's root and how far its journal was taken in, for a
    /// snapshot.
    pub(crate) fn positions(&self) -> Vec<(u32, Position)> {
        let noted = self.volumes.iter();
        noted
            .filter_map(|volume| Some((volume.root, volume.position?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::volume::{File, Name, ROOT};

    fn change(record: u32, folder: bool, names: &[(u32, &str)], size: u64, written: i64) -> Change {
        let names = names.iter().map(|&(parent, text)| Name {
            parent,
            text: text.to_string(),
        });
        Change {
            record,
            file: Some(File {
                folder,
                size,
                written,
                names: names.collect(),
            }),
        }
    }

    fn folder(record: u32, parent: u32, name: &str) -> Change {
        change(record, true, &[(parent, name)], 0, 1_000)
    }

    fn file(record: u32, parent: u32, name: &str) -> Change {
        change(record, false, &[(parent, name)], 5, 2_000)
    }

    fn gone(record: u32) -> Change {
        Change { record, file: None }
    }

    /// C: holding Users\Me\notes.txt (records 40, 41, 50) and Windows (60).
    fn volume() -> (Index, EntryId) {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        for listed in [
            folder(40, ROOT, "Users"),
            folder(41, 40, "Me"),
            folder(60, ROOT, "Windows"),
            file(50, 41, "notes.txt"),
        ] {
            index.settle(c, &listed).unwrap();
        }
        (index, c)
    }

    fn found(index: &Index, path: &str) -> bool {
        index.find(path).is_some()
    }

    #[test]
    fn a_listing_settles_into_entries_under_the_root() {
        let (index, _) = volume();
        let notes = index.find(r"C:\Users\Me\notes.txt").expect("listed");
        assert_eq!(index.stat(notes), Some((5, 2_000)));
        assert_eq!(index.is_folder(notes), Some(false));
        let me = index.find(r"C:\Users\Me").expect("listed");
        assert_eq!(index.is_folder(me), Some(true));
        assert_eq!(index.stat(me), Some((0, 1_000)));
        assert_eq!(index.len(), 5, "the root, three folders, a file");
    }

    #[test]
    fn settling_the_same_again_changes_nothing() {
        let (mut index, c) = volume();
        let version = index.version();
        index.settle(c, &file(50, 41, "notes.txt")).unwrap();
        index.settle(c, &folder(41, 40, "Me")).unwrap();
        assert_eq!(index.version(), version);
        assert_eq!(index.len(), 5);
    }

    #[test]
    fn a_file_renamed_or_moved_keeps_its_entry() {
        let (mut index, c) = volume();
        let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
        index.settle(c, &file(50, 41, "renamed.txt")).unwrap();
        assert_eq!(index.find(r"C:\Users\Me\renamed.txt"), Some(notes));
        index.settle(c, &file(50, 60, "renamed.txt")).unwrap();
        assert_eq!(index.find(r"C:\Windows\renamed.txt"), Some(notes));
        assert!(!found(&index, r"C:\Users\Me\notes.txt"));
        assert_eq!(index.len(), 5);
    }

    #[test]
    fn a_folder_renamed_keeps_what_it_holds() {
        let (mut index, c) = volume();
        let moved = index.settle(c, &folder(41, 60, "You")).unwrap();
        assert!(found(&index, r"C:\Windows\You\notes.txt"));
        assert!(!moved, "nothing to ask for: it was here");
    }

    #[test]
    fn a_gone_folder_leaves_with_everything_under_it() {
        let (mut index, c) = volume();
        index.settle(c, &gone(41)).unwrap();
        assert!(!found(&index, r"C:\Users\Me"));
        assert!(!found(&index, r"C:\Users\Me\notes.txt"));
        assert_eq!(index.len(), 3);
        index.settle(c, &gone(41)).unwrap();
        index.settle(c, &gone(999)).unwrap();
        assert_eq!(index.len(), 3, "gone again, or never there: nothing");
    }

    #[test]
    fn a_file_has_an_entry_for_each_hard_link() {
        let (mut index, c) = volume();
        let both = change(70, false, &[(40, "a.txt"), (60, "b.txt")], 7, 3_000);
        index.settle(c, &both).unwrap();
        let (a, b) = (
            index.find(r"C:\Users\a.txt"),
            index.find(r"C:\Windows\b.txt"),
        );
        assert_eq!(a.and_then(|a| index.stat(a)), Some((7, 3_000)));
        assert_eq!(b.and_then(|b| index.stat(b)), Some((7, 3_000)));
        let one = change(70, false, &[(60, "b.txt")], 8, 4_000);
        index.settle(c, &one).unwrap();
        assert!(!found(&index, r"C:\Users\a.txt"));
        assert_eq!(
            index.find(r"C:\Windows\b.txt"),
            b,
            "the link left keeps its entry"
        );
        assert_eq!(b.and_then(|b| index.stat(b)), Some((8, 4_000)));
        index.settle(c, &gone(70)).unwrap();
        assert!(!found(&index, r"C:\Windows\b.txt"));
    }

    #[test]
    fn a_name_in_a_folder_the_index_does_not_hold_is_left_out() {
        let (mut index, c) = volume();
        index.settle(c, &file(80, 999, "nowhere.txt")).unwrap();
        assert_eq!(index.len(), 5);
        // Moved into a folder the index leaves out, such as the recycle bin.
        index.settle(c, &folder(41, 999, "$R123")).unwrap();
        assert!(!found(&index, r"C:\Users\Me"));
        assert!(!found(&index, r"C:\Users\Me\notes.txt"));
        // A file's name is never a folder to put another in.
        index.settle(c, &file(60, ROOT, "Windows")).unwrap();
        index.settle(c, &file(81, 60, "inside a file.txt")).unwrap();
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn a_folder_that_comes_in_says_its_files_must_be_asked_for() {
        let (mut index, c) = volume();
        assert!(index.settle(c, &folder(41, 999, "binned")).is_ok());
        assert_eq!(
            index.settle(c, &folder(41, 40, "Me")),
            Ok(true),
            "back from the bin"
        );
        assert_eq!(index.settle(c, &folder(41, 40, "Me")), Ok(false));
        assert_eq!(index.settle(c, &folder(90, 40, "new")), Ok(true));
        assert_eq!(index.settle(c, &file(91, 90, "new.txt")), Ok(false));
    }

    #[test]
    fn a_file_s_size_and_last_write_follow_it() {
        let (mut index, c) = volume();
        let grown = change(50, false, &[(41, "notes.txt")], 900, 9_000);
        index.settle(c, &grown).unwrap();
        let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
        assert_eq!(index.stat(notes), Some((900, 9_000)));
    }

    #[test]
    fn a_file_that_became_a_folder_is_one_now() {
        let (mut index, c) = volume();
        let now = folder(50, 41, "notes.txt");
        assert_eq!(index.settle(c, &now), Ok(true));
        let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
        assert_eq!(index.is_folder(notes), Some(true));
        index.settle(c, &file(51, 50, "inside.txt")).unwrap();
        assert!(found(&index, r"C:\Users\Me\notes.txt\inside.txt"));
    }

    #[test]
    fn a_name_another_file_had_goes_to_the_one_that_has_it_now() {
        let (mut index, c) = volume();
        index.settle(c, &file(92, 40, "x.txt")).unwrap();
        // 93 took the name before 92's going was read.
        let taken = change(93, false, &[(40, "X.TXT")], 11, 5_000);
        index.settle(c, &taken).unwrap();
        let x = index.find(r"C:\Users\x.txt").expect("there");
        assert_eq!(index.stat(x), Some((11, 5_000)));
        assert_eq!(index.name(x), Some("X.TXT"));
        index.settle(c, &gone(92)).unwrap();
        assert_eq!(
            index.find(r"C:\Users\x.txt"),
            Some(x),
            "92's going takes not 93's"
        );
    }

    #[test]
    fn two_volumes_keep_their_records_apart() {
        let (mut index, c) = volume();
        let d = index.add_root("D:").unwrap();
        index.settle(d, &folder(40, ROOT, "Data")).unwrap();
        index.settle(d, &file(50, 40, "notes.txt")).unwrap();
        index.settle(d, &gone(41)).unwrap();
        assert!(found(&index, r"C:\Users\Me\notes.txt"));
        index.settle(c, &gone(50)).unwrap();
        assert!(found(&index, r"D:\Data\notes.txt"));
        assert!(!found(&index, r"C:\Users\Me\notes.txt"));
    }

    #[test]
    fn an_entry_removed_by_its_path_is_not_found_by_its_record() {
        let (mut index, c) = volume();
        let me = index.find(r"C:\Users\Me").unwrap();
        index.remove(me).unwrap();
        // Its place goes to an entry a walk added, with no record.
        let users = index.find(r"C:\Users").unwrap();
        let walked = index.add(users, "walked.txt", false).unwrap();
        index.settle(c, &gone(41)).unwrap();
        index.settle(c, &gone(50)).unwrap();
        assert_eq!(index.find(r"C:\Users\walked.txt"), Some(walked));
        index.settle(c, &file(50, 41, "notes.txt")).unwrap();
        assert!(!found(&index, r"C:\Users\Me\notes.txt"), "its folder went");
    }

    #[test]
    fn only_a_root_takes_changes() {
        let (mut index, _) = volume();
        let users = index.find(r"C:\Users").unwrap();
        assert_eq!(
            index.settle(users, &file(95, ROOT, "a")),
            Err(Error::NoSuchEntry)
        );
        assert_eq!(
            index.settle(EntryId(999), &file(95, ROOT, "a")),
            Err(Error::NoSuchEntry)
        );
    }

    #[test]
    fn a_name_the_index_refuses_is_left_out() {
        let (mut index, c) = volume();
        let bad = change(96, false, &[(40, ""), (40, r"a\b"), (40, "good.txt")], 1, 1);
        index.settle(c, &bad).unwrap();
        assert!(found(&index, r"C:\Users\good.txt"));
        assert_eq!(index.len(), 6);
    }

    fn position(next: i64) -> Position {
        Position {
            volume: 0xABCD,
            journal: 7,
            next,
        }
    }

    #[test]
    fn a_volume_keeps_how_far_its_journal_was_taken_in() {
        let (mut index, c) = volume();
        assert_eq!(index.position(c), None);
        index.set_position(c, position(4_096)).unwrap();
        index.set_position(c, position(8_192)).unwrap();
        assert_eq!(index.position(c), Some(position(8_192)));
        let d = index.add_root("D:").unwrap();
        assert_eq!(index.position(d), None);
        let users = index.find(r"C:\Users").unwrap();
        assert_eq!(
            index.set_position(users, position(1)),
            Err(Error::NoSuchEntry)
        );
        index.remove(c).unwrap();
        let again = index.add_root("C:").unwrap();
        assert_eq!(
            index.position(again),
            None,
            "a root made again starts afresh"
        );
    }

    /// `volume()`'s, listed: the same entries, known by their records.
    fn listed() -> (Index, EntryId) {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        for change in [
            folder(40, ROOT, "Users"),
            folder(41, 40, "Me"),
            folder(60, ROOT, "Windows"),
            file(50, 41, "notes.txt"),
            change(70, false, &[(40, "a.txt"), (60, "b.txt")], 7, 3_000),
            file(80, 999, "nowhere.txt"),
            gone(81),
            change(82, false, &[(40, ""), (40, "good.txt")], 1, 1),
        ] {
            index.list(c, &change).unwrap();
        }
        (index, c)
    }

    #[test]
    fn a_listing_adds_each_name_in_a_folder_the_index_holds() {
        let (index, _) = listed();
        let notes = index.find(r"C:\Users\Me\notes.txt").expect("listed");
        assert_eq!(index.stat(notes), Some((5, 2_000)));
        assert_eq!(
            index.is_folder(index.find(r"C:\Users\Me").expect("listed")),
            Some(true)
        );
        assert!(found(&index, r"C:\Users\a.txt") && found(&index, r"C:\Windows\b.txt"));
        assert!(
            found(&index, r"C:\Users\good.txt"),
            "the name refused left out"
        );
        assert_eq!(
            index.len(),
            8,
            "the root, three folders, notes, two links, good"
        );
    }

    #[test]
    fn what_was_listed_is_known_by_its_records_after() {
        let (mut index, c) = listed();
        index.settle(c, &folder(41, 60, "You")).unwrap();
        assert!(found(&index, r"C:\Windows\You\notes.txt"));
        index.settle(c, &gone(70)).unwrap();
        assert!(!found(&index, r"C:\Users\a.txt") && !found(&index, r"C:\Windows\b.txt"));
        assert_eq!(index.list(EntryId(999), &gone(1)), Err(Error::NoSuchEntry));
        assert_eq!(index.list(c, &file(1 << 30, ROOT, "far")), Ok(()));
        assert!(!found(&index, r"C:\far"), "no record a volume could have");
    }

    #[test]
    fn a_volume_put_in_place_of_another_comes_with_its_records_and_position() {
        let mut walked = Index::default();
        let c = walked.add_root("C:").unwrap();
        walked.add(c, "walked.txt", false).unwrap();
        walked.add_root("D:").unwrap();
        let (mut fresh, fresh_c) = volume();
        fresh.set_position(fresh_c, position(77)).unwrap();
        walked.replace(&fresh);
        let c = walked.find("C:").unwrap();
        assert!(!found(&walked, r"C:\walked.txt"));
        assert_eq!(walked.position(c), Some(position(77)));
        walked.settle(c, &file(50, 60, "moved.txt")).unwrap();
        assert!(found(&walked, r"C:\Windows\moved.txt"));
        assert!(!found(&walked, r"C:\Users\Me\notes.txt"));
        let both = change(97, false, &[(40, "a"), (60, "b")], 1, 1);
        fresh.settle(fresh_c, &both).unwrap();
        walked.replace(&fresh);
        let c = walked.find("C:").unwrap();
        walked.settle(c, &gone(97)).unwrap();
        assert!(!found(&walked, r"C:\Users\a"), "every link's entry known");
        assert!(!found(&walked, r"C:\Windows\b"));
    }
}
