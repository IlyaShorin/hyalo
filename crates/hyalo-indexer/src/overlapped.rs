//! Calls issued on a handle opened for overlapped I/O, waited for in a way
//! that can be stopped: the volume's and the pipe's.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
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
