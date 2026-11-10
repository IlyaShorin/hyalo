//! The clipboard: what Enter on an answer, Ctrl+C and Ctrl+X put on it,
//! and what Ctrl+V takes from it. Text only, as UTF-16.
//!
//! Tests that touch it run in a window station of their own
//! (`test_desktop::in_own_window_station`): the user's clipboard is never
//! overwritten.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::core::{Error, Result};

/// The clipboard format of UTF-16 text, CF_UNICODETEXT. The windows crate
/// keeps its name in the large Ole module.
const UNICODE_TEXT: u32 = 13;

/// Puts `text` on the clipboard in place of what was there.
pub fn copy(text: &str) -> Result<()> {
    let _open = Clipboard::open()?;
    // SAFETY: the clipboard is open on this thread.
    unsafe { EmptyClipboard()? };
    let memory = global_text(text)?;
    // SAFETY: `memory` holds NUL-terminated UTF-16. Once set, it belongs
    // to the clipboard; if setting fails, it is still ours to free.
    unsafe {
        SetClipboardData(UNICODE_TEXT, Some(HANDLE(memory.0))).inspect_err(|_| {
            let _ = GlobalFree(Some(memory));
        })?;
    }
    Ok(())
}

/// `text` as NUL-terminated UTF-16 in movable global memory, the form the
/// clipboard takes it in.
fn global_text(text: &str) -> Result<HGLOBAL> {
    // Windows also writes a NUL over the last unit when the text is set,
    // but the clipboard's contract is to hand it NUL-terminated text.
    let units: Vec<u16> = format!("{text}\0").encode_utf16().collect();
    // SAFETY: the memory holds `units.len()` units and stays locked while
    // they are copied in.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * size_of::<u16>())?;
        let target = GlobalLock(memory).cast::<u16>();
        if target.is_null() {
            let error = Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error);
        }
        target.copy_from_nonoverlapping(units.as_ptr(), units.len());
        // Releasing the last lock reports FALSE with no error: not a failure.
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

/// The clipboard, open on this thread until dropped.
struct Clipboard;

impl Clipboard {
    /// Opens it with no owner window: the text is all there at once, so
    /// no one needs to be asked to render it later.
    fn open() -> Result<Self> {
        // SAFETY: a plain call; Drop closes what it opened.
        unsafe { OpenClipboard(None)? };
        Ok(Self)
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

/// The text on the clipboard; `None` when it holds no text or cannot be
/// opened (another app holding it, say).
pub fn text() -> Option<String> {
    // SAFETY: a plain call.
    unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) }.ok()?;
    let _open = Clipboard::open().ok()?;
    // SAFETY: the clipboard is open on this thread; the handle it gives
    // stays the clipboard's, and is only read while locked.
    unsafe {
        let memory = HGLOBAL(GetClipboardData(UNICODE_TEXT).ok()?.0);
        let units = GlobalLock(memory).cast::<u16>().cast_const();
        if units.is_null() {
            return None;
        }
        // The text ends at its NUL, and never past its memory.
        let room = GlobalSize(memory) / size_of::<u16>();
        let all = std::slice::from_raw_parts(units, room);
        let length = all.iter().position(|&unit| unit == 0).unwrap_or(room);
        let text = String::from_utf16_lossy(&all[..length]);
        let _ = GlobalUnlock(memory);
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_desktop::in_own_window_station;

    #[test]
    fn text_copied_is_the_text_read_back_and_an_empty_clipboard_has_none() {
        let name =
            "clipboard::tests::text_copied_is_the_text_read_back_and_an_empty_clipboard_has_none";
        in_own_window_station(name, || {
            copy(
                "pad
plus 🔍",
            )
            .expect("copy");
            assert_eq!(
                text().as_deref(),
                Some(
                    "pad
plus 🔍"
                )
            );
            copy("").expect("copy nothing");
            assert_eq!(text().as_deref(), Some(""));
            {
                let _open = Clipboard::open().expect("open");
                // SAFETY: the clipboard is open on this thread.
                unsafe { EmptyClipboard() }.expect("empty");
            }
            assert_eq!(text(), None);
        });
    }
}
