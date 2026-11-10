//! The Bank of Russia's official exchange rates: what one unit of each
//! currency costs in roubles on a date.
//!
//! They come from the bank's daily XML (`XML_daily.asp`): a `ValCurs`
//! with the rates' date and one `Valute` per currency, holding its
//! `CharCode`, its `Nominal` (how many units the rate is for: 100 yen)
//! and its `Value` in roubles with a decimal comma. The file is in
//! windows-1251, but every field read here is ASCII, so the Cyrillic
//! names are skipped without decoding them.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// A currency's ISO 4217 code: three capital Latin letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Code([u8; 3]);

impl Code {
    pub const RUB: Code = Code(*b"RUB");
    pub const USD: Code = Code(*b"USD");

    /// `text` as a code, if it is three capital Latin letters.
    pub fn new(text: &str) -> Option<Code> {
        let letters: [u8; 3] = text.as_bytes().try_into().ok()?;
        letters
            .iter()
            .all(u8::is_ascii_uppercase)
            .then_some(Code(letters))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("a code is ASCII letters")
    }
}

/// A calendar day, as the bank dates its rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// A date written as the bank and the user write it: "10.11.2026".
    pub fn parse(text: &str) -> Option<Date> {
        let [day, month, year] = text.split('.').collect::<Vec<_>>()[..] else {
            return None;
        };
        let date = Date {
            year: digits(year, 4)?,
            month: digits(month, 2)?,
            day: digits(day, 2)?,
        };
        let real = (1..=12).contains(&date.month) && (1..=31).contains(&date.day);
        real.then_some(date)
    }

    /// The day in Moscow `unix_seconds` after 1970 began: the bank dates
    /// its rates by Moscow time, UTC+3 all year round since 2014.
    pub fn moscow(unix_seconds: u64) -> Date {
        const MOSCOW: u64 = 3 * 3_600;
        civil((unix_seconds + MOSCOW) / 86_400)
    }
}

/// `text` as a number, if it is exactly `count` ASCII digits.
fn digits<T: FromStr>(text: &str, count: usize) -> Option<T> {
    let all = text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit());
    all.then(|| text.parse().ok()).flatten()
}

/// The date `days` after 1 January 1970: Howard Hinnant's
/// `civil_from_days`. It counts years from 1 March, so a leap day ends
/// its year, and in eras of 400 years, which repeat exactly.
fn civil(days: u64) -> Date {
    let from_march_0000 = days + 719_468;
    let era = from_march_0000 / 146_097;
    let day_of_era = from_march_0000 % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = (month_from_march + 2) % 12 + 1;
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    Date {
        year: year as u16,
        month: month as u8,
        day: day as u8,
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:02}.{:02}.{:04}", self.day, self.month, self.year)
    }
}

/// The rates of one day: roubles per unit of each currency the bank
/// publishes. The rouble itself is not listed; it is worth one rouble.
#[derive(Debug, Clone, PartialEq)]
pub struct Rates {
    date: Date,
    roubles: BTreeMap<Code, f64>,
}

impl Rates {
    /// The rates in the bank's daily XML; `None` unless it is all there
    /// and well formed: a date, at least one currency, and for each a
    /// code, a whole nominal above zero and a value above zero.
    pub fn parse(xml: &[u8]) -> Option<Rates> {
        // Bytes past ASCII turn into U+FFFD; the markup and every field
        // read here are ASCII, so they come through as they are.
        let text = String::from_utf8_lossy(xml);
        let (_, body) = text.split_once("<ValCurs")?;
        let (tag, _) = body.split_once('>')?;
        let (_, date) = tag.split_once("Date=\"")?;
        let date = Date::parse(date.split_once('"')?.0)?;
        let valutes = body.split("<Valute").skip(1);
        let roubles = valutes.map(rate).collect::<Option<BTreeMap<_, _>>>()?;
        (!roubles.is_empty()).then_some(Rates { date, roubles })
    }

    /// The day the bank set these rates for.
    pub fn date(&self) -> Date {
        self.date
    }

    /// Whether `code` has a rate: the rouble and what the bank publishes.
    pub fn knows(&self, code: Code) -> bool {
        self.roubles(code).is_some()
    }

    /// `amount` of `from` in `to`, through the rouble; `None` when either
    /// has no rate.
    pub fn convert(&self, amount: f64, from: Code, to: Code) -> Option<f64> {
        Some(amount * self.roubles(from)? / self.roubles(to)?)
    }

    /// Roubles per unit of `code`.
    fn roubles(&self, code: Code) -> Option<f64> {
        match code == Code::RUB {
            true => Some(1.0),
            false => self.roubles.get(&code).copied(),
        }
    }
}

/// One `Valute` element, after its tag name: the currency's code and
/// roubles per unit. `None` unless it is closed and has every field.
fn rate(valute: &str) -> Option<(Code, f64)> {
    let (valute, _) = valute.split_once("</Valute>")?;
    let code = Code::new(field(valute, "CharCode")?)?;
    let nominal: u32 = field(valute, "Nominal")?.parse().ok()?;
    let value = decimal(field(valute, "Value")?)?;
    (nominal > 0).then(|| (code, value / f64::from(nominal)))
}

