//! Carrying out what was picked: opening it the way Explorer opens it, or
//! copying it.
//!
//! The launcher never runs elevated, so what it opens is not elevated
//! either, unless asked: the `runas` verb shows the UAC prompt.

use hyalo_core::item::Action;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use crate::com::Com;

/// Carries out a picked item's action. Waits for the shell, so call it
/// off the UI thread.
pub fn run(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::Open(target) => open(target, as_admin),
        Action::CopyText(text) => copy(text),
    }
}

/// Opens `target` (a path, an address, or `shell:AppsFolder\<app>`)
/// through the shell; with `as_admin`, through the `runas` verb. Waits
/// until the shell has started it, so call it off the UI thread. Errors
/// come back instead of a message box.
fn open(target: &str, as_admin: bool) -> Result<()> {
    let _com = Com::init()?;
    let file = HSTRING::from(target);
    let verb = match as_admin {
        true => w!("runas"),
        false => PCWSTR::null(),
    };
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: this thread may end as soon as the call returns.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb,
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
}

/// The clipboard format of UTF-16 text, CF_UNICODETEXT. The windows crate
/// keeps its name in the large Ole module.
const UNICODE_TEXT: u32 = 13;

/// Puts `text` on the clipboard in place of what was there.
fn copy(text: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_desktop::in_own_window_station;
    use windows::Win32::Foundation::E_NOTIMPL;
    use windows::Win32::System::DataExchange::GetClipboardData;

    #[test]
    fn an_app_that_is_not_there_fails_without_a_message_box() {
        let missing = r"shell:AppsFolder\hyalo.test.no-such-app";
        let error = run(&Action::Open(missing.to_string()), false).expect_err("no such app");
        assert_ne!(error.code(), E_NOTIMPL);
    }

    /// The text on the clipboard, if it holds text.
    fn clipboard_text() -> Option<String> {
        // SAFETY: the clipboard is open while its data is read; the text
        // is NUL-terminated UTF-16, read while its memory is locked.
        unsafe {
            OpenClipboard(None).ok()?;
            let data = GetClipboardData(UNICODE_TEXT).ok();
            let text = data.and_then(|data| {
                let memory = HGLOBAL(data.0);
                let units = GlobalLock(memory).cast::<u16>().cast_const();
                let length = (0..).take_while(|&at| *units.add(at) != 0).count();
                let text = String::from_utf16(std::slice::from_raw_parts(units, length));
                let _ = GlobalUnlock(memory);
                text.ok()
            });
            let _ = CloseClipboard();
            text
        }
    }

    #[test]
    fn copying_text_puts_it_on_the_clipboard_in_place_of_what_was_there() {
        let name =
            "launch::tests::copying_text_puts_it_on_the_clipboard_in_place_of_what_was_there";
        in_own_window_station(name, || {
            run(&Action::CopyText("1\u{A0}234,5".to_string()), false).expect("copy");
            assert_eq!(clipboard_text().as_deref(), Some("1\u{A0}234,5"));
            run(&Action::CopyText("6".to_string()), false).expect("copy again");
            assert_eq!(clipboard_text().as_deref(), Some("6"));
        });
    }
}
