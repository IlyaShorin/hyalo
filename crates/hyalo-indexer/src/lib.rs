//! The indexer service: what hyalo reads of an NTFS volume with the
//! rights only an administrator has, so the launcher, which never has
//! them, need not walk the drives.
//!
//! The master file table lists every file and folder of a volume: read
//! whole, it gives every name, the folder it is in, its size and its last
//! write in a few seconds. The USN journal then says which files changed,
//! and the table is read again for those alone.
//!
//! The service is hyalo.exe itself, copied where only an administrator
//! may write and started by the service manager with `--indexer`
//! (`command`); the launcher talks to it over a pipe (`client`).

use std::io;
use std::path::{Path, PathBuf};

mod bytes;
pub mod client;
pub mod directory;
pub mod disk;
pub mod listing;
pub mod manager;
pub mod mft;
pub mod ntfs;
mod overlapped;
pub mod read;
pub mod scm;
pub mod security;
pub mod server;
pub mod service;
pub mod usn;
mod wire;

/// The service's pipe.
pub const PIPE: &str = r"\\.\pipe\hyalo.indexer";

/// What hyalo.exe is started to do for the service, instead of running the
/// launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Be the service: the service manager starts it so.
    Serve,
    /// Install the service for the user of this SID: an administrator's
    /// process the launcher starts.
    Install(String),
    /// Remove the service, the same way.
    Remove,
}

/// The argument that installs the service, followed by the user's SID.
pub const INSTALL: &str = "--indexer-install";

/// The argument that removes it.
pub const REMOVE: &str = "--indexer-remove";

/// What the command line `arguments` (the exe's own path first) asks of
/// the service; none if it asks for the launcher.
pub fn command(arguments: &[String]) -> Option<Command> {
    let given: Vec<&str> = arguments.iter().skip(1).map(String::as_str).collect();
    match given[..] {
        [scm::ARGUMENT] => Some(Command::Serve),
        [INSTALL, user] => Some(Command::Install(user.to_string())),
        [REMOVE] => Some(Command::Remove),
        _ => None,
    }
}

/// Where the service's exe lies: under Program Files, where only an
/// administrator may write.
pub fn target_in(program_files: &Path) -> PathBuf {
    program_files.join(r"hyalo\hyalo-indexer.exe")
}

/// Does what the command line `arguments` asks of the service, if it
/// asks anything, and ends the process; else nothing.
pub fn run_if_asked(arguments: &[String]) {
    if let Some(asked) = command(arguments) {
        std::process::exit(run(asked));
    }
}

/// Does what `command` asks; the process's exit code: 0 when it went
/// well.
pub fn run(command: Command) -> i32 {
    exit_code(act(command))
}

/// The exit code that says how it went.
fn exit_code(done: io::Result<()>) -> i32 {
    match done {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("hyalo indexer: {error}");
            1
        }
    }
}

/// Runs the service, or installs or removes it on this machine.
fn act(command: Command) -> io::Result<()> {
    match command {
        Command::Serve => service::dispatch(),
        Command::Install(user) => install_here(&user),
        Command::Remove => scm::remove(&manager::Manager, &target()?),
    }
}

/// Installs this exe as the service for `user`.
fn install_here(user: &str) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    scm::install(&manager::Manager, &exe, &target()?, user)
}

/// Where the service's exe lies on this machine.
fn target() -> io::Result<PathBuf> {
    let folder = std::env::var_os("ProgramFiles").ok_or(io::ErrorKind::NotFound)?;
    Ok(target_in(Path::new(&folder)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(given: &[&str]) -> Vec<String> {
        let exe = r"C:\Program Files\hyalo\hyalo-indexer.exe";
        [exe]
            .iter()
            .chain(given)
            .map(|argument| argument.to_string())
            .collect()
    }

    #[test]
    fn the_command_line_says_what_is_asked_of_the_service() {
        assert_eq!(command(&arguments(&["--indexer"])), Some(Command::Serve));
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            command(&arguments(&[INSTALL, sid])),
            Some(Command::Install(sid.to_string()))
        );
        assert_eq!(command(&arguments(&[REMOVE])), Some(Command::Remove));
    }

    #[test]
    fn anything_else_is_the_launcher_s() {
        for given in [
            &[][..],
            &["--hidden"],
            &["--settings"],
            &[INSTALL],
            &["--indexer", "more"],
            &[REMOVE, "more"],
            &[INSTALL, "S-1-5-18", "more"],
        ] {
            assert_eq!(command(&arguments(given)), None, "{given:?}");
        }
        assert_eq!(command(&[]), None);
    }

    #[test]
    fn installing_for_what_is_no_sid_fails_before_it_touches_anything() {
        assert_eq!(run(Command::Install("WD".to_string())), 1);
        assert_eq!(exit_code(Ok(())), 0);
    }

    #[test]
    fn the_service_lies_in_hyalo_s_folder_under_program_files() {
        assert_eq!(
            target_in(Path::new(r"C:\Program Files")),
            Path::new(r"C:\Program Files\hyalo\hyalo-indexer.exe")
        );
    }
}
