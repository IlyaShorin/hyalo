// Release builds have no console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod catalog;
mod com;
mod files;
mod highlight;
mod http;
mod keys;
mod launch;
mod layout;
mod query;
mod rates;
mod render;
mod results;
mod store;
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

fn main() -> windows::core::Result<()> {
    // DPI awareness is per process and must be set before the first window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    window::run()
}
