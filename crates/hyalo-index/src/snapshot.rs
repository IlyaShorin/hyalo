//! The index on disk, so a restart does not walk the drives again.
//!
//! Little-endian: the magic `HYALOIDX`, a version, three lengths
//! (entries, bytes of names, free places), then each part as it lies in
//! memory. The folded names are not kept: loading folds the names again,
//! which also makes sure the copy matches. A snapshot is checked where it
//! comes in; one that is damaged, cut short or of another version is
//! refused, never trusted.

use std::io::{self, Read, Write};

use crate::Index;
use crate::index::{Entry, Parts};

const MAGIC: &[u8; 8] = b"HYALOIDX";

/// Changes with the format, and with the way names are folded.
const VERSION: u32 = 1;

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
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        for length in [entries.len(), names.len(), free.len()] {
            out.write_all(&(length as u64).to_le_bytes())?;
        }
        for entry in entries {
            out.write_all(&entry.to_bytes())?;
        }
        out.write_all(names.as_bytes())?;
        for at in free {
            out.write_all(&at.to_le_bytes())?;
        }
        Ok(())
    }

    /// Reads back what `save` wrote, all of it and nothing after.
    pub fn load(input: &mut impl Read) -> Result<Index, LoadError> {
        let magic = exactly(input, MAGIC.len()).map_err(|_| LoadError::Foreign)?;
        let version = exactly(input, 4)?;
        if magic != MAGIC || version != VERSION.to_le_bytes() {
            return Err(LoadError::Foreign);
        }
        let lengths = exactly(input, 3 * 8)?;
        // Offsets are u32: no part of a snapshot is longer.
        let mut lengths = lengths.as_chunks::<8>().0.iter().map(|&length| {
            let length = u32::try_from(u64::from_le_bytes(length));
            length.ok().map(|length| length as usize)
        });
        let mut next = || lengths.next().flatten().ok_or(LoadError::Damaged);
        let (entries, names, free) = (next()?, next()?, next()?);
        let entries = exactly(input, entries * 16)?;
        let names = String::from_utf8(exactly(input, names)?).map_err(|_| LoadError::Damaged)?;
        let free = exactly(input, free * 4)?;
        // Its first value never matters: a read either fills it or not.
        let mut probe = [u8::MIN];
        if input.read(&mut probe).map_err(LoadError::Io)? > 0 {
            return Err(LoadError::Damaged);
        }
        let entries = entries.as_chunks::<16>().0.iter();
        let free = free.as_chunks::<4>().0.iter();
        let parts = Parts {
            entries: entries.map(|&bytes| Entry::from_bytes(bytes)).collect(),
            names,
            free: free.map(|&bytes| u32::from_le_bytes(bytes)).collect(),
        };
        Index::from_parts(parts).ok_or(LoadError::Damaged)
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

    /// The magic, the version and three lengths, in bytes.
    const HEADER: usize = 8 + 4 + 3 * 8;

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
    fn loading_takes_no_more_memory_than_it_needs() {
        let back = loaded(&saved(&small())).expect("load");
        // Four entries; C:, a, gone and Ёлка.txt; the three live ones
        // folded; one free place.
        assert_eq!(back.memory(), 4 * 16 + 19 + 15 + 4);
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
        let bytes = saved(&small());
        let free = bytes.len() - 4;
        assert!(damaged(&with(&bytes, free, 1)), "a live entry");
        assert!(damaged(&with(&bytes, free, 9)), "past the end");
        let mut index = small();
        let c = EntryId(0);
        let x = index.add(c, "x", false).unwrap();
        let y = index.add(c, "y", false).unwrap();
        index.remove(x).unwrap();
        index.remove(y).unwrap();
        let mut index_bytes = saved(&index);
        let len = index_bytes.len();
        let first = index_bytes[len - 8..len - 4].to_vec();
        index_bytes[len - 4..].copy_from_slice(&first);
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
}
