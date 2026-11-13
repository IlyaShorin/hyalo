//! An end of the service's pipe, opened for overlapped I/O: frames written
//! whole and read whole, each wait one that can be stopped. The server's
//! and the launcher's.

use std::io;
use std::sync::atomic::AtomicBool;

use hyalo_core::pipe;
use windows::Win32::Storage::FileSystem::WriteFile;

use crate::overlapped::{Call, Owned, pending, read_at, wait};

/// One end of the pipe.
pub(crate) struct End(pub(crate) Owned);

impl End {
    /// Writes all of `bytes`, unless `stop`, if there is one, is set
    /// first.
    pub(crate) fn write(&self, bytes: &[u8], stop: Option<&AtomicBool>) -> io::Result<()> {
        let mut written = 0;
        while written < bytes.len() {
            let mut call = Call::new(0)?;
            // SAFETY: the bytes and the call outlive the write: `wait`
            // sees it end, or cancels it and sees it end.
            let issued = unsafe {
                WriteFile(
                    self.0.0,
                    Some(&bytes[written..]),
                    None,
                    Some(&raw mut call.overlapped),
                )
            };
            pending(issued)?;
            match wait(self.0.0, &raw const call.overlapped, stop)? {
                None => return Err(io::ErrorKind::Interrupted.into()),
                Some(0) => return Err(io::ErrorKind::WriteZero.into()),
                Some(more) => written += more,
            }
        }
        Ok(())
    }

    /// Fills `buffer`; false if `stop` was set first.
    fn fill(&self, buffer: &mut [u8], stop: &AtomicBool) -> io::Result<bool> {
        let mut filled = 0;
        while filled < buffer.len() {
            match read_at(self.0.0, 0, &mut buffer[filled..], Some(stop))? {
                None => return Ok(false),
                Some(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Some(more) => filled += more,
            }
        }
        Ok(true)
    }

    /// The body of the next frame; none if `stop` was set first.
    pub(crate) fn frame(&self, stop: &AtomicBool) -> io::Result<Option<Vec<u8>>> {
        let mut head = [0; 4];
        if !self.fill(&mut head, stop)? {
            return Ok(None);
        }
        let length = pipe::length(head).ok_or(io::ErrorKind::InvalidData)?;
        let mut body = vec![0; length];
        Ok(self.fill(&mut body, stop)?.then_some(body))
    }
}
