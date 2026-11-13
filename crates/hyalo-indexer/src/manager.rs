//! Windows' service manager, a call or two for each of `scm::Services`,
//! and the registry key that keeps whom the service serves. Asking how a
//! service is and reading the key need no rights: those are tested; the
//! rest needs an administrator's and is checked by reading, and live.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_NOT_ACTIVE, WIN32_ERROR,
};
use windows::Win32::Security::DACL_SECURITY_INFORMATION;
use windows::Win32::Storage::FileSystem::{DELETE, WRITE_DAC};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_MULTI_SZ, RRF_RT_REG_MULTI_SZ, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ALL_ACCESS,
    SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START,
    SERVICE_DESCRIPTIONW, SERVICE_ERROR, SERVICE_ERROR_NORMAL, SERVICE_NO_CHANGE,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    SERVICE_STOP, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::overlapped::os;
use crate::scm::{DESCRIPTION, DISPLAY, Services, State};
use crate::security::Security;

/// How long a stop may take before the service is given up on.
const STOPPING: Duration = Duration::from_secs(10);

/// Windows' own service manager.
pub struct Manager;

/// A handle of the service manager's, closed when dropped.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed only here.
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

/// The service manager, for `access`.
fn manager(access: u32) -> io::Result<Handle> {
    // SAFETY: a plain call; the handle is closed by `Handle`.
    let opened = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) };
    opened.map(Handle).map_err(os)
}

/// The service `name`, for `access`.
fn service(name: &str, access: u32) -> io::Result<Handle> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    // SAFETY: a plain call; the handle is closed by `Handle`.
    let opened = unsafe { OpenServiceW(manager.0, &HSTRING::from(name), access) };
    opened.map(Handle).map_err(os)
}

/// The key that keeps the service `name`'s parameters, under the
/// machine's.
fn parameters(name: &str) -> HSTRING {
    HSTRING::from(format!(
        r"SYSTEM\CurrentControlSet\Services\{name}\Parameters"
    ))
}

/// Whether `error` is the Win32 error `code`.
fn is(error: &io::Error, code: WIN32_ERROR) -> bool {
    error.raw_os_error() == Some(code.0 as i32)
}

impl Services for Manager {
    fn state(&self, name: &str) -> io::Result<Option<State>> {
        match service(name, SERVICE_QUERY_STATUS) {
            Err(error) if is(&error, ERROR_SERVICE_DOES_NOT_EXIST) => Ok(None),
            opened => status_of(&opened?).map(Some),
        }
    }

    fn create(&self, name: &str, command: &str) -> io::Result<()> {
        let manager = manager(SC_MANAGER_CREATE_SERVICE)?;
        // SAFETY: plain call; every string outlives it; the handle it gives
        // is closed by `Handle`. No account: the local system's.
        let made = unsafe {
            CreateServiceW(
                manager.0,
                &HSTRING::from(name),
                &HSTRING::from(DISPLAY),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_DEMAND_START,
                SERVICE_ERROR_NORMAL,
                &HSTRING::from(command),
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
            )
        };
        made.map(drop).map_err(os)
    }

