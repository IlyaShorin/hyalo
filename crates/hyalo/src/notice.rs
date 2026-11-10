//! What the launcher tells the user, quietly: a row under the empty bar
//! (never while typing, so Enter still opens what was searched for), and
//! the tray's tooltip. Enter on it opens the settings window, which shows
//! the way to the fix.

use hyalo_core::config::Problem;
use hyalo_core::hotkey::Hotkey;
use hyalo_core::item::{Action, Item, ItemId, Kind};
use hyalo_core::text::{Language, Text};

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
        }
    }

    /// The row the launcher lists for it, in `language`; Enter opens the
    /// settings window, which shows the problem and the way to the file.
    pub fn item(&self, language: Language) -> Item {
        let advice = match self {
            Notice::Config(_) => Text::KeepsSettings,
            Notice::Hotkey { .. } => Text::ChooseHotkey,
        };
        Item {
            id: ItemId("hyalo:notice".to_string()),
            title: self.said(language),
            subtitle: language.fill(Text::OpenSettings, &[language.say(advice)]),
            kind: Kind::Notice,
            action: Action::Settings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::config::Why;

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

    #[test]
    fn enter_opens_the_settings_and_the_row_is_a_notice() {
        let item = problem().item(Language::English);
        assert_eq!(item.action, Action::Settings);
        assert_eq!(item.kind, Kind::Notice);
        assert_eq!(item.id, ItemId("hyalo:notice".to_string()));
        assert_eq!(item.path(), None, "not a file to show in its folder");
    }
}
