//! Filters typed among a query's words: `ext:pdf`, `in:Downloads`,
//! `size:>10mb`, `mod:7d`. They narrow the files and folders listed; the
//! words left match names as before. A word that only looks like a filter
//! (`size:big`, `http:`) stays a word.

use std::ops::RangeInclusive;

use crate::matcher;

/// A query taken apart: the words that match names, and the filters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    /// The query without its filters; the query as typed when it has none.
    pub words: String,
    pub filters: Filters,
}

/// What a file or folder must be to be listed. Each kind of filter given
/// twice narrows further, but extensions and folders, which add choices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filters {
    /// Extensions, folded as names are (`matcher::folded`), without their
    /// dot: a file with one of them.
    pub extensions: Vec<String>,
    /// Folders, lowercased: by name, anywhere, or by full path. What is
    /// listed is somewhere under one of them.
    pub folders: Vec<String>,
    /// Bytes a file holds.
    pub size: Option<RangeInclusive<u64>>,
    /// When it was last written, in seconds since 1970 (UTC).
    pub written: Option<RangeInclusive<i64>>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        *self == Filters::default()
    }

    /// Whether a folder can be listed at all: extensions and sizes are
    /// files' alone.
    pub fn take_folders(&self) -> bool {
        self.extensions.is_empty() && self.size.is_none()
    }

    /// Whether a name ends in one of the extensions, if any are asked for.
    pub fn extension_of(&self, name: &str) -> bool {
        let extension = name
            .rsplit_once('.')
            .map(|(_, extension)| matcher::folded(extension));
        let same = |wanted: &String| extension.as_ref() == Some(wanted);
        self.extensions.is_empty() || self.extensions.iter().any(same)
    }

    /// Whether `bytes` and a last write at `written` are as asked.
    pub fn sized_and_dated(&self, bytes: u64, written: i64) -> bool {
        let sized = self.size.as_ref().is_none_or(|size| size.contains(&bytes));
        sized
            && self
                .written
                .as_ref()
                .is_none_or(|range| range.contains(&written))
    }
}

/// The time a query is read at: now, and how far local time is ahead of
/// UTC, for `today` and dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// Seconds since 1970, UTC.
    pub now: i64,
    /// Seconds local time is ahead of UTC (Moscow: 3 hours).
    pub offset: i64,
}

impl Query {
    /// `text` taken apart at `clock`.
    pub fn parse(text: &str, clock: Clock) -> Query {
        let mut filters = Filters::default();
        let mut words = Vec::new();
        for token in tokens(text) {
            if !filter(token, clock, &mut filters) {
                words.push(token);
            }
        }
        let words = match filters.is_empty() {
            true => text.to_string(),
            false => words.join(" "),
        };
        Query { words, filters }
    }
}

/// The query's tokens: runs of letters between spaces, a quoted value
/// (`in:"My files"`) taken whole.
fn tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let end = token_end(rest);
        tokens.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    tokens
}

/// Where the token at the start of `text` ends.
fn token_end(text: &str) -> usize {
    let space = text.find(char::is_whitespace).unwrap_or(text.len());
    let quoted = text
        .find(":\"")
        .filter(|&colon| colon < space)
        .and_then(|colon| text[colon + 2..].find('"').map(|close| colon + 3 + close));
    quoted.unwrap_or(space)
}

/// Takes `token` into `filters` if it is a filter that reads; whether it
/// was.
fn filter(token: &str, clock: Clock, filters: &mut Filters) -> bool {
    let Some((key, value)) = token.split_once(':') else {
        return false;
    };
    let value = unquoted(value);
    match key.to_lowercase().as_str() {
        "ext" => extensions(value).map(|more| filters.extensions.extend(more)),
        "in" => folder(value).map(|folder| filters.folders.push(folder)),
        "size" => {
            size(value).map(|range| filters.size = Some(narrowed(filters.size.take(), range)))
        }
        "mod" => written(value, clock)
            .map(|range| filters.written = Some(narrowed(filters.written.take(), range))),
        _ => None,
    }
    .is_some()
}

