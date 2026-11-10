//! Files and folders: the index the window searches, the walk of the
//! fixed drives that fills it, and the snapshot that keeps it between
//! runs.
//!
//! Until the indexer service exists, this is how hyalo finds files: a
//! plain walk, folder by folder, with the user's own rights, then a watch
//! on every root that keeps the index in step with what changes. The
//! first walk writes straight into the index the window searches, so
//! files turn up while it runs; later walks build a fresh index and swap
//! it in whole.

use std::fs::{self, DirEntry, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::windows::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use hyalo_core::filter::Filters;
use hyalo_core::item::{Item, ItemId};
use hyalo_core::places::{self, Places};
use hyalo_core::rank::Ranked;
use hyalo_index::{EntryId, Index};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDriveStringsW};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, HSTRING};

use crate::catalog::{Found, Provider, Request};
use crate::store;

mod changes;
mod watch;

use changes::{Change, Kind, Record, Stat};
use watch::{Event, Watch};

/// The file index the window searches.
#[derive(Clone, Default)]
pub struct Files(Arc<RwLock<Index>>);

/// The one hand that changes the index: the files provider's. It is not
/// `Clone`, so there is never a second.
pub struct Writer(Arc<RwLock<Index>>);

impl Files {
    /// An empty index, and the one writer for it.
    pub fn new() -> (Files, Writer) {
        let shared = Arc::default();
        (Files(Arc::clone(&shared)), Writer(shared))
    }

    /// The `limit` best files and folders for `words`, among those
    /// `filters` lets through.
    pub fn search(
        &self,
        words: &str,
        filters: &Filters,
        places: &Places,
        limit: usize,
    ) -> Vec<Ranked> {
        read(&self.0)
            .search_with(words, filters, places, limit)
            .rows
    }

    /// Those of `items` that are files or folders `filters` lets through.
    pub fn let_through<'a>(
        &self,
        filters: &Filters,
        items: impl IntoIterator<Item = &'a Item>,
    ) -> Vec<&'a Item> {
        let index = read(&self.0);
        let sieve = index.sieve(filters);
        let passes = |item: &&Item| item.path().is_some_and(|path| sieve.lets_through(path));
        items.into_iter().filter(passes).collect()
    }
}

fn read(index: &RwLock<Index>) -> RwLockReadGuard<'_, Index> {
    index.read().unwrap_or_else(PoisonError::into_inner)
}

fn write(index: &RwLock<Index>) -> RwLockWriteGuard<'_, Index> {
    index.write().unwrap_or_else(PoisonError::into_inner)
}

/// What the walk leaves out, as the config last said: full paths, each
/// with everything under it. The window sets it; the walk reads it as it
/// goes, so a change reaches a walk under way too.
#[derive(Clone, Default)]
pub struct Excluded(Arc<RwLock<Vec<String>>>);

impl Excluded {
    /// Leaves out `paths` from now on; whether they differ from before.
    pub fn set(&self, paths: &[String]) -> bool {
        let mut held = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let changed = *held != paths;
        held.clone_from(&paths.to_vec());
        changed
    }

    fn paths(&self) -> Vec<String> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The names left out of the folder at `folder`, lowercased.
    fn within(&self, folder: &Path) -> Vec<String> {
        let paths = self.0.read().unwrap_or_else(PoisonError::into_inner);
        if paths.is_empty() {
            return Vec::new();
        }
        let folder = lowercase(folder.to_string_lossy().trim_end_matches('\\'));
        let split = paths.iter().filter_map(|path| path.rsplit_once('\\'));
        let here = split.filter(|(parent, _)| lowercase(parent) == folder);
        here.map(|(_, name)| lowercase(name)).collect()
    }
}

/// Windows' time, hundreds of nanoseconds since 1601 (UTC), in seconds
/// since 1970.
fn unix_seconds(filetime: i64) -> i64 {
    filetime.div_euclid(10_000_000) - 11_644_473_600
}

/// `text` lowercased, as Windows compares names.
fn lowercase(text: &str) -> String {
    text.to_lowercase()
}

/// How often showing the launcher makes sure the files provider runs: it
/// watches the drives for as long as the index is searched, so a later
/// run finds it at work and does nothing, unless it ended.
pub const REFRESH_AFTER: Duration = Duration::from_secs(20 * 60);

/// How often a first walk tells the window to search again.
const TICK: Duration = Duration::from_millis(250);

/// The window is told of changes at most this often: a burst of them
/// costs one search, not one each.
const TELL_EVERY: Duration = Duration::from_millis(500);

/// How long the provider waits for changes before it looks again whether
/// the index is still searched and what is excluded.
const IDLE: Duration = Duration::from_secs(1);

/// How often a changed index is saved: the snapshot only has to be good
/// enough to start from, since every start walks again, and saving
/// writes the whole index (86 MB for two million names). Something always
/// changes (browsers' caches, the snapshot itself), so this is how often
/// it is written while hyalo runs.
const SAVE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The folders a walk starts from: the fixed drives, or a stand-in.
pub type Roots = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// The files provider. The first run loads the snapshot, if there is
/// one; every run takes out what is `excluded`, watches the roots, walks
/// them leaving it out, saves the snapshot, then follows what the watches
/// see until nobody searches the index any more. A run that starts while
/// another is at work does nothing: that one keeps up with what is
/// excluded.
pub fn provider(
    writer: Writer,
    roots: Roots,
    snapshot: Option<PathBuf>,
    excluded: Excluded,
) -> Provider {
    let writer = Mutex::new(writer);
    Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
        let writer = match writer.try_lock() {
            Ok(writer) => writer,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let walk = Walk {
            roots: &*roots,
            snapshot: snapshot.as_deref(),
            excluded: &excluded,
        };
        walk.serve(&writer.0, report);
    })
}

/// What a run of the provider goes by.
struct Walk<'a> {
    roots: &'a dyn Fn() -> Vec<PathBuf>,
    snapshot: Option<&'a Path>,
    excluded: &'a Excluded,
}

