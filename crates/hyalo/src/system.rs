//! Carries out system commands. Which commands there are, what they are
//! called and that every one but Lock asks first is decided in
//! `hyalo_core::system_commands` and `results`; which Windows call each
//! makes is `run`, tested with a stand-in for Windows. The real calls
//! (`System`) are thin glue, one call each.
//!
//! No test reaches `System`: window tests hand the launcher a stand-in
//! runner, and nothing here was run by hand while the user was away (it
//! would lock, sleep, restart or empty something of theirs).

use hyalo_core::system_commands::Command;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::{IsPwrHibernateAllowed, SetSuspendState};
use windows::Win32::System::Shutdown::{
    EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EXIT_WINDOWS_FLAGS, ExitWindowsEx, LockWorkStation,
    SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHEmptyRecycleBinW};
use windows::core::{Error, PCWSTR, Result};

use crate::com::Com;

/// What the system commands ask of Windows: one call each.
pub trait Windows {
    fn lock(&self) -> Result<()>;
    /// Sleeps, or with `hibernate` hibernates.
    fn suspend(&self, hibernate: bool) -> Result<()>;
    /// Restarts, shuts down or signs out, as `how` says.
    fn exit(&self, how: EXIT_WINDOWS_FLAGS) -> Result<()>;
    fn empty_recycle_bin(&self) -> Result<()>;
}

/// Carries out `command` through `windows`. Waits for Windows, so call
/// it off the UI thread.
pub fn run(command: Command, windows: &dyn Windows) -> Result<()> {
    match command {
        Command::Lock => windows.lock(),
        Command::Sleep => windows.suspend(false),
        Command::Hibernate => windows.suspend(true),
        Command::Restart => windows.exit(EWX_REBOOT),
        Command::ShutDown => windows.exit(EWX_POWEROFF),
        Command::SignOut => windows.exit(EWX_LOGOFF),
        Command::EmptyRecycleBin => windows.empty_recycle_bin(),
    }
}

/// Whether Windows has hibernation turned on, so Hibernate is offered.
pub fn hibernates() -> bool {
    // SAFETY: a plain call.
    unsafe { IsPwrHibernateAllowed() }
}

/// The real Windows.
pub struct System;

impl Windows for System {
    fn lock(&self) -> Result<()> {
        // SAFETY: a plain call.
        unsafe { LockWorkStation() }
    }

    fn suspend(&self, hibernate: bool) -> Result<()> {
        allow_shutdown()?;
        // SAFETY: a plain call; it returns once the machine wakes again.
        succeeded(unsafe { SetSuspendState(hibernate, false, false) })
    }

    fn exit(&self, how: EXIT_WINDOWS_FLAGS) -> Result<()> {
        allow_shutdown()?;
        let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
        // SAFETY: a plain call.
        unsafe { ExitWindowsEx(how, reason) }
    }

    /// Every drive's bin; the row already asked, so Windows does not ask
    /// again.
    fn empty_recycle_bin(&self) -> Result<()> {
        let _com = Com::init()?;
        // SAFETY: no window and no path: every drive's bin.
        unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION) }
    }
}

/// A call that says whether it worked as a `BOOL`, as a result: its
/// error is the thread's last.
fn succeeded(worked: bool) -> Result<()> {
    match worked {
        true => Ok(()),
        false => Err(Error::from_thread()),
    }
}

/// Enables the shutdown privilege of this process, which a user has but
/// which starts disabled: restarting, shutting down and sleeping need it.
fn allow_shutdown() -> Result<()> {
    let mut token = HANDLE::default();
    // SAFETY: the token opened here is closed below, whatever happens.
    unsafe {
        let access = TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY;
        OpenProcessToken(GetCurrentProcess(), access, &mut token)?;
        let enabled = enable_shutdown(token);
        let _ = CloseHandle(token);
        enabled
    }
}

/// Enables the shutdown privilege in `token`.
///
/// # Safety
/// `token` is open with the right to adjust its privileges.
unsafe fn enable_shutdown(token: HANDLE) -> Result<()> {
    let mut luid = LUID::default();
    // SAFETY: `luid` and the privileges outlive the calls.
    unsafe {
        LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid)?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, SetLastError};

    /// Windows as a test sees it: notes each call, carries out none.
    #[derive(Default)]
    struct Noted(RefCell<Vec<String>>);

    impl Noted {
        fn note(&self, call: String) -> Result<()> {
            self.0.borrow_mut().push(call);
            Ok(())
        }
    }

    impl Windows for Noted {
        fn lock(&self) -> Result<()> {
            self.note("lock".to_string())
        }

        fn suspend(&self, hibernate: bool) -> Result<()> {
            self.note(format!("suspend, hibernate {hibernate}"))
        }

        fn exit(&self, how: EXIT_WINDOWS_FLAGS) -> Result<()> {
            self.note(format!("exit {}", how.0))
        }

        fn empty_recycle_bin(&self) -> Result<()> {
            self.note("empty the bin".to_string())
        }
    }

    fn called(command: Command) -> Vec<String> {
        let windows = Noted::default();
        run(command, &windows).expect("noted");
        windows.0.into_inner()
    }

    #[test]
    fn each_command_makes_its_own_one_call() {
        assert_eq!(called(Command::Lock), ["lock"]);
        assert_eq!(called(Command::Sleep), ["suspend, hibernate false"]);
        assert_eq!(called(Command::Hibernate), ["suspend, hibernate true"]);
        assert_eq!(called(Command::Restart), [format!("exit {}", EWX_REBOOT.0)]);
        assert_eq!(
            called(Command::ShutDown),
            [format!("exit {}", EWX_POWEROFF.0)]
        );
        assert_eq!(called(Command::SignOut), [format!("exit {}", EWX_LOGOFF.0)]);
        assert_eq!(called(Command::EmptyRecycleBin), ["empty the bin"]);
    }

    #[test]
    fn a_call_that_failed_says_why() {
        assert_eq!(succeeded(true), Ok(()));
        // SAFETY: a plain call.
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        let error = succeeded(false).expect_err("failed");
        assert_eq!(error.code(), ERROR_ACCESS_DENIED.to_hresult());
    }
}
