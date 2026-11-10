//! What a watch reports, turned into changes to the index, by the rules
//! the walk goes by: what it would not list stays out, and a folder that
//! turns up whole, moved in from where the index does not look, is
//! walked.

use std::path::Path;

use hyalo_index::{EntryId, Index};

use super::{Excluded, kept, lowercase};

/// What a name is, as the walk takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    /// A junction or a symbolic link to a folder: listed as a folder,
    /// never entered.
    Link,
}

const DIRECTORY: u32 = 0x10;
const REPARSE_POINT: u32 = 0x400;
/// The bit of a reparse tag that says the point names another place, as
/// junctions and symbolic links do: std takes such a folder as a link.
const NAME_SURROGATE: u32 = 0x2000_0000;

impl Kind {
    /// The kind of a name with these attributes and reparse tag. Other
    /// reparse points, such as OneDrive's folders, are what they hold.
    pub fn of(attributes: u32, tag: u32) -> Kind {
        let folder = attributes & DIRECTORY != 0;
        let link = attributes & REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0;
        match (folder, link) {
            (false, _) => Kind::File,
            (true, false) => Kind::Folder,
            (true, true) => Kind::Link,
        }
    }

    fn folder(self) -> bool {
        self != Kind::File
    }
}

/// What happened to a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Added,
    Removed,
    /// Its contents or times changed.
    Modified,
    /// Renamed or moved: its old name, then its new one.
    MovedFrom,
    MovedTo,
}

/// How many bytes a name holds and when it was last written, in seconds
/// since 1970.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    pub size: u64,
    pub written: i64,
}

/// One thing a watch reports: what happened to the name at the full path
/// `path`, what it is, and its size and time then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub action: Action,
    pub path: String,
    pub kind: Kind,
    pub stat: Stat,
}

/// One change to make in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(String, Kind, Stat),
    Removed(String),
    /// From the first path to the second.
    Moved(String, String, Kind, Stat),
    /// Its size or time changed.
    Touched(String, Stat),
}

/// The changes `records` make, in their order: an old name and the new
/// name after it are one move; an old name alone is a name gone where the
/// watch does not see, a new name alone one come from there. A change
/// just like the one before it is made once, and of two changes of size
/// or time to one name in a row, the later.
pub fn changes(records: &[Record]) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut records = records.iter().peekable();
    while let Some(record) = records.next() {
        let change = match record.action {
            Action::Added | Action::MovedTo => {
                Change::Added(record.path.clone(), record.kind, record.stat)
            }
            Action::Removed => Change::Removed(record.path.clone()),
            Action::Modified => Change::Touched(record.path.clone(), record.stat),
            Action::MovedFrom => match records.next_if(|next| next.action == Action::MovedTo) {
                Some(to) => Change::Moved(record.path.clone(), to.path.clone(), to.kind, to.stat),
                None => Change::Removed(record.path.clone()),
            },
        };
        then(&mut changes, change);
    }
    changes
}

/// Puts `change` after `changes`, unless it is just like the last; one
/// of size or time takes the place of one just before it for that name.
fn then(changes: &mut Vec<Change>, change: Change) {
    if let (Some(Change::Touched(before, _)), Change::Touched(path, _)) = (changes.last(), &change)
        && before == path
    {
        changes.pop();
    }
    if changes.last() != Some(&change) {
        changes.push(change);
    }
}

/// Makes `change` in the index; the folder to walk if it brought one,
/// whose contents the index has not seen.
pub fn apply(index: &mut Index, change: &Change, excluded: &Excluded) -> Option<String> {
    match change {
        Change::Added(path, kind, stat) => add(index, path, *kind, *stat, excluded),
        Change::Removed(path) => {
            remove(index, path);
            None
        }
        Change::Moved(from, to, kind, stat) => match below_root(index, from) {
            Some(entry) => {
                move_to(index, entry, to, *stat, excluded);
                None
            }
            None => add(index, to, *kind, *stat, excluded),
        },
        Change::Touched(path, stat) => {
            if let Some(entry) = below_root(index, path) {
                set(index, entry, *stat);
            }
            None
        }
    }
}

/// Puts the name at `path` in the index if the walk would list it; a
/// folder it would enter is to be walked.
fn add(
    index: &mut Index,
    path: &str,
    kind: Kind,
    stat: Stat,
    excluded: &Excluded,
) -> Option<String> {
    let (folder, name) = listed(index, path, excluded)?;
    let entry = index.put(folder, name, kind.folder()).ok()?;
    set(index, entry, stat);
    (kind == Kind::Folder).then(|| path.to_string())
}

