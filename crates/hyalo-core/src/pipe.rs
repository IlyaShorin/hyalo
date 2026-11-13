//! What the launcher and the indexer service say to each other over the
//! service's pipe. The launcher asks once, as it connects: to follow a
//! volume, from a position it saved or from nothing, or for the files of
//! folders that came into the index whole. The service answers in frames
//! until the launcher lets go: each a length, then the answer.
//!
//! Little-endian throughout. A request is its frame's length, the
//! version, its kind and what it asks; the service answers one of another
//! version with only `Outdated`, whose frame never changes. A change is its
//! record, whether it is gone, a file or a folder, then for one there its
//! size, last write and names, each its folder's record and its UTF-8
//! bytes. Everything is checked where it comes in: a frame that does not
//! read whole and exactly is refused.

use crate::volume::{Change, File, Name, Position};

/// Changes with the format of anything said over the pipe.
pub const VERSION: u32 = 1;

/// The longest frame either side reads: a listing comes in many.
pub const MOST: usize = 64 << 20;

/// What the launcher asks the service, once per connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Every change to the volume `drive` (`C`): from `from` on, if the
    /// journal still holds it; else the whole volume, then every change.
    Follow { drive: char, from: Option<Position> },
    /// The files and folders under the folders `records` of the volume
    /// `drive`, whatever they hold.
    Subtree { drive: char, records: Vec<u32> },
}

/// Why a request was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Of another version than the service's.
    Outdated,
    /// Not a request.
    Garbled,
}

/// What the service answers, frame by frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The service speaks this version, not the request's.
    Outdated(u32),
    /// The service does not read this volume: not NTFS, not there, or not
    /// readable.
    Refused,
    /// What follows is the whole volume, up to `Listed`; then what changed
    /// from this position on.
    Listing(Position),
    /// Part of the listing, or of the folders asked for.
    Files(Vec<Change>),
    /// The listing, or the folders asked for, came whole.
    Listed,
    /// What the journal said, read up to this position.
    Changed(Position, Vec<Change>),
}

const FOLLOW: u8 = 0;
const SUBTREE: u8 = 1;

const OUTDATED: u8 = 0;
const REFUSED: u8 = 1;
const LISTING: u8 = 2;
const FILES: u8 = 3;
const LISTED: u8 = 4;
const CHANGED: u8 = 5;

/// What a change says of its file.
const GONE: u8 = 0;
const FILE: u8 = 1;
const FOLDER: u8 = 2;

impl Request {
    /// The request as sent, framed.
    pub fn bytes(&self) -> Vec<u8> {
        let mut body = VERSION.to_le_bytes().to_vec();
        match self {
            Request::Follow { drive, from } => {
                body.extend([FOLLOW, *drive as u8, u8::from(from.is_some())]);
                from.iter().for_each(|from| position(&mut body, *from));
            }
            Request::Subtree { drive, records } => {
                body.extend([SUBTREE, *drive as u8]);
                body.extend((records.len() as u32).to_le_bytes());
                records
                    .iter()
                    .for_each(|record| body.extend(record.to_le_bytes()));
            }
        }
        framed(body)
    }

    /// The request a frame's body holds.
    pub fn read(body: &[u8]) -> Result<Request, Unread> {
        let mut reader = Reader(body);
        if reader.u32().ok_or(Unread::Garbled)? != VERSION {
            return Err(Unread::Outdated);
        }
        let request = reader.request().ok_or(Unread::Garbled)?;
        reader.ended().then_some(request).ok_or(Unread::Garbled)
    }
}

impl Answer {
    /// The answer as sent, framed.
    pub fn bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Answer::Outdated(version) => {
                body.push(OUTDATED);
                body.extend(version.to_le_bytes());
            }
            Answer::Refused => body.push(REFUSED),
            Answer::Listing(at) => {
                body.push(LISTING);
                position(&mut body, *at);
            }
            Answer::Files(found) => {
                body.push(FILES);
                changes(&mut body, found);
            }
            Answer::Listed => body.push(LISTED),
            Answer::Changed(at, found) => {
                body.push(CHANGED);
                position(&mut body, *at);
                changes(&mut body, found);
            }
        }
        framed(body)
    }

    /// The answer a frame's body holds, if it reads whole.
    pub fn read(body: &[u8]) -> Option<Answer> {
        let mut reader = Reader(body);
        let answer = reader.answer()?;
        reader.ended().then_some(answer)
    }
}

/// How long the body is of the frame that starts with `head`, unless it is
/// longer than either side reads.
pub fn length(head: [u8; 4]) -> Option<usize> {
    let length = u32::from_le_bytes(head) as usize;
    (length <= MOST).then_some(length)
}

/// `body`, after its length.
fn framed(body: Vec<u8>) -> Vec<u8> {
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend(body);
    frame
}

