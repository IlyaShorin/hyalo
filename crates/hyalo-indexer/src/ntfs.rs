//! The real volume: each of `Disk`'s needs is a Windows call or two on a
//! handle to the volume, which only an administrator, or the system, may
//! open for reading. What each call gives is read by tested code (here,
//! and in `mft`, `usn`, `directory`, `read`); of the calls themselves, a
//! test can make only those Windows answers through a folder's handle too
//! (the volume's geometry, its journal, a folder's entries): the others
//! are checked by reading them, and live, once the service is installed.

use std::io;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use hyalo_core::volume::Journal;
use windows::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_JOURNAL_NOT_ACTIVE, ERROR_NO_MORE_FILES, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdType, FlushFileBuffers,
    GetFileInformationByHandleEx, OPEN_EXISTING, OpenFileById, SYNCHRONIZE,
};
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_GET_NTFS_FILE_RECORD,
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    NTFS_VOLUME_DATA_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::core::HSTRING;

use crate::directory;
use crate::disk::{Disk, Geometry};
use crate::mft;
use crate::overlapped::{Owned, bytes_mut, bytes_of, control, control_into, os, read_at};
use crate::usn;

/// How long changes gather between two reads of the journal, so a burst
/// comes in one batch.
const GATHER: Duration = Duration::from_millis(100);

/// The journal made for a volume that has none: as big as Windows makes
/// the system drive's.
const JOURNAL_SIZE: u64 = 32 << 20;
const JOURNAL_GROWTH: u64 = 8 << 20;

/// Room for one read of the journal.
const JOURNAL_ROOM: usize = 64 << 10;

/// Room for a folder's entries, read a roomful at a time.
const ENTRIES_ROOM: usize = 64 << 10;

/// A page of memory: what a read of the volume, which may bypass the
/// cache, needs its buffer aligned to.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
struct Page([u8; 4096]);

/// An NTFS volume, open for reading with the system's rights.
pub struct Ntfs {
    volume: Owned,
    path: HSTRING,
    geometry: Geometry,
}

impl Ntfs {
    /// The volume of the drive `drive` (`C`), if it is NTFS and may be
    /// opened.
    pub fn open(drive: char) -> io::Result<Ntfs> {
        Ntfs::at(&format!(r"\\.\{drive}:"))
    }

    /// The volume through the handle of `path`: the volume itself, or in
    /// tests a folder on it, which answers some of what the volume does.
    fn at(path: &str) -> io::Result<Ntfs> {
        let path = HSTRING::from(path);
        let volume = open(&path, FILE_GENERIC_READ.0)?;
        let data = control_into(volume.0, FSCTL_GET_NTFS_VOLUME_DATA, &[])?;
        Ok(Ntfs {
            geometry: geometry_of(&data),
            volume,
            path,
        })
    }

    /// What the journal is now.
    fn query(&self) -> io::Result<Journal> {
        control_into(self.volume.0, FSCTL_QUERY_USN_JOURNAL, &[]).map(|data| journal_of(&data))
    }

    /// Makes a journal for the volume, through a handle that may change it.
    fn create_journal(&self) -> io::Result<()> {
        let size = CREATE_USN_JOURNAL_DATA {
            MaximumSize: JOURNAL_SIZE,
            AllocationDelta: JOURNAL_GROWTH,
        };
        let writable = open(&self.path, FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0)?;
        let input = bytes_of(&size);
        control(writable.0, FSCTL_CREATE_USN_JOURNAL, input, &mut [], None).map(drop)
    }

    /// The folder of the file reference `reference`, open for listing.
    fn open_by_id(&self, reference: u64) -> io::Result<Owned> {
        let id = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: reference as i64,
            },
        };
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let access = FILE_LIST_DIRECTORY.0 | SYNCHRONIZE.0;
        let flags = FILE_FLAG_BACKUP_SEMANTICS;
        // SAFETY: a plain call; the handle it gives is closed by `Owned`.
        let folder = unsafe { OpenFileById(self.volume.0, &id, access, share, None, flags) };
        folder.map(Owned).map_err(os)
    }
}

impl Disk for Ntfs {
    fn geometry(&self) -> io::Result<Geometry> {
        Ok(self.geometry)
    }

    fn read(&self, at: u64, length: usize) -> io::Result<Vec<u8>> {
        let mut pages = vec![Page([0; 4096]); length.div_ceil(4096)];
        // SAFETY: the pages hold at least `length` bytes, and are not used
        // otherwise while `buffer` lives.
        let buffer = unsafe { std::slice::from_raw_parts_mut(pages.as_mut_ptr().cast(), length) };
        let read = read_at(self.volume.0, at, &mut *buffer, None)?;
        whole(read, length).map(|()| buffer.to_vec())
    }

