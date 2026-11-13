use super::*;
use hyalo_core::volume::{File, Name, ROOT};

fn change(record: u32, folder: bool, names: &[(u32, &str)], size: u64, written: i64) -> Change {
    let names = names.iter().map(|&(parent, text)| Name {
        parent,
        text: text.to_string(),
    });
    Change {
        record,
        file: Some(File {
            folder,
            size,
            written,
            names: names.collect(),
        }),
    }
}

fn folder(record: u32, parent: u32, name: &str) -> Change {
    change(record, true, &[(parent, name)], 0, 1_000)
}

fn file(record: u32, parent: u32, name: &str) -> Change {
    change(record, false, &[(parent, name)], 5, 2_000)
}

fn gone(record: u32) -> Change {
    Change { record, file: None }
}

/// C: holding Users\Me\notes.txt (records 40, 41, 50) and Windows (60).
fn volume() -> (Index, EntryId) {
    let mut index = Index::default();
    let c = index.add_root("C:").unwrap();
    for listed in [
        folder(40, ROOT, "Users"),
        folder(41, 40, "Me"),
        folder(60, ROOT, "Windows"),
        file(50, 41, "notes.txt"),
    ] {
        index.settle(c, &listed).unwrap();
    }
    (index, c)
}

fn found(index: &Index, path: &str) -> bool {
    index.find(path).is_some()
}

#[test]
fn a_listing_settles_into_entries_under_the_root() {
    let (index, _) = volume();
    let notes = index.find(r"C:\Users\Me\notes.txt").expect("listed");
    assert_eq!(index.stat(notes), Some((5, 2_000)));
    assert_eq!(index.is_folder(notes), Some(false));
    let me = index.find(r"C:\Users\Me").expect("listed");
    assert_eq!(index.is_folder(me), Some(true));
    assert_eq!(index.stat(me), Some((0, 1_000)));
    assert_eq!(index.len(), 5, "the root, three folders, a file");
}

#[test]
fn settling_the_same_again_changes_nothing() {
    let (mut index, c) = volume();
    let version = index.version();
    index.settle(c, &file(50, 41, "notes.txt")).unwrap();
    index.settle(c, &folder(41, 40, "Me")).unwrap();
    assert_eq!(index.version(), version);
    assert_eq!(index.len(), 5);
}

#[test]
fn a_file_renamed_or_moved_keeps_its_entry() {
    let (mut index, c) = volume();
    let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
    index.settle(c, &file(50, 41, "renamed.txt")).unwrap();
    assert_eq!(index.find(r"C:\Users\Me\renamed.txt"), Some(notes));
    index.settle(c, &file(50, 60, "renamed.txt")).unwrap();
    assert_eq!(index.find(r"C:\Windows\renamed.txt"), Some(notes));
    assert!(!found(&index, r"C:\Users\Me\notes.txt"));
    assert_eq!(index.len(), 5);
}

#[test]
fn a_folder_renamed_keeps_what_it_holds() {
    let (mut index, c) = volume();
    let moved = index.settle(c, &folder(41, 60, "You")).unwrap();
    assert!(found(&index, r"C:\Windows\You\notes.txt"));
    assert!(!moved, "nothing to ask for: it was here");
}

#[test]
fn a_gone_folder_leaves_with_everything_under_it() {
    let (mut index, c) = volume();
    index.settle(c, &gone(41)).unwrap();
    assert!(!found(&index, r"C:\Users\Me"));
    assert!(!found(&index, r"C:\Users\Me\notes.txt"));
    assert_eq!(index.len(), 3);
    index.settle(c, &gone(41)).unwrap();
    index.settle(c, &gone(999)).unwrap();
    assert_eq!(index.len(), 3, "gone again, or never there: nothing");
}

