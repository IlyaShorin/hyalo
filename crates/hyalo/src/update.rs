//! Updating hyalo from its GitHub releases: asking for the latest one,
//! downloading its exe and checking it against its SHA-256, swapping it
//! in for the exe that runs, and starting it in this one's place. What
//! to make of a release is `hyalo_core::update`'s; this is the disk, the
//! network and the processes.
//!
//! A running exe can be renamed but not overwritten, so the swap renames
//! `hyalo.exe` to `hyalo.exe.old` and puts the new one in its place. The
//! new build starts with `--hidden --replace`: it waits for this process
//! to end before it takes the launcher's mutex, then removes the old exe.
//! A new build that cannot start puts the old one back and starts it.

use std::fs;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use hyalo_core::update::{Digest, Failure, Release, Version};
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

use crate::autostart;
use crate::http::{self, Address};
use crate::instance;

/// Where GitHub answers with hyalo's latest release.
pub fn releases() -> Address {
    Address::https("api.github.com", "/repos/IlyaShorin/hyalo/releases/latest")
}

/// The version of the build that runs: the workspace's.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version is a version")
}

/// Brings back what a URL holds, or why it could not.
pub type Download = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Starts the exe at a path in place of the one that runs.
pub type Starter = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

/// What `url` holds, through WinHTTP.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let address = Address::parse(url).ok_or_else(|| format!("{url} is not a URL to download"))?;
    http::get(&address).map_err(|error| error.message())
}

/// The SHA-256 of `bytes`, from Windows' own cryptography.
pub fn sha256(bytes: &[u8]) -> Digest {
    let mut digest = [0_u8; 32];
    // SAFETY: the pseudo-handle is always there (Windows 10 and later),
    // and `digest` is as long as a SHA-256.
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut digest) };
    assert!(status.is_ok(), "SHA-256 failed: {status:?}");
    Digest(digest)
}

/// What installing needs: where to download from, the exe to replace,
/// and how to start the new one.
pub struct Installer {
    pub download: Download,
    pub exe: PathBuf,
    pub start: Starter,
}

/// What the launcher needs to update itself: the real ones in `real`,
/// stand-ins in tests.
pub struct Updates {
    /// The version that runs.
    pub current: Version,
    /// GitHub's answer about the latest release.
    pub latest: Arc<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync>,
    pub installer: Arc<Installer>,
}

