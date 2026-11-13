// Release builds have no console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apps;
mod autostart;
mod catalog;
mod clipboard;
mod com;
mod field;
mod files;
mod highlight;
mod hotkeys;
mod http;
mod install;
mod instance;
mod keys;
mod launch;
mod layout;
mod notice;
mod page;
mod paint;
mod panel;
mod picker;
mod picture;
mod popup;
mod rates;
mod recent;
mod recorder;
mod registry;
mod render;
mod results;
mod row_menu;
mod settings;
mod store;
mod surface;
mod switcher;
mod system;
mod tray;
mod uninstall;
mod update;
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

/// One launcher per session: the first start runs it, shown, in the tray
/// (started with Windows) or with its settings open (`--settings`); a
/// second start asks the first to show or to open its settings, and goes.
/// A new build an update started (`--replace`) waits for the old one to
/// end, then runs in its place. A release build started from anywhere
/// but `%LOCALAPPDATA%\Programs\hyalo` installs itself there first.
fn main() -> windows::core::Result<()> {
    let arguments: Vec<String> = std::env::args().collect();
    let start = instance::Start::from_args(arguments.clone());
    if install::elsewhere(&arguments, start) {
        return Ok(());
    }
    let replace = instance::replacing(&arguments);
    let wait = replace.then_some(instance::REPLACE_WAIT);
    let run = |start| run(start, replace);
    instance::first(instance::NAME, start, wait, run, window::ask_running)
}

fn run(start: instance::Start, replace: bool) -> windows::core::Result<()> {
    // DPI awareness is per process and must be set before the first window.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };
    window::run(start, replace)
}
