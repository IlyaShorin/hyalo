//! The service's side of the pipe: an instance for each connection, on a
//! thread of its own, answering the one request it reads until the
//! launcher lets go; the pipe only the system and the users who installed
//! the service may open.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use hyalo_core::pipe::{Answer, Request, Unread, VERSION};
use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::core::HSTRING;

use crate::disk::Disk;
use crate::overlapped::{Call, Owned, pending, wait_while};
use crate::read::{self, Out};
use crate::security::Security;
use crate::wire::End;

/// Opens the volume of a drive (`C`) for reading.
pub type Opener<'a> = &'a (dyn Fn(char) -> io::Result<Box<dyn Disk>> + Sync);

/// What the pipe holds for writing to the launcher: a listing frame or two.
const OUT_ROOM: u32 = 2 << 20;
/// What it holds of what the launcher writes: a request.
const IN_ROOM: u32 = 64 << 10;

/// Answers one request, until it is answered or `stop` is set: another
/// version, nothing for garble, the volume refused, or what was asked.
pub fn answer(
    request: Result<Request, Unread>,
    open: Opener,
    out: Out,
    stop: &AtomicBool,
) -> io::Result<()> {
    let (drive, asked) = match request {
        Err(Unread::Outdated) => return out(Answer::Outdated(VERSION)),
        Err(Unread::Garbled) => return Ok(()),
        Ok(Request::Follow { drive, from }) => (drive, Asked::Follow(from)),
        Ok(Request::Subtree { drive, records }) => (drive, Asked::Subtree(records)),
    };
    let Ok(disk) = open(drive) else {
        return out(Answer::Refused);
    };
    match asked {
        Asked::Follow(from) => read::follow(&*disk, from, out, stop),
        Asked::Subtree(records) => read::subtree(&*disk, &records, out),
    }
}

/// What a request asks of its volume.
enum Asked {
    Follow(Option<hyalo_core::volume::Position>),
    Subtree(Vec<u32>),
}

/// Serves the pipe `name`, secured as `sddl` says (`security::pipe_sddl`),
/// until `stop` is set, or nobody has used it for `idle`. Refused if
/// another process holds the name.
pub fn serve(
    name: &str,
    sddl: &str,
    open: Opener,
    stop: &AtomicBool,
    idle: Duration,
) -> io::Result<()> {
    let security = Security::new(sddl)?;
    let name = HSTRING::from(name);
    let (busy, last) = (AtomicUsize::new(0), Mutex::new(Instant::now()));
    let (busy, last) = (&busy, &last);
    let unused = || {
        let since = *last.lock().unwrap_or_else(PoisonError::into_inner);
        busy.load(Ordering::SeqCst) == 0 && since.elapsed() >= idle
    };
    let going = || !stop.load(Ordering::SeqCst) && !unused();
    thread::scope(|scope| {
        let mut first = true;
        loop {
            let end = instance(&name, &security, first)?;
            first = false;
            if !connect(&end, &going)? {
                return Ok(());
            }
            busy.fetch_add(1, Ordering::SeqCst);
            scope.spawn(move || {
                let _ = connection(end, open, stop);
                *last.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
                busy.fetch_sub(1, Ordering::SeqCst);
            });
        }
    })
}

/// A new instance of the pipe: the first refused if the name is taken,
/// so nobody posing as the service gets there before it.
fn instance(name: &HSTRING, security: &Security, first: bool) -> io::Result<End> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    let only = match first {
        true => FILE_FLAG_FIRST_PIPE_INSTANCE,
        false => FILE_FLAGS_AND_ATTRIBUTES(0),
    };
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: the name and the attributes outlive the call; the handle it
    // gives is closed by `Owned`.
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | only,
            mode,
            PIPE_UNLIMITED_INSTANCES,
            OUT_ROOM,
            IN_ROOM,
            0,
            Some(&attributes),
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Ok(End(Owned(handle)))
}

/// Waits for the launcher to connect to `end`, while `going` says to;
/// whether it did.
fn connect(end: &End, going: &dyn Fn() -> bool) -> io::Result<bool> {
    let mut call = Call::new(0)?;
    // SAFETY: the call outlives the wait, which sees it end or cancels it.
    let issued = unsafe { ConnectNamedPipe(end.0.0, Some(&raw mut call.overlapped)) };
    if let Err(error) = &issued
        && error.code() == ERROR_PIPE_CONNECTED.to_hresult()
    {
        return Ok(true);
    }
    pending(issued)?;
    Ok(wait_while(end.0.0, &raw const call.overlapped, going)?.is_some())
}

