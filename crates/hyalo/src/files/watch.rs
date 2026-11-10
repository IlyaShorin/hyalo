//! Watching a root: Windows tells of every name added, removed or moved
//! anywhere under it, with its attributes, and the watch hands each batch
//! to the files worker as records. No admin rights: this is the
//! directory's own change notification, not the NTFS journal.

use std::mem::offset_of;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
    FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_EXTENDED_INFORMATION, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesExW,
    ReadDirectoryNotifyExtendedInformation,
};
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::core::{HRESULT, HSTRING};

use super::changes::{Action, Kind, Record};
use super::root_name;

/// What a watch tells the files worker.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// What changed under the root, in the order it happened.
    Changed(Vec<Record>),
    /// The watch lost track: its buffer overflowed, or it had to open the
    /// root again. What changed meanwhile is not known.
    Lost,
}

/// Room for the changes Windows holds between two reads: a burst bigger
/// than this is lost track of.
const BUFFER: usize = 1 << 20;

/// What the watch asks to be told of: names, of files and of folders.
const NAMES: FILE_NOTIFY_CHANGE =
    FILE_NOTIFY_CHANGE(FILE_NOTIFY_CHANGE_FILE_NAME.0 | FILE_NOTIFY_CHANGE_DIR_NAME.0);

/// How long a watch waits before it looks whether it is to stop, and
/// before it tries again to open a root it could not.
const PATIENCE: Duration = Duration::from_millis(500);

/// Where a watch sends what it sees; false once nobody listens.
pub type Tell = Box<dyn Fn(Event) -> bool + Send>;

