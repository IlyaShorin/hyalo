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
    /// A match of no letters: a row filters alone list.
    pub fn none() -> Match {
        Match {
            strength: Strength::Substring,
            layout: Layout::AsTyped,
            positions: Vec::new(),
        }
    }

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

/// What `Pattern::ceiling` knows of the points `find` gives a name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ceiling {
    /// These points: the kind of match is known.
    Exactly(f64),
    /// At most these: an acronym only `find` can confirm.
    AtMost(f64),
}

impl Ceiling {
    pub fn points(self) -> f64 {
        match self {
            Ceiling::Exactly(points) | Ceiling::AtMost(points) => points,
        }
    }
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

    /// A first, cheap look at a name, from its `folded` text alone (as
    /// `folded` gives it); `None` when the query's letters are not all in
    /// it in order, in either layout, so `find` gives `None` too.
    pub fn rough<'a>(&'a self, folded: &'a str) -> Option<Rough<'a>> {
        let kinds = self
            .layouts
            .each_ref()
            .map(|(layout, query)| (*layout, query, roughly(&query.text, folded)));
        let (_, points) = best(kinds.iter().map(|&(layout, _, kind)| (layout, kind)))?;
        Some(Rough {
            folded,
            kinds,
            points,
        })
    }

    /// The most points `find` could give `name`, judged from it and its
    /// `folded` text without finding where it matches.
    pub fn ceiling(&self, folded: &str, name: &str) -> Option<Ceiling> {
        self.rough(folded)?.ceiling(name)
    }
}

/// What a first look at a folded name found: per layout, the query and
/// the strongest kind of match the folded text allows.
pub struct Rough<'a> {
    folded: &'a str,
    kinds: [(Layout, &'a Folded, Option<Strength>); 2],
    points: f64,
}

impl Rough<'_> {
    /// At most the points `find` gives; never below the ceiling's.
    pub fn points(&self) -> f64 {
        self.points
    }

    /// A closer look, with the words of `name`, the name folded here.
    pub fn ceiling(&self, name: &str) -> Option<Ceiling> {
        let (strength, points) = best(self.closer(name))?;
        Some(match strength {
            Strength::Acronym => Ceiling::AtMost(points),
            _ => Ceiling::Exactly(points),
        })
    }

    /// The points `find` gives `name`, without finding where: the ceiling,
    /// with a possible acronym checked word by word. A letter that starts
    /// a word with the rest of the query after it is no acronym yet: in a
    /// long name of many words (a Windows component's manifest) it rarely
    /// is one.
    pub fn exactly(&self, name: &str) -> Option<f64> {
        let kinds = self.kinds.iter().zip(self.closer(name));
        let checked = kinds.map(|(&(layout, query, rough), (_, closer))| {
            let checked = match closer {
                Some(Strength::Acronym) if !acronym_of(&query.letters, self.folded, name) => {
                    (rough == Some(Strength::WordStart)).then_some(Strength::Substring)
                }
                closer => closer,
            };
            (layout, checked)
        });
        best(checked).map(|(_, points)| points)
    }

    /// Per layout, the strongest kind of match the words of `name` allow,
    /// an acronym only roughly.
    fn closer(&self, name: &str) -> impl Iterator<Item = (Layout, Option<Strength>)> {
        self.kinds.iter().map(move |&(layout, query, kind)| {
            let kind = kind.and_then(|kind| closer(&query.text, self.folded, name, kind));
            (layout, kind)
        })
    }
}

/// Whether the folded `query` splits into pieces, each the start of a
/// word of `name`, the words in order: what `acronym` finds, without
/// finding where. `folded` is `name` folded.
fn acronym_of(query: &[char], folded: &str, name: &str) -> bool {
    // The places in the query its pieces so far reach, one bit each.
    let Some(whole) = u32::try_from(query.len())
        .ok()
        .and_then(|length| 1_u128.checked_shl(length))
    else {
        return long_acronym_of(query, name);
    };
    let mut reached = 1_u128;
    for word in folded_words(folded, name) {
        let mut from = reached;
        while from != 0 && reached & whole == 0 {
            let at = from.trailing_zeros() as usize;
            from &= from - 1;
            let common = query[at..]
                .iter()
                .zip(word.chars())
                .take_while(|(typed, named)| *typed == named)
                .count();
            // Every piece of 1..=common letters from `at` reaches further.
            reached |= ((1_u128 << common) - 1) << at << 1;
        }
    }
    reached & whole != 0
}

/// `acronym_of` for a query too long for its bits: `find`'s own search.
fn long_acronym_of(query: &[char], name: &str) -> bool {
    let letters: Vec<char> = name.chars().collect();
    let folded: Vec<char> = letters.iter().copied().map(compared).collect();
    acronym(query, &folded, &words(&letters)).is_some()
}

