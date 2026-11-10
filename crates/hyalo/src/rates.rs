//! The Bank of Russia's rates for the currency answer: the last good
//! answer kept on disk and handed over at start, and asked for again when
//! older than today, off the UI thread like any provider.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use hyalo_core::rates::{self, Date, Rates};
use windows::core::Result;

use crate::catalog::{Found, Provider, Request};
use crate::http::Address;
use crate::store;

/// Where the bank publishes the day's rates.
pub fn bank() -> Address {
    Address::https("www.cbr.ru", "/scripts/XML_daily.asp")
}

/// How long showing the launcher waits before looking at the rates again.
/// Looking costs nothing while they are current; it also spaces out the
/// retries after a failed request.
pub const REFRESH_AFTER: Duration = Duration::from_secs(60 * 60);

/// Asks the bank for its daily file.
pub type Fetch = Arc<dyn Fn() -> Result<Vec<u8>> + Send + Sync>;

/// Today, as the bank counts days.
pub type Today = Arc<dyn Fn() -> Date + Send + Sync>;

/// `%LOCALAPPDATA%\hyalo\rates.xml`: the bank's last good answer, as it
/// came.
pub fn default_cache() -> Option<PathBuf> {
    store::ours("LOCALAPPDATA", "rates.xml")
}

/// Today in Moscow by the system clock.
pub fn moscow_today() -> Date {
    let since_1970 = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    Date::moscow(since_1970.unwrap_or_default().as_secs())
}

/// The rates provider. Its first run hands over the rates kept in
/// `cache`, if they read. Each run then asks `fetch` for the bank's file
/// when `rates::due` says so, hands over the rates in a good answer and
/// keeps it in `cache`. A failed request or an answer that is not rates
/// leaves the rates there were.
pub fn provider(fetch: Fetch, cache: Option<PathBuf>, today: Today) -> Provider {
    let known = Mutex::new(Known::default());
    Arc::new(move |_: &Request, report: &dyn Fn(Found)| {
        let mut known = known.lock().unwrap_or_else(PoisonError::into_inner);
        if known.newest.is_none() {
            known.take(cache.as_deref().and_then(load), report);
        }
        let today = today();
        if rates::due(known.newest, known.checked, today) {
            known.ask(&*fetch, cache.as_deref(), today, report);
        }
    })
}

/// What the provider knows between runs.
#[derive(Default)]
struct Known {
    /// The date of the newest rates handed over.
    newest: Option<Date>,
    /// The day the bank last answered with rates.
    checked: Option<Date>,
}

impl Known {
    /// Hands over `rates`, if any, as the newest.
    fn take(&mut self, rates: Option<Rates>, report: &dyn Fn(Found)) {
        if let Some(rates) = rates {
            self.newest = Some(rates.date());
            report(Found::Rates(rates));
        }
    }

    /// Asks the bank; keeps a good answer in `cache` and hands it over.
    fn ask(
        &mut self,
        fetch: &dyn Fn() -> Result<Vec<u8>>,
        cache: Option<&Path>,
        today: Date,
        report: &dyn Fn(Found),
    ) {
        let xml = match fetch() {
            Ok(xml) => xml,
            Err(error) => return eprintln!("hyalo: cannot fetch the rates: {error}"),
        };
        let Some(rates) = parsed(&xml, "the bank's answer") else {
            return;
        };
        self.checked = Some(today);
        if let Some(path) = cache
            && let Err(error) = store::replace(path, &xml)
        {
            eprintln!(
                "hyalo: cannot keep the rates in {}: {error}",
                path.display()
            );
        }
        self.take(Some(rates), report);
    }
}

/// The rates kept at `path`, if there are any and they read.
fn load(path: &Path) -> Option<Rates> {
    parsed(&fs::read(path).ok()?, &path.display().to_string())
}