impl Walk<'_> {
    /// Loads the snapshot into an empty index; takes out what is
    /// excluded, before the walk and again after it, for what was
    /// excluded while it went; watches the roots, from before the walk so
    /// nothing that changes during it is missed; walks them and saves the
    /// snapshot; then follows the watches.
    fn serve(&self, shared: &Arc<RwLock<Index>>, report: &dyn Fn(Found)) {
        if read(shared).is_empty() {
            load_into(shared, self.snapshot, report);
        }
        prune(shared, self.excluded, report);
        let roots = (self.roots)();
        let (sender, events) = mpsc::channel();
        // Each root's events say which root they are about.
        let tell = |sender: mpsc::Sender<(usize, Event)>, root: usize| {
            Box::new(move |event| sender.send((root, event)).is_ok())
        };
        let watches: Vec<Watch> = roots
            .iter()
            .enumerate()
            .map(|(at, root)| Watch::start(root, tell(sender.clone(), at)))
            .collect();
        self.walk(shared, &roots, report);
        self.follow(shared, &roots, &events, report);
        drop(watches);
    }

    /// Walks the roots, takes out what was excluded while it went, and
    /// saves the snapshot.
    fn walk(&self, shared: &RwLock<Index>, roots: &[PathBuf], report: &dyn Fn(Found)) {
        walk_into(shared, roots, self.excluded, report);
        prune(shared, self.excluded, report);
        save_to(shared, self.snapshot);
    }

    /// Keeps the index in step with what the watches see, until nobody
    /// but the provider holds it: changes as they come, a walk of a root
    /// whose watch lost track, what is excluded as it changes. Tells
    /// the window, and saves the snapshot, now and then when the index
    /// changed, and once more at the end.
    fn follow(
        &self,
        shared: &Arc<RwLock<Index>>,
        roots: &[PathBuf],
        events: &Receiver<(usize, Event)>,
        report: &dyn Fn(Found),
    ) {
        let version = read(shared).version();
        let mut kept = Kept {
            told: Mark::new(version),
            saved: Mark::new(version),
            excluded: self.excluded.paths(),
        };
        while Arc::strong_count(shared) > 1 {
            // Out of the match, which would hold the lock through it.
            let patience = kept.patience(read(shared).version());
            match events.recv_timeout(patience) {
                Ok((_, Event::Changed(records))) => self.take(shared, &records),
                Ok((root, Event::Lost)) => {
                    self.rewalk(shared, &roots[root]);
                    kept.saved = Mark::new(read(shared).version());
                }
                Err(_) => {}
            }
            self.keep_out(shared, &mut kept.excluded, report);
            let version = read(shared).version();
            if kept.told.due(version, Instant::now(), TELL_EVERY) {
                report(Found::Files);
            }
            if kept.saved.due(version, Instant::now(), SAVE_EVERY) {
                save_to(shared, self.snapshot);
            }
        }
        if kept
            .saved
            .due(read(shared).version(), Instant::now(), Duration::ZERO)
        {
            save_to(shared, self.snapshot);
        }
    }

    /// Walks `root` again into an index of its own and puts what it found
    /// in place of what the index held of it; takes out what is excluded,
    /// and saves the snapshot. The other roots stay as they are.
    fn rewalk(&self, shared: &RwLock<Index>, root: &Path) {
        let fresh = RwLock::default();
        walk(root, &fresh, self.excluded);
        let fresh = fresh.into_inner().unwrap_or_else(PoisonError::into_inner);
        write(shared).replace(&fresh);
        prune(shared, self.excluded, &|_| {});
        save_to(shared, self.snapshot);
    }

    /// Makes the changes `records` hold, then walks the folders they
    /// brought, and takes out what is excluded, should a folder moved in
    /// hold some of it.
    fn take(&self, shared: &RwLock<Index>, records: &[Record]) {
        self.make(shared, &changes::changes(records));
        prune(shared, self.excluded, &|_| {});
    }

    /// Makes `changes`, then walks the folders they brought.
    fn make(&self, shared: &RwLock<Index>, changes: &[Change]) {
        let mut walks = Vec::new();
        for change in changes {
            walks.extend(changes::apply(&mut write(shared), change, self.excluded));
        }
        for path in changes::outermost(walks) {
            walk_afresh(shared, &path, self.excluded);
        }
    }

    /// Puts in force what is excluded, if it changed since `seen`: takes
    /// out what is excluded now, and brings back what no longer is.
    fn keep_out(&self, shared: &RwLock<Index>, seen: &mut Vec<String>, report: &dyn Fn(Found)) {
        let now = self.excluded.paths();
        if now == *seen {
            return;
        }
        prune(shared, self.excluded, report);
        let back = seen.iter().filter(|path| !now.contains(path));
        let back = back.filter_map(|path| {
            let (kind, stat) = on_disk(path)?;
            Some(Change::Added(path.clone(), kind, stat))
        });
        self.make(shared, &back.collect::<Vec<_>>());
        *seen = now;
    }
}

/// What the provider last told the window, saved, and left out: the
/// index's version and when.
struct Kept {
    told: Mark,
    saved: Mark,
    excluded: Vec<String>,
}

impl Kept {
    /// How long to wait for changes: until the window is due to be told of
    /// those made, or a while.
    fn patience(&self, version: u64) -> Duration {
        match version == self.told.version {
            true => IDLE,
            false => TELL_EVERY.saturating_sub(self.told.at.elapsed()),
        }
    }
}

/// The index's version when something was last done with it, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    version: u64,
    at: Instant,
}

impl Mark {
    fn new(version: u64) -> Mark {
        Mark {
            version,
            at: Instant::now(),
        }
    }

    /// Whether it is due again for the index at `version`, at `now`, when
    /// done at most every `every` and only for a version not done yet;
    /// marks it done if so.
    fn due(&mut self, version: u64, now: Instant, every: Duration) -> bool {
        let due = version != self.version && now.duration_since(self.at) >= every;
        if due {
            *self = Mark { version, at: now };
        }
        due
    }
}

/// What the walk would take the name at `path` for, if it is there, and
/// its size and time.
fn on_disk(path: &str) -> Option<(Kind, Stat)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let kind = metadata.file_type();
    let kind = match (kind.is_dir(), kind.is_symlink_dir()) {
        (true, _) => Kind::Folder,
        (false, true) => Kind::Link,
        (false, false) => Kind::File,
    };
    Some((kind, stat_of(&metadata)))
}

/// The size and last write that `metadata` gives: a folder holds none.
fn stat_of(metadata: &fs::Metadata) -> Stat {
    Stat {
        size: metadata.file_size(),
        written: unix_seconds(metadata.last_write_time() as i64),
    }
}

/// Walks the folder at `path` again from nothing: whatever the index
/// held under it goes, and what is there now comes in. Nothing, unless it
/// is in the index and, once listed, on disk a folder the walk enters: a
/// junction made a moment ago is first told of as a plain folder, and a
/// listing taken through it holds another folder's entries. A junction
/// stays one, so a plain folder after the listing was one during it.
fn walk_afresh(shared: &RwLock<Index>, path: &str, excluded: &Excluded) {
    let folder = Path::new(path);
    let children = listing(folder, false, &excluded.within(folder));
    if on_disk(path).map(|(kind, _)| kind) != Some(Kind::Folder) {
        return;
    }
    let entered = {
        let mut index = write(shared);
        let Some(entry) = emptied(&mut index, path) else {
            return;
        };
        take_in(&mut index, folder, entry, children)
    };
    for (below, entry) in entered {
        walk_under(&below, entry, false, shared, excluded);
    }
}

