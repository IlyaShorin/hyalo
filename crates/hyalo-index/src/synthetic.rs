//! A made-up drive for benches and tests, shaped like a real one: folders
//! nested several deep, names made of everyday words joined by
//! separators or camel humps, digits, extensions, some Cyrillic, and the
//! folders the ranking cares about (Desktop, node_modules, .git, AppData).

use crate::{EntryId, Index};

const WORDS: [&str; 48] = [
    "report",
    "notes",
    "index",
    "main",
    "config",
    "test",
    "data",
    "image",
    "photo",
    "video",
    "project",
    "source",
    "library",
    "build",
    "module",
    "readme",
    "license",
    "setup",
    "install",
    "update",
    "backup",
    "document",
    "invoice",
    "budget",
    "plan",
    "draft",
    "final",
    "archive",
    "user",
    "app",
    "server",
    "client",
    "api",
    "util",
    "helper",
    "view",
    "model",
    "controller",
    "service",
    "music",
    "game",
    "отчёт",
    "документ",
    "фото",
    "проект",
    "заметки",
    "план",
    "счёт",
];

const EXTENSIONS: [&str; 18] = [
    "txt", "md", "rs", "js", "ts", "json", "png", "jpg", "pdf", "docx", "xlsx", "exe", "dll",
    "log", "html", "css", "py", "zip",
];

/// Folder names a real drive is full of, given now and then.
const FOLDERS: [&str; 5] = ["Desktop", "node_modules", ".git", "AppData", "Documents"];

/// The same drive for the same `seed`: a root `C:` and `count` files and
/// folders under it, about one in eight a folder.
pub fn drive(count: usize, seed: u64) -> Index {
    let mut index = Index::default();
    let root = index.add_root("C:").expect("a root name");
    let mut folders: Vec<EntryId> = vec![root];
    let mut random = Random(seed | 1);
    for _ in 0..count {
        let parent = folders[random.below(folders.len())];
        let folder = random.below(8) == 0;
        let name = name(&mut random, folder);
        let id = index.add(parent, &name, folder).expect("a made-up name");
        if folder {
            folders.push(id);
        }
    }
    index
}

/// A made-up name: one to three words, then digits now and then, then an
/// extension for a file.
fn name(random: &mut Random, folder: bool) -> String {
    if folder && random.below(16) == 0 {
        return FOLDERS[random.below(FOLDERS.len())].to_string();
    }
    let joiner = ["_", "-", " ", ".", ""][random.below(5)];
    let words: Vec<String> = (0..=random.below(3))
        .map(|at| word(random, at > 0 && joiner.is_empty()))
        .collect();
    let mut name = words.join(joiner);
    if random.below(4) == 0 {
        name.push_str(&random.below(2030).to_string());
    }
    if !folder {
        name = format!("{name}.{}", EXTENSIONS[random.below(EXTENSIONS.len())]);
    }
    name
}

/// A word, capitalised when it starts a hump (`ReadmeNotes`) and now and
/// then anyway.
fn word(random: &mut Random, hump: bool) -> String {
    let word = WORDS[random.below(WORDS.len())];
    match hump || random.below(4) == 0 {
        true => capitalised(word),
        false => word.to_string(),
    }
}

fn capitalised(word: &str) -> String {
    let mut letters = word.chars();
    let first = letters
        .next()
        .map(|first| first.to_uppercase().collect::<String>());
    first.unwrap_or_default() + letters.as_str()
}

/// xorshift64*: plenty for made-up names, and the same every run.
struct Random(u64);

impl Random {
    /// A number in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let next = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (next >> 32) as usize % bound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(index: &Index) -> Vec<String> {
        (0..index.len() as u32)
            .filter_map(|at| index.name(EntryId(at)).map(String::from))
            .collect()
    }

    #[test]
    fn a_drive_holds_its_root_and_as_many_entries_as_asked() {
        assert_eq!(drive(1000, 1).len(), 1001);
    }

    #[test]
    fn the_same_seed_makes_the_same_drive() {
        assert_eq!(names(&drive(500, 9)), names(&drive(500, 9)));
        assert_ne!(names(&drive(500, 9)), names(&drive(500, 10)));
    }

    #[test]
    fn a_drive_looks_like_a_real_one() {
        let index = drive(20_000, 3);
        let names = names(&index);
        let has = |test: &dyn Fn(&str) -> bool| names.iter().any(|name| test(name));
        assert!(has(&|name| name.contains('_')), "separators");
        assert!(has(&|name| name.ends_with(".rs")), "extensions");
        assert!(has(&|name| name
            .chars()
            .any(|letter| letter.is_ascii_digit())));
        let hump = |name: &str| {
            let mut pairs = name.chars().zip(name.chars().skip(1));
            pairs.any(|(one, two)| one.is_lowercase() && two.is_uppercase())
        };
        assert!(has(&hump), "humps");
        assert!(
            has(&|name| name.contains("отчёт") || name.contains("Отчёт")),
            "Cyrillic"
        );
        assert!(has(&|name| name == "node_modules"), "buried folders");
        let folders = (0..index.len() as u32)
            .filter(|&at| index.is_folder(EntryId(at)) == Some(true))
            .count();
        assert!((1500..3500).contains(&folders), "{folders} folders");
        let deepest = (0..index.len() as u32)
            .filter_map(|at| index.path(EntryId(at)))
            .map(|path| path.matches('\\').count())
            .max();
        assert!(deepest >= Some(6), "{deepest:?} deep");
    }
}
