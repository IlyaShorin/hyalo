//! Starting with Windows: the `hyalo` value under the current user's Run
//! key names this executable, with `--hidden` so it starts in the tray.
//! Windows' own switch for startup apps (Settings, Task Manager) keeps its
//! mark under `StartupApproved\Run`; a value whose mark says off does not
//! start, and the menu shows it unchecked.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, Result};

/// Where Windows looks for what to start at logon, under the current
/// user's key.
pub const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Where Windows keeps whether each of those may start.
pub const APPROVED: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// The value's name in both keys.
const NAME: &str = "hyalo";

/// The argument that starts the launcher hidden, in the tray.
pub const HIDDEN: &str = "--hidden";

/// Whether to show the launcher at start, by the command line's
/// arguments: not when started with Windows.
pub fn shown(arguments: impl IntoIterator<Item = String>) -> bool {
    !arguments.into_iter().any(|argument| argument == HIDDEN)
}

/// Whether, and how, this executable starts with Windows.
pub struct Autostart {
    /// The Run key and the StartupApproved key, under the current user's.
    run: String,
    approved: String,
    exe: PathBuf,
}

impl Autostart {
    /// For this executable with Windows' keys; `None` for a debug build,
    /// which lives in `target\debug` and must not start with Windows.
    pub fn real() -> Option<Autostart> {
        if cfg!(debug_assertions) {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        Some(Autostart::new(RUN, APPROVED, exe))
    }

    /// For `exe` with the keys `run` and `approved` under the current
    /// user's.
    pub fn new(run: &str, approved: &str, exe: PathBuf) -> Autostart {
        Autostart {
            run: run.to_string(),
            approved: approved.to_string(),
            exe,
        }
    }

    /// Whether Windows starts this executable at logon.
    pub fn starts(&self) -> bool {
        let run = read(&self.run, RRF_RT_REG_SZ).map(|units| text(&units));
        let approved = read(&self.approved, RRF_RT_REG_BINARY).map(|bytes| bytes.to_vec());
        starts(run.as_deref(), approved.as_deref(), &self.exe)
    }

    /// Starts with Windows, or stops. Asked for here, starting also lifts
    /// a mark that Windows' switch set to off; stopping leaves neither
    /// value behind.
    pub fn set(&self, on: bool) -> Result<()> {
        if on {
            write(&self.run, &command(&self.exe))?;
        } else {
            delete(&self.run)?;
        }
        delete(&self.approved)
    }
}

/// The Run value for `exe`: its path in quotes, then `--hidden`.
fn command(exe: &Path) -> String {
    format!("\"{}\" {HIDDEN}", exe.display())
}

/// Whether Windows starts `exe` at logon, by the Run value and the mark
/// Windows keeps for it: the value must be this executable's command,
/// letters in any case, as paths compare; a mark whose first byte is odd
/// means off, and no mark means on.
fn starts(run: Option<&str>, approved: Option<&[u8]>, exe: &Path) -> bool {
    let ours = run.is_some_and(|run| run.to_lowercase() == command(exe).to_lowercase());
    let off = approved
        .and_then(|mark| mark.first())
        .is_some_and(|first| first % 2 == 1);
    ours && !off
}

/// The value `NAME` under the current user's `key`, as `kind` reads it,
/// in bytes; `None` when there is none.
fn read(key: &str, kind: REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(NAME));
    let mut size = 0_u32;
    // SAFETY: the first call only measures; the second writes at most
    // `size` bytes into a buffer of that size.
    unsafe {
        let measured = RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            kind,
            None,
            None,
            Some(&mut size),
        );
        measured.ok().ok()?;
        let mut bytes = vec![0_u8; size as usize];
        let data = Some(bytes.as_mut_ptr().cast());
        let read = RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            kind,
            None,
            data,
            Some(&mut size),
        );
        read.ok().ok()?;
        bytes.truncate(size as usize);
        Some(bytes)
    }
}

/// A registry string's bytes as text, without the NUL at its end.
fn text(bytes: &[u8]) -> String {
    let pairs = bytes.as_chunks::<2>().0.iter();
    let units: Vec<u16> = pairs.map(|&pair| u16::from_le_bytes(pair)).collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

/// Writes `value` as the string `NAME` under the current user's `key`,
/// making the key if it is not there.
fn write(key: &str, value: &str) -> Result<()> {
    let mut units: Vec<u16> = value.encode_utf16().collect();
    units.push(0);
    let size = (units.len() * 2) as u32;
    // SAFETY: the data is `size` bytes long and outlives the call.
    let written = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(NAME),
            REG_SZ.0,
            Some(units.as_ptr().cast()),
            size,
        )
    };
    written.ok()
}

/// Deletes the value `NAME` under the current user's `key`; one that is
/// not there is gone already.
fn delete(key: &str) -> Result<()> {
    // SAFETY: a plain call with strings that outlive it.
    let deleted =
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(NAME)) };
    gone(deleted)
}