/// The folder at `path`, put again in place of the one there, with
/// nothing in it.
fn emptied(index: &mut Index, path: &str) -> Option<EntryId> {
    let entry = index.find(path)?;
    let (folder, name) = (index.parent(entry)?, index.name(entry)?.to_string());
    index.remove(entry).ok()?;
    index.add(folder, &name, true).ok()
}

/// Takes what is excluded out of the index, with everything under it,
/// and says so if anything was there.
fn prune(shared: &RwLock<Index>, excluded: &Excluded, report: &dyn Fn(Found)) {
    let mut index = write(shared);
    let found: Vec<_> = excluded
        .paths()
        .iter()
        .filter_map(|path| index.find(path))
        .collect();
    for entry in &found {
        // An entry under another excluded one went with it.
        let _ = index.remove(*entry);
    }
    drop(index);
    if !found.is_empty() {
        report(Found::Files);
    }
}

/// Saves the index at `snapshot`, if there is one.
fn save_to(shared: &RwLock<Index>, snapshot: Option<&Path>) {
    let index = read(shared);
    if let Some(path) = snapshot
        && let Err(error) = save(&index, path)
    {
        eprintln!("hyalo: cannot save {}: {error}", path.display());
    }
}

/// Puts the index saved at `snapshot` in place of the empty one, if it
/// can be read.
fn load_into(shared: &RwLock<Index>, snapshot: Option<&Path>, report: &dyn Fn(Found)) {
    let started = Instant::now();
    let Some(index) = snapshot.and_then(load) else {
        return;
    };
    let took = started.elapsed();
    eprintln!(
        "hyalo: {} files and folders from the snapshot in {took:.1?}",
        index.len()
    );
    *write(shared) = index;
    report(Found::Files);
}

/// Walks the roots: straight into an empty index, so files turn up as
/// they are found, or into a fresh one swapped in whole. Either way gives
/// back the room kept for growth.
fn walk_into(
    shared: &RwLock<Index>,
    roots: &[PathBuf],
    excluded: &Excluded,
    report: &dyn Fn(Found),
) {
    let started = Instant::now();
    let empty = read(shared).is_empty();
    if empty {
        walk_all(roots, shared, excluded, &|| report(Found::Files));
        write(shared).shrink_to_fit();
    } else {
        let fresh = RwLock::default();
        walk_all(roots, &fresh, excluded, &|| {});
        let mut fresh = fresh.into_inner().unwrap_or_else(PoisonError::into_inner);
        fresh.shrink_to_fit();
        *write(shared) = fresh;
    }
    report(Found::Files);
    let index = read(shared);
    let (mebibytes, took) = (index.memory() >> 20, started.elapsed());
    eprintln!(
        "hyalo: {} files and folders, {mebibytes} MiB, walked in {took:.1?}",
        index.len()
    );
}

/// `%LOCALAPPDATA%\hyalo\files.idx`: local, since it describes this
/// machine's drives.
pub fn default_snapshot() -> Option<PathBuf> {
    store::ours("LOCALAPPDATA", "files.idx")
}

/// The index saved at `path`, unless there is none or it is refused; a
/// refused one is simply walked again and saved over.
fn load(path: &Path) -> Option<Index> {
    let file = File::open(path).ok()?;
    let loaded = Index::load(&mut BufReader::new(file));
    let report = |error: &_| eprintln!("hyalo: cannot read {}: {error:?}", path.display());
    loaded.inspect_err(report).ok()
}

/// Writes the index whole or not at all: into a file beside `path`, then
/// renamed over it.
fn save(index: &Index, path: &Path) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension("tmp");
    let mut out = BufWriter::new(File::create(&temporary)?);
    index.save(&mut out)?;
    out.flush()?;
    drop(out);
    fs::rename(&temporary, path)
}

/// Walks every root into `into`, each on a thread of its own; `tick`
/// runs on this thread every so often until all are done.
fn walk_all(roots: &[PathBuf], into: &RwLock<Index>, excluded: &Excluded, tick: &dyn Fn()) {
    thread::scope(|scope| {
        let walkers: Vec<_> = roots
            .iter()
            .map(|root| scope.spawn(move || walk(root, into, excluded)))
            .collect();
        while !walkers.iter().all(|walker| walker.is_finished()) {
            thread::sleep(TICK);
            tick();
        }
    });
}

/// Adds `root` to the index as a root of its own, then everything under
/// it that is not excluded, a folder at a time.
fn walk(root: &Path, into: &RwLock<Index>, excluded: &Excluded) {
    let Ok(top) = write(into).add_root(&root_name(root)) else {
        return;
    };
    walk_under(root, top, true, into, excluded);
}

/// Adds everything under `folder`, the entry `id`, that is not excluded,
/// a folder at a time, as entries it has not seen. `at_root`: whether it
/// is a root.
fn walk_under(
    folder: &Path,
    id: EntryId,
    at_root: bool,
    into: &RwLock<Index>,
    excluded: &Excluded,
) {
    let mut folders = vec![(folder.to_path_buf(), id, at_root)];
    while let Some((folder, id, at_root)) = folders.pop() {
        let children = listing(&folder, at_root, &excluded.within(&folder));
        let entered = take_in(&mut write(into), &folder, id, children);
        folders.extend(
            entered
                .into_iter()
                .map(|(below, entry)| (below, entry, false)),
        );
    }
}

/// Adds `children`, listed in `folder`, to its entry `id`; the folders
/// among them the walk enters, with their entries.
fn take_in(
    index: &mut Index,
    folder: &Path,
    id: EntryId,
    children: Vec<Child>,
) -> Vec<(PathBuf, EntryId)> {
    let mut entered = Vec::new();
    for child in children {
        let Ok(entry) = index.add(id, &child.name, child.folder) else {
            continue;
        };
        // Added just now, so it is there.
        let _ = index.set_stat(entry, child.stat.size, child.stat.written);
        if child.enter {
            entered.push((folder.join(&child.name), entry));
        }
    }
    entered
}

/// A root's name in the index: `C:` for `C:\`, so paths join with one
/// backslash.
fn root_name(root: &Path) -> String {
    root.to_string_lossy().trim_end_matches('\\').to_string()
}

