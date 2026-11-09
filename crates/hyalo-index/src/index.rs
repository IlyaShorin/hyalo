//! The entries and their names, and the changes a walk or a journal makes
//! to them.

use hyalo_core::item::Item;
use hyalo_core::matcher;

/// One file or folder, as long as it is in the index. A removed entry's id
/// may be given to an entry added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub(crate) u32);

/// Why a change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The entry was removed, or never was.
    NoSuchEntry,
    /// Only a folder can hold entries.
    NotAFolder,
    /// A name is empty, too long, or holds a backslash.
    BadName,
    /// A folder cannot move into itself or anything under it.
    WouldLoop,
}

/// Bits of `Entry::lengths` given to each length.
const LENGTH_BITS: u32 = 12;
const LENGTH_MASK: u32 = (1 << LENGTH_BITS) - 1;

/// The longest name, in UTF-8 bytes, as given or folded. NTFS names are at
/// most 255 UTF-16 units: 765 bytes, a little more folded.
pub const MAX_NAME: usize = LENGTH_MASK as usize;

/// `Entry::parent` of a root.
const NO_PARENT: u32 = u32::MAX;

const FOLDER: u32 = 1;
const REMOVED: u32 = 2;

/// One file or folder: the folder it is in and where its names are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The folder's entry, or `NO_PARENT` for a root.
    pub(crate) parent: u32,
    /// Where the name starts in `Index::names`.
    name: u32,
    /// Where the folded name starts in `Index::folded`.
    folded: u32,
    /// The name's length in bytes, the folded name's above it, then the
    /// flags in the top byte.
    lengths: u32,
}

impl Entry {
    fn name_len(self) -> usize {
        (self.lengths & LENGTH_MASK) as usize
    }

    fn folded_len(self) -> usize {
        (self.lengths >> LENGTH_BITS & LENGTH_MASK) as usize
    }

    fn flags(self) -> u32 {
        self.lengths >> (2 * LENGTH_BITS)
    }

    pub(crate) fn is_folder(self) -> bool {
        self.flags() & FOLDER != 0
    }

    pub(crate) fn is_live(self) -> bool {
        self.flags() & REMOVED == 0
    }

    pub(crate) fn is_root(self) -> bool {
        self.parent == NO_PARENT
    }
}

/// Every file and folder fed in so far.
#[derive(Debug, Default)]
pub struct Index {
    pub(crate) entries: Vec<Entry>,
    /// Every name, one after another, as given.
    names: String,
    /// Every name folded, in the same order.
    folded: String,
    /// Removed entries, given to the next ones added.
    free: Vec<u32>,
}

impl Index {
    /// Adds a drive, or any folder searched from the top, such as `C:`.
    /// Roots hold entries but are not found by a search.
    pub fn add_root(&mut self, name: &str) -> Result<EntryId, Error> {
        let folded = checked(name, true)?;
        Ok(self.place(NO_PARENT, name, &folded, FOLDER))
    }

    /// Adds a file or folder named `name` to the folder `parent`.
    pub fn add(&mut self, parent: EntryId, name: &str, folder: bool) -> Result<EntryId, Error> {
        self.folder(parent)?;
        let folded = checked(name, false)?;
        let flags = match folder {
            true => FOLDER,
            false => 0,
        };
        Ok(self.place(parent.0, name, &folded, flags))
    }

    /// Removes an entry and, if it is a folder, everything under it. A
    /// folder costs one pass over the whole index.
    pub fn remove(&mut self, id: EntryId) -> Result<(), Error> {
        let entry = self.live(id)?;
        self.kill(id.0);
        if entry.is_folder() {
            for orphan in self.orphans() {
                self.kill(orphan);
            }
        }
        Ok(())
    }

    /// Moves an entry into the folder `parent` under the name `name`; what
    /// is under it moves along.
    pub fn rename(&mut self, id: EntryId, parent: EntryId, name: &str) -> Result<(), Error> {
        let entry = self.live(id)?;
        self.folder(parent)?;
        if self.ancestry(parent.0).any(|at| at == id.0) {
            return Err(Error::WouldLoop);
        }
        let folded = checked(name, false)?;
        self.entries[id.0 as usize] = self.store(parent.0, name, &folded, entry.flags());
        Ok(())
    }