    fn record(&self, number: u32) -> io::Result<Option<Vec<u8>>> {
        let mut room = vec![0_u64; (16 + self.geometry.record as usize).div_ceil(8)];
        let input = i64::from(number).to_le_bytes();
        let output = bytes_mut(&mut room);
        let asked = control(
            self.volume.0,
            FSCTL_GET_NTFS_FILE_RECORD,
            &input,
            output,
            None,
        );
        let written = answered(asked)?;
        Ok(file_record(&bytes_mut(&mut room)[..written], number))
    }

    fn journal(&self) -> io::Result<Journal> {
        made_if_missing(|| self.query(), || self.create_journal())
    }

    fn flush(&self) -> io::Result<()> {
        let writable = open(&self.path, FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0)?;
        // SAFETY: a plain call on a handle that is ours.
        unsafe { FlushFileBuffers(writable.0) }.map_err(os)
    }

    fn changes(&self, from: i64, journal: u64, stop: &AtomicBool) -> io::Result<Option<Vec<u8>>> {
        let asked = reading(from, journal);
        let mut room = vec![0_u64; JOURNAL_ROOM / 8];
        let output = bytes_mut(&mut room);
        let code = FSCTL_READ_USN_JOURNAL;
        let read = control(self.volume.0, code, bytes_of(&asked), output, Some(stop))?;
        Ok(read.map(|length| {
            thread::sleep(GATHER);
            bytes_mut(&mut room)[..length].to_vec()
        }))
    }

    fn children(&self, record: u32) -> io::Result<Vec<u32>> {
        let found = self.reference_of(record)?;
        found.map_or(Ok(Vec::new()), |reference| self.listed(reference))
    }
}

impl Ntfs {
    /// The reference that opens the file of the record `record`, if it is
    /// in use.
    fn reference_of(&self, record: u32) -> io::Result<Option<u64>> {
        let bytes = self.record(record);
        bytes.map(|bytes| bytes.and_then(|bytes| reference(record, &bytes)))
    }

    /// The records of what the folder of `reference` holds.
    fn listed(&self, reference: u64) -> io::Result<Vec<u32>> {
        self.open_by_id(reference)
            .and_then(|folder| entries(&folder))
    }
}

/// Opens `path` for `access`, overlapped, shared with everyone; a folder
/// as well as a volume.
fn open(path: &HSTRING, access: u32) -> io::Result<Owned> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let flags = FILE_FLAG_OVERLAPPED | FILE_FLAG_BACKUP_SEMANTICS;
    // SAFETY: a plain call; the handle it gives is closed by `Owned`.
    let handle = unsafe { CreateFileW(path, access, share, None, OPEN_EXISTING, flags, None) };
    handle.map(Owned).map_err(os)
}

/// How the volume lies, as `FSCTL_GET_NTFS_VOLUME_DATA` says.
fn geometry_of(data: &NTFS_VOLUME_DATA_BUFFER) -> Geometry {
    Geometry {
        serial: data.VolumeSerialNumber as u64,
        cluster: u64::from(data.BytesPerCluster),
        record: u64::from(data.BytesPerFileRecordSegment),
        table: data.MftStartLcn as u64,
        valid: data.MftValidDataLength as u64,
    }
}

/// The journal, as `FSCTL_QUERY_USN_JOURNAL` says: the first place still
/// read is the later of the first kept and the first valid.
fn journal_of(data: &USN_JOURNAL_DATA_V0) -> Journal {
    Journal {
        id: data.UsnJournalID,
        lowest: data.FirstUsn.max(data.LowestValidUsn),
        next: data.NextUsn,
    }
}

/// The volume's journal from `query`, made by `create` first if the volume
/// has none.
fn made_if_missing(
    query: impl Fn() -> io::Result<Journal>,
    create: impl Fn() -> io::Result<()>,
) -> io::Result<Journal> {
    match query() {
        Err(error) if error.raw_os_error() == Some(ERROR_JOURNAL_NOT_ACTIVE.0 as i32) => {
            create().and_then(|()| query())
        }
        other => other,
    }
}

/// What `FSCTL_GET_NTFS_FILE_RECORD` wrote: none for a number past the
/// table's end, which Windows refuses.
fn answered(asked: io::Result<Option<usize>>) -> io::Result<usize> {
    match asked {
        Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER.0 as i32) => Ok(0),
        other => other.map(|written| written.unwrap_or(0)),
    }
}

