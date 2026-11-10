//! One launcher per user session: whoever holds a named mutex in the
//! session's `Local\` namespace is the launcher; a second start finds it
//! held and shows the running launcher instead.
//!
//! The mutex is created owned, and its owner holds it until the process
//! ends, so a new build that replaces a running one (`--replace`) waits
//! on the handle `Taken` keeps: the wait ends, with the mutex its own,
//! when the old process exits.

use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::{HSTRING, Result};

use crate::autostart;

/// The launcher's mutex.
pub const NAME: &str = r"Local\hyalo.launcher";

/// Who holds the mutex.
pub enum Claim {
    /// This process: it is the launcher.
    Ours(Instance),
    /// Another process; its mutex, to wait on.
    Taken(Taken),
}

/// The mutex, owned by the thread that claimed it, until dropped.
pub struct Instance(HANDLE);

/// The mutex another process holds.
pub struct Taken(HANDLE);

/// How hyalo was asked to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Show the launcher.
    Shown,
    /// Only the tray icon, as Windows starts it at logon.
    Hidden,
    /// Open the settings window, as a Start menu entry would.
    Settings,
}

impl Start {
    /// How the command line's arguments ask hyalo to start: `--settings`,
    /// `--hidden` (as the Run value has it), or shown.
    pub fn from_args(arguments: impl IntoIterator<Item = String>) -> Start {
        let arguments: Vec<String> = arguments.into_iter().collect();
        let given = |flag: &str| arguments.iter().any(|argument| argument == flag);
        match (given(SETTINGS), given(autostart::HIDDEN)) {
            (true, _) => Start::Settings,
            (false, true) => Start::Hidden,
            (false, false) => Start::Shown,
        }
    }
}

/// The argument that opens the settings window.
pub const SETTINGS: &str = "--settings";

/// The argument an update starts the new build with: it waits for the
/// launcher it replaces to end, then runs in its place.
pub const REPLACE: &str = "--replace";

/// Whether the command line's arguments ask this start to replace the
/// launcher that runs.
pub fn replacing(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == REPLACE)
}

/// How long a start that replaces the launcher waits for it to end: it
/// ends as soon as it has started the new build, but saving the picks
/// and closing the window take a moment on a busy machine.
pub const REPLACE_WAIT: Duration = Duration::from_secs(60);

/// Runs the launcher if this is the first start: `run`, told how to
/// start, holds the mutex `name` until it returns. A start that replaces
/// the launcher waits up to `replace` for that one to end, then runs; it
/// gives up when it does not. A later start calls `ask` instead, to have
/// the running launcher show or open its settings, unless it was to start
/// hidden: a start with Windows has nothing to ask.
pub fn first(
    name: &str,
    start: Start,
    replace: Option<Duration>,
    run: impl FnOnce(Start) -> Result<()>,
    ask: impl FnOnce(Start),
) -> Result<()> {
    match claim(name)? {
        Claim::Ours(_launcher) => run(start),
        Claim::Taken(running) if let Some(wait) = replace => match running.wait(wait) {
            Some(_launcher) => run(start),
            None => Ok(()),
        },
        Claim::Taken(_running) if start != Start::Hidden => {
            ask(start);
            Ok(())
        }
        Claim::Taken(_running) => Ok(()),
    }
}

/// Creates the mutex `name`, owned by this thread, unless it is there
/// already.
pub fn claim(name: &str) -> Result<Claim> {
    // SAFETY: a plain call; the handle it gives is closed by whoever
    // keeps it. Asking for ownership of a mutex that is there already
    // opens it without owning it, and says so in the last error.
    unsafe {
        let mutex = CreateMutexW(None, true, &HSTRING::from(name))?;
        match GetLastError() == ERROR_ALREADY_EXISTS {
            true => Ok(Claim::Taken(Taken(mutex))),
            false => Ok(Claim::Ours(Instance(mutex))),
        }
    }
}

