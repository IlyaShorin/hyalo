//! What the service reads a volume through: a Windows call for each
//! (`ntfs::Ntfs`), or in tests an image of a volume held in memory
//! (`image::Image`).

use std::io;
use std::sync::atomic::AtomicBool;

use hyalo_core::volume::Journal;

/// How an NTFS volume lies: what `FSCTL_GET_NTFS_VOLUME_DATA` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// The volume's serial number: another after it is formatted again.
    pub serial: u64,
    /// Bytes in a cluster.
    pub cluster: u64,
    /// Bytes in a record of the master file table.
    pub record: u64,
    /// The cluster the table starts in.
    pub table: u64,
    /// How many of the table's bytes hold records.
    pub valid: u64,
}

/// A volume, as the service reads it.
pub trait Disk: Send + Sync {
    fn geometry(&self) -> io::Result<Geometry>;

    /// `length` bytes from the byte `at` on, both whole sectors; reads
    /// may run on several threads at once.
    fn read(&self, at: u64, length: usize) -> io::Result<Vec<u8>>;

    /// The record `number` as NTFS holds it now, if it is in use.
    fn record(&self, number: u32) -> io::Result<Option<Vec<u8>>>;

    /// The volume's journal of changes, made if it has none.
    fn journal(&self) -> io::Result<Journal>;

    /// Writes to the disk what NTFS holds of the volume in memory, so a
    /// read of the table sees every change made so far.
    fn flush(&self) -> io::Result<()>;

    /// What the journal `journal` holds from `from` on, as soon as it
    /// holds anything: the place to read on from, then records of changes
    /// (`usn::changed`). None once `stop` is set.
    fn changes(&self, from: i64, journal: u64, stop: &AtomicBool) -> io::Result<Option<Vec<u8>>>;

    /// The records of the files and folders in the folder `record`.
    fn children(&self, record: u32) -> io::Result<Vec<u32>>;
}

#[cfg(test)]
pub(crate) mod image;