/// The record `number` in what `FSCTL_GET_NTFS_FILE_RECORD` answered (its
/// reference, its length, then the record), with its sectors' ends put
/// back if NTFS left them out; none if Windows gave the last record in
/// use before it, as it does for one not in use.
fn file_record(answer: &[u8], number: u32) -> Option<Vec<u8>> {
    let given = u64::from_le_bytes(answer.get(..8)?.try_into().ok()?);
    let length = u32::from_le_bytes(answer.get(8..12)?.try_into().ok()?) as usize;
    let mut record = answer.get(12..12 + length)?.to_vec();
    // NTFS hands records back whole; one that is not is put right, and
    // either way reads the same.
    mft::fix(&mut record);
    ((given & 0xFFFF_FFFF_FFFF) == u64::from(number)).then_some(record)
}

/// The reference that opens the file of the record `record`, whose bytes
/// are `bytes`: its number and its sequence.
fn reference(record: u32, bytes: &[u8]) -> Option<u64> {
    let read = mft::parse(bytes)?;
    Some(u64::from(record) | u64::from(read.sequence) << 48)
}

/// Whether a read gave all `length` bytes asked for.
fn whole(read: Option<usize>, length: usize) -> io::Result<()> {
    match read == Some(length) {
        true => Ok(()),
        false => Err(io::ErrorKind::UnexpectedEof.into()),
    }
}

/// A read of the journal from `from` on, of the changes that matter.
fn reading(from: i64, journal: u64) -> READ_USN_JOURNAL_DATA_V0 {
    READ_USN_JOURNAL_DATA_V0 {
        StartUsn: from,
        ReasonMask: usn::REASONS,
        ReturnOnlyOnClose: 0,
        Timeout: 0,
        BytesToWaitFor: 1,
        UsnJournalID: journal,
    }
}

/// The records of what the open folder `folder` holds.
fn entries(folder: &Owned) -> io::Result<Vec<u32>> {
    let mut room = vec![0_u64; ENTRIES_ROOM / 8];
    let mut records = Vec::new();
    let mut class = FileIdBothDirectoryRestartInfo;
    while let Some(more) = list(folder.0, class, &mut room)? {
        records.extend(directory::records(more));
        class = FileIdBothDirectoryInfo;
    }
    Ok(records)
}