/// The words of `name` as `words` cuts them, each as its text in
/// `folded`, `name` folded.
fn folded_words<'a>(folded: &'a str, name: &str) -> impl Iterator<Item = &'a str> {
    let mut before = None;
    let starts = name
        .chars()
        .zip(folded.char_indices())
        .filter_map(move |(letter, (at, _))| {
            let start = starts_word(before, letter);
            before = Some(letter);
            start.then_some(at)
        });
    let mut starts = starts.chain([folded.len()]).peekable();
    std::iter::from_fn(move || {
        let start = starts.next()?;
        Some(&folded[start..*starts.peek()?])
    })
}

/// The kind of match, over the layouts, that gives the most points, and
/// its points.
fn best(kinds: impl Iterator<Item = (Layout, Option<Strength>)>) -> Option<(Strength, f64)> {
    let found = kinds.filter_map(|(layout, kind)| Some((kind?, points(kind?, layout))));
    found.max_by(|(_, one), (_, other)| one.total_cmp(other))
}

/// The strongest kind of match a folded query could make in a folded
/// name, not knowing where words start: one at the start is what it is,
/// one later in a single run a word start at best, letters apart an
/// acronym at best.
fn roughly(query: &str, folded: &str) -> Option<Strength> {
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

/// The strongest kind of match a folded query could make in `name`, with
/// its words, where a first look found `rough`: past the start, a run at
/// a word start is a word start, the first letter at one with the rest
/// after it an acronym at best, a run anywhere a substring.
fn closer(query: &str, folded: &str, name: &str, rough: Strength) -> Option<Strength> {
    match rough {
        Strength::WordStart => among_words(query, folded, name, true),
        Strength::Acronym => among_words(query, folded, name, false),
        known => Some(known),
    }
}

/// The kind of match past the start: looks for word starts only where
/// the query's first letter is. `run` says whether the query is in the
/// name in one run.
fn among_words(query: &str, folded: &str, name: &str, run: bool) -> Option<Strength> {
    let first = query.chars().next()?;
    let rest = &query[first.len_utf8()..];
    let ascii = name.is_ascii();
    let mut acronym = false;
    for (at, _) in folded.match_indices(first) {
        if !starts_word_at(name, folded, at, ascii) {
            continue;
        }
        let word = &folded[at..];
        if run && word.starts_with(query) {
            return Some(Strength::WordStart);
        }
        acronym = acronym || in_order(rest, &word[first.len_utf8()..]);
    }
    match acronym {
        true => Some(Strength::Acronym),
        false => run.then_some(Strength::Substring),
    }
}

/// Whether a word of `name` starts at byte `at` of its `folded` text. An
/// `ascii` name folds byte for byte, so `at` is a place in it too.
fn starts_word_at(name: &str, folded: &str, at: usize, ascii: bool) -> bool {
    if ascii {
        let letter = |at: usize| char::from(name.as_bytes()[at]);
        return starts_word(at.checked_sub(1).map(letter), letter(at));
    }
    let index = folded[..at].chars().count();
    let mut letters = name.chars().skip(index.saturating_sub(1));
    let before = match index {
        0 => None,
        _ => letters.next(),
    };
    letters
        .next()
        .is_some_and(|letter| starts_word(before, letter))
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
    let mut text = String::new();
    fold_onto(name, &mut text);
    text
}

/// `name` folded as `folded` folds it, put after what `text` holds: an
/// index folds two million names onto one text without a string for each.
pub fn fold_onto(name: &str, text: &mut String) {
    text.extend(name.chars().map(compared));
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
    // The first piece starts a word with the first letter: without one,
    // there is nothing to search.
    let first = query.first()?;
    if !words.iter().any(|word| folded[word.start] == *first) {
        return None;
    }
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
    fn an_acronym_may_take_every_piece_from_words_with_one_first_letter() {
        assert_eq!(strength("aac", "ab ac"), Some(Strength::Acronym));
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
    fn a_name_is_folded_onto_the_end_of_a_text() {
        let mut text = String::from("Ab");
        fold_onto("Ёлка.TXT", &mut text);
        assert_eq!(text, "Abелка.txt", "what was there stays as it was");
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
        let ceiling = Pattern::new(query).ceiling(&folded(name), name);
        ceiling.map(Ceiling::points)
    }

    fn exactly(query: &str, name: &str) -> bool {
        let ceiling = Pattern::new(query).ceiling(&folded(name), name);
        matches!(ceiling, Some(Ceiling::Exactly(_)))
    }

    #[test]
    fn the_ceiling_is_exact_unless_it_is_an_acronym() {
        assert!(exactly("code", "Code"));
        assert!(exactly("co", "Code"));
        assert!(exactly("code", "Visual Studio Code"));
        assert!(exactly("ode", "Code"));
        assert!(exactly("мысщву", "vscode"));
        assert!(!exactly("vsc", "Visual Studio Code"));
        assert!(!exactly("ae", "a table"));
    }

    #[test]
    fn the_ceiling_of_a_match_at_the_start_is_its_points() {
        assert_eq!(ceiling("code", "Code"), Some(100.0));
        assert_eq!(ceiling("co", "Code"), Some(80.0));
    }

    #[test]
    fn the_ceiling_of_letters_together_later_is_a_word_start() {
        assert_eq!(ceiling("code", "Visual Studio Code"), Some(60.0));
    }

    #[test]
    fn the_ceiling_of_letters_apart_in_order_is_an_acronym() {
        assert_eq!(ceiling("vsc", "Visual Studio Code"), Some(40.0));
        assert_eq!(ceiling("ae", "a table"), Some(40.0), "only a bound");
    }

    fn rough(query: &str, name: &str) -> Option<f64> {
        let folded = folded(name);
        Pattern::new(query)
            .rough(&folded)
            .map(|rough| rough.points())
    }

    #[test]
    fn a_rough_ceiling_does_not_know_where_words_start() {
        assert_eq!(rough("code", "Code"), Some(100.0));
        assert_eq!(rough("co", "Code"), Some(80.0));
        assert_eq!(rough("ode", "Code"), Some(60.0));
        assert_eq!(rough("ae", "table"), Some(40.0));
        assert_eq!(rough("cvs", "Visual Studio Code"), None);
        assert_eq!(rough("мысщву", "vscode"), Some(95.0));
    }

    #[test]
    fn the_ceiling_sees_where_words_start() {
        assert_eq!(ceiling("e", "invoice.exe"), Some(60.0), "exe starts a word");
        assert_eq!(ceiling("e", "invoice"), Some(20.0), "inside a word");
        assert_eq!(ceiling("ae", "a table"), Some(40.0), "a starts a word");
        assert_eq!(ceiling("ae", "table"), None, "neither a run nor an acronym");
        assert_eq!(
            ceiling(".md", "readme.md"),
            Some(20.0),
            "a dot starts no word"
        );
    }

    #[test]
    fn the_ceiling_sees_the_humps_folding_hides() {
        assert_eq!(ceiling("shell", "PowerShell"), Some(60.0));
        assert_eq!(ceiling("hell", "PowerShell"), Some(20.0));
        assert_eq!(ceiling("ps", "PowerShell"), Some(40.0));
    }

    #[test]
    fn letters_out_of_order_have_no_ceiling() {
        assert_eq!(ceiling("cvs", "Visual Studio Code"), None);
        assert_eq!(ceiling("codes", "Code"), None);
    }

    fn exact(query: &str, name: &str) -> Option<f64> {
        let folded = folded(name);
        Pattern::new(query).rough(&folded)?.exactly(name)
    }

    #[test]
    fn exactly_checks_an_acronym_word_by_word() {
        assert_eq!(exact("vsc", "Visual Studio Code"), Some(40.0));
        assert_eq!(exact("vscode", "Visual Studio Code"), Some(40.0));
        assert_eq!(exact("ae", "a table"), None, "e starts no word");
        assert_eq!(exact("ab", "a cab"), Some(20.0), "a run, no acronym");
        assert_eq!(exact("aab", "aa ab"), Some(40.0), "a shorter first piece");
        // A Windows component's manifest: its r starts a word and the
        // rest follow in order, but none of them at the start of a word.
        let manifest = "amd64_microsoft-windows-resampledmo_31bf3856ad364e35_10.0.26100.9549_none_3b264855887cfba8";
        assert_eq!(exact("readme", manifest), None);
        assert_eq!(exact("мысщву", "Visual Studio Code"), Some(35.0));
    }

    #[test]
    fn exactly_checks_an_acronym_of_any_length() {
        let (a, b) = ("a".repeat(70), "b".repeat(70));
        assert_eq!(exact(&format!("{a}{b}"), &format!("{a} {b}")), Some(40.0));
        assert_eq!(exact(&format!("{a}{b}"), &format!("{a} x{b}")), None);
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
                let exact = pattern.ceiling(&folded(name), name);
                if let Some(Ceiling::Exactly(points)) = exact {
                    assert_eq!(found, Some(points), "{query:?} in {name:?}");
                }
                let ceiling = exact.map(Ceiling::points);
                let below = found.is_some_and(|points| ceiling.is_none_or(|top| top < points));
                assert!(!below, "{query:?} in {name:?}: {found:?} over {ceiling:?}");
                let folded = folded(name);
                let rough = pattern.rough(&folded);
                let exact = rough.as_ref().and_then(|rough| rough.exactly(name));
                assert_eq!(exact, found, "{query:?} in {name:?} exactly");
                let rough = rough.map(|rough| rough.points());
                let below = ceiling.is_some_and(|points| rough.is_none_or(|top| top < points));
                assert!(!below, "{query:?} in {name:?}: {ceiling:?} over {rough:?}");
            }
        }
    }
}
