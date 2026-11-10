//! The entries and their names, and the changes a walk or a journal makes
//! to them.

use std::borrow::Cow;

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

    /// The same entry, its names at `name` and `folded` in the arenas,
    /// with the lengths given.
    fn named(self, name: u32, name_len: usize, folded: u32, folded_len: usize) -> Entry {
        let lengths = name_len as u32 | (folded_len as u32) << LENGTH_BITS;
        Entry {
            name,
            folded,
            lengths: lengths | self.flags() << (2 * LENGTH_BITS),
            ..self
        }
    }

    /// The entry as a snapshot keeps it: its four fields, little-endian.
    pub(crate) fn to_bytes(self) -> [u8; 16] {
        let fields = [self.parent, self.name, self.folded, self.lengths].map(u32::to_le_bytes);
        std::array::from_fn(|at| fields[at / 4][at % 4])
    }

    pub(crate) fn from_bytes(bytes: [u8; 16]) -> Entry {
        let field = |at: usize| u32::from_le_bytes(bytes.as_chunks::<4>().0[at]);
        Entry {
            parent: field(0),
            name: field(1),
            folded: field(2),
            lengths: field(3),
        }
    }
}

/// An index taken apart, as a snapshot holds it. The folded names are
/// left out: they are folded again from the names.
pub(crate) struct Parts {
    pub(crate) entries: Vec<Entry>,
    pub(crate) names: String,
    pub(crate) free: Vec<u32>,
}

/// The end of a list of entries, or a folder with none.
const NONE: u32 = u32::MAX;

/// Where an entry sits in the list of its folder's entries, and where
/// its own list starts if it is a folder: so a folder's entries are found
/// without a pass over the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Links {
    /// The first entry in it.
    first: u32,
    /// The entries before and after it in its folder.
    previous: u32,
    next: u32,
}

const UNLINKED: Links = Links {
    first: NONE,
    previous: NONE,
    next: NONE,
};

/// Every file and folder fed in so far.
#[derive(Debug)]
pub struct Index {
    pub(crate) entries: Vec<Entry>,
    /// Beside each entry, its place among its folder's entries.
    links: Vec<Links>,
    /// The first root; the roots are listed as a folder's entries are.
    top: u32,
    /// Every name, one after another, as given.
    names: String,
    /// Every name folded, in the same order.
    folded: String,
    /// Removed entries, given to the next ones added.
    free: Vec<u32>,
    /// Bytes of `names` that no entry names any more.
    garbage: usize,
    /// Counts the changes made.
    version: u64,
}

impl Default for Index {
    fn default() -> Self {
        Index {
            entries: Vec::new(),
            links: Vec::new(),
            top: NONE,
            names: String::new(),
            folded: String::new(),
            free: Vec::new(),
            garbage: 0,
            version: 0,
        }
    }
}

impl Index {
    /// Adds a drive, or any folder searched from the top, such as `C:`.
    /// Roots hold entries but are not found by a search.
    pub fn add_root(&mut self, name: &str) -> Result<EntryId, Error> {
        let folded = checked(name, true)?;
        Ok(self.place(NO_PARENT, name, &folded, true))
    }

    /// Adds a file or folder named `name` to the folder `parent`.
    pub fn add(&mut self, parent: EntryId, name: &str, folder: bool) -> Result<EntryId, Error> {
        self.folder(parent)?;
        let folded = checked(name, false)?;
        Ok(self.place(parent.0, name, &folded, folder))
    }

    /// The file or folder named `name` in the folder `parent`, added if
    /// it is not there: one of the other kind there is replaced. Unlike
    /// `add`, which a walk of a folder it has not seen uses, it looks
    /// among the folder's entries first, so it may run twice.
    pub fn put(&mut self, parent: EntryId, name: &str, folder: bool) -> Result<EntryId, Error> {
        self.folder(parent)?;
        match self.child(parent.0, name) {
            Some(at) if self.entries[at as usize].is_folder() == folder => Ok(EntryId(at)),
            Some(at) => {
                self.remove(EntryId(at))?;
                self.add(parent, name, folder)
            }
            None => self.add(parent, name, folder),
        }
    }

