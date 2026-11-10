//! hyalo's releases on GitHub, apart from the network: what a release
//! is, whether it is newer than the build that runs, the sum its download
//! is checked against, when to ask GitHub again, and how installing one
//! goes. Bytes in, decisions out; asking, downloading and swapping the
//! exe are the launcher's.

use std::fmt;

use serde::Deserialize;

use crate::text::{Language, Text};

/// A version as the workspace and the release tags write it: `0.0.2`,
/// or `v0.0.2` in a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// The version `text` names: three numbers joined by points, perhaps
    /// after a `v`. Anything else, such as a tag with a suffix
    /// (`v0.1.0-rc1`), names none: it is no release hyalo offers.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut numbers = text.split('.').map(number);
        let version = Version {
            major: numbers.next()??,
            minor: numbers.next()??,
            patch: numbers.next()??,
        };
        numbers.next().is_none().then_some(version)
    }
}

/// A part of a version: digits only, so `+1` and ` 1` are not one.
fn number(text: &str) -> Option<u32> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| text.parse().ok()).flatten()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The exe a release carries.
pub const EXE: &str = "hyalo.exe";

/// The file that carries the exe's SHA-256.
pub const SUM: &str = "hyalo.exe.sha256";

/// A release hyalo can install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// What the release says is new, as written.
    pub notes: String,
    /// Where the exe is downloaded from, over HTTPS.
    pub exe: String,
    /// Where its SHA-256 is downloaded from, over HTTPS.
    pub sum: String,
}

/// A release as GitHub's API answers with it.
#[derive(Deserialize)]
struct Answer {
    tag_name: String,
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    /// The release GitHub's `releases/latest` answers with, if it is one
    /// hyalo can install: published, tagged with a version, carrying the
    /// exe and its sum, both over HTTPS.
    pub fn parse(json: &[u8]) -> Option<Release> {
        let answer: Answer = serde_json::from_slice(json).ok()?;
        if answer.draft || answer.prerelease {
            return None;
        }
        let url = |name: &str| {
            let asset = answer.assets.iter().find(|asset| asset.name == name)?;
            let url = &asset.browser_download_url;
            url.starts_with("https://").then(|| url.clone())
        };
        Some(Release {
            version: Version::parse(&answer.tag_name)?,
            exe: url(EXE)?,
            sum: url(SUM)?,
            notes: answer.body.unwrap_or_default(),
        })
    }

    /// The notes as lines to show: blank lines left out, a Markdown
    /// heading's `#`s taken off, a list item's `*` or `-` drawn as a
    /// bullet; at most `NOTES_LINES`, the last of them `…` when there
    /// were more.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.notes.lines().filter_map(plain).collect();
        if lines.len() > NOTES_LINES {
            lines.truncate(NOTES_LINES - 1);
            lines.push("…".to_string());
        }
        lines
    }
}

/// How many lines of notes the settings window shows.
pub const NOTES_LINES: usize = 12;

/// A line of Markdown as plain text; none when it is blank.
fn plain(line: &str) -> Option<String> {
    let line = line.trim();
    let heading = line.trim_start_matches('#');
    let marked = heading.len() < line.len() && (heading.is_empty() || heading.starts_with(' '));
    let line = match marked {
        true => heading.trim_start(),
        false => line,
    };
    let item = ["* ", "- "].iter().find_map(|mark| line.strip_prefix(mark));
    match item {
        Some(item) => Some(format!("• {}", item.trim_start())),
        None => (!line.is_empty()).then(|| line.to_string()),
    }
}

/// A SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// The digest a `.sha256` file holds: 64 hex digits, in either case,
    /// first on its first line, perhaps followed by the file's name, as
    /// `sha256sum` and the release workflow write it.
    pub fn parse(text: &[u8]) -> Option<Digest> {
        let text = std::str::from_utf8(text).ok()?;
        let hex = text.split_whitespace().next()?.as_bytes();
        let mut digest = [0_u8; 32];
        if hex.len() != 2 * digest.len() {
            return None;
        }
        for (byte, pair) in digest.iter_mut().zip(hex.chunks(2)) {
            *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
        }
        Some(Digest(digest))
    }
}

/// The value of one hex digit.
fn nibble(digit: u8) -> Option<u8> {
    char::from(digit).to_digit(16).map(|value| value as u8)
}

