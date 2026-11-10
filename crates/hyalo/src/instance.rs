//! One launcher per user session: whoever holds a named mutex in the
//! session's `Local\` namespace is the launcher; a second start finds it
//! held and shows the running launcher instead.
//!
//! The mutex is created owned, and its owner holds it until the process
//! ends, so a new build that replaces a running one can wait on the
//! handle `Taken` keeps: the wait ends, with the mutex its own, when the
//! old process exits.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::core::{HSTRING, Result};

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

/// Runs the launcher if this is the first start: `run`, told whether to
/// show it, holds the mutex `name` until it returns. A later start calls
/// `show` instead, unless it was to start hidden: a start with Windows
/// has nothing to show.
pub fn first(
    name: &str,
    shown: bool,
    run: impl FnOnce(bool) -> Result<()>,
    show: impl FnOnce(),
) -> Result<()> {
    match claim(name)? {
        Claim::Ours(_launcher) => run(shown),
        // A build replacing the running one would wait on it here.
        Claim::Taken(_running) if shown => {
            show();
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

    /// What `first` did: ran the launcher (and whether shown), or showed
    /// the one running.
    fn started(name: &str, shown: bool) -> (Option<bool>, bool) {
        let ran = std::cell::Cell::new(None);
        let showed = std::cell::Cell::new(false);
        let run = |shown| {
            ran.set(Some(shown));
            Ok(())
        };
        first(name, shown, run, || showed.set(true)).expect("started");
        (ran.get(), showed.get())
    }

    #[test]
    fn the_first_start_runs_the_launcher_shown_or_hidden() {
        assert_eq!(started(&name("first-shown"), true), (Some(true), false));
        assert_eq!(started(&name("first-hidden"), false), (Some(false), false));
    }

    #[test]
    fn a_later_start_shows_the_running_launcher_unless_it_starts_hidden() {
        let name = name("later");
        let _running = claim(&name).expect("claim");
        assert_eq!(started(&name, true), (None, true));
        assert_eq!(started(&name, false), (None, false));
    }

    #[test]
    fn the_launcher_holds_the_mutex_while_it_runs_and_frees_it_after() {
        let name = name("holds");
        let run = |_| {
            assert!(!ours(&claim(&name).expect("claim")), "held while running");
            Ok(())
        };
        first(&name, true, run, || {}).expect("started");
        assert!(ours(&claim(&name).expect("claim")), "free once it returned");
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
