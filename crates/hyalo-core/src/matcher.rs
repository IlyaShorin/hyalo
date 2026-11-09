//! How well a query matches a name.
//!
//! Strongest first: the whole name, the start of the name, the start of a
//! later word, the starts of several words (an acronym), anywhere inside.
//! Letters compare case-insensitively, Cyrillic included, and ё as е. A
//! query typed in the wrong keyboard layout matches too, a little weaker.

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
        points(self.strength, self.layout)
    }
}

fn points(strength: Strength, layout: Layout) -> f64 {
    let penalty = match layout {
        Layout::AsTyped => 0.0,
        Layout::Swapped => WRONG_LAYOUT,
    };
    strength.points() - penalty
}

/// A query made ready to match many names: folded once, as typed and as
/// read in the other keyboard layout.
pub struct Pattern {
    layouts: [(Layout, Folded); 2],
}

/// A query's letters as matching compares them, as `char`s and as text.
struct Folded {
    letters: Vec<char>,
    text: String,
}

impl Folded {
    fn new(letters: impl Iterator<Item = char>) -> Self {
        let letters: Vec<char> = letters.collect();
        let text = letters.iter().collect();
        Self { letters, text }
    }
}

impl Pattern {
    pub fn new(query: &str) -> Self {
        let typed = Folded::new(query.chars().map(compared));
        // Swap before ё becomes е: they sit on different keys.
        let swapped = query.chars().map(fold).map(keyboard::swap).map(compared);
        Self {
            layouts: [
                (Layout::AsTyped, typed),
                (Layout::Swapped, Folded::new(swapped)),
            ],
        }
    }

    /// Matches `name` in both layouts and keeps the better; `None` when
    /// neither matches.
    pub fn find(&self, name: &str) -> Option<Match> {
        let letters: Vec<char> = name.chars().collect();
        let folded: Vec<char> = letters.iter().copied().map(compared).collect();
        let words = words(&letters);
        self.layouts
            .iter()
            .filter_map(|(layout, query)| {
                let (strength, positions) = classify(&query.letters, &folded, &words)?;
                Some(Match {
                    strength,
                    layout: *layout,
                    positions,
                })
            })
            .max_by(|one, other| one.points().total_cmp(&other.points()))
    }

    /// The most points `find` could give a name, judged cheaply from the
    /// name as `folded` gives it, without working out where words start;
    /// `None` when the query's letters are not all in it in order, in
    /// either layout, so `find` gives `None` too.
    pub fn ceiling(&self, folded: &str) -> Option<f64> {
        self.layouts
            .iter()
            .filter_map(|(layout, query)| {
                let strength = best_possible(&query.text, folded)?;
                Some(points(strength, *layout))
            })
            .max_by(f64::total_cmp)
    }
}

/// The strongest kind of match a folded query could make in a folded
/// name: one at the start is what it is; one later in a single run is a
/// word start at best; letters apart, an acronym at best.
fn best_possible(query: &str, folded: &str) -> Option<Strength> {
    if !in_order(query, folded) {
        return None;
    }
    if folded.starts_with(query) {
        return Some(at_start(query.len(), folded.len()));
    }
    match folded.contains(query) {
        true => Some(Strength::WordStart),
        false => Some(Strength::Acronym),
    }
}

/// Whether the bytes of `query` appear in `text` in order. Letters in
/// order have their bytes in order, so this never refuses a match.
fn in_order(query: &str, text: &str) -> bool {
    let mut rest = text.bytes();
    query.bytes().all(|byte| rest.any(|other| other == byte))
}

/// A name as matching compares it: every letter folded, ё as е. Indexes
/// keep this beside the name for `Pattern::ceiling`.
pub fn folded(name: &str) -> String {
    name.chars().map(compared).collect()
}

/// Matches `query` against `name`, as typed and as if typed in the other
/// keyboard layout, and keeps the better; `None` when neither matches.
pub fn find(query: &str, name: &str) -> Option<Match> {
    Pattern::new(query).find(name)
}