    /// Removes an entry and, if it is a folder, everything under it.
    pub fn remove(&mut self, id: EntryId) -> Result<(), Error> {
        self.live(id)?;
        self.version += 1;
        self.unlink(id.0);
        let mut doomed = vec![id.0];
        while let Some(at) = doomed.pop() {
            doomed.extend(self.children(at));
            self.kill(at);
        }
        self.reclaim();
        Ok(())
    }

    /// Moves an entry into the folder `parent` under the name `name`; what
    /// is under it moves along. Another entry by that name there is
    /// replaced, as a move that replaces a file does.
    pub fn rename(&mut self, id: EntryId, parent: EntryId, name: &str) -> Result<(), Error> {
        let entry = self.live(id)?;
        self.folder(parent)?;
        if self.ancestry(parent.0).any(|at| at == id.0) {
            return Err(Error::WouldLoop);
        }
        let folded = checked(name, false)?;
        if let Some(there) = self.child(parent.0, name).filter(|&at| at != id.0) {
            self.remove(EntryId(there))?;
        }
        self.version += 1;
        self.unlink(id.0);
        self.garbage += entry.name_len();
        self.entries[id.0 as usize] = self.store(parent.0, name, &folded, entry.is_folder());
        self.link(id.0);
        self.reclaim();
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

    /// The folder the entry is in; none for a root, or an entry removed.
    pub fn parent(&self, id: EntryId) -> Option<EntryId> {
        let entry = self.live(id).ok()?;
        (!entry.is_root()).then_some(EntryId(entry.parent))
    }

    /// A number that changes with every change made, so a reader can tell
    /// whether there was one.
    pub fn version(&self) -> u64 {
        self.version
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

    /// The entry at the full path `path`, such as `C:\Users\me`, names
    /// compared regardless of case, as Windows compares them. Each name
    /// costs a look through one folder's entries.
    pub fn find(&self, path: &str) -> Option<EntryId> {
        let names: Vec<&str> = path.split('\\').filter(|name| !name.is_empty()).collect();
        let found = self.children(NO_PARENT).find_map(|root| {
            let top: Vec<&str> = self.name_at(root).split('\\').collect();
            let rest = names.get(top.len()..)?;
            let ours = top.iter().zip(&names).all(|(one, other)| same(one, other));
            ours.then_some(())?;
            rest.iter().try_fold(root, |at, name| self.child(at, name))
        });
        found.map(EntryId)
    }

    /// The entries in the folder `parent`, or the roots for `NO_PARENT`.
    fn children(&self, parent: u32) -> impl Iterator<Item = u32> + '_ {
        let first = Some(self.first(parent)).filter(|&at| at != NONE);
        std::iter::successors(first, |&at| {
            let next = self.links[at as usize].next;
            (next != NONE).then_some(next)
        })
    }

    /// The entry named `name`, in any case, in the folder `parent`.
    fn child(&self, parent: u32, name: &str) -> Option<u32> {
        self.children(parent)
            .find(|&at| same(self.name_at(at), name))
    }

    /// Where the list of the folder `parent`'s entries starts.
    fn first(&self, parent: u32) -> u32 {
        match parent {
            NO_PARENT => self.top,
            _ => self.links[parent as usize].first,
        }
    }

    fn first_mut(&mut self, parent: u32) -> &mut u32 {
        match parent {
            NO_PARENT => &mut self.top,
            _ => &mut self.links[parent as usize].first,
        }
    }

    /// Puts the entry at `at` first in its folder's list.
    fn link(&mut self, at: u32) {
        let first = *self.first_mut(self.entries[at as usize].parent);
        let links = &mut self.links[at as usize];
        (links.previous, links.next) = (NONE, first);
        if first != NONE {
            self.links[first as usize].previous = at;
        }
        *self.first_mut(self.entries[at as usize].parent) = at;
    }

    /// Takes the entry at `at` out of its folder's list.
    fn unlink(&mut self, at: u32) {
        let Links { previous, next, .. } = self.links[at as usize];
        match previous {
            NONE => *self.first_mut(self.entries[at as usize].parent) = next,
            _ => self.links[previous as usize].next = next,
        }
        if next != NONE {
            self.links[next as usize].previous = previous;
        }
    }

    /// The row a search lists for the entry.
    pub fn item(&self, id: EntryId) -> Option<Item> {
        Some(Item::file(self.path(id)?, self.is_folder(id)?))
    }

    /// Gives back the room kept for growth: after a walk, the arenas can
    /// hold up to twice what they use.
    pub fn shrink_to_fit(&mut self) {
        self.entries.shrink_to_fit();
        self.links.shrink_to_fit();
        self.names.shrink_to_fit();
        self.folded.shrink_to_fit();
        self.free.shrink_to_fit();
    }

    /// The bytes it holds on the heap.
    pub fn memory(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
            + self.links.capacity() * size_of::<Links>()
            + self.names.capacity()
            + self.folded.capacity()
            + self.free.capacity() * size_of::<u32>()
    }

    /// The index's parts, for a snapshot: entries, names, folded names
    /// and free places.
    pub(crate) fn parts(&self) -> (&[Entry], &str, &[u32]) {
        (&self.entries, &self.names, &self.free)
    }

    /// The index made of `parts`, if they hold together: known flags, every
    /// live entry named within the arena and in a live folder, every folder
    /// reaching a root, the free places exactly the removed entries, and
    /// every name one `add` would take. The names are folded again here,
    /// so the folded copy always is the fold of the name.
    pub(crate) fn from_parts(parts: Parts) -> Option<Index> {
        let mut index = Index {
            links: vec![UNLINKED; parts.entries.len()],
            entries: parts.entries,
            names: parts.names,
            free: parts.free,
            ..Index::default()
        };
        let sound = index.entries.iter().all(|&entry| index.sound(entry));
        let whole = sound && index.rooted() && index.free_is_removed();
        if !(whole && index.rebuild(Fold::Again)) {
            return None;
        }
        for at in (0..index.entries.len()).rev() {
            if index.entries[at].is_live() {
                index.link(offset(at));
            }
        }
        Some(index)
    }

    fn sound(&self, entry: Entry) -> bool {
        if entry.flags() & !(FOLDER | REMOVED) != 0 {
            return false;
        }
        let start = entry.name as usize;
        let named = self.names.get(start..start + entry.name_len()).is_some();
        let in_folder = || {
            let parent = self.entries.get(entry.parent as usize);
            entry.is_root() || parent.is_some_and(|parent| parent.is_live() && parent.is_folder())
        };
        !entry.is_live() || (named && in_folder())
    }

    /// Copies every live name and its folded copy into fresh arenas, which
    /// leaves out the names given up; whether each was a name `add` would
    /// take. A removed entry is left naming nothing.
    fn rebuild(&mut self, fold: Fold) -> bool {
        let live = self.entries.iter().filter(|entry| entry.is_live());
        let room: usize = live.map(|entry| entry.name_len()).sum();
        let (mut names, mut folded) = (String::with_capacity(room), String::with_capacity(room));
        for at in 0..self.entries.len() {
            let entry = self.entries[at];
            let moved = match entry.is_live() {
                true => {
                    let name = self.name_of(entry);
                    let Some(copy) = fold.of(self, entry) else {
                        return false;
                    };
                    let moved = entry.named(
                        offset(names.len()),
                        name.len(),
                        offset(folded.len()),
                        copy.len(),
                    );
                    names.push_str(name);
                    folded.push_str(&copy);
                    moved
                }
                false => entry.named(0, 0, 0, 0),
            };
            self.entries[at] = moved;
        }
        folded.shrink_to_fit();
        (self.names, self.folded, self.garbage) = (names, folded, 0);
        true
    }

    /// Rebuilds the arenas once more than half of the names are given up,
    /// so a long run of changes takes no more room than a walk.
    fn reclaim(&mut self) {
        if self.garbage * 2 > self.names.len() {
            self.rebuild(Fold::Kept);
        }
    }

    /// Whether every live entry reaches a root: no folder is inside itself.
    fn rooted(&self) -> bool {
        // 0: not seen; 1: on the way up from the entry being checked; 2:
        // reaches a root.
        let mut state = vec![0_u8; self.entries.len()];
        let mut chain = Vec::new();
        (0..self.entries.len()).all(|start| self.reaches_root(start, &mut state, &mut chain))
    }

    fn reaches_root(&self, start: usize, state: &mut [u8], chain: &mut Vec<usize>) -> bool {
        chain.clear();
        let mut at = start;
        while state[at] == 0 && self.entries[at].is_live() {
            state[at] = 1;
            chain.push(at);
            if self.entries[at].is_root() {
                break;
            }
            at = self.entries[at].parent as usize;
        }
        let looped = state[at] == 1 && !self.entries[at].is_root();
        for &step in chain.iter() {
            state[step] = 2;
        }
        !looped
    }

    /// Whether the free places are the removed entries, each listed once.
    fn free_is_removed(&self) -> bool {
        let removed = self.entries.iter().filter(|entry| !entry.is_live()).count();
        let mut listed = vec![false; self.entries.len()];
        let mut fresh = |at: &u32| {
            let at = *at as usize;
            let entry = self.entries.get(at);
            let new = entry.is_some_and(|entry| !entry.is_live()) && !listed[at];
            if new {
                listed[at] = true;
            }
            new
        };
        removed == self.free.len() && self.free.iter().all(&mut fresh)
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
    fn place(&mut self, parent: u32, name: &str, folded: &str, folder: bool) -> EntryId {
        self.version += 1;
        let entry = self.store(parent, name, folded, folder);
        let at = match self.free.pop() {
            Some(at) => {
                self.entries[at as usize] = entry;
                self.links[at as usize] = UNLINKED;
                at
            }
            None => {
                self.entries.push(entry);
                self.links.push(UNLINKED);
                offset(self.entries.len() - 1)
            }
        };
        self.link(at);
        EntryId(at)
    }

    /// Appends both names to the arenas, and gives the entry for them.
    fn store(&mut self, parent: u32, name: &str, folded: &str, folder: bool) -> Entry {
        let flags = match folder {
            true => FOLDER,
            false => 0,
        };
        let entry = Entry {
            parent,
            name: 0,
            folded: 0,
            lengths: flags << (2 * LENGTH_BITS),
        };
        let entry = entry.named(
            offset(self.names.len()),
            name.len(),
            offset(self.folded.len()),
            folded.len(),
        );
        self.names.push_str(name);
        self.folded.push_str(folded);
        entry
    }

    /// Marks the entry at `at` removed, its place free and its name given
    /// up. It is no longer in any list.
    fn kill(&mut self, at: u32) {
        let entry = &mut self.entries[at as usize];
        entry.lengths |= REMOVED << (2 * LENGTH_BITS);
        self.garbage += entry.name_len();
        self.links[at as usize] = UNLINKED;
        self.free.push(at);
    }
}

/// Where `rebuild` takes a name's folded copy from.
#[derive(Clone, Copy)]
enum Fold {
    /// Folds the name again, refusing one `add` would refuse.
    Again,
    /// The copy the arena holds.
    Kept,
}

impl Fold {
    fn of(self, index: &Index, entry: Entry) -> Option<Cow<'_, str>> {
        match self {
            Fold::Again => checked(index.name_of(entry), entry.is_root())
                .ok()
                .map(Cow::Owned),
            Fold::Kept => Some(Cow::Borrowed(index.folded_of(entry))),
        }
    }
}

/// A position or count as the u32 entries keep. The index holds far fewer
/// than four billion entries and bytes of names.
fn offset(at: usize) -> u32 {
    u32::try_from(at).expect("the index stays under 4 GB of names and 4 billion entries")
}

/// Whether two names are the same to Windows, which compares them letter
/// by letter in any case: `Ё` as `ё`, never as `е`.
fn same(one: &str, other: &str) -> bool {
    let lower = |name: &str| {
        name.chars()
            .flat_map(char::to_lowercase)
            .collect::<Vec<_>>()
    };
    one == other || lower(one) == lower(other)
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
    fn finds_an_entry_by_its_full_path_in_any_case() {
        let (index, [c, _, me, notes, _, system]) = drive();
        assert_eq!(index.find("C:"), Some(c));
        assert_eq!(index.find(r"c:\"), Some(c));
        assert_eq!(index.find(r"C:\Users\Me"), Some(me));
        assert_eq!(index.find(r"C:\USERS\me\NOTES.TXT"), Some(notes));
        assert_eq!(index.find(r"C:\Windows\System32\"), Some(system));
    }

    #[test]
    fn finds_entries_under_a_root_named_by_a_whole_path() {
        let mut index = Index::default();
        let data = index.add_root(r"D:\Data").unwrap();
        let notes = index.add(data, "notes.txt", false).unwrap();
        index.add_root(r"D:\Data2").unwrap();
        assert_eq!(index.find(r"d:\data"), Some(data));
        assert_eq!(index.find(r"D:\Data\NOTES.txt"), Some(notes));
        assert_eq!(index.find(r"D:\DataX\notes.txt"), None);
        assert_eq!(index.find(r"D:\Data2\notes.txt"), None);
    }

    #[test]
    fn a_path_that_is_not_there_finds_nothing() {
        let (mut index, [_, users, ..]) = drive();
        let d = index.add_root("D:").unwrap();
        index.add(d, "Elsewhere", true).unwrap();
        for path in [
            r"C:\Users\You",
            r"C:\Elsewhere",
            r"E:\Users",
            r"Users\Me",
            "",
            r"C:\Users\Me\notes",
        ] {
            assert_eq!(index.find(path), None, "{path}");
        }
        index.remove(users).unwrap();
        assert_eq!(index.find(r"C:\Users"), None, "removed");
        assert_eq!(index.find(r"C:\Users\Me"), None, "removed with its folder");
    }

    #[test]
    fn compares_letters_beyond_ascii_regardless_of_case_but_not_ё_as_е() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let tree = index.add(c, "Ёлка", true).unwrap();
        index.add(c, "Елка", true).unwrap();
        assert_eq!(index.find(r"C:\ЁЛКА"), Some(tree));
        assert_eq!(index.find(r"c:\ёлка"), Some(tree));
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
    fn a_root_may_be_a_folder_with_backslashes_in_its_name() {
        let mut index = Index::default();
        let data = index.add_root(r"D:\Data").unwrap();
        let file = index.add(data, "x.txt", false).unwrap();
        assert_eq!(index.path(file).as_deref(), Some(r"D:\Data\x.txt"));
    }

    #[test]
    fn a_rename_takes_one_name() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        assert_eq!(index.rename(notes, me, r"a\b"), Err(Error::BadName));
    }

    #[test]
    fn removing_a_folder_finds_what_was_put_in_the_first_place_under_it() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let d = index.add_root("D:").unwrap();
        let folder = index.add(d, "folder", true).unwrap();
        index.remove(c).unwrap();
        let inside = index.add(folder, "inside.txt", false).unwrap();
        assert_eq!(inside, EntryId(0));
        index.remove(d).unwrap();
        assert_eq!(index.path(inside), None);
        assert!(index.is_empty());
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
    fn putting_a_name_already_there_keeps_the_one_entry() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        assert_eq!(index.put(me, "notes.txt", false), Ok(notes));
        assert_eq!(index.put(me, "NOTES.TXT", false), Ok(notes), "in any case");
        assert_eq!(index.len(), 6);
        assert_eq!(index.name(notes), Some("notes.txt"));
    }

    #[test]
    fn putting_a_new_name_adds_it() {
        let (mut index, [_, _, me, ..]) = drive();
        let todo = index.put(me, "todo.md", false).unwrap();
        assert_eq!(index.path(todo).as_deref(), Some(r"C:\Users\Me\todo.md"));
        assert_eq!(index.find(r"C:\Users\Me\todo.md"), Some(todo));
        assert_eq!(index.len(), 7);
        assert_eq!(index.put(todo, "x", false), Err(Error::NotAFolder));
    }

    #[test]
    fn putting_a_name_of_the_other_kind_replaces_it() {
        let (mut index, [_, users, me, ..]) = drive();
        let folder = index.put(me, "notes.txt", true).unwrap();
        assert_eq!(index.is_folder(folder), Some(true));
        assert_eq!(index.len(), 6);
        let file = index.put(users, "Me", false).unwrap();
        assert_eq!(index.is_folder(file), Some(false));
        let inside = index.find(r"C:\Users\Me\notes.txt");
        assert_eq!(inside, None, "gone with the folder");
        assert_eq!(index.len(), 5, "Me and notes.txt went, the file Me came");
    }

    #[test]
    fn a_moved_entry_is_found_at_its_new_path_only() {
        let (mut index, [_, _, me, notes, windows, _]) = drive();
        index.rename(notes, windows, "n.txt").unwrap();
        assert_eq!(index.find(r"C:\Users\Me\notes.txt"), None);
        assert_eq!(index.find(r"C:\Windows\n.txt"), Some(notes));
        index.rename(me, windows, "Me").unwrap();
        assert_eq!(index.find(r"C:\Users\Me"), None);
        assert_eq!(index.find(r"C:\Windows\Me"), Some(me));
    }

    #[test]
    fn a_rename_onto_a_name_there_takes_its_place() {
        let (mut index, [_, _, me, notes, windows, _]) = drive();
        let there = index.add(windows, "a.txt", false).unwrap();
        index.rename(notes, windows, "A.TXT").unwrap();
        assert_eq!(index.find(r"C:\Windows\a.txt"), Some(notes));
        assert_eq!(index.name(there), None, "replaced");
        assert_eq!(index.len(), 6);
        index.rename(notes, windows, "a.txt").unwrap();
        assert_eq!(index.name(notes), Some("a.txt"), "a change of case alone");
        assert_eq!(index.len(), 6);
        assert!(index.path(me).is_some());
    }

    #[test]
    fn names_given_up_are_reclaimed() {
        let (mut index, [_, _, me, notes, ..]) = drive();
        for n in 0..10_000 {
            let temporary = index.put(me, &format!("temporary file {n}.tmp"), false);
            index.remove(temporary.unwrap()).unwrap();
            index.rename(notes, me, &format!("notes {n}.txt")).unwrap();
        }
        assert!(index.memory() < 4096, "{} bytes", index.memory());
        assert_eq!(index.find(r"C:\Users\Me\notes 9999.txt"), Some(notes));
        assert_eq!(index.find(r"C:\Windows\System32").map(|_| ()), Some(()));
    }

    #[test]
    fn an_entry_knows_its_folder_and_a_root_has_none() {
        let (mut index, [c, users, me, notes, ..]) = drive();
        assert_eq!(index.parent(notes), Some(me));
        assert_eq!(index.parent(users), Some(c));
        assert_eq!(index.parent(c), None);
        index.remove(me).unwrap();
        assert_eq!(index.parent(notes), None, "removed");
    }

    #[test]
    fn the_version_moves_with_every_change_and_only_then() {
        let (mut index, [c, _, me, notes, windows, _]) = drive();
        let mut seen = vec![index.version()];
        let mut changed = |index: &Index| {
            let moved = !seen.contains(&index.version());
            seen.push(index.version());
            moved
        };
        index.put(me, "notes.txt", false).unwrap();
        assert!(!changed(&index), "put of what is there");
        index.put(me, "new.txt", false).unwrap();
        assert!(changed(&index), "put");
        index.add(c, "x", false).unwrap();
        assert!(changed(&index), "add");
        index.rename(notes, windows, "n.txt").unwrap();
        assert!(changed(&index), "rename");
        index.remove(notes).unwrap();
        assert!(changed(&index), "remove");
        assert_eq!(index.remove(notes), Err(Error::NoSuchEntry));
        assert!(!changed(&index), "a refused change");
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

    #[test]
    fn shrinking_gives_back_the_room_kept_for_growth() {
        let (mut index, [_, _, _, notes, ..]) = drive();
        index.remove(notes).unwrap();
        index.shrink_to_fit();
        // Six entries and their links; C:, Users, Me, notes.txt, Windows,
        // System32 twice; one free place.
        assert_eq!(index.memory(), 6 * (16 + 12) + 2 * 33 + 4);
        assert_eq!(
            index.path(EntryId(5)).as_deref(),
            Some(r"C:\Windows\System32")
        );
    }
}