/// Reads the launcher's request on `end` and answers it, until it lets go
/// or `stop` is set; then sees what was written read, and lets go.
fn connection(end: End, open: Opener, stop: &AtomicBool) -> io::Result<()> {
    let Some(body) = end.frame(stop)? else {
        return Ok(());
    };
    let over = AtomicBool::new(false);
    let answered = thread::scope(|scope| {
        scope.spawn(|| watch(&end, &over, stop));
        let mut out = |answer: Answer| end.write(&answer.bytes(), &over);
        let answered = answer(Request::read(&body), open, &mut out, &over);
        over.store(true, Ordering::SeqCst);
        answered
    });
    // SAFETY: plain calls on a pipe that is ours.
    unsafe {
        let _ = FlushFileBuffers(end.0.0);
        let _ = DisconnectNamedPipe(end.0.0);
    }
    answered
}

/// Waits for the launcher, which sends nothing after its request, to let
/// go of `end`, or for `over` or `stop` to be set; then sets `over`.
fn watch(end: &End, over: &AtomicBool, stop: &AtomicBool) {
    let mut byte = [0_u8; 1];
    let going = || !over.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst);
    if let Ok(mut call) = Call::new(0) {
        // SAFETY: the byte and the call outlive the read: the wait sees it
        // end, or cancels it and sees it end.
        let issued = unsafe {
            ReadFile(
                end.0.0,
                Some(&mut byte),
                None,
                Some(&raw mut call.overlapped),
            )
        };
        if pending(issued).is_ok() {
            let _ = wait_while(end.0.0, &raw const call.overlapped, &going);
        }
    }
    over.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::image::Image;
    use crate::mft::build::record;
    use hyalo_core::volume::Position;
    use std::sync::Arc;

    /// Opens a volume in memory as `C`, and nothing else.
    fn opener(image: &Arc<Image>) -> impl Fn(char) -> io::Result<Box<dyn Disk>> + Sync + use<> {
        let image = Arc::clone(image);
        move |drive| match drive {
            'C' => Ok(Box::new(Arc::clone(&image)) as Box<dyn Disk>),
            _ => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn answered(request: Result<Request, Unread>, image: &Arc<Image>) -> Vec<Answer> {
        let mut answers = Vec::new();
        let mut out = |answer| {
            answers.push(answer);
            Ok(())
        };
        let stop = AtomicBool::new(true);
        answer(request, &opener(image), &mut out, &stop).expect("answered");
        answers
    }

    fn volume() -> Arc<Image> {
        Arc::new(Image::new(&[(16, record().folder().name(5, "Users", 1))]))
    }

    #[test]
    fn a_request_of_another_version_is_told_the_service_s() {
        assert_eq!(
            answered(Err(Unread::Outdated), &volume()),
            [Answer::Outdated(VERSION)]
        );
        assert_eq!(answered(Err(Unread::Garbled), &volume()), []);
    }

    #[test]
    fn a_volume_the_service_does_not_read_is_refused() {
        for request in [
            Request::Follow {
                drive: 'D',
                from: None,
            },
            Request::Subtree {
                drive: 'D',
                records: vec![16],
            },
        ] {
            assert_eq!(answered(Ok(request), &volume()), [Answer::Refused]);
        }
    }

    #[test]
    fn a_volume_followed_is_listed_until_stopped() {
        let follow = Request::Follow {
            drive: 'C',
            from: None,
        };
        let answers = answered(Ok(follow), &volume());
        let at = Position {
            volume: crate::disk::image::SERIAL,
            journal: 1,
            next: 0,
        };
        assert_eq!(answers[0], Answer::Listing(at));
        assert_eq!(answers.last(), Some(&Answer::Listed));
    }

    #[test]
    fn folders_asked_for_are_sent_with_what_they_hold() {
        let image = volume();
        image.hold(16, &[0]);
        let subtree = Request::Subtree {
            drive: 'C',
            records: vec![16],
        };
        let answers = answered(Ok(subtree), &image);
        assert!(matches!(&answers[0], Answer::Files(files) if files[0].record == 0));
        assert_eq!(answers[1..], [Answer::Listed]);
    }
}