/// How long after an answer GitHub is asked again, in seconds: a day.
pub const AGAIN_AFTER: u64 = 24 * 60 * 60;

/// How long after a check that failed (offline, GitHub down) it is
/// tried again, in seconds: an hour.
pub const RETRY_AFTER: u64 = 60 * 60;

/// How the last check for a newer release went; times in seconds since
/// 1970.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Checked {
    /// Not asked yet.
    #[default]
    Never,
    /// Asked; the answer is on its way.
    Asking,
    /// GitHub answered at this time.
    Answered(u64),
    /// Asking failed at this time.
    Failed(u64),
}

impl Checked {
    /// Whether to ask GitHub at `now`: never while an answer is on its
    /// way, a day after an answer, an hour after a failure. A clock set
    /// back as far counts as that much time gone.
    pub fn due(self, now: u64) -> bool {
        match self {
            Checked::Never => true,
            Checked::Asking => false,
            Checked::Answered(at) => now.abs_diff(at) >= AGAIN_AFTER,
            Checked::Failed(at) => now.abs_diff(at) >= RETRY_AFTER,
        }
    }
}

/// Why an update was not installed: the version that runs stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A download failed, as Windows says.
    Download(String),
    /// The exe downloaded is not the one its sum names, or there was no
    /// sum to read.
    Mismatch,
    /// The new exe could not be put in place of the old one.
    Swap(String),
    /// The new exe could not be started; the old one is back in place.
    Start(String),
}

impl Failure {
    /// What went wrong, in `language`.
    pub fn said(&self, language: Language) -> String {
        match self {
            Failure::Download(why) => language.fill(Text::DownloadFailed, &[why]),
            Failure::Mismatch => language.say(Text::Mismatch).to_string(),
            Failure::Swap(why) => language.fill(Text::SwapFailed, &[why]),
            Failure::Start(why) => language.fill(Text::StartFailed, &[why]),
        }
    }
}

/// A release newer than the build that runs, and how installing it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub release: Release,
    pub progress: Progress,
}

/// How far installing an update has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Told, waiting for the user.
    Offered,
    /// Downloading, checking and swapping it in.
    Installing,
    /// It did not install, for this reason.
    Failed(Failure),
}

