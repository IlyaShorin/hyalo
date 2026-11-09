//! Arithmetic typed into the search bar: "2+2*2", "2,5 * 4", "sqrt(2)".
//!
//! The grammar, loosest first:
//!
//! ```text
//! sum     = product (("+" | "-") product)*
//! product = signed (("*" | "/" | "%") signed)*
//! signed  = "-" signed | power
//! power   = percent ("^" signed)?
//! percent = atom "%"?         (when no operand follows the "%")
//! atom    = number | "(" sum ")" | function atom
//! ```
//!
//! "%" reads as calculators read it: before an operand it takes the
//! remainder (7%3 is 1); otherwise it is per cent (50% is 0.5), and a
//! number with a percent sign, standing alone after + or -, is a share of
//! what came before (200+10% is 220).
//!
//! The functions are `sqrt`, `round` (halves away from zero) and `abs`, in
//! any case. A function applies to the atom right after it, so
//! round(2.4)^2 is 4 and "sqrt 2" works without parentheses.
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
    Function(Function),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Function {
    Sqrt,
    Round,
    Abs,
}

impl Function {
    fn named(name: &str) -> Option<Self> {
        match name {
            "sqrt" => Some(Self::Sqrt),
            "round" => Some(Self::Round),
            "abs" => Some(Self::Abs),
            _ => None,
        }
    }

    fn apply(self, number: f64) -> f64 {
        match self {
            Self::Sqrt => number.sqrt(),
            Self::Round => number.round(),
            Self::Abs => number.abs(),
        }
    }
}

const SYMBOLS: &str = "+-*/^%()";

/// The tokens of `text`; `None` at a character that starts none.
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    loop {
        while chars.next_if(|next| next.is_whitespace()).is_some() {}
        let Some(&next) = chars.peek() else {
            return Some(tokens);
        };
        tokens.push(token(next, &mut chars)?);
    }
}

/// The token that starts with `next`.
fn token(next: char, chars: &mut Peekable<Chars>) -> Option<Token> {
    if SYMBOLS.contains(next) {
        chars.next();
        return Some(Token::Symbol(next));
    }
    match next.is_alphabetic() {
        true => function(chars),
        false => number(chars),
    }
}

/// A word that names a function, in any case.
fn function(chars: &mut Peekable<Chars>) -> Option<Token> {
    let mut name = String::new();
    while let Some(letter) = chars.next_if(|next| next.is_alphabetic()) {
        name.extend(letter.to_lowercase());
    }
    Function::named(&name).map(Token::Function)
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
            let start = self.at;
            let term = self.product()?;
            let share = self.at == start + 2 && self.tokens[start + 1] == Token::Symbol('%');
            total += sign * if share { total * term } else { term };
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut total = self.signed()?;
        loop {
            if self.eat('*') {
                total *= self.signed()?;
            } else if self.eat('/') {
                total /= self.signed()?;
            } else if self.eat('%') {
                total %= self.signed()?;
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
        let base = self.percent()?;
        match self.eat('^') {
            true => Some(base.powf(self.signed()?)),
            false => Some(base),
        }
    }

    /// Per cent, unless an operand follows: then "%" takes the remainder.
    fn percent(&mut self) -> Option<f64> {
        let value = self.atom()?;
        let after = self.tokens.get(self.at + 1);
        let operand = matches!(after, Some(Token::Number(_) | Token::Symbol('(')));
        match !operand && self.eat('%') {
            true => Some(value / 100.0),
            false => Some(value),
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
            Token::Function(function) => Some(function.apply(self.atom()?)),
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
    fn a_percent_is_hundredths() {
        assert_eq!(value("50%"), 0.5);
        assert_eq!(value("2*50%"), 1.0);
        assert_eq!(value("25%*4"), 1.0);
        assert_eq!(value("-50%"), -0.5);
    }

    #[test]
    fn a_lone_percent_after_plus_or_minus_is_a_share_of_what_came_before() {
        assert_eq!(value("200+10%"), 220.0);
        assert_eq!(value("200-10%"), 180.0);
        assert_eq!(value("100+50%+50%"), 225.0);
    }

    #[test]
    fn a_percent_with_more_after_it_is_no_share() {
        assert_eq!(value("200+50%*2"), 201.0);
        assert_eq!(value("3+-2"), 1.0);
    }

    #[test]
    fn a_percent_sign_before_an_operand_takes_the_remainder() {
        assert_eq!(value("7%3"), 1.0);
        assert_eq!(value("7 % (2+1)"), 1.0);
        assert_eq!(value("10%4*2"), 4.0);
        assert_eq!(value("200+7%3"), 201.0);
    }

    #[test]
    fn takes_square_roots_rounds_and_drops_signs() {
        assert_eq!(value("sqrt(16)"), 4.0);
        assert_eq!(value("abs(-3)"), 3.0);
        assert_eq!(value("round(2.4)"), 2.0);
    }

    #[test]
    fn rounds_halves_away_from_zero() {
        assert_eq!(value("round(2,5)"), 3.0);
        assert_eq!(value("round(-2.5)"), -3.0);
    }

    #[test]
    fn a_function_applies_to_what_follows_it_directly() {
        assert_eq!(value("2*sqrt(9)+1"), 7.0);
        assert_eq!(value("sqrt 2,25"), 1.5);
        assert_eq!(value("round(2.4)^2"), 4.0);
        assert_eq!(value("abs(-3)-abs(-1)"), 2.0);
    }

    #[test]
    fn function_names_take_any_case() {
        assert_eq!(value("SQRT(4)"), 2.0);
    }

    #[test]
    fn names_that_are_not_functions_have_no_value() {
        for text in ["log(10)", "sqrt", "sqrt()", "sqrt(-1)", "квадрат(4)"] {
            assert_eq!(evaluate(text), None, "{text:?}");
        }
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
