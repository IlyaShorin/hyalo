//! Roots the indexer service reads: a volume's files come from its master
//! file table, by record number, and its changes from its journal, so
//! such a root is neither walked nor watched. A root the service does not
//! read, or no longer does, is walked and watched as ever.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hyalo_core::pipe::{Answer, Request};
use hyalo_core::volume::{Change, Position, ROOT};
use hyalo_index::{EntryId, Index};
use hyalo_indexer::client::Connection;
use hyalo_indexer::manager::Manager;
use hyalo_indexer::scm::{self, Services};

/// Where the volumes come from: the service, or nowhere.
pub trait Indexer: Send + Sync {
    /// Follows the volume whose root is `root`, from `from`, the position
    /// the index holds of it; refused if the service does not read it.
    fn follow(&self, root: &Path, from: Option<Position>) -> io::Result<Box<dyn Stream>>;

    /// What the folders `records` of the volume whose root is `root` hold.
    fn subtree(&self, root: &Path, records: &[u32]) -> io::Result<Vec<Change>>;
}

/// What the service says of a volume, answer by answer.
pub trait Stream: Send {
    /// The next answer; none once `stop` is set.
    fn next(&mut self, stop: &AtomicBool) -> io::Result<Option<Answer>>;
}

impl Stream for Connection {
    fn next(&mut self, stop: &AtomicBool) -> io::Result<Option<Answer>> {
        Connection::next(self, stop)
    }
}

/// No service: every root is walked.
#[cfg(test)]
pub struct Walked;

