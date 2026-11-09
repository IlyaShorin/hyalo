//! Text typed in the wrong keyboard layout.
//!
//! The user switches between the US English and the Russian (ЙЦУКЕН)
//! layouts and often types a query in the wrong one: "мысщву" for
//! "vscode", "ghbdtn" for "привет". Reading each character as the one the
//! same key gives in the other layout recovers what was meant.

/// The unshifted keys, row by row, in each layout.
const ENGLISH: &str = "`qwertyuiop[]asdfghjkl;'zxcvbnm,./";
const RUSSIAN: &str = "ёйцукенгшщзхъфывапролджэячсмитьбю.";

/// The character the same key gives in the other layout; anything not on
/// the swapped keys comes back unchanged. Expects lowercase: capitals are
/// folded before matching.
///
/// '.' is on two keys: English '.' (Russian 'ю') and English '/' (Russian
/// '.'). It reads as English, so it becomes 'ю': '/' is rare in a query.
pub fn swap(letter: char) -> char {
    across(letter, ENGLISH, RUSSIAN)
        .or_else(|| across(letter, RUSSIAN, ENGLISH))
        .unwrap_or(letter)
}

fn across(letter: char, from: &str, to: &str) -> Option<char> {
    let key = from.chars().position(|key| key == letter)?;
    to.chars().nth(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swapped(text: &str) -> String {
        text.chars().map(swap).collect()
    }

    #[test]
    fn russian_letters_become_the_english_ones_on_the_same_keys() {
        assert_eq!(swapped("мысщву"), "vscode");
        assert_eq!(
            swapped("ёйцукенгшщзхъфывапролджэячсмитьбю"),
            "`qwertyuiop[]asdfghjkl;'zxcvbnm,."
        );
    }

    #[test]
    fn english_letters_become_the_russian_ones_on_the_same_keys() {
        assert_eq!(swapped("ghbdtn"), "привет");
        assert_eq!(swapped("`krf"), "ёлка");
        assert_eq!(swapped("k.,jdm"), "любовь");
    }

    #[test]
    fn the_period_follows_its_key() {
        assert_eq!(swapped("ащщюече"), "foo.txt");
        assert_eq!(swapped("ghbdtn/"), "привет.");
    }

    #[test]
    fn digits_spaces_and_other_keys_stay() {
        assert_eq!(swapped("7 - 2 = 5!"), "7 - 2 = 5!");
    }
}