    fn configure(&self, name: &str, command: &str) -> io::Result<()> {
        let service = service(name, SERVICE_CHANGE_CONFIG)?;
        let unchanged = windows::Win32::System::Services::ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE);
        // SAFETY: plain call; every string outlives it.
        let changed = unsafe {
            ChangeServiceConfigW(
                service.0,
                unchanged,
                SERVICE_DEMAND_START,
                SERVICE_ERROR(SERVICE_NO_CHANGE),
                &HSTRING::from(command),
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
                &HSTRING::from(DISPLAY),
            )
        };
        changed.map_err(os)
    }

    fn describe(&self, name: &str) -> io::Result<()> {
        let service = service(name, SERVICE_CHANGE_CONFIG)?;
        let text = HSTRING::from(DESCRIPTION);
        let description = SERVICE_DESCRIPTIONW {
            lpDescription: PWSTR(text.as_ptr().cast_mut()),
        };
        // SAFETY: the description and its text outlive the call, which
        // only reads them.
        let described = unsafe {
            ChangeServiceConfig2W(
                service.0,
                SERVICE_CONFIG_DESCRIPTION,
                Some((&raw const description).cast()),
            )
        };
        described.map_err(os)
    }

    fn secure(&self, name: &str, sddl: &str) -> io::Result<()> {
        service(name, WRITE_DAC.0).and_then(|service| secured(&service, sddl))
    }

    fn users(&self, name: &str) -> io::Result<Vec<String>> {
        match read_users(name) {
            Err(error) if is(&error, ERROR_FILE_NOT_FOUND) => Ok(Vec::new()),
            other => other,
        }
    }

    fn set_users(&self, name: &str, users: &[String]) -> io::Result<()> {
        let units = to_multi_sz(users);
        // SAFETY: the data outlives the call, which only reads it.
        let set = unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                &parameters(name),
                &HSTRING::from(USERS),
                REG_MULTI_SZ.0,
                Some(units.as_ptr().cast()),
                (units.len() * 2) as u32,
            )
        };
        set.ok().map_err(os)
    }

    fn start(&self, name: &str) -> io::Result<()> {
        let service = service(name, SERVICE_START)?;
        // SAFETY: a plain call on a handle that is ours.
        let started = unsafe { StartServiceW(service.0, None) }.map_err(os);
        tolerating(started, ERROR_SERVICE_ALREADY_RUNNING)
    }

    fn stop(&self, name: &str) -> io::Result<()> {
        let service = service(name, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
        let mut status = SERVICE_STATUS::default();
        // SAFETY: a plain call on a handle that is ours.
        let asked = unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) };
        let asked = tolerating(asked.map_err(os), ERROR_SERVICE_NOT_ACTIVE);
        let done = || status_of(&service).map(|state| state.process == 0);
        asked.and_then(|()| waited(STOPPING, done))
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        let service = service(name, DELETE.0)?;
        // SAFETY: a plain call on a handle that is ours.
        unsafe { DeleteService(service.0) }.map_err(os)
    }
}

/// Gives the service of `service` the security `sddl` says.
fn secured(service: &Handle, sddl: &str) -> io::Result<()> {
    let security = Security::new(sddl)?;
    // SAFETY: the descriptor outlives the call.
    let set = unsafe {
        windows::Win32::System::Services::SetServiceObjectSecurity(
            service.0,
            DACL_SECURITY_INFORMATION,
            security.0,
        )
    };
    set.map_err(os)
}

/// The value that lists whom the service serves.
const USERS: &str = "Users";

/// The SIDs the service's parameters list.
fn read_users(name: &str) -> io::Result<Vec<String>> {
    let mut room = vec![0_u16; 4096];
    let mut length = size_of_val(room.as_slice()) as u32;
    // SAFETY: the room is ours and as long as said.
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &parameters(name),
            &HSTRING::from(USERS),
            RRF_RT_REG_MULTI_SZ,
            None,
            Some(room.as_mut_ptr().cast()),
            Some(&mut length),
        )
    };
    read.ok().map_err(os)?;
    Ok(multi_sz(&room[..length as usize / 2]))
}

/// How the service of `service` is.
fn status_of(service: &Handle) -> io::Result<State> {
    let mut room = vec![0_u8; size_of::<SERVICE_STATUS_PROCESS>()];
    let mut needed = 0;
    // SAFETY: the room is ours and as long as a SERVICE_STATUS_PROCESS.
    let asked = unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            Some(&mut room),
            &mut needed,
        )
    };
    asked.map_err(os)?;
    // SAFETY: Windows wrote a SERVICE_STATUS_PROCESS into the room.
    let status = unsafe {
        room.as_ptr()
            .cast::<SERVICE_STATUS_PROCESS>()
            .read_unaligned()
    };
    Ok(State {
        running: status.dwCurrentState == SERVICE_RUNNING,
        process: status.dwProcessId,
    })
}

