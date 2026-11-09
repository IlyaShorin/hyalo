// Release builds have no console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "listed once the window shows results")
)]
mod apps;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "joined once the window lists apps")
)]
mod com;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "drawn once the window lists results")
)]
mod highlight;
mod layout;
mod query;
mod render;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "listed once the window shows results")
)]
mod results;
#[cfg_attr(not(test), expect(dead_code, reason = "used once Enter records picks"))]
mod store;
mod window;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod test_desktop;

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

fn main() -> windows::core::Result<()> {
    // DPI awareness is per process and must be set before the first window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    window::run()
}
