//! Test windows live on a hidden desktop, so `cargo test` (and every
//! mutator run) never flashes windows on screen or steals the user's focus.
//! Tests that use the clipboard run in a child process with a window
//! station of its own, so they never overwrite what the user copied.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::GENERIC_ALL;
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, CreateWindowStationW, DESKTOP_CONTROL_FLAGS, HDESK, SetProcessWindowStation,
    SetThreadDesktop,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::{HSTRING, PCWSTR};

/// Runs `test` on a fresh thread attached to the hidden test desktop.
/// A thread can only switch desktops before it owns a window, hence the
/// new thread.
pub fn on_hidden_desktop(test: impl FnOnce() + Send + 'static) {
    let desktop = hidden_desktop();
    let outcome = std::thread::spawn(move || {
        let desktop = HDESK(desktop as *mut _);
        unsafe { SetThreadDesktop(desktop) }.expect("attach to the test desktop");
        test();
    })
    .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The desktop handle, as an integer so it can cross threads. Created once
/// per test process, together with the DPI awareness `main` sets up.
fn hidden_desktop() -> isize {
    static DESKTOP: OnceLock<isize> = OnceLock::new();
    *DESKTOP.get_or_init(|| {
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .expect("per-monitor DPI awareness");
        let name = HSTRING::from(format!("hyalo-test-{}", std::process::id()));
        let desktop = unsafe {
            CreateDesktopW(
                &name,
                None,
                None,
                DESKTOP_CONTROL_FLAGS(0),
                GENERIC_ALL.0,
                None,
            )
        }
        .expect("create the test desktop");
        desktop.0 as isize
    })
}

/// Set in a child process that runs one test in a window station of its
/// own.
const OWN_STATION: &str = "HYALO_TEST_OWN_WINDOW_STATION";

/// Runs `test` in a child process attached to a window station of its
/// own. The clipboard belongs to a window station, and the hidden desktop
/// shares the user's: a test that copies there would overwrite what the
/// user copied. `name` is the test's path (`launch::tests::...`); the
/// child runs that test alone.
pub fn in_own_window_station(name: &str, test: impl FnOnce()) {
    if std::env::var_os(OWN_STATION).is_some() {
        let _turn = take_turn("window-station");
        enter_own_window_station();
        test();
        return;
    }
    let output = alone(name, OWN_STATION);
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    let ran = out.contains("1 passed");
    assert!(
        output.status.success() && ran,
        "{name} in its own window station:\n{out}\n{err}"
    );
}

/// Set in a child process that runs one test and then exits.
const EXITING: &str = "HYALO_TEST_EXITING";

/// The exit code of a child process that runs `test` on the hidden
/// desktop and then exits from that same thread, as hyalo does when
/// `main` returns: what the thread still holds goes only as the process
/// exits, after Windows has stopped every other thread. A test thread
/// that ends cleans up while the rest of the process still runs, so only
/// a process that exits shows what that costs. `name` is the test's path;
/// the child runs that test alone. The child's output is shown only when
/// the test fails.
pub fn exit_code_on_hidden_desktop(name: &str, test: impl FnOnce() + Send + 'static) -> i32 {
    if std::env::var_os(EXITING).is_some() {
        on_hidden_desktop(|| {
            test();
            std::process::exit(0);
        });
    }
    let output = alone(name, EXITING);
    let out = String::from_utf8_lossy(&output.stdout);
    print!("{out}{}", String::from_utf8_lossy(&output.stderr));
    assert!(out.contains("running 1 test"), "{name} did not run");
    output.status.code().expect("an exit code")
}

/// Runs the test `name` alone in a child process with `flag` set.
fn alone(name: &str, flag: &str) -> Output {
    Command::new(std::env::current_exe().expect("the test binary"))
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(flag, "1")
        .output()
        .expect("run the test in a child process")
}

/// Waits for this process's turn at `what` the whole session shares, and
/// holds it until the file is dropped: test runs side by side, as mutator
/// runs them, would otherwise meet there. A normal user may not name a
/// window station, and one without a name is named after the logon
/// session, so every child gets the same clipboard; global hotkeys are
/// the session's too. The turn ends with the process at the latest.
pub fn take_turn(what: &str) -> File {
    let path = std::env::temp_dir().join(format!("hyalo-test-{what}.lock"));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        // No sharing: a second open fails until the first is closed.
        let opened = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path);
        match opened {
            Ok(turn) => return turn,
            Err(error) => assert!(Instant::now() < deadline, "no turn: {error}"),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Moves this process into a new window station, with its own clipboard.
fn enter_own_window_station() {
    // SAFETY: plain calls; the station handle stays open for the process.
    unsafe {
        let station = CreateWindowStationW(PCWSTR::null(), 0, GENERIC_ALL.0, None)
            .expect("create a window station");
        SetProcessWindowStation(station).expect("enter the window station");
    }
}
