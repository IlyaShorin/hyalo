//! The indexer service: what hyalo reads of an NTFS volume with the
//! rights only an administrator has, so the launcher, which never has
//! them, need not walk the drives.
//!
//! The master file table lists every file and folder of a volume: read
//! whole, it gives every name, the folder it is in, its size and its last
//! write in a few seconds. The USN journal then says which files changed,
//! and the table is read again for those alone.

mod bytes;
pub mod listing;
pub mod mft;
pub mod usn;

pub use mft::Name;

/// The record of a volume's root folder, the folder its top-level files
/// and folders are in.
pub const ROOT: u32 = 5;

/// What a file is now, by its record's number: gone, or there with all
/// its names. A volume's listing is the changes from nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub record: u32,
    pub file: Option<File>,
}

/// What the index keeps of a file or folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    pub folder: bool,
    /// Bytes; none for a folder.
    pub size: u64,
    /// Last written, in seconds since 1970.
    pub written: i64,
    /// One for each hard link; a folder has one.
    pub names: Vec<Name>,
}
