//! What the service reads of a volume for the launcher: the whole volume
//! from its master file table, a file as it is now from its record, the
//! files under folders, and what the journal says changed. Over a `Disk`,
//! so all of it is tested on a volume in memory.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::atomic::AtomicBool;
use std::thread;

use hyalo_core::pipe::Answer;
use hyalo_core::volume::{Change, Journal, Position};

use crate::disk::{Disk, Geometry};
use crate::listing::Listing;
use crate::mft::{self, Piece, Table};
use crate::usn;

/// Where answers go: to the launcher, until it lets go.
pub type Out<'a> = &'a mut dyn FnMut(Answer) -> io::Result<()>;

/// Bytes of the table read at once: enough for the disk to stream, few
/// enough to share among threads.
const PIECE: u64 = 4 << 20;

/// The most threads reading one table.
const READERS: usize = 8;

/// The most files sent in one frame: a megabyte or so.
const BATCH: usize = 16_384;

fn unreadable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the master file table does not read",
    )
}

/// Every file and folder of the volume, read from its master file table.
pub fn list(disk: &dyn Disk) -> io::Result<Listing> {
    let geometry = disk.geometry()?;
    let table = table(disk, &geometry)?;
    let pieces = table.pieces(geometry.valid, PIECE);
    let readers = thread::available_parallelism().map_or(1, |count| count.get().min(READERS));
    let mut shares = vec![Vec::new(); readers];
    for (at, piece) in pieces.into_iter().enumerate() {
        shares[at % readers].push(piece);
    }
    thread::scope(|scope| {
        let reading: Vec<_> = shares
            .iter()
            .map(|share| scope.spawn(|| read_pieces(disk, &geometry, &table, share)))
            .collect();
        let mut listing = Listing::default();
        for reader in reading {
            listing.join(reader.join().expect("a reader does not panic")?);
        }
        Ok(listing)
    })
}

/// Where the table lies: what its own record says, and the records its
/// attribute list names for the stretches it had no room for.
fn table(disk: &dyn Disk, geometry: &Geometry) -> io::Result<Table> {
    let own = raw_record(disk, geometry, &[geometry.table * geometry.cluster])?;
    let mut segments = vec![mft::data_runs(&own).ok_or_else(unreadable)?];
    let (cluster, record) = (geometry.cluster, geometry.record);
    let start = Table::new(cluster, record, segments.clone()).ok_or_else(unreadable)?;
    let elsewhere = mft::parse(&own).ok_or_else(unreadable)?.elsewhere;
    for extension in elsewhere.into_iter().filter(|&number| number != 0) {
        let spans = start.spans(u64::from(extension) * record, record);
        let spans = spans.ok_or_else(unreadable)?;
        let places: Vec<u64> = spans.iter().map(|span| span.at).collect();
        let more = raw_record(disk, geometry, &places)?;
        segments.extend(mft::data_runs(&more).filter(|&(first, _)| first != 0));
    }
    Table::new(cluster, record, segments).ok_or_else(unreadable)
}

/// A record of the table read from the disk, at `places` (one, unless it
/// lies across two stretches), with its sectors' ends put back.
fn raw_record(disk: &dyn Disk, geometry: &Geometry, places: &[u64]) -> io::Result<Vec<u8>> {
    let share = geometry.record as usize / places.len();
    let mut bytes = Vec::new();
    for &at in places {
        bytes.extend(disk.read(at, share)?);
    }
    match mft::fix(&mut bytes) {
        true => Ok(bytes),
        false => Err(unreadable()),
    }
}

/// The records in `pieces` of the table, those that read whole.
fn read_pieces(
    disk: &dyn Disk,
    geometry: &Geometry,
    table: &Table,
    pieces: &[Piece],
) -> io::Result<Listing> {
    let mut listing = Listing::default();
    for &piece in pieces {
        let mut bytes = Vec::with_capacity(piece.length as usize);
        for span in table
            .spans(piece.start, piece.length)
            .ok_or_else(unreadable)?
        {
            bytes.extend(disk.read(span.at, span.length as usize)?);
        }
        let first = piece.first(geometry.record);
        for (at, record) in bytes.chunks_exact_mut(geometry.record as usize).enumerate() {
            if mft::fix(record)
                && let Some(read) = mft::parse(record)
            {
                listing.take(first + at as u32, read);
            }
        }
    }
    Ok(listing)
}

/// What the file of the record `number` is now; none for an extension
/// record, which is part of another file.
pub fn state(disk: &dyn Disk, number: u32) -> io::Result<Option<Change>> {
    let gone = Change {
        record: number,
        file: None,
    };
    let Some(mut read) = disk.record(number)?.and_then(|bytes| mft::parse(&bytes)) else {
        return Ok(Some(gone));
    };
    if read.base.is_some() {
        return Ok(None);
    }
    for extension in read.elsewhere.clone() {
        let more = disk.record(extension)?.and_then(|bytes| mft::parse(&bytes));
        if let Some(more) = more.filter(|more| more.base == Some(number)) {
            read.merge(more);
        }
    }
    Ok(Some(Change {
        record: number,
        file: Some(read.file()),
    }))
}

