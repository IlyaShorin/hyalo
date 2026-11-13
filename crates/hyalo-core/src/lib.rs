//! The launcher's pure core: the items it lists, how a query matches them
//! and how they are ranked. No Win32 here; everything is tested directly.

pub mod calculator;
pub mod clipboard;
pub mod config;
pub mod currency;
pub mod expression;
pub mod filter;
pub mod frecency;
pub mod hotkey;
pub mod item;
pub mod keyboard;
pub mod matcher;
pub mod places;
pub mod rank;
pub mod rates;
pub mod switcher;
pub mod system_commands;
pub mod system_settings;
pub mod text;
pub mod uninstall;
pub mod update;
pub mod web;
