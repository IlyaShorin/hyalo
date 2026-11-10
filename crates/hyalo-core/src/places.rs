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
/// lives under one, at any depth, ranks lower. So does what lives under
/// a folder whose name starts with a dot (`.git`, `.cache`, `.vscode`):
/// tools keep their state there.
const BURIED: [&str; 6] = [
    "node_modules",
    "appdata",
    "cache",
    "caches",
    "__pycache__",
    "windows",
];

/// A folder whose contents rank higher or lower: a name, wherever a
/// folder of that name is (`target`), or a full path (`D:\old`). Names
/// compare folded, as `matcher::folded` folds them.
#[derive(Debug, Clone, PartialEq)]
pub struct Place(Where);

#[derive(Debug, Clone, PartialEq)]
enum Where {
    /// A folder name, folded.
    Named(String),
    /// A path from the drive down, every name folded.
    Under(Vec<String>),
}

impl Place {
    /// `text` as a place: a folder name with no backslash, or a full path
    /// from a drive (`C:\` and on), with backslashes or slashes. `None`
    /// for anything else: an empty name, a path from no drive.
    pub fn parse(text: &str) -> Option<Place> {
        let text = text.trim().replace('/', "\\");
        match (full_path(&text), text.contains(['\\', ':'])) {
            (true, _) => Some(Place::under(&text)),
            (false, false) if !text.is_empty() => Some(Place(Where::Named(matcher::folded(&text)))),
            _ => None,
        }
    }

    /// The folder at the full path `path`.
    fn under(path: &str) -> Place {
        let folded = matcher::folded(path);
        let names = folded.split('\\').filter(|name| !name.is_empty());
        Place(Where::Under(names.map(String::from).collect()))
    }

    /// Whether something in the folder whose folded path from the drive
    /// down is `folders` lives in this place.
    fn holds(&self, folders: &[&str]) -> bool {
        match &self.0 {
            Where::Named(name) => folders.contains(&name.as_str()),
            Where::Under(path) => {
                let mut pairs = path.iter().zip(folders);
                path.len() <= folders.len() && pairs.all(|(one, other)| one == other)
            }
        }
    }
}

/// Whether `text` starts like a full path on a drive: `C:\`.
pub fn full_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\"
}

/// The places whose contents rank higher, and lower.
#[derive(Debug, Clone, PartialEq)]
pub struct Places {
    favoured: Vec<Place>,
    buried: Vec<Place>,
}

impl Default for Places {
    /// No favoured folders; tool output, caches and the system buried.
    fn default() -> Self {
        let named = |name: &&str| Place(Where::Named(name.to_string()));
        Self {
            favoured: Vec::new(),
            buried: BURIED.iter().map(named).collect(),
        }
    }
}

impl Places {
    /// `favoured` are full paths, such as `C:\Users\me\Desktop`.
    pub fn new(favoured: &[&str]) -> Self {
        Self {
            favoured: favoured.iter().map(|path| Place::under(path)).collect(),
            ..Self::default()
        }
    }

    /// These places with `higher` favoured too and `lower` buried too.
    pub fn with(mut self, higher: &[Place], lower: &[Place]) -> Self {
        self.favoured.extend_from_slice(higher);
        self.buried.extend_from_slice(lower);
        self
    }

    /// Points for something in the folder whose path from the drive down
    /// is `folders`, each name folded as `matcher::folded` folds it.
    pub fn points(&self, folders: &[&str]) -> f64 {
        let holds = |place: &Place| place.holds(folders);
        let boost = match self.favoured.iter().any(holds) {
            true => BOOST,
            false => 0.0,
        };
        let dotted = folders.iter().any(|folder| folder.starts_with('.'));
        let penalty = match dotted || self.buried.iter().any(holds) {
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
    fn a_folder_whose_name_starts_with_a_dot_holds_tool_state() {
        for folder in [".fingerprint", ".vscode", ".metrics"] {
            let folders = ["c:", "code", "app", folder];
            assert_eq!(Places::default().points(&folders), -PENALTY, "{folder}");
        }
        let named = ["c:", "work", "v1.2", "docs.old"];
        assert_eq!(Places::default().points(&named), 0.0, "a dot inside a name");
    }

    #[test]
    fn a_buried_folder_under_a_favoured_one_still_loses() {
        let folders = ["c:", "users", "me", "desktop", "app", "node_modules"];
        assert_eq!(desktop().points(&folders), BOOST - PENALTY);
    }

    fn place(text: &str) -> Place {
        Place::parse(text).expect("a place")
    }

    #[test]
    fn a_name_is_a_place_wherever_a_folder_has_it() {
        let target = place("Target");
        assert!(target.holds(&["c:", "code", "app", "target"]));
        assert!(target.holds(&["c:", "target", "debug", "deps"]));
        assert!(!target.holds(&["c:", "code", "targets"]));
        assert!(!target.holds(&[]));
        assert!(place("ЁЛКА").holds(&["d:", "елка"]), "folded");
    }

    #[test]
    fn a_full_path_is_a_place_from_its_drive_down() {
        let old = place(r"D:\Old\");
        assert!(old.holds(&["d:", "old"]));
        assert!(old.holds(&["d:", "old", "photos"]));
        assert!(!old.holds(&["c:", "old"]));
        assert!(!old.holds(&["d:", "older"]));
        assert!(!old.holds(&["d:"]));
        assert_eq!(place("d:/old/"), old, "slashes as backslashes");
        assert!(place(r"C:\").holds(&["c:", "x"]), "a whole drive");
    }

    #[test]
    fn neither_a_name_nor_a_full_path_is_no_place() {
        let texts = [
            "",
            "  ",
            r"old\stuff",
            "old/stuff",
            "C:",
            r"\\server\share",
            r"\old",
            ":",
        ];
        for text in texts {
            assert_eq!(Place::parse(text), None, "{text:?}");
        }
        assert_eq!(place(" target "), place("target"), "spaces around it go");
    }

    #[test]
    fn places_added_rank_higher_or_lower_beside_the_built_in_ones() {
        let higher = [place("notes"), place(r"D:\work")];
        let places = desktop().with(&higher, &[place("target")]);
        assert_eq!(places.points(&["c:", "a", "notes", "b"]), BOOST);
        assert_eq!(places.points(&["d:", "work"]), BOOST);
        assert_eq!(places.points(&["c:", "code", "app", "target"]), -PENALTY);
        assert_eq!(places.points(&["c:", "users", "me", "desktop"]), BOOST);
        assert_eq!(places.points(&["c:", "x", "node_modules"]), -PENALTY);
        assert_eq!(places.points(&["c:", "x", ".git"]), -PENALTY);
        assert_eq!(places.points(&["c:", "x"]), 0.0);
    }

    #[test]
    fn a_full_path_starts_with_a_drive_letter_a_colon_and_a_backslash() {
        assert!(full_path(r"C:\"));
        assert!(full_path(r"z:\a"));
        for text in ["C:", r"1:\a", "C:/a", r"\\a\b", r"CC:\a", ""] {
            assert!(!full_path(text), "{text:?}");
        }
    }

    #[test]
    fn being_in_two_favoured_places_boosts_once() {
        let places = desktop().with(&[place("desktop")], &[]);
        assert_eq!(places.points(&["c:", "users", "me", "desktop"]), BOOST);
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