/// Sends the launcher every change to the volume until `stop` is set: from
/// `from` on, if the journal still holds it; else the whole volume first,
/// from where the journal was before it was read.
pub fn follow(
    disk: &dyn Disk,
    from: Option<Position>,
    out: Out,
    stop: &AtomicBool,
) -> io::Result<()> {
    let volume = disk.geometry()?.serial;
    let journal = disk.journal()?;
    let at = match from.filter(|from| from.follows(volume, journal)) {
        Some(from) => from,
        None => send_listing(disk, volume, journal, out)?,
    };
    send_changes(disk, at, out, stop)
}

/// Sends what the journal says changed from `at` on, until `stop` is set.
fn send_changes(disk: &dyn Disk, mut at: Position, out: Out, stop: &AtomicBool) -> io::Result<()> {
    while let Some(read) = disk.changes(at.next, at.journal, stop)? {
        let (next, records) = usn::changed(&read).ok_or_else(unreadable)?;
        let changes = states(disk, &records)?;
        at = Position { next, ..at };
        out(Answer::Changed(at, changes))?;
    }
    Ok(())
}

/// Sends the whole volume, read once what NTFS holds in memory is on the
/// disk; the position the journal is to be read on from.
fn send_listing(disk: &dyn Disk, volume: u64, journal: Journal, out: Out) -> io::Result<Position> {
    let at = Position {
        volume,
        journal: journal.id,
        next: journal.next,
    };
    out(Answer::Listing(at))?;
    disk.flush()?;
    send_files(list(disk)?.changes(), out)?;
    Ok(at)
}

/// What the files of `records` are now, but for extension records.
fn states(disk: &dyn Disk, records: &[u32]) -> io::Result<Vec<Change>> {
    let mut changes = Vec::new();
    for &record in records {
        changes.extend(state(disk, record)?);
    }
    Ok(changes)
}

/// Sends `changes` in frames of at most a batch, then that they came
/// whole.
fn send_files(changes: impl Iterator<Item = Change>, out: Out) -> io::Result<()> {
    let mut batch = Vec::new();
    for change in changes {
        batch.push(change);
        if batch.len() == BATCH {
            out(Answer::Files(std::mem::take(&mut batch)))?;
        }
    }
    if !batch.is_empty() {
        out(Answer::Files(batch))?;
    }
    out(Answer::Listed)
}

