//! A line of text being edited: the launcher's query, a folder name typed
//! in the settings window. Both edit text the same way, through this.
//!
//! The text, the caret and the other end of the selection (the anchor)
//! are counted in `char`s; DirectWrite counts UTF-16 units, and
//! `utf16_offset` and `char_index` go between the two.
//!
//! Windows delivers typed text as UTF-16 code units in `WM_CHAR`, together
//! with control codes: 0x08 for Backspace and 0x7F for Ctrl+Backspace.
//! A character outside the Basic Multilingual Plane (an emoji) arrives as
//! two messages, a surrogate pair. The keys that move the caret, select
//! and use the clipboard arrive as `WM_KEYDOWN`; `key` says what they do.

use std::ops::Range;
use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_A, VK_C, VK_DELETE, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_RIGHT, VK_V, VK_X,
};

use crate::keys::Held;

#[derive(Debug, Default)]
pub struct Field {
    text: String,
    /// Where the caret is.
    caret: usize,
    /// The other end of the selection: at the caret when nothing is
    /// selected.
    anchor: usize,
    pending_high: Option<u16>,
}

/// Where a key moves the caret to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    /// To the start of the word before.
    WordLeft,
    /// To the start of the next word.
    WordRight,
    Home,
    End,
}

/// A change to the caret, the selection or the text that needs nothing
/// from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Moves the caret; a selection goes.
    Move(Motion),
    /// Moves the caret and keeps the anchor: the selection grows or
    /// shrinks.
    Select(Motion),
    /// Takes out the selection, or what lies between the caret and where
    /// the motion goes.
    Erase(Motion),
    SelectAll,
}

/// What a key does in a field, besides typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Edit(Edit),
    Copy,
    Cut,
    Paste,
}

/// What an edit changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edited {
    Nothing,
    /// The caret or the selection, not the text.
    Caret,
    Text,
}

/// What `key` does in a field with `held` modifiers, if anything: the
/// arrows, Home and End move the caret (by word with Ctrl) and select
/// with Shift; Delete takes out a letter, or a word with Ctrl; Ctrl+A
/// selects all; Ctrl+C, Ctrl+X and Ctrl+V copy, cut and paste, as do
/// Windows' older Ctrl+Insert, Shift+Delete and Shift+Insert.
pub fn key(key: VIRTUAL_KEY, held: Held) -> Option<Key> {
    if let Some(motion) = motion(key, held.ctrl) {
        return Some(Key::Edit(match held.shift {
            true => Edit::Select(motion),
            false => Edit::Move(motion),
        }));
    }
    let keys: &[(VIRTUAL_KEY, Key)] = match (held.ctrl, held.shift) {
        (true, _) => &WITH_CTRL,
        (false, true) => &WITH_SHIFT,
        (false, false) => &ALONE,
    };
    let found = keys.iter().find(|&&(pressed, _)| pressed == key);
    found.map(|&(_, does)| does)
}

/// Where a key that moves the caret moves it, by word with Ctrl.
fn motion(key: VIRTUAL_KEY, ctrl: bool) -> Option<Motion> {
    let (by_letter, by_word) = match key {
        VK_LEFT => (Motion::Left, Motion::WordLeft),
        VK_RIGHT => (Motion::Right, Motion::WordRight),
        VK_HOME => (Motion::Home, Motion::Home),
        VK_END => (Motion::End, Motion::End),
        _ => return None,
    };
    Some(if ctrl { by_word } else { by_letter })
}

/// The keys that do something with Ctrl held, Shift or not.
const WITH_CTRL: [(VIRTUAL_KEY, Key); 6] = [
    (VK_DELETE, Key::Edit(Edit::Erase(Motion::WordRight))),
    (VK_INSERT, Key::Copy),
    (VK_A, Key::Edit(Edit::SelectAll)),
    (VK_C, Key::Copy),
    (VK_X, Key::Cut),
    (VK_V, Key::Paste),
];

/// The keys that do something with Shift alone held.
const WITH_SHIFT: [(VIRTUAL_KEY, Key); 2] = [(VK_DELETE, Key::Cut), (VK_INSERT, Key::Paste)];

/// The keys that do something alone.
const ALONE: [(VIRTUAL_KEY, Key); 1] = [(VK_DELETE, Key::Edit(Edit::Erase(Motion::Right)))];

