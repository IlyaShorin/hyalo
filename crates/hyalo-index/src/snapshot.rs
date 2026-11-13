//! The index on disk, so a restart does not walk the drives again.
//!
//! Little-endian: the magic `HYALOIDX`, a version, four lengths
//! (entries, bytes of names, free places, positions), then each part as
//! it lies in memory: entries, names, free places, then each entry's size
//! (8 bytes), last write (4) and record (4), then each volume's position:
//! its root (4), the volume (8), its journal (8) and the place in it (8).
//! The folded names are not kept: loading folds the names again, which
//! also makes sure the copy matches. A snapshot is checked where it comes
//! in; one that is damaged, cut short or of another version is refused,
//! never trusted. Version 2, without records and positions, is still read.

use std::io::{self, Read, Write};

use hyalo_core::volume::Position;

use crate::Index;
use crate::index::{Entry, Parts};
use crate::volume::NO_FILE;

const MAGIC: &[u8; 8] = b"HYALOIDX";

/// Changes with the format, and with the way names are folded. 2: sizes
/// and times; 3: records and positions.
const VERSION: u32 = 3;

/// The version before, still read: what it saved had no records.
const BEFORE: u32 = 2;

/// The bytes of a volume's position.
const POSITION: usize = 4 + 8 + 8 + 8;

/// Why a snapshot was refused.
#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    /// Not a snapshot, or one of another version.
    Foreign,
    /// Cut short, or its parts do not fit together.
    Damaged,
}

impl Index {
    /// Writes the whole index to `out`. Many small writes: buffer them.
    pub fn save(&self, out: &mut impl Write) -> io::Result<()> {
        let (entries, names, free) = self.parts();
        let positions = self.positions();
        let lengths = [entries.len(), names.len(), free.len(), positions.len()];
        out.write_all(&header(lengths))?;
        all(out, entries.iter().map(|entry| entry.to_bytes()))?;
        out.write_all(names.as_bytes())?;
        all(out, free.iter().map(|at| at.to_le_bytes()))?;
        all(out, self.sizes.iter().map(|size| size.to_le_bytes()))?;
        all(out, self.times.iter().map(|time| time.to_le_bytes()))?;
        all(out, self.files.iter().map(|record| record.to_le_bytes()))?;
        all(
            out,
            positions.iter().map(|&(root, at)| position_bytes(root, at)),
        )
    }

    /// Reads back what `save` wrote, all of it and nothing after.
    pub fn load(input: &mut impl Read) -> Result<Index, LoadError> {
        let version = ours(input)?;
        let lengths = lengths(input, version)?;
        let parts = parts(input, version, lengths)?;
        Index::from_parts(parts).ok_or(LoadError::Damaged)
    }
}

/// Writes each of `pieces`.
fn all<const N: usize>(
    out: &mut impl Write,
    mut pieces: impl Iterator<Item = [u8; N]>,
) -> io::Result<()> {
    pieces.try_for_each(|piece| out.write_all(&piece))
}

/// A volume's position, as a snapshot keeps it.
fn position_bytes(root: u32, at: Position) -> [u8; POSITION] {
    let mut bytes = [0; POSITION];
    bytes[..4].copy_from_slice(&root.to_le_bytes());
    bytes[4..12].copy_from_slice(&at.volume.to_le_bytes());
    bytes[12..20].copy_from_slice(&at.journal.to_le_bytes());
    bytes[20..].copy_from_slice(&at.next.to_le_bytes());
    bytes
}

/// The root and position a snapshot keeps in `bytes`.
fn position_of(bytes: [u8; POSITION]) -> (u32, Position) {
    let (root, rest) = bytes.split_at(4);
    let (volume, rest) = rest.split_at(8);
    let (journal, next) = rest.split_at(8);
    let eight = |bytes: &[u8]| -> [u8; 8] { bytes.try_into().expect("eight bytes") };
    let position = Position {
        volume: u64::from_le_bytes(eight(volume)),
        journal: u64::from_le_bytes(eight(journal)),
        next: i64::from_le_bytes(eight(next)),
    };
    let root: [u8; 4] = root.try_into().expect("four bytes");
    (u32::from_le_bytes(root), position)
}