/// The rates in `xml`, `what` holds; logged either way.
fn parsed(xml: &[u8], what: &str) -> Option<Rates> {
    let rates = Rates::parse(xml);
    match &rates {
        Some(rates) => eprintln!("hyalo: rates of {} in {what}", rates.date()),
        None => eprintln!("hyalo: {what} is not the bank's rates"),
    }
    rates
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows::Win32::Foundation::E_FAIL;
    use windows::core::Error;

    /// The bank's answer on 10 November 2026, saved as it came.
    pub(crate) const FIXTURE: &[u8] =
        include_bytes!("../../hyalo-core/fixtures/cbr-daily-2026-11-10.xml");

    /// The fixture, dated `date` instead.
    pub(crate) fn xml(date: &str) -> Vec<u8> {
        let at = FIXTURE.windows(10).position(|text| text == b"10.11.2026");
        let at = at.expect("the fixture's date");
        [&FIXTURE[..at], date.as_bytes(), &FIXTURE[at + 10..]].concat()
    }

    /// The fixture's rates, of 10.11.2026: a dollar for 82.7536 roubles.
    pub(crate) fn fixture() -> Rates {
        Rates::parse(FIXTURE).expect("the fixture reads")
    }

    /// The fixture's rates, dated `date`.
    pub(crate) fn dated(date: &str) -> Rates {
        Rates::parse(&xml(date)).expect("the fixture reads")
    }

    fn date(text: &str) -> Date {
        Date::parse(text).expect("a date")
    }

    /// A stand-in for the bank that gives `answers` in turn, then
    /// errors, and counts how often it was asked.
    fn bank_answering(answers: Vec<Result<Vec<u8>>>) -> (Fetch, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = asked.clone();
        let answers = Mutex::new(answers.into_iter());
        let fetch: Fetch = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let next = answers.lock().expect("the answers").next();
            next.unwrap_or_else(|| Err(Error::new(E_FAIL, "no more answers")))
        });
        (fetch, asked)
    }

    fn offline() -> Result<Vec<u8>> {
        Err(Error::new(E_FAIL, "offline"))
    }

    /// A clock that says `day` until the test turns it.
    fn clock(day: &str) -> (Today, Arc<Mutex<Date>>) {
        let shown = Arc::new(Mutex::new(date(day)));
        let read = shown.clone();
        (Arc::new(move || *read.lock().expect("the day")), shown)
    }

    /// The dates of the rates one run of `provider` hands over.
    fn run(provider: &Provider) -> Vec<String> {
        let found = RefCell::new(Vec::new());
        let request = Request {
            icon_size: 32,
            known: HashSet::new(),
        };
        provider(&request, &|piece| {
            if let Found::Rates(rates) = piece {
                found.borrow_mut().push(rates.date().to_string());
            }
        });
        found.into_inner()
    }

    /// A scratch folder with `xml` kept as the rates, if any.
    fn kept(test: &str, xml: Option<&[u8]>) -> (Scratch, PathBuf) {
        let folder = Scratch::new(test);
        let path = folder.0.join("hyalo").join("rates.xml");
        if let Some(xml) = xml {
            store::replace(&path, xml).expect("keep the rates");
        }
        (folder, path)
    }

    #[test]
    fn kept_rates_come_first_then_the_banks_newer_ones_are_kept() {
        let (_folder, path) = kept("rates-newer", Some(&xml("10.11.2026")));
        let (fetch, asked) = bank_answering(vec![Ok(xml("11.11.2026"))]);
        let provider = provider(fetch, Some(path.clone()), clock("11.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026", "11.11.2026"]);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(&path).expect("kept"), xml("11.11.2026"));
    }

    #[test]
    fn current_kept_rates_are_handed_over_once_and_not_asked_for() {
        let (_folder, path) = kept("rates-current", Some(FIXTURE));
        let (fetch, asked) = bank_answering(Vec::new());
        let provider = provider(fetch, Some(path), clock("10.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(run(&provider), Vec::<String>::new());
        assert_eq!(asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn without_kept_rates_the_bank_is_asked_and_its_answer_kept() {
        let (_folder, path) = kept("rates-none", None);
        let (fetch, _) = bank_answering(vec![Ok(FIXTURE.to_vec())]);
        let provider = provider(fetch, Some(path.clone()), clock("10.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(fs::read(&path).expect("kept"), FIXTURE);
    }

    #[test]
    fn without_a_place_to_keep_them_the_banks_rates_are_still_handed_over() {
        let (fetch, _) = bank_answering(vec![Ok(FIXTURE.to_vec())]);
        let provider = provider(fetch, None, clock("10.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
    }

    #[test]
    fn a_failed_request_keeps_the_rates_there_were_and_the_next_run_asks_again() {
        let (_folder, path) = kept("rates-offline", Some(FIXTURE));
        let answers = vec![offline(), Ok(xml("11.11.2026"))];
        let (fetch, asked) = bank_answering(answers);
        let provider = provider(fetch, Some(path.clone()), clock("11.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(fs::read(&path).expect("kept"), FIXTURE);
        assert_eq!(run(&provider), ["11.11.2026"]);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_answer_that_is_not_rates_is_neither_handed_over_nor_kept() {
        let (_folder, path) = kept("rates-html", Some(FIXTURE));
        let page = b"<html><body>Service unavailable</body></html>".to_vec();
        let (fetch, asked) = bank_answering(vec![Ok(page), Ok(xml("11.11.2026"))]);
        let provider = provider(fetch, Some(path.clone()), clock("11.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(fs::read(&path).expect("kept"), FIXTURE);
        assert_eq!(run(&provider), ["11.11.2026"], "asked again");
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_bank_with_nothing_newer_is_asked_again_only_the_next_day() {
        let (fetch, asked) = bank_answering(vec![Ok(FIXTURE.to_vec()), Ok(FIXTURE.to_vec())]);
        let (today, day) = clock("11.11.2026");
        let provider = provider(fetch, None, today);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(run(&provider), Vec::<String>::new());
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        *day.lock().expect("the day") = date("12.11.2026");
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn kept_rates_that_do_not_read_are_replaced_by_the_banks() {
        let (_folder, path) = kept("rates-broken", Some(b"{ broken"));
        let (fetch, _) = bank_answering(vec![Ok(FIXTURE.to_vec())]);
        let provider = provider(fetch, Some(path.clone()), clock("10.11.2026").0);
        assert_eq!(run(&provider), ["10.11.2026"]);
        assert_eq!(fs::read(&path).expect("kept"), FIXTURE);
    }

    #[test]
    fn the_rates_come_from_the_banks_own_site_over_https() {
        let bank = bank();
        let address = (
            bank.host.as_str(),
            bank.port,
            bank.path.as_str(),
            bank.secure,
        );
        assert_eq!(address, ("www.cbr.ru", 443, "/scripts/XML_daily.asp", true));
    }

    #[test]
    fn the_rates_are_kept_in_local_app_data() {
        let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set on Windows");
        let expected = PathBuf::from(local).join("hyalo").join("rates.xml");
        assert_eq!(default_cache(), Some(expected));
    }
}
