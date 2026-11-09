//! Files and folders: the index the window searches, the walk of the
//! fixed drives that fills it, and the snapshot that keeps it between
//! runs.
//!
//! Until the indexer service exists, this is how hyalo finds files: a
//! plain walk, folder by folder, with the user's own rights. The first
//! walk writes straight into the index the window searches, so files turn
//! up while it runs; later walks build a fresh index and swap it in whole.

use std::fs::{self, DirEntry, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::windows::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use hyalo_core::item::{Item, ItemId};
use hyalo_core::places::Places;
use hyalo_core::rank::Ranked;
use hyalo_index::Index;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDriveStringsW};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, HSTRING};

use crate::catalog::{Found, Provider, Request};
use crate::store;

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

    /// The `limit` best files and folders for `query`.
    pub fn search(&self, query: &str, places: &Places, limit: usize) -> Vec<Ranked> {
        read(&self.0).search(query, places, limit).rows
    }
}

fn read(index: &RwLock<Index>) -> RwLockReadGuard<'_, Index> {
    index.read().unwrap_or_else(PoisonError::into_inner)
}

fn write(index: &RwLock<Index>) -> RwLockWriteGuard<'_, Index> {
    index.write().unwrap_or_else(PoisonError::into_inner)
}

/// How long the files found are trusted before showing the launcher walks
/// the drives again: a walk costs the disks and a core for a while.
pub const REFRESH_AFTER: Duration = Duration::from_secs(20 * 60);

/// How often a first walk tells the window to search again.
const TICK: Duration = Duration::from_millis(250);

/// The folders a walk starts from: the fixed drives, or a stand-in.
pub type Roots = Arc<dyn Fn() -> Vec<PathBuf> + Send + Sync>;

/// The files provider. The first run loads the snapshot, if there is
/// one; every run walks the roots and saves the snapshot. A run that
/// starts while another walks does nothing.
pub fn provider(writer: Writer, roots: Roots, snapshot: Option<PathBuf>) -> Provider {
    let writer = Mutex::new(writer);
    Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
        let writer = match writer.try_lock() {
            Ok(writer) => writer,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        refresh(&writer.0, &*roots, snapshot.as_deref(), report);
    })
}

