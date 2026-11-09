//! How well a query matches a name.
//!
//! Strongest first: the whole name, the start of the name, the start of a
//! later word, the starts of several words (an acronym), anywhere inside.
//! Letters compare case-insensitively, Cyrillic included.

/// How a query matched a name, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
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
    let query = fold(query);
    let name = fold(name);
    let strength = if query == name {
        Strength::Exact
    } else {
        Strength::Prefix
    };
    name.starts_with(&query).then(|| Match {
        strength,
        positions: (0..query.len()).collect(),
    })
}

/// Lowercases letter by letter, one `char` for one `char`, so positions in
/// the folded text are positions in the original.
fn fold(text: &str) -> Vec<char> {
    text.chars()
        .map(|letter| letter.to_lowercase().next().unwrap_or(letter))
        .collect()
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
    fn letters_that_are_not_in_the_name_do_not_match() {
        assert_eq!(find("xyz", "Code"), None);
        assert_eq!(find("codes", "Code"), None);
    }
}