impl Taken {
    /// Waits up to `wait` for the process that holds the mutex to let it
    /// go or to end: the mutex is then this thread's. `None` when it was
    /// still held after `wait`.
    pub fn wait(self, wait: Duration) -> Option<Instance> {
        let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the handle is ours and open; a mutex whose owner ended
        // without letting go is had all the same (WAIT_ABANDONED).
        let waited = unsafe { WaitForSingleObject(self.0, millis) };
        if waited != WAIT_OBJECT_0 && waited != WAIT_ABANDONED {
            return None;
        }
        // The handle moves to the instance, which closes it once.
        let handle = self.0;
        std::mem::forget(self);
        Some(Instance(handle))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and owned by this thread; closed once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        // SAFETY: the handle is ours; closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::JoinHandle;

    fn name(test: &str) -> String {
        format!(r"Local\hyalo-test-{}-{test}", std::process::id())
    }

    fn ours(claim: &Claim) -> bool {
        matches!(claim, Claim::Ours(_))
    }

    #[test]
    fn the_first_claim_is_ours_and_a_second_finds_it_taken() {
        let name = name("second");
        let first = claim(&name).expect("claim");
        assert!(ours(&first));
        let second = claim(&name).expect("claim again");
        assert!(!ours(&second));
    }

    #[test]
    fn once_the_launcher_is_gone_the_next_claim_is_ours() {
        let name = name("gone");
        drop(claim(&name).expect("claim"));
        assert!(ours(&claim(&name).expect("claim again")));
    }

    /// What `first` did: ran the launcher (and how it started), or asked
    /// the one running (and what).
    fn started(name: &str, start: Start) -> (Option<Start>, Option<Start>) {
        let ran = std::cell::Cell::new(None);
        let asked = std::cell::Cell::new(None);
        let run = |start| {
            ran.set(Some(start));
            Ok(())
        };
        first(name, start, None, run, |start| asked.set(Some(start))).expect("started");
        (ran.get(), asked.get())
    }

    #[test]
    fn the_first_start_runs_the_launcher_as_it_was_asked_to_start() {
        for start in [Start::Shown, Start::Hidden, Start::Settings] {
            assert_eq!(started(&name("first"), start), (Some(start), None));
        }
    }

    #[test]
    fn a_later_start_asks_the_running_launcher_unless_it_starts_hidden() {
        let name = name("later");
        let _running = claim(&name).expect("claim");
        assert_eq!(started(&name, Start::Shown), (None, Some(Start::Shown)));
        assert_eq!(
            started(&name, Start::Settings),
            (None, Some(Start::Settings))
        );
        assert_eq!(started(&name, Start::Hidden), (None, None));
    }

    fn arguments(list: &[&str]) -> Vec<String> {
        list.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn the_command_line_says_how_to_start() {
        assert_eq!(Start::from_args(arguments(&["hyalo.exe"])), Start::Shown);
        assert_eq!(
            Start::from_args(arguments(&["hyalo.exe", "--hidden"])),
            Start::Hidden
        );
        assert_eq!(
            Start::from_args(arguments(&["hyalo.exe", "--settings"])),
            Start::Settings
        );
        assert_eq!(
            Start::from_args(arguments(&["hyalo.exe", "--hide"])),
            Start::Shown
        );
        let both = arguments(&["hyalo.exe", "--hidden", "--settings"]);
        assert_eq!(Start::from_args(both), Start::Settings, "settings win");
    }

    #[test]
    fn the_launcher_holds_the_mutex_while_it_runs_and_frees_it_after() {
        let name = name("holds");
        let run = |_| {
            assert!(!ours(&claim(&name).expect("claim")), "held while running");
            Ok(())
        };
        first(&name, Start::Shown, None, run, |_| {}).expect("started");
        assert!(ours(&claim(&name).expect("claim")), "free once it returned");
    }

    /// Whether the mutex `taken` holds can be had within `wait`.
    fn had(taken: &Taken, wait: u32) -> bool {
        use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::WaitForSingleObject;
        let had = unsafe { WaitForSingleObject(taken.0, wait) };
        let had = [WAIT_OBJECT_0, WAIT_ABANDONED].contains(&had);
        if had {
            unsafe { ReleaseMutex(taken.0) }.expect("give it back");
        }
        had
    }

    #[test]
    fn a_later_start_can_wait_for_the_launcher_to_end() {
        let name = name("wait");
        let (claimed, release) = std::sync::mpsc::channel();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let elsewhere = name.clone();
        let launcher = std::thread::spawn(move || {
            let ours = claim(&elsewhere).expect("claim");
            claimed.send(()).expect("say so");
            let _ = gone.recv();
            drop(ours);
        });
        release.recv().expect("claimed");
        let Claim::Taken(taken) = claim(&name).expect("claim again") else {
            panic!("the launcher holds it");
        };
        assert!(!had(&taken, 0), "held while the launcher runs");
        go.send(()).expect("let it end");
        launcher.join().expect("the launcher");
        assert!(had(&taken, 5_000), "had once it ended");
    }

    /// A launcher on a thread of its own that holds the mutex `name`
    /// until the test lets it end, `released` or not.
    fn running(name: &str, released: bool) -> (std::sync::mpsc::Sender<()>, JoinHandle<()>) {
        let (claimed, ready) = std::sync::mpsc::channel();
        let (go, gone) = std::sync::mpsc::channel::<()>();
        let name = name.to_string();
        let launcher = std::thread::spawn(move || {
            let ours = claim(&name).expect("claim");
            claimed.send(()).expect("say so");
            let _ = gone.recv();
            match released {
                true => drop(ours),
                // The thread ends owning it: the mutex is abandoned.
                false => std::mem::forget(ours),
            }
        });
        ready.recv().expect("claimed");
        (go, launcher)
    }

    #[test]
    fn a_start_that_replaces_the_launcher_runs_once_that_one_has_ended() {
        let name = name("replace");
        let (go, launcher) = running(&name, true);
        let ending = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            go.send(()).expect("let it end");
        });
        let began = std::time::Instant::now();
        let ran = std::cell::Cell::new(None);
        let run = |start| {
            let elsewhere = name.clone();
            let held = std::thread::spawn(move || !ours(&claim(&elsewhere).expect("claim")));
            assert!(held.join().expect("the thread"), "held by the new launcher");
            ran.set(Some(start));
            Ok(())
        };
        let wait = Some(std::time::Duration::from_secs(5));
        first(&name, Start::Hidden, wait, run, |_| panic!("asked")).expect("started");
        assert_eq!(ran.get(), Some(Start::Hidden));
        assert!(began.elapsed() >= std::time::Duration::from_millis(90));
        ending.join().expect("ended");
        launcher.join().expect("the launcher");
        assert!(ours(&claim(&name).expect("claim")), "free once it returned");
    }