    /// How many files, folders and roots it holds.
    pub fn len(&self) -> usize {
        self.entries.len() - self.free.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn name(&self, id: EntryId) -> Option<&str> {
        let entry = self.live(id).ok()?;
        Some(self.name_of(entry))
    }

    pub fn is_folder(&self, id: EntryId) -> Option<bool> {
        Some(self.live(id).ok()?.is_folder())
    }

    /// The full path: the root's name, then every folder's, joined by
    /// backslashes.
    pub fn path(&self, id: EntryId) -> Option<String> {
        self.live(id).ok()?;
        let names = self.ancestry(id.0).map(|at| self.name_at(at));
        let mut names: Vec<&str> = names.collect();
        names.reverse();
        Some(names.join("\\"))
    }

    /// The row a search lists for the entry.
    pub fn item(&self, id: EntryId) -> Option<Item> {
        Some(Item::file(self.path(id)?, self.is_folder(id)?))
    }

    /// The bytes it holds on the heap.
    pub fn memory(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
            + self.names.capacity()
            + self.folded.capacity()
            + self.free.capacity() * size_of::<u32>()
    }

    /// The entry `id`, if it is in the index.
    fn live(&self, id: EntryId) -> Result<Entry, Error> {
        let entry = self.entries.get(id.0 as usize).copied();
        entry
            .filter(|entry| entry.is_live())
            .ok_or(Error::NoSuchEntry)
    }

    /// The entry `id`, if it is a folder in the index.
    fn folder(&self, id: EntryId) -> Result<Entry, Error> {
        let entry = self.live(id)?;
        match entry.is_folder() {
            true => Ok(entry),
            false => Err(Error::NotAFolder),
        }
    }

    pub(crate) fn name_of(&self, entry: Entry) -> &str {
        let start = entry.name as usize;
        &self.names[start..start + entry.name_len()]
    }

    pub(crate) fn folded_of(&self, entry: Entry) -> &str {
        let start = entry.folded as usize;
        &self.folded[start..start + entry.folded_len()]
    }

    fn name_at(&self, at: u32) -> &str {
        self.name_of(self.entries[at as usize])
    }

    /// `at`, then the folder it is in, and so on up to its root.
    pub(crate) fn ancestry(&self, at: u32) -> impl Iterator<Item = u32> + '_ {
        std::iter::successors(Some(at), |&at| {
            let entry = self.entries[at as usize];
            (!entry.is_root()).then_some(entry.parent)
        })
    }

    /// Stores a new entry, in the place of a removed one if there is one.
    fn place(&mut self, parent: u32, name: &str, folded: &str, flags: u32) -> EntryId {
        let entry = self.store(parent, name, folded, flags);
        match self.free.pop() {
            Some(at) => {
                self.entries[at as usize] = entry;
                EntryId(at)
            }
            None => {
                self.entries.push(entry);
                EntryId(offset(self.entries.len() - 1))
            }
        }
    }

    /// Appends both names to the arenas, and gives the entry for them.
    fn store(&mut self, parent: u32, name: &str, folded: &str, flags: u32) -> Entry {
        let lengths = name.len() as u32 | (folded.len() as u32) << LENGTH_BITS;
        let entry = Entry {
            parent,
            name: offset(self.names.len()),
            folded: offset(self.folded.len()),
            lengths: lengths | flags << (2 * LENGTH_BITS),
        };
        self.names.push_str(name);
        self.folded.push_str(folded);
        entry
    }

    fn kill(&mut self, at: u32) {
        self.entries[at as usize].lengths |= REMOVED << (2 * LENGTH_BITS);
        self.free.push(at);
    }

    /// The live entries with a removed folder above them.
    fn orphans(&self) -> Vec<u32> {
        let mut known = vec![None; self.entries.len()];
        let mut chain = Vec::new();
        let all = 0..offset(self.entries.len());
        let live = all.filter(|&at| self.entries[at as usize].is_live());
        live.filter(|&at| self.under_removed(at, &mut known, &mut chain))
            .collect()
    }

