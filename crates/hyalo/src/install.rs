//! Installing for the current user: a release `hyalo.exe` started from
//! anywhere else (Downloads, say) puts itself in
//! `%LOCALAPPDATA%\Programs\hyalo` and runs from there. No admin rights,
//! nothing to click through.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hyalo_core::update::Version;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::{HSTRING, w};

use crate::autostart;
use crate::instance::{self, Start};
use crate::update;

/// The argument that keeps hyalo where it is started, uninstalled.
pub const PORTABLE: &str = "--portable";

/// Where a start of hyalo runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Where it is: the installed copy itself, a debug build, or a start
    /// asked to stay (`--portable`).
    Here,
    /// Nothing is installed: copy itself there, start that one.
    Install,
    /// An older build is installed: swap itself in, start it to replace
    /// the one that runs, if one does.
    Replace,
    /// A build as new or newer is installed: start that one, which runs
    /// or asks the one that runs to show.
    Defer,
}

/// What is installed, as far as a start can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Missing,
    /// There, saying it is this version; builds before 0.2.0 say none.
    At(Option<Version>),
}

/// Where a start runs, by whether it is the installed copy, was asked to
/// stay, is a debug build (which runs from its build folder), what is
/// installed, and its own version.
pub fn placement(
    is_installed: bool,
    portable: bool,
    debug: bool,
    installed: Installed,
    current: Version,
) -> Placement {
    if is_installed || portable || debug {
        return Placement::Here;
    }
    match installed {
        Installed::Missing => Placement::Install,
        Installed::At(Some(version)) if version >= current => Placement::Defer,
        Installed::At(_) => Placement::Replace,
    }
}

/// The flags that start a build the way `start` asks: in the tray, with
/// the settings open, or shown.
fn flags(start: Start) -> Vec<&'static str> {
    match start {
        Start::Hidden => vec![autostart::HIDDEN],
        Start::Settings => vec![instance::SETTINGS],
        Start::Shown => vec![],
    }
}

/// Starts an exe with flags, detached from this process.
pub type Starter<'a> = &'a dyn Fn(&Path, &[&str]) -> io::Result<()>;

/// Carries out `placement` for the exe at `running`, `installed` being
/// where hyalo installs, started as `start` asks: copies or swaps it in
/// and starts the installed build with `starter`. Replacing, `close`
/// first asks the launcher that runs, if one does, to end (before the new
/// build has a window it could reach), and the new build waits for it;
/// a new build that does not start is taken out and the old one started
/// again, hidden. Whether this start is done, another having taken over.
pub fn settle(
    placement: Placement,
    running: &Path,
    installed: &Path,
    start: Start,
    starter: Starter,
    close: &dyn Fn(),
) -> io::Result<bool> {
    match placement {
        Placement::Here => Ok(false),
        Placement::Install => install(running, installed, start, starter),
        Placement::Replace => replace(running, installed, start, starter, close),
        Placement::Defer => starter(installed, &flags(start)).map(|()| true),
    }
}

/// Copies the exe at `running` to `installed`, making its folder, and
/// starts it as `start` asks.
fn install(running: &Path, installed: &Path, start: Start, starter: Starter) -> io::Result<bool> {
    fs::create_dir_all(installed.parent().unwrap_or(Path::new(".")))?;
    fs::copy(running, installed)?;
    starter(installed, &flags(start)).map(|()| true)
}

/// Swaps the exe at `running` in for the older one at `installed`, asks
/// the hyalo that runs to end, and starts the new build to take over; one
/// that does not start goes, and the old build comes back, hidden.
fn replace(
    running: &Path,
    installed: &Path,
    start: Start,
    starter: Starter,
    close: &dyn Fn(),
) -> io::Result<bool> {
    update::swap(installed, &fs::read(running)?)?;
    close();
    let mut replacing = flags(start);
    replacing.push(instance::REPLACE);
    starter(installed, &replacing).inspect_err(|_| {
        if update::put_back(installed).is_ok() {
            let _ = starter(installed, &[autostart::HIDDEN]);
        }
    })?;
    Ok(true)
}

/// Where hyalo installs for the current user:
/// `%LOCALAPPDATA%\Programs\hyalo\hyalo.exe`.
pub fn installed_exe() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(r"Programs\hyalo\hyalo.exe"))
}

/// Whether `a` and `b` are one file: the same full path, letters in any
/// case, once both are made absolute and links followed where they exist.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let full = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (a, b) = (full(a), full(b));
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// What is installed at `exe`.
pub fn installed_at(exe: &Path) -> Installed {
    match exe.exists() {
        true => Installed::At(version_of(exe)),
        false => Installed::Missing,
    }
}

