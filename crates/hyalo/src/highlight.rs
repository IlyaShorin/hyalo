//! Which parts of a title to highlight, in the units DirectWrite counts.
//!
//! `hyalo-core` reports the matched characters as `char` indices.
//! DirectWrite addresses text in UTF-16 code units, where a character
//! outside the Basic Multilingual Plane (an emoji) takes two.

use std::ops::Range;

/// The UTF-16 ranges of the characters at `positions` (ascending `char`
/// indices) in `text`. Neighbouring characters share one range.
pub fn utf16_ranges(text: &str, positions: &[usize]) -> Vec<Range<u32>> {
    let mut wanted = positions.iter().copied().peekable();
    let mut ranges: Vec<Range<u32>> = Vec::new();
    let mut at = 0;
    for (index, letter) in text.chars().enumerate() {
        let width = letter.len_utf16() as u32;
        if wanted.next_if_eq(&index).is_some() {
            match ranges.last_mut() {
                Some(last) if last.end == at => last.end += width,
                _ => ranges.push(at..at + width),
            }
        }
        at += width;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ranges as (start, end) pairs, easy to write in a test.
    fn ranges(text: &str, positions: &[usize]) -> Vec<(u32, u32)> {
        let ranges = utf16_ranges(text, positions).into_iter();
        ranges.map(|range| (range.start, range.end)).collect()
    }

    #[test]
    fn nothing_matched_highlights_nothing() {
        assert_eq!(ranges("Notepad", &[]), []);
    }

    #[test]
    fn each_matched_letter_gets_its_range() {
        assert_eq!(
            ranges("Visual Studio Code", &[0, 7, 14]),
            [(0, 1), (7, 8), (14, 15)]
        );
    }

    #[test]
    fn neighbouring_letters_share_one_range() {
        assert_eq!(ranges("Notepad", &[0, 1, 2, 4]), [(0, 3), (4, 5)]);
    }

    #[test]
    fn cyrillic_letters_take_one_unit_each() {
        assert_eq!(ranges("Блокнот", &[1, 2]), [(1, 3)]);
    }

    #[test]
    fn an_emoji_takes_two_units_and_shifts_what_follows() {
        assert_eq!(ranges("🔍 Find", &[0, 2]), [(0, 2), (3, 4)]);
    }

    #[test]
    fn positions_past_the_end_are_ignored() {
        assert_eq!(ranges("ab", &[1, 5]), [(1, 2)]);
    }
}