/// A watch on one root, on a thread of its own until it is dropped.
pub struct Watch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    /// Watches `root`, telling `tell` what it sees; back once the watch
    /// is set, so whatever changes after is told of.
    pub fn start(root: &Path, tell: Tell) -> Watch {
        Watch::with_room(root, tell, BUFFER)
    }

    fn with_room(root: &Path, tell: Tell, room: usize) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let (root, halt) = (root.to_path_buf(), Arc::clone(&stop));
        let (set, ready) = mpsc::channel::<()>();
        let thread = thread::spawn(move || watch(&root, &*tell, &halt, room, set));
        // The watch drops its end once it is set, or could not be.
        let _ = ready.recv();
        Watch {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Follows `root` until told to stop, opening it again whenever the watch
/// breaks; every opening but a first one that worked says `Lost`. `set`
/// is dropped once the first read waits for changes, or the first try
/// failed.
fn watch(
    root: &Path,
    tell: &dyn Fn(Event) -> bool,
    stop: &AtomicBool,
    room: usize,
    set: Sender<()>,
) {
    let top = root_name(root);
    let mut buffer = vec![0_u64; room.div_ceil(8)];
    let (mut lost, mut set) = (false, Some(set));
    while !stop.load(Ordering::SeqCst) {
        let Ok(directory) = Directory::open(root) else {
            (lost, set) = (true, None);
            thread::sleep(PATIENCE);
            continue;
        };
        if lost && !tell(Event::Lost) {
            return;
        }
        lost = true;
        let ended = directory.follow(&top, &mut buffer, tell, stop, &mut set);
        set = None;
        match ended {
            Ended::Broken => thread::sleep(PATIENCE),
            Ended::Stopped | Ended::Refused => return,
        }
    }
}

/// How following a directory ended.
enum Ended {
    /// Told to stop, or nobody listens any more.
    Stopped,
    /// It broke after it worked: open it again.
    Broken,
    /// It cannot be watched at all.
    Refused,
}

/// An open directory and the event its reads signal.
struct Directory {
    handle: HANDLE,
    event: HANDLE,
}

impl Directory {
    fn open(root: &Path) -> windows::core::Result<Directory> {
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED;
        let path = HSTRING::from(root.as_os_str());
        // SAFETY: plain calls; each handle made is closed by `Drop`, the
        // first alone if the second is refused.
        unsafe {
            let handle = CreateFileW(
                &path,
                FILE_LIST_DIRECTORY.0,
                share,
                None,
                OPEN_EXISTING,
                flags,
                None,
            )?;
            match CreateEventW(None, true, false, None) {
                Ok(event) => Ok(Directory { handle, event }),
                Err(error) => {
                    let _ = CloseHandle(handle);
                    Err(error)
                }
            }
        }
    }

    /// Reads change after change into `buffer`, sending what each read
    /// held, until told to stop or the directory fails.
    fn follow(
        &self,
        top: &str,
        buffer: &mut [u64],
        tell: &dyn Fn(Event) -> bool,
        stop: &AtomicBool,
        set: &mut Option<Sender<()>>,
    ) -> Ended {
        let mut worked = false;
        loop {
            match self.read(top, buffer, tell, stop, set) {
                Ok(()) => worked = true,
                // Refused after it worked: broken, so tried again.
                Err(Ended::Refused) if worked => return Ended::Broken,
                Err(ended) => return ended,
            }
        }
    }

    /// Reads one batch of changes into `buffer` and tells of it.
    fn read(
        &self,
        top: &str,
        buffer: &mut [u64],
        tell: &dyn Fn(Event) -> bool,
        stop: &AtomicBool,
        set: &mut Option<Sender<()>>,
    ) -> Result<(), Ended> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        // SAFETY: the buffer and `overlapped` outlive the read: `wait`
        // sees it end, or cancels it and sees it end, before either is
        // touched again.
        let read = unsafe {
            ReadDirectoryChangesExW(
                self.handle,
                buffer.as_mut_ptr().cast(),
                size_of_val(buffer) as u32,
                true,
                NAMES,
                None,
                Some(&raw mut overlapped),
                None,
                ReadDirectoryNotifyExtendedInformation,
            )
        };
        read.map_err(|_| Ended::Refused)?;
        *set = None;
        let event = match self.wait(&raw const overlapped, stop) {
            Waited::Read(0) | Waited::Overflowed => Event::Lost,
            Waited::Read(length) => Event::Changed(records(&bytes(buffer)[..length], top)),
            Waited::Stopped => return Err(Ended::Stopped),
            Waited::Failed => return Err(Ended::Broken),
        };
        let nothing = event == Event::Changed(Vec::new());
        match nothing || tell(event) {
            true => Ok(()),
            false => Err(Ended::Stopped),
        }
    }

    /// Waits for the read issued with `overlapped` to end.
    fn wait(&self, overlapped: *const OVERLAPPED, stop: &AtomicBool) -> Waited {
        let timeout = HRESULT::from_win32(WAIT_TIMEOUT.0);
        loop {
            let mut length = 0;
            // SAFETY: the read was issued on this handle with `overlapped`.
            let result = unsafe {
                GetOverlappedResultEx(
                    self.handle,
                    overlapped,
                    &mut length,
                    PATIENCE.as_millis() as u32,
                    false,
                )
            };
            match result {
                Ok(()) => return Waited::Read(length as usize),
                Err(error) if error.code() == timeout => {
                    if stop.load(Ordering::SeqCst) {
                        self.cancel(overlapped);
                        return Waited::Stopped;
                    }
                }
                Err(error) if error.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => {
                    return Waited::Overflowed;
                }
                Err(_) => return Waited::Failed,
            }
        }
    }

    /// Cancels the read issued with `overlapped` and waits for it to end,
    /// so nothing writes into the buffer after.
    fn cancel(&self, overlapped: *const OVERLAPPED) {
        let mut length = 0;
        // SAFETY: the read is ours, on this handle.
        unsafe {
            let _ = CancelIoEx(self.handle, Some(overlapped));
            let _ = GetOverlappedResult(self.handle, overlapped, &mut length, true);
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: both handles are ours, closed once, with no read left
        // on them: `follow` ends only after its read did.
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// How waiting for a read ended.
enum Waited {
    /// It holds this many bytes; none when Windows had more than room for.
    Read(usize),
    Overflowed,
    Stopped,
    Failed,
}

/// The buffer's bytes.
fn bytes(buffer: &[u64]) -> &[u8] {
    // SAFETY: u64s are bytes, eight each, with no padding; u8 needs no
    // alignment.
    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), size_of_val(buffer)) }
}

/// Where a record's name starts.
const NAME: usize = offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileName);

/// The records a read put in `bytes`, their names joined to `top`, the
/// root's name in the index. A record with an action hyalo does not know,
/// or a name that is not valid Unicode, which the walk leaves out too, is
/// skipped; a record cut short ends the list.
fn records(bytes: &[u8], top: &str) -> Vec<Record> {
    let mut found = Vec::new();
    let mut start = 0;
    while let Some((record, next)) = bytes.get(start..).and_then(|rest| record(rest, top)) {
        found.extend(record);
        if next == 0 {
            break;
        }
        start += next;
    }
    found
}