/// Whether the command line's arguments keep this start where it is:
/// `--portable`, or `--replace`, as an update starts the build it swapped
/// in where the old one was, installed or not.
pub fn stays(arguments: &[String]) -> bool {
    let stay = |argument: &String| argument == PORTABLE || argument == instance::REPLACE;
    arguments.iter().any(stay)
}

/// Installs, or hands over to the installed build, when this release
/// build runs from anywhere but the install folder; whether this start
/// is done. An install that fails is said and this build runs here. Not
/// tested: it reads the real install folder and starts real builds;
/// `placement` and `settle` are.
pub fn elsewhere(arguments: &[String], start: Start) -> bool {
    let (Ok(running), Some(installed)) = (std::env::current_exe(), installed_exe()) else {
        return false;
    };
    let portable = stays(arguments);
    let is_installed = same_file(&running, &installed);
    let debug = cfg!(debug_assertions);
    let current = update::current();
    let placement = placement(
        is_installed,
        portable,
        debug,
        installed_at(&installed),
        current,
    );
    let starter = |exe: &Path, flags: &[&str]| start_exe(exe, flags);
    let close = crate::window::close_running;
    settle(placement, &running, &installed, start, &starter, &close).unwrap_or_else(|error| {
        eprintln!("hyalo: cannot install, running from here: {error}");
        false
    })
}

/// Starts `exe` with `flags`, letting it take the foreground: this start
/// was the user's, and the build it starts shows the launcher.
fn start_exe(exe: &Path, flags: &[&str]) -> io::Result<()> {
    // SAFETY: a plain call; refused when this process may not hand the
    // foreground on, and then the launcher shows behind.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    update::detached(exe, flags).spawn().map(drop)
}