fn unquoted(value: &str) -> &str {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'));
    inner.unwrap_or(value)
}

/// Both ranges at once: what is in each.
fn narrowed<T: Ord + Copy>(
    before: Option<RangeInclusive<T>>,
    range: RangeInclusive<T>,
) -> RangeInclusive<T> {
    match before {
        None => range,
        Some(before) => *before.start().max(range.start())..=*before.end().min(range.end()),
    }
}

/// `pdf,.docx` as `["pdf", "docx"]`; nothing if any is empty.
fn extensions(value: &str) -> Option<Vec<String>> {
    let each = value
        .split(',')
        .map(|extension| extension.trim().trim_start_matches('.'));
    let good = |extension: &&str| !extension.is_empty() && !extension.contains(['\\', '/', '.']);
    let wanted: Vec<&str> = each.collect();
    wanted.iter().all(good).then(|| {
        wanted
            .iter()
            .map(|extension| matcher::folded(extension))
            .collect()
    })
}

/// A folder's name, or its full path without a backslash at its end.
fn folder(value: &str) -> Option<String> {
    let folder = value
        .trim()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\");
    (!folder.is_empty()).then(|| folder.to_lowercase())
}

/// The times the filter `value` asks for, as `period` reads a value:
/// after the period, from its start, before it, until its end, in it, or
/// from the start of one to the end of another.
fn written(value: &str, clock: Clock) -> Option<RangeInclusive<i64>> {
    let period = |value: &str| period(value, clock);
    if let Some((from, to)) = value.split_once("..") {
        return Some(*period(from)?.start()..=*period(to)?.end());
    }
    let (sign, rest) = sign(value);
    let span = period(rest)?;
    let (start, end) = (*span.start(), *span.end());
    Some(match sign {
        Sign::After => end.checked_add(1)?..=i64::MAX,
        Sign::From => start..=i64::MAX,
        Sign::Before => i64::MIN..=start.checked_sub(1)?,
        Sign::Until => i64::MIN..=end,
        Sign::In => span,
    })
}

/// How a value is compared.
enum Sign {
    After,
    From,
    Before,
    Until,
    In,
}

fn sign(value: &str) -> (Sign, &str) {
    let signs = [
        (">=", Sign::From),
        ("<=", Sign::Until),
        (">", Sign::After),
        ("<", Sign::Before),
    ];
    let found = signs
        .into_iter()
        .find_map(|(mark, sign)| Some((sign, value.strip_prefix(mark)?)));
    found.unwrap_or((Sign::In, value))
}

/// `>10mb`, `<1kb`, `1mb..5mb` in bytes, each amount just that many;
/// `5mb` alone is about that: from 5 MB to under 6.
fn size(value: &str) -> Option<RangeInclusive<u64>> {
    let amount = |value: &str| Some(*bytes(value)?.start());
    if let Some((from, to)) = value.split_once("..") {
        return Some(amount(from)?..=amount(to)?);
    }
    let (sign, rest) = sign(value);
    let about = bytes(rest)?;
    let amount = *about.start();
    Some(match sign {
        Sign::After => amount.checked_add(1)?..=u64::MAX,
        Sign::From => amount..=u64::MAX,
        Sign::Before => 0..=amount.checked_sub(1)?,
        Sign::Until => 0..=amount,
        Sign::In => about,
    })
}

/// Units of size, 1024 apiece, as Windows counts them.
const UNITS: [(&str, u64); 5] = [
    ("tb", 1 << 40),
    ("gb", 1 << 30),
    ("mb", 1 << 20),
    ("kb", 1 << 10),
    ("b", 1),
];

