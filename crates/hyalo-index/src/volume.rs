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
mod tests;
