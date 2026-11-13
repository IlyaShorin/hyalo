//! The master file table: a record for every file and folder of an NTFS
//! volume, each a list of attributes. Pure: it reads bytes the volume
//! gave, and checks them where they come in, since names are anyone's to
//! choose and this runs with the rights of the system.

use hyalo_core::volume::Name;

use crate::bytes::{i64_at, number, u16_at, u32_at, u64_at};

/// Bytes of a record each entry of its update sequence stands for: NTFS's
/// own sector, whatever the disk's.
const STRIDE: usize = 512;

const STANDARD_INFORMATION: u32 = 0x10;
const ATTRIBUTE_LIST: u32 = 0x20;
const FILE_NAME: u32 = 0x30;
const DATA: u32 = 0x80;
const END: u32 = 0xFFFF_FFFF;

/// A record's flags: whether it holds a file at all, and whether that is
/// a folder.
const IN_USE: u16 = 1;
const DIRECTORY: u16 = 2;

/// The namespace of a short 8.3 name kept beside the long one.
const DOS: u8 = 2;

/// What the index needs of a record that holds a file or a folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Record {
    /// Counts how often the record was given to a file.
    pub sequence: u16,
    /// For an extension record, which holds attributes a file's own record
    /// had no room for: that record's number.
    pub base: Option<u32>,
    pub folder: bool,
    /// Every name the file has, one per hard link, without short 8.3
    /// names.
    pub names: Vec<Name>,
    /// When it was last written, in seconds since 1970; only a file's own
    /// record says.
    pub written: Option<i64>,
    /// The bytes of its unnamed data stream, if this record holds the
    /// stream's start.
    pub size: Option<u64>,
    /// The other records its attribute list says hold its names or data.
    pub elsewhere: Vec<u32>,
}

impl Record {
    /// Takes in what an extension record of this file holds.
    pub fn merge(&mut self, extension: Record) {
        self.names.extend(extension.names);
        self.size = self.size.or(extension.size);
    }
}

/// Windows' time, hundreds of nanoseconds since 1601 (UTC), in seconds
/// since 1970.
pub fn seconds(filetime: i64) -> i64 {
    filetime.div_euclid(10_000_000) - 11_644_473_600
}

/// Puts back the bytes the update sequence keeps of each sector's end,
/// as a record read straight from the disk has them; whether every sector
/// was written whole. A record NTFS hands over through its own calls has
/// them back already.
pub fn fix(record: &mut [u8]) -> bool {
    let (Some(offset), Some(count)) = (u16_at(record, 4), u16_at(record, 6)) else {
        return false;
    };
    let (offset, sectors) = (offset as usize, (count as usize).saturating_sub(1));
    if sectors == 0 || sectors * STRIDE != record.len() {
        return false;
    }
    let Some(sequence) = record.get(offset..offset + 2 * (sectors + 1)) else {
        return false;
    };
    let sequence = sequence.to_vec();
    let check = &sequence[..2];
    let ends = (1..=sectors).map(|sector| sector * STRIDE - 2);
    if ends.clone().any(|end| record[end..end + 2] != *check) {
        return false;
    }
    for (sector, end) in ends.enumerate() {
        let kept = 2 * (sector + 1);
        record[end..end + 2].copy_from_slice(&sequence[kept..kept + 2]);
    }
    true
}

/// What a record says, if it holds a file or a folder and reads whole.
pub fn parse(record: &[u8]) -> Option<Record> {
    if record.get(..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(record, 0x16)?;
    if flags & IN_USE == 0 {
        return None;
    }
    let base = number(u64_at(record, 0x20)?)?;
    let mut parsed = Record {
        sequence: u16_at(record, 0x10)?,
        base: (base != 0).then_some(base),
        folder: flags & DIRECTORY != 0,
        ..Record::default()
    };
    for attribute in attributes(record) {
        parsed.take(attribute);
    }
    Some(parsed)
}

impl Record {
    /// Takes what the index needs from one of the record's attributes.
    fn take(&mut self, attribute: Attribute) {
        match (attribute.kind, attribute.named, attribute.body) {
            (STANDARD_INFORMATION, _, Body::Resident(value)) => {
                self.written = i64_at(value, 0x08).map(seconds);
            }
            (FILE_NAME, _, Body::Resident(value)) => self.names.extend(name(value)),
            (DATA, false, body) => self.size = self.size.or(body.size()),
            (ATTRIBUTE_LIST, _, Body::Resident(value)) => self.elsewhere = listed(value),
            _ => {}
        }
    }
}

/// The name a `$FILE_NAME` attribute's value holds, unless it is a short
/// 8.3 one, or not valid Unicode, which could not be opened by name.
fn name(value: &[u8]) -> Option<Name> {
    let parent = number(u64_at(value, 0)?)?;
    let length = *value.get(0x40)? as usize;
    if *value.get(0x41)? == DOS {
        return None;
    }
    let units = value.get(0x42..0x42 + 2 * length)?;
    let units = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&unit| u16::from_le_bytes(unit));
    let text = char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()?;
    Some(Name { parent, text })
}

