//! Where picks live between runs: `%APPDATA%\hyalo\frecency.json`.
//!
//! hyalo-core owns the format; this module only moves the text to and
//! from disk.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{fs, io};

use hyalo_core::frecency::Frecency;

/// `%APPDATA%\hyalo\frecency.json`, so picks follow a roaming profile.
pub fn default_path() -> Option<PathBuf> {
    let app_data = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(app_data).join("hyalo").join("frecency.json"))
}

/// The picks saved at `path`, or none if nothing could be read. A file
/// that is there but is not picks this version can read is moved aside
/// to `<path>.bad`, so the next save does not destroy it.
pub fn load(path: &Path) -> Frecency {
    let Ok(json) = fs::read_to_string(path) else {
        return Frecency::default();
    };
    Frecency::from_json(&json).unwrap_or_else(|error| {
        eprintln!("hyalo: cannot read {}: {error:?}", path.display());
        let _ = fs::rename(path, beside(path, ".bad"));
        Frecency::default()
    })
}

/// Writes the picks whole or not at all: into a temporary file beside
/// `path`, then renamed over it. Creates the folder the first time.
pub fn save(path: &Path, frecency: &Frecency) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let temporary = beside(path, ".tmp");
    fs::write(&temporary, frecency.to_json())?;
    fs::rename(&temporary, path)
}

/// `path` with `suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(suffix);
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::frecency::Timestamp;
    use hyalo_core::item::ItemId;

    /// A fresh folder for one test, removed when the test is done.
    struct Folder(PathBuf);

    impl Folder {
        fn new(test: &str) -> Self {
            let path = std::env::temp_dir().join(format!("hyalo-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("hyalo").join("frecency.json")
        }

        fn names(&self) -> Vec<String> {
            let entries = fs::read_dir(self.0.join("hyalo")).expect("the folder exists");
            let mut names: Vec<String> = entries
                .map(|entry| {
                    entry
                        .expect("an entry")
                        .file_name()
                        .to_string_lossy()
                        .into()
                })
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn picked(query: &str) -> Frecency {
        let mut frecency = Frecency::default();
        frecency.record(&ItemId("notepad".into()), query, Timestamp(1_700_000_000));
        frecency
    }

    #[test]
    fn nothing_saved_yet_loads_as_no_picks() {
        let folder = Folder::new("nothing-saved");
        assert_eq!(load(&folder.file()), Frecency::default());
    }

    #[test]
    fn saves_into_a_new_folder_and_loads_back() {
        let folder = Folder::new("round-trip");
        save(&folder.file(), &picked("note")).expect("save");
        assert_eq!(load(&folder.file()), picked("note"));
    }

    #[test]
    fn saving_again_replaces_the_picks_and_leaves_one_file() {
        let folder = Folder::new("replace");
        save(&folder.file(), &picked("note")).expect("save");
        save(&folder.file(), &picked("pad")).expect("save again");
        assert_eq!(load(&folder.file()), picked("pad"));
        assert_eq!(folder.names(), ["frecency.json"]);
    }

    #[test]
    fn a_file_it_cannot_read_is_moved_aside_not_lost() {
        let folder = Folder::new("broken");
        fs::create_dir_all(folder.file().parent().expect("a parent")).expect("mkdir");
        fs::write(folder.file(), "{ broken").expect("write");
        assert_eq!(load(&folder.file()), Frecency::default());
        assert_eq!(folder.names(), ["frecency.json.bad"]);
        let kept = fs::read_to_string(folder.0.join("hyalo").join("frecency.json.bad"));
        assert_eq!(kept.expect("kept"), "{ broken");
    }

    #[test]
    fn the_file_lives_in_roaming_app_data() {
        let app_data = std::env::var_os("APPDATA").expect("APPDATA is set on Windows");
        let expected = PathBuf::from(app_data).join("hyalo").join("frecency.json");
        assert_eq!(default_path(), Some(expected));
    }
}
