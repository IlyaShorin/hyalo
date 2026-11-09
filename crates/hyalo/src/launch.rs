//! Opening what was picked, the way Explorer opens it.
//!
//! The launcher never runs elevated, so what it opens is not elevated
//! either, unless asked: the `runas` verb shows the UAC prompt.

use hyalo_core::item::Action;
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::com::Com;

/// Carries out a picked item's action. Waits for the shell, so call it
/// off the UI thread.
pub fn run(action: &Action, as_admin: bool) -> Result<()> {
    match action {
        Action::Open(target) => open(target, as_admin),
        // Copying comes with the calculator, the first item that copies.
        Action::CopyText(_) => Err(E_NOTIMPL.into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_is_not_there_fails_without_a_message_box() {
        let missing = r"shell:AppsFolder\hyalo.test.no-such-app";
        let error = run(&Action::Open(missing.to_string()), false).expect_err("no such app");
        assert_ne!(error.code(), E_NOTIMPL);
    }

    #[test]
    fn copying_text_is_not_there_yet() {
        let error = run(&Action::CopyText("42".to_string()), false).expect_err("not yet");
        assert_eq!(error.code(), E_NOTIMPL);
    }
}
