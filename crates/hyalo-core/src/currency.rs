//! Currency conversion as a calculator answer: "100 usd" answers what a
//! hundred dollars cost in roubles, at the Bank of Russia's rate, and
//! Enter copies it.
//!
//! A query is an amount and a currency, either way round, with or
//! without a space, in any case: "100 usd", "usd 100", "$100", "100$",
//! "50 долларов". The amount may be arithmetic: "(100+50) usd". Any
//! currency but the rouble converts to roubles, roubles to dollars;
//! "in" or "в" and a currency names another target: "10 usd in eur",
//! "10$ в евро".
//!
//! A currency is its code (any the bank publishes), its sign or a word
//! for it. A code or a word of three letters or more also counts typed
//! in the wrong keyboard layout ("гыв" is "usd").

use crate::calculator::{grouped, mark, written};
use crate::expression;
use crate::item::{Action, Item, ItemId, Kind};
use crate::keyboard;
use crate::rates::{Code, Rates};
use crate::text::{Language, Text};

/// The currency row's id: there is one at a time.
pub const ID: &str = "hyalo:currency";

/// The words before the target currency, and the same keys typed in the
/// other layout.
const INTO: [&str; 4] = ["in", "в", "шт", "d"];

/// Signs, read in a query and shown after an amount. Left out: `¥` is
/// the yen and the yuan alike, `kr` the crowns of four countries.
const SIGNS: [(&str, &str); 8] = [
    ("₽", "RUB"),
    ("$", "USD"),
    ("€", "EUR"),
    ("£", "GBP"),
    ("₸", "KZT"),
    ("₺", "TRY"),
    ("₴", "UAH"),
    ("₹", "INR"),
];

/// Words for currencies, in lowercase. Not "pound": it weighs too.
const WORDS: [(&[&str], &str); 4] = [
    (
        &[
            "р",
            "руб",
            "рубль",
            "рубля",
            "рублей",
            "рубли",
            "рублях",
            "rouble",
            "roubles",
            "ruble",
            "rubles",
        ],
        "RUB",
    ),
    (
        &[
            "доллар",
            "доллара",
            "долларов",
            "доллары",
            "долларах",
            "бакс",
            "бакса",
            "баксов",
            "баксы",
            "dollar",
            "dollars",
        ],
        "USD",
    ),
    (&["евро", "euro", "euros"], "EUR"),
    (&["юань", "юаня", "юаней", "юани", "юанях", "yuan"], "CNY"),
];

/// The row converting what `query` asks, at `rates`, its second line in
/// `language`; `None` unless it asks for an amount of a currency both it
/// and the target have rates.
pub fn answer(query: &str, rates: &Rates, language: Language) -> Option<Item> {
    let question = question(query, rates)?;
    let value = rates.convert(question.amount, question.from, question.to)?;
    let mark = mark(query);
    let copied = money(value.is_finite().then_some(value)?, mark);
    let asked = grouped(&written(question.amount, mark));
    let (from, to) = (question.from.as_str(), question.to.as_str());
    Some(Item {
        id: ItemId(ID.to_string()),
        title: format!("{}\u{A0}{}", grouped(&copied), sign(&question.to)),
        subtitle: language.fill(
            Text::Currency,
            &[&asked, from, to, &rates.date().to_string()],
        ),
        keywords: Vec::new(),
        kind: Kind::Calculator,
        action: Action::CopyText(copied),
    })
}

/// What a query asks: `amount` of `from` in `to`.
struct Question {
    amount: f64,
    from: Code,
    to: Code,
}

/// The question in `query`. Without a target named, the rouble goes to
/// the dollar and everything else to the rouble.
fn question(query: &str, rates: &Rates) -> Option<Question> {
    let query = query.trim().to_lowercase();
    let (source, to) = match target(&query, rates) {
        Some((source, to)) => (source, Some(to)),
        None => (query.as_str(), None),
    };
    let (amount, from) = priced(source, rates)?;
    let to = to.unwrap_or(match from == Code::RUB {
        true => Code::USD,
        false => Code::RUB,
    });
    Some(Question { amount, from, to })
}

