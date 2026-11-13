//! What the indexer service says of a volume's files: each file by the
//! number of its record in the volume's master file table, what it is now,
//! and how far the volume's journal of changes was read. Shared by the
//! service, which reads the volume, and the launcher's index, which takes
//! it in.

/// The record of a volume's root folder, the folder its top-level files
/// and folders are in.
pub const ROOT: u32 = 5;

/// What a file is now, by its record's number: gone, or there with all
/// its names. A volume's listing is the changes from nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub record: u32,
    pub file: Option<File>,
}

/// What the index keeps of a file or folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct File {
    pub folder: bool,
    /// Bytes; none for a folder.
    pub size: u64,
    /// Last written, in seconds since 1970.
    pub written: i64,
    /// One for each hard link; a folder has one.
    pub names: Vec<Name>,
}

/// One of a file's names: the folder it is in, by record number, and the
/// name there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub parent: u32,
    pub text: String,
}

/// A volume's journal, as NTFS says it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    /// Which journal: one made again has another.
    pub id: u64,
    /// The first place still kept: older records were let go.
    pub lowest: i64,
    /// Where the next record will be written.
    pub next: i64,
}

/// How far a volume's changes were taken in: the volume, by serial
/// number, its journal, and the place in it to read on from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub volume: u64,
    pub journal: u64,
    pub next: i64,
}

impl Position {
    /// Whether the volume `volume`, whose journal is `journal` now, can be
    /// read on from here: the same volume and journal, and nothing between
    /// here and now let go.
    pub fn follows(self, volume: u64, journal: Journal) -> bool {
        let kept = journal.lowest..=journal.next;
        self.volume == volume && self.journal == journal.id && kept.contains(&self.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
