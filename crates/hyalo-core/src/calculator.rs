//! The calculator: a query that is arithmetic gets its answer as a row,
//! and Enter copies the answer.
//!
//! The answer keeps 15 significant digits, as many as a 64-bit float
//! holds reliably, which also hides the binary noise of decimal fractions
//! (0.1+0.2 is 0.3). It takes the decimal mark the query used, so "2,5*3"
//! answers "7,5" for pasting into Russian-locale apps. The row shows the
//! whole part in groups of three; the copied text has no group marks.

use crate::expression;
use crate::item::{Action, Item, ItemId, Kind};

/// The calculator row's id: there is one at a time.
pub const ID: &str = "hyalo:calculator";

/// What a query needs besides numbers to be a question: an operator or
/// a function name. A bare number ("42", "7") only searches, so it does
/// not push "7-Zip" down with an answer that repeats the query.
const OPERATORS: &str = "+-*/^%";

/// The calculator's row for `query`, when the query is arithmetic.
pub fn answer(query: &str) -> Option<Item> {
    let operates = |letter: char| OPERATORS.contains(letter) || letter.is_alphabetic();
    if !query.contains(operates) {
        return None;
    }
    let value = expression::evaluate(query)?;
    let mark = match query.contains(',') {
        true => ',',
        false => '.',
    };
    let copied = written(value, mark);
    Some(Item {
        id: ItemId(ID.to_string()),
        title: grouped(&copied),
        subtitle: "Calculator · Enter to copy".to_string(),
        kind: Kind::Calculator,
        action: Action::CopyText(copied),
    })
}

/// `value` to 15 significant digits with `mark` before the decimals; in
/// exponent form past 15 digits or below a millionth, where plain digits
/// would run long or end in made-up zeros.
fn written(value: f64, mark: char) -> String {
    let rounded: f64 = format!("{value:.14e}")
        .parse()
        .expect("a float reads back what it writes");
    let size = rounded.abs();
    let text = match size != 0.0 && !(1e-6..1e15).contains(&size) {
        true => format!("{rounded:e}"),
        // Adding 0 turns -0 into 0.
        false => (rounded + 0.0).to_string(),
    };
    text.replace('.', mark.encode_utf8(&mut [0; 4]))
}

/// `written` with its whole part in groups of three digits, apart by a
/// narrow no-break space: "1 234 500,5".
fn grouped(written: &str) -> String {
    let (sign, unsigned) = written.split_at(usize::from(written.starts_with('-')));
    let whole = unsigned
        .find(|letter: char| !letter.is_ascii_digit())
        .unwrap_or(unsigned.len());
    let (digits, rest) = unsigned.split_at(whole);
    let mut shown = sign.to_string();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            shown.push('\u{202F}');
        }
        shown.push(digit);
    }
    shown + rest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Action, ItemId, Kind};

    /// The answer as shown and as copied.
    fn answered(query: &str) -> (String, String) {
        let item = answer(query).unwrap_or_else(|| panic!("no answer to {query:?}"));
        let Action::CopyText(copied) = item.action else {
            panic!("{query:?} answers with {:?}", item.action);
        };
        (item.title, copied)
    }

    fn copied(query: &str) -> String {
        answered(query).1
    }

    #[test]
    fn arithmetic_gets_its_answer_to_show_and_to_copy() {
        let item = answer("2+2*2").expect("an answer");
        assert_eq!(item.id, ItemId("hyalo:calculator".to_string()));
        assert_eq!(item.kind, Kind::Calculator);
        assert_eq!(item.title, "6");
        assert_eq!(item.subtitle, "Calculator · Enter to copy");
        assert_eq!(item.action, Action::CopyText("6".to_string()));
    }

    #[test]
    fn a_bare_number_gets_no_answer_so_it_still_just_searches() {
        for query in ["42", "7", "2,5", "(7)", " 3 "] {
            assert_eq!(answer(query), None, "{query:?}");
        }
    }

    #[test]
    fn text_that_is_not_arithmetic_gets_no_answer() {
        for query in ["", "notepad", "7-zip", "2+", "sqrt"] {
            assert_eq!(answer(query), None, "{query:?}");
        }
    }

    #[test]
    fn a_minus_sign_or_a_function_is_arithmetic_enough() {
        assert_eq!(copied("-5"), "-5");
        assert_eq!(copied("sqrt 9"), "3");
    }

    #[test]
    fn binary_noise_in_decimal_fractions_does_not_show() {
        assert_eq!(copied("0.1+0.2"), "0.3");
        assert_eq!(copied("1/3"), "0.333333333333333");
        assert_eq!(copied("2/3"), "0.666666666666667");
    }

    #[test]
    fn the_answer_takes_the_decimal_mark_the_query_used() {
        assert_eq!(copied("2,5*3"), "7,5");
        assert_eq!(copied("2.5*3"), "7.5");
        assert_eq!(copied("1/4"), "0.25");
    }

    #[test]
    fn minus_zero_is_zero() {
        assert_eq!(copied("0*-1"), "0");
    }

    #[test]
    fn numbers_past_fifteen_digits_or_below_a_millionth_take_an_exponent() {
        assert_eq!(copied("2^64"), "1.84467440737096e19");
        assert_eq!(copied("2^64*1,0"), "1,84467440737096e19");
        assert_eq!(copied("10^15"), "1e15");
        assert_eq!(copied("-10^15"), "-1e15");
        assert_eq!(copied("10^15-1"), "999999999999999");
        assert_eq!(copied("1/10^7"), "1e-7");
        assert_eq!(copied("1/10^6"), "0.000001");
    }

    #[test]
    fn thousands_are_grouped_where_shown_but_not_where_copied() {
        let space = '\u{202F}';
        assert_eq!(
            answered("1000*1234,5"),
            (format!("1{space}234{space}500"), "1234500".to_string())
        );
        assert_eq!(
            answered("-1234567+0,5"),
            (
                format!("-1{space}234{space}566,5"),
                "-1234566,5".to_string()
            )
        );
        assert_eq!(answered("999+1").0, format!("1{space}000"));
        assert_eq!(answered("100+1").0, "101");
        assert_eq!(answered("2^64").0, "1.84467440737096e19");
    }
}