/// The record at the start of `entry`, if hyalo knows its kind, and how
/// far on the next one starts (0: none); `None` if it is cut short.
fn record(entry: &[u8], top: &str) -> Option<(Option<Record>, usize)> {
    let field = |at: usize| Some(u32::from_ne_bytes(*entry.get(at..at + 4)?.as_array()?));
    let next = field(offset_of!(
        FILE_NOTIFY_EXTENDED_INFORMATION,
        NextEntryOffset
    ))?;
    let action = field(offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, Action))?;
    let attributes = field(offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileAttributes))?;
    let tag = field(offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, Anonymous))?;
    let length = field(offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileNameLength))? as usize;
    let name = entry.get(NAME..NAME + length)?.as_chunks::<2>().0;
    let name = String::from_utf16(
        &name
            .iter()
            .map(|&unit| u16::from_ne_bytes(unit))
            .collect::<Vec<_>>(),
    );
    let record = action_of(FILE_ACTION(action))
        .zip(name.ok())
        .map(|(action, name)| Record {
            action,
            path: format!("{top}\\{name}"),
            kind: Kind::of(attributes, tag),
        });
    Some((record, next as usize))
}

fn action_of(action: FILE_ACTION) -> Option<Action> {
    match action {
        FILE_ACTION_ADDED => Some(Action::Added),
        FILE_ACTION_REMOVED => Some(Action::Removed),
        FILE_ACTION_MODIFIED => Some(Action::Modified),
        FILE_ACTION_RENAMED_OLD_NAME => Some(Action::MovedFrom),
        FILE_ACTION_RENAMED_NEW_NAME => Some(Action::MovedTo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::fs;
    use std::sync::mpsc::Receiver;

    /// A watch on `root`, and what it sees.
    fn watching(root: &Path) -> (Watch, Receiver<Event>) {
        let (sender, events) = mpsc::channel();
        let watch = Watch::start(root, Box::new(move |event| sender.send(event).is_ok()));
        (watch, events)
    }
    use std::time::Instant;

    /// One record as Windows writes it: the fields, the name, then room
    /// up to the next eight bytes; `last` ends the list.
    fn written(
        action: FILE_ACTION,
        attributes: u32,
        tag: u32,
        name: &[u16],
        last: bool,
    ) -> Vec<u8> {
        let length = (NAME + 2 * name.len()).next_multiple_of(8);
        let mut bytes = vec![0_u8; length];
        let mut put =
            |at: usize, value: u32| bytes[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        put(
            offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, NextEntryOffset),
            if last { 0 } else { length as u32 },
        );
        put(
            offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, Action),
            action.0,
        );
        put(
            offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileAttributes),
            attributes,
        );
        put(offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, Anonymous), tag);
        put(
            offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileNameLength),
            2 * name.len() as u32,
        );
        for (at, unit) in name.iter().enumerate() {
            bytes[NAME + 2 * at..NAME + 2 * at + 2].copy_from_slice(&unit.to_ne_bytes());
        }
        bytes
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn a_read_holds_records_each_named_under_the_root() {
        let bytes = [
            written(FILE_ACTION_ADDED, 0x10, 0, &utf16(r"Users\новая"), false),
            written(
                FILE_ACTION_RENAMED_OLD_NAME,
                0x20,
                0,
                &utf16("a.txt"),
                false,
            ),
            written(
                FILE_ACTION_RENAMED_NEW_NAME,
                0x410,
                0xA000_0003,
                &utf16("b"),
                true,
            ),
        ]
        .concat();
        let found = records(&bytes, "C:");
        let record = |action, path: &str, kind| Record {
            action,
            path: path.to_string(),
            kind,
        };
        assert_eq!(
            found,
            [
                record(Action::Added, r"C:\Users\новая", Kind::Folder),
                record(Action::MovedFrom, r"C:\a.txt", Kind::File),
                record(Action::MovedTo, r"C:\b", Kind::Link),
            ]
        );
    }

    #[test]
    fn records_hyalo_cannot_read_are_skipped_and_one_cut_short_ends_the_list() {
        let bytes = [
            written(FILE_ACTION(9), 0, 0, &utf16("odd"), false),
            written(FILE_ACTION_REMOVED, 0, 0, &[0xD800], false),
            written(FILE_ACTION_MODIFIED, 0, 0, &utf16("kept"), false),
            written(FILE_ACTION_REMOVED, 0, 0, &utf16("cut short"), true),
        ]
        .concat();
        let found = records(&bytes[..bytes.len() - 4], "D:");
        let paths: Vec<_> = found
            .iter()
            .map(|record| (record.action, record.path.as_str()))
            .collect();
        assert_eq!(paths, [(Action::Modified, r"D:\kept")]);
        assert!(records(&[], "D:").is_empty());
    }

    /// What `events` brings within ten seconds, until `enough` holds of it.
    fn gathered(events: &Receiver<Event>, enough: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while !enough(&seen) && Instant::now() < deadline {
            if let Ok(event) = events.recv_timeout(Duration::from_millis(50)) {
                seen.push(event);
            }
        }
        seen
    }

    fn records_of(events: &[Event]) -> Vec<(Action, String, Kind)> {
        let records = events.iter().flat_map(|event| match event {
            Event::Changed(records) => records.clone(),
            Event::Lost => Vec::new(),
        });
        records
            .map(|record| (record.action, record.path, record.kind))
            .collect()
    }

    #[test]
    fn a_watch_tells_of_names_made_moved_and_removed_anywhere_below() {
        let folder = Scratch::new("watch").with(&["deep\\", r"deep\old.txt"]);
        let (watch, events) = watching(&folder.0);
        let path = |name: &str| folder.0.join(name);
        fs::create_dir(path(r"deep\made")).expect("make a folder");
        fs::rename(path(r"deep\old.txt"), path(r"deep\new.txt")).expect("rename");
        fs::rename(path(r"deep\new.txt"), path(r"deep\made\new.txt")).expect("move");
        fs::remove_file(path(r"deep\made\new.txt")).expect("remove");
        // A rename in its folder is one; a move to another is two.
        let expected = [
            (Action::Added, folder.path(r"deep\made"), Kind::Folder),
            (Action::MovedFrom, folder.path(r"deep\old.txt"), Kind::File),
            (Action::MovedTo, folder.path(r"deep\new.txt"), Kind::File),
            (Action::Removed, folder.path(r"deep\new.txt"), Kind::File),
            (Action::Added, folder.path(r"deep\made\new.txt"), Kind::File),
            (
                Action::Removed,
                folder.path(r"deep\made\new.txt"),
                Kind::File,
            ),
        ];
        let seen = gathered(&events, |seen| records_of(seen).len() >= expected.len());
        drop(watch);
        assert_eq!(records_of(&seen), expected);
    }

    #[test]
    fn a_watch_that_falls_behind_says_it_lost_track() {
        let folder = Scratch::new("watch-overflow");
        let (sender, events) = mpsc::channel();
        let (stalled, stall) = mpsc::channel::<()>();
        let stall = std::sync::Mutex::new(stall);
        // The first event waits until the test lets it go: what happens
        // meanwhile piles up in Windows' room for it, and spills over.
        let tell = move |event| {
            let _ = stall
                .lock()
                .expect("the stall")
                .recv_timeout(Duration::from_secs(10));
            sender.send(event).is_ok()
        };
        let watch = Watch::with_room(&folder.0, Box::new(tell), 4096);
        for n in 0..200 {
            let name = format!("a file with a name long enough to fill the room {n:03}.txt");
            fs::write(folder.0.join(name), b"x").expect("write");
        }
        drop(stalled);
        let seen = gathered(&events, |seen| seen.contains(&Event::Lost));
        drop(watch);
        assert!(seen.contains(&Event::Lost), "{} events", seen.len());
    }

    #[test]
    fn a_root_made_again_after_it_went_is_watched_again_and_lost_track_of() {
        let folder = Scratch::new("watch-again");
        let (watch, events) = watching(&folder.0);
        fs::remove_dir(&folder.0).expect("remove the root");
        // Long enough for the watch to break and fail to open it again.
        thread::sleep(3 * PATIENCE);
        fs::create_dir(&folder.0).expect("make it again");
        let seen = gathered(&events, |seen| seen.contains(&Event::Lost));
        fs::write(folder.0.join("after.txt"), b"x").expect("write");
        let after = gathered(&events, |seen| !records_of(seen).is_empty());
        drop(watch);
        assert!(seen.contains(&Event::Lost), "{seen:?}");
        assert_eq!(
            records_of(&after),
            [(Action::Added, folder.path("after.txt"), Kind::File)]
        );
    }
}