impl Field {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What is selected, from its first character to past its last;
    /// empty, at the caret, when nothing is.
    pub fn selection(&self) -> Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    /// The text selected; empty when nothing is.
    pub fn selected(&self) -> &str {
        let Range { start, end } = self.selection();
        &self.text[self.byte(start)..self.byte(end)]
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = 0;
        self.pending_high = None;
    }

    /// Applies one `WM_CHAR` code unit: a letter takes the place of the
    /// selection; Backspace and Ctrl+Backspace take out the selection or
    /// what is before the caret. Returns whether the text changed.
    pub fn type_unit(&mut self, unit: u16) -> bool {
        let high = self.pending_high.take();
        match unit {
            BACKSPACE => self.apply(Edit::Erase(Motion::Left)) == Edited::Text,
            CTRL_BACKSPACE => self.apply(Edit::Erase(Motion::WordLeft)) == Edited::Text,
            0xD800..=0xDBFF => {
                self.pending_high = Some(unit);
                false
            }
            0xDC00..=0xDFFF => self.insert(high.and_then(|high| decode_pair(high, unit))),
            0..=0x1F => false,
            _ => self.insert(char::from_u32(u32::from(unit))),
        }
    }

    /// Carries out `edit`; what it changed.
    pub fn apply(&mut self, edit: Edit) -> Edited {
        let before = (self.caret, self.anchor);
        match edit {
            Edit::Move(motion) => {
                self.caret = self.moved(motion);
                self.anchor = self.caret;
            }
            Edit::Select(motion) => self.caret = self.target(motion),
            Edit::Erase(motion) => return self.erase(motion),
            Edit::SelectAll => (self.anchor, self.caret) = (0, self.length()),
        }
        match (self.caret, self.anchor) == before {
            true => Edited::Nothing,
            false => Edited::Caret,
        }
    }

    /// Pastes `text` as one line in place of the selection: line breaks
    /// at its end go, the others and every other control character
    /// become spaces. Whether the text changed.
    pub fn paste(&mut self, text: &str) -> bool {
        let line = text.trim_end_matches(['\r', '\n']).replace("\r\n", " ");
        let line: String = line
            .chars()
            .map(|letter| match letter.is_control() {
                true => ' ',
                false => letter,
            })
            .collect();
        self.replace(&line)
    }

    /// Takes the selection out and gives it, for the clipboard; `None`
    /// when nothing is selected.
    pub fn cut(&mut self) -> Option<String> {
        let selected = self.selected().to_string();
        self.replace("").then_some(selected)
    }

    /// Puts the caret at `at`, a position in the text: the selection goes,
    /// or with `extend` reaches from the anchor to there. What changed.
    pub fn place(&mut self, at: usize, extend: bool) -> Edited {
        let before = (self.caret, self.anchor);
        self.caret = at.min(self.length());
        if !extend {
            self.anchor = self.caret;
        }
        match (self.caret, self.anchor) == before {
            true => Edited::Nothing,
            false => Edited::Caret,
        }
    }

    /// Selects the word at `at`, or the spaces or marks there: a
    /// double-click. Past the end, the last one.
    pub fn select_word(&mut self, at: usize) -> Edited {
        let letters: Vec<char> = self.text.chars().collect();
        let Some(here) = at
            .min(letters.len())
            .checked_sub(usize::from(at >= letters.len()))
        else {
            return Edited::Nothing;
        };
        let kind = class(letters[here]);
        let same = |letter: &&char| class(**letter) == kind;
        let start = here - letters[..here].iter().rev().take_while(same).count();
        let end = here + letters[here..].iter().take_while(same).count();
        let before = (self.caret, self.anchor);
        (self.anchor, self.caret) = (start, end);
        match (self.caret, self.anchor) == before {
            true => Edited::Nothing,
            false => Edited::Caret,
        }
    }

    /// The caret, in UTF-16 units.
    pub fn caret_utf16(&self) -> u32 {
        utf16_offset(&self.text, self.caret)
    }

    /// The selection, in UTF-16 units.
    pub fn selection_utf16(&self) -> Range<u32> {
        let Range { start, end } = self.selection();
        utf16_offset(&self.text, start)..utf16_offset(&self.text, end)
    }

    fn length(&self) -> usize {
        self.text.chars().count()
    }

