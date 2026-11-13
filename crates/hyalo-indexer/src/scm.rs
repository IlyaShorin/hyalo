//! The service as Windows' service manager keeps it: installed once, with
//! the rights of an administrator, by a process of hyalo's own that does
//! nothing else (`hyalo.exe --indexer-install <SID>`); removed the same
//! way. The exe the system runs lies where only an administrator may
//! write, never in the user's folders. Through `Services`, the service
//! manager's calls, so the steps are tested with a stand-in.

use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::security;

/// The service's name, as the service manager knows it.
pub const NAME: &str = "hyalo-indexer";

/// Its name in the Services console.
pub const DISPLAY: &str = "hyalo indexer";

/// What the Services console says it does.
pub const DESCRIPTION: &str = "Reads the NTFS master file table and journal for the hyalo launcher, \
     so it finds every file at once. Started by hyalo, it stops a minute after hyalo lets go.";

/// What the service manager starts the exe with.
pub const ARGUMENT: &str = "--indexer";

/// How long a file the service's process held may stay held after it
/// stopped.
const LOCKED_FOR: Duration = Duration::from_secs(10);

/// A service, as its manager says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub running: bool,
    /// Its process, while it runs.
    pub process: u32,
}

/// Windows' service manager: a call for each.
pub trait Services {
    /// The service `name`, if it is installed.
    fn state(&self, name: &str) -> io::Result<Option<State>>;
    fn create(&self, name: &str, command: &str) -> io::Result<()>;
    /// Makes the installed service run `command`.
    fn configure(&self, name: &str, command: &str) -> io::Result<()>;
    fn describe(&self, name: &str) -> io::Result<()>;
    /// Who may do what with the service, in SDDL.
    fn secure(&self, name: &str, sddl: &str) -> io::Result<()>;
    /// The users it serves, by SID, as its parameters keep them.
    fn users(&self, name: &str) -> io::Result<Vec<String>>;
    fn set_users(&self, name: &str, users: &[String]) -> io::Result<()>;
    fn start(&self, name: &str) -> io::Result<()>;
    /// Stops it, and waits until it has.
    fn stop(&self, name: &str) -> io::Result<()>;
    fn delete(&self, name: &str) -> io::Result<()>;
}

/// The security the service manager gives a service it makes.
const DEFAULT_SDDL: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)\
     (A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";

/// The service's own security: the system and administrators as for any
/// service, interactive users may ask how it is, and each of `users` may
/// also start it.
pub fn service_sddl(users: &[String]) -> String {
    let mut sddl = DEFAULT_SDDL.to_string();
    for user in users.iter().filter(|user| security::is_sid(user)) {
        sddl.push_str(&format!("(A;;CCLCSWRPLOCRRC;;;{user})"));
    }
    sddl
}

/// `users` with `user` after them, each once, SIDs alone.
pub fn merged(users: &[String], user: &str) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    let all = users.iter().map(String::as_str).chain([user]);
    for one in all.filter(|one| security::is_sid(one)) {
        if !merged.iter().any(|there| there == one) {
            merged.push(one.to_string());
        }
    }
    merged
}

/// What the service manager runs: the exe at `target`, quoted, and the
/// argument that makes it the service.
pub fn command(target: &Path) -> String {
    format!("\"{}\" {ARGUMENT}", target.display())
}

/// Installs the exe at `exe` as the service, at `target`, for the user
/// `user` (a SID) beside those it serves already: stops it if it runs,
/// copies the exe over, makes it or points it at the copy, and lets the
/// user start it and use its pipe.
pub fn install(services: &dyn Services, exe: &Path, target: &Path, user: &str) -> io::Result<()> {
    if !security::is_sid(user) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    put_in_place(services, exe, target)?;
    let users = merged(&services.users(NAME)?, user);
    services.set_users(NAME, &users)?;
    services.secure(NAME, &service_sddl(&users))?;
    services.describe(NAME)
}

/// Copies the exe at `exe` to `target`, the service stopped if it runs,
/// and makes the service, or points it at the copy.
fn put_in_place(services: &dyn Services, exe: &Path, target: &Path) -> io::Result<()> {
    let state = services.state(NAME)?;
    stop_running(services, state)?;
    copy(exe, target)?;
    match state {
        None => services.create(NAME, &command(target)),
        Some(_) => services.configure(NAME, &command(target)),
    }
}

/// Stops the service if it is in `state`, running.
fn stop_running(services: &dyn Services, state: Option<State>) -> io::Result<()> {
    match state.is_some_and(|state| state.running) {
        true => services.stop(NAME),
        false => Ok(()),
    }
}

