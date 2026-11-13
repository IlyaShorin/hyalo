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
mod tests;