/// "10 usd in eur": what comes before "in", and the currency after it.
fn target<'a>(query: &'a str, rates: &Rates) -> Option<(&'a str, Code)> {
    let (rest, name) = query.rsplit_once(char::is_whitespace)?;
    let (source, into) = rest.trim_end().rsplit_once(char::is_whitespace)?;
    INTO.contains(&into).then_some(())?;
    Some((source.trim_end(), currency(name, rates)?))
}

/// An amount and its currency, the currency after it or before it:
/// "100 usd", "100$", "$100", "usd 100".
fn priced(text: &str, rates: &Rates) -> Option<(f64, Code)> {
    let amount_first = text.trim_end_matches(naming);
    let name_after = &text[amount_first.len()..];
    let name_first = text.trim_start_matches(naming);
    let name_before = &text[..text.len() - name_first.len()];
    let reading = |amount, name| Some((expression::evaluate(amount)?, currency(name, rates)?));
    reading(amount_first, name_after).or_else(|| reading(name_first, name_before))
}

/// Whether `letter` can be part of a currency's name.
fn naming(letter: char) -> bool {
    letter.is_alphabetic() || SIGNS.iter().any(|(sign, _)| sign.starts_with(letter))
}

/// The currency `name` names, as typed or, from three letters on, typed
/// in the other layout. A shorter one is too likely meant as typed.
fn currency(name: &str, rates: &Rates) -> Option<Code> {
    let swapped = || -> Option<String> {
        let long = name.chars().count() >= 3;
        long.then(|| name.chars().map(keyboard::swap).collect())
    };
    named(name, rates).or_else(|| named(&swapped()?, rates))
}

/// The currency `name` is the code, the sign or a word of.
fn named(name: &str, rates: &Rates) -> Option<Code> {
    let code = Code::new(&name.to_ascii_uppercase()).filter(|&code| rates.knows(code));
    let sign = SIGNS.iter().find(|(sign, _)| *sign == name);
    let word = WORDS.iter().find(|(words, _)| words.contains(&name));
    let spelled = sign.map(|(_, code)| code).or(word.map(|(_, code)| code));
    code.or_else(|| Code::new(spelled?))
}

/// How `code` shows after an amount: its sign, or the code itself.
fn sign(code: &Code) -> &str {
    let sign = SIGNS.iter().find(|(_, of)| *of == code.as_str());
    sign.map_or(code.as_str(), |(sign, _)| sign)
}

/// The most decimals an amount shows; anything smaller shows as zero.
const MOST_PLACES: f64 = 15.0;

