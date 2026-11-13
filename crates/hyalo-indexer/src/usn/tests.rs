use super::*;

/// A version 2 record of a change to the file `record` in `parent`.
fn change(record: u32, parent: u32, name: &str) -> Vec<u8> {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let length = (60 + units.len()).next_multiple_of(8);
    let mut bytes = vec![0; length];
    bytes[0..4].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
    let reference = |record: u32| (u64::from(record) | 5 << 48).to_le_bytes();
    bytes[8..16].copy_from_slice(&reference(record));
    bytes[16..24].copy_from_slice(&reference(parent));
    bytes[40..44].copy_from_slice(&FILE_CREATE.to_le_bytes());
    bytes[56..58].copy_from_slice(&(units.len() as u16).to_le_bytes());
    bytes[58..60].copy_from_slice(&60_u16.to_le_bytes());
    bytes[60..60 + units.len()].copy_from_slice(&units);
    bytes
}

/// What a read answers: the place to read on from, then the records.
fn answer(next: i64, records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = next.to_le_bytes().to_vec();
    records.iter().for_each(|record| bytes.extend(record));
    bytes
}

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

const VOLUME: u64 = 0x1234_5678_9ABC_DEF0;

fn position(next: i64) -> Position {
    Position {
        volume: VOLUME,
        journal: 7,
        next,
    }
}

fn journal(lowest: i64, next: i64) -> Journal {
    Journal {
        id: 7,
        lowest,
        next,
    }
}

#[test]
fn a_saved_place_is_read_on_from_while_the_journal_still_holds_it() {
    assert!(position(5_000).follows(VOLUME, journal(1_000, 9_000)));
    assert!(
        position(1_000).follows(VOLUME, journal(1_000, 9_000)),
        "the oldest kept"
    );
    assert!(
        position(9_000).follows(VOLUME, journal(1_000, 9_000)),
        "nothing since"
    );
}

#[test]
fn a_saved_place_let_go_or_ahead_of_the_journal_is_not() {
    assert!(
        !position(999).follows(VOLUME, journal(1_000, 9_000)),
        "let go"
    );
    assert!(
        !position(9_001).follows(VOLUME, journal(1_000, 9_000)),
        "ahead"
    );
}

#[test]
fn another_journal_or_volume_is_read_from_nothing() {
    let other = Journal {
        id: 8,
        ..journal(1_000, 9_000)
    };
    assert!(!position(5_000).follows(VOLUME, other));
    assert!(!position(5_000).follows(VOLUME + 1, journal(1_000, 9_000)));
}

#[test]
fn the_reasons_asked_for_are_those_that_change_what_the_index_holds() {
    assert_eq!(REASONS, 0x1_b307);
}
