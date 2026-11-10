//! Carrying out what was picked: opening it the way Explorer opens it,
//! showing it in its folder, or copying it.
//!
//! The launcher never runs elevated, so what it opens is not elevated
//! either, unless asked: the `runas` verb shows the UAC prompt.

use std::path::Path;

use hyalo_core::item::Action;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ILCreateFromPathW, ILFree,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems,
    ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::clipboard::copy;
use crate::com::Com;

/// Carries out a picked item's action. Waits for the shell, so call it
/// off the UI thread.
pub fn run(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::Open(target) => open(target, as_admin),
        Action::Reveal(path) => reveal(path),
        Action::CopyText(text) => copy(text),
        Action::Edit(path) => edit(path),
        // The launcher opens its own window; there is nothing to run.
        Action::Settings => Ok(()),
    }
}

/// Opens the text file `path` with the app Windows opens its kind of
/// file with, or, when it has none (`.toml` often has none), in Notepad
/// rather than with Windows' dialog asking which app to use.
fn edit(path: &str) -> Result<()> {
    let (file, argument) = editor(path);
    execute(&file, argument.as_deref(), PCWSTR::null())
}

/// What opens the text file `path`, and with what argument: the file
/// itself, when Windows has an app for its kind, or Notepad with it.
fn editor(path: &str) -> (String, Option<String>) {
    match has_app(path) {
        true => (path.to_string(), None),
        false => ("notepad.exe".to_string(), Some(format!("\"{path}\""))),
    }
}

/// Whether Windows has an app to open `path`'s kind of file with, by
/// its extension; the dialog that asks for one does not count.
fn has_app(path: &str) -> bool {
    let Some(extension) = Path::new(path).extension() else {
        return false;
    };
    let kind = HSTRING::from(format!(".{}", extension.to_string_lossy()));
    let mut length = 0_u32;
    // SAFETY: with no buffer the call only measures what it would write.
    let found = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN,
            ASSOCSTR_EXECUTABLE,
            &kind,
            w!("open"),
            None,
            &mut length,
        )
    };
    found.is_ok()
}

/// Opens the folder `path` is in, in Explorer, with it selected. Waits
/// for the shell, so call it off the UI thread.
fn reveal(path: &str) -> Result<()> {
    let _com = Com::init()?;
    // SAFETY: the item list ILCreateFromPathW gives is ours: used once,
    // then freed once.
    unsafe {
        let item = ILCreateFromPathW(&HSTRING::from(path));
        if item.is_null() {
            return Err(ERROR_FILE_NOT_FOUND.to_hresult().into());
        }
        let shown = SHOpenFolderAndSelectItems(item, None, 0);
        ILFree(Some(item));
        shown
    }
}

/// Opens `target` (a path, an address, or `shell:AppsFolder\<app>`)
/// through the shell; with `as_admin`, through the `runas` verb. Waits
/// until the shell has started it, so call it off the UI thread. Errors
/// come back instead of a message box.
fn open(target: &str, as_admin: bool) -> Result<()> {
    let verb = match as_admin {
        true => w!("runas"),
        false => PCWSTR::null(),
    };
    execute(target, None, verb)
}

/// Has the shell carry out `verb` (none: the usual one) on `file`, with
/// `argument` if it is a program.
fn execute(file: &str, argument: Option<&str>, verb: PCWSTR) -> Result<()> {
    let _com = Com::init()?;
    let file = HSTRING::from(file);
    let argument = argument.map(HSTRING::from);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: this thread may end as soon as the call returns.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: argument
            .as_ref()
            .map_or(PCWSTR::null(), |argument| PCWSTR(argument.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_desktop::in_own_window_station;
    use windows::Win32::Foundation::E_NOTIMPL;

    #[test]
    fn an_app_that_is_not_there_fails_without_a_message_box() {
        let missing = r"shell:AppsFolder\hyalo.test.no-such-app";
        let error = run(&Action::Open(missing.to_string()), false).expect_err("no such app");
        assert_ne!(error.code(), E_NOTIMPL);
    }

    #[test]
    fn text_files_have_an_app_and_a_kind_no_app_knows_has_none() {
        assert!(has_app(r"C:\Users\Me\notes.txt"), "Notepad, at least");
        assert!(!has_app(r"C:\Users\Me\notes.hyalo-test-no-such-kind"));
        assert!(!has_app(r"C:\Users\Me\README"), "no extension");
    }

    #[test]
    fn a_text_file_opens_with_its_app_or_else_in_notepad() {
        let text = r"C:\Users\Me\notes.txt";
        assert_eq!(editor(text), (text.to_string(), None));
        let unknown = r"C:\Users\Me\hyalo\config.hyalo-test-no-such-kind";
        let quoted = format!("\"{unknown}\"");
        assert_eq!(editor(unknown), ("notepad.exe".to_string(), Some(quoted)));
    }

    #[test]
    fn a_file_that_is_not_there_is_not_shown() {
        let missing = r"C:\no such folder here\no such file.txt";
        let error = run(&Action::Reveal(missing.to_string()), false).expect_err("not there");
        assert_eq!(error.code(), ERROR_FILE_NOT_FOUND.to_hresult());
    }

    #[test]
    fn copying_text_puts_it_on_the_clipboard_in_place_of_what_was_there() {
        let name =
            "launch::tests::copying_text_puts_it_on_the_clipboard_in_place_of_what_was_there";
        in_own_window_station(name, || {
            run(&Action::CopyText("1\u{A0}234,5".to_string()), false).expect("copy");
            assert_eq!(crate::clipboard::text().as_deref(), Some("1\u{A0}234,5"));
            run(&Action::CopyText("6".to_string()), false).expect("copy again");
            assert_eq!(crate::clipboard::text().as_deref(), Some("6"));
        });
    }
}