fn remove(index: &mut Index, path: &str) {
    if let Some(entry) = below_root(index, path) {
        // Found just now, so it is there to remove.
        let _ = index.remove(entry);
    }
}

/// Moves `entry` to `path`, or takes it out if the walk would not list
/// it there: it went where the index does not look.
fn move_to(index: &mut Index, entry: EntryId, path: &str, stat: Stat, excluded: &Excluded) {
    let moved = listed(index, path, excluded)
        .is_some_and(|(folder, name)| index.rename(entry, folder, name).is_ok());
    if moved {
        set(index, entry, stat);
    } else {
        // Found just now, so it is there to remove.
        let _ = index.remove(entry);
    }
}

/// Gives `entry`, found or put just now, its size and time.
fn set(index: &mut Index, entry: EntryId, stat: Stat) {
    // Found or put just now, so it is there.
    let _ = index.set_stat(entry, stat.size, stat.written);
}

/// The entry at `path`, unless it is a root, which no change removes.
fn below_root(index: &Index, path: &str) -> Option<EntryId> {
    index
        .find(path)
        .filter(|&entry| index.parent(entry).is_some())
}

/// The folder the name at `path` is in, and the name, if that folder is
/// in the index and the walk would list the name in it.
fn listed<'a>(index: &Index, path: &'a str, excluded: &Excluded) -> Option<(EntryId, &'a str)> {
    let (folder, name) = path.rsplit_once('\\')?;
    let entry = index.find(folder)?;
    let at_root = index.parent(entry).is_none();
    let left_out = excluded
        .within(Path::new(folder))
        .contains(&lowercase(name));
    (kept(name, at_root) && !left_out).then_some((entry, name))
}

