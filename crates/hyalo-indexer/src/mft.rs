//! The master file table: a record for every file and folder of an NTFS
//! volume, each a list of attributes. Pure: it reads bytes the volume
//! gave, and checks them where they come in, since names are anyone's to
//! choose and this runs with the rights of the system.

use hyalo_core::volume::{File, Name};

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

/// The shortest entry of an attribute list: what it says, rounded up to
/// eight bytes, as every entry is.
const LIST_ENTRY: u16 = 0x20;

/// The shortest attribute: the header of one kept in the record.
const ATTRIBUTE: usize = 0x18;

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
    /// What the index keeps of the file: none for a size or time the
    /// record did not say.
    pub fn file(self) -> File {
        File {
            folder: self.folder,
            size: self.size.unwrap_or(0),
            written: self.written.unwrap_or(0),
            names: self.names,
        }
    }

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
    if sectors * STRIDE != record.len() {
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
    let mut parsed = Record {
        sequence: u16_at(record, 0x10)?,
        base: base(record)?,
        folder: flags & DIRECTORY != 0,
        ..Record::default()
    };
    for attribute in attributes(record) {
        parsed.take(attribute);
    }
    Some(parsed)
}

/// The file's own record, for an extension record; none if it names one
/// beyond the index's numbers. A reference to record 0 still has a
/// sequence number: only nothing at all says there is no base.
fn base(record: &[u8]) -> Option<Option<u32>> {
    match u64_at(record, 0x20)? {
        0 => Some(None),
        reference => Some(Some(number(reference)?)),
    }
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
    while let Some(length) = u16_at(value, at + 4).filter(|&length| length >= LIST_ENTRY) {
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
    // A record that says it uses more than it holds holds nothing.
    let used = u32_at(record, 0x18).and_then(|used| record.get(..used as usize));
    let record = used.unwrap_or_default();
    let mut at = u16_at(record, 0x14).map(usize::from);
    std::iter::from_fn(move || {
        let start = at?;
        let kind = u32_at(record, start).filter(|&kind| kind != END)?;
        let length = u32_at(record, start + 4)? as usize;
        let attribute = record.get(start..start.checked_add(length)?)?;
        at = (length >= ATTRIBUTE).then_some(start + length);
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
    // A length of no bytes is no clusters, refused below.
    if length_size > 8 || place_size > 8 {
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
mod tests {
    use super::build::{name, record};
    use super::*;
    use hyalo_core::volume::File;

    /// The size of a record on most volumes.
    const SIZE: usize = 1024;

    /// 13.11.2026 10:30 UTC, as Windows counts.
    const WRITTEN: i64 = 134_390_394_000_000_000;
    const WRITTEN_SECONDS: i64 = 1_794_565_800;

    const WIN32: u8 = 1;
    const POSIX: u8 = 0;
    const WIN32_AND_DOS: u8 = 3;

    #[test]
    fn windows_time_reads_in_seconds_since_1970() {
        assert_eq!(seconds(116_444_736_000_000_000), 0);
        assert_eq!(seconds(WRITTEN + 9_999_999), WRITTEN_SECONDS);
        assert_eq!(seconds(-1), -11_644_473_601, "rounded down");
    }

    #[test]
    fn a_file_s_record_gives_its_name_folder_size_and_last_write() {
        let made = record()
            .sequence(4)
            .written(WRITTEN)
            .name(77, "report.pdf", WIN32_AND_DOS)
            .data(0, 123_456, &[0x11, 0x20, 0x30, 0x00]);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(
            parsed,
            Record {
                sequence: 4,
                base: None,
                folder: false,
                names: vec![name(77, "report.pdf")],
                written: Some(WRITTEN_SECONDS),
                size: Some(123_456),
                elsewhere: vec![],
            }
        );
    }

    #[test]
    fn a_folder_s_record_says_it_is_one_and_holds_no_data() {
        let made = record()
            .folder()
            .written(WRITTEN)
            .name(5, "Documents", WIN32);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert!(parsed.folder);
        assert_eq!(parsed.names, [name(5, "Documents")]);
        assert_eq!(parsed.size, None);
    }

    #[test]
    fn a_small_file_s_data_lies_in_its_record() {
        let made = record().name(5, "a.txt", WIN32).small_data(37, "");
        assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, Some(37));
    }

    #[test]
    fn another_stream_of_the_file_is_not_its_size() {
        let made = record()
            .name(5, "a.txt", WIN32)
            .small_data(9, "Zone.Identifier")
            .small_data(3, "");
        assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, Some(3));
        let only_another = record()
            .name(5, "b.txt", WIN32)
            .small_data(9, "Zone.Identifier");
        let parsed = parse(&only_another.bytes(SIZE)).expect("in use");
        assert_eq!(parsed.size, None);
    }

    #[test]
    fn a_short_8_3_name_is_left_out_and_every_hard_link_kept() {
        let made = record()
            .name(40, "LONGFI~1.TXT", DOS)
            .name(40, "long file name.txt", WIN32)
            .name(41, "Case Sensitive", POSIX)
            .name(42, "LINK.TXT", WIN32_AND_DOS);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(
            parsed.names,
            [
                name(40, "long file name.txt"),
                name(41, "Case Sensitive"),
                name(42, "LINK.TXT")
            ]
        );
    }

    #[test]
    fn a_name_that_is_not_valid_unicode_is_left_out() {
        let made = record()
            .name_units(5, &[0x61, 0xD800, 0x62], WIN32)
            .name(5, "ёлка", WIN32);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(parsed.names, [name(5, "ёлка")]);
    }

    #[test]
    fn a_record_no_longer_in_use_holds_no_file() {
        let made = record().free().name(5, "gone.txt", WIN32);
        assert_eq!(parse(&made.bytes(SIZE)), None);
    }

    #[test]
    fn bytes_that_are_no_record_hold_no_file() {
        let mut bytes = record().name(5, "a.txt", WIN32).bytes(SIZE);
        bytes[0..4].copy_from_slice(b"BAAD");
        assert_eq!(parse(&bytes), None);
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(b"FILE"), None);
    }

    #[test]
    fn an_extension_record_names_its_base_and_holds_what_it_could_not() {
        let made = record()
            .extending(300)
            .name(9, "fourth link.txt", WIN32)
            .data(0, 5_000, &[0x11, 0x02, 0x40, 0x00]);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(parsed.base, Some(300));
        assert_eq!(parsed.written, None);
        let mut base = parse(&record().written(WRITTEN).bytes(SIZE)).expect("in use");
        base.merge(parsed);
        assert_eq!(base.names, [name(9, "fourth link.txt")]);
        assert_eq!(base.size, Some(5_000));
        assert_eq!(base.written, Some(WRITTEN_SECONDS));
    }

    #[test]
    fn data_mapped_from_a_later_cluster_on_does_not_say_the_size() {
        let made = record()
            .extending(300)
            .data(16, 999, &[0x11, 0x02, 0x40, 0x00]);
        assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, None);
        let mut base = parse(&record().data(0, 5_000, &[]).bytes(SIZE)).expect("in use");
        base.merge(parse(&made.bytes(SIZE)).expect("in use"));
        assert_eq!(base.size, Some(5_000), "the start's size stays");
    }

    #[test]
    fn an_attribute_list_names_the_records_that_hold_names_or_data() {
        let made = record().written(WRITTEN).list(&[
            (0x10, 0, 300),
            (0x30, 0, 300),
            (0x30, 0, 301),
            (0x80, 0, 302),
            (0x80, 16, 301),
            (0x90, 0, 303),
        ]);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(parsed.elsewhere, [300, 301, 302]);
    }

    #[test]
    fn a_record_read_from_the_disk_gets_its_sectors_ends_back() {
        // A name long enough to cross the first sector's end.
        let long = "x".repeat(200);
        let made = record().name(5, &long, WIN32);
        let mut on_disk = made.on_disk(SIZE);
        assert_ne!(on_disk, made.bytes(SIZE));
        assert!(fix(&mut on_disk));
        assert_eq!(on_disk, made.bytes(SIZE));
        assert_eq!(parse(&on_disk).expect("in use").names, [name(5, &long)]);
    }

    #[test]
    fn a_record_whose_sector_was_not_written_whole_is_refused() {
        let made = record().name(5, "torn.txt", WIN32);
        let mut torn = made.on_disk(SIZE);
        torn[SIZE - 2] ^= 0xFF;
        assert!(!fix(&mut torn));
        let mut first_torn = made.on_disk(SIZE);
        first_torn[510] ^= 0xFF;
        assert!(!fix(&mut first_torn));
    }

    #[test]
    fn a_record_of_four_kilobytes_has_eight_sectors_to_put_back() {
        let made = record().name(5, &"y".repeat(250), WIN32);
        let mut on_disk = made.on_disk(4096);
        assert!(fix(&mut on_disk));
        assert_eq!(on_disk, made.bytes(4096));
    }

    #[test]
    fn an_update_sequence_that_does_not_fit_the_record_is_refused() {
        let made = record().name(5, "a.txt", WIN32);
        assert!(!fix(&mut made.on_disk(SIZE)[..512].to_vec()), "too short");
        let mut wrong_count = made.on_disk(SIZE);
        wrong_count[6] = 9;
        assert!(!fix(&mut wrong_count));
        let mut no_count = made.on_disk(SIZE);
        no_count[6..8].copy_from_slice(&[0, 0]);
        assert!(!fix(&mut no_count));
        let mut out_of_it = made.on_disk(SIZE);
        out_of_it[4..6].copy_from_slice(&1020_u16.to_le_bytes());
        assert!(!fix(&mut out_of_it));
        assert!(!fix(&mut []));
    }

    #[test]
    fn an_attribute_running_past_the_record_ends_the_list() {
        let made = record()
            .name(5, "kept.txt", WIN32)
            .name(5, "cut.txt", WIN32);
        let mut bytes = made.bytes(SIZE);
        // The second attribute claims to run on past the end.
        let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        let length = u32::from_le_bytes(bytes[first + 4..first + 8].try_into().unwrap()) as usize;
        let second = first + length;
        bytes[second + 4..second + 8].copy_from_slice(&5000_u32.to_le_bytes());
        let parsed = parse(&bytes).expect("in use");
        assert_eq!(parsed.names, [name(5, "kept.txt")]);
    }

    #[test]
    fn an_attribute_too_short_to_be_one_ends_the_list() {
        let made = record()
            .name(5, "kept.txt", WIN32)
            .name(5, "never.txt", WIN32);
        let mut bytes = made.bytes(SIZE);
        let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        bytes[first + 4..first + 8].copy_from_slice(&0_u32.to_le_bytes());
        // A length of nothing would read the same attribute for ever.
        assert_eq!(parse(&bytes).expect("in use").names, []);
    }

    #[test]
    fn a_name_longer_than_its_attribute_is_left_out() {
        let made = record().name(5, "short", WIN32);
        let mut bytes = made.bytes(SIZE);
        let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        let value = first + 0x18;
        bytes[value + 0x40] = 200;
        assert_eq!(parse(&bytes).expect("in use").names, []);
    }

    #[test]
    fn a_reference_beyond_the_index_s_numbers_is_no_parent() {
        let made = record().name(5, "a.txt", WIN32);
        let mut bytes = made.bytes(SIZE);
        let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        let value = first + 0x18;
        let huge = (1_u64 << 40) | 5;
        bytes[value..value + 8].copy_from_slice(&huge.to_le_bytes());
        assert_eq!(parse(&bytes).expect("in use").names, []);
    }

    fn at(lcn: u64, clusters: u64) -> Extent {
        Extent {
            lcn: Some(lcn),
            clusters,
        }
    }

    #[test]
    fn runs_say_where_each_piece_of_a_value_lies() {
        assert_eq!(runs(&[0x11, 0x20, 0x30, 0x00]), Some(vec![at(0x30, 0x20)]));
        // The second starts 16 clusters before the first: offsets are signed
        // and each counts from the one before.
        let two = [0x21, 0x10, 0x00, 0x01, 0x11, 0x08, 0xF0, 0x00];
        assert_eq!(runs(&two), Some(vec![at(256, 16), at(240, 8)]));
        let long = [0x33, 0x00, 0x00, 0x01, 0x00, 0x00, 0x10, 0x00];
        assert_eq!(runs(&long), Some(vec![at(0x10_0000, 0x1_0000)]));
    }

    #[test]
    fn a_run_with_no_place_is_sparse() {
        let sparse = [0x11, 0x04, 0x10, 0x01, 0x06, 0x11, 0x02, 0x08, 0x00];
        let expected = vec![
            at(16, 4),
            Extent {
                lcn: None,
                clusters: 6,
            },
            at(24, 2),
        ];
        assert_eq!(runs(&sparse), Some(expected));
    }

    #[test]
    fn runs_that_do_not_read_whole_are_refused() {
        assert_eq!(runs(&[]), None, "no end");
        assert_eq!(runs(&[0x11, 0x20]), None, "cut short");
        assert_eq!(runs(&[0x10, 0x20, 0x00]), None, "no length");
        assert_eq!(runs(&[0x11, 0x00, 0x30, 0x00]), None, "a length of nothing");
        assert_eq!(runs(&[0x11, 0x01, 0xFF, 0x00]), None, "before the volume");
        assert_eq!(
            runs(&[0x91, 0x01, 0x00]),
            None,
            "a place longer than 8 bytes"
        );
        assert_eq!(runs(&[0x00]), Some(vec![]), "nothing");
    }

    #[test]
    fn the_table_s_own_record_says_where_the_table_lies() {
        let made =
            record()
                .name(5, "$MFT", WIN32_AND_DOS)
                .data(0, 64 * 4096, &[0x11, 0x40, 0x10, 0x00]);
        assert_eq!(
            data_runs(&made.bytes(SIZE)),
            Some((0, vec![at(0x10, 0x40)]))
        );
        let more = record().extending(0).data(64, 0, &[0x11, 0x08, 0x50, 0x00]);
        assert_eq!(data_runs(&more.bytes(SIZE)), Some((64, vec![at(0x50, 8)])));
        assert_eq!(data_runs(&record().small_data(4, "").bytes(SIZE)), None);
    }

    /// A table of 1 KB records in 4 KB clusters: 16 clusters at 100, then 8 at
    /// 300.
    fn table() -> Table {
        Table::new(
            4096,
            1024,
            vec![(16, vec![at(300, 8)]), (0, vec![at(100, 16)])],
        )
        .expect("whole")
    }

    #[test]
    fn the_table_maps_its_bytes_to_the_volume_s_across_its_pieces() {
        let table = table();
        assert_eq!(table.length(), 24 * 4096);
        let span = |at, length| Span { at, length };
        assert_eq!(table.spans(0, 4096), Some(vec![span(100 * 4096, 4096)]));
        // Record 70 is in the second piece: 70 KB is 17.5 clusters in.
        assert_eq!(
            table.spans(70 * 1024, 1024),
            Some(vec![span(301 * 4096 + 2048, 1024)])
        );
        assert_eq!(
            table.spans(15 * 4096, 2 * 4096),
            Some(vec![span(115 * 4096, 4096), span(300 * 4096, 4096)]),
            "across the two"
        );
        assert_eq!(table.spans(23 * 4096, 2 * 4096), None, "beyond its end");
    }

    #[test]
    fn a_table_with_a_gap_or_a_sparse_piece_is_refused() {
        assert!(Table::new(4096, 1024, vec![(16, vec![at(300, 8)])]).is_none());
        assert!(Table::new(4096, 1024, vec![(0, vec![at(1, 4)]), (5, vec![at(9, 4)])]).is_none());
        let sparse = Extent {
            lcn: None,
            clusters: 4,
        };
        assert!(Table::new(4096, 1024, vec![(0, vec![at(1, 4), sparse])]).is_none());
        assert!(
            Table::new(4096, 1000, vec![(0, vec![at(1, 4)])]).is_none(),
            "odd records"
        );
        assert!(Table::new(0, 1024, vec![(0, vec![at(1, 4)])]).is_none());
    }

    #[test]
    fn the_table_is_read_in_pieces_of_whole_records_and_clusters() {
        let table = table();
        // 70 records in use: 17.5 clusters, read as 18.
        let pieces = table.pieces(70 * 1024, 8 * 4096);
        let piece = |start, length| Piece { start, length };
        assert_eq!(
            pieces,
            [
                piece(0, 8 * 4096),
                piece(8 * 4096, 8 * 4096),
                piece(16 * 4096, 2 * 4096)
            ]
        );
        assert_eq!(pieces[1].first(1024), 32);
        assert_eq!(
            table.pieces(1 << 40, 1 << 30),
            [piece(0, 24 * 4096)],
            "no further than it maps"
        );
        assert_eq!(table.pieces(0, 4096), []);
    }

    #[test]
    fn records_bigger_than_a_cluster_are_read_whole() {
        let table = Table::new(512, 1024, vec![(0, vec![at(10, 3), at(20, 5)])]).expect("whole");
        let pieces = table.pieces(4 * 1024, 1500);
        let piece = |start, length| Piece { start, length };
        assert_eq!(
            pieces,
            [
                piece(0, 1024),
                piece(1024, 1024),
                piece(2048, 1024),
                piece(3072, 1024)
            ]
        );
        let span = |at, length| Span { at, length };
        assert_eq!(
            table.spans(1024, 1024),
            Some(vec![span(12 * 512, 512), span(20 * 512, 512)])
        );
    }

    #[test]
    fn a_record_gives_the_index_its_file_saying_none_for_what_it_did_not() {
        let made = record()
            .written(WRITTEN)
            .name(5, "a.txt", WIN32)
            .small_data(3, "");
        let file = parse(&made.bytes(SIZE)).expect("in use").file();
        assert_eq!(
            file,
            File {
                folder: false,
                size: 3,
                written: WRITTEN_SECONDS,
                names: vec![name(5, "a.txt")],
            }
        );
        let bare = parse(&record().folder().bytes(SIZE))
            .expect("in use")
            .file();
        assert_eq!((bare.size, bare.written, bare.folder), (0, 0, true));
    }

    #[test]
    fn runs_made_for_a_test_read_back() {
        let bytes = super::build::runs_bytes(&[(100, 8), (40, 3)]);
        assert_eq!(runs(&bytes), Some(vec![at(100, 8), at(40, 3)]));
    }

    #[test]
    fn an_extension_of_the_table_s_own_record_names_it_as_its_base() {
        let made = record().extending(0).data(8, 0, &[0x11, 0x08, 0x50, 0x00]);
        assert_eq!(parse(&made.bytes(SIZE)).expect("in use").base, Some(0));
    }

    #[test]
    fn a_record_of_one_sector_gets_its_end_back() {
        let made = record().name(5, "a", WIN32);
        let mut small = made.on_disk(512);
        assert!(fix(&mut small));
        assert_eq!(small, made.bytes(512));
    }

    #[test]
    fn an_attribute_list_entry_shorter_than_one_can_be_ends_the_list() {
        let made = record().list(&[(0x30, 0, 300), (0x30, 0, 301)]);
        let mut bytes = made.bytes(SIZE);
        let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
        let entry = first + 0x18;
        bytes[entry + 4..entry + 6].copy_from_slice(&0x18_u16.to_le_bytes());
        assert_eq!(parse(&bytes).expect("in use").elsewhere, Vec::<u32>::new());
    }

    #[test]
    fn a_record_that_says_it_uses_more_than_it_holds_holds_no_attributes() {
        let made = record().name(5, "a.txt", WIN32);
        let mut bytes = made.bytes(SIZE);
        bytes[0x18..0x1C].copy_from_slice(&2000_u32.to_le_bytes());
        assert_eq!(parse(&bytes).expect("in use").names, []);
    }

    #[test]
    fn the_shortest_attribute_is_read_and_the_next_one_after_it() {
        let made = record().small_data(0, "").name(5, "after", WIN32);
        let parsed = parse(&made.bytes(SIZE)).expect("in use");
        assert_eq!(parsed.size, Some(0));
        assert_eq!(parsed.names, [name(5, "after")]);
    }

    #[test]
    fn a_run_of_one_cluster_or_of_eight_byte_numbers_reads() {
        assert_eq!(runs(&[0x11, 0x01, 0x05, 0x00]), Some(vec![at(5, 1)]));
        let eight = [0x18, 2, 0, 0, 0, 0, 0, 0, 0, 0x07, 0x00];
        assert_eq!(runs(&eight), Some(vec![at(7, 2)]));
        let nine = [0x09, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x00];
        assert_eq!(runs(&nine), None, "a length longer than 8 bytes");
    }

    #[test]
    fn a_named_stream_kept_outside_the_record_is_not_the_table() {
        let made = record().named_data(0, 4096, &[0x11, 0x01, 0x05, 0x00], "other");
        assert_eq!(data_runs(&made.bytes(SIZE)), None);
    }

    #[test]
    fn a_span_ends_where_the_table_s_bytes_asked_for_end() {
        let table = table();
        let span = |at, length| Span { at, length };
        assert_eq!(
            table.spans(0, 16 * 4096),
            Some(vec![span(100 * 4096, 16 * 4096)]),
            "no empty span from the stretch after"
        );
    }

    #[test]
    fn pieces_smaller_than_a_cluster_are_read_a_cluster_at_a_time() {
        let piece = |start, length| Piece { start, length };
        assert_eq!(
            table().pieces(8192, 100),
            [piece(0, 4096), piece(4096, 4096)]
        );
    }
}