/// The version the exe at `exe` says it is in its version resource;
/// none when it has none (builds before 0.2.0) or cannot be read.
pub fn version_of(exe: &Path) -> Option<Version> {
    let path = HSTRING::from(exe.as_os_str());
    // SAFETY: a plain call; the path outlives it.
    let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0_u8; size as usize];
    // SAFETY: `data` holds the `size` bytes the call may write.
    unsafe { GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()) }.ok()?;
    let mut fixed: *mut VS_FIXEDFILEINFO = std::ptr::null_mut();
    let mut length = 0_u32;
    // SAFETY: `fixed` comes back pointing into `data`, which outlives
    // every use of it below.
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            w!("\\"),
            (&raw mut fixed).cast(),
            &mut length,
        )
    };
    if !found.as_bool() || fixed.is_null() || (length as usize) < size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: checked above: not null, and as long as the structure.
    let fixed = unsafe { *fixed };
    let (major, minor) = (fixed.dwFileVersionMS >> 16, fixed.dwFileVersionMS & 0xFFFF);
    let patch = fixed.dwFileVersionLS >> 16;
    Version::parse(&format!("{major}.{minor}.{patch}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use crate::update;

    use std::cell::{Cell, RefCell};

    fn v(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    #[test]
    fn the_installed_copy_a_debug_build_and_a_portable_start_run_where_they_are() {
        let older = Installed::At(Some(v("0.1.0")));
        assert_eq!(
            placement(true, false, false, older, v("0.2.0")),
            Placement::Here
        );
        assert_eq!(
            placement(false, true, false, older, v("0.2.0")),
            Placement::Here
        );
        assert_eq!(
            placement(false, false, true, older, v("0.2.0")),
            Placement::Here
        );
    }

    #[test]
    fn a_start_from_elsewhere_installs_replaces_an_older_build_or_defers_to_a_newer() {
        let now = v("0.2.0");
        let place = |installed| placement(false, false, false, installed, now);
        assert_eq!(place(Installed::Missing), Placement::Install);
        assert_eq!(place(Installed::At(Some(v("0.1.9")))), Placement::Replace);
        assert_eq!(
            place(Installed::At(None)),
            Placement::Replace,
            "builds before 0.2.0"
        );
        assert_eq!(place(Installed::At(Some(v("0.2.0")))), Placement::Defer);
        assert_eq!(place(Installed::At(Some(v("0.3.0")))), Placement::Defer);
    }

    /// A starter that notes what it was asked to start, failing the
    /// `fail`-th call (counting from 1; 0 never).
    struct Noted {
        calls: RefCell<Vec<(PathBuf, Vec<String>)>>,
        fail: usize,
    }

    impl Noted {
        fn new(fail: usize) -> Noted {
            Noted {
                calls: RefCell::new(Vec::new()),
                fail,
            }
        }

        fn start(&self, exe: &Path, flags: &[&str]) -> io::Result<()> {
            let mut calls = self.calls.borrow_mut();
            calls.push((
                exe.to_path_buf(),
                flags.iter().map(|flag| flag.to_string()).collect(),
            ));
            match calls.len() == self.fail {
                true => Err(io::Error::other("would not start")),
                false => Ok(()),
            }
        }

        fn flags(&self) -> Vec<Vec<String>> {
            self.calls
                .borrow()
                .iter()
                .map(|(_, flags)| flags.clone())
                .collect()
        }
    }

    /// A downloaded build at `Downloads\hyalo.exe` holding `new`, and
    /// where it installs, under `Programs\hyalo`, not made.
    fn downloaded(test: &str, new: &[u8]) -> (Scratch, PathBuf, PathBuf) {
        let scratch = Scratch::new(test).with(&["Downloads\\"]);
        let running = scratch.0.join(r"Downloads\hyalo.exe");
        fs::write(&running, new).expect("the downloaded build");
        let installed = scratch.0.join(r"Programs\hyalo\hyalo.exe");
        (scratch, running, installed)
    }

    #[test]
    fn installing_copies_the_build_into_a_new_folder_and_starts_it_as_asked() {
        let (_scratch, running, installed) = downloaded("install-copies", b"new");
        let noted = Noted::new(0);
        let closed = Cell::new(false);
        let start = |exe: &Path, flags: &[&str]| noted.start(exe, flags);
        let done = settle(
            Placement::Install,
            &running,
            &installed,
            Start::Settings,
            &start,
            &|| closed.set(true),
        );
        assert!(done.expect("installed"));
        assert_eq!(fs::read(&installed).expect("installed"), b"new");
        assert_eq!(noted.calls.borrow()[0].0, installed);
        assert_eq!(noted.flags(), [vec!["--settings".to_string()]]);
        assert!(!closed.get(), "nothing to replace");
    }

    #[test]
    fn replacing_swaps_the_build_in_closes_the_old_one_and_starts_it_to_take_over() {
        let (_scratch, running, installed) = downloaded("install-replaces", b"new");
        fs::create_dir_all(installed.parent().expect("a folder")).expect("the folder");
        fs::write(&installed, b"old").expect("the old build");
        let noted = Noted::new(0);
        let closed = Cell::new(false);
        let start = |exe: &Path, flags: &[&str]| {
            assert!(closed.get(), "the old one is asked to end first");
            noted.start(exe, flags)
        };
        let done = settle(
            Placement::Replace,
            &running,
            &installed,
            Start::Shown,
            &start,
            &|| closed.set(true),
        );
        assert!(done.expect("replaced"));
        assert_eq!(fs::read(&installed).expect("the new build"), b"new");
        let old = installed.with_extension("exe.old");
        assert_eq!(fs::read(old).expect("the old build kept"), b"old");
        assert_eq!(noted.flags(), [vec!["--replace".to_string()]]);
    }

    #[test]
    fn a_replacement_that_does_not_start_puts_the_old_build_back_and_starts_it_hidden() {
        let (_scratch, running, installed) = downloaded("install-puts-back", b"new");
        fs::create_dir_all(installed.parent().expect("a folder")).expect("the folder");
        fs::write(&installed, b"old").expect("the old build");
        let noted = Noted::new(1);
        let start = |exe: &Path, flags: &[&str]| noted.start(exe, flags);
        let done = settle(
            Placement::Replace,
            &running,
            &installed,
            Start::Hidden,
            &start,
            &|| {},
        );
        assert!(done.is_err());
        assert_eq!(fs::read(&installed).expect("the old build back"), b"old");
        let flags = noted.flags();
        assert_eq!(flags[0], ["--hidden", "--replace"]);
        assert_eq!(flags[1], ["--hidden"]);
    }

    #[test]
    fn deferring_starts_the_installed_build_and_copies_nothing() {
        let (_scratch, running, installed) = downloaded("install-defers", b"new");
        fs::create_dir_all(installed.parent().expect("a folder")).expect("the folder");
        fs::write(&installed, b"newer").expect("a newer build");
        let noted = Noted::new(0);
        let start = |exe: &Path, flags: &[&str]| noted.start(exe, flags);
        let done = settle(
            Placement::Defer,
            &running,
            &installed,
            Start::Hidden,
            &start,
            &|| {},
        );
        assert!(done.expect("deferred"));
        assert_eq!(fs::read(&installed).expect("untouched"), b"newer");
        assert_eq!(noted.flags(), [vec!["--hidden".to_string()]]);
    }

    #[test]
    fn running_here_touches_nothing() {
        let (_scratch, running, installed) = downloaded("install-here", b"new");
        let noted = Noted::new(0);
        let start = |exe: &Path, flags: &[&str]| noted.start(exe, flags);
        let done = settle(
            Placement::Here,
            &running,
            &installed,
            Start::Shown,
            &start,
            &|| {},
        );
        assert!(!done.expect("nothing to do"));
        assert!(!installed.exists());
        assert!(noted.flags().is_empty());
    }

    #[test]
    fn one_file_is_the_same_whatever_the_case_of_its_letters() {
        let scratch = Scratch::new("install-same").with(&["hyalo.exe", "other.exe"]);
        let exe = scratch.0.join("hyalo.exe");
        let shouted = PathBuf::from(exe.to_string_lossy().to_uppercase());
        assert!(same_file(&exe, &shouted));
        assert!(!same_file(&exe, &scratch.0.join("other.exe")));
        assert!(same_file(
            Path::new(r"C:\Nowhere\A.exe"),
            Path::new(r"c:\nowhere\a.EXE")
        ));
    }

    #[test]
    fn what_is_installed_is_missing_or_there_with_its_version_if_it_says_one() {
        let scratch = Scratch::new("install-at").with(&["hyalo.exe"]);
        assert_eq!(
            installed_at(&scratch.0.join("nothing.exe")),
            Installed::Missing
        );
        assert_eq!(
            installed_at(&scratch.0.join("hyalo.exe")),
            Installed::At(None)
        );
        let exe = std::env::current_exe().expect("the test exe");
        assert_eq!(installed_at(&exe), Installed::At(Some(update::current())));
    }

    #[test]
    fn a_portable_start_and_an_updates_restart_stay_where_they_are() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert!(stays(&args(&["hyalo.exe", "--portable"])));
        assert!(stays(&args(&["hyalo.exe", "--hidden", "--replace"])));
        assert!(!stays(&args(&["hyalo.exe", "--hidden"])));
        assert!(!stays(&args(&["hyalo.exe", "--settings"])));
    }

    #[test]
    fn hyalo_installs_into_programs_under_local_app_data() {
        let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA");
        let expected = PathBuf::from(local).join(r"Programs\hyalo\hyalo.exe");
        assert_eq!(installed_exe(), Some(expected));
    }

    #[test]
    fn the_exe_knows_its_own_version() {
        let exe = std::env::current_exe().expect("the test exe");
        assert_eq!(version_of(&exe), Some(update::current()));
    }

    /// The string `key` of the exe's English (US, Unicode) version strings.
    fn said(exe: &Path, key: &str) -> Option<String> {
        let path = HSTRING::from(exe.as_os_str());
        let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
        let mut data = vec![0_u8; size as usize];
        unsafe { GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()) }.ok()?;
        let query = HSTRING::from(format!("\\StringFileInfo\\040904B0\\{key}"));
        let mut text: *mut u16 = std::ptr::null_mut();
        let mut length = 0_u32;
        let found = unsafe {
            VerQueryValueW(
                data.as_ptr().cast(),
                &query,
                (&raw mut text).cast(),
                &mut length,
            )
        };
        if !found.as_bool() || text.is_null() {
            return None;
        }
        let units = unsafe { std::slice::from_raw_parts(text, length as usize) };
        Some(
            String::from_utf16_lossy(units)
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    #[test]
    fn the_exe_names_itself_hyalo_with_its_version_and_licence() {
        let exe = std::env::current_exe().expect("the test exe");
        assert_eq!(said(&exe, "ProductName").as_deref(), Some("hyalo"));
        assert_eq!(said(&exe, "FileDescription").as_deref(), Some("hyalo"));
        let version = update::current().to_string();
        assert_eq!(said(&exe, "FileVersion"), Some(version));
        let copyright = said(&exe, "LegalCopyright").expect("a copyright");
        assert!(copyright.starts_with("Copyright (c) 2026"), "{copyright}");
    }

    #[test]
    fn a_file_without_a_version_resource_has_no_version() {
        let scratch = Scratch::new("install-no-version").with(&["hyalo.exe"]);
        assert_eq!(version_of(Path::new(&scratch.path("hyalo.exe"))), None);
    }
}
