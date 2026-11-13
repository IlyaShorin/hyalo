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
mod tests;
