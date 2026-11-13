//! The service's process as the service manager runs it: it tells the
//! manager it runs, serves the pipe to the users the service's parameters
//! list until it is stopped or nobody used it for a minute, then tells the
//! manager it stopped, and the process ends. The calls are checked by
//! reading them, and live; what the service does for each control the
//! manager sends is decided in `heard`, tested.

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows::core::{HSTRING, PWSTR};

use crate::PIPE;
use crate::disk::Disk;
use crate::manager::Manager;
use crate::ntfs::Ntfs;
use crate::overlapped::os;
use crate::scm::{self, Services};
use crate::security;
use crate::server;

/// How long the service waits for the launcher before it ends.
pub const IDLE: Duration = Duration::from_secs(60);

/// Set once the manager asks the service to stop.
static STOP: AtomicBool = AtomicBool::new(false);

/// Where the service tells the manager how it is.
static STATUS: OnceLock<Status> = OnceLock::new();

struct Status(SERVICE_STATUS_HANDLE);

// SAFETY: the manager's handle for a service's status may be used from any
// of the service's threads.
unsafe impl Send for Status {}
// SAFETY: as above.
unsafe impl Sync for Status {}

/// Hands this process to the service manager, which runs the service on a
/// thread of its own; back once it stopped.
pub fn dispatch() -> io::Result<()> {
    let name = HSTRING::from(scm::NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_ptr().cast_mut()),
            lpServiceProc: Some(main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: the table and the name it holds outlive the call, which
    // returns once the service stopped.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.map_err(os)
}

/// The service, as the manager starts it.
unsafe extern "system" fn main(_: u32, _: *mut PWSTR) {
    // SAFETY: a plain call; the handler lives as long as the process.
    let registered =
        unsafe { RegisterServiceCtrlHandlerExW(&HSTRING::from(scm::NAME), Some(handler), None) };
    let Ok(status) = registered else {
        return;
    };
    let _ = STATUS.set(Status(status));
    report(SERVICE_RUNNING);
    if let Err(error) = serve(&STOP) {
        eprintln!("hyalo indexer: {error}");
    }
    report(SERVICE_STOPPED);
}

/// What the manager sends the service.
unsafe extern "system" fn handler(
    control: u32,
    _: u32,
    _: *mut core::ffi::c_void,
    _: *mut core::ffi::c_void,
) -> u32 {
    let (stop, reply) = heard(control);
    if stop {
        STOP.store(true, Ordering::SeqCst);
        report(SERVICE_STOP_PENDING);
    }
    reply
}

/// Whether the control `control` stops the service, and what the service
/// replies: stopping or shutting down stops it, asking how it is is
/// answered, nothing else is known.
fn heard(control: u32) -> (bool, u32) {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => (true, NO_ERROR.0),
        SERVICE_CONTROL_INTERROGATE => (false, NO_ERROR.0),
        _ => (false, ERROR_CALL_NOT_IMPLEMENTED.0),
    }
}

/// Tells the manager the service is in `state`.
fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let Some(status) = STATUS.get() else {
        return;
    };
    let told = told(state);
    // SAFETY: a plain call with a handle the manager gave.
    let _ = unsafe { SetServiceStatus(status.0, &told) };
}

/// What the service tells the manager in `state`: running, it takes stop
/// and shut down; stopping, it asks for a few seconds.
fn told(state: SERVICE_STATUS_CURRENT_STATE) -> SERVICE_STATUS {
    let running = state == SERVICE_RUNNING;
    SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: match running {
            true => SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            false => 0,
        },
        dwWaitHint: match state == SERVICE_STOP_PENDING {
            true => 5_000,
            false => 0,
        },
        ..Default::default()
    }
}

/// Serves the pipe, reading real volumes, to the users the service's
/// parameters list, until `stop` is set or nobody used it for a while.
fn serve(stop: &AtomicBool) -> io::Result<()> {
    let users = Manager.users(scm::NAME)?;
    let open = |drive| Ntfs::open(drive).map(|ntfs| Box::new(ntfs) as Box<dyn Disk>);
    server::serve(PIPE, &security::pipe_sddl(&users), &open, stop, IDLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_and_shut_down_stop_the_service_and_nothing_else_does() {
        assert_eq!(heard(SERVICE_CONTROL_STOP), (true, NO_ERROR.0));
        assert_eq!(heard(SERVICE_CONTROL_SHUTDOWN), (true, NO_ERROR.0));
        assert_eq!(heard(SERVICE_CONTROL_INTERROGATE), (false, NO_ERROR.0));
        assert_eq!(heard(77), (false, ERROR_CALL_NOT_IMPLEMENTED.0));
    }

    #[test]
    fn a_running_service_takes_stop_and_a_stopping_one_asks_for_time() {
        let running = told(SERVICE_RUNNING);
        assert_eq!(
            running.dwControlsAccepted,
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        );
        assert_eq!(running.dwServiceType, SERVICE_WIN32_OWN_PROCESS);
        assert_eq!(running.dwWaitHint, 0);
        let stopping = told(SERVICE_STOP_PENDING);
        assert_eq!(
            (stopping.dwControlsAccepted, stopping.dwWaitHint),
            (0, 5_000)
        );
        let stopped = told(SERVICE_STOPPED);
        assert_eq!((stopped.dwControlsAccepted, stopped.dwWaitHint), (0, 0));
        assert_eq!(stopped.dwCurrentState, SERVICE_STOPPED);
    }
}
