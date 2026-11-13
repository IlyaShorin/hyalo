//! A volume held in memory, for tests: its master file table laid out on
//! its clusters as NTFS lays it out, in two stretches; beside it the
//! records as NTFS holds them now, which reach the clusters only when the
//! volume is flushed, as on a real volume; a journal; and each folder's
//! entries.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hyalo_core::volume::Journal;

use super::{Disk, Geometry};
use crate::mft::build::{Made, record, runs_bytes};
use crate::usn::build::{answer, change};

pub const CLUSTER: u64 = 4096;
pub const RECORD: u64 = 1024;
pub const SERIAL: u64 = 0x5E1A_0001;

/// How many records the table holds.
pub const RECORDS: u32 = 64;

/// Where the table lies: 8 clusters at 100, then 8 at 300.
const STRETCHES: [(u64, u64); 2] = [(100, 8), (300, 8)];

/// The clusters of the whole volume.
const CLUSTERS: u64 = 400;

/// The bytes a journal record takes, as the image counts them.
const STEP: i64 = 96;

/// A volume in memory.
pub struct Image {
    geometry: Geometry,
    clusters: Mutex<Vec<u8>>,
    now: Mutex<BTreeMap<u32, Made>>,
    journal: Mutex<Kept>,
    written: Condvar,
    children: Mutex<HashMap<u32, Vec<u32>>>,
    /// How often it was flushed.
    pub flushes: AtomicUsize,
}

/// The journal, and the records it holds, each at its place.
struct Kept {
    id: u64,
    lowest: i64,
    next: i64,
    records: Vec<(i64, Vec<u8>)>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Image {
    /// A volume whose table holds `records`, by number, on disk as NTFS
    /// holds them; its own record and the root's are made here.
    pub fn new(records: &[(u32, Made)]) -> Image {
        let mut now: BTreeMap<u32, Made> = records.iter().cloned().collect();
        let length = u64::from(RECORDS) * RECORD;
        let own = record()
            .name(5, "$MFT", 3)
            .data(0, length, &runs_bytes(&STRETCHES));
        now.entry(0).or_insert(own);
        now.entry(5).or_insert(record().folder().name(5, ".", 3));
        let image = Image {
            geometry: Geometry {
                serial: SERIAL,
                cluster: CLUSTER,
                record: RECORD,
                table: STRETCHES[0].0,
                valid: length,
            },
            clusters: Mutex::new(vec![0; (CLUSTERS * CLUSTER) as usize]),
            now: Mutex::new(now),
            journal: Mutex::new(Kept {
                id: 1,
                lowest: 0,
                next: 0,
                records: Vec::new(),
            }),
            written: Condvar::new(),
            children: Mutex::new(HashMap::new()),
            flushes: AtomicUsize::new(0),
        };
        image.write_back();
        image
    }

    /// Where the record `number` lies on the volume.
    pub fn place(number: u32) -> usize {
        let offset = u64::from(number) * RECORD;
        let cluster = offset / CLUSTER;
        let (first, length) = STRETCHES[0];
        let lcn = match cluster < length {
            true => first + cluster,
            false => STRETCHES[1].0 + cluster - length,
        };
        (lcn * CLUSTER + offset % CLUSTER) as usize
    }

    /// Writes every record as NTFS holds it now to the clusters.
    fn write_back(&self) {
        let now = locked(&self.now);
        let mut clusters = locked(&self.clusters);
        for number in 0..RECORDS {
            let at = Image::place(number);
            let bytes = match now.get(&number) {
                Some(made) => made.on_disk(RECORD as usize),
                None => vec![0; RECORD as usize],
            };
            clusters[at..at + bytes.len()].copy_from_slice(&bytes);
        }
    }

    /// Writes `bytes` to the clusters at `at`, as a sector torn or a
    /// record NTFS never wrote leaves them.
    pub fn scribble(&self, at: usize, bytes: &[u8]) {
        locked(&self.clusters)[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Makes the record `number` hold `made`, or nothing, as NTFS holds it
    /// now: the clusters see it once flushed. The journal tells of it.
    pub fn change(&self, number: u32, made: Option<Made>) {
        match made {
            Some(made) => locked(&self.now).insert(number, made),
            None => locked(&self.now).remove(&number),
        };
        let mut journal = locked(&self.journal);
        let at = journal.next;
        journal.records.push((at, change(number, 5, "changed")));
        journal.next += STEP;
        self.written.notify_all();
    }

    /// Lets go of the journal's records before `before`.
    pub fn let_go(&self, before: i64) {
        let mut journal = locked(&self.journal);
        journal.lowest = before;
        journal.records.retain(|&(at, _)| at >= before);
    }

    /// Makes the journal again: another, from where the old one ended.
    pub fn renew_journal(&self) {
        let mut journal = locked(&self.journal);
        journal.id += 1;
        journal.lowest = journal.next;
        journal.records.clear();
    }

    /// Makes the folder `folder` hold `entries`.
    pub fn hold(&self, folder: u32, entries: &[u32]) {
        locked(&self.children).insert(folder, entries.to_vec());
    }
}

impl Disk for Image {
    fn geometry(&self) -> io::Result<Geometry> {
        Ok(self.geometry)
    }

    fn read(&self, at: u64, length: usize) -> io::Result<Vec<u8>> {
        let whole = at.is_multiple_of(512) && length.is_multiple_of(512);
        let clusters = locked(&self.clusters);
        let bytes = clusters.get(at as usize..at as usize + length);
        match (whole, bytes) {
            (true, Some(bytes)) => Ok(bytes.to_vec()),
            _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        }
    }

    fn record(&self, number: u32) -> io::Result<Option<Vec<u8>>> {
        let now = locked(&self.now);
        Ok(now.get(&number).map(|made| made.bytes(RECORD as usize)))
    }

    fn journal(&self) -> io::Result<Journal> {
        let journal = locked(&self.journal);
        Ok(Journal {
            id: journal.id,
            lowest: journal.lowest,
            next: journal.next,
        })
    }

    fn flush(&self) -> io::Result<()> {
        self.write_back();
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn changes(&self, from: i64, journal: u64, stop: &AtomicBool) -> io::Result<Option<Vec<u8>>> {
        let mut kept = locked(&self.journal);
        loop {
            if kept.id != journal || from < kept.lowest {
                return Err(io::Error::other("the journal let go of that place"));
            }
            let held = kept.records.iter().filter(|&&(at, _)| at >= from);
            let held: Vec<Vec<u8>> = held.map(|(_, bytes)| bytes.clone()).collect();
            if !held.is_empty() {
                return Ok(Some(answer(kept.next, &held)));
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let waited = self.written.wait_timeout(kept, Duration::from_millis(20));
            kept = waited.unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn children(&self, record: u32) -> io::Result<Vec<u32>> {
        let children = locked(&self.children);
        Ok(children.get(&record).cloned().unwrap_or_default())
    }
}
