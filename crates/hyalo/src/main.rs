// Release builds have no console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod autostart;
mod catalog;
mod com;
mod files;
mod highlight;
mod hotkeys;
mod http;
mod instance;
mod keys;
mod launch;
mod layout;
mod query;
mod rates;
mod render;
mod results;
mod store;
mod tray;
mod window;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod scratch;
#[cfg(test)]
mod test_desktop;

use instance::Claim;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

fn main() -> windows::core::Result<()> {
    // Started with Windows: into the tray, not onto the screen.
    let shown = !std::env::args().any(|arg| arg == autostart::HIDDEN);
    // One launcher per session: a second start shows the first and goes.
    let _launcher = match instance::claim(instance::NAME)? {
        Claim::Ours(launcher) => launcher,
        Claim::Taken(running) => {
            // A build replacing the running one would wait on it here.
            drop(running);
            if shown {
                window::show_running();
            }
            return Ok(());
        }
    };
    // DPI awareness is per process and must be set before the first window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    window::run(shown)
}