/// One entry of a folder, as the walk sees it.
struct Child {
    name: String,
    folder: bool,
    /// Whether the walk goes into it: a folder, not a link to one.
    enter: bool,
    stat: Stat,
}

/// What the walk keeps of a folder's entries, but for the names
/// `left_out` (lowercased); nothing if it cannot be read (no rights,
/// gone since).
fn listing(folder: &Path, at_root: bool, left_out: &[String]) -> Vec<Child> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let children = entries.filter_map(|entry| child(entry.ok()?, at_root));
    let kept = |child: &Child| left_out.is_empty() || !left_out.contains(&lowercase(&child.name));
    children.filter(kept).collect()
}

/// An entry as a child, unless the walk leaves it out. A junction or a
/// symbolic link to a folder is listed as a folder but never entered: it
/// leads somewhere walked anyway, or round in a loop. A name that is not
/// valid Unicode cannot be opened by name again, so it is left out.
fn child(entry: DirEntry, at_root: bool) -> Option<Child> {
    let name = entry.file_name().into_string().ok()?;
    let kind = entry.file_type().ok()?;
    // Read with the name, so it costs nothing more.
    let stat = entry.metadata().map(|metadata| stat_of(&metadata));
    kept(&name, at_root).then(|| Child {
        folder: kind.is_dir() || kind.is_symlink_dir(),
        enter: kind.is_dir(),
        stat: stat.unwrap_or_default(),
        name,
    })
}

/// Whether the walk lists an entry: at a drive's root, not the system's
/// own folders: those whose names start with `$` (`$Recycle.Bin`,
/// `$WinREAgent`, `$Windows.~BT`) and `System Volume Information`.
fn kept(name: &str, at_root: bool) -> bool {
    let system = name.starts_with('$') || name.eq_ignore_ascii_case("System Volume Information");
    !(at_root && system)
}

/// The roots of the fixed drives: `C:\`, `D:\`. Removable, network and
/// optical drives are left out: they come and go, and walking them is
/// slow.
pub fn fixed_drives() -> Vec<PathBuf> {
    let mut buffer = [0_u16; 512];
    // SAFETY: the buffer is ours, and its length goes with it.
    let length = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) } as usize;
    let roots = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
    // The list ends in empty names, which are no fixed drive.
    let fixed = roots.split('\0').filter(|root| fixed(root));
    fixed.map(PathBuf::from).collect()
}

fn fixed(root: &str) -> bool {
    // SAFETY: a plain call with a string that outlives it.
    unsafe { GetDriveTypeW(&HSTRING::from(root)) == DRIVE_FIXED }
}

/// Folders in the user's profile that often hold their projects.
const PROJECTS: [&str; 7] = ["code", "projects", "source", "src", "dev", "repos", "git"];

/// The user's folders: Desktop, Documents and Downloads wherever they
/// were moved (to OneDrive, say), and the project folders in their
/// profile that exist.
pub fn places() -> Places {
    let known = [FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads];
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    favoured(
        known.iter().filter_map(known_folder).collect(),
        home.as_deref(),
    )
}

/// The `known` folders, and the project folders that exist in `home`.
fn favoured(mut known: Vec<String>, home: Option<&Path>) -> Places {
    let projects = home
        .into_iter()
        .flat_map(|home| PROJECTS.map(|name| home.join(name)));
    let projects = projects.filter(|path| path.is_dir());
    known.extend(projects.map(|path| path.to_string_lossy().into_owned()));
    let folders: Vec<&str> = known.iter().map(String::as_str).collect();
    Places::new(&folders)
}

/// Where a known folder lives, if it has a place on disk.
fn known_folder(id: &GUID) -> Option<String> {
    // SAFETY: the shell allocates the path; it is read once, then freed
    // once.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0.cast()));
        text
    }
}

/// The files and folders among `ids` that are still there, as rows: a
/// picked file is known by its full path.
pub fn picked<'a>(ids: impl IntoIterator<Item = &'a ItemId>) -> Vec<Item> {
    let paths = ids.into_iter().filter(|id| full_path(&id.0));
    let rows = paths.map(|id| Some(Item::file(id.0.clone(), fs::metadata(&id.0).ok()?.is_dir())));
    rows.flatten().collect()
}