/// An amount and its unit as the bytes from it to just under one unit
/// more: `5mb` is from 5 MiB up to 6 MiB less a byte; `1.5kb` from 1536.
fn bytes(value: &str) -> Option<RangeInclusive<u64>> {
    let value = value.to_lowercase();
    let (number, unit) = UNITS
        .iter()
        .find_map(|&(name, unit)| Some((value.strip_suffix(name)?, unit)))
        .or_else(|| {
            UNITS.iter().find_map(|&(name, unit)| {
                Some((
                    value.strip_suffix(&name[..1]).filter(|_| name.len() == 2)?,
                    unit,
                ))
            })
        })
        .unwrap_or((value.as_str(), 1));
    let amount: f64 = number
        .trim()
        .replace(',', ".")
        .parse()
        .ok()
        .filter(|amount: &f64| amount.is_finite() && *amount >= 0.0)?;
    let start = amount * unit as f64;
    (start < u64::MAX as f64)
        .then(|| start as u64..=(start + unit as f64 - 1.0).min(u64::MAX as f64) as u64)
}

const DAY: i64 = 24 * 60 * 60;

/// The time a value names: a day, a month or a year, local, or since a
/// while ago until any time later.
fn period(value: &str, clock: Clock) -> Option<RangeInclusive<i64>> {
    let today = (clock.now + clock.offset).div_euclid(DAY) * DAY - clock.offset;
    match value.to_lowercase().as_str() {
        "today" => Some(today..=today + DAY - 1),
        "yesterday" => Some(today - DAY..=today - 1),
        value => ago(value, clock.now).or_else(|| date(value, clock.offset)),
    }
}

/// `7d`, `3h`, `2w`: from that long ago on.
fn ago(value: &str, now: i64) -> Option<RangeInclusive<i64>> {
    let units = [('h', 60 * 60), ('d', DAY), ('w', 7 * DAY)];
    let (count, unit) = units
        .iter()
        .find_map(|&(name, unit)| Some((value.strip_suffix(name)?, unit)))?;
    let count: i64 = count.parse().ok()?;
    Some(now.checked_sub(count.checked_mul(unit)?)?..=i64::MAX)
}

/// `2026`, `2026-11` or `2026-11-10`, local, as the seconds it spans.
fn date(value: &str, offset: i64) -> Option<RangeInclusive<i64>> {
    let parts: Vec<&str> = value.split('-').collect();
    let number = |at: usize, digits: usize| -> Option<i64> {
        let part = parts.get(at)?;
        (part.len() == digits && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse().ok())?
    };
    let year = number(0, 4).filter(|year| (1601..=9999).contains(year))?;
    let (start, end) = match parts.len() {
        1 => ((year, 1, 1), (year + 1, 1, 1)),
        2 => {
            let month = number(1, 2).filter(|month| (1..=12).contains(month))?;
            let next = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            ((year, month, 1), (next.0, next.1, 1))
        }
        3 => {
            let month = number(1, 2).filter(|month| (1..=12).contains(month))?;
            let day = number(2, 2).filter(|&day| day >= 1 && day <= days_in(year, month))?;
            ((year, month, day), (year, month, day + 1))
        }
        _ => return None,
    };
    let seconds = |(year, month, day)| days_since_1970(year, month, day) * DAY - offset;
    Some(seconds(start)..=seconds(end) - 1)
}

