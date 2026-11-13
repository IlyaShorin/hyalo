//! The indexer service: what hyalo reads of an NTFS volume with the
//! rights only an administrator has, so the launcher, which never has
//! them, need not walk the drives.
//!
//! The master file table lists every file and folder of a volume: read
//! whole, it gives every name, the folder it is in, its size and its last
//! write in a few seconds. The USN journal then says which files changed,
//! and the table is read again for those alone.

mod bytes;
pub mod directory;
pub mod disk;
pub mod listing;
pub mod mft;
pub mod ntfs;
pub mod read;
pub mod usn;
