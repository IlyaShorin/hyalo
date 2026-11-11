//! Web searches: a prefix, a space and words (`g rust lifetimes`) get a
//! row that searches the web for the words; Enter opens the browser at
//! the engine's address with the words in it. A prefix alone, or a word
//! that only starts like one (`git`), searches as any query does.

use crate::item::{Action, Item, ItemId, Kind};
use crate::text::{Language, Text};

/// Where `{query}` stands in an engine's address: the words go there.
pub const QUERY: &str = "{query}";

/// A web search engine: the prefix that asks it, what the row calls it,
/// and its address with `{query}` where the words go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engine {
    pub prefix: String,
    pub name: String,
    pub url: String,
}

/// What can be wrong with an engine of the user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrong {
    /// The prefix is empty, holds a space, or starts with `>`, which
    /// lists the windows.
    Prefix,
    /// The address is not `http://` or `https://`, holds a space, or has
    /// no `{query}`.
    Url,
}

impl Engine {
    /// One of the user's: `prefix` asks `url`, and the row calls it by
    /// the address's host.
    pub fn custom(prefix: &str, url: &str) -> Result<Engine, Wrong> {
        let blank = |text: &str| text.is_empty() || text.contains(char::is_whitespace);
        if blank(prefix) || prefix.starts_with('>') {
            return Err(Wrong::Prefix);
        }
        let scheme = ["http://", "https://"].into_iter().find(|scheme| {
            url.get(..scheme.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
        });
        let Some(scheme) = scheme.filter(|_| !blank(url) && url.contains(QUERY)) else {
            return Err(Wrong::Url);
        };
        let host = host(&url[scheme.len()..]);
        Ok(Engine {
            prefix: prefix.to_string(),
            name: known(host).unwrap_or(host).to_string(),
            url: url.to_string(),
        })
    }

    /// The address that searches for `words`: each byte of their UTF-8
    /// that is not a letter, a digit or one of `-._~` percent-encoded.
    pub fn address(&self, words: &str) -> String {
        self.url.replace(QUERY, &encoded(words))
    }
}

/// What the row calls the site at `host`, when it is one people know by
/// name: those the config writes at first.
fn known(host: &str) -> Option<&'static str> {
    match host {
        "google.com" => Some("Google"),
        "github.com" => Some("GitHub"),
        "youtube.com" => Some("YouTube"),
        "yandex.ru" => Some("Yandex"),
        host if host.ends_with(".wikipedia.org") => Some("Wikipedia"),
        _ => None,
    }
}

/// The host an address names after its scheme, without `www.`.
fn host(rest: &str) -> &str {
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host)
}

/// `text` percent-encoded as RFC 3986 wants a query's words: the
/// unreserved characters stay, every other byte becomes `%XX`.
fn encoded(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            true => out.push(char::from(byte)),
            false => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The engines the config writes at first, and searches when it names
/// none: Google, GitHub, Wikipedia in `language`, Yandex and YouTube, in
/// the order of their prefixes, as the file reads them.
pub fn defaults(language: Language) -> Vec<Engine> {
    let wikipedia = format!(
        "https://{}.wikipedia.org/w/index.php?search={QUERY}",
        language.code()
    );
    [
        ("g", "https://www.google.com/search?q={query}"),
        ("gh", "https://github.com/search?q={query}"),
        ("w", wikipedia.as_str()),
        ("ya", "https://yandex.ru/search/?text={query}"),
        ("yt", "https://www.youtube.com/results?search_query={query}"),
    ]
    .into_iter()
    .map(|(prefix, url)| Engine::custom(prefix, url).expect("a good engine"))
    .collect()
}

/// The engine `query` asks and the words it asks for: a prefix, in any
/// case, then a space and words.
pub fn asked<'a>(query: &'a str, engines: &'a [Engine]) -> Option<(&'a Engine, &'a str)> {
    let (prefix, words) = query.trim_start().split_once(' ')?;
    let words = words.trim();
    if words.is_empty() {
        return None;
    }
    let engine = engines
        .iter()
        .find(|engine| engine.prefix.to_lowercase() == prefix.to_lowercase())?;
    Some((engine, words))
}

