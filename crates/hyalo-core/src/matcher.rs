//! How well a query matches a name.
//!
//! Strongest first: the whole name, the start of the name, the start of a
//! later word, the starts of several words (an acronym), anywhere inside.
//! Letters compare case-insensitively, Cyrillic included.

/// How a query matched a name, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    WordStart,
    Prefix,
    Exact,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub strength: Strength,
    /// Indices of the matched characters (`char`s, not bytes) in the name,
    /// ascending, for highlighting.
    pub positions: Vec<usize>,
}

/// Matches `query` against `name`; `None` when it does not match at all.
pub fn find(query: &str, name: &str) -> Option<Match> {
    let query: Vec<char> = query.chars().map(fold).collect();
    let letters: Vec<char> = name.chars().collect();
    let folded: Vec<char> = letters.iter().copied().map(fold).collect();
    let found_at = |start: usize| folded[start..].starts_with(&query);
    let run = |strength, start: usize| Match {
        strength,
        positions: (start..start + query.len()).collect(),
    };
    if found_at(0) {
        let whole = query.len() == folded.len();
        return Some(run(
            if whole {
                Strength::Exact
            } else {
                Strength::Prefix
            },
            0,
        ));
    }
    let start = word_starts(&letters)
        .into_iter()
        .find(|&start| found_at(start))?;
    Some(run(Strength::WordStart, start))
}

/// Lowercases one `char` into one `char`, so positions in folded text are
/// positions in the original.
fn fold(letter: char) -> char {
    letter.to_lowercase().next().unwrap_or(letter)
}

/// Where words begin: a letter or digit after a gap (start, space,
/// punctuation), or a capital right after a small letter ("PowerShell").
fn word_starts(letters: &[char]) -> Vec<usize> {
    (0..letters.len())
        .filter(|&at| starts_word(at.checked_sub(1).map(|before| letters[before]), letters[at]))
        .collect()
}

fn starts_word(before: Option<char>, letter: char) -> bool {
    let after_gap = !before.is_some_and(char::is_alphanumeric);
    let hump = before.is_some_and(char::is_lowercase) && letter.is_uppercase();
    (after_gap && letter.is_alphanumeric()) || hump
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strength(query: &str, name: &str) -> Option<Strength> {
        find(query, name).map(|found| found.strength)
    }

    fn positions(query: &str, name: &str) -> Vec<usize> {
        find(query, name).map_or_else(Vec::new, |found| found.positions)
    }

    #[test]
    fn the_whole_name_is_exact_in_any_case() {
        assert_eq!(strength("code", "Code"), Some(Strength::Exact));
        assert_eq!(strength("БЛОКНОТ", "Блокнот"), Some(Strength::Exact));
        assert_eq!(positions("код", "Код"), [0, 1, 2]);
    }

    #[test]
    fn the_start_of_the_name_is_a_prefix() {
        assert_eq!(strength("note", "Notepad++"), Some(Strength::Prefix));
        assert_eq!(strength("БЛОК", "Блокнот"), Some(Strength::Prefix));
        assert_eq!(positions("note", "Notepad++"), [0, 1, 2, 3]);
    }

    #[test]
    fn exact_is_stronger_than_prefix() {
        assert!(Strength::Exact > Strength::Prefix);
    }

    #[test]
    fn a_later_word_can_start_the_match() {
        assert_eq!(
            strength("studio", "Visual Studio Code"),
            Some(Strength::WordStart)
        );
        assert_eq!(
            positions("studio", "Visual Studio Code"),
            [7, 8, 9, 10, 11, 12]
        );
        assert_eq!(strength("ред", "Видео редактор"), Some(Strength::WordStart));
    }

    #[test]
    fn a_word_start_match_runs_on_into_the_next_words() {
        assert_eq!(
            strength("studio co", "Visual Studio Code"),
            Some(Strength::WordStart)
        );
    }

    #[test]
    fn a_capital_after_a_small_letter_starts_a_word() {
        assert_eq!(strength("shell", "PowerShell"), Some(Strength::WordStart));
        assert_eq!(positions("shell", "PowerShell"), [5, 6, 7, 8, 9]);
        assert_eq!(strength("ред", "ВидеоРедактор"), Some(Strength::WordStart));
    }

    #[test]
    fn a_letter_after_punctuation_starts_a_word() {
        assert_eq!(strength("zip", "7-Zip"), Some(Strength::WordStart));
        assert_eq!(
            strength("x86", "Program Files (x86)"),
            Some(Strength::WordStart)
        );
    }

    #[test]
    fn letters_inside_a_word_do_not_start_one() {
        for (query, name) in [
            ("hell", "PowerShell"),
            ("shell", "POWERSHELL"),
            ("32", "Win32"),
            ("tudio", "Visual Studio Code"),
        ] {
            assert_ne!(
                strength(query, name),
                Some(Strength::WordStart),
                "{query} in {name}"
            );
        }
    }

    #[test]
    fn prefix_is_stronger_than_a_word_start() {
        assert!(Strength::Prefix > Strength::WordStart);
    }

    #[test]
    fn letters_that_are_not_in_the_name_do_not_match() {
        assert_eq!(find("xyz", "Code"), None);
        assert_eq!(find("codes", "Code"), None);
    }
}
