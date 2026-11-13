use super::*;
use crate::mft::build::name;

fn folder(parent: u32, text: &str) -> Record {
    Record {
        folder: true,
        names: vec![name(parent, text)],
        written: Some(100),
        ..Record::default()
    }
}

fn file(names: &[(u32, &str)], size: u64) -> Record {
    Record {
        names: names
            .iter()
            .map(|&(parent, text)| name(parent, text))
            .collect(),
        written: Some(200),
        size: Some(size),
        ..Record::default()
    }
}

fn listed(records: Vec<(u32, Record)>) -> Vec<Change> {
    let mut listing = Listing::default();
    for (record, read) in records {
        listing.take(record, read);
    }
    listing.changes().collect()
}

fn there(record: u32, folder: bool, size: u64, written: i64, names: &[(u32, &str)]) -> Change {
    Change {
        record,
        file: Some(File {
            folder,
            size,
            written,
            names: names
                .iter()
                .map(|&(parent, text)| name(parent, text))
                .collect(),
        }),
    }
}

/// The records named, in the order listed.
fn order(changes: &[Change]) -> Vec<u32> {
    changes.iter().map(|change| change.record).collect()
}

#[test]
fn folders_come_first_each_after_its_own_then_the_files_by_record() {
    let changes = listed(vec![
        (ROOT, folder(ROOT, ".")),
        (20, file(&[(30, "deep.txt")], 7)),
        (30, folder(40, "inner")),
        (40, folder(ROOT, "outer")),
        (50, file(&[(ROOT, "top.txt")], 9)),
        (60, folder(ROOT, "beside")),
    ]);
    assert_eq!(order(&changes), [40, 60, 30, 20, 50]);
    assert_eq!(changes[2], there(30, true, 0, 100, &[(40, "inner")]));
    assert_eq!(changes[3], there(20, false, 7, 200, &[(30, "deep.txt")]));
}

#[test]
fn every_name_of_a_file_comes_with_it() {
    let changes = listed(vec![
        (40, folder(ROOT, "a")),
        (41, folder(ROOT, "b")),
        (50, file(&[(40, "one.txt"), (41, "two.txt")], 3)),
    ]);
    assert_eq!(
        changes[2],
        there(50, false, 3, 200, &[(40, "one.txt"), (41, "two.txt")])
    );
}

#[test]
fn an_extension_record_s_names_and_size_go_with_its_file() {
    let base = Record {
        size: None,
        ..file(&[(ROOT, "many links.txt")], 0)
    };
    let extension = Record {
        base: Some(70),
        names: vec![name(ROOT, "another link.txt")],
        size: Some(4_000),
        ..Record::default()
    };
    let changes = listed(vec![(71, extension), (70, base)]);
    assert_eq!(
        changes,
        [there(
            70,
            false,
            4_000,
            200,
            &[(ROOT, "many links.txt"), (ROOT, "another link.txt")]
        )]
    );
}

#[test]
fn an_extension_record_whose_file_was_not_read_is_left_out() {
    let extension = Record {
        base: Some(70),
        names: vec![name(ROOT, "orphan link.txt")],
        ..Record::default()
    };
    assert_eq!(listed(vec![(71, extension)]), []);
}

#[test]
fn folders_not_under_the_root_are_left_out() {
    let changes = listed(vec![
        (40, folder(999, "lost")),
        (41, folder(40, "under lost")),
        (42, folder(43, "loop one")),
        (43, folder(42, "loop two")),
        (44, folder(ROOT, "kept")),
        (
            45,
            Record {
                folder: true,
                ..Record::default()
            },
        ),
    ]);
    assert_eq!(order(&changes), [44]);
}

#[test]
fn a_file_without_its_last_write_or_size_says_none() {
    let bare = Record {
        names: vec![name(ROOT, "bare")],
        ..Record::default()
    };
    assert_eq!(
        listed(vec![(80, bare)]),
        [there(80, false, 0, 0, &[(ROOT, "bare")])]
    );
}

#[test]
fn listings_read_apart_join_into_one() {
    let mut first = Listing::default();
    first.take(40, folder(ROOT, "a"));
    first.take(
        71,
        Record {
            base: Some(50),
            names: vec![name(40, "second.txt")],
            ..Record::default()
        },
    );
    let mut second = Listing::default();
    second.take(50, file(&[(ROOT, "first.txt")], 5));
    second.take(30, folder(40, "b"));
    first.join(second);
    let changes: Vec<Change> = first.changes().collect();
    assert_eq!(order(&changes), [40, 30, 50]);
    assert_eq!(
        changes[2],
        there(
            50,
            false,
            5,
            200,
            &[(ROOT, "first.txt"), (40, "second.txt")]
        )
    );
}
