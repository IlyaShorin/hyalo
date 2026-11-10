//! Test windows live on a hidden desktop, so `cargo test` (and every
//! mutator run) never flashes windows on screen or steals the user's focus.
//! Tests that use the clipboard run in a child process with a window
//! station of its own, so they never overwrite what the user copied.
//! Tests that wait for the shell do it `within` a deadline, so a call
//! that never returns fails its test instead of holding up the run.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::process::Command;
use std::sync::{OnceLock, mpsc};
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
    let output = Command::new(std::env::current_exe().expect("the test binary"))
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(OWN_STATION, "1")
        .output()
        .expect("run the test in a child process");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    let ran = out.contains("1 passed");
    assert!(
        output.status.success() && ran,
        "{name} in its own window station:\n{out}\n{err}"
    );
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

/// Runs `work` on a thread of its own and hands back what it returns, or
/// fails the test if it has not returned within `deadline`. A test run
/// waits for every test, and nothing kills a test: one call that never
/// returns would hold up the run for good (the shell did, under eight
/// test suites at once). The stuck thread is left; it ends with the
/// process.
#[track_caller]
pub fn within<T: Send + 'static>(
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (answer, answered) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(work));
        // The test may have given up and gone; then no one is listening.
        let _ = answer.send(outcome);
    });
    let Ok(outcome) = answered.recv_timeout(deadline) else {
        panic!("no answer within {deadline:?}; the work is left running");
    };
    outcome.unwrap_or_else(|panic| resume_unwind(panic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    /// What a panic said.
    fn said(panic: Box<dyn Any + Send>) -> String {
        match panic.downcast::<String>() {
            Ok(text) => *text,
            Err(panic) => panic
                .downcast::<&str>()
                .map(|text| text.to_string())
                .unwrap_or_default(),
        }
    }

    #[test]
    fn work_that_never_returns_fails_the_test_once_the_deadline_passes() {
        // The stand-in waits for a word the test sends only after `within`
        // has given up: as far as `within` can tell, it never returns.
        let (release, held) = mpsc::channel::<()>();
        let started = Instant::now();
        let deadline = Duration::from_millis(200);
        let outcome = catch_unwind(AssertUnwindSafe(|| within(deadline, move || held.recv())));
        let waited = started.elapsed();
        drop(release);
        let panic = outcome.expect_err("the test fails");
        assert_eq!(
            said(panic),
            "no answer within 200ms; the work is left running"
        );
        assert!(waited >= deadline, "gave up after {waited:?}");
        assert!(waited < Duration::from_secs(5), "gave up after {waited:?}");
    }

    #[test]
    fn what_the_work_returns_comes_back() {
        assert_eq!(within(Duration::from_secs(5), || 42), 42);
    }

    #[test]
    fn work_that_fails_fails_the_test_in_its_own_words() {
        let fails = || -> u8 { panic!("Explorer is not listed") };
        let outcome = catch_unwind(|| within(Duration::from_secs(5), fails));
        assert_eq!(
            said(outcome.expect_err("the test fails")),
            "Explorer is not listed"
        );
    }
}
