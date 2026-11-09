//! The text typed into the search bar.
//!
//! Windows delivers typed text as UTF-16 code units in `WM_CHAR`, together
//! with control codes: 0x08 for Backspace and 0x7F for Ctrl+Backspace.
//! A character outside the Basic Multilingual Plane (an emoji) arrives as
//! two messages, a surrogate pair.

#[derive(Debug, Default)]
pub struct Query {
    text: String,
    pending_high: Option<u16>,
}

impl Query {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.pending_high = None;
    }

    /// Applies one `WM_CHAR` code unit. Returns whether the text changed.
    pub fn type_unit(&mut self, unit: u16) -> bool {
        let high = self.pending_high.take();
        match unit {
            BACKSPACE => self.text.pop().is_some(),
            CTRL_BACKSPACE => self.delete_word(),
            0xD800..=0xDBFF => {
                self.pending_high = Some(unit);
                false
            }
            0xDC00..=0xDFFF => self.push(high.and_then(|high| decode_pair(high, unit))),
            0..=0x1F => false,
            _ => self.push(char::from_u32(u32::from(unit))),
        }
    }

    fn push(&mut self, letter: Option<char>) -> bool {
        letter.map(|letter| self.text.push(letter)).is_some()
    }

    /// Ctrl+Backspace: drop trailing spaces, then the word before them.
    fn delete_word(&mut self) -> bool {
        let before = self.text.len();
        let trimmed = self.text.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, letter)| letter.is_whitespace())
            .map_or(0, |(at, space)| at + space.len_utf8());
        self.text.truncate(cut);
        self.text.len() < before
    }
}

const BACKSPACE: u16 = 0x08;
const CTRL_BACKSPACE: u16 = 0x7F;

fn decode_pair(high: u16, low: u16) -> Option<char> {
    char::decode_utf16([high, low]).next()?.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Query {
        let mut query = Query::default();
        for unit in text.encode_utf16() {
            query.type_unit(unit);
        }
        query
    }

    #[test]
    fn collects_latin_and_cyrillic_letters() {
        assert_eq!(typed("vs код").text(), "vs код");
    }

    #[test]
    fn backspace_removes_the_last_letter() {
        let mut query = typed("абв");
        assert!(query.type_unit(BACKSPACE));
        assert_eq!(query.text(), "аб");
    }

    #[test]
    fn backspace_on_empty_text_changes_nothing() {
        assert!(!Query::default().type_unit(BACKSPACE));
    }

    #[test]
    fn ctrl_backspace_removes_the_last_word_and_the_spaces_after_it() {
        let mut query = typed("open my  docs  ");
        assert!(query.type_unit(CTRL_BACKSPACE));
        assert_eq!(query.text(), "open my  ");
    }

    #[test]
    fn ctrl_backspace_on_a_single_word_empties_the_text() {
        let mut query = typed("notepad");
        query.type_unit(CTRL_BACKSPACE);
        assert_eq!(query.text(), "");
    }

    #[test]
    fn ctrl_backspace_on_empty_text_changes_nothing() {
        assert!(!Query::default().type_unit(CTRL_BACKSPACE));
    }

    #[test]
    fn joins_a_surrogate_pair_into_one_character() {
        assert_eq!(typed("find 🔍").text(), "find 🔍");
    }

    #[test]
    fn the_first_half_of_a_pair_changes_nothing_yet() {
        assert!(!Query::default().type_unit(0xD83D));
    }

    #[test]
    fn drops_a_high_surrogate_not_followed_by_its_pair() {
        let mut query = Query::default();
        query.type_unit(0xD83D);
        query.type_unit(u16::from(b'a'));
        assert_eq!(query.text(), "a");
    }

    #[test]
    fn ignores_a_lone_low_surrogate() {
        let mut query = Query::default();
        assert!(!query.type_unit(0xDD0D));
        assert_eq!(query.text(), "");
    }

    #[test]
    fn ignores_other_control_codes() {
        let mut query = typed("x");
        for code in [0x00, 0x01, 0x09, 0x0D, 0x1B, 0x1F] {
            assert!(!query.type_unit(code), "code {code:#04x}");
        }
        assert_eq!(query.text(), "x");
    }

    #[test]
    fn reports_a_change_when_a_letter_is_typed() {
        assert!(Query::default().type_unit(u16::from(b' ')));
    }

    #[test]
    fn clear_forgets_text_and_a_pending_surrogate() {
        let mut query = typed("abc");
        query.type_unit(0xD83D);
        query.clear();
        query.type_unit(0xDD0D);
        assert_eq!(query.text(), "");
    }
}
