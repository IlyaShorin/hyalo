//! The file index: every file and folder name on the drives, kept small
//! and searched fast. Pure: whoever walks the drives (or reads the NTFS
//! journal) feeds it entries and changes; it never touches the disk
//! itself, except through the snapshot the caller hands it.
//!
//! Names live in one arena, with a folded copy (as `matcher::folded`
//! gives it) in another; each file or folder is a 16-byte entry pointing
//! into both and at the folder it is in. Full paths are built only for
//! the rows a search returns.

mod index;

pub use index::{EntryId, Error, Index};