/// A match from the start of a name: the whole of it, or its start. The
/// lengths are in any one unit.
fn at_start(query: usize, name: usize) -> Strength {
    match query == name {
        true => Strength::Exact,
        false => Strength::Prefix,
    }
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
        return Some(run(at_start(query.len(), folded.len()), 0));
    }
    let mut word_starts = words.iter().map(|word| word.start);
    word_starts
        .find(&found_at)
        .map(|start| run(Strength::WordStart, start))
        .or_else(|| acronym(query, folded, words))
        .or_else(|| {
            let mut windows = folded.windows(query.len());
            let start = windows.position(|window| window == query)?;
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

/// A letter as matching compares it: folded, and ё as е, since Russian
/// names are written both ways. Frecency keeps its queries in `fold`
/// alone, so picks saved before still load.
fn compared(letter: char) -> char {
    match fold(letter) {
        'ё' => 'е',
        other => other,
    }
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
    fn yo_and_ye_are_the_same_letter() {
        assert_eq!(strength("елка", "Ёлка"), Some(Strength::Exact));
        assert_eq!(strength("ЁЛКА", "Елка"), Some(Strength::Exact));
        assert_eq!(strength("ёж", "Мой ежик"), Some(Strength::WordStart));
        assert_eq!(positions("ёж", "Мой ежик"), [4, 5]);
    }

    #[test]
    fn yo_typed_in_the_wrong_layout_is_ye_too() {
        let found = find("`krf", "Елка").unwrap();
        assert_eq!(found.strength, Strength::Exact);
        assert_eq!(found.layout, Layout::Swapped);
    }

    #[test]
    fn yo_on_the_wrong_layout_is_still_its_own_key() {
        assert_eq!(strength("ё1", "`1"), Some(Strength::Exact));
    }

    #[test]
    fn a_query_that_reads_the_same_in_both_layouts_counts_as_typed() {
        let found = find("7", "7-Zip").unwrap();
        assert_eq!(found.layout, Layout::AsTyped);
    }

    #[test]
    fn a_name_is_folded_as_matching_compares_it() {
        assert_eq!(folded("Ёлка.TXT"), "елка.txt");
    }

    #[test]
    fn a_prepared_pattern_finds_what_find_finds() {
        let pattern = Pattern::new("мысщву");
        assert_eq!(
            pattern.find("Visual Studio Code"),
            find("мысщву", "Visual Studio Code")
        );
    }

    fn ceiling(query: &str, name: &str) -> Option<f64> {
        Pattern::new(query).ceiling(&folded(name))
    }

    #[test]
    fn the_ceiling_of_a_match_at_the_start_is_its_points() {
        assert_eq!(ceiling("code", "Code"), Some(100.0));
        assert_eq!(ceiling("co", "Code"), Some(80.0));
    }

    #[test]
    fn the_ceiling_of_letters_together_later_is_a_word_start() {
        assert_eq!(ceiling("ode", "Code"), Some(60.0));
    }

    #[test]
    fn the_ceiling_of_letters_apart_in_order_is_an_acronym() {
        assert_eq!(ceiling("vsc", "Visual Studio Code"), Some(40.0));
        assert_eq!(ceiling("ae", "table"), Some(40.0), "only a bound");
    }

    #[test]
    fn letters_out_of_order_have_no_ceiling() {
        assert_eq!(ceiling("cvs", "Visual Studio Code"), None);
        assert_eq!(ceiling("codes", "Code"), None);
    }

    #[test]
    fn the_ceiling_counts_the_wrong_layout_a_little_lower() {
        assert_eq!(ceiling("мысщву", "vscode"), Some(95.0));
        assert_eq!(ceiling("мысщву", "vscode!"), Some(75.0));
    }

    /// Every text of `1..=longest` letters taken from `alphabet`.
    fn texts(alphabet: &[char], longest: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut last = vec![String::new()];
        for _ in 0..longest {
            last = last
                .iter()
                .flat_map(|text| alphabet.iter().map(move |letter| format!("{text}{letter}")))
                .collect();
            all.extend(last.iter().cloned());
        }
        all
    }

    #[test]
    fn the_ceiling_is_never_below_what_find_gives() {
        // Names mix case (a hump), a gap, ё, and ф, which the 'a' key
        // types in the other layout.
        let names = texts(&['a', 'B', ' ', 'ё', 'ф'], 4);
        let queries = texts(&['a', 'b', 'е', 'ф'], 3);
        for query in &queries {
            let pattern = Pattern::new(query);
            for name in &names {
                let found = pattern.find(name).map(|found| found.points());
                let ceiling = pattern.ceiling(&folded(name));
                let below = found.is_some_and(|points| ceiling.is_none_or(|top| top < points));
                assert!(!below, "{query:?} in {name:?}: {found:?} over {ceiling:?}");
            }
        }
    }
}
