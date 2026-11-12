//! Carrying out what was picked: opening it the way Explorer opens it,
//! showing it in its folder, or copying it.
//!
//! The launcher never runs elevated, so what it opens is not elevated
//! either, unless asked: the `runas` verb shows the UAC prompt.

use std::path::Path;

use hyalo_core::item::Action;
use hyalo_core::uninstall::Uninstall;
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
use crate::{system, uninstall};

/// Carries out a picked item's action. Waits for the shell, so call it
/// off the UI thread.
pub fn run(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::Open(target) => open(target, as_admin),
        Action::Launch { program, arguments } => execute(program, Some(arguments), verb(as_admin)),
        Action::Reveal(path) => reveal(path),
        Action::System(_) | Action::Uninstall(_) => change(action, as_admin),
        Action::CopyText(text) => copy(text),
        Action::Edit(path) => edit(path),
        // The launcher opens its own window; there is nothing to run.
        Action::Settings => Ok(()),
        Action::Switch(window) => crate::switcher::switch_to(*window),
    }
}

/// Carries out what changes the machine, which its row asked about
/// first: a system command, or an uninstall, whose uninstaller starts as
/// any program does (as administrator only when asked) or whose package
/// is removed. `run` hands it nothing else.
fn change(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::System(command) => system::run(*command, &system::System),
        Action::Uninstall(Uninstall::Program {
            program, arguments, ..
        }) => execute(program, Some(arguments), verb(as_admin)),
        Action::Uninstall(Uninstall::Package { full_name, .. }) => uninstall::remove(full_name),
        _ => Ok(()),
    }
}

/// Whether `action` must be carried out at once, on the launcher's
/// thread, rather than on a thread of its own: switching to a window
/// must happen while the launcher is still in front, since Windows lets
/// only the foreground hand the foreground on, and it hides next.
pub fn at_once(action: &Action) -> bool {
    matches!(action, Action::Switch(_))
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
    execute(target, None, verb(as_admin))
}

/// The verb that opens as administrator, or the usual one.
fn verb(as_admin: bool) -> PCWSTR {
    match as_admin {
        true => w!("runas"),
        false => PCWSTR::null(),
    }
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
    fn a_program_that_is_not_there_fails_without_a_message_box() {
        let missing = Action::Launch {
            program: "hyalo-test-no-such-program.exe".to_string(),
            arguments: "--never".to_string(),
        };
        let error = run(&missing, false).expect_err("no such program");
        assert_eq!(error.code(), ERROR_FILE_NOT_FOUND.to_hresult());
    }

    #[test]
    fn an_uninstaller_that_is_not_there_fails_without_a_message_box() {
        let missing = Uninstall::Program {
            name: "Nothing".to_string(),
            program: r"C:\hyalo-test-no-such-folder\unins000.exe".to_string(),
            arguments: "/S".to_string(),
        };
        let error = run(&Action::Uninstall(missing), false).expect_err("no such program");
        assert_eq!(error.code(), ERROR_FILE_NOT_FOUND.to_hresult());
    }

    #[test]
    fn a_package_that_is_not_there_is_not_removed() {
        let missing = Uninstall::Package {
            name: "Nothing".to_string(),
            full_name: "hyalo.test.NoSuchPackage_1.0.0.0_x64__0000000000000".to_string(),
        };
        let error = run(&Action::Uninstall(missing), false).expect_err("no such package");
        assert_eq!(error.code().0 as u32, 0x8007_3CF1, "{error}");
    }

    #[test]
    fn the_settings_window_is_the_launchers_to_open() {
        assert_eq!(run(&Action::Settings, false), Ok(()));
    }

    #[test]
    fn switching_happens_at_once_and_everything_else_on_a_thread() {
        assert!(at_once(&Action::Switch(0x1234)));
        for action in [
            Action::Open(r"C:\a.txt".to_string()),
            Action::Launch {
                program: "a.exe".to_string(),
                arguments: String::new(),
            },
            Action::Reveal(r"C:\a.txt".to_string()),
            Action::CopyText("4".to_string()),
            Action::Settings,
            Action::Edit(r"C:\a.toml".to_string()),
            Action::Uninstall(Uninstall::Package {
                name: "App".to_string(),
                full_name: "App_1.0.0.0_x64__0000000000000".to_string(),
            }),
        ] {
            assert!(!at_once(&action), "{action:?}");
        }
    }

    #[test]
    fn a_window_that_is_gone_is_not_switched_to() {
        let gone = run(&Action::Switch(0x7FFF_FFF0), false).expect_err("no such window");
        let invalid = windows::Win32::Foundation::ERROR_INVALID_WINDOW_HANDLE;
        assert_eq!(gone.code(), invalid.to_hresult());
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