/// The next roomful of a folder's entries; none once there are no more.
fn list(
    folder: HANDLE,
    class: FILE_INFO_BY_HANDLE_CLASS,
    room: &mut [u64],
) -> io::Result<Option<&[u8]>> {
    let length = size_of_val(room) as u32;
    // SAFETY: the room is ours, as long as said, for this call alone.
    let listed =
        unsafe { GetFileInformationByHandleEx(folder, class, room.as_mut_ptr().cast(), length) };
    match listed {
        Ok(()) => Ok(Some(bytes_mut(room))),
        Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => Ok(None),
        Err(error) => Err(os(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mft::build::record;
    use crate::overlapped::tests::Scratch;
    use std::fs;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    /// The system drive's root, which answers what the volume says of
    /// itself to anyone, not only to an administrator.
    fn system_drive() -> Ntfs {
        let drive = std::env::var("SystemDrive").expect("SystemDrive is set");
        Ntfs::at(&format!(r"{drive}\")).expect("the system drive is NTFS")
    }

    #[test]
    fn the_system_drive_says_how_it_lies_and_where_its_journal_is() {
        let ntfs = system_drive();
        let geometry = ntfs.geometry().expect("its geometry");
        assert!(geometry.cluster >= 512 && geometry.cluster.is_power_of_two());
        assert!([1024, 4096].contains(&geometry.record), "{geometry:?}");
        assert!(geometry.valid >= 16 * geometry.record);
        let journal = ntfs
            .journal()
            .expect("Windows keeps one on the system drive");
        assert!(
            journal.lowest <= journal.next && journal.next > 0,
            "{journal:?}"
        );
    }

    #[test]
    fn a_folder_is_no_volume_to_read() {
        assert!(system_drive().read(0, 4096).is_err());
        assert!(Ntfs::at(r"C:\no such folder here").is_err());
    }

    /// The record of the file at `path`, as Windows numbers it.
    fn record_of(path: &std::path::Path) -> u32 {
        let path = HSTRING::from(path.as_os_str());
        let opened = open(&path, FILE_GENERIC_READ.0).expect("opened");
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: a plain call on a handle that is ours.
        unsafe { GetFileInformationByHandle(opened.0, &mut information).expect("its information") };
        information.nFileIndexLow
    }

    #[test]
    fn a_folder_lists_the_records_of_its_files_and_folders() {
        let scratch = Scratch::new("entries");
        fs::write(scratch.0.join("a.txt"), b"a").expect("written");
        fs::create_dir(scratch.0.join("sub")).expect("made");
        let path = HSTRING::from(scratch.0.as_os_str());
        let folder = open(&path, FILE_LIST_DIRECTORY.0 | SYNCHRONIZE.0).expect("opened");
        let mut listed = entries(&folder).expect("listed");
        listed.sort_unstable();
        let a = record_of(&scratch.0.join("a.txt"));
        let mut expected = [a, record_of(&scratch.0.join("sub"))];
        expected.sort_unstable();
        assert_eq!(listed, expected);
    }

    #[test]
    fn a_journal_missing_is_made_then_read_and_one_there_is_read() {
        let made = std::cell::Cell::new(0);
        let journal = Journal {
            id: 7,
            lowest: 0,
            next: 9,
        };
        let query = || match made.get() {
            0 => Err(io::Error::from_raw_os_error(
                ERROR_JOURNAL_NOT_ACTIVE.0 as i32,
            )),
            _ => Ok(journal),
        };
        let create = || {
            made.set(made.get() + 1);
            Ok(())
        };
        assert_eq!(made_if_missing(query, create).expect("made"), journal);
        assert_eq!(made.get(), 1);
        assert_eq!(made_if_missing(query, create).expect("there"), journal);
        assert_eq!(made.get(), 1, "made once");
        let refused = || Err(io::ErrorKind::PermissionDenied.into());
        assert!(made_if_missing(refused, create).is_err());
    }

    #[test]
    fn a_number_past_the_table_s_end_answers_nothing() {
        let invalid = io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER.0 as i32);
        assert_eq!(answered(Err(invalid)).expect("nothing"), 0);
        assert_eq!(answered(Ok(Some(44))).expect("written"), 44);
        assert_eq!(answered(Ok(None)).expect("nothing"), 0);
        assert!(answered(Err(io::ErrorKind::PermissionDenied.into())).is_err());
    }

    /// What `FSCTL_GET_NTFS_FILE_RECORD` answers for `given`'s record.
    fn answer(given: u64, record: &[u8]) -> Vec<u8> {
        let mut answer = given.to_le_bytes().to_vec();
        answer.extend((record.len() as u32).to_le_bytes());
        answer.extend(record);
        answer
    }

    #[test]
    fn a_record_answered_is_the_one_asked_for_or_none() {
        let made = record().name(5, "a.txt", 1);
        let bytes = made.bytes(1024);
        let ours = 40 | 3 << 48;
        assert_eq!(file_record(&answer(ours, &bytes), 40), Some(bytes.clone()));
        let on_disk = made.on_disk(1024);
        assert_eq!(
            file_record(&answer(ours, &on_disk), 40),
            Some(bytes.clone()),
            "put right"
        );
        let earlier = answer(39 | 3 << 48, &bytes);
        assert_eq!(file_record(&earlier, 40), None, "an earlier one");
        assert_eq!(file_record(&answer(ours, &bytes)[..100], 40), None, "cut");
        assert_eq!(file_record(&[], 40), None);
    }

    #[test]
    fn a_file_is_opened_by_its_number_and_sequence() {
        let bytes = record().sequence(9).bytes(1024);
        assert_eq!(reference(40, &bytes), Some(40 | 9 << 48));
        assert_eq!(reference(40, &[0; 1024]), None);
    }

    #[test]
    fn a_read_is_whole_or_an_error() {
        assert!(whole(Some(4096), 4096).is_ok());
        assert!(whole(Some(512), 4096).is_err());
        assert!(whole(None, 4096).is_err());
    }

    #[test]
    fn the_journal_is_read_for_the_changes_that_matter_once_there_are_any() {
        let asked = reading(4096, 7);
        assert_eq!((asked.StartUsn, asked.UsnJournalID), (4096, 7));
        assert_eq!((asked.ReasonMask, asked.BytesToWaitFor), (usn::REASONS, 1));
        assert_eq!((asked.ReturnOnlyOnClose, asked.Timeout), (0, 0));
    }

    #[test]
    fn the_journal_starts_where_both_its_first_and_its_first_valid_place_allow() {
        let data = USN_JOURNAL_DATA_V0 {
            UsnJournalID: 3,
            FirstUsn: 100,
            NextUsn: 900,
            LowestValidUsn: 200,
            ..Default::default()
        };
        let expected = Journal {
            id: 3,
            lowest: 200,
            next: 900,
        };
        assert_eq!(journal_of(&data), expected);
    }
}
