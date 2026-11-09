//! Where a file or folder lives, in points: under the user's own folders
//! a little up, under tool output, caches and the system down.

use crate::matcher;

/// Points for living under one of the user's folders (Desktop, Documents,
/// Downloads, projects). Small: it orders files that match equally, but an
/// app still outranks a file that matches one kind better (a kind of match
/// is 20 points, an app's kind 24 more than a file's).
pub const BOOST: f64 = 3.0;

/// Points lost for living under tool output, a cache or the system: half a
/// kind of match, so a better kind of match still wins.
pub const PENALTY: f64 = 10.0;

/// Folders of tool output, caches and the system, by folded name: what
/// lives under one, at any depth, ranks lower.
const BURIED: [&str; 8] = [
    "node_modules",
    ".git",
    "appdata",
    "cache",
    ".cache",
    "caches",
    "__pycache__",
    "windows",
];

/// The user's folders, whose contents rank a little higher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Places {
    /// Each a path from the drive down, every name folded.
    favoured: Vec<Vec<String>>,
}

impl Places {
    /// `favoured` are full paths, such as `C:\Users\me\Desktop`.
    pub fn new(favoured: &[&str]) -> Self {
        let split = |path: &&str| {
            let folded = matcher::folded(path);
            let names = folded.split('\\').filter(|name| !name.is_empty());
            names.map(String::from).collect()
        };
        Self {
            favoured: favoured.iter().map(split).collect(),
        }
    }

    /// Points for something in the folder whose path from the drive down
    /// is `folders`, each name folded as `matcher::folded` folds it.
    pub fn points(&self, folders: &[&str]) -> f64 {
        let under = |place: &Vec<String>| {
            let mut pairs = place.iter().zip(folders);
            place.len() <= folders.len() && pairs.all(|(one, other)| one == other)
        };
        let boost = match self.favoured.iter().any(under) {
            true => BOOST,
            false => 0.0,
        };
        let penalty = match folders.iter().any(|folder| BURIED.contains(folder)) {
            true => PENALTY,
            false => 0.0,
        };
        boost - penalty
    }

    /// Points for the file or folder at the full path `path`.
    pub fn of_path(&self, path: &str) -> f64 {
        let folded = matcher::folded(path);
        let mut folders: Vec<&str> = folded.split('\\').collect();
        folders.pop();
        self.points(&folders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop() -> Places {
        Places::new(&[r"C:\Users\Me\Desktop"])
    }

    #[test]
    fn anything_under_a_favoured_folder_gains_the_boost() {
        assert_eq!(desktop().points(&["c:", "users", "me", "desktop"]), BOOST);
        let deeper = ["c:", "users", "me", "desktop", "notes"];
        assert_eq!(desktop().points(&deeper), BOOST);
    }

    #[test]
    fn a_folder_beside_or_above_a_favoured_one_gains_nothing() {
        assert_eq!(desktop().points(&["c:", "users", "me"]), 0.0);
        let beside = ["c:", "users", "me", "desktop2"];
        assert_eq!(desktop().points(&beside), 0.0);
    }

    #[test]
    fn a_favoured_path_compares_folded_and_may_end_in_a_backslash() {
        let places = Places::new(&[r"c:\USERS\me\Ёлка\"]);
        assert_eq!(places.points(&["c:", "users", "me", "елка"]), BOOST);
    }

    #[test]
    fn tool_output_caches_and_the_system_lose_points_at_any_depth() {
        let buried = [
            "node_modules",
            ".git",
            "appdata",
            "cache",
            ".cache",
            "caches",
            "__pycache__",
            "windows",
        ];
        for folder in buried {
            let folders = ["d:", "work", folder, "lib"];
            assert_eq!(Places::default().points(&folders), -PENALTY, "{folder}");
        }
    }

    #[test]
    fn a_buried_folder_under_a_favoured_one_still_loses() {
        let folders = ["c:", "users", "me", "desktop", "app", "node_modules"];
        assert_eq!(desktop().points(&folders), BOOST - PENALTY);
    }

    #[test]
    fn a_path_counts_the_folders_it_is_in_not_its_own_name() {
        let places = desktop();
        assert_eq!(places.of_path(r"C:\Users\Me\Desktop\notes.txt"), BOOST);
        assert_eq!(places.of_path(r"C:\Users\Me\Desktop"), 0.0);
        assert_eq!(places.of_path(r"D:\AppData"), 0.0);
        assert_eq!(places.of_path(r"D:\AppData\x.txt"), -PENALTY);
    }
}
