//! The real volume: each of `Disk`'s needs is a Windows call or two on a
//! handle to the volume, which only an administrator, or the system, may
//! open. Not tested: no test runs with those rights. What each call gives
//! is read by tested code (`mft`, `usn`, `directory`, `read`); the calls
//! themselves are checked by reading them, and live, once the service is
//! installed.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use hyalo_core::volume::Journal;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_IO_PENDING, ERROR_JOURNAL_NOT_ACTIVE,
    ERROR_NO_MORE_FILES, HANDLE, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdType, FlushFileBuffers,
    GetFileInformationByHandleEx, OPEN_EXISTING, OpenFileById, ReadFile, SYNCHRONIZE,
};
use windows::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
    OVERLAPPED_0, OVERLAPPED_0_0,
};
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_GET_NTFS_FILE_RECORD,
    FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    NTFS_VOLUME_DATA_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::core::{HRESULT, HSTRING};

use crate::directory;
use crate::disk::{Disk, Geometry};
use crate::mft;
use crate::usn;

/// How long a wait goes before it looks whether it is to stop.
const PATIENCE: Duration = Duration::from_millis(500);

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

/// A handle Windows gave, closed once when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

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

// SAFETY: a volume handle may be used from any thread, and from several at
// once: each read and control carries its own `OVERLAPPED` and event.
unsafe impl Send for Ntfs {}
// SAFETY: as above; nothing in `Ntfs` changes after it is opened.
unsafe impl Sync for Ntfs {}

impl Ntfs {
    /// The volume of the drive `drive` (`C`), if it is NTFS and may be
    /// opened.
    pub fn open(drive: char) -> io::Result<Ntfs> {
        let path = HSTRING::from(format!(r"\\.\{drive}:"));
        let volume = Owned(open(&path, FILE_GENERIC_READ.0, FILE_FLAG_OVERLAPPED)?);
        let unknown = Geometry {
            serial: 0,
            cluster: 0,
            record: 0,
            table: 0,
            valid: 0,
        };
        let mut ntfs = Ntfs {
            volume,
            path,
            geometry: unknown,
        };
        // Refused for a volume that is not NTFS.
        ntfs.geometry = ntfs.read_geometry()?;
        Ok(ntfs)
    }

    fn read_geometry(&self) -> io::Result<Geometry> {
        let data: NTFS_VOLUME_DATA_BUFFER = self.control_into(FSCTL_GET_NTFS_VOLUME_DATA, &[])?;
        Ok(Geometry {
            serial: data.VolumeSerialNumber as u64,
            cluster: u64::from(data.BytesPerCluster),
            record: u64::from(data.BytesPerFileRecordSegment),
            table: data.MftStartLcn as u64,
            valid: data.MftValidDataLength as u64,
        })
    }

    /// Issues the control `code` with `input` and waits for it, looking
    /// at `stop` now and then; the bytes it wrote to `output`, none if it
    /// was stopped.
    fn control(
        &self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        stop: Option<&AtomicBool>,
    ) -> io::Result<Option<usize>> {
        let event = event()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: both buffers and `overlapped` outlive the call: `wait`
        // sees it end, or cancels it and sees it end.
        let issued = unsafe {
            DeviceIoControl(
                self.volume.0,
                code,
                Some(input.as_ptr().cast()),
                input.len() as u32,
                Some(output.as_mut_ptr().cast()),
                output.len() as u32,
                None,
                Some(&raw mut overlapped),
            )
        };
        pending(issued)?;
        self.wait(&raw const overlapped, stop)
    }

    /// The structure the control `code` with `input` answers.
    fn control_into<T: Copy>(&self, code: u32, input: &[u8]) -> io::Result<T> {
        let mut room = vec![0_u64; size_of::<T>().div_ceil(8) + 32];
        self.control(code, input, bytes_mut(&mut room), None)?;
        // SAFETY: the room is at least as long as `T`, aligned to 8, and
        // holds what Windows wrote of a `T`, a plain C structure.
        Ok(unsafe { room.as_ptr().cast::<T>().read_unaligned() })
    }