#[test]
fn a_file_has_an_entry_for_each_hard_link() {
    let (mut index, c) = volume();
    let both = change(70, false, &[(40, "a.txt"), (60, "b.txt")], 7, 3_000);
    index.settle(c, &both).unwrap();
    let (a, b) = (
        index.find(r"C:\Users\a.txt"),
        index.find(r"C:\Windows\b.txt"),
    );
    assert_eq!(a.and_then(|a| index.stat(a)), Some((7, 3_000)));
    assert_eq!(b.and_then(|b| index.stat(b)), Some((7, 3_000)));
    let one = change(70, false, &[(60, "b.txt")], 8, 4_000);
    index.settle(c, &one).unwrap();
    assert!(!found(&index, r"C:\Users\a.txt"));
    assert_eq!(
        index.find(r"C:\Windows\b.txt"),
        b,
        "the link left keeps its entry"
    );
    assert_eq!(b.and_then(|b| index.stat(b)), Some((8, 4_000)));
    index.settle(c, &gone(70)).unwrap();
    assert!(!found(&index, r"C:\Windows\b.txt"));
}

#[test]
fn a_name_in_a_folder_the_index_does_not_hold_is_left_out() {
    let (mut index, c) = volume();
    index.settle(c, &file(80, 999, "nowhere.txt")).unwrap();
    assert_eq!(index.len(), 5);
    // Moved into a folder the index leaves out, such as the recycle bin.
    index.settle(c, &folder(41, 999, "$R123")).unwrap();
    assert!(!found(&index, r"C:\Users\Me"));
    assert!(!found(&index, r"C:\Users\Me\notes.txt"));
    // A file's name is never a folder to put another in.
    index.settle(c, &file(60, ROOT, "Windows")).unwrap();
    index.settle(c, &file(81, 60, "inside a file.txt")).unwrap();
    assert_eq!(index.len(), 3);
}

#[test]
fn a_folder_that_comes_in_says_its_files_must_be_asked_for() {
    let (mut index, c) = volume();
    assert!(index.settle(c, &folder(41, 999, "binned")).is_ok());
    assert_eq!(
        index.settle(c, &folder(41, 40, "Me")),
        Ok(true),
        "back from the bin"
    );
    assert_eq!(index.settle(c, &folder(41, 40, "Me")), Ok(false));
    assert_eq!(index.settle(c, &folder(90, 40, "new")), Ok(true));
    assert_eq!(index.settle(c, &file(91, 90, "new.txt")), Ok(false));
}

#[test]
fn a_file_s_size_and_last_write_follow_it() {
    let (mut index, c) = volume();
    let grown = change(50, false, &[(41, "notes.txt")], 900, 9_000);
    index.settle(c, &grown).unwrap();
    let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
    assert_eq!(index.stat(notes), Some((900, 9_000)));
}

#[test]
fn a_file_that_became_a_folder_is_one_now() {
    let (mut index, c) = volume();
    let now = folder(50, 41, "notes.txt");
    assert_eq!(index.settle(c, &now), Ok(true));
    let notes = index.find(r"C:\Users\Me\notes.txt").unwrap();
    assert_eq!(index.is_folder(notes), Some(true));
    index.settle(c, &file(51, 50, "inside.txt")).unwrap();
    assert!(found(&index, r"C:\Users\Me\notes.txt\inside.txt"));
}

#[test]
fn a_name_another_file_had_goes_to_the_one_that_has_it_now() {
    let (mut index, c) = volume();
    index.settle(c, &file(92, 40, "x.txt")).unwrap();
    // 93 took the name before 92's going was read.
    let taken = change(93, false, &[(40, "X.TXT")], 11, 5_000);
    index.settle(c, &taken).unwrap();
    let x = index.find(r"C:\Users\x.txt").expect("there");
    assert_eq!(index.stat(x), Some((11, 5_000)));
    assert_eq!(index.name(x), Some("X.TXT"));
    index.settle(c, &gone(92)).unwrap();
    assert_eq!(
        index.find(r"C:\Users\x.txt"),
        Some(x),
        "92's going takes not 93's"
    );
}

