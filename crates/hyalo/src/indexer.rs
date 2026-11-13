//! The indexer service, as the launcher sees it: whether it is installed,
//! and installing or removing it, which takes an administrator's yes. The
//! launcher never runs with those rights: it starts hyalo.exe again, as
//! administrator through the UAC prompt, to do that and nothing else
//! (`hyalo_indexer::command`), and waits for it to end.

use std::io;

use hyalo_indexer::manager::Manager;
use hyalo_indexer::scm::{self, Services};
use hyalo_indexer::security;

/// Installs, removes and asks after the service.
pub trait Control: Send + Sync {
    fn installed(&self) -> bool;

    /// Installs the service for this user (`true`), or removes it, and
    /// waits until it is done; refused if the UAC prompt was.
    fn change(&self, install: bool) -> io::Result<()>;
}

/// The command line that installs the service for `user`, or removes it.
pub fn arguments(install: bool, user: &str) -> String {
    match install {
        true => format!("{} {user}", hyalo_indexer::INSTALL),
        false => hyalo_indexer::REMOVE.to_string(),
    }
}

/// How the process that installed or removed the service ended: 0 when
/// it went well.
pub fn outcome(code: u32) -> io::Result<()> {
    match code {
        0 => Ok(()),
        code => Err(io::Error::other(format!("hyalo ended with {code}"))),
    }
}

/// This machine's service, changed through hyalo.exe started as
/// administrator. Not tested: it asks the UAC prompt and changes the
/// machine; what it asks is tested in `hyalo_indexer::scm`.
pub struct System;

impl Control for System {
    fn installed(&self) -> bool {
        Manager.state(scm::NAME).is_ok_and(|state| state.is_some())
    }

    fn change(&self, install: bool) -> io::Result<()> {
        let asked = command_line(install);
        let ended = asked.and_then(|(exe, arguments)| {
            crate::launch::waited(&exe, &arguments, true).map_err(io::Error::from)
        });
        ended.and_then(outcome)
    }
}

/// This exe, and the arguments that install the service for this
/// process's user, or remove it.
fn command_line(install: bool) -> io::Result<(String, String)> {
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    Ok((exe, arguments(install, &security::current_user()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyalo_is_asked_to_install_the_service_for_a_user_or_to_remove_it() {
        let user = "S-1-5-21-1-2-3-1001";
        assert_eq!(arguments(true, user), format!("--indexer-install {user}"));
        assert_eq!(arguments(false, user), "--indexer-remove");
    }

    #[test]
    fn this_exe_is_started_for_this_user() {
        let (exe, install) = command_line(true).expect("a command line");
        let here = std::env::current_exe().expect("this exe");
        assert_eq!(exe, here.to_string_lossy());
        let user = security::current_user().expect("a user");
        assert_eq!(install, arguments(true, &user));
        assert_eq!(
            command_line(false).expect("a command line").1,
            "--indexer-remove"
        );
    }

    #[test]
    fn only_an_exit_of_nothing_is_done() {
        assert!(outcome(0).is_ok());
        let failed = outcome(1).expect_err("failed");
        assert!(failed.to_string().contains('1'), "{failed}");
    }
}