/// The records other than this one that an attribute list names, each
/// once, in the order listed.
fn listed(value: &[u8]) -> Vec<u32> {
    let mut records: Vec<u32> = Vec::new();
    let mut at = 0;
    while let Some(length) = u16_at(value, at + 4).filter(|&length| length >= 0x1A) {
        let kind = u32_at(value, at);
        let record = u64_at(value, at + 0x10).and_then(number);
        if let (Some(FILE_NAME | DATA), Some(record)) = (kind, record)
            && !records.contains(&record)
        {
            records.push(record);
        }
        at += length as usize;
    }
    records
}

/// One attribute of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attribute<'a> {
    kind: u32,
    /// Whether it has a name: a named `$DATA` is another stream.
    named: bool,
    body: Body<'a>,
}

/// Where an attribute's value is: in the record, or in clusters of the
/// volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body<'a> {
    Resident(&'a [u8]),
    NonResident {
        /// The first of the value's clusters this record maps.
        first: u64,
        /// The value's length, in bytes; told only where `first` is 0.
        length: u64,
        /// Where its clusters lie, as runs.
        runs: &'a [u8],
    },
}

impl Body<'_> {
    /// The length of the value, if this holds it.
    fn size(self) -> Option<u64> {
        match self {
            Body::Resident(value) => Some(value.len() as u64),
            Body::NonResident {
                first: 0, length, ..
            } => Some(length),
            Body::NonResident { .. } => None,
        }
    }
}

/// The record's attributes, up to the end marker or the first that does
/// not fit in the record.
fn attributes(record: &[u8]) -> impl Iterator<Item = Attribute<'_>> {
    let used = u32_at(record, 0x18).map_or(0, |used| (used as usize).min(record.len()));
    let record = &record[..used];
    let mut at = u16_at(record, 0x14).map(usize::from);
    std::iter::from_fn(move || {
        let start = at?;
        let kind = u32_at(record, start).filter(|&kind| kind != END)?;
        let length = u32_at(record, start + 4)? as usize;
        let attribute = record.get(start..start.checked_add(length)?)?;
        at = (length >= 0x18).then_some(start + length);
        Some(Attribute {
            kind,
            named: *attribute.get(9)? != 0,
            body: body(attribute)?,
        })
    })
}

/// Where the attribute's value is.
fn body(attribute: &[u8]) -> Option<Body<'_>> {
    match *attribute.get(8)? {
        0 => resident(attribute),
        _ => non_resident(attribute),
    }
}

/// The value kept in the attribute itself.
fn resident(attribute: &[u8]) -> Option<Body<'_>> {
    let length = u32_at(attribute, 0x10)? as usize;
    let offset = u16_at(attribute, 0x14)? as usize;
    let value = attribute.get(offset..offset.checked_add(length)?)?;
    Some(Body::Resident(value))
}

/// Where in the volume's clusters the value lies.
fn non_resident(attribute: &[u8]) -> Option<Body<'_>> {
    Some(Body::NonResident {
        first: u64_at(attribute, 0x10)?,
        length: u64_at(attribute, 0x30)?,
        runs: attribute.get(u16_at(attribute, 0x20)? as usize..)?,
    })
}

/// A stretch of clusters a value lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Where on the volume it starts; none for a sparse stretch, which
    /// reads as zeros.
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Where a value's clusters lie, from the runs that say so, if they read
/// whole. Each run is a byte telling the sizes of the two numbers after
/// it: the stretch's length, and where it starts counted from where the
/// one before started (none for a sparse stretch).
pub fn runs(bytes: &[u8]) -> Option<Vec<Extent>> {
    let mut extents = Vec::new();
    let (mut at, mut lcn) = (0, 0_i64);
    while *bytes.get(at)? != 0 {
        let (clusters, place, next) = run(bytes, at)?;
        let start = match place {
            None => None,
            Some(place) => {
                lcn = lcn.checked_add(place)?;
                Some(u64::try_from(lcn).ok()?)
            }
        };
        extents.push(Extent {
            lcn: start,
            clusters,
        });
        at = next;
    }
    Some(extents)
}