/// Loads the snapshot into an empty index, then asks for the roots,
/// walks them and saves the snapshot.
fn refresh(
    shared: &RwLock<Index>,
    roots: &dyn Fn() -> Vec<PathBuf>,
    snapshot: Option<&Path>,
    report: &dyn Fn(Found),
) {
    if read(shared).is_empty() {
        load_into(shared, snapshot, report);
    }
    walk_into(shared, &roots(), report);
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
fn walk_into(shared: &RwLock<Index>, roots: &[PathBuf], report: &dyn Fn(Found)) {
    let started = Instant::now();
    let empty = read(shared).is_empty();
    if empty {
        walk_all(roots, shared, &|| report(Found::Files));
        write(shared).shrink_to_fit();
    } else {
        let fresh = RwLock::default();
        walk_all(roots, &fresh, &|| {});
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
fn walk_all(roots: &[PathBuf], into: &RwLock<Index>, tick: &dyn Fn()) {
    thread::scope(|scope| {
        let walkers: Vec<_> = roots
            .iter()
            .map(|root| scope.spawn(move || walk(root, into)))
            .collect();
        while !walkers.iter().all(|walker| walker.is_finished()) {
            thread::sleep(TICK);
            tick();
        }
    });
}

/// Adds `root` to the index as a root of its own, then everything under
/// it, a folder at a time.
fn walk(root: &Path, into: &RwLock<Index>) {
    let Ok(top) = write(into).add_root(&root_name(root)) else {
        return;
    };
    let mut folders = vec![(root.to_path_buf(), top)];
    while let Some((folder, id)) = folders.pop() {
        let children = listing(&folder, id == top);
        let mut index = write(into);
        for child in children {
            let Ok(entry) = index.add(id, &child.name, child.folder) else {
                continue;
            };
            if child.enter {
                folders.push((folder.join(&child.name), entry));
            }
        }
    }
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
}

/// What the walk keeps of a folder's entries; nothing if it cannot be
/// read (no rights, gone since).
fn listing(folder: &Path, at_root: bool) -> Vec<Child> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| child(entry.ok()?, at_root))
        .collect()
}

/// An entry as a child, unless the walk leaves it out. A junction or a
/// symbolic link to a folder is listed as a folder but never entered: it
/// leads somewhere walked anyway, or round in a loop. A name that is not
/// valid Unicode cannot be opened by name again, so it is left out.
fn child(entry: DirEntry, at_root: bool) -> Option<Child> {
    let name = entry.file_name().into_string().ok()?;
    let kind = entry.file_type().ok()?;
    kept(&name, at_root).then(|| Child {
        folder: kind.is_dir() || kind.is_symlink_dir(),
        enter: kind.is_dir(),
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

/// Whether `text` starts like a full path on a drive: `C:\`.
fn full_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use hyalo_core::item::Kind;
    use std::cell::RefCell;

    fn walked(root: &Path) -> Index {
        let index = RwLock::default();
        walk(root, &index);
        index.into_inner().expect("the index")
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
        assert!(listing(Path::new(r"C:\no such folder here"), false).is_empty());
    }

    #[test]
    fn a_drive_is_named_without_its_backslash() {
        assert_eq!(root_name(Path::new("C:\\")), "C:");
        assert_eq!(root_name(Path::new(r"D:\Data")), r"D:\Data");
    }

    /// A provider walking `folder`, keeping its snapshot at `snapshot`, and
    /// what the window searches.
    fn walking(folder: &Scratch, snapshot: Option<PathBuf>) -> (Provider, Files) {
        let (files, writer) = Files::new();
        let root = folder.0.clone();
        let roots: Roots = Arc::new(move || vec![root.clone()]);
        (provider(writer, roots, snapshot), files)
    }

    fn run(provider: &Provider) -> usize {
        let reports = RefCell::new(0);
        let request = Request {
            icon_size: 32,
            known: Default::default(),
        };
        provider(&request, &|found| {
            assert!(matches!(found, Found::Files));
            *reports.borrow_mut() += 1;
        });
        reports.into_inner()
    }

    fn search(files: &Files, query: &str) -> Vec<String> {
        let rows = files.search(query, &Places::default(), 20);
        rows.into_iter().map(|row| row.item.id.0).collect()
    }

    #[test]
    fn the_provider_walks_into_what_the_window_searches_and_says_so() {
        let folder = Scratch::new("provider").with(&["report.pdf"]);
        let (provider, files) = walking(&folder, None);
        assert!(run(&provider) >= 1);
        assert_eq!(search(&files, "report"), [folder.path("report.pdf")]);
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
        let (provider, files) = walking(&folder, None);
        run(&provider);
        let (walked, exact) = memory(&files);
        assert_eq!(walked, exact, "the first walk");
        run(&provider);
        let (walked, exact) = memory(&files);
        assert_eq!(walked, exact, "a walk into a fresh index");
    }

    #[test]
    fn walking_again_finds_what_changed() {
        let folder = Scratch::new("again").with(&["old.txt"]);
        let (provider, files) = walking(&folder, None);
        run(&provider);
        fs::remove_file(folder.0.join("old.txt")).expect("remove");
        fs::write(folder.0.join("new.txt"), b"x").expect("write");
        run(&provider);
        assert_eq!(search(&files, "old"), Vec::<String>::new());
        assert_eq!(search(&files, "new"), [folder.path("new.txt")]);
    }

    #[test]
    fn a_snapshot_finds_files_before_any_walk() {
        let folder = Scratch::new("snapshot").with(&["kept.txt"]);
        let snapshot = Scratch::new("snapshot-file");
        let path = snapshot.0.join("hyalo").join("files.idx");
        let (first, _) = walking(&folder, Some(path.clone()));
        run(&first);
        assert!(path.exists(), "saved");
        fs::remove_file(folder.0.join("kept.txt")).expect("remove");
        let (files, writer) = Files::new();
        let before_walking = Arc::new(Mutex::new(Vec::new()));
        let (watched, seen) = (files.clone(), Arc::clone(&before_walking));
        let roots: Roots = Arc::new(move || {
            *seen.lock().expect("seen") = search(&watched, "kept");
            vec![]
        });
        run(&provider(writer, roots, Some(path)));
        let seen = before_walking.lock().expect("seen").clone();
        assert_eq!(seen, [folder.path("kept.txt")]);
    }

    #[test]
    fn a_snapshot_it_cannot_read_is_walked_again_and_saved_over() {
        let folder = Scratch::new("bad-snapshot").with(&["fresh.txt"]);
        let snapshot = Scratch::new("bad-snapshot-file").with(&["files.idx"]);
        let path = snapshot.0.join("files.idx");
        let (provider, files) = walking(&folder, Some(path.clone()));
        run(&provider);
        assert_eq!(search(&files, "fresh"), [folder.path("fresh.txt")]);
        let mut saved = BufReader::new(File::open(&path).expect("saved"));
        assert!(Index::load(&mut saved).is_ok());
    }

    #[test]
    fn a_run_while_another_walks_does_nothing() {
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
        let provider = provider(writer, roots, None);
        let first = Arc::clone(&provider);
        let walker = thread::spawn(move || run(&first));
        shared.0.wait();
        assert_eq!(run(&provider), 0, "busy");
        shared.1.wait();
        walker.join().expect("the first walk");
        assert_eq!(search(&files, "x"), [folder.path("x.txt")]);
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