fn gone(status: WIN32_ERROR) -> Result<()> {
    match status {
        ERROR_FILE_NOT_FOUND => Ok(()),
        status => status.ok(),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use windows::Win32::System::Registry::{REG_BINARY, RegDeleteTreeW};

    /// Keys of their own for one test, under `Software\hyalo-test-<pid>-<test>`
    /// in the current user's key, deleted afterwards. Never Windows' own.
    pub struct Keys {
        root: String,
        pub run: String,
        pub approved: String,
    }

    impl Keys {
        pub fn new(test: &str) -> Keys {
            let root = format!(r"Software\hyalo-test-{}-{test}", std::process::id());
            Keys {
                run: format!(r"{root}\Run"),
                approved: format!(r"{root}\StartupApproved\Run"),
                root,
            }
        }

        pub fn autostart(&self, exe: &str) -> Autostart {
            Autostart::new(&self.run, &self.approved, PathBuf::from(exe))
        }

        /// The Run value, if there is one.
        pub fn run_value(&self) -> Option<String> {
            read(&self.run, RRF_RT_REG_SZ).map(|bytes| text(&bytes))
        }

        /// Sets the mark Windows' switch keeps.
        fn mark(&self, bytes: &[u8]) {
            let written = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(&self.approved),
                    &HSTRING::from(NAME),
                    REG_BINARY.0,
                    Some(bytes.as_ptr().cast()),
                    bytes.len() as u32,
                )
            };
            written.ok().expect("mark");
        }

        fn marked(&self) -> bool {
            read(&self.approved, RRF_RT_REG_BINARY).is_some()
        }
    }

    impl Drop for Keys {
        fn drop(&mut self) {
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(&self.root)) };
            let _ = unsafe {
                windows::Win32::System::Registry::RegDeleteKeyW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(&self.root),
                )
            };
        }
    }

    const EXE: &str = r"C:\Users\Me\AppData\Local\Programs\hyalo\hyalo.exe";

    /// Off, as Windows' switch writes it: 3, then when, as a FILETIME.
    const OFF: [u8; 12] = [3, 0, 0, 0, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x01];
    /// On, as Windows' switch writes it.
    const ON: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    #[test]
    fn the_run_value_names_the_exe_in_quotes_and_starts_it_hidden() {
        let exe = Path::new(r"C:\Program Files\x y\hyalo.exe");
        assert_eq!(command(exe), r#""C:\Program Files\x y\hyalo.exe" --hidden"#);
    }

    #[test]
    fn starts_only_the_exe_the_value_names() {
        let exe = Path::new(EXE);
        let ours = command(exe);
        assert!(starts(Some(&ours), None, exe));
        assert!(starts(Some(&ours.to_uppercase()), None, exe), "any case");
        assert!(!starts(None, None, exe));
        let elsewhere = command(Path::new(r"D:\old\hyalo.exe"));
        assert!(!starts(Some(&elsewhere), None, exe));
        assert!(!starts(Some(&format!("{ours} --other")), None, exe));
    }

    #[test]
    fn windows_switch_set_to_off_keeps_it_from_starting() {
        let exe = Path::new(EXE);
        let ours = command(exe);
        assert!(!starts(Some(&ours), Some(&OFF), exe));
        assert!(!starts(Some(&ours), Some(&[7, 0, 0, 0]), exe));
        assert!(starts(Some(&ours), Some(&ON), exe));
        assert!(starts(Some(&ours), Some(&[6, 0, 0, 0]), exe));
        assert!(starts(Some(&ours), Some(&[]), exe), "a mark of nothing");
    }

    #[test]
    fn started_with_windows_it_is_not_shown() {
        let arguments =
            |list: &[&str]| list.iter().map(|text| text.to_string()).collect::<Vec<_>>();
        assert!(shown(arguments(&["hyalo.exe"])));
        assert!(!shown(arguments(&["hyalo.exe", "--hidden"])));
        assert!(shown(arguments(&["hyalo.exe", "--hide"])));
    }

    #[test]
    fn nothing_written_does_not_start() {
        let keys = Keys::new("nothing");
        assert!(!keys.autostart(EXE).starts());
        assert_eq!(keys.run_value(), None);
    }

    #[test]
    fn turning_it_on_writes_the_run_value_and_off_deletes_it() {
        let keys = Keys::new("on-off");
        let autostart = keys.autostart(EXE);
        autostart.set(true).expect("on");
        assert_eq!(keys.run_value(), Some(command(Path::new(EXE))));
        assert!(autostart.starts());
        autostart.set(true).expect("on again");
        assert!(autostart.starts());
        autostart.set(false).expect("off");
        assert_eq!(keys.run_value(), None);
        assert!(!autostart.starts());
        autostart.set(false).expect("off again");
    }

    #[test]
    fn turning_it_on_here_lifts_windows_switch() {
        let keys = Keys::new("switch");
        let autostart = keys.autostart(EXE);
        autostart.set(true).expect("on");
        keys.mark(&OFF);
        assert!(!autostart.starts(), "switched off in Windows");
        autostart.set(true).expect("on");
        assert!(autostart.starts());
        assert!(!keys.marked());
    }

    #[test]
    fn turning_it_off_leaves_no_mark_behind() {
        let keys = Keys::new("tidy");
        let autostart = keys.autostart(EXE);
        autostart.set(true).expect("on");
        keys.mark(&ON);
        autostart.set(false).expect("off");
        assert!(!keys.marked());
        assert_eq!(keys.run_value(), None);
    }

    #[test]
    fn a_value_for_another_copy_is_taken_over_when_turned_on() {
        let keys = Keys::new("other");
        keys.autostart(r"D:\old\hyalo.exe")
            .set(true)
            .expect("the other copy");
        let autostart = keys.autostart(EXE);
        assert!(!autostart.starts());
        autostart.set(true).expect("on");
        assert!(autostart.starts());
    }

    #[test]
    fn a_debug_build_never_starts_with_windows() {
        assert!(Autostart::real().is_none());
    }
}