    /// The byte where the character at `at` starts.
    fn byte(&self, at: usize) -> usize {
        let mut starts = self.text.char_indices().map(|(byte, _)| byte);
        starts.nth(at).unwrap_or(self.text.len())
    }

    /// Where `motion` takes the caret, the selection kept.
    fn target(&self, motion: Motion) -> usize {
        let letters: Vec<char> = self.text.chars().collect();
        match motion {
            Motion::Left => self.caret.saturating_sub(1),
            Motion::Right => (self.caret + 1).min(letters.len()),
            Motion::WordLeft => word_left(&letters, self.caret),
            Motion::WordRight => word_right(&letters, self.caret),
            Motion::Home => 0,
            Motion::End => letters.len(),
        }
    }

    /// Where `motion` takes the caret when the selection goes: a letter
    /// left or right of a selection is its edge on that side.
    fn moved(&self, motion: Motion) -> usize {
        let Range { start, end } = self.selection();
        match motion {
            Motion::Left if start < end => start,
            Motion::Right if start < end => end,
            motion => self.target(motion),
        }
    }

    /// Takes out the selection, or what lies between the caret and where
    /// `motion` goes.
    fn erase(&mut self, motion: Motion) -> Edited {
        if self.caret == self.anchor {
            self.anchor = self.target(motion);
        }
        match self.replace("") {
            true => Edited::Text,
            false => Edited::Nothing,
        }
    }

    /// Types `letter`, if there is one, in place of the selection.
    fn insert(&mut self, letter: Option<char>) -> bool {
        letter.is_some_and(|letter| self.replace(letter.encode_utf8(&mut [0; 4])))
    }

    /// Puts `with` in place of the selection, the caret after it; whether
    /// the text changed.
    fn replace(&mut self, with: &str) -> bool {
        let Range { start, end } = self.selection();
        let (from, to) = (self.byte(start), self.byte(end));
        self.text.replace_range(from..to, with);
        self.caret = start + with.chars().count();
        self.anchor = self.caret;
        start < end || !with.is_empty()
    }
}

const BACKSPACE: u16 = 0x08;
const CTRL_BACKSPACE: u16 = 0x7F;

fn decode_pair(high: u16, low: u16) -> Option<char> {
    char::decode_utf16([high, low]).next()?.ok()
}

/// What a character is, for moving by words: a word is a run of letters
/// and digits, or of other marks, between spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Mark,
}

fn class(letter: char) -> Class {
    match letter {
        _ if letter.is_whitespace() => Class::Space,
        _ if letter.is_alphanumeric() || letter == '_' => Class::Word,
        _ => Class::Mark,
    }
}

/// The start of the word before `at`: past the spaces before it, then
/// the word.
fn word_left(letters: &[char], at: usize) -> usize {
    let before = &letters[..at];
    let spaces = before
        .iter()
        .rev()
        .take_while(|&&letter| class(letter) == Class::Space);
    let at = at - spaces.count();
    let Some(&last) = letters[..at].last() else {
        return at;
    };
    let word = letters[..at]
        .iter()
        .rev()
        .take_while(|&&letter| class(letter) == class(last));
    at - word.count()
}

/// The start of the next word: past the word at `at`, then the spaces.
fn word_right(letters: &[char], at: usize) -> usize {
    let after = &letters[at..];
    let word = after
        .first()
        .filter(|&&letter| class(letter) != Class::Space)
        .map_or(0, |&first| {
            after
                .iter()
                .take_while(|&&letter| class(letter) == class(first))
                .count()
        });
    let at = at + word;
    at + letters[at..]
        .iter()
        .take_while(|&&letter| class(letter) == Class::Space)
        .count()
}

/// How far a line `width` DIPs wide is scrolled left in a view `room`
/// wide, so that the caret, `caret` DIPs from the line's start, shows:
/// as far as before, unless the caret went past either edge; never so
/// far that the view has room to spare after the line's end.
pub fn scrolled(scroll: f32, caret: f32, width: f32, room: f32) -> f32 {
    let room = room.max(0.0);
    let most = (width - room).max(0.0);
    scroll.clamp(caret - room, caret).clamp(0.0, most)
}