/// `value` with two decimals, or as many more as a value under one needs
/// to keep three significant digits ("0.0125"), with `mark` before them.
fn money(value: f64, mark: char) -> String {
    let places = 2.0 - value.abs().log10().floor();
    let places = places.clamp(2.0, MOST_PLACES) as usize;
    // Adding 0 turns -0 into 0.
    let text = format!("{:.places$}", value + 0.0);
    let (kept, more) = text.split_at(text.find('.').expect("decimals") + 3);
    let text = kept.to_string() + more.trim_end_matches('0');
    text.replace('.', &mark.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{Action, ItemId, Kind};
    use crate::rates::tests::{daily, valute};

    /// Rates of 10.11.2026: a dollar for 80 roubles, a euro for 100, a
    /// yuan for 10, a pound for 110, a hundred yen for 50 and a hundred
    /// tenge for 20.
    fn rates() -> Rates {
        let valutes = [
            valute("USD", "1", "80"),
            valute("EUR", "1", "100"),
            valute("CNY", "1", "10"),
            valute("GBP", "1", "110"),
            valute("JPY", "100", "50"),
            valute("KZT", "100", "20"),
        ];
        Rates::parse(&daily("10.11.2026", &valutes)).expect("rates")
    }

    /// `text` with its spaces made no-break, as the row shows them.
    fn shown(text: &str) -> String {
        text.replace(' ', "\u{A0}")
    }

    /// What the row for `query` shows and copies.
    fn answered(query: &str) -> (String, String) {
        let item = answer(query, &rates(), Language::English)
            .unwrap_or_else(|| panic!("no answer to {query:?}"));
        let Action::CopyText(copied) = item.action else {
            panic!("{query:?} answers with {:?}", item.action);
        };
        (item.title, copied)
    }

    fn title(query: &str) -> String {
        answered(query).0
    }

    fn subtitle(query: &str) -> String {
        answer(query, &rates(), Language::English)
            .expect("an answer")
            .subtitle
    }

    #[test]
    fn dollars_convert_to_roubles_at_the_banks_rate_of_its_date() {
        let item = answer("100 usd", &rates(), Language::English).expect("an answer");
        assert_eq!(item.id, ItemId("hyalo:currency".to_string()));
        assert_eq!(item.kind, Kind::Calculator);
        assert_eq!(item.title, shown("8 000.00 ₽"));
        assert_eq!(
            item.subtitle,
            "100 USD → RUB · Bank of Russia, 10.11.2026 · Enter to copy"
        );
        assert_eq!(item.action, Action::CopyText("8000.00".to_string()));
        let russian = answer("100 usd", &rates(), Language::Russian).expect("an answer");
        assert_eq!(
            russian.subtitle,
            "100 USD → RUB · Банк России, 10.11.2026 · Enter — скопировать"
        );
    }

    #[test]
    fn roubles_convert_to_dollars() {
        assert_eq!(title("400 rub"), shown("5.00 $"));
        assert_eq!(subtitle("400 rub"), subtitle("400 rub in usd"));
    }

    #[test]
    fn any_other_currency_converts_to_roubles() {
        assert_eq!(title("50 eur"), shown("5 000.00 ₽"));
        assert_eq!(title("100 jpy"), shown("50.00 ₽"));
        assert_eq!(title("3 cny"), shown("30.00 ₽"));
    }

    #[test]
    fn in_or_v_names_the_target() {
        assert_eq!(title("10 usd in eur"), shown("8.00 €"));
        assert_eq!(title("10$ в евро"), shown("8.00 €"));
        assert_eq!(title("1000 руб in usd"), shown("12.50 $"));
        assert_eq!(title("100 usd в рублях"), shown("8 000.00 ₽"));
        assert_eq!(title("1 eur in jpy"), shown("200.00 JPY"));
        assert_eq!(title("1 eur  IN  usd "), shown("1.25 $"));
        assert_eq!(
            subtitle("10 usd in eur").split(" · ").next(),
            Some("10 USD → EUR")
        );
    }

    #[test]
    fn a_currency_is_a_code_a_sign_or_a_word_either_side_in_any_case() {
        let dollars = [
            "100 usd",
            "100 USD",
            "Usd 100",
            "usd100",
            "100usd",
            "$100",
            "100$",
            "100 $",
            "$ 100",
            " 100 usd ",
            "100 доллар",
            "100 доллара",
            "100 долларов",
            "100 Долларов",
            "100 бакс",
            "100 dollars",
            "100 dollar",
        ];
        for query in dollars {
            assert_eq!(title(query), shown("8 000.00 ₽"), "{query:?}");
        }
        for query in ["100€", "€100", "100 евро", "100 EUR", "100 euro"] {
            assert_eq!(title(query), shown("10 000.00 ₽"), "{query:?}");
        }
        for query in [
            "400 rub",
            "₽400",
            "400₽",
            "400 руб",
            "400 рублей",
            "400 рубля",
            "400р",
        ] {
            assert_eq!(title(query), shown("5.00 $"), "{query:?}");
        }
        for query in ["100 cny", "100 юаней", "100 юань", "100 юаня", "100 yuan"] {
            assert_eq!(title(query), shown("1 000.00 ₽"), "{query:?}");
        }
    }

    #[test]
    fn signs_name_their_currency_in_the_query_and_in_the_answer() {
        assert_eq!(title("£1"), shown("110.00 ₽"));
        assert_eq!(title("₸1000"), shown("200.00 ₽"));
        assert_eq!(title("1100 rub in £"), shown("10.00 £"));
        assert_eq!(title("200 rub in kzt"), shown("1 000.00 ₸"));
        assert_eq!(title("1 eur in cny"), shown("10.00 CNY"));
    }

    #[test]
    fn a_currency_without_a_rate_answers_nothing() {
        for query in ["100 xyz", "100 abc", "100 ₺", "10 usd in ₺", "1 TRY"] {
            assert_eq!(
                answer(query, &rates(), Language::English),
                None,
                "{query:?}"
            );
        }
    }

    #[test]
    fn the_amount_can_be_arithmetic() {
        assert_eq!(title("(100+50) usd"), shown("12 000.00 ₽"));
        assert_eq!(
            subtitle("(100+50) usd").split(" · ").next(),
            Some("150 USD → RUB")
        );
        assert_eq!(title("100*3 eur"), shown("30 000.00 ₽"));
        assert_eq!(title("usd sqrt 16"), shown("320.00 ₽"));
        assert_eq!(title("-1 usd"), shown("-80.00 ₽"));
    }

    #[test]
    fn without_both_an_amount_and_a_currency_there_is_no_answer() {
        let questions = [
            "",
            " ",
            "100",
            "usd",
            "$",
            "в евро",
            "100 usd in",
            "in eur",
            "100 in eur",
            "100 usd eur",
            "usd 100 eur",
            "usd usd",
            "100 usd usd",
            "notepad",
            "100 notes",
            "2+2",
            "10 usdin eur",
            "10 usd ineur",
            "10 usd in eur 5",
            "100 $$",
            "1/0 usd",
        ];
        for query in questions {
            assert_eq!(
                answer(query, &rates(), Language::English),
                None,
                "{query:?}"
            );
        }
    }

    #[test]
    fn the_same_currency_on_both_sides_answers_the_amount() {
        assert_eq!(title("100 usd in usd"), shown("100.00 $"));
        assert_eq!(title("100 rub in rub"), shown("100.00 ₽"));
    }

    #[test]
    fn small_results_keep_three_significant_digits() {
        assert_eq!(title("1 rub"), shown("0.0125 $"));
        assert_eq!(title("10 rub"), shown("0.125 $"));
        assert_eq!(title("40 rub"), shown("0.50 $"));
        assert_eq!(title("8 rub"), shown("0.10 $"));
        assert_eq!(title("0.08 rub"), shown("0.001 $"));
        assert_eq!(title("-1 rub"), shown("-0.0125 $"));
        assert_eq!(title("0.0001 rub"), shown("0.00000125 $"));
        assert_eq!(title("0.00000000001 rub"), shown("0.000000000000125 $"));
        assert_eq!(title("0.000000000000001 rub"), shown("0.00 $"), "too small");
    }

    #[test]
    fn usual_results_keep_two_decimals() {
        assert_eq!(title("0.123456 usd"), shown("9.88 ₽"));
        assert_eq!(title("0 usd"), shown("0.00 ₽"));
        assert_eq!(title("0*-1 usd"), shown("0.00 ₽"));
        assert_eq!(title("80 rub"), shown("1.00 $"));
    }

    #[test]
    fn the_decimal_mark_follows_the_query() {
        assert_eq!(
            answered("2,5 usd"),
            (shown("200,00 ₽"), "200,00".to_string())
        );
        assert_eq!(
            subtitle("2,5 usd").split(" · ").next(),
            Some("2,5 USD → RUB")
        );
        assert_eq!(
            answered("2.5 usd"),
            (shown("200.00 ₽"), "200.00".to_string())
        );
    }

    #[test]
    fn thousands_are_grouped_where_shown_but_not_where_copied() {
        let (title, copied) = answered("12345 usd");
        assert_eq!(
            (title, copied),
            (shown("987 600.00 ₽"), "987600.00".to_string())
        );
        let amount = subtitle("12345 usd");
        assert_eq!(amount.split(" · ").next(), Some("12\u{A0}345 USD → RUB"));
    }

    #[test]
    fn codes_and_words_typed_in_the_wrong_layout_still_convert() {
        assert_eq!(title("100 гыв"), shown("8 000.00 ₽"));
        assert_eq!(title("10 гыв шт угк"), shown("8.00 €"));
        assert_eq!(title("10 ljkkfhjd d tdhj"), shown("8.00 €"));
        assert_eq!(title("400 кги"), shown("5.00 $"));
    }

    #[test]
    fn a_short_name_in_the_wrong_layout_is_too_likely_meant_as_typed() {
        assert_eq!(answer("100 h", &rates(), Language::English), None, "р");
        assert_eq!(answer("100 d", &rates(), Language::English), None, "в");
    }
}