/// Sends the launcher what the folders `folders` hold, at any depth:
/// folders first, each after the one it is in, then the files; then that
/// they came whole.
pub fn subtree(disk: &dyn Disk, folders: &[u32], out: Out) -> io::Result<()> {
    let mut waiting: VecDeque<u32> = folders.iter().copied().collect();
    let mut seen: HashSet<u32> = folders.iter().copied().collect();
    let (mut found, mut files) = (Vec::new(), Vec::new());
    while let Some(folder) = waiting.pop_front() {
        // A folder gone meanwhile holds nothing: the journal tells of it.
        let children = disk.children(folder).unwrap_or_default();
        for child in children.into_iter().filter(|&child| seen.insert(child)) {
            let Some(change) = state(disk, child)? else {
                continue;
            };
            match change.file.as_ref().is_some_and(|file| file.folder) {
                true => {
                    waiting.push_back(child);
                    found.push(change);
                }
                false => files.push(change),
            }
        }
    }
    send_files(found.into_iter().chain(files), out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    use hyalo_core::pipe::Answer;
    use hyalo_core::volume::{File, Position};

    use super::*;
    use crate::disk::image::{Image, RECORDS, SERIAL};
    use crate::mft::build::{Made, name, record, runs_bytes};

    /// 13.11.2026 10:30 UTC, as Windows counts and in seconds since 1970.
    const WRITTEN: i64 = 134_390_394_000_000_000;
    const SECONDS: i64 = 1_794_565_800;

    const WIN32: u8 = 1;

    fn file(parent: u32, text: &str, size: u64) -> Made {
        record()
            .written(WRITTEN)
            .name(parent, text, WIN32)
            .data(0, size, &runs_bytes(&[(390, 1)]))
    }

    fn folder(parent: u32, text: &str) -> Made {
        record().folder().written(WRITTEN).name(parent, text, WIN32)
    }

    /// Users\Me\notes.txt, a file with a hard link, a free record, a file
    /// whose third name is in an extension record, and one in the table's
    /// second stretch.
    fn volume() -> Image {
        Image::new(&[
            (16, folder(5, "Users")),
            (17, folder(16, "Me")),
            (18, file(17, "notes.txt", 1234)),
            (19, file(5, "a.txt", 7).name(16, "b.txt", WIN32)),
            (20, record().free().name(5, "removed.txt", WIN32)),
            (21, record().extending(22).name(16, "third.txt", WIN32)),
            (
                22,
                file(5, "first.txt", 9).list(&[(0x30, 0, 22), (0x30, 0, 21)]),
            ),
            (40, file(5, "far.txt", 1)),
        ])
    }

    fn there(record: u32, folder: bool, size: u64, names: &[(u32, &str)]) -> Change {
        Change {
            record,
            file: Some(File {
                folder,
                size,
                written: SECONDS,
                names: names
                    .iter()
                    .map(|&(parent, text)| name(parent, text))
                    .collect(),
            }),
        }
    }

    fn gone(record: u32) -> Change {
        Change { record, file: None }
    }

    fn listed(image: &Image) -> Vec<Change> {
        list(image).expect("listed").changes().collect()
    }

    fn records(changes: &[Change]) -> Vec<u32> {
        changes.iter().map(|change| change.record).collect()
    }

    #[test]
    fn a_volume_is_listed_from_its_table_in_both_its_stretches() {
        let changes = listed(&volume());
        assert_eq!(records(&changes), [16, 17, 0, 18, 19, 22, 40]);
        assert_eq!(changes[1], there(17, true, 0, &[(16, "Me")]));
        assert_eq!(changes[3], there(18, false, 1234, &[(17, "notes.txt")]));
        assert_eq!(
            changes[4],
            there(19, false, 7, &[(5, "a.txt"), (16, "b.txt")])
        );
        assert_eq!(
            changes[5],
            there(22, false, 9, &[(5, "first.txt"), (16, "third.txt")])
        );
        assert_eq!(changes[6], there(40, false, 1, &[(5, "far.txt")]));
    }

    #[test]
    fn a_record_torn_on_disk_is_left_out_of_the_listing() {
        let image = volume();
        let end = Image::place(18) + 510;
        image.scribble(end, &[0xEE, 0xEE]);
        assert_eq!(records(&listed(&image)), [16, 17, 0, 19, 22, 40]);
    }

    #[test]
    fn the_table_s_own_record_may_say_its_further_stretches_lie_elsewhere() {
        let length = u64::from(RECORDS) * 1024;
        let own = record()
            .name(5, "$MFT", 3)
            .data(0, length, &runs_bytes(&[(100, 8)]))
            .list(&[(0x80, 0, 0), (0x80, 8, 1)]);
        let more = record().extending(0).data(8, 0, &runs_bytes(&[(300, 8)]));
        let image = Image::new(&[(0, own), (1, more), (40, file(5, "far.txt", 1))]);
        assert_eq!(records(&listed(&image)), [0, 40]);
    }

    #[test]
    fn a_table_whose_own_record_does_not_read_is_not_listed() {
        let image = volume();
        image.scribble(Image::place(0), b"BAAD");
        assert!(list(&image).is_err());
        let unmapped = volume();
        let own = record().name(5, "$MFT", 3).small_data(4, "");
        unmapped.scribble(Image::place(0), &own.on_disk(1024));
        assert!(list(&unmapped).is_err(), "no stretches");
    }

    #[test]
    fn a_file_is_read_as_it_is_now_not_as_the_disk_holds_it() {
        let image = volume();
        image.change(18, Some(file(16, "moved.txt", 99)));
        assert_eq!(
            state(&image, 18).unwrap(),
            Some(there(18, false, 99, &[(16, "moved.txt")]))
        );
    }

    #[test]
    fn a_file_s_extension_records_are_read_with_it_and_are_none_alone() {
        let image = volume();
        assert_eq!(
            state(&image, 22).unwrap(),
            Some(there(22, false, 9, &[(5, "first.txt"), (16, "third.txt")]))
        );
        assert_eq!(state(&image, 21).unwrap(), None);
    }

    #[test]
    fn a_record_not_in_use_or_not_a_record_is_gone() {
        let image = volume();
        assert_eq!(state(&image, 20).unwrap(), Some(gone(20)));
        assert_eq!(state(&image, 63).unwrap(), Some(gone(63)));
        image.change(18, Some(record().free()));
        assert_eq!(state(&image, 18).unwrap(), Some(gone(18)));
    }

    /// What `follow` sends, as it sends it, from a thread of its own; the
    /// flag that stops it, and how it ended.
    struct Following {
        answers: Receiver<Answer>,
        stop: Arc<AtomicBool>,
        thread: thread::JoinHandle<io::Result<()>>,
    }

    impl Following {
        fn start(image: &Arc<Image>, from: Option<Position>) -> Following {
            let (send, answers) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let (image, halt) = (Arc::clone(image), Arc::clone(&stop));
            let thread = thread::spawn(move || {
                let mut out = |answer| {
                    send.send(answer)
                        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
                };
                follow(&*image, from, &mut out, &halt)
            });
            Following {
                answers,
                stop,
                thread,
            }
        }

        fn next(&self) -> Answer {
            self.answers
                .recv_timeout(Duration::from_secs(10))
                .expect("an answer")
        }

        /// The listing's files, up to `Listed`.
        fn files(&self) -> Vec<Change> {
            let mut files = Vec::new();
            loop {
                match self.next() {
                    Answer::Files(more) => files.extend(more),
                    Answer::Listed => return files,
                    other => panic!("{other:?} in a listing"),
                }
            }
        }

        fn end(self) -> io::Result<()> {
            self.stop.store(true, Ordering::SeqCst);
            self.thread.join().expect("it ends")
        }
    }

    fn at(next: i64) -> Position {
        Position {
            volume: SERIAL,
            journal: 1,
            next,
        }
    }

    #[test]
    fn following_from_nothing_lists_the_volume_then_sends_what_changed() {
        let image = Arc::new(volume());
        // Changed before, but not yet written to the disk.
        image.change(30, Some(file(5, "late.txt", 3)));
        let following = Following::start(&image, None);
        assert_eq!(following.next(), Answer::Listing(at(96)));
        let files = following.files();
        assert!(files.contains(&there(30, false, 3, &[(5, "late.txt")])));
        assert_eq!(files.len(), 8);
        image.change(18, Some(file(17, "renamed.txt", 5)));
        assert_eq!(
            following.next(),
            Answer::Changed(at(192), vec![there(18, false, 5, &[(17, "renamed.txt")])])
        );
        image.change(19, None);
        image.change(21, None);
        // One read of the journal or two, as it happens.
        let mut changes = Vec::new();
        let mut last = at(192);
        while last != at(384) {
            let Answer::Changed(read_to, more) = following.next() else {
                panic!("only changes after the listing");
            };
            (last, changes) = (read_to, [changes, more].concat());
        }
        // An extension record removed reads as any record gone.
        assert_eq!(changes, [gone(19), gone(21)]);
        assert!(following.end().is_ok());
    }

    #[test]
    fn following_from_where_the_journal_still_is_lists_nothing() {
        let image = Arc::new(volume());
        image.change(30, Some(file(5, "seen.txt", 3)));
        image.change(31, Some(file(5, "unseen.txt", 3)));
        let following = Following::start(&image, Some(at(96)));
        assert_eq!(
            following.next(),
            Answer::Changed(at(192), vec![there(31, false, 3, &[(5, "unseen.txt")])])
        );
        assert!(following.end().is_ok());
        assert_eq!(image.flushes.load(Ordering::SeqCst), 0, "no listing read");
    }

    #[test]
    fn a_place_the_journal_let_go_of_or_another_volume_s_lists_again() {
        let image = Arc::new(volume());
        image.change(30, Some(file(5, "a.txt", 3)));
        image.change(31, Some(file(5, "b.txt", 3)));
        image.let_go(96);
        for from in [
            at(0),
            Position {
                volume: SERIAL + 1,
                ..at(96)
            },
            Position {
                journal: 2,
                ..at(96)
            },
        ] {
            let following = Following::start(&image, Some(from));
            assert_eq!(following.next(), Answer::Listing(at(192)), "{from:?}");
            assert!(following.end().is_ok());
        }
    }

    #[test]
    fn following_ends_when_the_launcher_lets_go() {
        let image = Arc::new(volume());
        let following = Following::start(&image, Some(at(0)));
        drop(following.answers);
        image.change(18, None);
        assert!(following.thread.join().expect("it ends").is_err());
    }

    #[test]
    fn a_journal_made_again_while_followed_ends_the_following() {
        let image = Arc::new(volume());
        let following = Following::start(&image, None);
        assert_eq!(following.next(), Answer::Listing(at(0)));
        following.files();
        image.renew_journal();
        image.change(18, None);
        assert!(following.thread.join().expect("it ends").is_err());
    }

    #[test]
    fn the_files_under_folders_come_folders_first_each_once() {
        let image = volume();
        image.hold(16, &[17, 19]);
        image.hold(17, &[18, 19, 20]);
        let mut answers = Vec::new();
        let mut out = |answer| {
            answers.push(answer);
            Ok(())
        };
        subtree(&image, &[16], &mut out).expect("sent");
        let Answer::Files(files) = &answers[0] else {
            panic!("{answers:?}");
        };
        assert_eq!(records(files), [17, 19, 18, 20]);
        assert_eq!(files[3], gone(20), "gone since it was listed");
        assert_eq!(answers[1..], [Answer::Listed]);
    }
}
