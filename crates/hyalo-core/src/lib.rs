//! The launcher's pure core: the items it lists, how a query matches them
//! and how they are ranked. No Win32 here; everything is tested directly.

pub mod calculator;
pub mod currency;
pub mod expression;
pub mod frecency;
pub mod hotkey;
pub mod item;
pub mod keyboard;
pub mod matcher;
pub mod places;
pub mod rank;
pub mod rates;