/// Removes the service and its exe at `target`, stopping it first.
pub fn remove(services: &dyn Services, target: &Path) -> io::Result<()> {
    let Some(state) = services.state(NAME)? else {
        return Ok(());
    };
    stop_running(services, Some(state))?;
    services.delete(NAME)?;
    held_on(|| fs::remove_file(target))?;
    match target.parent() {
        Some(folder) => held_on(|| fs::remove_dir(folder)),
        None => Ok(()),
    }
}

/// Copies `exe` to `target`, making its folder, trying again for a while
/// while the file there is held.
fn copy(exe: &Path, target: &Path) -> io::Result<()> {
    if let Some(folder) = target.parent() {
        fs::create_dir_all(folder)?;
    }
    held_on(|| fs::copy(exe, target).map(drop))
}

/// Does `act`, trying again for a while as long as a file it needs is
/// held: the service's process holds its exe for a moment after the
/// service manager says it stopped. A file already gone is no failure.
fn held_on(act: impl Fn() -> io::Result<()>) -> io::Result<()> {
    let until = Instant::now() + LOCKED_FOR;
    loop {
        match act() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if held(&error) && Instant::now() < until => {
                thread::sleep(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// Whether `error` says a file is held by another process.
fn held(error: &io::Error) -> bool {
    // ERROR_SHARING_VIOLATION, and ERROR_ACCESS_DENIED for an exe still
    // mapped by the process that ran it.
    matches!(error.raw_os_error(), Some(32 | 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlapped::tests::Scratch;
    use std::cell::RefCell;
    use std::path::PathBuf;

    const ME: &str = "S-1-5-21-1-2-3-1001";
    const YOU: &str = "S-1-5-21-1-2-3-1002";

    /// The service manager, noting what it was asked, with a service in
    /// the state given, serving the users given.
    struct Noted {
        state: RefCell<Option<State>>,
        users: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
    }

    impl Noted {
        fn new(state: Option<State>, users: &[&str]) -> Noted {
            Noted {
                state: RefCell::new(state),
                users: RefCell::new(users.iter().map(|user| user.to_string()).collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn note(&self, call: String) -> io::Result<()> {
            self.calls.borrow_mut().push(call);
            Ok(())
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Services for Noted {
        fn state(&self, name: &str) -> io::Result<Option<State>> {
            assert_eq!(name, NAME);
            Ok(*self.state.borrow())
        }

        fn create(&self, _: &str, command: &str) -> io::Result<()> {
            *self.state.borrow_mut() = Some(STOPPED);
            self.note(format!("create {command}"))
        }

        fn configure(&self, _: &str, command: &str) -> io::Result<()> {
            self.note(format!("configure {command}"))
        }

        fn describe(&self, _: &str) -> io::Result<()> {
            self.note("describe".to_string())
        }

        fn secure(&self, _: &str, sddl: &str) -> io::Result<()> {
            self.note(format!("secure {sddl}"))
        }

        fn users(&self, _: &str) -> io::Result<Vec<String>> {
            Ok(self.users.borrow().clone())
        }

        fn set_users(&self, _: &str, users: &[String]) -> io::Result<()> {
            *self.users.borrow_mut() = users.to_vec();
            self.note(format!("users {}", users.join(" ")))
        }

        fn start(&self, _: &str) -> io::Result<()> {
            self.note("start".to_string())
        }

        fn stop(&self, _: &str) -> io::Result<()> {
            *self.state.borrow_mut() = Some(STOPPED);
            self.note("stop".to_string())
        }

        fn delete(&self, _: &str) -> io::Result<()> {
            *self.state.borrow_mut() = None;
            self.note("delete".to_string())
        }
    }

    const STOPPED: State = State {
        running: false,
        process: 0,
    };

    const RUNNING: State = State {
        running: true,
        process: 4242,
    };

    /// The exe to install, and where it goes.
    fn paths(scratch: &Scratch) -> (PathBuf, PathBuf) {
        let exe = scratch.0.join(r"user\hyalo.exe");
        fs::create_dir_all(exe.parent().unwrap()).expect("made");
        fs::write(&exe, b"the new build").expect("written");
        (
            exe,
            scratch.0.join(r"Program Files\hyalo\hyalo-indexer.exe"),
        )
    }

    #[test]
    fn the_service_lets_each_of_its_users_start_it_and_ask_how_it_is() {
        let users = [ME.to_string(), "WD".to_string(), YOU.to_string()];
        let default = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";
        assert_eq!(
            service_sddl(&users),
            format!("{default}(A;;CCLCSWRPLOCRRC;;;{ME})(A;;CCLCSWRPLOCRRC;;;{YOU})")
        );
        assert_eq!(service_sddl(&[]), default);
    }

    #[test]
    fn a_user_is_added_once_after_those_there() {
        let there = [ME.to_string(), "WD".to_string()];
        assert_eq!(merged(&there, YOU), [ME, YOU]);
        assert_eq!(merged(&there, ME), [ME]);
        assert_eq!(merged(&[], "not a sid"), Vec::<String>::new());
    }

    #[test]
    fn the_service_manager_runs_the_exe_quoted_as_the_service() {
        let target = Path::new(r"C:\Program Files\hyalo\hyalo-indexer.exe");
        assert_eq!(
            command(target),
            r#""C:\Program Files\hyalo\hyalo-indexer.exe" --indexer"#
        );
    }

    #[test]
    fn installing_where_nothing_is_copies_the_exe_and_makes_the_service() {
        let scratch = Scratch::new("install");
        let (exe, target) = paths(&scratch);
        let services = Noted::new(None, &[]);
        install(&services, &exe, &target, ME).expect("installed");
        assert_eq!(fs::read(&target).expect("copied"), b"the new build");
        assert_eq!(
            services.calls(),
            [
                format!("create {}", command(&target)),
                format!("users {ME}"),
                format!("secure {}", service_sddl(&[ME.to_string()])),
                "describe".to_string(),
            ]
        );
    }

    #[test]
    fn installing_for_another_user_keeps_the_first_and_stops_it_first() {
        let scratch = Scratch::new("install-again");
        let (exe, target) = paths(&scratch);
        fs::create_dir_all(target.parent().unwrap()).expect("made");
        fs::write(&target, b"the old build").expect("written");
        let services = Noted::new(Some(RUNNING), &[ME]);
        install(&services, &exe, &target, YOU).expect("installed");
        assert_eq!(fs::read(&target).expect("copied"), b"the new build");
        let both = [ME.to_string(), YOU.to_string()];
        assert_eq!(
            services.calls(),
            [
                "stop".to_string(),
                format!("configure {}", command(&target)),
                format!("users {ME} {YOU}"),
                format!("secure {}", service_sddl(&both)),
                "describe".to_string(),
            ]
        );
    }

    #[test]
    fn what_is_no_sid_is_installed_for_nobody() {
        let scratch = Scratch::new("install-no-sid");
        let (exe, target) = paths(&scratch);
        let services = Noted::new(None, &[]);
        let refused = install(&services, &exe, &target, "S-1-5-21)(A;;GA;;;WD");
        assert_eq!(
            refused.map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidInput)
        );
        assert_eq!(services.calls(), Vec::<String>::new());
        assert!(!target.exists());
    }

    #[test]
    fn an_exe_still_held_a_moment_after_the_service_stopped_is_copied_over_once_let_go() {
        use std::os::windows::fs::OpenOptionsExt;
        let scratch = Scratch::new("install-held");
        let (exe, target) = paths(&scratch);
        fs::create_dir_all(target.parent().unwrap()).expect("made");
        fs::write(&target, b"the old build").expect("written");
        // Held without sharing writes, as a process's own exe is.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .expect("held");
        let letting_go = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            drop(held);
        });
        let services = Noted::new(Some(RUNNING), &[ME]);
        install(&services, &exe, &target, ME).expect("installed");
        letting_go.join().expect("let go");
        assert_eq!(fs::read(&target).expect("copied"), b"the new build");
    }

    #[test]
    fn removing_stops_the_service_deletes_it_and_its_exe() {
        let scratch = Scratch::new("remove");
        let (_, target) = paths(&scratch);
        fs::create_dir_all(target.parent().unwrap()).expect("made");
        fs::write(&target, b"installed").expect("written");
        let services = Noted::new(Some(RUNNING), &[ME]);
        remove(&services, &target).expect("removed");
        assert_eq!(services.calls(), ["stop", "delete"]);
        assert!(!target.parent().unwrap().exists(), "its folder too");
    }

    #[test]
    fn removing_what_is_not_installed_does_nothing() {
        let scratch = Scratch::new("remove-nothing");
        let (_, target) = paths(&scratch);
        let services = Noted::new(None, &[]);
        remove(&services, &target).expect("nothing to remove");
        assert_eq!(services.calls(), Vec::<String>::new());
    }

    #[test]
    fn removing_a_stopped_service_does_not_stop_it() {
        let scratch = Scratch::new("remove-stopped");
        let (_, target) = paths(&scratch);
        let services = Noted::new(Some(STOPPED), &[ME]);
        remove(&services, &target).expect("removed");
        assert_eq!(services.calls(), ["delete"]);
    }
}
