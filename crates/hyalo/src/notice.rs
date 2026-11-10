//! What the launcher tells the user, quietly: a row under the empty bar
//! (never while typing, so Enter still opens what was searched for), and
//! the tray's tooltip. Enter on it opens the settings window, which shows
//! the way to the fix.

use hyalo_core::config::Problem;
use hyalo_core::hotkey::Hotkey;
use hyalo_core::item::{Action, Item, ItemId, Kind};
use hyalo_core::text::{Language, Text};
use hyalo_core::update::{Progress, Update, Version};

/// Something the user should know and can fix in the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// The config has a problem; the last good settings stay.
    Config(Problem),
    /// Windows refused `wanted`; `kept`, if any, still shows hyalo.
    Hotkey {
        wanted: Hotkey,
        kept: Option<Hotkey>,
    },
    /// A newer release than `current`, the version that runs, and how
    /// far installing it came.
    Update { update: Update, current: Version },
}

impl Notice {
    /// What the notice says, in a line, in `language`.
    pub fn said(&self, language: Language) -> String {
        match self {
            Notice::Config(problem) => {
                language.fill(Text::NoticeConfig, &[&problem.said(language)])
            }
            Notice::Hotkey {
                wanted,
                kept: Some(kept),
            } => language.fill(Text::TakenKept, &[&wanted.to_string(), &kept.to_string()]),
            Notice::Hotkey { wanted, kept: None } => {
                language.fill(Text::Taken, &[&wanted.to_string()])
            }
            Notice::Update { update, .. } => {
                let version = update.release.version.to_string();
                match &update.progress {
                    Progress::Failed(failure) => {
                        language.fill(Text::NotInstalled, &[&version, &failure.said(language)])
                    }
                    _ => language.fill(Text::UpdateOut, &[&version]),
                }
            }
        }
    }

    /// What to do about it, in `language`.
    fn advice(&self, language: Language) -> String {
        let advice = match self {
            Notice::Config(_) => Text::KeepsSettings,
            Notice::Hotkey { .. } => Text::ChooseHotkey,
            Notice::Update { update, current } => match update.progress {
                Progress::Offered => Text::SeeWhatsNew,
                Progress::Installing => Text::InstallingHelp,
                Progress::Failed(_) => {
                    return language.fill(Text::KeepsVersion, &[&current.to_string()]);
                }
            },
        };
        language.say(advice).to_string()
    }

    /// The row the launcher lists for it, in `language`; Enter opens the
    /// settings window, which shows the problem and the way to the file.
    pub fn item(&self, language: Language) -> Item {
        Item {
            id: ItemId("hyalo:notice".to_string()),
            title: self.said(language),
            subtitle: language.fill(Text::OpenSettings, &[&self.advice(language)]),
            kind: Kind::Notice,
            action: Action::Settings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::config::Why;
    use hyalo_core::update::{Failure, Progress, Release, Version};

    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("a hotkey")
    }

    fn problem() -> Notice {
        Notice::Config(Problem {
            line: 2,
            why: Why::Toml("unknown field `hotkye`".to_string()),
        })
    }

    #[test]
    fn a_problem_in_the_config_names_its_line() {
        assert_eq!(
            problem().said(Language::English),
            "config.toml, line 2: unknown field `hotkye`"
        );
        let item = problem().item(Language::English);
        assert_eq!(item.title, problem().said(Language::English));
        assert!(
            item.subtitle.contains("last good settings"),
            "{}",
            item.subtitle
        );
        assert!(
            item.subtitle.contains("Enter to open settings"),
            "{}",
            item.subtitle
        );
    }

    #[test]
    fn a_refused_hotkey_says_which_one_still_shows_hyalo() {
        let refused = Notice::Hotkey {
            wanted: hotkey("Ctrl+K"),
            kept: Some(Hotkey::default()),
        };
        assert_eq!(
            refused.said(Language::English),
            "Ctrl+K is taken: Alt+Space still shows hyalo"
        );
        let alone = Notice::Hotkey {
            wanted: Hotkey::default(),
            kept: None,
        };
        assert_eq!(
            alone.said(Language::English),
            "Alt+Space is taken by another app"
        );
        let item = alone.item(Language::English);
        assert!(
            item.subtitle.contains("another hotkey"),
            "{}",
            item.subtitle
        );
    }

    #[test]
    fn a_notice_speaks_russian_in_russian() {
        let item = problem().item(Language::Russian);
        assert_eq!(item.title, "config.toml, строка 2: unknown field `hotkye`");
        let advice = "Действуют последние исправные настройки · Enter — открыть параметры";
        assert_eq!(item.subtitle, advice);
        let refused = Notice::Hotkey {
            wanted: hotkey("Ctrl+K"),
            kept: Some(Hotkey::default()),
        };
        let said = "Ctrl+K уже занято: hyalo по-прежнему открывается по Alt+Space";
        assert_eq!(refused.said(Language::Russian), said);
        let alone = Notice::Hotkey {
            wanted: Hotkey::default(),
            kept: None,
        };
        let item = alone.item(Language::Russian);
        assert_eq!(item.title, "Alt+Space уже занято другим приложением");
        let choose = "Выберите другое сочетание клавиш · Enter — открыть параметры";
        assert_eq!(item.subtitle, choose);
    }

    fn update(progress: Progress) -> Notice {
        let version = |text| Version::parse(text).expect("a version");
        Notice::Update {
            update: Update {
                release: Release {
                    version: version("0.0.2"),
                    notes: String::new(),
                    exe: String::new(),
                    sum: String::new(),
                },
                progress,
            },
            current: version("0.0.1"),
        }
    }

    #[test]
    fn a_newer_release_is_told_with_the_way_to_its_notes() {
        let item = update(Progress::Offered).item(Language::English);
        assert_eq!(item.title, "hyalo 0.0.2 is out");
        let advice = "See what is new and install it · Enter to open settings";
        assert_eq!(item.subtitle, advice);
        let item = update(Progress::Offered).item(Language::Russian);
        assert_eq!(item.title, "Вышла hyalo 0.0.2");
        let advice = "Посмотрите, что нового, и установите · Enter — открыть параметры";
        assert_eq!(item.subtitle, advice);
        assert_eq!(item.action, Action::Settings);
    }

    #[test]
    fn an_update_installing_or_refused_says_so() {
        let item = update(Progress::Installing).item(Language::English);
        assert_eq!(item.title, "hyalo 0.0.2 is out");
        assert!(
            item.subtitle.starts_with("Downloading and checking it"),
            "{}",
            item.subtitle
        );
        let refused = update(Progress::Failed(Failure::Mismatch));
        let item = refused.item(Language::English);
        let said = "hyalo 0.0.2 is not installed: the download is not the build its SHA-256 names";
        assert_eq!(item.title, said);
        let advice = "hyalo 0.0.1 keeps running · Enter to open settings";
        assert_eq!(item.subtitle, advice);
        let item = refused.item(Language::Russian);
        let said = "hyalo 0.0.2 не установлена: скачанный файл не совпадает со своей SHA-256";
        assert_eq!(item.title, said);
        let advice = "Работает прежняя hyalo 0.0.1 · Enter — открыть параметры";
        assert_eq!(item.subtitle, advice);
    }

    #[test]
    fn enter_opens_the_settings_and_the_row_is_a_notice() {
        let item = problem().item(Language::English);
        assert_eq!(item.action, Action::Settings);
        assert_eq!(item.kind, Kind::Notice);
        assert_eq!(item.id, ItemId("hyalo:notice".to_string()));
        assert_eq!(item.path(), None, "not a file to show in its folder");
    }
}