/// Whether `text` is the full path of something on a drive, not the
/// drive itself: `C:\` and more.
fn full_path(text: &str) -> bool {
    text.len() > 3 && places::full_path(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use hyalo_core::item::Kind;
    use std::cell::RefCell;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::SeqCst;

    fn walked(root: &Path) -> Index {
        walked_leaving_out(root, &Excluded::default())
    }

    fn walked_leaving_out(root: &Path, excluded: &Excluded) -> Index {
        let index = RwLock::default();
        walk(root, &index, excluded);
        index.into_inner().expect("the index")
    }

    /// What leaves out `paths`.
    fn excluding(paths: &[String]) -> Excluded {
        let excluded = Excluded::default();
        excluded.set(paths);
        excluded
    }

    #[test]
    fn windows_time_reads_in_seconds_since_1970() {
        assert_eq!(unix_seconds(116_444_736_000_000_000), 0);
        assert_eq!(
            unix_seconds(134_387_838_000_000_000 + 9_999_999),
            1_794_310_200
        );
        assert_eq!(unix_seconds(0), -11_644_473_600);
        assert_eq!(unix_seconds(-1), -11_644_473_601, "rounded down");
    }

    #[test]
    fn setting_what_is_left_out_says_whether_it_changed() {
        let excluded = Excluded::default();
        assert!(!excluded.set(&[]), "nothing to nothing");
        let one = [r"C:\Windows\WinSxS".to_string()];
        assert!(excluded.set(&one));
        assert!(!excluded.set(&one), "the same again");
        assert!(excluded.set(&[]));
    }

    #[test]
    fn the_names_left_out_of_a_folder_are_those_excluded_in_it() {
        let paths = [
            r"C:\Windows\WinSxS".to_string(),
            r"c:\windows\Installer".to_string(),
            r"C:\Temp".to_string(),
            r"D:\Windows\Other".to_string(),
        ];
        let excluded = excluding(&paths);
        assert_eq!(
            excluded.within(Path::new(r"C:\WINDOWS")),
            ["winsxs", "installer"]
        );
        assert_eq!(
            excluded.within(Path::new(r"C:\")),
            ["temp"],
            "at a drive's root"
        );
        assert_eq!(
            excluded.within(Path::new(r"C:\Windows\System32")),
            Vec::<String>::new()
        );
        assert_eq!(
            Excluded::default().within(Path::new(r"C:\Windows")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_walk_leaves_out_what_is_excluded_with_everything_in_it() {
        let folder = Scratch::new("excluded").with(&[
            "kept\\",
            r"kept\notes.txt",
            "Dropped\\",
            r"Dropped\notes.txt",
            r"Dropped\deeper\",
            r"Dropped\deeper\notes.md",
            "secret.txt",
        ]);
        let paths = [folder.path("dropped"), folder.path("SECRET.TXT")];
        let index = walked_leaving_out(&folder.0, &excluding(&paths));
        assert_eq!(found(&index, "notes"), [folder.path(r"kept\notes.txt")]);
        assert_eq!(
            index.find(&folder.path("Dropped")),
            None,
            "the folder itself too"
        );
        assert_eq!(found(&index, "secret"), Vec::<String>::new());
    }

    fn found(index: &Index, query: &str) -> Vec<String> {
        let rows = index.search(query, &Places::default(), 20).rows;
        rows.into_iter().map(|row| row.item.id.0).collect()
    }

    #[test]
    fn a_walk_finds_every_file_and_folder_under_its_root() {
        let folder =
            Scratch::new("walk").with(&["notes.txt", "a\\", "a\\b\\", "a\\b\\deep notes.md"]);
        let index = walked(&folder.0);
        assert_eq!(index.len(), 5, "the root and four entries");
        assert_eq!(
            found(&index, "notes"),
            [folder.path("notes.txt"), folder.path(r"a\b\deep notes.md")]
        );
        let rows = index.search("b", &Places::default(), 5).rows;
        assert_eq!(rows[0].item.kind, Kind::Folder);
    }

    /// The size and last write of the file at `path` on disk.
    fn stat_on_disk(path: &Path) -> (u64, i64) {
        let metadata = fs::metadata(path).expect("there");
        let written = metadata.modified().expect("a time");
        let written = written
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970");
        (metadata.len(), written.as_secs() as i64)
    }

    #[test]
    fn a_walk_keeps_each_files_size_and_last_write() {
        let folder = Scratch::new("walk-stat").with(&["sub\\"]);
        fs::write(folder.0.join(r"sub\five.txt"), b"12345").expect("write");
        let index = walked(&folder.0);
        let five = index.find(&folder.path(r"sub\five.txt")).expect("found");
        assert_eq!(
            index.stat(five),
            Some(stat_on_disk(&folder.0.join(r"sub\five.txt")))
        );
        assert_eq!(index.stat(five).map(|(size, _)| size), Some(5));
    }

    #[test]
    fn a_link_to_a_folder_is_listed_but_not_entered() {
        let folder = Scratch::new("link").with(&["real\\", "real\\inside.txt"]);
        let link = folder.0.join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(folder.0.join("real"))
            .output()
            .expect("run mklink");
        assert!(made.status.success(), "make a junction");
        let index = walked(&folder.0);
        assert_eq!(found(&index, "inside"), [folder.path(r"real\inside.txt")]);
        let rows = index.search("link", &Places::default(), 5).rows;
        assert_eq!(rows[0].item.id.0, folder.path("link"));
        assert_eq!(rows[0].item.kind, Kind::Folder);
    }

    #[test]
    fn the_system_folders_at_a_drives_root_are_left_out() {
        for name in [
            "$Recycle.Bin",
            "$WinREAgent",
            "System Volume Information",
            "system volume information",
        ] {
            assert!(!kept(name, true), "{name}");
        }
        assert!(kept("$Recycle.Bin", false), "only at the root");
        assert!(kept("Windows", true));
        assert!(kept("pagefile.sys", true));
    }

    #[test]
    fn a_walk_leaves_out_the_system_folders_at_its_root_only() {
        let folder = Scratch::new("system").with(&[
            "$Recycle.Bin\\",
            r"$Recycle.Bin\deleted.txt",
            "System Volume Information\\",
            "sub\\$kept\\",
            r"sub\$kept\kept.txt",
        ]);
        let index = walked(&folder.0);
        assert_eq!(found(&index, "deleted"), Vec::<String>::new());
        assert_eq!(found(&index, "volume"), Vec::<String>::new());
        assert_eq!(
            found(&index, "kept.txt"),
            [folder.path(r"sub\$kept\kept.txt")]
        );
    }

    #[test]
    fn a_folder_it_cannot_read_lists_nothing() {
        assert!(listing(Path::new(r"C:\no such folder here"), false, &[]).is_empty());
    }

    #[test]
    fn a_drive_is_named_without_its_backslash() {
        assert_eq!(root_name(Path::new("C:\\")), "C:");
        assert_eq!(root_name(Path::new(r"D:\Data")), r"D:\Data");
    }

    /// A provider walking `folder`, keeping its snapshot at `snapshot`, and
    /// what the window searches.
    fn walking(folder: &Scratch, snapshot: Option<PathBuf>) -> (Provider, Files) {
        let (provider, files, _) = walking_leaving_out(folder, snapshot);
        (provider, files)
    }

    /// The same, and what it leaves out.
    fn walking_leaving_out(
        folder: &Scratch,
        snapshot: Option<PathBuf>,
    ) -> (Provider, Files, Excluded) {
        let (files, writer) = Files::new();
        let root = folder.0.clone();
        let roots: Roots = Arc::new(move || vec![root.clone()]);
        let excluded = Excluded::default();
        (
            provider(writer, roots, snapshot, excluded.clone()),
            files,
            excluded,
        )
    }

    /// A provider at work on a thread of its own, and what the window
    /// searches. Dropped, nobody searches the index, so the provider ends.
    struct Serving {
        files: Option<Files>,
        reports: Arc<AtomicUsize>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Serving {
        fn new(provider: Provider, files: Files) -> Serving {
            let reports = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&reports);
            let thread = thread::spawn(move || {
                provider(&request(), &|found| {
                    assert!(matches!(found, Found::Files));
                    counted.fetch_add(1, SeqCst);
                })
            });
            Serving {
                files: Some(files),
                reports,
                thread: Some(thread),
            }
        }

        fn files(&self) -> &Files {
            self.files.as_ref().expect("still served")
        }

        fn reports(&self) -> usize {
            self.reports.load(SeqCst)
        }

        /// Waits up to ten seconds for `done` to hold; whether it did.
        fn until(&self, done: impl Fn(&Serving) -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done(self) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            done(self)
        }

        /// Waits for `query` to find just `paths`, in their order.
        fn finds(&self, query: &str, paths: &[String]) {
            self.until(|serving| search(serving.files(), query) == paths);
            assert_eq!(search(self.files(), query), paths, "{query}");
        }

        /// Stops the provider; how often it told the window.
        fn stop(mut self) -> usize {
            self.end();
            self.reports()
        }

        fn end(&mut self) {
            self.files = None;
            if let Some(thread) = self.thread.take() {
                let ended = thread.join();
                assert!(
                    ended.is_ok() || thread::panicking(),
                    "the provider panicked"
                );
            }
        }
    }

    impl Drop for Serving {
        fn drop(&mut self) {
            self.end();
        }
    }

    fn request() -> Request {
        Request {
            icon_size: 32,
            known: Default::default(),
        }
    }

    /// Runs the provider here; how often it told the window. Only for a
    /// run that ends at once.
    fn run(provider: &Provider) -> usize {
        let reports = RefCell::new(0);
        provider(&request(), &|found| {
            assert!(matches!(found, Found::Files));
            *reports.borrow_mut() += 1;
        });
        reports.into_inner()
    }

    fn search(files: &Files, query: &str) -> Vec<String> {
        let rows = files.search(query, &Filters::default(), &Places::default(), 20);
        rows.into_iter().map(|row| row.item.id.0).collect()
    }

    #[test]
    fn the_provider_walks_into_what_the_window_searches_and_says_so() {
        let folder = Scratch::new("provider").with(&["report.pdf"]);
        let serving = Serving::new_walking(&folder, None);
        serving.finds("report", &[folder.path("report.pdf")]);
        assert!(serving.stop() >= 1);
    }

    impl Serving {
        fn new_walking(folder: &Scratch, snapshot: Option<PathBuf>) -> Serving {
            let (provider, files) = walking(folder, snapshot);
            Serving::new(provider, files)
        }
    }

    #[test]
    fn a_file_made_renamed_and_removed_is_followed_at_once() {
        let folder = Scratch::new("live").with(&["sub\\"]);
        let serving = Serving::new_walking(&folder, None);
        serving.until(|serving| serving.reports() >= 1);
        let path = |name: &str| folder.0.join(name);
        fs::write(path(r"sub\fresh.txt"), b"x").expect("write");
        serving.finds("fresh", &[folder.path(r"sub\fresh.txt")]);
        fs::rename(path(r"sub\fresh.txt"), path(r"sub\renamed.txt")).expect("rename");
        serving.finds("renamed", &[folder.path(r"sub\renamed.txt")]);
        assert_eq!(search(serving.files(), "fresh"), Vec::<String>::new());
        fs::rename(path(r"sub\renamed.txt"), path("moved.txt")).expect("move");
        serving.finds("moved", &[folder.path("moved.txt")]);
        fs::remove_file(path("moved.txt")).expect("remove");
        serving.finds("moved", &[]);
        assert_eq!(read(&serving.files().0).len(), 2, "the root and sub");
    }

    #[test]
    fn a_folder_moved_in_whole_is_walked_and_one_renamed_keeps_its_files() {
        let folder = Scratch::new("live-folder").with(&["watched\\"]);
        let outside = Scratch::new("live-outside").with(&[
            "brought\\",
            r"brought\deep\",
            r"brought\deep\inside.txt",
        ]);
        let root = folder.0.join("watched");
        let (files, writer) = Files::new();
        let roots: Roots = Arc::new(move || vec![root.clone()]);
        let serving = Serving::new(provider(writer, roots, None, Excluded::default()), files);
        serving.until(|serving| serving.reports() >= 1);
        fs::rename(outside.0.join("brought"), folder.0.join(r"watched\brought")).expect("move in");
        serving.finds("inside", &[folder.path(r"watched\brought\deep\inside.txt")]);
        let renamed = folder.0.join(r"watched\taken");
        fs::rename(folder.0.join(r"watched\brought"), &renamed).expect("rename");
        serving.finds("inside", &[folder.path(r"watched\taken\deep\inside.txt")]);
    }

    #[test]
    fn a_junction_made_while_watched_is_listed_but_not_entered() {
        let folder = Scratch::new("live-link").with(&["real\\", r"real\inside.txt"]);
        let serving = Serving::new_walking(&folder, None);
        serving.finds("inside", &[folder.path(r"real\inside.txt")]);
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(folder.0.join("link"))
            .arg(folder.0.join("real"))
            .output()
            .expect("run mklink");
        assert!(made.status.success(), "make a junction");
        serving.finds("link", &[folder.path("link")]);
        // Long enough for a walk of the junction to have shown.
        thread::sleep(3 * TELL_EVERY);
        assert_eq!(
            search(serving.files(), "inside"),
            [folder.path(r"real\inside.txt")]
        );
    }

    #[test]
    fn a_file_written_while_watched_has_its_new_size_and_time_at_once() {
        let folder = Scratch::new("live-written").with(&["grows.txt"]);
        let serving = Serving::new_walking(&folder, None);
        serving.finds("grows", &[folder.path("grows.txt")]);
        let path = folder.0.join("grows.txt");
        fs::write(&path, b"now ten b.").expect("write");
        let stat = |serving: &Serving| {
            let index = read(&serving.files().0);
            index
                .find(&folder.path("grows.txt"))
                .and_then(|entry| index.stat(entry))
        };
        serving.until(|serving| stat(serving).is_some_and(|(size, _)| size == 10));
        assert_eq!(stat(&serving), Some(stat_on_disk(&path)));
    }

    #[test]
    fn the_files_and_folders_the_filters_let_through_are_kept_of_those_given() {
        let folder = Scratch::new("let-through").with(&["a.pdf", "b.txt", "sub\\"]);
        let (files, writer) = Files::new();
        walk(&folder.0, &writer.0, &Excluded::default());
        let items = [
            Item::file(folder.path("a.pdf"), false),
            Item::file(folder.path("b.txt"), false),
            Item::file(folder.path("gone.pdf"), false),
            Item {
                kind: Kind::App,
                ..Item::file(folder.path("a.pdf"), false)
            },
        ];
        let clock = hyalo_core::filter::Clock { now: 0, offset: 0 };
        let filters = hyalo_core::filter::Query::parse("ext:pdf", clock).filters;
        let kept = files.let_through(&filters, &items);
        assert_eq!(kept, [&items[0]], "files there alone");
    }

    #[test]
    fn what_is_excluded_stays_out_when_made_while_watched() {
        let folder = Scratch::new("live-excluded").with(&["kept\\"]);
        let (provider, files, excluded) = walking_leaving_out(&folder, None);
        excluded.set(&[folder.path("secret")]);
        let serving = Serving::new(provider, files);
        serving.until(|serving| serving.reports() >= 1);
        fs::create_dir(folder.0.join("secret")).expect("make the excluded folder");
        fs::write(folder.0.join(r"secret\plans.txt"), b"x").expect("write");
        fs::write(folder.0.join(r"kept\plans.txt"), b"x").expect("write");
        serving.finds("plans", &[folder.path(r"kept\plans.txt")]);
        assert_eq!(search(serving.files(), "secret"), Vec::<String>::new());
    }

    #[test]
    fn a_folder_excluded_while_watched_leaves_and_comes_back_when_no_longer() {
        let folder = Scratch::new("pruned").with(&[
            "kept\\",
            r"kept\notes.txt",
            "gone\\",
            r"gone\notes.txt",
        ]);
        let (provider, files, excluded) = walking_leaving_out(&folder, None);
        let serving = Serving::new(provider, files);
        let both = [
            folder.path(r"gone\notes.txt"),
            folder.path(r"kept\notes.txt"),
        ];
        let listed = |serving: &Serving| {
            let mut found = search(serving.files(), "notes");
            found.sort();
            found
        };
        assert!(serving.until(|serving| listed(serving) == both));
        excluded.set(&[folder.path("gone")]);
        serving.finds("notes", &[folder.path(r"kept\notes.txt")]);
        excluded.set(&[]);
        assert!(
            serving.until(|serving| listed(serving) == both),
            "back once no longer excluded"
        );
    }

    #[test]
    fn a_watch_that_lost_track_walks_its_root_again_and_that_one_alone() {
        let (lost, kept) = (Scratch::new("lost"), Scratch::new("lost-not"));
        let (files, writer) = Files::new();
        let excluded = Excluded::default();
        let walk = Walk {
            roots: &|| Vec::new(),
            snapshot: None,
            excluded: &excluded,
        };
        let roots = [kept.0.clone(), lost.0.clone()];
        walk.walk(&writer.0, &roots, &|_| {});
        // Made while nothing watches: only a walk finds them.
        fs::write(lost.0.join("unseen.txt"), b"x").expect("write");
        fs::write(kept.0.join("unseen.txt"), b"x").expect("write");
        let (sender, events) = mpsc::channel();
        sender.send((1, Event::Lost)).expect("send");
        // Searches until it finds, then lets the index go, which ends the
        // following.
        let searching = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while search(&files, "unseen").is_empty() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            search(&files, "unseen")
        });
        walk.follow(&writer.0, &roots, &events, &|_| {});
        let found = searching.join().expect("the search");
        assert_eq!(
            found,
            [lost.path("unseen.txt")],
            "the other root not walked"
        );
    }

    #[test]
    fn what_changed_is_saved_by_the_time_it_stops() {
        let folder = Scratch::new("saved-live");
        let snapshot = folder.0.join("files.idx");
        let serving = Serving::new_walking(&folder, Some(snapshot.clone()));
        serving.until(|serving| serving.reports() >= 1);
        fs::write(folder.0.join("late.txt"), b"x").expect("write");
        serving.finds("late", &[folder.path("late.txt")]);
        serving.stop();
        let saved = load(&snapshot).expect("saved");
        let rows = saved.search("late", &Places::default(), 5).rows;
        assert_eq!(rows[0].item.id.0, folder.path("late.txt"));
    }

    #[test]
    fn something_done_now_and_then_is_due_for_a_new_version_once_enough_time_passed() {
        let start = Instant::now();
        let mut mark = Mark {
            version: 1,
            at: start,
        };
        let every = Duration::from_millis(500);
        let later = |millis| start + Duration::from_millis(millis);
        assert!(!mark.due(1, later(600), every), "nothing new");
        assert!(mark.due(2, later(600), every));
        assert_eq!(
            mark,
            Mark {
                version: 2,
                at: later(600)
            }
        );
        assert!(!mark.due(3, later(700), every), "too soon");
        assert!(mark.due(3, later(1100), every));
        assert!(!mark.due(3, later(1700), every), "done already");
        assert!(
            mark.due(4, later(1100), Duration::ZERO),
            "at once, when asked to"
        );
    }

    #[test]
    fn it_waits_a_while_unless_the_window_is_due_to_be_told() {
        let kept = Kept {
            told: Mark::new(1),
            saved: Mark::new(1),
            excluded: Vec::new(),
        };
        assert_eq!(kept.patience(1), IDLE);
        assert!(kept.patience(2) <= TELL_EVERY);
        assert!(kept.patience(2) > TELL_EVERY / 2);
        let long_ago = Kept {
            told: Mark {
                version: 1,
                at: Instant::now() - IDLE,
            },
            ..kept
        };
        assert_eq!(long_ago.patience(2), Duration::ZERO);
    }

    #[test]
    fn a_snapshot_holding_what_is_excluded_now_loses_it_before_the_walk() {
        let folder = Scratch::new("pruned-snapshot").with(&["gone\\", r"gone\notes.txt"]);
        let snapshot = folder.0.join("files.idx");
        Serving::new_walking(&folder, Some(snapshot.clone())).stop();
        let (gated, gate) = mpsc::channel::<()>();
        let (files, writer) = Files::new();
        let gate = Mutex::new(gate);
        let roots: Roots = Arc::new(move || {
            let _ = gate.lock().expect("the gate").recv();
            Vec::new()
        });
        let excluded = excluding(&[folder.path("gone")]);
        let serving = Serving::new(provider(writer, roots, Some(snapshot), excluded), files);
        // Before the walk asks for its roots, the snapshot is in, what is
        // excluded is out, and the window was told of both.
        let pruned = |serving: &Serving| {
            let index = read(&serving.files().0);
            !index.is_empty() && index.find(&folder.path("gone")).is_none()
        };
        serving.until(|serving| pruned(serving) && serving.reports() == 2);
        let (pruned, told) = (pruned(&serving), serving.reports());
        let listed = search(serving.files(), "notes");
        gated.send(()).expect("let the walk go");
        serving.until(|serving| serving.reports() == 3);
        assert!(pruned, "pruned before the walk");
        assert_eq!(told, 2, "the snapshot, then the prune");
        assert!(listed.is_empty(), "{listed:?}");
        assert_eq!(
            serving.stop(),
            3,
            "then the walk; nothing left to prune after it"
        );
    }

    /// The memory `files` takes, and what the same index takes loaded
    /// from its snapshot, which sizes every part exactly.
    fn memory(files: &Files) -> (usize, usize) {
        let index = read(&files.0);
        let mut bytes = Vec::new();
        index.save(&mut bytes).expect("save to memory");
        let loaded = Index::load(&mut bytes.as_slice()).expect("load");
        (index.memory(), loaded.memory())
    }

    #[test]
    fn a_walk_keeps_no_room_for_growth() {
        let names: Vec<String> = (0..300).map(|n| format!("file {n}.txt")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let folder = Scratch::new("shrink").with(&names);
        let snapshot = Scratch::new("shrink-snapshot");
        let path = snapshot.0.join("files.idx");
        let first = Serving::new_walking(&folder, Some(path.clone()));
        first.until(|serving| serving.reports() >= 1 && read(&serving.files().0).len() == 301);
        let (walked, exact) = memory(first.files());
        assert_eq!(walked, exact, "the first walk");
        first.stop();
        // From the snapshot, then a walk into a fresh index.
        let again = Serving::new_walking(&folder, Some(path));
        again.until(|serving| serving.reports() >= 2);
        let (walked, exact) = memory(again.files());
        assert_eq!(walked, exact, "a walk into a fresh index");
    }

    #[test]
    fn a_snapshot_finds_files_before_any_walk() {
        let folder = Scratch::new("snapshot").with(&["kept.txt"]);
        let snapshot = Scratch::new("snapshot-file");
        let path = snapshot.0.join("hyalo").join("files.idx");
        Serving::new_walking(&folder, Some(path.clone())).stop();
        assert!(path.exists(), "saved");
        fs::remove_file(folder.0.join("kept.txt")).expect("remove");
        let (files, writer) = Files::new();
        let before_walking = Arc::new(Mutex::new(None));
        // Held weakly: a reader held for good would keep the provider at
        // work for good.
        let (watched, seen) = (Arc::downgrade(&files.0), Arc::clone(&before_walking));
        let roots: Roots = Arc::new(move || {
            let files = Files(watched.upgrade().expect("still searched"));
            *seen.lock().expect("seen") = Some(search(&files, "kept"));
            vec![]
        });
        let serving = Serving::new(
            provider(writer, roots, Some(path), Excluded::default()),
            files,
        );
        serving.until(|_| before_walking.lock().expect("seen").is_some());
        drop(serving);
        let seen = before_walking.lock().expect("seen").clone();
        assert_eq!(seen, Some(vec![folder.path("kept.txt")]));
    }

    #[test]
    fn a_snapshot_it_cannot_read_is_walked_again_and_saved_over() {
        let folder = Scratch::new("bad-snapshot").with(&["fresh.txt"]);
        let snapshot = Scratch::new("bad-snapshot-file").with(&["files.idx"]);
        let path = snapshot.0.join("files.idx");
        let serving = Serving::new_walking(&folder, Some(path.clone()));
        serving.finds("fresh", &[folder.path("fresh.txt")]);
        serving.stop();
        let mut saved = BufReader::new(File::open(&path).expect("saved"));
        assert!(Index::load(&mut saved).is_ok());
    }

    #[test]
    fn a_run_while_another_is_at_work_does_nothing() {
        let folder = Scratch::new("busy").with(&["x.txt"]);
        let (files, writer) = Files::new();
        let (entered, release) = (std::sync::Barrier::new(2), std::sync::Barrier::new(2));
        let shared = Arc::new((entered, release));
        let gate = Arc::clone(&shared);
        let root = folder.0.clone();
        let roots: Roots = Arc::new(move || {
            gate.0.wait();
            gate.1.wait();
            vec![root.clone()]
        });
        let provider = provider(writer, roots, None, Excluded::default());
        let serving = Serving::new(Arc::clone(&provider), files);
        shared.0.wait();
        assert_eq!(run(&provider), 0, "busy");
        shared.1.wait();
        serving.finds("x", &[folder.path("x.txt")]);
        assert_eq!(run(&provider), 0, "still at work, watching");
    }

    #[test]
    fn the_snapshot_lives_in_local_app_data() {
        let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set on Windows");
        let expected = PathBuf::from(local).join("hyalo").join("files.idx");
        assert_eq!(default_snapshot(), Some(expected));
    }

    #[test]
    fn picked_files_and_folders_that_are_still_there_become_rows() {
        let folder = Scratch::new("picked").with(&["kept.txt", "dir\\"]);
        let ids = [
            ItemId(folder.path("kept.txt")),
            ItemId(folder.path("dir")),
            ItemId(folder.path("gone.txt")),
            ItemId("Microsoft.Windows.Explorer".to_string()),
        ];
        let rows = picked(&ids);
        assert_eq!(
            rows,
            [
                Item::file(folder.path("kept.txt"), false),
                Item::file(folder.path("dir"), true)
            ]
        );
    }

    #[test]
    fn nothing_is_not_a_fixed_drive() {
        assert!(!fixed(""));
    }

    #[test]
    fn the_system_drive_is_a_fixed_drive() {
        let system = std::env::var("SystemDrive").expect("SystemDrive is set");
        let drives = fixed_drives();
        assert!(drives.contains(&PathBuf::from(format!("{system}\\"))));
        let drive_alone = |drive: &PathBuf| full_path(&format!("{}x", drive.display()));
        assert!(drives.iter().all(drive_alone), "{drives:?}");
    }

    #[test]
    fn the_desktop_is_favoured_wherever_it_lives() {
        let desktop = known_folder(&FOLDERID_Desktop).expect("a desktop");
        let file = format!(r"{desktop}\notes.txt");
        assert_eq!(places().of_path(&file), hyalo_core::places::BOOST);
    }

    #[test]
    fn project_folders_in_the_profile_are_favoured_when_they_exist() {
        // The scratch folder lives in AppData, which costs every file
        // there the same: compare with a neighbour.
        let home = Scratch::new("home").with(&["code\\", "notes\\"]);
        let places = favoured(vec![], Some(&home.0));
        let points = |path: &str| places.of_path(&home.path(path));
        let beside = points(r"notes\a.txt");
        assert_eq!(
            points(r"code\app\main.rs") - beside,
            hyalo_core::places::BOOST
        );
        assert_eq!(points(r"projects\a.txt"), beside, "not there");
        assert_eq!(favoured(vec![], None), Places::default());
    }

    #[test]
    fn a_full_path_starts_with_a_drive() {
        assert!(full_path(r"C:\a"));
        assert!(full_path(r"d:\a"));
        assert!(!full_path(r"C:\"), "a drive alone is not picked");
        assert!(!full_path(r"\\server\share"));
        assert!(!full_path("Microsoft.Windows.Explorer"));
        assert!(!full_path(r"1:\a"));
        assert!(!full_path(r"C:/a"));
    }
}