    /// Whether a removed entry is above `at`, remembering the answer for
    /// every folder on the way up. `chain` is scratch space.
    fn under_removed(&self, at: u32, known: &mut [Option<bool>], chain: &mut Vec<u32>) -> bool {
        chain.clear();
        let mut verdict = false;
        for step in self.ancestry(at) {
            if let Some(answer) = known[step as usize] {
                verdict = answer;
                break;
            }
            if !self.entries[step as usize].is_live() {
                verdict = true;
                break;
            }
            chain.push(step);
        }
        for &step in chain.iter() {
            known[step as usize] = Some(verdict);
        }
        verdict
    }
}

/// A position or count as the u32 entries keep. The index holds far fewer
/// than four billion entries and bytes of names.
fn offset(at: usize) -> u32 {
    u32::try_from(at).expect("the index stays under 4 GB of names and 4 billion entries")
}

/// The name folded, if it can be a name: not empty, not too long either
/// way, and one name, without a backslash, unless it is a root.
fn checked(name: &str, root: bool) -> Result<String, Error> {
    let folded = matcher::folded(name);
    let fits = |text: &str| !text.is_empty() && text.len() <= MAX_NAME;
    let one = root || !name.contains('\\');
    match fits(name) && fits(&folded) && one {
        true => Ok(folded),
        false => Err(Error::BadName),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::item::Kind;

    /// C: with Users\Me\notes.txt and Windows\System32.
    fn drive() -> (Index, [EntryId; 6]) {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let users = index.add(c, "Users", true).unwrap();
        let me = index.add(users, "Me", true).unwrap();
        let notes = index.add(me, "notes.txt", false).unwrap();
        let windows = index.add(c, "Windows", true).unwrap();
        let system = index.add(windows, "System32", true).unwrap();
        (index, [c, users, me, notes, windows, system])
    }

    #[test]
    fn a_new_index_is_empty() {
        let index = Index::default();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn an_entry_knows_its_name_kind_and_full_path() {
        let (index, [c, _, me, notes, ..]) = drive();
        assert_eq!(index.len(), 6);
        assert!(!index.is_empty());
        assert_eq!(index.name(notes), Some("notes.txt"));
        assert_eq!(index.is_folder(notes), Some(false));
        assert_eq!(index.is_folder(me), Some(true));
        assert_eq!(index.path(notes).as_deref(), Some(r"C:\Users\Me\notes.txt"));
        assert_eq!(index.path(c).as_deref(), Some("C:"));
    }

    #[test]
    fn an_entry_is_listed_as_a_file_or_folder_row() {
        let (index, [_, _, me, notes, ..]) = drive();
        let row = index.item(notes).unwrap();
        assert_eq!(row, Item::file(r"C:\Users\Me\notes.txt".to_string(), false));
        assert_eq!(index.item(me).unwrap().kind, Kind::Folder);
    }

    #[test]
    fn names_keep_their_case_and_letters() {
        let mut index = Index::default();
        let d = index.add_root("D:").unwrap();
        let tree = index.add(d, "Ёлка ДЛЯ Всех.TXT", false).unwrap();
        assert_eq!(index.name(tree), Some("Ёлка ДЛЯ Всех.TXT"));
    }

    #[test]
    fn only_a_live_folder_holds_entries() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        assert_eq!(index.add(notes, "x", false), Err(Error::NotAFolder));
        index.remove(me).unwrap();
        assert_eq!(index.add(me, "x", false), Err(Error::NoSuchEntry));
        assert_eq!(index.add(EntryId(99), "x", false), Err(Error::NoSuchEntry));
    }

    #[test]
    fn a_name_must_be_one_name_of_a_sane_length() {
        let (mut index, [c, ..]) = drive();
        assert_eq!(index.add(c, "", false), Err(Error::BadName));
        assert_eq!(index.add(c, r"a\b", false), Err(Error::BadName));
        let long = "я".repeat(MAX_NAME / 2 + 1);
        assert_eq!(index.add(c, &long, false), Err(Error::BadName));
        let longest = "a".repeat(MAX_NAME);
        assert!(index.add(c, &longest, false).is_ok());
        assert_eq!(index.add_root(""), Err(Error::BadName));
    }

    #[test]
    fn a_name_whose_folded_copy_grows_too_long_is_refused() {
        let (mut index, [c, ..]) = drive();
        // Ⱥ takes two bytes, its small letter three.
        let growing = "Ⱥ".repeat(MAX_NAME / 2);
        assert_eq!(index.add(c, &growing, false), Err(Error::BadName));
    }

    #[test]
    fn removing_a_file_removes_it_alone() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        index.remove(notes).unwrap();
        assert_eq!(index.len(), 5);
        assert_eq!(index.path(notes), None);
        assert_eq!(index.name(notes), None);
        assert_eq!(index.is_folder(notes), None);
        assert_eq!(index.item(notes), None);
        assert!(index.path(me).is_some());
        assert_eq!(index.remove(notes), Err(Error::NoSuchEntry), "twice");
    }

    #[test]
    fn removing_a_folder_removes_everything_under_it() {
        let (mut index, [c, users, me, notes, windows, system]) = drive();
        index.remove(users).unwrap();
        for gone in [users, me, notes] {
            assert_eq!(index.path(gone), None);
        }
        for kept in [c, windows, system] {
            assert!(index.path(kept).is_some());
        }
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn removing_a_folder_finds_what_was_added_under_it_in_a_reused_place() {
        let (mut index, [c, users, me, notes, ..]) = drive();
        index.remove(notes).unwrap();
        let later = index.add(c, "Later", true).unwrap();
        index.rename(me, later, "Me").unwrap();
        let deep = index.add(me, "deep.txt", false).unwrap();
        index.remove(later).unwrap();
        assert_eq!(index.path(deep), None);
        assert_eq!(index.path(me), None);
        assert!(index.path(users).is_some());
    }

    #[test]
    fn a_removed_place_is_given_to_the_next_entry() {
        let (mut index, [c, _, _, notes, ..]) = drive();
        let memory = index.memory();
        index.remove(notes).unwrap();
        let next = index.add(c, "next.txt", false).unwrap();
        assert_eq!(next, notes);
        assert_eq!(index.path(next).as_deref(), Some(r"C:\next.txt"));
        assert_eq!(index.len(), 6);
        assert!(index.memory() > memory, "the old name stays in the arena");
    }

    #[test]
    fn renaming_changes_the_name_in_place() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        index.rename(notes, me, "todo.md").unwrap();
        assert_eq!(index.path(notes).as_deref(), Some(r"C:\Users\Me\todo.md"));
    }

    #[test]
    fn moving_a_folder_takes_what_is_under_it_along() {
        let (mut index, [_, _, me, notes, windows, _]) = drive();
        index.rename(me, windows, "You").unwrap();
        assert_eq!(
            index.path(notes).as_deref(),
            Some(r"C:\Windows\You\notes.txt")
        );
    }

    #[test]
    fn a_folder_cannot_move_into_itself_or_below() {
        let (mut index, [_, users, me, ..]) = drive();
        assert_eq!(index.rename(users, users, "x"), Err(Error::WouldLoop));
        assert_eq!(index.rename(users, me, "x"), Err(Error::WouldLoop));
        assert_eq!(index.path(users).as_deref(), Some(r"C:\Users"));
    }

    #[test]
    fn a_rename_checks_the_entry_the_folder_and_the_name() {
        let (mut index, [c, _, me, notes, ..]) = drive();
        assert_eq!(index.rename(notes, notes, "x"), Err(Error::NotAFolder));
        assert_eq!(index.rename(notes, c, ""), Err(Error::BadName));
        assert_eq!(index.rename(EntryId(99), c, "x"), Err(Error::NoSuchEntry));
        index.remove(me).unwrap();
        assert_eq!(index.rename(me, c, "x"), Err(Error::NoSuchEntry));
        let (mut index, [c, _, me, ..]) = drive();
        index.remove(me).unwrap();
        let file = index.add(c, "file", false).unwrap();
        assert_eq!(index.rename(file, me, "x"), Err(Error::NoSuchEntry));
    }

    #[test]
    fn an_entry_is_sixteen_bytes() {
        assert_eq!(size_of::<Entry>(), 16);
    }

    #[test]
    fn memory_counts_the_entries_and_both_copies_of_the_names() {
        let mut index = Index::default();
        let empty = index.memory();
        let d = index.add_root("D:").unwrap();
        index.add(d, "abc", false).unwrap();
        // Two entries, and "D:" and "abc" twice each, at least.
        assert!(index.memory() >= empty + 2 * 16 + 2 * 5);
    }
}
