//! Carries out system commands: thin glue, one Windows call each. Which
//! commands there are, what they are called and that every one but Lock
//! asks first is decided in `hyalo_core::system_commands` and `results`.
//!
//! No test runs these: window tests hand the launcher a stand-in runner,
//! and nothing here is run by hand either while the user is away (it
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

/// Carries out `command`. Waits for Windows, so call it off the UI
/// thread.
pub fn run(command: Command) -> Result<()> {
    match command {
        // SAFETY: a plain call.
        Command::Lock => unsafe { LockWorkStation() },
        Command::Sleep => suspend(false),
        Command::Hibernate => suspend(true),
        Command::Restart => exit_windows(EWX_REBOOT),
        Command::ShutDown => exit_windows(EWX_POWEROFF),
        Command::SignOut => exit_windows(EWX_LOGOFF),
        Command::EmptyRecycleBin => empty_recycle_bin(),
    }
}

/// Whether Windows has hibernation turned on, so Hibernate is offered.
pub fn hibernates() -> bool {
    // SAFETY: a plain call.
    unsafe { IsPwrHibernateAllowed() }
}

/// Sleeps, or hibernates.
fn suspend(hibernate: bool) -> Result<()> {
    allow_shutdown()?;
    // SAFETY: a plain call; it returns once the machine wakes again.
    match unsafe { SetSuspendState(hibernate, false, false) } {
        true => Ok(()),
        false => Err(Error::from_thread()),
    }
}

/// Restarts, shuts down or signs out, as a planned shutdown.
fn exit_windows(how: EXIT_WINDOWS_FLAGS) -> Result<()> {
    allow_shutdown()?;
    let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
    // SAFETY: a plain call.
    unsafe { ExitWindowsEx(how, reason) }
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

/// Empties the Recycle Bin of every drive; the row already asked, so
/// Windows does not ask again.
fn empty_recycle_bin() -> Result<()> {
    let _com = Com::init()?;
    // SAFETY: no window and no path: every drive's bin.
    unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION) }
}
