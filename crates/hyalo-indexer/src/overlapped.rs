//! Calls issued on a handle opened for overlapped I/O, waited for in a way
//! that can be stopped: the volume's and the pipe's.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::ReadFile;
use windows::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::core::HRESULT;

/// How long a wait goes before it looks whether it is to stop.
pub(crate) const PATIENCE: Duration = Duration::from_millis(500);

/// A handle Windows gave, closed once when dropped.
pub(crate) struct Owned(pub(crate) HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: a handle may be used from any thread; whoever shares one issues
// each call with an `OVERLAPPED` of its own.
unsafe impl Send for Owned {}
// SAFETY: as above.
unsafe impl Sync for Owned {}

/// An `OVERLAPPED` with an event of its own, for one call; at `at`, for a
/// read or write of a file.
pub(crate) struct Call {
    pub(crate) overlapped: OVERLAPPED,
    _event: Owned,
}

impl Call {
    pub(crate) fn new(at: u64) -> io::Result<Call> {
        // SAFETY: a plain call; the event is closed by `Owned`.
        let event = Owned(unsafe { CreateEventW(None, true, false, None).map_err(os)? });
        let mut overlapped = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        overlapped.Anonymous.Anonymous.Offset = at as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (at >> 32) as u32;
        Ok(Call {
            overlapped,
            _event: event,
        })
    }
}

/// A Windows error as the error Rust knows: the Win32 code an HRESULT
/// holds, so its kind reads (the windows crate's own conversion keeps the
/// HRESULT, which no kind matches).
pub(crate) fn os(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    match code & 0xFFFF_0000 {
        0x8007_0000 => io::Error::from_raw_os_error((code & 0xFFFF) as i32),
        _ => io::Error::other(error),
    }
}

/// What a call issued on an overlapped handle said: it ended, or it is
/// under way.
pub(crate) fn pending(issued: windows::core::Result<()>) -> io::Result<()> {
    match issued {
        Err(error) if error.code() != ERROR_IO_PENDING.to_hresult() => Err(os(error)),
        _ => Ok(()),
    }
}

/// Waits for the call issued on `handle` with `overlapped` to end, looking
/// at `stop` now and then: the bytes it moved, none if it was stopped, in
/// which case it is cancelled first.
pub(crate) fn wait(
    handle: HANDLE,
    overlapped: *const OVERLAPPED,
    stop: Option<&AtomicBool>,
) -> io::Result<Option<usize>> {
    let going = || stop.is_none_or(|stop| !stop.load(Ordering::SeqCst));
    wait_while(handle, overlapped, &going)
}

/// Waits for the call issued on `handle` with `overlapped` to end, asking
/// `going` now and then whether to wait on: the bytes it moved, none if it
/// was not to, in which case it is cancelled first.
pub(crate) fn wait_while(
    handle: HANDLE,
    overlapped: *const OVERLAPPED,
    going: &dyn Fn() -> bool,
) -> io::Result<Option<usize>> {
    let timeout = HRESULT::from_win32(WAIT_TIMEOUT.0);
    loop {
        let mut length = 0;
        // SAFETY: the call was issued on this handle with `overlapped`.
        let result = unsafe {
            GetOverlappedResultEx(
                handle,
                overlapped,
                &mut length,
                PATIENCE.as_millis() as u32,
                false,
            )
        };
        match result {
            Ok(()) => return Ok(Some(length as usize)),
            Err(error) if error.code() == timeout => {
                if !going() {
                    cancel(handle, overlapped);
                    return Ok(None);
                }
            }
            Err(error) => return Err(os(error)),
        }
    }
}

/// Cancels the call issued on `handle` with `overlapped` and waits for it
/// to end, so nothing touches its buffer after.
pub(crate) fn cancel(handle: HANDLE, overlapped: *const OVERLAPPED) {
    let mut length = 0;
    // SAFETY: the call is ours, on this handle.
    unsafe {
        let _ = CancelIoEx(handle, Some(overlapped));
        let _ = GetOverlappedResult(handle, overlapped, &mut length, true);
    }
}

/// Issues the control `code` on `handle` with `input`, Windows writing its
/// answer into `output`, and waits for it: the bytes it wrote, none if
/// `stop` was set first.
pub(crate) fn control(
    handle: HANDLE,
    code: u32,
    input: &[u8],
    output: &mut [u8],
    stop: Option<&AtomicBool>,
) -> io::Result<Option<usize>> {
    let mut call = Call::new(0)?;
    // SAFETY: both buffers and the call outlive it: `wait` sees it end, or
    // cancels it and sees it end.
    let issued = unsafe {
        DeviceIoControl(
            handle,
            code,
            Some(input.as_ptr().cast()),
            input.len() as u32,
            Some(output.as_mut_ptr().cast()),
            output.len() as u32,
            None,
            Some(&raw mut call.overlapped),
        )
    };
    pending(issued)?;
    wait(handle, &raw const call.overlapped, stop)
}

/// The plain C structure the control `code` on `handle` answers.
pub(crate) fn control_into<T: Copy>(handle: HANDLE, code: u32, input: &[u8]) -> io::Result<T> {
    let mut room = vec![0_u64; size_of::<T>().div_ceil(8) + 32];
    control(handle, code, input, bytes_mut(&mut room), None)?;
    // SAFETY: the room is longer than a `T`, aligned to 8, and holds what
    // Windows wrote of one.
    Ok(unsafe { room.as_ptr().cast::<T>().read_unaligned() })
}

/// Reads into `buffer` from the byte `at` of `handle` (none for a pipe):
/// the bytes read, none if `stop` was set first.
pub(crate) fn read_at(
    handle: HANDLE,
    at: u64,
    buffer: &mut [u8],
    stop: Option<&AtomicBool>,
) -> io::Result<Option<usize>> {
    let mut call = Call::new(at)?;
    // SAFETY: the buffer and the call outlive the read: `wait` sees it end,
    // or cancels it and sees it end.
    let issued = unsafe { ReadFile(handle, Some(buffer), None, Some(&raw mut call.overlapped)) };
    pending(issued)?;
    wait(handle, &raw const call.overlapped, stop)
}

/// Memory of 8-byte words, as the bytes Windows writes into it.
pub(crate) fn bytes_mut(room: &mut [u64]) -> &mut [u8] {
    // SAFETY: the words are plain memory, as long in bytes as said.
    unsafe { std::slice::from_raw_parts_mut(room.as_mut_ptr().cast(), room.len() * 8) }
}

/// A plain C structure, as the bytes Windows reads of it.
pub(crate) fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: the structure is plain memory, as long in bytes as its type.
    unsafe { std::slice::from_raw_parts((value as *const T).cast(), size_of::<T>()) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_SHARE_READ, OPEN_EXISTING,
    };
    use windows::core::HSTRING;

    /// A folder of the test's own under `%TEMP%`, gone when dropped.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new(test: &str) -> Scratch {
            let at =
                std::env::temp_dir().join(format!("hyalo-indexer-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&at);
            fs::create_dir_all(&at).expect("a scratch folder");
            Scratch(at)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The file at `path`, open for overlapped reads.
    fn opened(path: &std::path::Path) -> Owned {
        let path = HSTRING::from(path.as_os_str());
        // SAFETY: a plain call; the handle is closed by `Owned`.
        let handle = unsafe {
            CreateFileW(
                &path,
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        Owned(handle.expect("opened"))
    }

    #[test]
    fn a_file_is_read_from_the_byte_asked_for() {
        let scratch = Scratch::new("read-at");
        let bytes: Vec<u8> = (0..8192_u32).map(|at| (at % 251) as u8).collect();
        fs::write(scratch.0.join("bytes"), &bytes).expect("written");
        let file = opened(&scratch.0.join("bytes"));
        let mut read = [0; 16];
        assert_eq!(
            read_at(file.0, 5000, &mut read, None).expect("read"),
            Some(16)
        );
        assert_eq!(read, bytes[5000..5016]);
        let mut beyond = [0; 16];
        let past = read_at(file.0, 9000, &mut beyond, None);
        assert!(
            !matches!(past, Ok(Some(16))),
            "nothing past the end: {past:?}"
        );
    }

    /// `FSCTL_GET_COMPRESSION`: which compression a file has, in two bytes.
    const GET_COMPRESSION: u32 = 0x0009_003C;
    /// A control of the file system's that no file system knows.
    const UNKNOWN: u32 = 0x0009_3FFC;

    #[test]
    fn a_control_answers_into_its_output_or_says_why_not() {
        let scratch = Scratch::new("control");
        fs::write(scratch.0.join("plain"), b"plain").expect("written");
        let file = opened(&scratch.0.join("plain"));
        let mut answer = [0xFF_u8; 2];
        let wrote = control(file.0, GET_COMPRESSION, &[], &mut answer, None);
        assert_eq!(wrote.expect("answered"), Some(2));
        assert_eq!(answer, [0, 0], "not compressed");
        assert_eq!(
            control_into::<u16>(file.0, GET_COMPRESSION, &[]).expect("answered"),
            0
        );
        assert!(control(file.0, UNKNOWN, &[], &mut [0; 128], None).is_err());
    }

    #[test]
    fn a_windows_error_keeps_its_win32_code() {
        let not_found = windows::core::Error::from_hresult(HRESULT::from_win32(2));
        assert_eq!(os(not_found).kind(), io::ErrorKind::NotFound);
        let failed = windows::core::Error::from_hresult(HRESULT(0x8000_4005_u32 as i32));
        assert_eq!(os(failed).raw_os_error(), None);
    }

    #[test]
    fn a_structure_reads_as_its_bytes() {
        let value: u32 = 0x0403_0201;
        assert_eq!(bytes_of(&value), [1, 2, 3, 4]);
        let mut words = [0x0807_0605_0403_0201_u64, 0];
        assert_eq!(bytes_mut(&mut words)[..9], [1, 2, 3, 4, 5, 6, 7, 8, 0]);
    }
}
