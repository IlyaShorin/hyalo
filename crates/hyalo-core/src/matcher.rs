//! How well a query matches a name.
//!
//! Strongest first: the whole name, the start of the name, the start of a
//! later word, the starts of several words (an acronym), anywhere inside.
//! Letters compare case-insensitively, Cyrillic included. A query typed in
//! the wrong keyboard layout matches too, a little weaker.

use crate::keyboard;
use std::collections::HashSet;
use std::ops::Range;

/// How a query matched a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Exact,
    Prefix,
    WordStart,
    Acronym,
    Substring,
}

impl Strength {
    /// One tier per kind, 20 points apart.
    fn points(self) -> f64 {
        match self {
            Strength::Exact => 100.0,
            Strength::Prefix => 80.0,
            Strength::WordStart => 60.0,
            Strength::Acronym => 40.0,
            Strength::Substring => 20.0,
        }
    }
}

/// Which keyboard layout the query matched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    AsTyped,
    /// Read as the same keys in the other layout: "мысщву" as "vscode".
    Swapped,
}

/// Points a match in the swapped layout loses: a quarter of a tier, so it
/// stays above the next weaker kind of match typed correctly.
const WRONG_LAYOUT: f64 = 5.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub strength: Strength,
    pub layout: Layout,
    /// Indices of the matched characters (`char`s, not bytes) in the name,
    /// ascending, for highlighting.
    pub positions: Vec<usize>,
}

impl Match {
    /// How good the match is; more is better.
    pub fn points(&self) -> f64 {
        let penalty = match self.layout {
            Layout::AsTyped => 0.0,
            Layout::Swapped => WRONG_LAYOUT,
        };
        self.strength.points() - penalty
    }
}

/// Matches `query` against `name`, as typed and as if typed in the other
/// keyboard layout, and keeps the better; `None` when neither matches.
pub fn find(query: &str, name: &str) -> Option<Match> {
    let letters: Vec<char> = name.chars().collect();
    let folded: Vec<char> = letters.iter().copied().map(fold).collect();
    let words = words(&letters);
    let typed: Vec<char> = query.chars().map(fold).collect();
    let swapped: Vec<char> = typed.iter().copied().map(keyboard::swap).collect();
    [(Layout::AsTyped, typed), (Layout::Swapped, swapped)]
        .into_iter()
        .filter_map(|(layout, query)| {
            let (strength, positions) = classify(&query, &folded, &words)?;
            Some(Match {
                strength,
                layout,
                positions,
            })
        })
        .max_by(|one, other| one.points().total_cmp(&other.points()))
}

/// The strongest way a folded query matches a folded name, and where.
fn classify(
    query: &[char],
    folded: &[char],
    words: &[Range<usize>],
) -> Option<(Strength, Vec<usize>)> {
    let found_at = |start: &usize| folded[*start..].starts_with(query);
    let run = |strength, start: usize| (strength, (start..start + query.len()).collect());
    if found_at(&0) {
        let strength = match query.len() == folded.len() {
            true => Strength::Exact,
            false => Strength::Prefix,
        };
        return Some(run(strength, 0));
    }
    let mut word_starts = words.iter().map(|word| word.start);
    word_starts
        .find(&found_at)
        .map(|start| run(Strength::WordStart, start))
        .or_else(|| acronym(query, folded, words))
        .or_else(|| {
            let start = (0..folded.len()).find(&found_at)?;
            Some(run(Strength::Substring, start))
        })
}

fn acronym(
    query: &[char],
    folded: &[char],
    words: &[Range<usize>],
) -> Option<(Strength, Vec<usize>)> {
    let mut search = Acronym {
        query,
        folded,
        words,
        tried: HashSet::new(),
    };
    Some((Strength::Acronym, search.positions(0, 0)?))
}

/// Lowercases one `char` into one `char`, so positions in folded text are
/// positions in the original.
pub(crate) fn fold(letter: char) -> char {
    letter.to_lowercase().next().unwrap_or(letter)
}

/// The words of a name, each from its start up to the next word's start.
/// A word starts at a letter or digit after a gap (start, space,
/// punctuation), or at a capital right after a small letter ("PowerShell").
fn words(letters: &[char]) -> Vec<Range<usize>> {
    let starts: Vec<usize> = (0..letters.len())
        .filter(|&at| starts_word(at.checked_sub(1).map(|before| letters[before]), letters[at]))
        .collect();
    let ends = starts.iter().skip(1).copied().chain([letters.len()]);
    starts
        .iter()
        .zip(ends)
        .map(|(&start, end)| start..end)
        .collect()
}

fn starts_word(before: Option<char>, letter: char) -> bool {
    let after_gap = !before.is_some_and(char::is_alphanumeric);
    let hump = before.is_some_and(char::is_lowercase) && letter.is_uppercase();
    (after_gap && letter.is_alphanumeric()) || hump
}

/// Splits a query into pieces, each the start of a word, in word order:
/// "vsc" and "vscode" both fit Visual Studio Code.
struct Acronym<'a> {
    query: &'a [char],
    folded: &'a [char],
    words: &'a [Range<usize>],
    /// `(from, word)` pairs already searched. The search stops at the first
    /// success, so one seen again has failed; skipping it keeps the search
    /// polynomial on long repetitive names.
    tried: HashSet<(usize, usize)>,
}

