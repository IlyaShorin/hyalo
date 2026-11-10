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