    #[test]
    fn a_launcher_that_ended_without_letting_go_is_replaced_too() {
        let name = name("abandoned");
        let (go, launcher) = running(&name, false);
        go.send(()).expect("let it end");
        launcher.join().expect("the launcher");
        let Claim::Taken(taken) = claim(&name).expect("claim") else {
            panic!("abandoned, not gone");
        };
        assert!(taken.wait(std::time::Duration::from_secs(5)).is_some());
    }

    #[test]
    fn a_start_that_replaces_gives_up_when_the_launcher_does_not_end() {
        let name = name("stays");
        let (go, launcher) = running(&name, true);
        let ran = std::cell::Cell::new(false);
        let run = |_| {
            ran.set(true);
            Ok(())
        };
        let wait = Some(std::time::Duration::from_millis(50));
        first(&name, Start::Hidden, wait, run, |_| panic!("asked")).expect("gave up");
        assert!(!ran.get());
        go.send(()).expect("let it end");
        launcher.join().expect("the launcher");
    }

    #[test]
    fn the_command_line_says_whether_to_replace_the_launcher() {
        let update = arguments(&["hyalo.exe", "--hidden", "--replace"]);
        assert!(replacing(&update));
        assert_eq!(Start::from_args(update), Start::Hidden);
        assert!(!replacing(&arguments(&["hyalo.exe", "--hidden"])));
        assert!(!replacing(&arguments(&["hyalo.exe", "--replaced"])));
    }

    #[test]
    fn a_claim_from_another_thread_finds_it_taken_too() {
        let name = name("thread");
        let first = claim(&name).expect("claim");
        assert!(ours(&first));
        let elsewhere = name.clone();
        let second = std::thread::spawn(move || ours(&claim(&elsewhere).expect("claim")));
        assert!(!second.join().expect("the thread"));
    }
}
