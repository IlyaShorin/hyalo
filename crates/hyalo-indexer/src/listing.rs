//! A volume's listing: the records of its whole table put together and
//! ordered so that whoever takes them in knows every folder before what
//! it holds.

use std::collections::VecDeque;

use crate::mft::Record;
use hyalo_core::volume::{Change, File, Name, ROOT};

/// The records read of a volume's table so far, kept small: a name,
/// stretch of a shared text, per item.
#[derive(Debug, Default)]
pub struct Listing {
    items: Vec<Item>,
    names: String,
}

/// One record's name, or a record with none.
#[derive(Debug, Clone, Copy)]
struct Item {
    /// The file's own record: the base, for an extension record.
    record: u32,
    /// Whether it was read from the file's own record, which says whether
    /// it is a folder and when it was last written.
    own: bool,
    folder: bool,
    size: Option<u64>,
    written: i64,
    /// The folder the name is in, and where the name lies in `names`; no
    /// name when its length is none.
    parent: u32,
    at: u32,
    length: u32,
}

/// The parent of a record with no name.
const NONE: u32 = u32::MAX;

impl Listing {
    /// Takes in the record `record` of the table, as read.
    pub fn take(&mut self, record: u32, read: Record) {
        let item = Item {
            record: read.base.unwrap_or(record),
            own: read.base.is_none(),
            folder: read.folder,
            size: read.size,
            written: read.written.unwrap_or(0),
            parent: NONE,
            at: 0,
            length: 0,
        };
        if read.names.is_empty() {
            self.items.push(item);
        }
        for name in read.names {
            let at = self.names.len() as u32;
            self.names.push_str(&name.text);
            self.items.push(Item {
                parent: name.parent,
                at,
                length: name.text.len() as u32,
                ..item
            });
        }
    }

    /// Takes in what another reader of the same table read.
    pub fn join(&mut self, other: Listing) {
        let shift = self.names.len() as u32;
        let moved = other.items.into_iter().map(|item| Item {
            at: item.at + shift,
            ..item
        });
        self.items.extend(moved);
        self.names.push_str(&other.names);
    }

    /// Every file and folder, each with all its names: the folders first,
    /// each after the folder it is in, leaving out those not under the
    /// root; then the files, by record.
    pub fn changes(self) -> impl Iterator<Item = Change> {
        let Listing { mut items, names } = self;
        // A file's own record first among its items.
        items.sort_by_key(|item| (item.record, !item.own));
        let files = files(&items);
        let folders = under_root(&items, &files);
        let others = files.into_iter().filter(|&(at, _)| !items[at].folder);
        let order: Vec<(usize, usize)> = folders.into_iter().chain(others).collect();
        order
            .into_iter()
            .map(move |(at, count)| change(&items[at..at + count], &names))
    }
}

/// Where each file's items start among `items`, sorted by record with its
/// own record first, and how many there are; files whose own record was
/// not read are left out.
fn files(items: &[Item]) -> Vec<(usize, usize)> {
    let mut start = 0;
    let groups = items.chunk_by(|one, other| one.record == other.record);
    let groups = groups.map(|group| {
        let found = (start, group.len());
        start += group.len();
        found
    });
    groups.filter(|&(at, _)| items[at].own).collect()
}

/// The folders among `files` under the root, each after the folder it is
/// in.
fn under_root(items: &[Item], files: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut folders: Vec<(u32, (usize, usize))> = files
        .iter()
        .filter(|&&(at, _)| items[at].folder && items[at].record != ROOT)
        .filter(|&&(at, _)| items[at].parent != NONE)
        .map(|&file| (items[file.0].parent, file))
        .collect();
    folders.sort_by_key(|&(parent, _)| parent);
    let mut order = Vec::new();
    let mut waiting = VecDeque::from([ROOT]);
    while let Some(parent) = waiting.pop_front() {
        let first = folders.partition_point(|&(of, _)| of < parent);
        let within = folders[first..].iter().take_while(|&&(of, _)| of == parent);
        for &(_, folder) in within {
            order.push(folder);
            waiting.push_back(items[folder.0].record);
        }
    }
    order
}

/// What one file's items say of it.
fn change(items: &[Item], names: &str) -> Change {
    let own = items[0];
    let named = items.iter().filter(|item| item.parent != NONE);
    let names = named.map(|item| Name {
        parent: item.parent,
        text: names[item.at as usize..(item.at + item.length) as usize].to_string(),
    });
    Change {
        record: own.record,
        file: Some(File {
            folder: own.folder,
            size: items.iter().find_map(|item| item.size).unwrap_or(0),
            written: own.written,
            names: names.collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
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
}