/// Whether a click at `at` is the third of a triple-click, after the
/// double-click `double` (when and where it was): soon enough after it,
/// and within the box `room` (width, height) Windows centres on it.
pub fn third_click(
    double: Option<(Instant, (i32, i32))>,
    now: Instant,
    at: (i32, i32),
    soon: Duration,
    room: (i32, i32),
) -> bool {
    double.is_some_and(|(when, (x, y))| {
        let near = (at.0 - x).abs() <= room.0 / 2 && (at.1 - y).abs() <= room.1 / 2;
        near && now.saturating_duration_since(when) <= soon
    })
}

/// The UTF-16 offset of the character at `at` in `text`.
pub fn utf16_offset(text: &str, at: usize) -> u32 {
    text.chars()
        .take(at)
        .map(|letter| letter.len_utf16() as u32)
        .sum()
}

/// The character at the UTF-16 offset `offset` in `text`; an offset in
/// the middle of a pair is the character it is in, past the end the end.
pub fn char_index(text: &str, offset: u32) -> usize {
    let mut units = 0;
    let before = |letter: &char| {
        units += letter.len_utf16() as u32;
        units <= offset
    };
    text.chars().take_while(before).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Field {
        let mut field = Field::default();
        for unit in text.encode_utf16() {
            field.type_unit(unit);
        }
        field
    }

    /// `text` with the caret at `caret` and the anchor at `anchor`.
    fn field(text: &str, anchor: usize, caret: usize) -> Field {
        let mut field = typed(text);
        field.place(anchor, false);
        field.place(caret, true);
        field
    }

    /// The text with the selection in brackets, or a bar at the caret.
    fn shown(field: &Field) -> String {
        let letters: Vec<char> = field.text().chars().collect();
        let Range { start, end } = field.selection();
        let mut out = String::new();
        for (at, letter) in letters.iter().enumerate() {
            out.push_str(mark(at, start, end));
            out.push(*letter);
        }
        out.push_str(mark(letters.len(), start, end));
        out
    }

    fn mark(at: usize, start: usize, end: usize) -> &'static str {
        match () {
            _ if start == end && at == start => "|",
            _ if start < end && at == start => "[",
            _ if start < end && at == end => "]",
            _ => "",
        }
    }

    fn applied(mut field: Field, edit: Edit) -> String {
        field.apply(edit);
        shown(&field)
    }

    const NONE: Held = Held {
        ctrl: false,
        shift: false,
    };

    const CTRL: Held = Held {
        ctrl: true,
        shift: false,
    };

    const SHIFT: Held = Held {
        ctrl: false,
        shift: true,
    };

    const BOTH: Held = Held {
        ctrl: true,
        shift: true,
    };

    #[test]
    fn collects_latin_and_cyrillic_letters_with_the_caret_after_them() {
        let field = typed("vs код");
        assert_eq!(field.text(), "vs код");
        assert_eq!(shown(&field), "vs код|");
    }

    #[test]
    fn a_letter_typed_goes_in_at_the_caret() {
        let mut field = field("note", 2, 2);
        field.type_unit(u16::from(b'X'));
        assert_eq!(shown(&field), "noX|te");
    }

    #[test]
    fn a_letter_typed_takes_the_place_of_the_selection() {
        let mut field = field("notepad", 4, 7);
        assert!(field.type_unit(u16::from(b's')));
        assert_eq!(shown(&field), "notes|");
    }

    #[test]
    fn backspace_takes_out_the_letter_before_the_caret() {
        let mut field = field("абв", 2, 2);
        assert!(field.type_unit(BACKSPACE));
        assert_eq!(shown(&field), "а|в");
    }

    #[test]
    fn backspace_takes_out_the_selection_alone() {
        let mut field = field("notepad", 1, 3);
        assert!(field.type_unit(BACKSPACE));
        assert_eq!(shown(&field), "n|epad");
    }

    #[test]
    fn backspace_at_the_start_or_on_empty_text_changes_nothing() {
        assert!(!Field::default().type_unit(BACKSPACE));
        let mut field = field("ab", 0, 0);
        assert!(!field.type_unit(BACKSPACE));
        assert_eq!(shown(&field), "|ab");
    }

    #[test]
    fn ctrl_backspace_takes_out_the_word_before_and_the_spaces_after_it() {
        let mut field = typed("open my  docs  ");
        assert!(field.type_unit(CTRL_BACKSPACE));
        assert_eq!(shown(&field), "open my  |");
    }

    #[test]
    fn ctrl_backspace_in_a_word_takes_out_its_start() {
        let mut field = field("notepad plus", 4, 4);
        field.type_unit(CTRL_BACKSPACE);
        assert_eq!(shown(&field), "|pad plus");
    }

    #[test]
    fn ctrl_backspace_on_a_single_word_empties_the_text() {
        let mut field = typed("notepad");
        field.type_unit(CTRL_BACKSPACE);
        assert_eq!(field.text(), "");
    }

    #[test]
    fn ctrl_backspace_on_empty_text_changes_nothing() {
        assert!(!Field::default().type_unit(CTRL_BACKSPACE));
    }

    #[test]
    fn joins_a_surrogate_pair_into_one_character() {
        let field = typed("find 🔍");
        assert_eq!(field.text(), "find 🔍");
        assert_eq!(field.selection(), 6..6);
        assert_eq!(field.caret_utf16(), 7);
    }

    #[test]
    fn the_first_half_of_a_pair_changes_nothing_yet() {
        assert!(!Field::default().type_unit(0xD83D));
    }

    #[test]
    fn drops_a_high_surrogate_not_followed_by_its_pair() {
        let mut field = Field::default();
        field.type_unit(0xD83D);
        field.type_unit(u16::from(b'a'));
        assert_eq!(field.text(), "a");
    }

    #[test]
    fn ignores_a_lone_low_surrogate() {
        let mut field = Field::default();
        assert!(!field.type_unit(0xDD0D));
        assert_eq!(field.text(), "");
    }

    #[test]
    fn ignores_other_control_codes_such_as_ctrl_a() {
        let mut field = typed("x");
        for code in [0x00, 0x01, 0x03, 0x09, 0x0D, 0x16, 0x18, 0x1B, 0x1F] {
            assert!(!field.type_unit(code), "code {code:#04x}");
        }
        assert_eq!(field.text(), "x");
    }

    #[test]
    fn reports_a_change_when_a_letter_is_typed() {
        assert!(Field::default().type_unit(u16::from(b' ')));
    }

    #[test]
    fn clear_forgets_text_caret_selection_and_a_pending_surrogate() {
        let mut field = field("abc", 0, 2);
        field.type_unit(0xD83D);
        field.clear();
        field.type_unit(0xDD0D);
        assert_eq!(shown(&field), "|");
        assert_eq!(field.selection(), 0..0);
    }

    #[test]
    fn left_and_right_move_the_caret_a_letter_and_stop_at_the_ends() {
        assert_eq!(
            applied(field("abc", 1, 1), Edit::Move(Motion::Left)),
            "|abc"
        );
        assert_eq!(
            applied(field("abc", 0, 0), Edit::Move(Motion::Left)),
            "|abc"
        );
        assert_eq!(
            applied(field("abc", 1, 1), Edit::Move(Motion::Right)),
            "ab|c"
        );
        assert_eq!(
            applied(field("abc", 3, 3), Edit::Move(Motion::Right)),
            "abc|"
        );
    }

    #[test]
    fn left_and_right_on_a_selection_go_to_its_edge_on_that_side() {
        assert_eq!(
            applied(field("abcd", 3, 1), Edit::Move(Motion::Left)),
            "a|bcd"
        );
        assert_eq!(
            applied(field("abcd", 1, 3), Edit::Move(Motion::Left)),
            "a|bcd"
        );
        assert_eq!(
            applied(field("abcd", 3, 1), Edit::Move(Motion::Right)),
            "abc|d"
        );
    }

    #[test]
    fn home_and_end_go_to_the_ends() {
        assert_eq!(
            applied(field("abc", 1, 2), Edit::Move(Motion::Home)),
            "|abc"
        );
        assert_eq!(applied(field("abc", 2, 1), Edit::Move(Motion::End)), "abc|");
    }

    #[test]
    fn ctrl_left_goes_to_the_start_of_this_word_or_the_one_before() {
        let left = |at| applied(field("open  my docs", at, at), Edit::Move(Motion::WordLeft));
        assert_eq!(left(13), "open  my |docs");
        assert_eq!(left(11), "open  my |docs");
        assert_eq!(left(9), "open  |my docs");
        assert_eq!(left(6), "|open  my docs");
        assert_eq!(left(5), "|open  my docs");
        assert_eq!(left(0), "|open  my docs");
    }

    #[test]
    fn ctrl_right_goes_to_the_start_of_the_next_word() {
        let right = |at| {
            applied(
                field("open  my docs", at, at),
                Edit::Move(Motion::WordRight),
            )
        };
        assert_eq!(right(0), "open  |my docs");
        assert_eq!(right(2), "open  |my docs");
        assert_eq!(right(4), "open  |my docs");
        assert_eq!(right(9), "open  my docs|");
        assert_eq!(right(13), "open  my docs|");
    }

    #[test]
    fn marks_between_letters_are_words_of_their_own() {
        let right = |at| applied(field(r"C:\Users\me", at, at), Edit::Move(Motion::WordRight));
        assert_eq!(right(0), r"C|:\Users\me");
        assert_eq!(right(1), r"C:\|Users\me");
        let left = |at| applied(field(r"C:\Users\me", at, at), Edit::Move(Motion::WordLeft));
        assert_eq!(left(11), r"C:\Users\|me");
        assert_eq!(left(9), r"C:\Users|\me");
    }

    #[test]
    fn shift_with_a_motion_moves_the_caret_and_keeps_the_anchor() {
        assert_eq!(
            applied(field("abcd", 1, 1), Edit::Select(Motion::Right)),
            "a[b]cd"
        );
        assert_eq!(
            applied(field("abcd", 1, 2), Edit::Select(Motion::Left)),
            "a|bcd"
        );
        assert_eq!(
            applied(field("abcd", 1, 2), Edit::Select(Motion::End)),
            "a[bcd]"
        );
        assert_eq!(
            applied(field("abcd", 2, 2), Edit::Select(Motion::Home)),
            "[ab]cd"
        );
        let words = field("one two", 7, 7);
        assert_eq!(applied(words, Edit::Select(Motion::WordLeft)), "one [two]");
        let words = field("one two", 0, 0);
        assert_eq!(applied(words, Edit::Select(Motion::WordRight)), "[one ]two");
    }

    #[test]
    fn a_selection_reads_the_same_whichever_end_the_caret_is_at() {
        let field = field("notepad", 7, 4);
        assert_eq!(field.selection(), 4..7);
        assert_eq!(field.selected(), "pad");
        assert_eq!(field.caret_utf16(), 4);
    }

    #[test]
    fn select_all_selects_every_letter() {
        assert_eq!(applied(field("abc", 1, 1), Edit::SelectAll), "[abc]");
    }

    #[test]
    fn a_move_says_whether_the_caret_or_the_selection_changed() {
        let mut moved = field("abc", 1, 1);
        assert_eq!(moved.apply(Edit::Move(Motion::Right)), Edited::Caret);
        assert_eq!(moved.apply(Edit::Move(Motion::End)), Edited::Caret);
        assert_eq!(moved.apply(Edit::Move(Motion::End)), Edited::Nothing);
        assert_eq!(moved.apply(Edit::SelectAll), Edited::Caret);
        assert_eq!(moved.apply(Edit::SelectAll), Edited::Nothing);
        let mut collapsed = field("abc", 0, 3);
        assert_eq!(collapsed.apply(Edit::Move(Motion::End)), Edited::Caret);
    }

    #[test]
    fn delete_takes_out_the_letter_after_the_caret_or_the_selection() {
        assert_eq!(
            applied(field("abc", 1, 1), Edit::Erase(Motion::Right)),
            "a|c"
        );
        assert_eq!(
            applied(field("abc", 3, 3), Edit::Erase(Motion::Right)),
            "abc|"
        );
        assert_eq!(
            applied(field("abcd", 3, 1), Edit::Erase(Motion::Right)),
            "a|d"
        );
    }

    #[test]
    fn ctrl_delete_takes_out_up_to_the_next_word() {
        let erased = applied(field("open my docs", 5, 5), Edit::Erase(Motion::WordRight));
        assert_eq!(erased, "open |docs");
        let erased = applied(field("open my docs", 2, 2), Edit::Erase(Motion::WordRight));
        assert_eq!(erased, "op|my docs");
    }

    #[test]
    fn erasing_says_whether_the_text_changed() {
        let mut field = field("ab", 2, 2);
        assert_eq!(field.apply(Edit::Erase(Motion::Right)), Edited::Nothing);
        assert_eq!(field.apply(Edit::Erase(Motion::Left)), Edited::Text);
    }

    #[test]
    fn paste_puts_one_line_in_place_of_the_selection() {
        let mut field = field("open notes", 5, 10);
        assert!(field.paste("my\r\ndocs\nand\rmore\tstuff\r\n"));
        assert_eq!(shown(&field), "open my docs and more stuff|");
    }

    #[test]
    fn pasting_nothing_over_nothing_changes_nothing() {
        let mut field = field("ab", 1, 1);
        assert!(!field.paste(""));
        assert!(!field.paste("\r\n"));
        assert_eq!(shown(&field), "a|b");
        let mut selected = self::field("ab", 0, 1);
        assert!(selected.paste(""), "the selection goes");
        assert_eq!(shown(&selected), "|b");
    }

    #[test]
    fn cut_takes_out_the_selection_and_gives_it() {
        let mut field = field("notepad", 4, 7);
        assert_eq!(field.cut().as_deref(), Some("pad"));
        assert_eq!(shown(&field), "note|");
        assert_eq!(field.cut(), None, "nothing selected");
        assert_eq!(field.text(), "note");
    }

    #[test]
    fn a_click_places_the_caret_and_a_shift_click_selects_to_it() {
        let mut field = field("notepad", 2, 2);
        assert_eq!(field.place(5, false), Edited::Caret);
        assert_eq!(shown(&field), "notep|ad");
        assert_eq!(field.place(1, true), Edited::Caret);
        assert_eq!(shown(&field), "n[otep]ad");
        assert_eq!(field.place(1, true), Edited::Nothing);
        assert_eq!(field.place(99, false), Edited::Caret);
        assert_eq!(shown(&field), "notepad|", "past the end is the end");
    }

    #[test]
    fn a_double_click_selects_the_word_or_the_run_of_spaces_or_marks() {
        let word = |at| {
            let mut field = field(r"open  C:\notes", 0, 0);
            field.select_word(at);
            shown(&field)
        };
        assert_eq!(word(0), r"[open]  C:\notes");
        assert_eq!(word(3), r"[open]  C:\notes");
        assert_eq!(word(4), r"open[  ]C:\notes");
        assert_eq!(word(7), r"open  C[:\]notes");
        assert_eq!(word(10), r"open  C:\[notes]");
        assert_eq!(word(14), r"open  C:\[notes]", "at the end, the last word");
        assert_eq!(word(99), r"open  C:\[notes]");
    }

    #[test]
    fn a_double_click_on_empty_text_selects_nothing() {
        let mut field = Field::default();
        assert_eq!(field.select_word(0), Edited::Nothing);
        let mut same = self::field("ab cd", 0, 2);
        assert_eq!(same.select_word(1), Edited::Nothing, "already selected");
        assert_eq!(same.select_word(4), Edited::Caret);
    }

    #[test]
    fn the_caret_and_the_selection_in_utf16_units_count_pairs_twice() {
        let field = field("a🔍bc", 1, 3);
        assert_eq!(field.selection_utf16(), 1..4);
        assert_eq!(field.caret_utf16(), 4);
    }

    #[test]
    fn a_line_that_fits_is_not_scrolled() {
        assert_eq!(scrolled(0.0, 50.0, 100.0, 600.0), 0.0);
        assert_eq!(scrolled(30.0, 50.0, 100.0, 600.0), 0.0, "it shrank");
    }

    #[test]
    fn the_view_follows_a_caret_past_either_edge_and_stays_otherwise() {
        assert_eq!(scrolled(0.0, 700.0, 900.0, 600.0), 100.0);
        assert_eq!(scrolled(100.0, 400.0, 900.0, 600.0), 100.0);
        assert_eq!(
            scrolled(100.0, 700.0, 900.0, 600.0),
            100.0,
            "just at the edge"
        );
        assert_eq!(scrolled(100.0, 50.0, 900.0, 600.0), 50.0);
    }

    #[test]
    fn the_view_never_leaves_room_after_the_end() {
        assert_eq!(scrolled(300.0, 500.0, 700.0, 600.0), 100.0);
    }

    #[test]
    fn a_view_with_no_room_shows_where_the_caret_is() {
        assert_eq!(scrolled(0.0, 10.0, 100.0, -5.0), 10.0);
    }

    #[test]
    fn a_click_soon_after_a_double_click_and_near_it_is_the_third() {
        let then = Instant::now();
        let soon = Duration::from_millis(500);
        let room = (4, 4);
        let third = |after: u64, at| {
            let now = then + Duration::from_millis(after);
            third_click(Some((then, (100, 30))), now, at, soon, room)
        };
        assert!(third(0, (100, 30)));
        assert!(third(500, (102, 28)), "just in time, at the box's edge");
        assert!(!third(501, (100, 30)), "too late");
        assert!(!third(10, (103, 30)), "too far right");
        assert!(!third(10, (97, 30)), "too far left");
        assert!(!third(10, (100, 33)), "too far down");
        assert!(!third(10, (100, 27)), "too far up");
        assert!(!third_click(None, then, (100, 30), soon, room));
    }

    #[test]
    fn utf16_offsets_and_char_indices_go_both_ways() {
        let text = "a🔍b";
        assert_eq!(utf16_offset(text, 0), 0);
        assert_eq!(utf16_offset(text, 2), 3);
        assert_eq!(utf16_offset(text, 9), 4);
        assert_eq!(char_index(text, 0), 0);
        assert_eq!(char_index(text, 1), 1);
        assert_eq!(char_index(text, 2), 1, "inside the pair");
        assert_eq!(char_index(text, 3), 2);
        assert_eq!(char_index(text, 4), 3);
        assert_eq!(char_index(text, 9), 3, "past the end");
    }

    #[test]
    fn the_arrows_home_and_end_move_and_with_shift_select() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_END, VK_HOME};
        let moves = |motion| Some(Key::Edit(Edit::Move(motion)));
        let selects = |motion| Some(Key::Edit(Edit::Select(motion)));
        assert_eq!(key(VK_LEFT, NONE), moves(Motion::Left));
        assert_eq!(key(VK_RIGHT, NONE), moves(Motion::Right));
        assert_eq!(key(VK_LEFT, CTRL), moves(Motion::WordLeft));
        assert_eq!(key(VK_RIGHT, CTRL), moves(Motion::WordRight));
        assert_eq!(key(VK_HOME, NONE), moves(Motion::Home));
        assert_eq!(key(VK_END, CTRL), moves(Motion::End));
        assert_eq!(key(VK_LEFT, SHIFT), selects(Motion::Left));
        assert_eq!(key(VK_RIGHT, BOTH), selects(Motion::WordRight));
        assert_eq!(key(VK_HOME, SHIFT), selects(Motion::Home));
        assert_eq!(key(VK_END, BOTH), selects(Motion::End));
    }

    #[test]
    fn delete_erases_a_letter_or_with_ctrl_a_word() {
        let erases = |motion| Some(Key::Edit(Edit::Erase(motion)));
        assert_eq!(key(VK_DELETE, NONE), erases(Motion::Right));
        assert_eq!(key(VK_DELETE, CTRL), erases(Motion::WordRight));
        assert_eq!(key(VK_DELETE, BOTH), erases(Motion::WordRight));
    }

    #[test]
    fn ctrl_a_c_x_v_select_all_copy_cut_and_paste() {
        assert_eq!(key(VK_A, CTRL), Some(Key::Edit(Edit::SelectAll)));
        assert_eq!(key(VK_C, CTRL), Some(Key::Copy));
        assert_eq!(key(VK_X, CTRL), Some(Key::Cut));
        assert_eq!(key(VK_V, CTRL), Some(Key::Paste));
        assert_eq!(key(VK_V, BOTH), Some(Key::Paste));
    }

    #[test]
    fn windows_older_clipboard_keys_work_too() {
        assert_eq!(key(VK_INSERT, CTRL), Some(Key::Copy));
        assert_eq!(key(VK_INSERT, SHIFT), Some(Key::Paste));
        assert_eq!(key(VK_DELETE, SHIFT), Some(Key::Cut));
        assert_eq!(key(VK_INSERT, NONE), None);
    }

    #[test]
    fn letters_alone_are_typing_and_other_keys_do_nothing_here() {
        for letter in [VK_A, VK_C, VK_X, VK_V] {
            assert_eq!(key(letter, NONE), None, "{letter:?}");
            assert_eq!(key(letter, SHIFT), None, "{letter:?}");
        }
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_B, VK_DOWN, VK_RETURN};
        for other in [VK_B, VK_DOWN, VK_RETURN] {
            assert_eq!(key(other, CTRL), None, "{other:?}");
            assert_eq!(key(other, NONE), None, "{other:?}");
        }
    }
}
