//! Arithmetic typed into the search bar: "2+2*2", "2,5 * 4".
//!
//! The grammar, loosest first:
//!
//! ```text
//! sum     = product (("+" | "-") product)*
//! product = signed (("*" | "/") signed)*
//! signed  = "-" signed | power
//! power   = atom ("^" signed)?
//! atom    = number | "(" sum ")"
//! ```
//!
//! A number takes a point or a comma before its decimals ("2.5", "2,5"),
//! as the user writes them in English or Russian. Spaces do not matter.

use std::iter::Peekable;
use std::str::Chars;

/// The value of `text` read as arithmetic; `None` unless all of it is
/// one whole expression with a finite value.
pub fn evaluate(text: &str) -> Option<f64> {
    let mut parser = Parser {
        tokens: tokens(text)?,
        at: 0,
    };
    let value = parser.sum()?;
    let whole = parser.at == parser.tokens.len();
    (whole && value.is_finite()).then_some(value)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number(f64),
    /// An operator or a parenthesis.
    Symbol(char),
}

const SYMBOLS: &str = "+-*/^()";

/// The tokens of `text`; `None` at a character that starts none.
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(&next) = chars.peek() {
        if next.is_whitespace() {
            chars.next();
            continue;
        }
        let token = match SYMBOLS.contains(next) {
            true => Token::Symbol(chars.next()?),
            false => number(&mut chars)?,
        };
        tokens.push(token);
    }
    Some(tokens)
}

/// Digits with a point or a comma among them; `None` for anything else,
/// such as a second point ("2.5.1") or no digits at all.
fn number(chars: &mut Peekable<Chars>) -> Option<Token> {
    let mut text = String::new();
    while let Some(digit) = chars.next_if(|&next| next.is_ascii_digit() || ".,".contains(next)) {
        text.push(match digit {
            ',' => '.',
            other => other,
        });
    }
    text.parse().ok().map(Token::Number)
}

/// Reads the tokens from `at` on, one level of the grammar per method.
struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    /// Steps past `symbol` if it comes next.
    fn eat(&mut self, symbol: char) -> bool {
        let found = self.tokens.get(self.at) == Some(&Token::Symbol(symbol));
        self.at += usize::from(found);
        found
    }

    fn sum(&mut self) -> Option<f64> {
        let mut total = self.product()?;
        loop {
            let sign = if self.eat('+') {
                1.0
            } else if self.eat('-') {
                -1.0
            } else {
                return Some(total);
            };
            total += sign * self.product()?;
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut total = self.signed()?;
        loop {
            if self.eat('*') {
                total *= self.signed()?;
            } else if self.eat('/') {
                total /= self.signed()?;
            } else {
                return Some(total);
            }
        }
    }

    fn signed(&mut self) -> Option<f64> {
        match self.eat('-') {
            true => Some(-self.signed()?),
            false => self.power(),
        }
    }

    /// A power, stacking from the right: 2^3^2 is 2^9. Its exponent may
    /// carry a sign (2^-1), its base may not: -2^2 is -(2^2).
    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        match self.eat('^') {
            true => Some(base.powf(self.signed()?)),
            false => Some(base),
        }
    }

    fn atom(&mut self) -> Option<f64> {
        let token = *self.tokens.get(self.at)?;
        self.at += 1;
        match token {
            Token::Number(number) => Some(number),
            Token::Symbol('(') => {
                let inner = self.sum()?;
                self.eat(')').then_some(inner)
            }
            Token::Symbol(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> f64 {
        evaluate(text).unwrap_or_else(|| panic!("{text:?} has no value"))
    }

    #[test]
    fn adds_subtracts_multiplies_and_divides() {
        assert_eq!(value("2+3"), 5.0);
        assert_eq!(value("7-10"), -3.0);
        assert_eq!(value("6*7"), 42.0);
        assert_eq!(value("1/4"), 0.25);
    }

    #[test]
    fn multiplies_and_divides_before_adding_and_subtracting() {
        assert_eq!(value("2+2*2"), 6.0);
        assert_eq!(value("2*3+4"), 10.0);
        assert_eq!(value("10-4/2"), 8.0);
    }

    #[test]
    fn works_left_to_right_within_a_level() {
        assert_eq!(value("10-3-2"), 5.0);
        assert_eq!(value("100/10/5"), 2.0);
    }

    #[test]
    fn parentheses_come_first() {
        assert_eq!(value("(2+2)*2"), 8.0);
        assert_eq!(value("2*(3+(4-1))"), 12.0);
        assert_eq!(value("((1))"), 1.0);
    }

    #[test]
    fn a_minus_sign_negates_what_follows() {
        assert_eq!(value("-3+5"), 2.0);
        assert_eq!(value("2*-3"), -6.0);
        assert_eq!(value("-(2+3)"), -5.0);
        assert_eq!(value("--2"), 2.0);
    }

    #[test]
    fn raises_to_a_power_before_multiplying() {
        assert_eq!(value("2^10"), 1024.0);
        assert_eq!(value("2*3^2"), 18.0);
        assert_eq!(value("(1+1)^3"), 8.0);
    }

    #[test]
    fn powers_stack_from_the_right() {
        assert_eq!(value("2^3^2"), 512.0);
    }

    #[test]
    fn a_power_binds_tighter_than_a_minus_sign() {
        assert_eq!(value("-2^2"), -4.0);
        assert_eq!(value("2^-1"), 0.5);
    }

    #[test]
    fn a_power_too_large_for_a_number_has_no_value() {
        assert_eq!(evaluate("10^400"), None);
    }

    #[test]
    fn decimals_take_a_point_or_a_comma() {
        assert_eq!(value("2,5*2"), 5.0);
        assert_eq!(value("2.5+0,5"), 3.0);
        assert_eq!(value(".5+,25"), 0.75);
    }

    #[test]
    fn spaces_do_not_matter() {
        assert_eq!(value(" 2 +\t2 "), 4.0);
    }

    #[test]
    fn text_that_is_not_one_whole_expression_has_no_value() {
        for text in [
            "", "2+", "*2", "+", "()", "(2+3", "2+3)", "2 3", "notepad", "7-zip", "2x", "2.5.1",
            "1,000.5", "2 ,5",
        ] {
            assert_eq!(evaluate(text), None, "{text:?}");
        }
    }

    #[test]
    fn dividing_by_zero_has_no_value() {
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("0/0"), None);
    }
}
