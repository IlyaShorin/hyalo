//! The USN journal: NTFS's record of what changed on a volume, read from a
//! place on. Pure: it reads what the volume gave.

use std::collections::HashSet;

use crate::bytes::{i64_at, number, u16_at, u32_at, u64_at};

/// Why a change is worth reading the file again for: its data, its name,
/// its place, its times, its links, its coming and going.
pub const REASONS: u32 = DATA_OVERWRITE
    | DATA_EXTEND
    | DATA_TRUNCATION
    | FILE_CREATE
    | FILE_DELETE
    | RENAME_OLD_NAME
    | RENAME_NEW_NAME
    | BASIC_INFO_CHANGE
    | HARD_LINK_CHANGE;

const DATA_OVERWRITE: u32 = 0x1;
const DATA_EXTEND: u32 = 0x2;
const DATA_TRUNCATION: u32 = 0x4;
const FILE_CREATE: u32 = 0x100;
const FILE_DELETE: u32 = 0x200;
const RENAME_OLD_NAME: u32 = 0x1000;
const RENAME_NEW_NAME: u32 = 0x2000;
const BASIC_INFO_CHANGE: u32 = 0x8000;
const HARD_LINK_CHANGE: u32 = 0x1_0000;

/// What a read of the journal gave: the place to read on from, and the
/// records of the files that changed, each once, in the order they first
/// changed; none if it is no such answer.
pub fn changed(answer: &[u8]) -> Option<(i64, Vec<u32>)> {
    let next = i64_at(answer, 0)?;
    let mut seen = HashSet::new();
    let records = records(answer).filter(|&record| seen.insert(record));
    Some((next, records.collect()))
}

/// The bytes a version 2 record holds before its name.
const HEAD: usize = 60;

/// The files the answer's records are about, up to the first record that
/// does not fit; records of other versions are passed over.
fn records(answer: &[u8]) -> impl Iterator<Item = u32> + '_ {
    let mut at = 8;
    let whole = std::iter::from_fn(move || {
        let length = u32_at(answer, at).map(|length| length as usize)?;
        let record = answer
            .get(at..at.checked_add(length)?)
            .filter(|_| length >= HEAD)?;
        at += length;
        Some(record)
    });
    whole.filter_map(|record| {
        let second = u16_at(record, 4) == Some(2);
        second.then(|| number(u64_at(record, 8)?)).flatten()
    })
}

#[cfg(test)]
pub(crate) mod build;

#[cfg(test)]
mod tests {
    use super::build::{answer, change};
    use super::*;

    #[test]
    fn a_read_of_the_journal_says_which_files_changed_each_once() {
        let read = answer(
            4096,
            &[
                change(70, 5, "new folder"),
                change(71, 70, "a.txt"),
                change(70, 5, "renamed"),
                change(72, 70, "b.txt"),
                change(71, 70, "a.txt"),
            ],
        );
        assert_eq!(changed(&read), Some((4096, vec![70, 71, 72])));
    }

    #[test]
    fn a_read_with_nothing_new_says_where_to_read_on() {
        assert_eq!(changed(&answer(77, &[])), Some((77, vec![])));
        assert_eq!(changed(&[1, 2, 3]), None, "no place");
    }

    #[test]
    fn a_record_of_another_version_is_passed_over() {
        let mut third = change(80, 5, "v3");
        third[4..6].copy_from_slice(&3_u16.to_le_bytes());
        let read = answer(9, &[third, change(81, 5, "v2")]);
        assert_eq!(changed(&read), Some((9, vec![81])));
    }

    #[test]
    fn a_record_that_does_not_fit_ends_the_read() {
        let mut cut = change(90, 5, "cut");
        cut[0..4].copy_from_slice(&4000_u32.to_le_bytes());
        let read = answer(9, &[change(91, 5, "kept"), cut, change(92, 5, "after")]);
        assert_eq!(changed(&read), Some((9, vec![91])));
        let mut empty = change(93, 5, "nothing");
        empty[0..4].copy_from_slice(&0_u32.to_le_bytes());
        let read = answer(9, &[change(94, 5, "kept"), empty]);
        assert_eq!(changed(&read), Some((9, vec![94])), "no length: no loop");
        let short = answer(9, &[change(95, 5, "kept")[..40].to_vec()]);
        assert_eq!(changed(&short), Some((9, vec![])), "shorter than a record");
    }

    #[test]
    fn a_record_beyond_the_index_s_numbers_is_passed_over() {
        let mut huge = change(0, 5, "far");
        huge[8..16].copy_from_slice(&((1_u64 << 40) | 1).to_le_bytes());
        let read = answer(9, &[huge, change(96, 5, "near")]);
        assert_eq!(changed(&read), Some((9, vec![96])));
    }

    #[test]
    fn the_reasons_asked_for_are_those_that_change_what_the_index_holds() {
        assert_eq!(REASONS, 0x1_b307);
    }
}