    /// Waits for the call issued with `overlapped` to end.
    fn wait(
        &self,
        overlapped: *const OVERLAPPED,
        stop: Option<&AtomicBool>,
    ) -> io::Result<Option<usize>> {
        let timeout = HRESULT::from_win32(WAIT_TIMEOUT.0);
        loop {
            let mut length = 0;
            // SAFETY: the call was issued on this handle with `overlapped`.
            let result = unsafe {
                GetOverlappedResultEx(
                    self.volume.0,
                    overlapped,
                    &mut length,
                    PATIENCE.as_millis() as u32,
                    false,
                )
            };
            match result {
                Ok(()) => return Ok(Some(length as usize)),
                Err(error) if error.code() == timeout => {
                    if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
                        self.cancel(overlapped);
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Cancels the call issued with `overlapped` and waits for it to end,
    /// so nothing writes to its buffer after.
    fn cancel(&self, overlapped: *const OVERLAPPED) {
        let mut length = 0;
        // SAFETY: the call is ours, on this handle.
        unsafe {
            let _ = CancelIoEx(self.volume.0, Some(overlapped));
            let _ = GetOverlappedResult(self.volume.0, overlapped, &mut length, true);
        }
    }

    /// What the journal is now.
    fn query(&self) -> io::Result<Journal> {
        let data: USN_JOURNAL_DATA_V0 = self.control_into(FSCTL_QUERY_USN_JOURNAL, &[])?;
        Ok(Journal {
            id: data.UsnJournalID,
            lowest: data.FirstUsn.max(data.LowestValidUsn),
            next: data.NextUsn,
        })
    }

    /// Makes a journal for the volume, through a handle that may change it.
    fn create_journal(&self) -> io::Result<()> {
        let writable = Owned(open(
            &self.path,
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_FLAGS_AND_ATTRIBUTES(0),
        )?);
        let size = CREATE_USN_JOURNAL_DATA {
            MaximumSize: JOURNAL_SIZE,
            AllocationDelta: JOURNAL_GROWTH,
        };
        // SAFETY: the input outlives the call, which is synchronous on this
        // handle and writes nothing back.
        unsafe {
            DeviceIoControl(
                writable.0,
                FSCTL_CREATE_USN_JOURNAL,
                Some((&raw const size).cast()),
                size_of::<CREATE_USN_JOURNAL_DATA>() as u32,
                None,
                0,
                None,
                None,
            )?;
        }
        Ok(())
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
        let event = event()?;
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            Anonymous: OVERLAPPED_0 {
                Anonymous: OVERLAPPED_0_0 {
                    Offset: at as u32,
                    OffsetHigh: (at >> 32) as u32,
                },
            },
            ..Default::default()
        };
        // SAFETY: the buffer and `overlapped` outlive the read: `wait`
        // sees it end before either is touched again.
        let issued = unsafe {
            ReadFile(
                self.volume.0,
                Some(&mut *buffer),
                None,
                Some(&raw mut overlapped),
            )
        };
        pending(issued)?;
        match self.wait(&raw const overlapped, None)? {
            Some(read) if read == length => Ok(buffer.to_vec()),
            _ => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }

    fn record(&self, number: u32) -> io::Result<Option<Vec<u8>>> {
        let input = i64::from(number).to_le_bytes();
        let mut room = vec![0_u64; (16 + self.geometry.record as usize).div_ceil(8)];
        let asked = self.control(
            FSCTL_GET_NTFS_FILE_RECORD,
            &input,
            bytes_mut(&mut room),
            None,
        );
        let invalid = ERROR_INVALID_PARAMETER.0 as i32;
        let written = match asked {
            Err(error) if error.raw_os_error() == Some(invalid) => return Ok(None),
            other => other?.unwrap_or(0),
        };
        let answer = &bytes_mut(&mut room)[..written];
        // The record asked for, or the last in use before it.
        let given = answer
            .get(..8)
            .map(|reference| u64::from_le_bytes(reference.try_into().unwrap_or_default()));
        let length = answer
            .get(8..12)
            .map(|length| u32::from_le_bytes(length.try_into().unwrap_or_default()));
        let (Some(given), Some(length)) = (given, length) else {
            return Ok(None);
        };
        let Some(record) = answer.get(12..12 + length as usize) else {
            return Ok(None);
        };
        let mut record = record.to_vec();
        // NTFS hands records back with their sectors' ends put back; one
        // that is not is put right here, and either way reads the same.
        mft::fix(&mut record);
        Ok(((given & 0xFFFF_FFFF_FFFF) == u64::from(number)).then_some(record))
    }

    fn journal(&self) -> io::Result<Journal> {
        match self.query() {
            Err(error) if error.raw_os_error() == Some(ERROR_JOURNAL_NOT_ACTIVE.0 as i32) => {
                self.create_journal()?;
                self.query()
            }
            other => other,
        }
    }

    fn flush(&self) -> io::Result<()> {
        let writable = Owned(open(
            &self.path,
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_FLAGS_AND_ATTRIBUTES(0),
        )?);
        // SAFETY: a plain call on a handle that is ours.
        unsafe { FlushFileBuffers(writable.0)? };
        Ok(())
    }

    fn changes(&self, from: i64, journal: u64, stop: &AtomicBool) -> io::Result<Option<Vec<u8>>> {
        let asked = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: from,
            ReasonMask: usn::REASONS,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 1,
            UsnJournalID: journal,
        };
        // SAFETY: a plain C structure, read as the bytes it is.
        let input = unsafe {
            std::slice::from_raw_parts(
                (&raw const asked).cast::<u8>(),
                size_of::<READ_USN_JOURNAL_DATA_V0>(),
            )
        };
        let mut room = vec![0_u64; JOURNAL_ROOM / 8];
        let Some(length) = self.control(
            FSCTL_READ_USN_JOURNAL,
            input,
            bytes_mut(&mut room),
            Some(stop),
        )?
        else {
            return Ok(None);
        };
        let read = bytes_mut(&mut room)[..length].to_vec();
        thread::sleep(GATHER);
        Ok(Some(read))
    }

    fn children(&self, record: u32) -> io::Result<Vec<u32>> {
        let Some(sequence) = self.record(record)?.and_then(|bytes| mft::parse(&bytes)) else {
            return Ok(Vec::new());
        };
        let reference = u64::from(record) | u64::from(sequence.sequence) << 48;
        let id = FILE_ID_DESCRIPTOR {
            dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 {
                FileId: reference as i64,
            },
        };
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        // SAFETY: a plain call; the handle it gives is closed by `Owned`.
        let folder = Owned(unsafe {
            OpenFileById(
                self.volume.0,
                &id,
                FILE_LIST_DIRECTORY.0 | SYNCHRONIZE.0,
                share,
                None,
                FILE_FLAG_BACKUP_SEMANTICS,
            )?
        });
        entries(&folder)
    }
}

/// The records of what the open folder `folder` holds.
fn entries(folder: &Owned) -> io::Result<Vec<u32>> {
    let mut room = vec![0_u64; ENTRIES_ROOM / 8];
    let mut records = Vec::new();
    let mut class = FileIdBothDirectoryRestartInfo;
    loop {
        // SAFETY: the room is ours, as long as said, for this call alone.
        let listed = unsafe {
            GetFileInformationByHandleEx(
                folder.0,
                class,
                room.as_mut_ptr().cast(),
                (room.len() * 8) as u32,
            )
        };
        match listed {
            Ok(()) => records.extend(directory::records(bytes_mut(&mut room))),
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(records),
            Err(error) => return Err(error.into()),
        }
        class = FileIdBothDirectoryInfo;
    }
}

/// Opens the volume at `path` for `access`, shared with everyone.
fn open(path: &HSTRING, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> io::Result<HANDLE> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: a plain call; the caller owns the handle it gives.
    let handle = unsafe { CreateFileW(path, access, share, None, OPEN_EXISTING, flags, None)? };
    Ok(handle)
}

/// An event for one call to signal, closed when dropped.
fn event() -> io::Result<Owned> {
    // SAFETY: a plain call; the event is closed by `Owned`.
    Ok(Owned(unsafe { CreateEventW(None, true, false, None)? }))
}

/// What a call issued on an overlapped handle said: it ended, or it is
/// under way.
fn pending(issued: windows::core::Result<()>) -> io::Result<()> {
    match issued {
        Err(error) if error.code() != ERROR_IO_PENDING.to_hresult() => Err(error.into()),
        _ => Ok(()),
    }
}

/// Memory of 8-byte words, as the bytes Windows writes into it.
fn bytes_mut(room: &mut [u64]) -> &mut [u8] {
    // SAFETY: the words are plain memory, as long in bytes as said.
    unsafe { std::slice::from_raw_parts_mut(room.as_mut_ptr().cast(), room.len() * 8) }
}
