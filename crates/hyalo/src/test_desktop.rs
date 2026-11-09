//! Test windows live on a hidden desktop, so `cargo test` (and every
//! mutator run) never flashes windows on screen or steals the user's focus.

use std::sync::OnceLock;

use windows::Win32::Foundation::GENERIC_ALL;
use windows::Win32::System::StationsAndDesktops::{
    CreateDesktopW, DESKTOP_CONTROL_FLAGS, HDESK, SetThreadDesktop,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::HSTRING;

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