impl Acronym<'_> {
    /// Positions for `query[from..]` taken from `words[word..]`. Each word
    /// gives the longest piece that still lets the rest match; a piece of
    /// length 0 skips the word.
    fn positions(&mut self, from: usize, word: usize) -> Option<Vec<usize>> {
        if from == self.query.len() {
            return Some(Vec::new());
        }
        let span = self.words.get(word)?.clone();
        if !self.tried.insert((from, word)) {
            return None;
        }
        let common = self.query[from..]
            .iter()
            .zip(&self.folded[span.clone()])
            .take_while(|(typed, named)| typed == named)
            .count();
        (0..=common).rev().find_map(|take| {
            let mut rest = self.positions(from + take, word + 1)?;
            rest.splice(0..0, span.start..span.start + take);
            Some(rest)
        })
    }
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
    fn first_letters_of_words_are_an_acronym() {
        assert_eq!(
            strength("vsc", "Visual Studio Code"),
            Some(Strength::Acronym)
        );
        assert_eq!(positions("vsc", "Visual Studio Code"), [0, 7, 14]);
        assert_eq!(strength("ps", "PowerShell"), Some(Strength::Acronym));
        assert_eq!(positions("ps", "PowerShell"), [0, 5]);
        assert_eq!(strength("вр", "Видео редактор"), Some(Strength::Acronym));
    }

    #[test]
    fn an_acronym_may_skip_words() {
        assert_eq!(positions("sc", "Visual Studio Code"), [7, 14]);
        assert_eq!(positions("vc", "Visual Studio Code"), [0, 14]);
    }

    #[test]
    fn an_acronym_piece_may_run_into_its_word() {
        assert_eq!(
            strength("vscode", "Visual Studio Code"),
            Some(Strength::Acronym)
        );
        assert_eq!(
            positions("vscode", "Visual Studio Code"),
            [0, 7, 14, 15, 16, 17]
        );
        assert_eq!(positions("visstu", "Visual Studio"), [0, 1, 2, 7, 8, 9]);
    }

    #[test]
    fn an_acronym_backs_off_when_a_longer_piece_leads_nowhere() {
        assert_eq!(positions("abc", "ab bc"), [0, 3, 4]);
    }

    #[test]
    fn an_acronym_keeps_the_word_order_and_uses_each_word_once() {
        assert_eq!(find("cvs", "Visual Studio Code"), None);
        assert_eq!(find("vv", "Visual Studio"), None);
    }

    #[test]
    fn a_long_repetitive_name_is_answered_at_once() {
        let name = "aa ".repeat(40);
        let query = format!("{}b", "a".repeat(30));
        assert_eq!(find(&query, &name), None);
    }

    #[test]
    fn letters_inside_a_word_are_a_substring() {
        assert_eq!(
            strength("ode", "Visual Studio Code"),
            Some(Strength::Substring)
        );
        assert_eq!(positions("ode", "Visual Studio Code"), [15, 16, 17]);
        assert_eq!(strength("hell", "PowerShell"), Some(Strength::Substring));
        assert_eq!(
            strength("ЕДАК", "Видео редактор"),
            Some(Strength::Substring)
        );
    }

    #[test]
    fn a_substring_is_the_first_one_in_the_name() {
        assert_eq!(positions("an", "Banana"), [1, 2]);
    }

    #[test]
    fn letters_that_are_not_in_the_name_do_not_match() {
        assert_eq!(find("xyz", "Code"), None);
        assert_eq!(find("codes", "Code"), None);
    }

    #[test]
    fn stronger_kinds_of_match_score_more_points() {
        let points = |query, name| find(query, name).map(|found| found.points());
        let ladder = [
            points("code", "Code"),
            points("co", "Code"),
            points("code", "Visual Studio Code"),
            points("vsc", "Visual Studio Code"),
            points("ode", "Code"),
        ];
        assert!(
            ladder.is_sorted_by(|stronger, weaker| stronger > weaker),
            "{ladder:?}"
        );
    }

    #[test]
    fn a_query_typed_in_the_wrong_layout_still_matches() {
        let found = find("мысщву", "Visual Studio Code").unwrap();
        assert_eq!(found.strength, Strength::Acronym);
        assert_eq!(found.layout, Layout::Swapped);
        assert_eq!(found.positions, [0, 7, 14, 15, 16, 17]);
        assert_eq!(strength("ЗЩЦУКЫРУДД", "PowerShell"), Some(Strength::Exact));
        assert_eq!(strength(",kjryjn", "Блокнот"), Some(Strength::Exact));
    }

    #[test]
    fn the_wrong_layout_scores_a_little_below_the_right_one() {
        let points = |query, name| find(query, name).map(|found| found.points());
        assert!(points("мысщву", "vscode") < points("vscode", "vscode"));
        assert!(points("мысщву", "vscode") > points("vsc", "vscode"));
    }

    #[test]
    fn a_query_that_reads_the_same_in_both_layouts_counts_as_typed() {
        let found = find("7", "7-Zip").unwrap();
        assert_eq!(found.layout, Layout::AsTyped);
    }
}
