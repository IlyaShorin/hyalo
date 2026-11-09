// Release builds have no console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod layout;
mod query;
mod render;
mod window;

fn main() -> windows::core::Result<()> {
    window::run()
}