fn days_in(year: i64, month: i64) -> i64 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the date, counted the civil way (a day past
/// the month's end is the next month's first).
fn days_since_1970(year: i64, month: i64, day: i64) -> i64 {
    let (year, month) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * month + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-10 14:30 in Moscow, three hours ahead of UTC.
    const CLOCK: Clock = Clock {
        now: 1_794_310_200,
        offset: 3 * 60 * 60,
    };

    fn parsed(text: &str) -> Query {
        Query::parse(text, CLOCK)
    }

    fn filters(text: &str) -> Filters {
        parsed(text).filters
    }

    /// Seconds since 1970 of a local time in Moscow.
    fn moscow(year: i64, month: i64, day: i64, hour: i64) -> i64 {
        days_since_1970(year, month, day) * DAY + hour * 3600 - CLOCK.offset
    }

    #[test]
    fn the_clock_reads_as_its_moscow_time() {
        assert_eq!(moscow(2026, 11, 10, 14) + 30 * 60, CLOCK.now);
        assert_eq!(days_since_1970(1970, 1, 1), 0);
        assert_eq!(days_since_1970(2000, 3, 1), 11_017);
    }

    #[test]
    fn a_query_without_filters_is_its_words_as_typed() {
        for text in [
            "",
            "report",
            "  two  spaces ",
            "http://x",
            "size:big",
            "ext:",
            "in:",
            "mod:soon",
        ] {
            let query = parsed(text);
            assert_eq!(query.words, text);
            assert!(query.filters.is_empty(), "{text}");
        }
    }

    #[test]
    fn filters_are_taken_out_of_the_words_wherever_they_are() {
        let query = parsed("q3  ext:pdf report EXT:.Docx,txt");
        assert_eq!(query.words, "q3 report");
        assert_eq!(query.filters.extensions, ["pdf", "docx", "txt"]);
        assert_eq!(parsed("ext:pdf").words, "");
    }

    #[test]
    fn an_extension_must_be_a_name_of_its_own() {
        for bad in ["ext:pdf,", "ext:,pdf", "ext:a.b", r"ext:a\b"] {
            assert!(filters(bad).is_empty(), "{bad}");
        }
    }

    #[test]
    fn a_file_has_one_of_the_extensions_asked_for() {
        let wanted = filters("ext:pdf,ЖПГ");
        assert!(wanted.extension_of("Report.PDF"));
        assert!(wanted.extension_of("фото.жпг"));
        assert!(!wanted.extension_of("report.pdf.txt"));
        assert!(!wanted.extension_of("pdf"));
        assert!(Filters::default().extension_of("anything"));
    }

    #[test]
    fn a_folder_is_named_or_given_by_its_full_path() {
        assert_eq!(filters("in:Downloads").folders, ["downloads"]);
        assert_eq!(filters(r"in:C:\Users\Me\").folders, [r"c:\users\me"]);
        assert_eq!(filters("in:C:/Users/Me").folders, [r"c:\users\me"]);
        let query = parsed(r#"notes in:"My Files" in:code"#);
        assert_eq!(query.filters.folders, ["my files", "code"]);
        assert_eq!(query.words, "notes");
    }

    #[test]
    fn a_size_is_more_less_between_or_about() {
        const MB: u64 = 1 << 20;
        assert_eq!(filters("size:>10mb").size, Some(10 * MB + 1..=u64::MAX));
        assert_eq!(filters("size:>=10MB").size, Some(10 * MB..=u64::MAX));
        assert_eq!(filters("size:<1kb").size, Some(0..=1023));
        assert_eq!(filters("size:<=1k").size, Some(0..=1024));
        assert_eq!(filters("size:1mb..5mb").size, Some(MB..=5 * MB));
        assert_eq!(filters("size:5mb").size, Some(5 * MB..=6 * MB - 1));
        assert_eq!(filters("size:1,5kb").size, Some(1536..=2559));
        assert_eq!(filters("size:100").size, Some(100..=100), "bytes");
        assert_eq!(filters("size:2g").size, Some(2 << 30..=(3 << 30) - 1));
        assert_eq!(filters("size:1tb").size, Some(1 << 40..=(2 << 40) - 1));
    }

    #[test]
    fn a_size_that_does_not_read_stays_a_word() {
        for bad in [
            "size:",
            "size:>",
            "size:-1mb",
            "size:mb",
            "size:1xb",
            "size:<0",
            "size:1..",
            "size:99999999999tb",
        ] {
            assert!(filters(bad).size.is_none(), "{bad}");
        }
    }

    #[test]
    fn two_sizes_narrow_each_other() {
        assert_eq!(
            filters("size:>1kb size:<1mb").size,
            Some(1025..=(1 << 20) - 1)
        );
        let none = filters("size:>1mb size:<1kb").size.expect("a size");
        assert!(none.is_empty());
    }

    #[test]
    fn a_file_is_sized_and_dated_as_asked() {
        let wanted = filters("size:>1kb mod:today");
        assert!(wanted.sized_and_dated(2000, CLOCK.now));
        assert!(!wanted.sized_and_dated(1024, CLOCK.now));
        assert!(!wanted.sized_and_dated(2000, CLOCK.now - DAY));
        assert!(Filters::default().sized_and_dated(0, 0));
        assert!(!filters("ext:pdf").take_folders());
        assert!(!filters("size:1kb").take_folders());
        assert!(filters("in:code mod:7d").take_folders());
    }

    #[test]
    fn today_and_yesterday_are_local_days() {
        let midnight = moscow(2026, 11, 10, 0);
        assert_eq!(
            filters("mod:today").written,
            Some(midnight..=midnight + DAY - 1)
        );
        assert_eq!(
            filters("mod:Yesterday").written,
            Some(midnight - DAY..=midnight - 1)
        );
        let late = Clock {
            now: moscow(2026, 11, 10, 23) + 3599,
            ..CLOCK
        };
        assert_eq!(
            Query::parse("mod:today", late).filters.written,
            Some(midnight..=midnight + DAY - 1)
        );
    }

    #[test]
    fn a_while_ago_is_from_then_on() {
        assert_eq!(
            filters("mod:7d").written,
            Some(CLOCK.now - 7 * DAY..=i64::MAX)
        );
        assert_eq!(
            filters("mod:3h").written,
            Some(CLOCK.now - 3 * 3600..=i64::MAX)
        );
        assert_eq!(
            filters("mod:2w").written,
            Some(CLOCK.now - 14 * DAY..=i64::MAX)
        );
        assert_eq!(
            filters("mod:<7d").written,
            Some(i64::MIN..=CLOCK.now - 7 * DAY - 1),
            "longer ago"
        );
    }

    #[test]
    fn a_date_is_a_local_day_month_or_year() {
        let november = moscow(2026, 11, 1, 0)..=moscow(2026, 12, 1, 0) - 1;
        assert_eq!(filters("mod:2026-11").written, Some(november));
        let december = moscow(2025, 12, 1, 0)..=moscow(2026, 1, 1, 0) - 1;
        assert_eq!(filters("mod:2025-12").written, Some(december));
        let day = moscow(2024, 2, 29, 0)..=moscow(2024, 3, 1, 0) - 1;
        assert_eq!(filters("mod:2024-02-29").written, Some(day));
        let year = moscow(2025, 1, 1, 0)..=moscow(2026, 1, 1, 0) - 1;
        assert_eq!(filters("mod:2025").written, Some(year));
    }

    #[test]
    fn a_date_compares_by_its_start_and_end() {
        let new_year = moscow(2026, 1, 1, 0);
        assert_eq!(
            filters("mod:>2026-01-01").written,
            Some(new_year + DAY..=i64::MAX)
        );
        assert_eq!(
            filters("mod:>=2026-01-01").written,
            Some(new_year..=i64::MAX)
        );
        assert_eq!(
            filters("mod:<2026-01-01").written,
            Some(i64::MIN..=new_year - 1)
        );
        assert_eq!(
            filters("mod:<=2026-01-01").written,
            Some(i64::MIN..=new_year + DAY - 1)
        );
        assert_eq!(
            filters("mod:2026-01..2026-02").written,
            Some(new_year..=moscow(2026, 3, 1, 0) - 1)
        );
    }

    #[test]
    fn a_date_that_does_not_read_stays_a_word() {
        for bad in [
            "mod:2026-13",
            "mod:2026-02-30",
            "mod:2025-02-29",
            "mod:26-11",
            "mod:2026-1",
            "mod:1600",
            "mod:2026-11-10-1",
            "mod:xd",
            "mod:2026-00",
            "mod:2026-11-00",
        ] {
            assert!(filters(bad).written.is_none(), "{bad}");
        }
    }
}