/// What was `done`, unless it failed with `code`, which is no failure.
fn tolerating(done: io::Result<()>, code: WIN32_ERROR) -> io::Result<()> {
    match done {
        Err(error) if is(&error, code) => Ok(()),
        other => other,
    }
}

/// Waits up to `longest` for `done` to say so, asking every tenth of a
/// second.
fn waited(longest: Duration, done: impl Fn() -> io::Result<bool>) -> io::Result<()> {
    let until = Instant::now() + longest;
    while !done()? {
        if Instant::now() > until {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// The strings a REG_MULTI_SZ holds: each ends in a nought, the list in
/// another.
pub fn multi_sz(units: &[u16]) -> Vec<String> {
    let strings = units.split(|&unit| unit == 0);
    let strings = strings.take_while(|string| !string.is_empty());
    strings.map(String::from_utf16_lossy).collect()
}

/// `strings` as a REG_MULTI_SZ.
pub fn to_multi_sz(strings: &[String]) -> Vec<u16> {
    let mut units: Vec<u16> = Vec::new();
    for string in strings {
        units.extend(string.encode_utf16());
        units.push(0);
    }
    units.push(0);
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multi_sz_is_strings_each_ending_in_a_nought_then_another() {
        let users = ["S-1-5-18".to_string(), "S-1-5-21-1-2".to_string()];
        let units = to_multi_sz(&users);
        assert_eq!(units.iter().filter(|&&unit| unit == 0).count(), 3);
        assert_eq!(multi_sz(&units), users);
        assert_eq!(to_multi_sz(&[]), [0]);
        assert_eq!(multi_sz(&[0]), Vec::<String>::new());
        assert_eq!(multi_sz(&[]), Vec::<String>::new());
        let unended: Vec<u16> = "S-1-5-18".encode_utf16().collect();
        assert_eq!(multi_sz(&unended), ["S-1-5-18"], "the last unended");
    }

    #[test]
    fn a_service_windows_runs_says_how_it_is_and_one_there_is_not_says_none() {
        // The event log runs on every Windows, and asking needs no rights.
        let log = Manager.state("EventLog").expect("asked");
        assert!(log.is_some_and(|log| log.running && log.process != 0));
        let none = Manager.state("hyalo-test-no-such-service").expect("asked");
        assert_eq!(none, None);
    }

    #[test]
    fn an_error_tolerated_is_no_failure_and_others_are() {
        let running = io::Error::from_raw_os_error(ERROR_SERVICE_ALREADY_RUNNING.0 as i32);
        assert!(tolerating(Err(running), ERROR_SERVICE_ALREADY_RUNNING).is_ok());
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(tolerating(Err(denied), ERROR_SERVICE_ALREADY_RUNNING).is_err());
        assert!(tolerating(Ok(()), ERROR_SERVICE_NOT_ACTIVE).is_ok());
    }

    #[test]
    fn a_wait_ends_once_done_or_too_long_or_failed() {
        let asked = std::cell::Cell::new(0);
        let third = || {
            asked.set(asked.get() + 1);
            Ok(asked.get() == 3)
        };
        assert!(waited(Duration::from_secs(5), third).is_ok());
        assert_eq!(asked.get(), 3);
        let never = waited(Duration::from_millis(250), || Ok(false));
        assert_eq!(
            never.map_err(|error| error.kind()),
            Err(io::ErrorKind::TimedOut)
        );
        let failed = waited(Duration::from_secs(5), || Err(io::ErrorKind::Other.into()));
        assert!(failed.is_err());
    }

    #[test]
    fn a_service_with_no_parameters_serves_nobody() {
        let users = Manager.users("hyalo-test-no-such-service").expect("read");
        assert_eq!(users, Vec::<String>::new());
    }
}