/// The parts after the lengths, of a snapshot of `version`; nothing may
/// follow.
fn parts(
    input: &mut impl Read,
    version: u32,
    [entries, names, free, positions]: [usize; 4],
) -> Result<Parts, LoadError> {
    let entries = decoded(&exactly(input, entries * 16)?, Entry::from_bytes);
    let names = String::from_utf8(exactly(input, names)?).map_err(|_| LoadError::Damaged)?;
    let free = decoded(&exactly(input, free * 4)?, u32::from_le_bytes);
    let Beside {
        sizes,
        times,
        files,
    } = beside(input, version, entries.len())?;
    let positions = decoded(&exactly(input, positions * POSITION)?, position_of);
    ended(input)?;
    Ok(Parts {
        entries,
        names,
        free,
        sizes,
        times,
        files,
        positions,
    })
}

/// What a snapshot keeps beside each entry.
struct Beside {
    sizes: Vec<u64>,
    times: Vec<u32>,
    files: Vec<u32>,
}

/// What a snapshot of `version` keeps beside each of `count` entries: its
/// size, its last write and its record; none came by record before.
fn beside(input: &mut impl Read, version: u32, count: usize) -> Result<Beside, LoadError> {
    let sizes = decoded(&exactly(input, count * 8)?, u64::from_le_bytes);
    let times = decoded(&exactly(input, count * 4)?, u32::from_le_bytes);
    let files = match version {
        BEFORE => vec![NO_FILE; count],
        _ => decoded(&exactly(input, count * 4)?, u32::from_le_bytes),
    };
    Ok(Beside {
        sizes,
        times,
        files,
    })
}

/// `bytes` cut into pieces of `N`, each read by `read`.
fn decoded<const N: usize, T>(bytes: &[u8], read: fn([u8; N]) -> T) -> Vec<T> {
    bytes
        .as_chunks::<N>()
        .0
        .iter()
        .map(|&piece| read(piece))
        .collect()
}

/// How a snapshot starts: the magic, the version and the four lengths.
fn header(lengths: [usize; 4]) -> Vec<u8> {
    let lengths = lengths.map(|length| (length as u64).to_le_bytes());
    [MAGIC.as_slice(), &VERSION.to_le_bytes(), &lengths.concat()].concat()
}

/// Reads the magic and the version, and refuses anything but ours: the
/// version it is.
fn ours(input: &mut impl Read) -> Result<u32, LoadError> {
    let magic = exactly(input, MAGIC.len()).map_err(|_| LoadError::Foreign)?;
    let version: [u8; 4] = exactly(input, 4)?
        .try_into()
        .map_err(|_| LoadError::Foreign)?;
    let version = u32::from_le_bytes(version);
    match magic == MAGIC && [VERSION, BEFORE].contains(&version) {
        true => Ok(version),
        false => Err(LoadError::Foreign),
    }
}

/// Reads the lengths a snapshot of `version` has: the version before kept
/// no positions. Offsets are u32: no part is longer.
fn lengths(input: &mut impl Read, version: u32) -> Result<[usize; 4], LoadError> {
    let count = match version {
        BEFORE => 3,
        _ => 4,
    };
    let bytes = exactly(input, count * 8)?;
    let length = |&bytes: &[u8; 8]| u32::try_from(u64::from_le_bytes(bytes));
    let mut lengths = [0; 4];
    for (at, bytes) in bytes.as_chunks::<8>().0.iter().enumerate() {
        lengths[at] = length(bytes).map_err(|_| LoadError::Damaged)? as usize;
    }
    Ok(lengths)
}

/// Refuses anything after the end.
fn ended(input: &mut impl Read) -> Result<(), LoadError> {
    // Its first value never matters: a read either fills it or not.
    let mut probe = [u8::MIN];
    match input.read(&mut probe).map_err(LoadError::Io)? {
        0 => Ok(()),
        _ => Err(LoadError::Damaged),
    }
}