/// The run at `at`: its stretch's length, where it starts counted from
/// the one before (none if sparse), and where the next run is.
fn run(bytes: &[u8], at: usize) -> Option<(u64, Option<i64>, usize)> {
    let header = *bytes.get(at)?;
    let (length_size, place_size) = (usize::from(header & 0xF), usize::from(header >> 4));
    if !(1..=8).contains(&length_size) || place_size > 8 {
        return None;
    }
    let place_at = at + 1 + length_size;
    let clusters = unsigned(bytes.get(at + 1..place_at)?);
    let place = bytes.get(place_at..place_at + place_size)?;
    let place = (place_size > 0).then(|| signed(place));
    (clusters > 0).then_some((clusters, place, place_at + place_size))
}

/// A little-endian number of up to 8 bytes.
fn unsigned(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0, |number, &byte| number << 8 | u64::from(byte))
}

/// A little-endian number of 1 to 8 bytes whose top bit is its sign.
fn signed(bytes: &[u8]) -> i64 {
    let unused = 64 - 8 * bytes.len() as u32;
    ((unsigned(bytes) << unused) as i64) >> unused
}

/// The first cluster of its value and where it lies, for the record's
/// unnamed data stream kept outside it: for the table's own record, where
/// the table is.
pub fn data_runs(record: &[u8]) -> Option<(u64, Vec<Extent>)> {
    attributes(record).find_map(|attribute| match attribute {
        Attribute {
            kind: DATA,
            named: false,
            body: Body::NonResident {
                first, runs: found, ..
            },
        } => Some((first, runs(found)?)),
        _ => None,
    })
}

/// Where the master file table lies on its volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    cluster: u64,
    record: u64,
    stretches: Vec<Stretch>,
}

/// Clusters of the table that lie one after another on the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stretch {
    /// The first of the table's clusters it holds.
    first: u64,
    /// Where on the volume.
    lcn: u64,
    clusters: u64,
}

/// Bytes of the volume, from a byte on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub at: u64,
    pub length: u64,
}

/// Bytes of the table read at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub start: u64,
    pub length: u64,
}

impl Piece {
    /// The number of the first record it holds, records being `record`
    /// bytes.
    pub fn first(self, record: u64) -> u32 {
        (self.start / record) as u32
    }
}

impl Table {
    /// The table of records of `record` bytes on a volume of clusters of
    /// `cluster` bytes, laid out as `segments` say, each the first of the
    /// table's clusters it maps and where those lie; none unless they map
    /// it from its start without a gap or a sparse stretch.
    pub fn new(cluster: u64, record: u64, mut segments: Vec<(u64, Vec<Extent>)>) -> Option<Table> {
        let sound = |size: u64| size >= 512 && size.is_power_of_two();
        if !sound(cluster) || !sound(record) {
            return None;
        }
        segments.sort_by_key(|&(first, _)| first);
        let mut stretches = Vec::new();
        let mut next = 0_u64;
        for (first, extents) in segments {
            if first != next {
                return None;
            }
            for extent in extents {
                stretches.push(Stretch {
                    first: next,
                    lcn: extent.lcn?,
                    clusters: extent.clusters,
                });
                next = next.checked_add(extent.clusters)?;
            }
        }
        Some(Table {
            cluster,
            record,
            stretches,
        })
    }

    /// How many of the table's bytes it maps.
    pub fn length(&self) -> u64 {
        let clusters: u64 = self.stretches.iter().map(|stretch| stretch.clusters).sum();
        clusters * self.cluster
    }

    /// Where the table's bytes from `start` on, `length` of them, lie on
    /// the volume; none if it maps not all of them.
    pub fn spans(&self, start: u64, length: u64) -> Option<Vec<Span>> {
        let end = start.checked_add(length)?;
        if end > self.length() {
            return None;
        }
        let spans = self.stretches.iter().filter_map(|stretch| {
            let from = stretch.first * self.cluster;
            let (within, to) = (
                start.max(from),
                end.min(from + stretch.clusters * self.cluster),
            );
            (within < to).then(|| Span {
                at: stretch.lcn * self.cluster + (within - from),
                length: to - within,
            })
        });
        Some(spans.collect())
    }

    /// The pieces to read the table's first `valid` bytes in, each of
    /// whole records and clusters and at most `most` bytes unless one
    /// record or cluster is more.
    pub fn pieces(&self, valid: u64, most: u64) -> Vec<Piece> {
        // Both are powers of two: the bigger is a whole number of the other.
        let unit = self.cluster.max(self.record);
        let mapped = self.length() / unit * unit;
        let total = valid.div_ceil(unit).saturating_mul(unit).min(mapped);
        let step = (most / unit).max(1) * unit;
        let starts = std::iter::successors(Some(0), |start| Some(start + step));
        let starts = starts.take_while(|&start| start < total);
        starts
            .map(|start| Piece {
                start,
                length: step.min(total - start),
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod build;

#[cfg(test)]
mod tests;