#[test]
fn two_volumes_keep_their_records_apart() {
    let (mut index, c) = volume();
    let d = index.add_root("D:").unwrap();
    index.settle(d, &folder(40, ROOT, "Data")).unwrap();
    index.settle(d, &file(50, 40, "notes.txt")).unwrap();
    index.settle(d, &gone(41)).unwrap();
    assert!(found(&index, r"C:\Users\Me\notes.txt"));
    index.settle(c, &gone(50)).unwrap();
    assert!(found(&index, r"D:\Data\notes.txt"));
    assert!(!found(&index, r"C:\Users\Me\notes.txt"));
}

#[test]
fn an_entry_removed_by_its_path_is_not_found_by_its_record() {
    let (mut index, c) = volume();
    let me = index.find(r"C:\Users\Me").unwrap();
    index.remove(me).unwrap();
    // Its place goes to an entry a walk added, with no record.
    let users = index.find(r"C:\Users").unwrap();
    let walked = index.add(users, "walked.txt", false).unwrap();
    index.settle(c, &gone(41)).unwrap();
    index.settle(c, &gone(50)).unwrap();
    assert_eq!(index.find(r"C:\Users\walked.txt"), Some(walked));
    index.settle(c, &file(50, 41, "notes.txt")).unwrap();
    assert!(!found(&index, r"C:\Users\Me\notes.txt"), "its folder went");
}

#[test]
fn only_a_root_takes_changes() {
    let (mut index, _) = volume();
    let users = index.find(r"C:\Users").unwrap();
    assert_eq!(
        index.settle(users, &file(95, ROOT, "a")),
        Err(Error::NoSuchEntry)
    );
    assert_eq!(
        index.settle(EntryId(999), &file(95, ROOT, "a")),
        Err(Error::NoSuchEntry)
    );
}

#[test]
fn a_name_the_index_refuses_is_left_out() {
    let (mut index, c) = volume();
    let bad = change(96, false, &[(40, ""), (40, r"a\b"), (40, "good.txt")], 1, 1);
    index.settle(c, &bad).unwrap();
    assert!(found(&index, r"C:\Users\good.txt"));
    assert_eq!(index.len(), 6);
}

fn position(next: i64) -> Position {
    Position {
        volume: 0xABCD,
        journal: 7,
        next,
    }
}

#[test]
fn a_volume_keeps_how_far_its_journal_was_taken_in() {
    let (mut index, c) = volume();
    assert_eq!(index.position(c), None);
    index.set_position(c, position(4_096)).unwrap();
    index.set_position(c, position(8_192)).unwrap();
    assert_eq!(index.position(c), Some(position(8_192)));
    let d = index.add_root("D:").unwrap();
    assert_eq!(index.position(d), None);
    let users = index.find(r"C:\Users").unwrap();
    assert_eq!(
        index.set_position(users, position(1)),
        Err(Error::NoSuchEntry)
    );
    index.remove(c).unwrap();
    let again = index.add_root("C:").unwrap();
    assert_eq!(
        index.position(again),
        None,
        "a root made again starts afresh"
    );
}

#[test]
fn a_volume_put_in_place_of_another_comes_with_its_records_and_position() {
    let mut walked = Index::default();
    let c = walked.add_root("C:").unwrap();
    walked.add(c, "walked.txt", false).unwrap();
    walked.add_root("D:").unwrap();
    let (mut fresh, fresh_c) = volume();
    fresh.set_position(fresh_c, position(77)).unwrap();
    walked.replace(&fresh);
    let c = walked.find("C:").unwrap();
    assert!(!found(&walked, r"C:\walked.txt"));
    assert_eq!(walked.position(c), Some(position(77)));
    walked.settle(c, &file(50, 60, "moved.txt")).unwrap();
    assert!(found(&walked, r"C:\Windows\moved.txt"));
    assert!(!found(&walked, r"C:\Users\Me\notes.txt"));
    let both = change(97, false, &[(40, "a"), (60, "b")], 1, 1);
    fresh.settle(fresh_c, &both).unwrap();
    walked.replace(&fresh);
    let c = walked.find("C:").unwrap();
    walked.settle(c, &gone(97)).unwrap();
    assert!(!found(&walked, r"C:\Users\a"), "every link's entry known");
    assert!(!found(&walked, r"C:\Windows\b"));
}
