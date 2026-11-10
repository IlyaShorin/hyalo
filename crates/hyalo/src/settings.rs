//! The config file, `%APPDATA%\hyalo\config.toml`, next to the picks:
//! written with the defaults the first time, read at start, and watched
//! off the UI thread so that what the user saves applies at once.
//! hyalo-core reads the text; this module only moves it.

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fs, io};

use hyalo_core::config::{self, Change, Config, Problem, Refusal, Why, Written};
use windows::Win32::Foundation::{HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::HSTRING;

use crate::store;

/// What reading the config gives: the settings, or what is wrong.
pub type Loaded = Result<Config, Problem>;

/// `%APPDATA%\hyalo\config.toml`.
pub fn default_path() -> Option<PathBuf> {
    store::ours("APPDATA", "config.toml")
}

/// What the file at `path` holds; a file that is not there is written
/// with the defaults first, so there is one to edit. `None` when it
/// cannot be read at all (an editor holding it, say).
pub fn contents(path: &Path) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Err(error) = store::replace(path, config::DEFAULT.as_bytes()) {
                eprintln!("hyalo: cannot write {}: {error}", path.display());
            }
            Some(config::DEFAULT.as_bytes().to_vec())
        }
        Err(error) => {
            eprintln!("hyalo: cannot read {}: {error}", path.display());
            None
        }
    }
}

/// The settings the file's bytes give.
pub fn read(bytes: Vec<u8>) -> Loaded {
    text(bytes).and_then(|text| config::parse(&text))
}

/// The settings as the file's bytes write them, for the settings window.
pub fn written(bytes: Vec<u8>) -> Result<Written, Problem> {
    text(bytes).and_then(|text| config::written(&text))
}

/// The file's bytes as text; text that is not UTF-8 is a problem on the
/// line where it stops being UTF-8.
fn text(bytes: Vec<u8>) -> Result<String, Problem> {
    String::from_utf8(bytes).map_err(|error| {
        let valid = &error.as_bytes()[..error.utf8_error().valid_up_to()];
        Problem {
            line: valid.iter().filter(|&&byte| byte == b'\n').count() + 1,
            why: Why::NotUtf8,
        }
    })
}

/// The text the file at `path` holds now, read afresh (a file that is not
/// there is written with the defaults first), with `change` made in it;
/// `None` when the change makes no difference. Nothing is written.
pub fn edited(path: &Path, change: &Change) -> Result<Option<String>, Refusal> {
    let unreadable = || Refusal::Wrong(Why::Unreadable);
    let text = text(contents(path).ok_or_else(unreadable)?).map_err(Refusal::Broken)?;
    let edited = config::edit(&text, change)?;
    Ok((edited != text).then_some(edited))
}

/// Puts `text` in the file at `path` whole or not at all, so the watcher
/// never reads half of it.
pub fn write(path: &Path, text: &str) -> io::Result<()> {
    store::replace(path, text.as_bytes())
}

/// How long after a change the file is read: an editor may write it in
/// more than one go.
const SETTLE: Duration = Duration::from_millis(100);

/// How often the watcher asks whether anyone still listens.
const LISTENING: u32 = 500;

/// Watches the folder of `path` on a thread of its own and hands over
/// what the file holds each time it holds something other than what was
/// `seen` last, until `listening` says no one listens. What was saved
/// after `seen` was read and before the watching began is handed over at
/// once.
pub fn watch(
    path: PathBuf,
    seen: Option<Vec<u8>>,
    listening: impl Fn() -> bool + Send + 'static,
    report: impl Fn(Vec<u8>) + Send + 'static,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(folder) = path.parent() else {
            return;
        };
        let changes = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE;
        // SAFETY: the handle is ours, and closed once when the watch ends.
        let Ok(handle) =
            (unsafe { FindFirstChangeNotificationW(&HSTRING::from(folder), false, changes) })
        else {
            return;
        };
        let mut seen = seen;
        let mut changed = true;
        while changed && listening() {
            if let Some(now) = fs::read(&path)
                .ok()
                .filter(|now| seen.as_ref() != Some(now))
            {
                seen = Some(now.clone());
                report(now);
            }
            changed = next_change(handle, &listening);
        }
        // SAFETY: see above.
        let _ = unsafe { FindCloseChangeNotification(handle) };
    })
}

