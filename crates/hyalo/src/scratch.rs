//! Folders of files made for one test and removed after it.

use std::fs;
use std::path::PathBuf;

/// A folder of its own for one test, in the temporary folder, removed
/// when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("hyalo-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a scratch folder");
        Self(path)
    }

    /// Makes the files and folders given, folders ending in a backslash.
    pub fn with(self, paths: &[&str]) -> Self {
        for path in paths {
            let full = self.0.join(path);
            match path.ends_with('\\') {
                true => fs::create_dir_all(&full),
                false => fs::write(&full, b"x"),
            }
            .expect("make a scratch entry");
        }
        self
    }

    /// The full path of `path` inside.
    pub fn path(&self, path: &str) -> String {
        self.0.join(path).to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