/// The text of the element `name` inside `element`.
fn field<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let (_, after) = element.split_once(&format!("<{name}>"))?;
    Some(after.split_once(&format!("</{name}>"))?.0)
}

/// A number above zero, with a decimal comma as the bank writes it
/// ("82,7536") or a point.
fn decimal(text: &str) -> Option<f64> {
    let text = text.replace(',', ".");
    let plain = text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.');
    let value: f64 = text.parse().ok()?;
    (plain && value > 0.0).then_some(value)
}

/// Whether to ask the bank for rates `today`, when the newest rates at
/// hand are of `newest` and the bank last answered on `checked` (`None`:
/// never). Rates dated today or later are current. Older ones are worth
/// asking about once a day: on weekends and holidays the bank publishes
/// nothing new, and asking again the same day would get the same rates.
pub fn due(newest: Option<Date>, checked: Option<Date>, today: Date) -> bool {
    let old = newest.is_none_or(|date| date < today);
    old && checked.is_none_or(|day| day < today)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The bank's answer on 10 November 2026, saved as it came.
    const FIXTURE: &[u8] = include_bytes!("../fixtures/cbr-daily-2026-11-10.xml");

    fn code(text: &str) -> Code {
        Code::new(text).unwrap_or_else(|| panic!("{text:?} is a code"))
    }

    fn date(text: &str) -> Date {
        Date::parse(text).unwrap_or_else(|| panic!("{text:?} is a date"))
    }

    /// A daily file of the bank's shape with these `Valute` elements,
    /// each currency's name "Доллар" in windows-1251, as the bank sends it.
    pub(crate) fn daily(date: &str, valutes: &[String]) -> Vec<u8> {
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"windows-1251\"?>\
             <ValCurs Date=\"{date}\" name=\"Foreign Currency Market\">"
        )
        .into_bytes();
        for valute in valutes {
            let pieces: Vec<&[u8]> = valute.as_bytes().split(|&byte| byte == b'@').collect();
            xml.extend(pieces.join(&b"\xc4\xee\xeb\xeb\xe0\xf0"[..]));
        }
        xml.extend(b"</ValCurs>");
        xml
    }

    /// One currency as the bank lists it; `@` marks where its name goes.
    pub(crate) fn valute(code: &str, nominal: &str, value: &str) -> String {
        format!(
            "<Valute ID=\"R01235\"><NumCode>840</NumCode><CharCode>{code}</CharCode>\
             <Nominal>{nominal}</Nominal><Name>@</Name><Value>{value}</Value>\
             <VunitRate>{value}</VunitRate></Valute>"
        )
    }

    /// Rates of 01.02.2026: a dollar for 80 roubles, a euro for 100 and
    /// a hundred yen for 50.
    fn simple() -> Rates {
        let valutes = [
            valute("USD", "1", "80,0"),
            valute("EUR", "1", "100"),
            valute("JPY", "100", "50,0000"),
        ];
        Rates::parse(&daily("01.02.2026", &valutes)).expect("rates")
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("a value");
        assert!(
            (actual - expected).abs() <= expected.abs() * 1e-12,
            "{actual} is not {expected}"
        );
    }

    #[test]
    fn a_code_is_three_capital_latin_letters() {
        assert_eq!(code("USD").as_str(), "USD");
        assert_eq!(Code::RUB.as_str(), "RUB");
        for text in ["usd", "US", "USDX", "", "U5D", "УСД", "uSD"] {
            assert_eq!(Code::new(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_date_reads_and_shows_as_day_month_year() {
        assert_eq!(date("10.11.2026").to_string(), "10.11.2026");
        assert_eq!(date("01.02.2026").to_string(), "01.02.2026");
        assert_eq!(date("31.12.0999").to_string(), "31.12.0999");
    }

    #[test]
    fn what_is_not_a_date_is_refused() {
        let wrong = [
            "",
            "1.11.2026",
            "10.11.26",
            "10-11-2026",
            "10.11.2026 ",
            "aa.11.2026",
            "00.11.2026",
            "32.11.2026",
            "10.00.2026",
            "10.13.2026",
            "+1.11.2026",
            "10.11.+026",
        ];
        for text in wrong {
            assert_eq!(Date::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn dates_order_by_year_then_month_then_day() {
        assert!(date("31.12.2025") < date("01.01.2026"));
        assert!(date("31.01.2026") < date("01.02.2026"));
        assert!(date("01.02.2026") < date("02.02.2026"));
    }

    #[test]
    fn the_day_is_the_day_in_moscow() {
        assert_eq!(Date::moscow(0), date("01.01.1970"));
        // 2023-11-14 21:00 and 22:13:20 UTC: midnight and past it in Moscow.
        assert_eq!(Date::moscow(1_699_995_599), date("14.11.2023"));
        assert_eq!(Date::moscow(1_699_995_600), date("15.11.2023"));
        assert_eq!(Date::moscow(1_700_000_000), date("15.11.2023"));
    }

    #[test]
    fn the_calendar_has_its_leap_days() {
        let midday = |days: u64| Date::moscow(days * 86_400 + 9 * 3_600);
        assert_eq!(midday(10_956), date("31.12.1999"));
        assert_eq!(midday(11_016), date("29.02.2000"));
        assert_eq!(midday(11_017), date("01.03.2000"));
        assert_eq!(midday(19_782), date("29.02.2024"));
        assert_eq!(midday(20_757), date("31.10.2026"));
        assert_eq!(midday(20_818), date("31.12.2026"));
        assert_eq!(midday(47_540), date("28.02.2100"));
        assert_eq!(midday(47_541), date("01.03.2100"));
    }

    #[test]
    fn the_banks_file_reads_as_the_rates_of_its_date() {
        let rates = Rates::parse(FIXTURE).expect("the fixture reads");
        assert_eq!(rates.date(), date("10.11.2026"));
        assert_eq!(rates.roubles.len(), 54);
        assert_close(rates.convert(1.0, Code::USD, Code::RUB), 82.7536);
        assert_close(rates.convert(1.0, code("EUR"), Code::RUB), 92.3944);
        assert_close(rates.convert(10_000.0, code("VND"), Code::RUB), 32.3130);
    }

    #[test]
    fn a_rate_for_many_units_is_divided_down_to_one() {
        assert_close(simple().convert(1.0, code("JPY"), Code::RUB), 0.5);
        assert_close(simple().convert(400.0, Code::RUB, code("JPY")), 800.0);
    }

    #[test]
    fn other_currencies_convert_through_the_rouble() {
        assert_close(simple().convert(10.0, Code::USD, code("EUR")), 8.0);
        assert_close(simple().convert(1.0, code("EUR"), Code::USD), 1.25);
        assert_close(simple().convert(400.0, Code::RUB, Code::USD), 5.0);
        assert_close(simple().convert(100.0, Code::USD, Code::RUB), 8_000.0);
    }

    #[test]
    fn the_rouble_is_worth_a_rouble() {
        assert!(simple().knows(Code::RUB));
        assert_close(simple().convert(7.0, Code::RUB, Code::RUB), 7.0);
    }

    #[test]
    fn a_currency_the_bank_does_not_publish_has_no_rate() {
        let rates = simple();
        assert!(rates.knows(Code::USD));
        assert!(!rates.knows(code("GBP")));
        assert_eq!(rates.convert(1.0, code("GBP"), Code::RUB), None);
        assert_eq!(rates.convert(1.0, Code::RUB, code("GBP")), None);
    }

    #[test]
    fn what_is_not_the_banks_rates_is_refused() {
        let usd = valute("USD", "1", "80,0");
        let refused = [
            b"".to_vec(),
            b"<html><body>Service unavailable</body></html>".to_vec(),
            daily("01.02.2026", &[]),
            daily("1 Feb 2026", std::slice::from_ref(&usd)),
            format!("<ValCurs name=\"Foreign Currency Market\">{usd}</ValCurs>").into_bytes(),
        ];
        for xml in refused {
            assert_eq!(
                Rates::parse(&xml),
                None,
                "{}",
                String::from_utf8_lossy(&xml)
            );
        }
    }

    #[test]
    fn a_currency_listed_wrong_spoils_the_whole_file() {
        let broken = [
            valute("usd", "1", "80,0"),
            valute("USD", "0", "80,0"),
            valute("USD", "-1", "80,0"),
            valute("USD", "1.5", "80,0"),
            valute("USD", "1", "0"),
            valute("USD", "1", "-80"),
            valute("USD", "1", "80,0,0"),
            valute("USD", "1", "inf"),
            valute("USD", "1", "NaN"),
            valute("USD", "1", ""),
            "<Valute><Nominal>1</Nominal><Value>80</Value></Valute>".to_string(),
            "<Valute><CharCode>USD</CharCode><Value>80</Value></Valute>".to_string(),
            "<Valute><CharCode>USD</CharCode><Nominal>1</Nominal></Valute>".to_string(),
            "<Valute><CharCode>USD</CharCode><Nominal>1</Nominal><Value>80".to_string(),
        ];
        for wrong in broken {
            let xml = daily("01.02.2026", &[valute("EUR", "1", "100"), wrong.clone()]);
            assert_eq!(Rates::parse(&xml), None, "{wrong}");
        }
    }

    #[test]
    fn rates_are_asked_for_when_older_than_today_once_a_day() {
        let (yesterday, today) = (date("09.11.2026"), date("10.11.2026"));
        let tomorrow = date("11.11.2026");
        assert!(due(None, None, today), "no rates yet");
        assert!(due(Some(yesterday), None, today));
        assert!(due(Some(yesterday), Some(yesterday), today));
        assert!(!due(Some(yesterday), Some(today), today), "asked today");
        assert!(!due(Some(today), None, today));
        assert!(!due(Some(tomorrow), None, today), "posted a day ahead");
    }
}
