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
mod notice;
mod query;
mod rates;
mod render;
mod results;
mod settings;
mod store;
mod tray;
mod window;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod scratch;
#[cfg(test)]
mod test_desktop;

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// One launcher per session: the first start runs it, shown or, started
/// with Windows, in the tray; a second start shows the first and goes.
fn main() -> windows::core::Result<()> {
    let shown = autostart::shown(std::env::args());
    instance::first(instance::NAME, shown, run, window::show_running)
}

fn run(shown: bool) -> windows::core::Result<()> {
    // DPI awareness is per process and must be set before the first window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    window::run(shown)
}