/// The folders of `paths` to walk: each once, and none inside another,
/// whose walk takes it in.
pub fn outermost(paths: Vec<String>) -> Vec<String> {
    let lowered: Vec<String> = paths.iter().map(|path| lowercase(path)).collect();
    let inside = |at: usize| {
        let path = &lowered[at];
        lowered.iter().enumerate().any(|(other, outer)| {
            let under = path
                .strip_prefix(outer.as_str())
                .is_some_and(|rest| rest.starts_with('\\'));
            under || (outer == path && other < at)
        })
    };
    let kept = paths.into_iter().enumerate().filter(|&(at, _)| !inside(at));
    kept.map(|(_, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNCTION: u32 = 0xA000_0003;
    const SYMBOLIC_LINK: u32 = 0xA000_000C;
    const ONEDRIVE: u32 = 0x9000_701A;

    #[test]
    fn a_name_is_a_file_a_folder_or_a_link_as_the_walk_takes_it() {
        assert_eq!(Kind::of(0x20, 0), Kind::File);
        assert_eq!(Kind::of(DIRECTORY, 0), Kind::Folder);
        assert_eq!(Kind::of(DIRECTORY | REPARSE_POINT, JUNCTION), Kind::Link);
        assert_eq!(
            Kind::of(DIRECTORY | REPARSE_POINT, SYMBOLIC_LINK),
            Kind::Link
        );
        assert_eq!(
            Kind::of(DIRECTORY | REPARSE_POINT, ONEDRIVE),
            Kind::Folder,
            "OneDrive's folders are entered"
        );
        assert_eq!(Kind::of(REPARSE_POINT, SYMBOLIC_LINK), Kind::File);
        assert_eq!(Kind::of(DIRECTORY, SYMBOLIC_LINK), Kind::Folder, "no point");
    }

    fn record(action: Action, path: &str) -> Record {
        Record {
            action,
            path: path.to_string(),
            kind: Kind::File,
            stat: Stat::default(),
        }
    }

    #[test]
    fn an_old_name_and_the_new_one_after_it_are_one_move() {
        let records = [
            record(Action::MovedFrom, r"C:\a"),
            record(Action::MovedTo, r"C:\b"),
        ];
        assert_eq!(
            changes(&records),
            [Change::Moved(
                r"C:\a".into(),
                r"C:\b".into(),
                Kind::File,
                Stat::default()
            )]
        );
    }

    #[test]
    fn an_old_name_alone_is_gone_and_a_new_name_alone_came() {
        let records = [
            record(Action::MovedFrom, r"C:\a"),
            record(Action::Added, r"C:\c"),
            record(Action::MovedTo, r"C:\b"),
        ];
        assert_eq!(
            changes(&records),
            [
                Change::Removed(r"C:\a".into()),
                Change::Added(r"C:\c".into(), Kind::File, Stat::default()),
                Change::Added(r"C:\b".into(), Kind::File, Stat::default()),
            ]
        );
    }

    #[test]
    fn a_change_just_like_the_one_before_is_made_once() {
        let records = [
            record(Action::Added, r"C:\a"),
            record(Action::Added, r"C:\a"),
            record(Action::Removed, r"C:\a"),
            record(Action::Removed, r"C:\a"),
            record(Action::Added, r"C:\a"),
        ];
        assert_eq!(
            changes(&records),
            [
                Change::Added(r"C:\a".into(), Kind::File, Stat::default()),
                Change::Removed(r"C:\a".into()),
                Change::Added(r"C:\a".into(), Kind::File, Stat::default()),
            ]
        );
    }

    fn touched(path: &str, size: u64) -> Record {
        Record {
            stat: Stat { size, written: 7 },
            ..record(Action::Modified, path)
        }
    }

    #[test]
    fn of_two_changes_of_size_or_time_in_a_row_to_one_name_the_later_is_made() {
        let records = [
            touched(r"C:\a", 1),
            touched(r"C:\a", 2),
            touched(r"C:\b", 3),
            touched(r"C:\a", 4),
        ];
        let stat = |size| Stat { size, written: 7 };
        assert_eq!(
            changes(&records),
            [
                Change::Touched(r"C:\a".into(), stat(2)),
                Change::Touched(r"C:\b".into(), stat(3)),
                Change::Touched(r"C:\a".into(), stat(4)),
            ]
        );
    }

    /// C: with Users\me\notes.txt and Users\me\code\main.rs.
    fn drive() -> Index {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let users = index.add(c, "Users", true).unwrap();
        let me = index.add(users, "me", true).unwrap();
        index.add(me, "notes.txt", false).unwrap();
        let code = index.add(me, "code", true).unwrap();
        index.add(code, "main.rs", false).unwrap();
        index
    }

    fn made(index: &mut Index, changes: &[Change], excluded: &Excluded) -> Vec<String> {
        let walks = changes.iter().map(|change| apply(index, change, excluded));
        walks.flatten().collect()
    }

    fn added(path: &str, kind: Kind) -> Change {
        Change::Added(path.to_string(), kind, Stat::default())
    }

    fn there(index: &Index, path: &str) -> bool {
        index.find(path).is_some()
    }

    #[test]
    fn a_name_added_is_put_in_its_folder_once() {
        let mut index = drive();
        let change = added(r"C:\Users\me\todo.md", Kind::File);
        let walks = made(&mut index, &[change.clone(), change], &Excluded::default());
        assert!(walks.is_empty());
        assert!(there(&index, r"C:\Users\me\todo.md"));
        assert_eq!(index.len(), 7, "once");
    }

    #[test]
    fn a_folder_added_is_walked_a_link_is_not() {
        let mut index = drive();
        let changes = [
            added(r"C:\Users\me\new", Kind::Folder),
            added(r"C:\Users\me\elsewhere", Kind::Link),
        ];
        let walks = made(&mut index, &changes, &Excluded::default());
        assert_eq!(walks, [r"C:\Users\me\new"]);
        let link = index.find(r"C:\Users\me\elsewhere").expect("listed");
        assert_eq!(index.is_folder(link), Some(true));
    }

    #[test]
    fn a_name_in_a_folder_the_index_lacks_is_left_out() {
        let mut index = drive();
        let changes = [
            added(r"C:\Windows\Temp\x.tmp", Kind::File),
            added(r"D:\x.txt", Kind::File),
            added("nowhere", Kind::File),
        ];
        assert!(made(&mut index, &changes, &Excluded::default()).is_empty());
        assert_eq!(index.len(), 6);
    }

    #[test]
    fn what_the_walk_leaves_out_stays_out() {
        let mut index = drive();
        let excluded = Excluded::default();
        excluded.set(&[r"C:\Users\me\Secret".to_string()]);
        let changes = [
            added(r"C:\$Recycle.Bin", Kind::Folder),
            added(r"C:\System Volume Information", Kind::Folder),
            added(r"C:\Users\me\secret", Kind::Folder),
            added(r"C:\Users\$kept", Kind::Folder),
        ];
        let walks = made(&mut index, &changes, &excluded);
        assert_eq!(walks, [r"C:\Users\$kept"], "a $ name below the root");
        assert!(!there(&index, r"C:\$Recycle.Bin"));
        assert!(!there(&index, r"C:\System Volume Information"));
        assert!(!there(&index, r"C:\Users\me\secret"));
    }

    #[test]
    fn a_name_removed_leaves_with_everything_in_it() {
        let mut index = drive();
        let changes = [
            Change::Removed(r"C:\Users\me\CODE".into()),
            Change::Removed(r"C:\Users\me\gone.txt".into()),
            Change::Removed("C:".into()),
        ];
        made(&mut index, &changes, &Excluded::default());
        assert!(!there(&index, r"C:\Users\me\code\main.rs"));
        assert!(there(&index, r"C:\Users\me\notes.txt"));
        assert!(there(&index, "C:"), "a root stays");
        assert_eq!(index.len(), 4);
    }

    fn moved(from: &str, to: &str, kind: Kind) -> Change {
        Change::Moved(from.to_string(), to.to_string(), kind, Stat::default())
    }

    #[test]
    fn a_folder_moved_takes_what_is_in_it_along() {
        let mut index = drive();
        let change = moved(r"C:\Users\me\code", r"C:\Users\projects", Kind::Folder);
        let walks = made(&mut index, &[change], &Excluded::default());
        assert!(walks.is_empty(), "its entries came along");
        assert!(there(&index, r"C:\Users\projects\main.rs"));
        assert!(!there(&index, r"C:\Users\me\code"));
    }

    #[test]
    fn a_name_moved_out_of_sight_leaves() {
        let mut index = drive();
        let excluded = Excluded::default();
        excluded.set(&[r"C:\Users\hidden".to_string()]);
        let changes = [
            moved(r"C:\Users\me\code", r"C:\$Recycle.Bin", Kind::Folder),
            moved(r"C:\Users\me\notes.txt", r"C:\Users\hidden", Kind::File),
        ];
        made(&mut index, &changes, &excluded);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn a_name_moved_in_from_out_of_sight_comes_and_a_folder_is_walked() {
        let mut index = drive();
        let changes = [
            moved(r"C:\$Recycle.Bin\$R1", r"C:\Users\me\back", Kind::Folder),
            moved(r"C:\Windows\x.txt", r"C:\Users\me\x.txt", Kind::File),
            moved("C:", r"C:\Users\me\root", Kind::Folder),
        ];
        let walks = made(&mut index, &changes, &Excluded::default());
        assert_eq!(walks, [r"C:\Users\me\back", r"C:\Users\me\root"]);
        assert!(there(&index, r"C:\Users\me\x.txt"));
        assert!(there(&index, "C:"), "a root is never moved");
    }

    #[test]
    fn a_name_keeps_the_size_and_time_it_came_or_moved_with_or_was_touched_to() {
        let mut index = drive();
        let stat = |size| Stat {
            size,
            written: 1_794_310_200,
        };
        let changes = [
            Change::Added(r"C:\Users\me\new.txt".into(), Kind::File, stat(1)),
            Change::Touched(r"C:\Users\me\notes.txt".into(), stat(2)),
            Change::Moved(
                r"C:\Users\me\code".into(),
                r"C:\Users\code".into(),
                Kind::Folder,
                stat(3),
            ),
            Change::Touched(r"C:\Users\me\gone.txt".into(), stat(4)),
            Change::Touched("C:".into(), stat(5)),
        ];
        made(&mut index, &changes, &Excluded::default());
        let stat_at = |path: &str| index.find(path).and_then(|entry| index.stat(entry));
        assert_eq!(stat_at(r"C:\Users\me\new.txt"), Some((1, 1_794_310_200)));
        assert_eq!(stat_at(r"C:\Users\me\notes.txt"), Some((2, 1_794_310_200)));
        assert_eq!(stat_at(r"C:\Users\code"), Some((3, 1_794_310_200)));
        assert_eq!(stat_at("C:"), Some((0, 0)), "a root is never touched");
    }

    #[test]
    fn a_folder_is_walked_once_and_not_inside_another_walked() {
        let paths = [r"C:\a\b", r"C:\a", r"C:\A", r"C:\ab", r"C:\a\b\c", r"C:\ab"];
        let paths = paths.map(String::from).to_vec();
        assert_eq!(outermost(paths), [r"C:\a", r"C:\ab"]);
    }
}