/// Waits for the next change in the folder `handle` watches, or for no
/// one to listen, and a moment more for a change to settle; `false` when
/// the folder can no longer be watched (it is gone, say).
fn next_change(handle: HANDLE, listening: &impl Fn() -> bool) -> bool {
    // SAFETY: the handle is open for as long as the watch runs.
    unsafe {
        if FindNextChangeNotification(handle).is_err() {
            return false;
        }
        while listening() && WaitForSingleObject(handle, LISTENING) == WAIT_TIMEOUT {}
    }
    thread::sleep(SETTLE);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use hyalo_core::config::List;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    fn config_in(scratch: &Scratch) -> PathBuf {
        scratch.0.join("hyalo").join("config.toml")
    }

    fn hotkey(loaded: &Loaded) -> String {
        loaded.as_ref().expect("settings").hotkey.to_string()
    }

    fn load(path: &Path) -> Option<Loaded> {
        contents(path).map(read)
    }

    #[test]
    fn a_missing_file_is_written_with_the_defaults() {
        let scratch = Scratch::new("settings-missing");
        let path = config_in(&scratch);
        assert_eq!(load(&path), Some(Ok(Config::default())));
        let written = fs::read_to_string(&path).expect("written");
        assert_eq!(written, config::DEFAULT);
    }

    #[test]
    fn a_file_that_is_there_is_read_and_left_as_it_is() {
        let scratch = Scratch::new("settings-there").with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, "hotkey = \"Ctrl+K\"\n").expect("write");
        assert_eq!(hotkey(&load(&path).expect("read")), "Ctrl+K");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "hotkey = \"Ctrl+K\"\n"
        );
    }

    #[test]
    fn a_broken_file_gives_its_problem() {
        let scratch = Scratch::new("settings-broken").with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, "\nhotkye = \"Ctrl+K\"\n").expect("write");
        let problem = load(&path).expect("read").expect_err("a problem");
        assert_eq!(problem.line, 2);
    }

    #[test]
    fn text_that_is_not_utf8_is_a_problem_on_its_line() {
        let scratch = Scratch::new("settings-utf8").with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, b"hotkey = \"Alt+Space\"\n# \xC0\xEB\xFC\n").expect("write");
        let problem = load(&path).expect("read").expect_err("a problem");
        assert_eq!(problem.line, 2);
        assert!(problem.why == Why::NotUtf8, "{}", problem.why);
    }

    #[test]
    fn a_file_that_cannot_be_read_gives_nothing() {
        let scratch = Scratch::new("settings-folder").with(&["hyalo\\config.toml\\"]);
        assert_eq!(load(&config_in(&scratch)), None);
    }

    fn change(list: List, entry: &str) -> Change {
        Change::Add(list, entry.to_string())
    }

    #[test]
    fn a_change_is_made_in_the_text_the_file_holds_now() {
        let scratch = Scratch::new("settings-edit").with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, "# mine\nhotkey = 'F4'\n").expect("write");
        let edited = edited(&path, &change(List::RankHigher, "notes")).expect("edited");
        let text = edited.expect("a change");
        let expected = "# mine\nhotkey = 'F4'\n\n[folders]\nrank_higher = ['notes']\n";
        assert_eq!(text, expected);
        let before = fs::read_to_string(&path).expect("read");
        assert_eq!(before, "# mine\nhotkey = 'F4'\n", "not written yet");
        write(&path, &text).expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), text);
        let names: Vec<_> = fs::read_dir(scratch.0.join("hyalo"))
            .expect("the folder")
            .collect();
        assert_eq!(names.len(), 1, "no temporary file left");
    }

    #[test]
    fn a_change_to_a_missing_file_is_made_in_the_defaults() {
        let scratch = Scratch::new("settings-edit-missing");
        let path = config_in(&scratch);
        let edited = edited(&path, &change(List::Exclude, r"D:\old")).expect("edited");
        let text = edited.expect("a change");
        assert!(text.starts_with("# hyalo's settings."), "{text}");
        assert!(
            text.contains(r"exclude = ['C:\Windows\WinSxS', 'D:\old']"),
            "{text}"
        );
        assert_eq!(fs::read_to_string(&path).expect("written"), config::DEFAULT);
    }

    #[test]
    fn a_change_that_changes_nothing_is_not_there_to_write() {
        let (_scratch, path) = configured("settings-edit-same");
        assert_eq!(edited(&path, &change(List::RankLower, "TARGET")), Ok(None));
    }

    #[test]
    fn a_broken_file_is_not_changed_and_says_why() {
        let scratch = Scratch::new("settings-edit-broken").with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, "\nhotkye = 'F4'\n").expect("write");
        let refused = edited(&path, &change(List::RankLower, "x")).expect_err("refused");
        assert!(
            matches!(refused, Refusal::Broken(Problem { line: 2, .. })),
            "{refused:?}"
        );
        fs::write(&path, b"# \xC0\n").expect("write");
        let refused = edited(&path, &change(List::RankLower, "x")).expect_err("refused");
        assert!(
            matches!(refused, Refusal::Broken(Problem { line: 1, .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_changed() {
        let scratch = Scratch::new("settings-edit-folder").with(&["hyalo\\config.toml\\"]);
        let refused = edited(&config_in(&scratch), &change(List::RankLower, "x"));
        assert_eq!(refused, Err(Refusal::Wrong(Why::Unreadable)));
    }

    #[test]
    fn the_settings_as_written_come_from_the_files_bytes() {
        let shown = written(b"hotkey = 'F4'\n".to_vec()).expect("good settings");
        assert_eq!(shown.hotkey.to_string(), "F4");
        assert_eq!(shown.list(List::RankLower), ["target"]);
        let broken = written(b"# \xC0\n".to_vec()).expect_err("not UTF-8");
        assert_eq!(broken.line, 1);
    }

    #[test]
    fn the_file_lives_in_roaming_app_data_next_to_the_picks() {
        let picks = store::default_path().expect("picks");
        let config = default_path().expect("config");
        assert_eq!(config.parent(), picks.parent());
        assert_eq!(config.file_name().expect("a name"), "config.toml");
    }

    /// A watcher on `path`, which has seen what the file holds now, whose
    /// settings come out of the receiver; it watches until the flag is
    /// lowered.
    fn watching(path: &Path) -> (Receiver<Loaded>, Arc<AtomicBool>, JoinHandle<()>) {
        watching_since(path, fs::read(path).ok())
    }

    fn watching_since(
        path: &Path,
        seen: Option<Vec<u8>>,
    ) -> (Receiver<Loaded>, Arc<AtomicBool>, JoinHandle<()>) {
        let (sender, receiver) = channel();
        let listening = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&listening);
        let watcher = watch(
            path.to_path_buf(),
            seen,
            move || flag.load(Ordering::SeqCst),
            move |bytes| sender.send(read(bytes)).expect("the test listens"),
        );
        (receiver, listening, watcher)
    }

    /// A folder with the default config in it.
    fn configured(test: &str) -> (Scratch, PathBuf) {
        let scratch = Scratch::new(test).with(&["hyalo\\"]);
        let path = config_in(&scratch);
        fs::write(&path, config::DEFAULT).expect("write");
        (scratch, path)
    }

    /// Lowers the flag and waits for the watcher to stop.
    fn stop(listening: &AtomicBool, watcher: JoinHandle<()>) {
        listening.store(false, Ordering::SeqCst);
        watcher.join().expect("the watcher");
    }

    #[test]
    fn a_change_saved_is_handed_over() {
        let (_scratch, path) = configured("settings-watch");
        let (settings, listening, watcher) = watching(&path);
        // Let the watcher start watching.
        thread::sleep(Duration::from_millis(200));
        fs::write(&path, "hotkey = \"Ctrl+Shift+K\"\n").expect("write");
        let loaded = settings
            .recv_timeout(Duration::from_secs(5))
            .expect("handed over");
        assert_eq!(hotkey(&loaded), "Ctrl+Shift+K");
        fs::write(&path, "hotkey = \"Ctrl+\"\n").expect("write");
        let loaded = settings
            .recv_timeout(Duration::from_secs(5))
            .expect("handed over");
        assert!(loaded.is_err(), "a problem is handed over too");
        stop(&listening, watcher);
    }

    #[test]
    fn the_same_text_saved_again_or_a_file_beside_it_is_not_handed_over() {
        let (scratch, path) = configured("settings-same");
        let (settings, listening, watcher) = watching(&path);
        thread::sleep(Duration::from_millis(200));
        fs::write(&path, config::DEFAULT).expect("write");
        fs::write(scratch.0.join("hyalo").join("frecency.json"), "{}").expect("write");
        let nothing = settings.recv_timeout(Duration::from_millis(600));
        assert!(nothing.is_err(), "{nothing:?}");
        stop(&listening, watcher);
    }

    #[test]
    fn a_file_saved_by_renaming_a_new_one_over_it_is_handed_over() {
        let (scratch, path) = configured("settings-rename");
        let (settings, listening, watcher) = watching(&path);
        thread::sleep(Duration::from_millis(200));
        let fresh = scratch.0.join("hyalo").join("config.toml.new");
        fs::write(&fresh, "hotkey = \"Win+F2\"\n").expect("write");
        fs::rename(&fresh, &path).expect("rename over");
        let loaded = settings
            .recv_timeout(Duration::from_secs(5))
            .expect("handed over");
        assert_eq!(hotkey(&loaded), "Win+F2");
        stop(&listening, watcher);
    }

    #[test]
    fn what_was_saved_before_the_watching_began_is_handed_over_at_once() {
        let (_scratch, path) = configured("settings-before");
        let seen = fs::read(&path).ok();
        fs::write(&path, "hotkey = \"Win+F3\"\n").expect("write");
        let (settings, listening, watcher) = watching_since(&path, seen);
        let loaded = settings
            .recv_timeout(Duration::from_secs(5))
            .expect("handed over");
        assert_eq!(hotkey(&loaded), "Win+F3");
        stop(&listening, watcher);
    }

    #[test]
    fn stops_once_its_folder_is_gone_though_someone_listens() {
        let (scratch, path) = configured("settings-gone");
        let (_settings, _listening, watcher) = watching(&path);
        thread::sleep(Duration::from_millis(200));
        fs::remove_dir_all(scratch.0.join("hyalo")).expect("remove the folder");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !watcher.is_finished() {
            assert!(
                Instant::now() < deadline,
                "still watching a folder that is gone"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn stops_soon_once_no_one_listens() {
        let (_scratch, path) = configured("settings-stop");
        let (_settings, listening, watcher) = watching(&path);
        let started = Instant::now();
        stop(&listening, watcher);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_folder_that_is_not_there_is_not_watched() {
        let scratch = Scratch::new("settings-nowhere");
        let path = scratch.0.join("nowhere").join("config.toml");
        let (_settings, _listening, watcher) = watching(&path);
        watcher.join().expect("the watcher ends at once");
    }
}