#[cfg(test)]
impl Indexer for Walked {
    fn follow(&self, _: &Path, _: Option<Position>) -> io::Result<Box<dyn Stream>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn subtree(&self, _: &Path, _: &[u32]) -> io::Result<Vec<Change>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// The drive a root is: `C` for `C:\`; none for any other folder, which
/// the service does not read.
pub fn drive_of(root: &Path) -> Option<char> {
    let text = root.to_str()?;
    let mut letters = text.strip_suffix('\\').unwrap_or(text).chars();
    match (letters.next(), letters.next(), letters.next()) {
        (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic() => {
            Some(letter.to_ascii_uppercase())
        }
        _ => None,
    }
}

/// The change, but for the names a walk leaves out at a volume's root:
/// the system's own (`$Recycle.Bin`, `System Volume Information`).
pub fn kept(change: &Change) -> Change {
    let mut kept = change.clone();
    if let Some(file) = &mut kept.file {
        file.names
            .retain(|name| super::kept(&name.text, name.parent == ROOT));
    }
    kept
}

/// Makes the volume whose root is `root` hold what `changes` say, the
/// names a walk leaves out at its root left out, and notes `position`:
/// the records of the folders that came in, whose files must be asked
/// for.
pub fn settle(
    index: &mut Index,
    root: EntryId,
    changes: &[Change],
    position: Position,
) -> Vec<u32> {
    let mut came = Vec::new();
    for change in changes {
        if index.settle(root, &kept(change)) == Ok(true) {
            came.push(change.record);
        }
    }
    // The root is the index's own: it is there.
    let _ = index.set_position(root, position);
    came
}

/// A volume's listing, put together apart from the index the window
/// searches, then put in place of what it held of the volume.
pub struct Listing {
    index: Index,
    root: EntryId,
    position: Position,
}

impl Listing {
    /// A listing of the root named `name` (`C:`), the journal to be read
    /// on from `position`.
    pub fn new(name: &str, position: Position) -> Option<Listing> {
        let mut index = Index::default();
        let root = index.add_root(name).ok()?;
        Some(Listing {
            index,
            root,
            position,
        })
    }

    pub fn take(&mut self, changes: &[Change]) {
        settle(&mut self.index, self.root, changes, self.position);
    }

    /// The volume, whole, with no room kept for growth.
    pub fn done(mut self) -> Index {
        self.index.shrink_to_fit();
        self.index
    }
}

/// A served root's answers, read on a thread of their own and sent on,
/// each with the root's place among the roots, until `stop` is set or
/// nobody listens. An error is sent too, and is the last.
pub struct Following {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Following {
    pub fn start<T: Send + 'static>(
        mut stream: Box<dyn Stream>,
        root: usize,
        send: Sender<(usize, T)>,
        wrap: fn(io::Result<Answer>) -> T,
    ) -> Following {
        let stop = Arc::new(AtomicBool::new(false));
        let halt = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            loop {
                let next = stream.next(&halt);
                let (last, heard) = match next {
                    Ok(Some(answer)) => (false, Ok(answer)),
                    Ok(None) => return,
                    Err(error) => (true, Err(error)),
                };
                if send.send((root, wrap(heard))).is_err() || last {
                    return;
                }
            }
        });
        Following {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Following {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How long the service may take to listen once started.
const STARTING: Duration = Duration::from_secs(10);

/// The service installed on this machine, started if it is not running.
/// Not tested: it starts the real service; what it reads is tested with
/// `hyalo_indexer`'s own server, and how the files provider takes it in
/// with a stand-in.
pub struct Service;

impl Service {
    /// A connection to the service, asking `request`: started first if it
    /// is installed but stopped, its pipe waited for while it starts.
    fn ask(&self, request: &Request) -> io::Result<Connection> {
        let process = running()?;
        let until = Instant::now() + STARTING;
        loop {
            match Connection::ask(hyalo_indexer::PIPE, Some(process), request) {
                Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < until => {
                    thread::sleep(Duration::from_millis(50));
                }
                asked => return asked,
            }
        }
    }
}

/// The process of the service, started if it is not running.
fn running() -> io::Result<u32> {
    let state = Manager.state(scm::NAME)?.ok_or(io::ErrorKind::NotFound)?;
    if state.running {
        return Ok(state.process);
    }
    Manager.start(scm::NAME)?;
    let state = Manager.state(scm::NAME)?.ok_or(io::ErrorKind::NotFound)?;
    Ok(state.process)
}

impl Indexer for Service {
    fn follow(&self, root: &Path, from: Option<Position>) -> io::Result<Box<dyn Stream>> {
        let drive = drive_of(root).ok_or(io::ErrorKind::Unsupported)?;
        let asked = self.ask(&Request::Follow { drive, from })?;
        Ok(Box::new(asked))
    }

    fn subtree(&self, root: &Path, records: &[u32]) -> io::Result<Vec<Change>> {
        let drive = drive_of(root).ok_or(io::ErrorKind::Unsupported)?;
        let records = records.to_vec();
        let mut asked = self.ask(&Request::Subtree { drive, records })?;
        gathered(&mut asked)
    }
}

/// The files a stream sends until it says they came whole.
pub fn gathered(stream: &mut dyn Stream) -> io::Result<Vec<Change>> {
    let mut files = Vec::new();
    let going = AtomicBool::new(false);
    loop {
        match stream.next(&going)? {
            Some(Answer::Files(more)) => files.extend(more),
            Some(Answer::Listed) => return Ok(files),
            _ => return Err(io::ErrorKind::InvalidData.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::volume::{File, Name};
    use std::sync::mpsc;

    fn change(record: u32, folder: bool, names: &[(u32, &str)]) -> Change {
        let names = names.iter().map(|&(parent, text)| Name {
            parent,
            text: text.to_string(),
        });
        Change {
            record,
            file: Some(File {
                folder,
                size: 3,
                written: 1_700_000_000,
                names: names.collect(),
            }),
        }
    }

    const AT: Position = Position {
        volume: 1,
        journal: 2,
        next: 3,
    };

    #[test]
    fn a_drive_s_root_is_its_letter_and_any_other_folder_none() {
        assert_eq!(drive_of(Path::new(r"C:\")), Some('C'));
        assert_eq!(drive_of(Path::new(r"d:\")), Some('D'));
        assert_eq!(drive_of(Path::new("E:")), Some('E'));
        for not in [r"C:\Temp", r"\\server\share\", "", r"CC:\", r"1:\", r"C:/"] {
            assert_eq!(drive_of(Path::new(not)), None, "{not}");
        }
    }

    #[test]
    fn the_system_s_own_names_at_the_root_are_left_out() {
        let names = [
            (ROOT, "$Recycle.Bin"),
            (ROOT, "System Volume Information"),
            (ROOT, "kept"),
            (40, "$kept below"),
        ];
        let kept = kept(&change(50, true, &names));
        let left: Vec<&str> = kept
            .file
            .iter()
            .flat_map(|file| &file.names)
            .map(|name| name.text.as_str())
            .collect();
        assert_eq!(left, ["kept", "$kept below"]);
        let gone = Change {
            record: 51,
            file: None,
        };
        assert_eq!(super::kept(&gone), gone);
    }

    #[test]
    fn changes_settle_into_the_volume_and_folders_that_came_in_are_told() {
        let mut index = Index::default();
        let c = index.add_root("C:").unwrap();
        let came = settle(
            &mut index,
            c,
            &[
                change(40, true, &[(ROOT, "Users")]),
                change(41, false, &[(40, "a.txt")]),
                change(42, true, &[(ROOT, "$Recycle.Bin")]),
                change(43, false, &[(42, "binned.txt")]),
            ],
            AT,
        );
        assert_eq!(came, [40]);
        assert!(index.find(r"C:\Users\a.txt").is_some());
        assert_eq!(index.find(r"C:\$Recycle.Bin"), None);
        assert_eq!(index.position(c), Some(AT));
        let again = settle(&mut index, c, &[change(41, false, &[(40, "a.txt")])], AT);
        assert_eq!(again, Vec::<u32>::new());
    }

    #[test]
    fn a_listing_is_put_together_apart_then_whole() {
        let mut listing = Listing::new("D:", AT).expect("a root");
        listing.take(&[change(40, true, &[(ROOT, "Data")])]);
        listing.take(&[change(41, false, &[(40, "notes.txt")])]);
        let index = listing.done();
        let d = index.find("D:").expect("the root");
        assert!(index.find(r"D:\Data\notes.txt").is_some());
        assert_eq!(index.position(d), Some(AT));
        assert!(Listing::new("", AT).is_none(), "no name");
    }

    /// A stream that gives `answers`, then fails.
    struct Given(Vec<Answer>);

    impl Stream for Given {
        fn next(&mut self, _: &AtomicBool) -> io::Result<Option<Answer>> {
            match self.0.is_empty() {
                true => Err(io::ErrorKind::BrokenPipe.into()),
                false => Ok(Some(self.0.remove(0))),
            }
        }
    }

    #[test]
    fn the_files_asked_for_are_gathered_until_they_came_whole() {
        let one = change(40, true, &[(ROOT, "a")]);
        let two = change(41, false, &[(40, "b")]);
        let mut stream = Given(vec![
            Answer::Files(vec![one.clone()]),
            Answer::Files(vec![two.clone()]),
            Answer::Listed,
        ]);
        assert_eq!(gathered(&mut stream).expect("gathered"), [one, two]);
        let mut refused = Given(vec![Answer::Refused]);
        assert!(gathered(&mut refused).is_err());
        let mut cut = Given(vec![Answer::Files(vec![])]);
        assert!(gathered(&mut cut).is_err());
    }

    #[test]
    fn a_followed_root_s_answers_are_sent_on_with_its_place_until_one_fails() {
        let (send, heard) = mpsc::channel();
        let stream = Box::new(Given(vec![Answer::Listing(AT), Answer::Listed]));
        let following =
            Following::start(stream, 3, send, |heard| heard.map_err(|error| error.kind()));
        let got: Vec<_> = heard.iter().take(3).collect();
        assert_eq!(
            got,
            [
                (3, Ok(Answer::Listing(AT))),
                (3, Ok(Answer::Listed)),
                (3, Err(io::ErrorKind::BrokenPipe))
            ]
        );
        drop(following);
    }
}