fn position(body: &mut Vec<u8>, at: Position) {
    body.extend(at.volume.to_le_bytes());
    body.extend(at.journal.to_le_bytes());
    body.extend(at.next.to_le_bytes());
}

fn changes(body: &mut Vec<u8>, found: &[Change]) {
    body.extend((found.len() as u32).to_le_bytes());
    for change in found {
        body.extend(change.record.to_le_bytes());
        let Some(file) = &change.file else {
            body.push(GONE);
            continue;
        };
        body.push(if file.folder { FOLDER } else { FILE });
        body.extend(file.size.to_le_bytes());
        body.extend(file.written.to_le_bytes());
        body.extend((file.names.len() as u16).to_le_bytes());
        for name in &file.names {
            body.extend(name.parent.to_le_bytes());
            body.extend((name.text.len() as u16).to_le_bytes());
            body.extend(name.text.as_bytes());
        }
    }
}

/// Bytes being read, front first; each read only if they are there.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(taken)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.array()?))
    }

    fn ended(&self) -> bool {
        self.0.is_empty()
    }

    fn request(&mut self) -> Option<Request> {
        let kind = self.u8()?;
        let drive = self.drive()?;
        match kind {
            FOLLOW => self.follow(drive),
            SUBTREE => self.subtree(drive),
            _ => None,
        }
    }

    fn follow(&mut self, drive: char) -> Option<Request> {
        let from = match self.u8()? {
            0 => None,
            1 => Some(self.position()?),
            _ => return None,
        };
        Some(Request::Follow { drive, from })
    }

    fn subtree(&mut self, drive: char) -> Option<Request> {
        let count = self.u32()?;
        let records = (0..count).map(|_| self.u32()).collect::<Option<_>>()?;
        Some(Request::Subtree { drive, records })
    }

    /// A drive's letter, as Windows names a volume: `C`.
    fn drive(&mut self) -> Option<char> {
        let letter = self.u8()?;
        letter.is_ascii_uppercase().then_some(char::from(letter))
    }

    fn answer(&mut self) -> Option<Answer> {
        let kind = self.u8()?;
        match kind {
            REFUSED => Some(Answer::Refused),
            LISTED => Some(Answer::Listed),
            _ => self.answer_with(kind),
        }
    }

    /// An answer of `kind` that says more than its kind.
    fn answer_with(&mut self, kind: u8) -> Option<Answer> {
        match kind {
            OUTDATED => self.u32().map(Answer::Outdated),
            LISTING => self.position().map(Answer::Listing),
            FILES => self.changes().map(Answer::Files),
            CHANGED => {
                let at = self.position()?;
                Some(Answer::Changed(at, self.changes()?))
            }
            _ => None,
        }
    }

    fn position(&mut self) -> Option<Position> {
        Some(Position {
            volume: self.u64()?,
            journal: self.u64()?,
            next: self.i64()?,
        })
    }

    fn changes(&mut self) -> Option<Vec<Change>> {
        let count = self.u32()?;
        (0..count).map(|_| self.change()).collect()
    }

    fn change(&mut self) -> Option<Change> {
        let record = self.u32()?;
        let file = match self.u8()? {
            GONE => None,
            FILE => Some(self.file(false)?),
            FOLDER => Some(self.file(true)?),
            _ => return None,
        };
        Some(Change { record, file })
    }

    /// What a change says of a file or folder there.
    fn file(&mut self, folder: bool) -> Option<File> {
        let (size, written) = (self.u64()?, self.i64()?);
        let count = self.u16()?;
        let names = (0..count).map(|_| self.name()).collect::<Option<_>>()?;
        Some(File {
            folder,
            size,
            written,
            names,
        })
    }

    fn name(&mut self) -> Option<Name> {
        let parent = self.u32()?;
        let length = self.u16()? as usize;
        let text = std::str::from_utf8(self.take(length)?).ok()?;
        Some(Name {
            parent,
            text: text.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSITION: Position = Position {
        volume: 0xFEDC_BA98_7654_3210,
        journal: 42,
        next: 1 << 40,
    };

    /// The body of a frame: what follows its length, which must say it.
    fn body(frame: &[u8]) -> &[u8] {
        let head: [u8; 4] = frame[..4].try_into().unwrap();
        assert_eq!(length(head), Some(frame.len() - 4), "the length says it");
        &frame[4..]
    }

    fn files() -> Vec<Change> {
        vec![
            Change {
                record: 40,
                file: Some(File {
                    folder: true,
                    size: 0,
                    written: 1_794_565_800,
                    names: vec![Name {
                        parent: 5,
                        text: "Документы".to_string(),
                    }],
                }),
            },
            Change {
                record: 70,
                file: Some(File {
                    folder: false,
                    size: u64::MAX,
                    written: -11_644_473_600,
                    names: vec![
                        Name {
                            parent: 40,
                            text: "a.txt".to_string(),
                        },
                        Name {
                            parent: 5,
                            text: "😀 b.txt".to_string(),
                        },
                    ],
                }),
            },
            Change {
                record: u32::MAX - 1,
                file: None,
            },
            Change {
                record: 71,
                file: Some(File::default()),
            },
        ]
    }

    #[test]
    fn every_request_reads_back_the_same() {
        for request in [
            Request::Follow {
                drive: 'C',
                from: None,
            },
            Request::Follow {
                drive: 'Z',
                from: Some(POSITION),
            },
            Request::Subtree {
                drive: 'D',
                records: vec![40, 41, u32::MAX],
            },
            Request::Subtree {
                drive: 'A',
                records: vec![],
            },
        ] {
            assert_eq!(Request::read(body(&request.bytes())), Ok(request));
        }
    }

    #[test]
    fn every_answer_reads_back_the_same() {
        for answer in [
            Answer::Outdated(7),
            Answer::Refused,
            Answer::Listing(POSITION),
            Answer::Files(files()),
            Answer::Files(vec![]),
            Answer::Listed,
            Answer::Changed(POSITION, files()),
        ] {
            assert_eq!(Answer::read(body(&answer.bytes())), Some(answer));
        }
    }

    #[test]
    fn a_request_of_another_version_is_told_apart_from_garble() {
        let mut other = Request::Follow {
            drive: 'C',
            from: None,
        }
        .bytes();
        other[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(Request::read(&other[4..]), Err(Unread::Outdated));
        assert_eq!(Request::read(&[1, 2]), Err(Unread::Garbled));
        assert_eq!(Request::read(&[]), Err(Unread::Garbled));
    }

    #[test]
    fn the_answer_of_another_version_never_changes() {
        assert_eq!(Answer::Outdated(3).bytes(), [5, 0, 0, 0, 0, 3, 0, 0, 0]);
    }

    #[test]
    fn a_drive_is_a_letter() {
        let mut frame = Request::Follow {
            drive: 'C',
            from: None,
        }
        .bytes();
        let drive = 4 + 4 + 1;
        for refused in [b'c', b'@', b'[', b'1', 0, 0xC0] {
            frame[drive] = refused;
            assert_eq!(
                Request::read(&frame[4..]),
                Err(Unread::Garbled),
                "{refused}"
            );
        }
    }

    #[test]
    fn an_answer_cut_short_anywhere_or_with_more_after_is_refused() {
        let answer = Answer::Changed(POSITION, files()).bytes();
        for end in 4..answer.len() {
            assert_eq!(Answer::read(&answer[4..end]), None, "cut at {end}");
        }
        let longer = [&answer[4..], &[0]].concat();
        assert_eq!(Answer::read(&longer), None);
    }

    #[test]
    fn a_request_cut_short_anywhere_or_with_more_after_is_refused() {
        let request = Request::Subtree {
            drive: 'C',
            records: vec![1, 2],
        }
        .bytes();
        for end in 4..request.len() {
            assert_eq!(Request::read(&request[4..end]), Err(Unread::Garbled));
        }
        let longer = [&request[4..], &[0]].concat();
        assert_eq!(Request::read(&longer), Err(Unread::Garbled));
    }

    #[test]
    fn a_name_that_is_not_utf8_or_a_kind_not_known_is_refused() {
        let answer = Answer::Files(files()).bytes();
        let at = answer
            .windows(5)
            .position(|bytes| bytes == b"a.txt")
            .expect("the name");
        let mut broken = answer.clone();
        broken[at] = 0xFF;
        assert_eq!(Answer::read(&broken[4..]), None);
        let mut unknown = answer.clone();
        unknown[4] = 99;
        assert_eq!(Answer::read(&unknown[4..]), None);
        let gone = answer
            .windows(5)
            .position(|bytes| bytes == [0xFE, 0xFF, 0xFF, 0xFF, 0])
            .expect("the record gone");
        let mut state = answer.clone();
        state[gone + 4] = 3;
        assert_eq!(
            Answer::read(&state[4..]),
            None,
            "neither gone, file nor folder"
        );
    }

    #[test]
    fn a_frame_longer_than_either_side_reads_is_refused() {
        assert_eq!(length((MOST as u32).to_le_bytes()), Some(MOST));
        assert_eq!(length((MOST as u32 + 1).to_le_bytes()), None);
        assert_eq!(length([0; 4]), Some(0));
    }

    #[test]
    fn a_request_of_a_kind_not_known_is_refused() {
        let mut frame = Request::Follow {
            drive: 'C',
            from: None,
        }
        .bytes();
        frame[8] = 7;
        assert_eq!(Request::read(&frame[4..]), Err(Unread::Garbled));
        let mut from = Request::Follow {
            drive: 'C',
            from: None,
        }
        .bytes();
        from[10] = 2;
        assert_eq!(
            Request::read(&from[4..]),
            Err(Unread::Garbled),
            "neither from nothing nor from a position"
        );
    }
}
