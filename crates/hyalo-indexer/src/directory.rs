//! A folder's entries as Windows lists them by handle
//! (`FILE_ID_BOTH_DIR_INFO`, one after another): the service asks for
//! them to send the files under a folder that came into the index whole.
//! Pure: it reads what Windows gave, and checks it where it comes in.

use crate::bytes::{number, u32_at, u64_at};

/// Where an entry's file reference is.
const FILE_ID: usize = 96;
/// Where an entry's name is.
const NAME: usize = 104;

/// The records of the entries in `listed`, but for `.` and `..`, up to
/// the first that does not read.
pub fn records(listed: &[u8]) -> Vec<u32> {
    let mut records = Vec::new();
    let mut at = 0;
    while let Some((record, next)) = entry(listed, at) {
        records.extend(record);
        if next == 0 {
            break;
        }
        at += next;
    }
    records
}

/// The entry at `at`: its file's record, unless it is the folder itself,
/// its parent or beyond the index's numbers; and how far on the next one
/// is, none for the last.
fn entry(listed: &[u8], at: usize) -> Option<(Option<u32>, usize)> {
    let next = u32_at(listed, at)? as usize;
    let length = u32_at(listed, at + 60)? as usize;
    let name = listed.get(at + NAME..at + NAME + length)?;
    let reference = u64_at(listed, at + FILE_ID)?;
    let own = name == b".\0" || name == b".\0.\0";
    Some(((!own).then(|| number(reference)).flatten(), next))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry as Windows lists it: its file's record, its name, and how
    /// far the next one is (none for the last).
    fn entry(record: u32, name: &str, next: bool) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let length = (NAME + units.len()).next_multiple_of(8);
        let mut bytes = vec![0x77; length];
        let next = if next { length as u32 } else { 0 };
        bytes[0..4].copy_from_slice(&next.to_le_bytes());
        bytes[60..64].copy_from_slice(&(units.len() as u32).to_le_bytes());
        let reference = u64::from(record) | 3 << 48;
        bytes[FILE_ID..FILE_ID + 8].copy_from_slice(&reference.to_le_bytes());
        bytes[NAME..NAME + units.len()].copy_from_slice(&units);
        bytes
    }

    #[test]
    fn a_folder_s_entries_are_its_files_records_without_itself_and_its_parent() {
        let listed = [
            entry(40, ".", true),
            entry(5, "..", true),
            entry(41, "a.txt", true),
            entry(42, "..hidden", true),
            entry(43, "Ёлка", false),
            entry(44, "after the last", false),
        ]
        .concat();
        assert_eq!(records(&listed), [41, 42, 43]);
    }

    #[test]
    fn an_entry_that_does_not_read_ends_the_list() {
        let mut cut = [entry(41, "a.txt", true), entry(42, "b.txt", false)].concat();
        cut.truncate(cut.len() - 8);
        assert_eq!(records(&cut), [41], "the last cut short");
        let mut long_name = entry(41, "a.txt", false);
        long_name[60..64].copy_from_slice(&999_u32.to_le_bytes());
        assert_eq!(records(&long_name), Vec::<u32>::new());
        let mut far = [entry(41, "a.txt", true)].concat();
        far[0..4].copy_from_slice(&9_999_u32.to_le_bytes());
        assert_eq!(records(&far), [41], "a next beyond the end");
        let mut huge = entry(41, "a.txt", false);
        huge[FILE_ID..FILE_ID + 8].copy_from_slice(&(1_u64 << 40).to_le_bytes());
        assert_eq!(records(&huge), Vec::<u32>::new());
        assert_eq!(records(&[]), Vec::<u32>::new());
    }
}