impl Update {
    /// What hyalo knows after GitHub named `latest` as its latest
    /// release, when it knew of `known` before and runs `current`: a
    /// release newer than `current` is offered, but the one known before
    /// keeps how far installing it came; a release no newer is nothing to
    /// offer.
    pub fn after_check(known: Option<Update>, latest: Release, current: Version) -> Option<Update> {
        if latest.version <= current {
            return None;
        }
        let progress = known
            .filter(|known| known.release == latest)
            .map_or(Progress::Offered, |known| known.progress);
        Some(Update {
            release: latest,
            progress,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    #[test]
    fn a_version_is_three_numbers_perhaps_after_a_v() {
        assert_eq!(version("0.0.1").to_string(), "0.0.1");
        assert_eq!(version("v1.20.300").to_string(), "1.20.300");
        for wrong in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "1..3",
            "1.2.x",
            "v1.2.3-rc1",
            "V1.2.3",
            "1.2.+3",
            " 1.2.3",
            "1.2.3 ",
            "vv1.2.3",
            "99999999999.0.0",
        ] {
            assert_eq!(Version::parse(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(version("0.0.10") > version("0.0.9"));
        assert!(version("0.2.0") > version("0.1.99"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert_eq!(version("v0.0.1"), version("0.0.1"));
    }

    #[test]
    fn the_workspace_version_is_one() {
        assert!(Version::parse(env!("CARGO_PKG_VERSION")).is_some());
    }

    /// A release as GitHub answers with it, its fields written in.
    fn answer(tag: &str, body: &str, assets: &[(&str, &str)]) -> Vec<u8> {
        let assets: Vec<String> = assets
            .iter()
            .map(|(name, url)| {
                format!(r#"{{"name": "{name}", "size": 5, "browser_download_url": "{url}"}}"#)
            })
            .collect();
        format!(
            r#"{{"tag_name": "{tag}", "name": "hyalo {tag}", "draft": false,
                "prerelease": false, "body": {body}, "assets": [{}]}}"#,
            assets.join(", ")
        )
        .into_bytes()
    }

    const DOWNLOAD: &str = "https://github.com/IlyaShorin/hyalo/releases/download/v0.0.2";

    fn assets() -> Vec<(&'static str, String)> {
        vec![
            ("hyalo.exe.sha256", format!("{DOWNLOAD}/hyalo.exe.sha256")),
            ("hyalo.exe", format!("{DOWNLOAD}/hyalo.exe")),
        ]
    }

    fn published(tag: &str, body: &str) -> Vec<u8> {
        let assets = assets();
        let named: Vec<(&str, &str)> = assets.iter().map(|(n, u)| (*n, u.as_str())).collect();
        answer(tag, body, &named)
    }

    #[test]
    fn a_release_is_its_tag_its_notes_and_where_the_exe_and_its_sum_are() {
        let release = Release::parse(&published("v0.0.2", r#""Faster.\nSmaller.""#));
        assert_eq!(
            release,
            Some(Release {
                version: version("0.0.2"),
                notes: "Faster.\nSmaller.".to_string(),
                exe: format!("{DOWNLOAD}/hyalo.exe"),
                sum: format!("{DOWNLOAD}/hyalo.exe.sha256"),
            })
        );
    }

    #[test]
    fn a_release_without_notes_has_empty_ones() {
        let release = Release::parse(&published("v0.0.2", "null")).expect("a release");
        assert_eq!(release.notes, "");
        assert!(release.lines().is_empty());
    }

    #[test]
    fn a_release_hyalo_cannot_install_is_none() {
        let assets = assets();
        let exe_only = [("hyalo.exe", assets[1].1.as_str())];
        let sum_only = [("hyalo.exe.sha256", assets[0].1.as_str())];
        let plain = [
            ("hyalo.exe", "http://github.com/hyalo.exe"),
            ("hyalo.exe.sha256", assets[0].1.as_str()),
        ];
        let plain_sum = [
            ("hyalo.exe", assets[1].1.as_str()),
            ("hyalo.exe.sha256", "http://github.com/hyalo.exe.sha256"),
        ];
        let wrong = [
            published("v0.0.2-rc1", "null"),
            published("nightly", "null"),
            answer("v0.0.2", "null", &exe_only),
            answer("v0.0.2", "null", &sum_only),
            answer("v0.0.2", "null", &plain),
            answer("v0.0.2", "null", &plain_sum),
            String::from_utf8(published("v0.0.2", "null"))
                .expect("text")
                .replace(r#""draft": false"#, r#""draft": true"#)
                .into_bytes(),
            String::from_utf8(published("v0.0.2", "null"))
                .expect("text")
                .replace(r#""prerelease": false"#, r#""prerelease": true"#)
                .into_bytes(),
            br#"{"message": "Not Found"}"#.to_vec(),
            b"<html>rate limited</html>".to_vec(),
        ];
        for json in wrong {
            assert_eq!(
                Release::parse(&json),
                None,
                "{}",
                String::from_utf8_lossy(&json)
            );
        }
    }

    #[test]
    fn a_release_written_without_draft_and_prerelease_is_published() {
        let json = String::from_utf8(published("v0.0.2", "null")).expect("text");
        let json = json.replace(r#""draft": false,"#, "");
        let json = json.replace(r#""prerelease": false,"#, "");
        assert!(Release::parse(json.as_bytes()).is_some(), "{json}");
    }

    fn noted(notes: &str) -> Release {
        Release {
            version: version("0.0.2"),
            notes: notes.to_string(),
            exe: String::new(),
            sum: String::new(),
        }
    }

    #[test]
    fn the_notes_show_as_plain_lines_without_markdowns_marks() {
        let notes = "## What's new\r\n\r\n* Updates itself\n- Checks the download\n  \n\
                     #hashtag stays\nA *word* stays\n### \n-dash stays";
        assert_eq!(
            noted(notes).lines(),
            [
                "What's new",
                "• Updates itself",
                "• Checks the download",
                "#hashtag stays",
                "A *word* stays",
                "-dash stays",
            ]
        );
    }

    #[test]
    fn long_notes_are_cut_short() {
        let twelve: Vec<String> = (1..=NOTES_LINES).map(|n| n.to_string()).collect();
        assert_eq!(noted(&twelve.join("\n")).lines(), twelve);
        let thirteen: Vec<String> = (1..=NOTES_LINES + 1).map(|n| n.to_string()).collect();
        let lines = noted(&thirteen.join("\n")).lines();
        assert_eq!(lines.len(), NOTES_LINES);
        assert_eq!(lines[NOTES_LINES - 2], (NOTES_LINES - 1).to_string());
        assert_eq!(lines[NOTES_LINES - 1], "…");
    }

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn abc() -> Digest {
        let mut bytes = [0_u8; 32];
        for (at, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&ABC[2 * at..2 * at + 2], 16).expect("hex");
        }
        Digest(bytes)
    }

    #[test]
    fn a_sum_file_is_its_hex_digest_then_perhaps_the_files_name() {
        for text in [
            ABC.to_string(),
            format!("{ABC}  hyalo.exe\n"),
            format!("{ABC} *hyalo.exe\r\n"),
            ABC.to_uppercase(),
        ] {
            assert_eq!(Digest::parse(text.as_bytes()), Some(abc()), "{text:?}");
        }
    }

    #[test]
    fn a_sum_file_without_a_whole_digest_is_none() {
        let short = &ABC[..63];
        let long = format!("{ABC}0");
        let wrong = ABC.replacen('b', "g", 1);
        for text in ["", "  \n", short, &long, &wrong, "hyalo.exe"] {
            assert_eq!(Digest::parse(text.as_bytes()), None, "{text:?}");
        }
        assert_eq!(Digest::parse(&[0xFF; 64]), None, "not UTF-8");
    }

    #[test]
    fn github_is_asked_at_first_then_a_day_after_an_answer_and_an_hour_after_a_failure() {
        let at = 1_800_000_000;
        assert!(Checked::Never.due(at));
        assert!(!Checked::Asking.due(at));
        assert!(!Checked::Answered(at).due(at + AGAIN_AFTER - 1));
        assert!(Checked::Answered(at).due(at + AGAIN_AFTER));
        assert!(!Checked::Failed(at).due(at + RETRY_AFTER - 1));
        assert!(Checked::Failed(at).due(at + RETRY_AFTER));
        assert_eq!(AGAIN_AFTER, 86_400);
        assert_eq!(RETRY_AFTER, 3_600);
    }

    #[test]
    fn a_clock_set_back_counts_as_time_gone() {
        let at = 1_800_000_000;
        assert!(!Checked::Answered(at).due(at - 1));
        assert!(Checked::Answered(at).due(at - AGAIN_AFTER));
        assert!(Checked::Failed(at).due(at - RETRY_AFTER));
    }

    fn update(tag: &str, progress: Progress) -> Update {
        Update {
            release: Release {
                version: version(tag),
                ..noted("")
            },
            progress,
        }
    }

    #[test]
    fn a_newer_release_is_offered_and_one_no_newer_is_not() {
        let current = version("0.0.2");
        let latest = |tag| update(tag, Progress::Offered).release;
        assert_eq!(
            Update::after_check(None, latest("0.0.3"), current),
            Some(update("0.0.3", Progress::Offered))
        );
        assert_eq!(Update::after_check(None, latest("0.0.2"), current), None);
        assert_eq!(Update::after_check(None, latest("0.0.1"), current), None);
    }

    #[test]
    fn the_release_known_keeps_its_progress_and_a_newer_one_starts_over() {
        let current = version("0.0.1");
        let failed = update("0.0.2", Progress::Failed(Failure::Mismatch));
        let same = failed.release.clone();
        assert_eq!(
            Update::after_check(Some(failed.clone()), same, current),
            Some(failed.clone())
        );
        let newer = update("0.0.3", Progress::Offered);
        assert_eq!(
            Update::after_check(Some(failed), newer.release.clone(), current),
            Some(newer)
        );
    }

    #[test]
    fn what_went_wrong_is_said_in_either_language() {
        let failures = [
            Failure::Download("timed out".to_string()),
            Failure::Mismatch,
            Failure::Swap("access denied".to_string()),
            Failure::Start("not a program".to_string()),
        ];
        let english: Vec<String> = failures.iter().map(|f| f.said(Language::English)).collect();
        assert_eq!(
            english,
            [
                "cannot download it: timed out",
                "the download is not the build its SHA-256 names",
                "cannot put it in place: access denied",
                "cannot start it: not a program",
            ]
        );
        let russian: Vec<String> = failures.iter().map(|f| f.said(Language::Russian)).collect();
        assert_eq!(
            russian,
            [
                "не удалось скачать: timed out",
                "скачанный файл не совпадает со своей SHA-256",
                "не удалось заменить файл: access denied",
                "не удалось запустить: not a program",
            ]
        );
    }
}