/// The next `length` bytes, in memory just big enough for them, unless the
/// input ends first.
fn exactly(input: &mut impl Read, length: usize) -> Result<Vec<u8>, LoadError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| LoadError::Damaged)?;
    let read = input.take(length as u64).read_to_end(&mut bytes);
    match read.map_err(LoadError::Io)? == length {
        true => Ok(bytes),
        false => Err(LoadError::Damaged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryId, synthetic};
    use hyalo_core::places::Places;

    use hyalo_core::volume::{Change, File, Name, Position, ROOT};

    /// The magic, the version and four lengths, in bytes.
    const HEADER: usize = 8 + 4 + 4 * 8;

    fn saved(index: &Index) -> Vec<u8> {
        let mut bytes = Vec::new();
        index.save(&mut bytes).expect("save to memory");
        bytes
    }

    fn loaded(bytes: &[u8]) -> Result<Index, LoadError> {
        Index::load(&mut &bytes[..])
    }

    /// C:\a\b.txt, with C:\gone removed so its place is free.
    fn small() -> Index {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let a = index.add(c, "a", true).unwrap();
        let gone = index.add(c, "gone", false).unwrap();
        index.add(a, "Ёлка.txt", false).unwrap();
        index.remove(gone).unwrap();
        index
    }

    fn paths(index: &Index) -> Vec<Option<String>> {
        (0..index.entries.len() as u32)
            .map(|at| index.path(EntryId(at)))
            .collect()
    }

    #[test]
    fn a_saved_index_loads_back_the_same() {
        let index = synthetic::drive(5_000, 2);
        let back = loaded(&saved(&index)).expect("load");
        assert_eq!(paths(&back), paths(&index));
        assert_eq!(back.len(), index.len());
        let places = Places::default();
        let ids = |index: &Index| -> Vec<String> {
            let found = index.search("rep", &places, 10);
            found.rows.into_iter().map(|row| row.item.id.0).collect()
        };
        assert_eq!(ids(&back), ids(&index));
    }

    #[test]
    fn a_loaded_index_still_gives_removed_places_to_new_entries() {
        let mut back = loaded(&saved(&small())).expect("load");
        let c = EntryId(0);
        assert_eq!(back.add(c, "new", false), Ok(EntryId(2)));
        assert_eq!(back.path(EntryId(2)).as_deref(), Some(r"C:\new"));
    }

    #[test]
    fn a_loaded_index_finds_entries_by_path_and_changes_them() {
        let mut back = loaded(&saved(&small())).expect("load");
        let a = back.find(r"C:\A").expect("a folder");
        assert_eq!(back.put(a, "b.txt", false).map(|_| back.len()), Ok(4));
        let tree = back.find(r"C:\a\ёлка.txt").expect("found in any case");
        back.remove(a).unwrap();
        assert_eq!(back.name(tree), None, "went with its folder");
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn sizes_and_times_load_back_the_same() {
        let mut index = small();
        let a = index.find(r"C:\a").expect("a");
        index.set_stat(a, 4096, 1_700_000_000).unwrap();
        let tree = index.find(r"C:\a\Ёлка.txt").expect("tree");
        index.set_stat(tree, u64::MAX, 7).unwrap();
        let back = loaded(&saved(&index)).expect("load");
        assert_eq!(back.stat(a), Some((4096, 1_700_000_000)));
        assert_eq!(back.stat(tree), Some((u64::MAX, 7)));
        assert_eq!(back.stat(EntryId(0)), Some((0, 0)));
    }

    #[test]
    fn a_snapshot_of_the_first_version_is_refused_and_walked_again() {
        let mut bytes = saved(&small());
        bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
        assert!(matches!(loaded(&bytes), Err(LoadError::Foreign)));
    }

    #[test]
    fn loading_takes_no_more_memory_than_it_needs() {
        let back = loaded(&saved(&small())).expect("load");
        // Four entries, their links, sizes, times and records; C:, a and
        // Ёлка.txt as given and folded (the name of the one removed is not
        // loaded); one free place.
        assert_eq!(back.memory(), 4 * (16 + 12 + 12 + 4) + 15 + 15 + 4);
    }

    #[test]
    fn something_else_is_refused() {
        assert!(matches!(
            loaded(b"not an index at all"),
            Err(LoadError::Foreign)
        ));
        let mut bytes = saved(&small());
        bytes[8] = 99;
        assert!(
            matches!(loaded(&bytes), Err(LoadError::Foreign)),
            "another version"
        );
    }

    #[test]
    fn a_snapshot_cut_short_anywhere_is_refused() {
        let bytes = saved(&small());
        for end in 0..bytes.len() {
            assert!(loaded(&bytes[..end]).is_err(), "cut at {end}");
        }
        let longer = [bytes.as_slice(), &[0]].concat();
        assert!(damaged(&longer), "more after the end");
    }

    #[test]
    fn no_damaged_byte_makes_loading_fail_badly() {
        let bytes = saved(&small());
        for at in 0..bytes.len() {
            for value in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
                let mut damaged = bytes.clone();
                damaged[at] = value;
                if let Ok(index) = loaded(&damaged) {
                    // Whatever loads must be whole: every path builds.
                    paths(&index);
                    index.search("a", &Places::default(), 10);
                }
            }
        }
    }

    /// Byte offset of field `field` (0 parent, 1 name, 2 folded, 3
    /// lengths) of entry `entry`.
    fn field(entry: usize, field: usize) -> usize {
        HEADER + entry * 16 + field * 4
    }

    fn with(bytes: &[u8], at: usize, value: u32) -> Vec<u8> {
        let mut damaged = bytes.to_vec();
        damaged[at..at + 4].copy_from_slice(&value.to_le_bytes());
        damaged
    }

    fn damaged(bytes: &[u8]) -> bool {
        matches!(loaded(bytes), Err(LoadError::Damaged))
    }

    #[test]
    fn entries_that_do_not_fit_together_are_refused() {
        let bytes = saved(&small());
        assert!(
            damaged(&with(&bytes, field(1, 0), 9)),
            "parent past the end"
        );
        assert!(damaged(&with(&bytes, field(1, 0), 1)), "its own parent");
        assert!(
            damaged(&with(&bytes, field(1, 1), 999)),
            "name past the end"
        );
        assert!(
            damaged(&with(&bytes, field(3, 3), 1 | 12 << 12)),
            "inside a letter"
        );
        assert!(
            damaged(&with(&bytes, field(1, 3), 1 << 24)),
            "an empty name"
        );
        assert!(
            damaged(&with(&bytes, field(1, 3), 1 | 1 << 12 | (1 | 64) << 24)),
            "a folder with an unknown flag"
        );
    }

    #[test]
    fn only_a_live_folder_holds_entries() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        index.add(c, "f.txt", false).unwrap();
        let d = index.add(c, "d", true).unwrap();
        index.add(c, "x.txt", false).unwrap();
        index.remove(d).unwrap();
        let bytes = saved(&index);
        assert!(loaded(&bytes).is_ok());
        assert!(damaged(&with(&bytes, field(3, 0), 1)), "in a file");
        assert!(
            damaged(&with(&bytes, field(3, 0), 2)),
            "in a removed folder"
        );
    }

    #[test]
    fn a_root_inside_itself_is_refused() {
        let mut index = Index::default();
        index.add_root("C:").unwrap();
        assert!(damaged(&with(&saved(&index), field(0, 0), 0)));
    }

    #[test]
    fn a_loop_of_folders_is_refused() {
        let mut index = small();
        let c = EntryId(0);
        let b = index.add(c, "b", true).unwrap();
        let bytes = saved(&index);
        // a (1) into b (2), b into a: neither reaches a root.
        let bytes = with(&with(&bytes, field(1, 0), b.0), field(2, 0), 1);
        assert!(damaged(&bytes));
    }

    #[test]
    fn a_free_place_must_be_a_removed_entry_listed_once() {
        // Each entry's size, time and record come after the free places.
        let after = |index: &Index| index.entries.len() * (8 + 4 + 4);
        let bytes = saved(&small());
        let free = bytes.len() - after(&small()) - 4;
        assert!(damaged(&with(&bytes, free, 1)), "a live entry");
        assert!(damaged(&with(&bytes, free, 9)), "past the end");
        let mut index = small();
        let c = EntryId(0);
        let x = index.add(c, "x", false).unwrap();
        let y = index.add(c, "y", false).unwrap();
        index.remove(x).unwrap();
        index.remove(y).unwrap();
        let mut index_bytes = saved(&index);
        let len = index_bytes.len() - after(&index);
        let first = index_bytes[len - 8..len - 4].to_vec();
        index_bytes[len - 4..len].copy_from_slice(&first);
        assert!(damaged(&index_bytes), "listed twice");
    }

    #[test]
    fn bytes_that_are_not_text_are_refused() {
        let bytes = saved(&small());
        let names = HEADER + 4 * 16;
        let mut damaged_bytes = bytes.clone();
        damaged_bytes[names] = 0xFF;
        assert!(damaged(&damaged_bytes));
    }

    #[test]
    fn a_name_add_would_refuse_is_refused() {
        let bytes = saved(&small());
        // "a", the second name, after "C:".
        let a = HEADER + 4 * 16 + 2;
        let mut slashed = bytes.clone();
        slashed[a] = b'\\';
        assert!(damaged(&slashed));
        let mut rooted = bytes.clone();
        rooted[a - 1] = b'\\';
        assert!(loaded(&rooted).is_ok(), "a root may hold one: C\\");
    }

    fn named(record: u32, folder: bool, names: &[(u32, &str)]) -> Change {
        let names = names.iter().map(|&(parent, text)| Name {
            parent,
            text: text.to_string(),
        });
        Change {
            record,
            file: Some(File {
                folder,
                size: 1,
                written: 2,
                names: names.collect(),
            }),
        }
    }

    const POSITION: Position = Position {
        volume: 0x0123_4567_89AB_CDEF,
        journal: 7,
        next: -9,
    };

    /// `small`, with files of C:'s table beside what was walked: a
    /// folder, a file in it, and a file with a hard link in each.
    fn listed() -> Index {
        let mut index = small();
        let c = EntryId(0);
        index
            .settle(c, &named(40, true, &[(ROOT, "listed")]))
            .unwrap();
        index
            .settle(c, &named(50, false, &[(40, "one.txt")]))
            .unwrap();
        let links = named(51, false, &[(40, "x.txt"), (ROOT, "y.txt")]);
        index.settle(c, &links).unwrap();
        index.set_position(c, POSITION).unwrap();
        index
    }

    fn gone(record: u32) -> Change {
        Change { record, file: None }
    }

    #[test]
    fn a_volume_s_records_and_position_load_back() {
        let mut back = loaded(&saved(&listed())).expect("load");
        let c = EntryId(0);
        assert_eq!(back.position(c), Some(POSITION));
        back.settle(c, &gone(40)).unwrap();
        back.settle(c, &gone(51)).unwrap();
        assert_eq!(back.find(r"C:\listed"), None);
        assert_eq!(back.find(r"C:\y.txt"), None, "every link known");
        assert!(back.find(r"C:\a\Ёлка.txt").is_some(), "the walked stay");
    }

    /// What version 2 saved of `index`: no records, no positions.
    fn version_2(index: &Index) -> Vec<u8> {
        let (entries, names, free) = index.parts();
        let lengths = [entries.len(), names.len(), free.len()];
        let lengths = lengths.map(|length| (length as u64).to_le_bytes());
        let mut bytes = [MAGIC.as_slice(), &2_u32.to_le_bytes(), &lengths.concat()].concat();
        bytes.extend(entries.iter().flat_map(|entry| entry.to_bytes()));
        bytes.extend(names.as_bytes());
        bytes.extend(free.iter().flat_map(|at| at.to_le_bytes()));
        bytes.extend(index.sizes.iter().flat_map(|size| size.to_le_bytes()));
        bytes.extend(index.times.iter().flat_map(|time| time.to_le_bytes()));
        bytes
    }

    #[test]
    fn a_snapshot_of_the_version_before_loads_with_no_records() {
        let index = listed();
        let mut back = loaded(&version_2(&index)).expect("load");
        assert_eq!(paths(&back), paths(&index));
        let c = EntryId(0);
        assert_eq!(back.position(c), None);
        back.settle(c, &gone(40)).unwrap();
        assert!(back.find(r"C:\listed").is_some(), "not known by its record");
        let cut = version_2(&index);
        assert!(damaged(&cut[..cut.len() - 1]));
    }

    #[test]
    fn a_position_for_what_is_not_a_root_is_refused() {
        let bytes = saved(&listed());
        // The position comes last: its root, then the rest.
        let root = bytes.len() - 28;
        assert!(loaded(&with(&bytes, root, 0)).is_ok());
        assert!(damaged(&with(&bytes, root, 1)), "a folder");
        assert!(damaged(&with(&bytes, root, 99)), "past the end");
    }

    #[test]
    fn a_record_for_every_entry_is_kept() {
        let bytes = saved(&listed());
        let longer = [bytes.as_slice(), &[0; 4]].concat();
        assert!(damaged(&longer));
    }
}