impl Updates {
    /// GitHub's releases, downloaded through WinHTTP, swapped in for the
    /// exe that runs. None in a debug build, which never offers updates:
    /// it runs from a build folder, and a release would take its place.
    pub fn real() -> Option<Updates> {
        if cfg!(debug_assertions) {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        Some(Updates {
            current: current(),
            latest: Arc::new(|| download_from(&releases())),
            installer: Arc::new(Installer {
                download: Arc::new(download),
                exe,
                start: Arc::new(restart),
            }),
        })
    }
}

/// What `address` holds, or why it could not be had.
fn download_from(address: &Address) -> Result<Vec<u8>, String> {
    http::get(address).map_err(|error| error.message())
}

/// Installs `release`: downloads its sum and its exe, refuses an exe the
/// sum does not name, swaps it in for `installer.exe` and starts it. A
/// new exe that does not start is taken out again, the old one put back.
/// Whatever fails, the exe that runs stays in place.
pub fn install(release: &Release, installer: &Installer) -> Result<(), Failure> {
    let sum = (installer.download)(&release.sum).map_err(Failure::Download)?;
    let sum = Digest::parse(&sum).ok_or(Failure::Mismatch)?;
    let exe = (installer.download)(&release.exe).map_err(Failure::Download)?;
    if sha256(&exe) != sum {
        return Err(Failure::Mismatch);
    }
    let said = |error: io::Error| error.to_string();
    swap(&installer.exe, &exe).map_err(|error| Failure::Swap(said(error)))?;
    (installer.start)(&installer.exe).map_err(|error| {
        if let Err(back) = put_back(&installer.exe) {
            eprintln!("hyalo: cannot put the old build back: {back}");
        }
        Failure::Start(said(error))
    })
}

/// The names the swap gives the exes beside `hyalo.exe`: the one written
/// before it is swapped in, the one swapped out, and one that failed to
/// start.
const NEW: &str = ".new";
const OLD: &str = ".old";
const FAILED: &str = ".failed";

/// `exe` with `suffix` added to its name: `hyalo.exe.old`.
fn beside(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

/// Puts `new` in place of the exe at `exe`, which goes to `exe.old`. On
/// failure everything is as it was.
pub fn swap(exe: &Path, new: &[u8]) -> io::Result<()> {
    let (fresh, old) = (beside(exe, NEW), beside(exe, OLD));
    fs::write(&fresh, new)?;
    let swapped = fs::rename(exe, &old).and_then(|()| {
        fs::rename(&fresh, exe).inspect_err(|_| {
            let _ = fs::rename(&old, exe);
        })
    });
    if swapped.is_err() {
        let _ = fs::remove_file(&fresh);
    }
    swapped
}

/// Undoes a swap: the exe at `exe` goes to `exe.failed` (renamed, as it
/// may be running) and `exe.old` comes back in its place. An error, and
/// nothing moved, when there is no `exe.old`.
pub fn put_back(exe: &Path) -> io::Result<()> {
    let old = beside(exe, OLD);
    fs::metadata(&old)?;
    fs::rename(exe, beside(exe, FAILED))?;
    fs::rename(&old, exe)
}

/// Removes what the last swap left beside `exe`, if anything; the error
/// of the first one that is there and cannot go.
pub fn tidy(exe: &Path) -> io::Result<()> {
    for suffix in [OLD, NEW, FAILED] {
        match fs::remove_file(beside(exe, suffix)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

/// Tidies up beside `exe`, trying `tries` times `pause` apart: the old
/// build that was swapped out may still be ending, and Windows keeps its
/// exe until it has. Whether it all went.
pub fn tidy_soon(exe: &Path, tries: u32, pause: Duration) -> bool {
    for _ in 0..tries {
        if tidy(exe).is_ok() {
            return true;
        }
        std::thread::sleep(pause);
    }
    false
}

/// How often, and how far apart, a start tries to remove the old build.
const TIDY_TRIES: u32 = 60;
const TIDY_PAUSE: Duration = Duration::from_millis(500);

/// What a start that was to `replace` the launcher that ran does about
/// the swap, once it knows whether its window opened (`started`): one
/// that works tidies up, on a thread of its own; one that could not start
/// puts the old build back and starts it with `start`. Any other start
/// leaves the files alone: a `hyalo.exe.old` there is the user's.
pub fn settle(exe: &Path, started: bool, replace: bool, start: &dyn Fn(&Path) -> io::Result<()>) {
    if !replace {
        return;
    }
    if started {
        let exe = exe.to_path_buf();
        std::thread::spawn(move || tidy_soon(&exe, TIDY_TRIES, TIDY_PAUSE));
    } else if let Err(error) = put_back(exe).and_then(|()| start(exe)) {
        eprintln!("hyalo: cannot go back to the old build: {error}");
    }
}

/// `settle` for the exe that runs, starting the old build for real. Not
/// tested: under test the exe that runs is the test binary.
pub fn settle_running(started: bool, replace: bool) {
    if let Ok(exe) = std::env::current_exe() {
        settle(&exe, started, replace, &restart);
    }
}

/// Starts the exe at `exe` hidden, in the tray, to replace this process,
/// with nothing of this one's: no console, no handles to stdin or stdout.
pub fn restart(exe: &Path) -> io::Result<()> {
    command(exe).spawn().map(drop)
}

/// The command that starts `exe` to replace this launcher.
fn command(exe: &Path) -> Command {
    detached(exe, &[autostart::HIDDEN, instance::REPLACE])
}

/// The command that starts `exe` with `flags` and nothing of this
/// process's: no console, no handles to stdin or stdout.
pub fn detached(exe: &Path, flags: &[&str]) -> Command {
    let mut command = Command::new(exe);
    command
        .args(flags)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::tests::{answer, serve};
    use crate::scratch::Scratch;
    use hyalo_core::update::{EXE, SUM};
    use std::collections::HashMap;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::Mutex;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    fn hex(digest: Digest) -> String {
        digest.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn sha256_is_the_standard_one() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let million = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
        assert_eq!(hex(sha256(b"")), empty);
        assert_eq!(hex(sha256(b"abc")), abc);
        assert_eq!(hex(sha256(&[b'a'; 1_000_000])), million);
    }

    #[test]
    fn the_latest_release_comes_from_githubs_api_over_https() {
        let address = releases();
        let named = (address.host.as_str(), address.port, address.path.as_str());
        let path = "/repos/IlyaShorin/hyalo/releases/latest";
        assert_eq!(named, ("api.github.com", 443, path));
        assert!(address.secure);
    }

    #[test]
    fn the_build_that_runs_is_the_workspaces_version() {
        assert_eq!(current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_debug_build_offers_no_updates() {
        assert!(Updates::real().is_none());
    }

    #[test]
    fn a_download_brings_back_what_the_url_holds() {
        let (port, server) = serve(answer("200 OK", b"the exe"));
        let url = format!("http://127.0.0.1:{port}/hyalo.exe");
        assert_eq!(download(&url), Ok(b"the exe".to_vec()));
        assert!(
            server
                .join()
                .expect("the server")
                .starts_with("GET /hyalo.exe ")
        );
        let (port, server) = serve(answer("404 Not Found", b""));
        let url = format!("http://127.0.0.1:{port}/hyalo.exe");
        let error = download(&url).expect_err("not found");
        assert!(error.contains("404"), "{error}");
        server.join().expect("the server");
        let error = download("hyalo.exe").expect_err("no URL");
        assert!(error.contains("hyalo.exe"), "{error}");
    }

    /// What a test's downloads and starts did.
    #[derive(Default)]
    struct Seen {
        downloaded: Vec<String>,
        started: Vec<PathBuf>,
    }

    const EXE_URL: &str = "https://github.com/download/hyalo.exe";
    const SUM_URL: &str = "https://github.com/download/hyalo.exe.sha256";

    fn release() -> Release {
        Release {
            version: Version::parse("0.0.2").expect("a version"),
            notes: String::new(),
            exe: EXE_URL.to_string(),
            sum: SUM_URL.to_string(),
        }
    }

    /// A release's sum file for `exe`, as the workflow writes it.
    fn sum_of(exe: &[u8]) -> Vec<u8> {
        format!("{}  {EXE}\n", hex(sha256(exe))).into_bytes()
    }

    /// A scratch folder holding `hyalo.exe` (`old build`), an installer
    /// that serves `served` by URL and starts as `starts` says, and what
    /// it saw.
    fn set_up(
        test: &str,
        served: HashMap<&'static str, Vec<u8>>,
        starts: bool,
    ) -> (Scratch, Installer, Arc<Mutex<Seen>>) {
        let folder = Scratch::new(test);
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"old build").expect("the old build");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (asked, started) = (seen.clone(), seen.clone());
        let installer = Installer {
            download: Arc::new(move |url: &str| {
                asked.lock().expect("seen").downloaded.push(url.to_string());
                served
                    .get(url)
                    .cloned()
                    .ok_or_else(|| "timed out".to_string())
            }),
            exe,
            start: Arc::new(move |exe: &Path| {
                started
                    .lock()
                    .expect("seen")
                    .started
                    .push(exe.to_path_buf());
                match starts {
                    true => Ok(()),
                    false => Err(io::Error::other("not a program")),
                }
            }),
        };
        (folder, installer, seen)
    }

    /// The files in `folder`, by name, with what they hold.
    fn files(folder: &Scratch) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = fs::read_dir(&folder.0)
            .expect("the folder")
            .map(|entry| {
                let entry = entry.expect("an entry");
                let text = fs::read_to_string(entry.path()).expect("the file");
                (entry.file_name().to_string_lossy().into_owned(), text)
            })
            .collect();
        files.sort();
        files
    }

    fn named(files: &[(&str, &str)]) -> Vec<(String, String)> {
        let named = files
            .iter()
            .map(|(name, text)| (name.to_string(), text.to_string()));
        named.collect()
    }

    #[test]
    fn install_checks_the_exe_against_its_sum_swaps_it_in_and_starts_it() {
        let served = HashMap::from([
            (SUM_URL, sum_of(b"new build")),
            (EXE_URL, b"new build".to_vec()),
        ]);
        let (folder, installer, seen) = set_up("update-install", served, true);
        assert_eq!(install(&release(), &installer), Ok(()));
        assert_eq!(
            files(&folder),
            named(&[("hyalo.exe", "new build"), ("hyalo.exe.old", "old build")])
        );
        let seen = seen.lock().expect("seen");
        assert_eq!(seen.downloaded, [SUM_URL, EXE_URL], "the sum first");
        assert_eq!(seen.started, std::slice::from_ref(&installer.exe));
    }

    #[test]
    fn an_exe_its_sum_does_not_name_is_refused_and_nothing_changes() {
        let served = HashMap::from([
            (SUM_URL, sum_of(b"new build")),
            (EXE_URL, b"new build, changed on the way".to_vec()),
        ]);
        let (folder, installer, seen) = set_up("update-mismatch", served, true);
        assert_eq!(install(&release(), &installer), Err(Failure::Mismatch));
        assert_eq!(files(&folder), named(&[("hyalo.exe", "old build")]));
        assert!(seen.lock().expect("seen").started.is_empty());
    }

    #[test]
    fn a_sum_that_does_not_read_is_a_mismatch() {
        let served = HashMap::from([
            (SUM_URL, b"<html>Not Found</html>".to_vec()),
            (EXE_URL, b"new build".to_vec()),
        ]);
        let (folder, installer, seen) = set_up("update-no-sum", served, true);
        assert_eq!(install(&release(), &installer), Err(Failure::Mismatch));
        assert_eq!(files(&folder), named(&[("hyalo.exe", "old build")]));
        assert!(seen.lock().expect("seen").started.is_empty());
    }

    #[test]
    fn a_download_that_fails_is_told_and_nothing_changes() {
        let timed_out = Err(Failure::Download("timed out".to_string()));
        let (folder, installer, _) = set_up("update-no-sum-download", HashMap::new(), true);
        assert_eq!(install(&release(), &installer), timed_out);
        assert_eq!(files(&folder), named(&[("hyalo.exe", "old build")]));
        let served = HashMap::from([(SUM_URL, sum_of(b"new build"))]);
        let (folder, installer, seen) = set_up("update-no-exe-download", served, true);
        assert_eq!(install(&release(), &installer), timed_out);
        assert_eq!(files(&folder), named(&[("hyalo.exe", "old build")]));
        assert!(seen.lock().expect("seen").started.is_empty());
    }

    #[test]
    fn a_build_that_does_not_start_is_taken_out_and_the_old_one_put_back() {
        let served = HashMap::from([
            (SUM_URL, sum_of(b"new build")),
            (EXE_URL, b"new build".to_vec()),
        ]);
        let (folder, installer, seen) = set_up("update-no-start", served, false);
        let failure = Failure::Start("not a program".to_string());
        assert_eq!(install(&release(), &installer), Err(failure));
        assert_eq!(
            files(&folder),
            named(&[
                ("hyalo.exe", "old build"),
                ("hyalo.exe.failed", "new build")
            ])
        );
        assert_eq!(seen.lock().expect("seen").started.len(), 1);
    }

    #[test]
    fn a_swap_that_cannot_move_the_exe_aside_leaves_everything_as_it_was() {
        let folder = Scratch::new("update-swap-held");
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"old build").expect("the old build");
        fs::write(beside(&exe, OLD), b"older build").expect("an older build");
        // The older build still runs, as Windows holds a running exe: it
        // may not be replaced.
        let held = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(beside(&exe, OLD))
            .expect("hold the older build");
        assert!(swap(&exe, b"new build").is_err());
        drop(held);
        assert_eq!(
            files(&folder),
            named(&[("hyalo.exe", "old build"), ("hyalo.exe.old", "older build")])
        );
    }

    #[test]
    fn a_swap_with_no_exe_to_move_aside_writes_nothing() {
        let folder = Scratch::new("update-swap-missing");
        let exe = folder.0.join(EXE);
        let error = swap(&exe, b"new build").expect_err("no exe");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(files(&folder), named(&[]));
    }

    #[test]
    fn a_swap_whose_new_exe_cannot_be_written_moves_nothing() {
        let folder = Scratch::new("update-swap-unwritable");
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"old build").expect("the old build");
        fs::create_dir(beside(&exe, NEW)).expect("a folder in the new exe's place");
        assert!(swap(&exe, b"new build").is_err());
        assert_eq!(fs::read(&exe).expect("the exe"), b"old build");
        assert!(!beside(&exe, OLD).exists());
    }

    #[test]
    fn putting_back_with_nothing_swapped_out_is_an_error_and_changes_nothing() {
        let folder = Scratch::new("update-put-back-nothing");
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"new build").expect("the exe");
        assert!(put_back(&exe).is_err());
        assert_eq!(files(&folder), named(&[("hyalo.exe", "new build")]));
    }

    #[test]
    fn tidying_removes_what_a_swap_left_and_nothing_else() {
        let folder = Scratch::new("update-tidy");
        let exe = folder.0.join(EXE);
        for (suffix, text) in [("", "new"), (OLD, "old"), (NEW, "half"), (FAILED, "bad")] {
            fs::write(beside(&exe, suffix), text).expect("a file");
        }
        fs::write(folder.0.join("hyalo.exe.sha256"), "sum").expect("a file");
        assert!(tidy(&exe).is_ok());
        assert_eq!(
            files(&folder),
            named(&[("hyalo.exe", "new"), ("hyalo.exe.sha256", "sum")])
        );
        assert!(tidy(&exe).is_ok(), "nothing left to remove");
    }

    #[test]
    fn tidying_soon_waits_for_the_old_build_to_let_go_of_its_exe() {
        let folder = Scratch::new("update-tidy-soon");
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"new").expect("the exe");
        fs::write(beside(&exe, OLD), b"old").expect("the old exe");
        let held = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(beside(&exe, OLD))
            .expect("hold the old exe");
        assert!(tidy(&exe).is_err(), "held");
        assert!(!tidy_soon(&exe, 2, Duration::from_millis(1)), "held still");
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        assert!(tidy_soon(&exe, 500, Duration::from_millis(10)));
        letting_go.join().expect("let go");
        assert_eq!(files(&folder), named(&[("hyalo.exe", "new")]));
    }

    #[test]
    fn tidying_soon_uses_every_try_and_does_not_wait_once_it_is_done() {
        let folder = Scratch::new("update-tidy-once");
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"new").expect("the exe");
        fs::write(beside(&exe, OLD), b"old").expect("the old exe");
        let began = std::time::Instant::now();
        assert!(
            tidy_soon(&exe, 1, Duration::from_secs(10)),
            "one try is enough"
        );
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "no pause after it"
        );
        assert!(!tidy_soon(&exe, 0, Duration::ZERO), "no try, so not done");
        assert_eq!(files(&folder), named(&[("hyalo.exe", "new")]));
    }

    /// The exes a test's starter was asked to start.
    type Started = Arc<Mutex<Vec<PathBuf>>>;

    /// A starter that starts nothing and notes what it was asked to.
    fn starter() -> (impl Fn(&Path) -> io::Result<()>, Started) {
        let started = Arc::new(Mutex::new(Vec::new()));
        let noted = started.clone();
        let start = move |exe: &Path| {
            noted.lock().expect("started").push(exe.to_path_buf());
            Ok(())
        };
        (start, started)
    }

    /// A scratch folder as a swap leaves it: the new build in place, the
    /// old one beside it.
    fn swapped(test: &str) -> (Scratch, PathBuf) {
        let folder = Scratch::new(test);
        let exe = folder.0.join(EXE);
        fs::write(&exe, b"new build").expect("the new build");
        fs::write(beside(&exe, OLD), b"old build").expect("the old build");
        (folder, exe)
    }

    #[test]
    fn a_new_build_that_starts_removes_the_old_one_soon() {
        let (folder, exe) = swapped("update-settle-started");
        let (start, started) = starter();
        settle(&exe, true, true, &start);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while beside(&exe, OLD).exists() {
            assert!(std::time::Instant::now() < deadline, "the old build stays");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(files(&folder), named(&[("hyalo.exe", "new build")]));
        assert!(started.lock().expect("started").is_empty());
    }

    #[test]
    fn a_new_build_that_cannot_start_puts_the_old_one_back_and_starts_it() {
        let (folder, exe) = swapped("update-settle-failed");
        let (start, started) = starter();
        settle(&exe, false, true, &start);
        assert_eq!(
            files(&folder),
            named(&[
                ("hyalo.exe", "old build"),
                ("hyalo.exe.failed", "new build")
            ])
        );
        assert_eq!(*started.lock().expect("started"), [exe]);
    }

    #[test]
    fn a_start_that_replaced_nothing_leaves_the_files_alone_whether_it_opened_or_not() {
        // A hyalo.exe.old of the user's own, kept by hand before hyalo
        // updated itself, is theirs: only an update's start tidies.
        let (folder, exe) = swapped("update-settle-plain");
        let (start, started) = starter();
        settle(&exe, false, false, &start);
        settle(&exe, true, false, &start);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            files(&folder),
            named(&[("hyalo.exe", "new build"), ("hyalo.exe.old", "old build")])
        );
        assert!(started.lock().expect("started").is_empty());
    }

    #[test]
    fn the_new_build_starts_hidden_to_replace_this_one_with_nothing_of_its_own() {
        let exe = Path::new(r"C:\Users\someone\AppData\Local\Programs\hyalo\hyalo.exe");
        let command = command(exe);
        assert_eq!(command.get_program(), exe.as_os_str());
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, ["--hidden", "--replace"]);
    }

    #[test]
    fn restarting_starts_the_exe_and_a_missing_one_is_an_error() {
        // A copy of this test binary stands in for the new build: started
        // with hyalo's flags, the test runner refuses them and ends.
        let folder = Scratch::new("update-restart");
        let exe = folder.0.join(EXE);
        fs::copy(std::env::current_exe().expect("the test binary"), &exe).expect("a copy");
        let mut child = command(&exe).spawn().expect("started");
        let status = child.wait().expect("ended");
        assert!(!status.success(), "the runner refused --hidden");
        let missing = folder.0.join("missing.exe");
        assert_eq!(
            restart(&missing).expect_err("no exe").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn the_release_workflow_publishes_the_exe_and_the_sum_the_updater_reads() {
        let workflow = include_str!("../../../.github/workflows/release.yml");
        let published = format!(r"gh release create $env:TAG dist\{EXE} dist\{SUM}");
        assert!(workflow.contains(&published), "{published}");
        let summed = format!(r#"[IO.File]::WriteAllText("$PWD\dist\{SUM}", "$hash  {EXE}`n")"#);
        assert!(workflow.contains(&summed), "{summed}");
        let line = format!("{}  {EXE}\n", hex(sha256(b"abc")));
        assert_eq!(Digest::parse(line.as_bytes()), Some(sha256(b"abc")));
    }

    #[test]
    fn the_release_script_publishes_the_same_exe_and_sum_as_the_workflow() {
        let script = include_str!("../../../scripts/release.ps1");
        for line in [
            format!(r#"$exe = Join-Path $dist "{EXE}""#),
            format!(r#"$sum = Join-Path $dist "{SUM}""#),
            format!(r#"[IO.File]::WriteAllText($sum, "$hash  {EXE}`n")"#),
            "gh release create $Tag $exe $sum --target $commit".to_string(),
        ] {
            assert!(script.contains(&line), "{line}");
        }
    }

    /// The DLLs the running exe imports from, as its import table names
    /// them, in lower case.
    fn imported_dlls() -> Vec<String> {
        use windows::Win32::System::Diagnostics::Debug::{
            IMAGE_DIRECTORY_ENTRY_IMPORT, IMAGE_NT_HEADERS64,
        };
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::SystemServices::{IMAGE_DOS_HEADER, IMAGE_IMPORT_DESCRIPTOR};
        // SAFETY: the loader mapped the running exe at its module handle,
        // each part at the offset from it (RVA) the headers give, and
        // keeps it mapped while the process runs; the import table ends
        // with an entry naming nothing.
        unsafe {
            let base = GetModuleHandleW(None).expect("the exe").0 as *const u8;
            let dos = &*base.cast::<IMAGE_DOS_HEADER>();
            let nt = &*base.add(dos.e_lfanew as usize).cast::<IMAGE_NT_HEADERS64>();
            let table = nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT.0 as usize];
            let mut entry = base
                .add(table.VirtualAddress as usize)
                .cast::<IMAGE_IMPORT_DESCRIPTOR>();
            let mut dlls = Vec::new();
            while (*entry).Name != 0 {
                let name = std::ffi::CStr::from_ptr(base.add((*entry).Name as usize).cast());
                dlls.push(name.to_string_lossy().to_lowercase());
                entry = entry.add(1);
            }
            dlls
        }
    }

    /// A fresh Windows 10 or 11 has no Visual C++ Redistributable, whose
    /// vcruntime140.dll a Rust exe imports unless it carries the C
    /// runtime itself. The tests are built as hyalo.exe is.
    #[test]
    fn the_exe_carries_the_c_runtime_a_fresh_windows_lacks() {
        let dlls = imported_dlls();
        assert!(dlls.iter().any(|dll| dll == "kernel32.dll"), "{dlls:?}");
        let runtime = dlls.iter().find(|dll| dll.starts_with("vcruntime"));
        assert_eq!(runtime, None, "{dlls:?}");
    }

    /// Windows PowerShell 5.1 stops a script whose errors stop it on any
    /// line a native command writes to its error output, and gh writes
    /// "release not found" there for a release not out yet: 0.2.1's run
    /// stopped there, before publishing.
    #[test]
    fn the_release_script_asks_whether_the_release_is_out_without_stopping_on_the_answer() {
        let script = include_str!("../../../scripts/release.ps1");
        let asked = "$ErrorActionPreference = \"Continue\"\ngh release view $Tag *> $null\n$out = $LASTEXITCODE -eq 0\n$ErrorActionPreference = \"Stop\"\n";
        assert!(script.contains(asked), "{asked}");
    }

    #[test]
    fn the_sum_file_is_named_after_the_exe() {
        assert_eq!(SUM, format!("{EXE}.sha256"));
        assert_eq!(
            beside(Path::new(r"C:\h\hyalo.exe"), OLD),
            Path::new(r"C:\h\hyalo.exe.old")
        );
    }
}
