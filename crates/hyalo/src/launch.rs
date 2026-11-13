//! Carrying out what was picked: opening it the way Explorer opens it,
//! showing it in its folder, or copying it.
//!
//! The launcher never runs elevated, so what it opens is not elevated
//! either, unless asked: the `runas` verb shows the UAC prompt.

use std::path::Path;

use hyalo_core::item::Action;
use hyalo_core::uninstall::Uninstall;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, HANDLE};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ILCreateFromPathW, ILFree,
    SEE_MASK_DEFAULT, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{SHOW_WINDOW_CMD, SW_HIDE, SW_SHOWNORMAL};
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::clipboard::{copy, put};
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
        Action::CopyText(_) | Action::PutBack(_) => put_back(action),
        Action::Edit(path) => edit(path),
        // The launcher opens its own window; there is nothing to run.
        Action::Settings => Ok(()),
        Action::Switch(window) => crate::switcher::switch_to(*window),
    }
}

/// Carries out what changes the machine, which its row asked about
/// first: a system command, or an uninstall, whose uninstaller starts as
/// any program does (as administrator only when asked) and is waited
/// for, or whose package is removed. `run` hands it nothing else.
fn change(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::System(command) => system::run(*command, &system::System),
        Action::Uninstall(Uninstall::Program {
            program, arguments, ..
        }) => uninstall_with(program, arguments, as_admin),
        Action::Uninstall(Uninstall::Package { full_name, .. }) => uninstall::remove(full_name),
        _ => Ok(()),
    }
}

/// Puts text on the clipboard, or what was copied back on it; a picture
/// is made pixels again through COM. `run` hands it nothing else.
fn put_back(action: &Action) -> Result<()> {
    match action {
        Action::CopyText(text) => copy(text),
        Action::PutBack(content) => {
            let _com = Com::init()?;
            put(content)
        }
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

/// Starts the uninstaller `program` with `arguments`, as administrator
/// only when asked, and waits until it ends: the launcher looks for the
/// apps again then.
fn uninstall_with(program: &str, arguments: &str, as_admin: bool) -> Result<()> {
    let process = started(
        program,
        Some(arguments),
        verb(as_admin),
        (SEE_MASK_NOCLOSEPROCESS, SW_SHOWNORMAL),
    )?;
    wait(process)
}

/// Starts `program` with `arguments`, hidden, as administrator when asked
/// (the UAC prompt asks the user), and waits until it ends: its exit
/// code. Call it off the UI thread.
pub fn waited(program: &str, arguments: &str, as_admin: bool) -> Result<u32> {
    let mask = (SEE_MASK_NOCLOSEPROCESS, SW_HIDE);
    let process = started(program, Some(arguments), verb(as_admin), mask)?;
    let code = exit_code(process);
    wait(process)?;
    code
}

/// The exit code of `process`, once it has ended.
fn exit_code(process: HANDLE) -> Result<u32> {
    let mut code = 0;
    // SAFETY: the handle is ours, alive until `wait` closes it.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        GetExitCodeProcess(process, &mut code)?;
    }
    Ok(code)
}

/// Has the shell carry out `verb` (none: the usual one) on `file`, with
/// `argument` if it is a program.
fn execute(file: &str, argument: Option<&str>, verb: PCWSTR) -> Result<()> {
    started(file, argument, verb, (SEE_MASK_DEFAULT, SW_SHOWNORMAL)).map(drop)
}

/// The same, with `mask` besides the usual flags, shown as `show` says;
/// the process the shell started, when `mask` asks to keep it and there
/// is one, else a null handle.
fn started(
    file: &str,
    argument: Option<&str>,
    verb: PCWSTR,
    (mask, show): (u32, SHOW_WINDOW_CMD),
) -> Result<HANDLE> {
    let _com = Com::init()?;
    let file = HSTRING::from(file);
    let argument = argument.map(HSTRING::from);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: this thread may end as soon as the call returns.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI | mask,
        lpVerb: verb,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: argument
            .as_ref()
            .map_or(PCWSTR::null(), |argument| PCWSTR(argument.as_ptr())),
        nShow: show.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    unsafe { ShellExecuteExW(&mut info)? };
    Ok(info.hProcess)
}

/// Waits until `process` ends, then closes it; a null one (the shell
/// started no process of its own) is done already.
fn wait(process: HANDLE) -> Result<()> {
    if process.is_invalid() {
        return Ok(());
    }
    // SAFETY: the handle is ours: waited on once, then closed once.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        CloseHandle(process)
    }
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
    fn a_program_waited_for_says_how_it_ended() {
        let cmd = std::env::var("ComSpec").expect("ComSpec is set");
        assert_eq!(waited(&cmd, "/c exit 3", false).expect("ran"), 3);
        assert_eq!(waited(&cmd, "/c exit 0", false).expect("ran"), 0);
        let missing = waited("hyalo-test-no-such-program.exe", "", false);
        assert_eq!(
            missing.expect_err("none").code(),
            ERROR_FILE_NOT_FOUND.to_hresult()
        );
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

    /// A handle of this process's own to the thread `worker` runs on.
    fn own_handle(worker: &std::thread::JoinHandle<()>) -> HANDLE {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut own = HANDLE::default();
        let thread = HANDLE(worker.as_raw_handle());
        // SAFETY: plain calls; the copy is ours, for `wait` to close.
        unsafe {
            let this = GetCurrentProcess();
            DuplicateHandle(
                this,
                thread,
                this,
                &mut own,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .expect("a handle of its own");
        own
    }

    #[test]
    fn waiting_for_an_uninstaller_lasts_until_it_ends() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let ended = Arc::new(AtomicBool::new(false));
        let ending = ended.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            ending.store(true, Ordering::SeqCst);
        });
        wait(own_handle(&worker)).expect("waited and closed");
        assert!(ended.load(Ordering::SeqCst), "back only once it ended");
        worker.join().expect("ended");
        assert_eq!(
            wait(HANDLE::default()),
            Ok(()),
            "no process, nothing to wait for"
        );
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