/// The row for `query` when it asks one of `engines`, said in
/// `language`: Enter opens the search.
pub fn answer(query: &str, engines: &[Engine], language: Language) -> Option<Item> {
    let (engine, words) = asked(query, engines)?;
    Some(Item {
        id: ItemId(format!("hyalo:web:{}", engine.prefix)),
        title: language.fill(Text::SearchWeb, &[&engine.name, words]),
        subtitle: language.say(Text::WebSearch).to_string(),
        keywords: Vec::new(),
        kind: Kind::Web,
        action: Action::Open(engine.address(words)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engines() -> Vec<Engine> {
        defaults(Language::English)
    }

    fn opened(item: &Item) -> &str {
        match &item.action {
            Action::Open(url) => url,
            action => panic!("{action:?}"),
        }
    }

    #[test]
    fn a_prefix_a_space_and_words_search_the_web_for_the_words() {
        let item = answer("g rust lifetimes", &engines(), Language::English).expect("a search");
        assert_eq!(item.title, "Search Google for “rust lifetimes”");
        assert_eq!(item.subtitle, "Web search · Enter to open in the browser");
        assert_eq!(item.kind, Kind::Web);
        assert_eq!(
            opened(&item),
            "https://www.google.com/search?q=rust%20lifetimes"
        );
    }

    #[test]
    fn the_row_speaks_russian_too() {
        let item = answer("g rust lifetimes", &engines(), Language::Russian).expect("a search");
        assert_eq!(item.title, "Искать в Google: «rust lifetimes»");
        assert_eq!(
            item.subtitle,
            "Поиск в интернете · Enter — открыть в браузере"
        );
    }

    #[test]
    fn each_of_the_five_prefixes_asks_its_engine() {
        let address = |query| {
            let item = answer(query, &engines(), Language::English).expect("a search");
            opened(&item).to_string()
        };
        assert_eq!(address("gh hyalo"), "https://github.com/search?q=hyalo");
        assert_eq!(
            address("yt lofi"),
            "https://www.youtube.com/results?search_query=lofi"
        );
        assert_eq!(
            address("w glass"),
            "https://en.wikipedia.org/w/index.php?search=glass"
        );
        assert_eq!(
            address("ya погода"),
            "https://yandex.ru/search/?text=%D0%BF%D0%BE%D0%B3%D0%BE%D0%B4%D0%B0"
        );
        let russian = defaults(Language::Russian);
        let russian = answer("w стекло", &russian, Language::Russian).expect("a search");
        assert!(opened(&russian).starts_with("https://ru.wikipedia.org/"));
    }

    #[test]
    fn words_are_percent_encoded_as_utf_8() {
        assert_eq!(encoded("rust lifetimes"), "rust%20lifetimes");
        assert_eq!(encoded("привет"), "%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82");
        assert_eq!(encoded("a-b.c_d~e"), "a-b.c_d~e", "unreserved");
        assert_eq!(encoded("c++ & c#?"), "c%2B%2B%20%26%20c%23%3F");
        assert_eq!(encoded("100%/2=50"), "100%25%2F2%3D50");
        assert_eq!(encoded("AZaz09"), "AZaz09");
        assert_eq!(encoded("ё🔍"), "%D1%91%F0%9F%94%8D");
        assert_eq!(encoded(""), "");
    }

    #[test]
    fn a_prefix_alone_or_a_word_that_starts_like_one_is_no_search() {
        for query in ["g", "g ", "g   ", "git", "github desktop", "gh", "", " "] {
            assert_eq!(
                answer(query, &engines(), Language::English),
                None,
                "{query:?}"
            );
        }
    }

    #[test]
    fn the_prefix_goes_in_any_case_and_spaces_around_the_words_go() {
        let engines = engines();
        let (engine, words) = asked("  YT   lofi  beats ", &engines).expect("a search");
        assert_eq!((engine.prefix.as_str(), words), ("yt", "lofi  beats"));
    }

    #[test]
    fn only_the_engines_given_are_asked() {
        let ddg = Engine::custom("g", "https://duckduckgo.com/?q={query}").expect("an engine");
        let so =
            Engine::custom("so", "https://stackoverflow.com/search?q={query}").expect("an engine");
        let custom = [ddg, so];
        let item = answer("g rust", &custom, Language::English).expect("a search");
        assert_eq!(item.title, "Search duckduckgo.com for “rust”");
        assert_eq!(opened(&item), "https://duckduckgo.com/?q=rust");
        let item = answer("so borrow", &custom, Language::English).expect("a search");
        assert_eq!(opened(&item), "https://stackoverflow.com/search?q=borrow");
        assert_eq!(item.id, ItemId("hyalo:web:so".to_string()));
        assert_eq!(
            answer("gh rust", &custom, Language::English),
            None,
            "none built in"
        );
        assert_eq!(answer("g rust", &[], Language::English), None);
    }

    #[test]
    fn an_engine_of_the_users_is_named_by_its_host() {
        let named = |url| Engine::custom("x", url).map(|engine| engine.name);
        assert_eq!(
            named("https://www.bing.com/search?q={query}"),
            Ok("bing.com".to_string())
        );
        assert_eq!(
            named("HTTP://example.org?q={query}"),
            Ok("example.org".to_string())
        );
        assert_eq!(named("https://docs.rs/{query}"), Ok("docs.rs".to_string()));
        assert_eq!(named("https://a.io#{query}"), Ok("a.io".to_string()));
    }

    /// The five the config writes are the user's like any other now, and
    /// their rows still say Google, not google.com.
    #[test]
    fn an_engine_at_a_known_site_is_named_after_it() {
        let named = |url| Engine::custom("x", url).map(|engine| engine.name);
        for (url, name) in [
            ("https://www.google.com/search?q={query}", "Google"),
            ("https://github.com/search?q={query}", "GitHub"),
            (
                "https://www.youtube.com/results?search_query={query}",
                "YouTube",
            ),
            (
                "https://ru.wikipedia.org/w/index.php?search={query}",
                "Wikipedia",
            ),
            (
                "https://en.wikipedia.org/w/index.php?search={query}",
                "Wikipedia",
            ),
            ("https://yandex.ru/search/?text={query}", "Yandex"),
        ] {
            assert_eq!(named(url), Ok(name.to_string()), "{url}");
        }
        let near = named("https://notwikipedia.org/?q={query}");
        assert_eq!(near, Ok("notwikipedia.org".to_string()), "not a subdomain");
    }

    #[test]
    fn an_engine_needs_a_one_word_prefix_and_a_web_address_with_the_query() {
        let url = "https://example.org/?q={query}";
        for prefix in ["", "a b", " so", ">", ">w", "s\to"] {
            assert_eq!(
                Engine::custom(prefix, url),
                Err(Wrong::Prefix),
                "{prefix:?}"
            );
        }
        for url in [
            "",
            "https://example.org/?q=",
            "example.org/?q={query}",
            "ftp://example.org/{query}",
            "https://example.org/?q={query} x",
            "file:///C:/{query}",
            "http:/",
        ] {
            assert_eq!(Engine::custom("x", url), Err(Wrong::Url), "{url:?}");
        }
        assert!(Engine::custom("so.", "http://a/{query}").is_ok());
    }

    #[test]
    fn the_address_has_the_words_wherever_the_query_stands() {
        let engine =
            Engine::custom("d", "https://docs.rs/{query}/latest?s={query}").expect("an engine");
        assert_eq!(
            engine.address("a b"),
            "https://docs.rs/a%20b/latest?s=a%20b"
        );
    }
}
