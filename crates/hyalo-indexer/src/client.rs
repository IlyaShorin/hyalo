//! The launcher's side of the service's pipe: one request per connection,
//! then the answers as they come. The launcher talks only to the service:
//! the process at the other end must be the one Windows runs as it, and
//! may only tell who the launcher is, never act as it.

use std::io;
use std::sync::atomic::AtomicBool;

use hyalo_core::pipe::{Answer, Request};
use windows::Win32::Foundation::ERROR_PIPE_BUSY;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::core::HSTRING;

use crate::overlapped::{Owned, os};
use crate::security::CLIENT_ACCESS;
use crate::wire::End;

/// How long to wait for an instance of the pipe when all are taken.
const BUSY_WAIT: u32 = 2_000;

/// A connection to the service, the request sent.
pub struct Connection {
    end: End,
}

impl Connection {
    /// Connects to the pipe `name` and sends `request`; refused unless
    /// the process at the other end is `server`, when one is named.
    pub fn ask(name: &str, server: Option<u32>, request: &Request) -> io::Result<Connection> {
        Connection::send(name, server, &request.bytes())
    }

    /// Connects and sends `bytes`, whatever they are.
    pub(crate) fn send(name: &str, server: Option<u32>, bytes: &[u8]) -> io::Result<Connection> {
        let name = HSTRING::from(name);
        let end = match open(&name) {
            Err(error) if error.code() == ERROR_PIPE_BUSY.to_hresult() => {
                // SAFETY: a plain call with a name that outlives it.
                let _ = unsafe { WaitNamedPipeW(&name, BUSY_WAIT) };
                open(&name)
            }
            other => other,
        };
        let end = End(Owned(end.map_err(os)?));
        if let Some(server) = server {
            let mut serving = 0;
            // SAFETY: a plain call on a pipe that is ours.
            unsafe { GetNamedPipeServerProcessId(end.0.0, &mut serving).map_err(os)? };
            if serving != server {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        end.write(bytes, None)?;
        Ok(Connection { end })
    }

    /// The next answer; none if `stop` was set first.
    pub fn next(&self, stop: &AtomicBool) -> io::Result<Option<Answer>> {
        let Some(body) = self.end.frame(stop)? else {
            return Ok(None);
        };
        let answer = Answer::read(&body).ok_or(io::ErrorKind::InvalidData)?;
        Ok(Some(answer))
    }
}

/// Opens the pipe `name` for what the launcher does with it, letting the
/// other end only tell who the launcher is.
fn open(name: &HSTRING) -> windows::core::Result<windows::Win32::Foundation::HANDLE> {
    let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    // SAFETY: a plain call; the handle it gives is closed by `Owned`.
    unsafe {
        CreateFileW(
            name,
            CLIENT_ACCESS,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::Disk;
    use crate::disk::image::{Image, SERIAL};
    use crate::mft::build::record;
    use crate::security;
    use crate::server;
    use hyalo_core::pipe::VERSION;
    use hyalo_core::volume::{Change, Position};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    /// A pipe name of the test's own.
    fn name(test: &str) -> String {
        format!(r"\\.\pipe\hyalo-test-{}-{test}", std::process::id())
    }

    /// The service on the pipe `name`, serving a volume in memory as `C`
    /// to this process's user, on a thread of its own until stopped or
    /// unused for `idle`.
    struct Serving {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<io::Result<()>>>,
    }

    /// The pipe's security for a service that runs as this process's user,
    /// as the tests' does: it may make the pipe's instances, as the system
    /// may, and use it, as the launcher's user may.
    fn served_by_me() -> String {
        let me = security::current_user().expect("a user");
        format!(
            "{}(A;;GA;;;{me})",
            security::pipe_sddl(std::slice::from_ref(&me))
        )
    }

    impl Serving {
        fn start(name: &str, image: &Arc<Image>, idle: Duration) -> Serving {
            Serving::secured(name, image, idle, served_by_me())
        }

        fn secured(name: &str, image: &Arc<Image>, idle: Duration, sddl: String) -> Serving {
            let (stop, image) = (Arc::new(AtomicBool::new(false)), Arc::clone(image));
            let (halt, name) = (Arc::clone(&stop), name.to_string());
            let thread = thread::spawn(move || {
                let open = move |drive| match drive {
                    'C' => Ok(Box::new(Arc::clone(&image)) as Box<dyn Disk>),
                    _ => Err(io::ErrorKind::NotFound.into()),
                };
                server::serve(&name, &sddl, &open, &halt, idle)
            });
            Serving {
                stop,
                thread: Some(thread),
            }
        }

        /// How serving ended, waiting up to `within`; none if it goes on.
        fn ended(&mut self, within: Duration) -> Option<io::Result<()>> {
            let deadline = Instant::now() + within;
            let thread = self.thread.take()?;
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            match thread.is_finished() {
                true => Some(thread.join().expect("serving does not panic")),
                false => {
                    self.thread = Some(thread);
                    None
                }
            }
        }
    }

    impl Drop for Serving {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Connects, trying for up to ten seconds while the pipe is not there
    /// yet.
    fn asked(name: &str, server: Option<u32>, request: &Request) -> io::Result<Connection> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Connection::ask(name, server, request) {
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                other => return other,
            }
        }
    }

    fn here() -> Option<u32> {
        Some(std::process::id())
    }

    fn next(connection: &Connection) -> Answer {
        let stop = AtomicBool::new(false);
        connection.next(&stop).expect("read").expect("an answer")
    }

    fn volume() -> Arc<Image> {
        Arc::new(Image::new(&[(16, record().folder().name(5, "Users", 1))]))
    }

    fn follow_c() -> Request {
        Request::Follow {
            drive: 'C',
            from: None,
        }
    }

    #[test]
    fn the_launcher_follows_a_volume_through_the_pipe() {
        let (name, image) = (name("follow"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        let connection = asked(&name, here(), &follow_c()).expect("connected");
        let at = Position {
            volume: SERIAL,
            journal: 1,
            next: 0,
        };
        assert_eq!(next(&connection), Answer::Listing(at));
        assert!(matches!(next(&connection), Answer::Files(files) if files.len() == 2));
        assert_eq!(next(&connection), Answer::Listed);
        // Longer than the server waits before it looks whether to stop.
        thread::sleep(crate::overlapped::PATIENCE * 2);
        image.change(16, None);
        let gone = Change {
            record: 16,
            file: None,
        };
        let moved = Position { next: 96, ..at };
        assert_eq!(next(&connection), Answer::Changed(moved, vec![gone]));
    }

    #[test]
    fn a_volume_the_service_does_not_read_is_refused_through_the_pipe() {
        let (name, image) = (name("refused"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        let d = Request::Follow {
            drive: 'D',
            from: None,
        };
        let connection = asked(&name, here(), &d).expect("connected");
        assert_eq!(next(&connection), Answer::Refused);
        let stop = AtomicBool::new(false);
        assert!(connection.next(&stop).is_err(), "and let go");
    }

    #[test]
    fn a_request_of_another_version_is_told_the_service_s_through_the_pipe() {
        let (name, image) = (name("outdated"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        let mut bytes = follow_c().bytes();
        bytes[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
        let deadline = Instant::now() + Duration::from_secs(10);
        let connection = loop {
            match Connection::send(&name, here(), &bytes) {
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                other => break other.expect("connected"),
            }
        };
        assert_eq!(next(&connection), Answer::Outdated(VERSION));
    }

    #[test]
    fn a_pipe_served_by_any_other_process_is_refused() {
        let (name, image) = (name("impostor"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        let other = Some(std::process::id() + 1);
        let refused = asked(&name, other, &follow_c()).err().expect("refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(asked(&name, None, &follow_c()).is_ok(), "no server named");
    }

    #[test]
    fn nobody_may_serve_a_name_the_service_holds() {
        let (name, image) = (name("taken"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        asked(&name, here(), &follow_c()).expect("served");
        let mut second = Serving::start(&name, &image, Duration::from_secs(60));
        assert!(
            matches!(second.ended(Duration::from_secs(10)), Some(Err(_))),
            "refused at once"
        );
    }

    #[test]
    fn the_service_serves_another_while_one_is_followed_however_long() {
        let (name, image) = (name("busy"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_millis(300));
        let first = asked(&name, here(), &follow_c()).expect("connected");
        assert!(matches!(next(&first), Answer::Listing(_)));
        thread::sleep(crate::overlapped::PATIENCE * 2);
        let refused = Request::Follow {
            drive: 'D',
            from: None,
        };
        let second = Connection::ask(&name, here(), &refused).expect("still served");
        assert_eq!(next(&second), Answer::Refused);
    }

    #[test]
    fn a_wait_for_an_answer_ends_once_stopped() {
        let (name, image) = (name("stopped"), volume());
        let _serving = Serving::start(&name, &image, Duration::from_secs(60));
        let connection = asked(&name, here(), &follow_c()).expect("connected");
        while !matches!(next(&connection), Answer::Listed) {}
        let stop = AtomicBool::new(true);
        assert_eq!(connection.next(&stop).expect("no error"), None);
    }

    #[test]
    fn the_service_ends_once_nobody_used_it_for_a_while() {
        let (name, image) = (name("idle"), volume());
        let mut serving = Serving::start(&name, &image, Duration::from_millis(300));
        let connection = asked(&name, here(), &follow_c()).expect("connected");
        let busy = serving.ended(Duration::from_secs(1));
        assert_eq!(busy.map(|ended| ended.is_ok()), None, "busy");
        drop(connection);
        assert!(
            matches!(serving.ended(Duration::from_secs(10)), Some(Ok(()))),
            "unused since"
        );
    }

    #[test]
    fn the_pipe_lets_no_one_make_an_instance_of_it() {
        use windows::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};
        let (name, image) = (name("rights"), volume());
        let me = security::current_user().expect("a user");
        let sddl = security::pipe_sddl(&[me]);
        let _serving = Serving::secured(&name, &image, Duration::from_secs(60), sddl);
        let busy = asked(&name, here(), &follow_c()).expect("the launcher's rights");
        drop(busy);
        let access = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
        // SAFETY: a plain call; a handle it gave would be closed by `Owned`.
        let opened = unsafe {
            CreateFileW(
                &HSTRING::from(name.as_str()),
                access,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        let refused = opened.map(Owned).err().expect("refused");
        assert_eq!(refused.code(), windows::Win32::Foundation::E_ACCESSDENIED);
    }
}
