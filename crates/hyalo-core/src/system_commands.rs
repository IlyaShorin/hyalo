//! System commands: lock the PC, sleep, hibernate, restart, shut down,
//! sign out, empty the Recycle Bin. Each is named in English and in
//! Russian and found by words people use for it, in either language.
//!
//! Every one but Lock asks first: Enter on its row asks to press Enter
//! again (what it says is `asking`), and only that second Enter runs it.

use crate::item::{Action, Item, ItemId, Kind};
use crate::text::{Language, Text};

/// A system command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Lock,
    Sleep,
    Hibernate,
    Restart,
    ShutDown,
    SignOut,
    EmptyRecycleBin,
}

impl Command {
    /// Every command, in the order equal matches list them.
    pub const ALL: [Command; 7] = [
        Command::Lock,
        Command::Sleep,
        Command::Hibernate,
        Command::Restart,
        Command::ShutDown,
        Command::SignOut,
        Command::EmptyRecycleBin,
    ];

    /// What the row says after the first Enter, for a command that asks
    /// before it runs; `None` for one that runs at once.
    pub fn asking(self) -> Option<Text> {
        match self {
            Command::Lock => None,
            Command::Sleep => Some(Text::AskSleep),
            Command::Hibernate => Some(Text::AskHibernate),
            Command::Restart => Some(Text::AskRestart),
            Command::ShutDown => Some(Text::AskShutDown),
            Command::SignOut => Some(Text::AskSignOut),
            Command::EmptyRecycleBin => Some(Text::AskEmptyRecycleBin),
        }
    }

    /// Its name, as the row's title.
    fn name(self) -> Text {
        match self {
            Command::Lock => Text::Lock,
            Command::Sleep => Text::Sleep,
            Command::Hibernate => Text::Hibernate,
            Command::Restart => Text::Restart,
            Command::ShutDown => Text::ShutDown,
            Command::SignOut => Text::SignOut,
            Command::EmptyRecycleBin => Text::EmptyRecycleBin,
        }
    }

    /// Words people search it by, in either language.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Command::Lock => &[
                "lock screen",
                "lock pc",
                "блокировка",
                "заблокировать компьютер",
            ],
            Command::Sleep => &["suspend", "standby", "сон", "спать", "ждущий режим"],
            Command::Hibernate => &["hibernation", "suspend to disk"],
            Command::Restart => &["reboot", "перезагрузить", "перезапуск", "рестарт", "ребут"],
            Command::ShutDown => &[
                "shutdown",
                "power off",
                "turn off",
                "выключить",
                "выключение",
                "отключить компьютер",
            ],
            Command::SignOut => &[
                "log out",
                "logout",
                "log off",
                "sign off",
                "выход",
                "выйти из системы",
            ],
            Command::EmptyRecycleBin => {
                &["recycle bin", "trash", "empty trash", "корзина", "очистить"]
            }
        }
    }

    /// Its frecency key.
    pub fn id(self) -> ItemId {
        let name = Language::English.say(self.name());
        ItemId(format!("system:{}", name.to_lowercase().replace(' ', "-")))
    }

    /// Its row in `language`: named in it, found by its name in the other
    /// language and its keywords too.
    pub fn item(self, language: Language) -> Item {
        let title = language.say(self.name());
        let names = Language::ALL.map(|each| each.say(self.name()));
        let other = names.into_iter().filter(|name| *name != title);
        let keywords = other.chain(self.keywords().iter().copied());
        Item {
            id: self.id(),
            title: title.to_string(),
            subtitle: language.say(Text::SystemCommand).to_string(),
            keywords: keywords.map(str::to_string).collect(),
            kind: Kind::Command,
            action: Action::System(self),
        }
    }
}

/// Every command's row in `language`; Hibernate only when `hibernate`,
/// that is, when Windows has hibernation turned on.
pub fn items(language: Language, hibernate: bool) -> Vec<Item> {
    let offered = Command::ALL.into_iter();
    let offered = offered.filter(|command| hibernate || *command != Command::Hibernate);
    offered.map(|command| command.item(language)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frecency::{Frecency, Timestamp};
    use crate::places::Places;
    use crate::rank::rank;
    use crate::system_settings;

    /// The title of the first row `query` finds among the commands and
    /// Windows' settings, in `language`.
    fn first(query: &str, language: Language) -> Option<String> {
        let mut items = items(language, true);
        items.extend(system_settings::items(language));
        let ranked = rank(
            query,
            &items,
            &Frecency::default(),
            &Places::default(),
            Timestamp(0),
        );
        Some(ranked.into_iter().next()?.item.title)
    }

    fn finds(query: &str, command: Command) {
        for language in Language::ALL {
            let name = language.say(command.name()).to_string();
            assert_eq!(first(query, language), Some(name), "{query}");
        }
    }

    #[test]
    fn each_command_is_found_by_its_names_and_words_in_either_language() {
        finds("lock", Command::Lock);
        finds("блок", Command::Lock);
        finds("sleep", Command::Sleep);
        finds("hibernate", Command::Hibernate);
        finds("перезагр", Command::Restart);
        finds("reboot", Command::Restart);
        finds("выкл", Command::ShutDown);
        finds("shut down", Command::ShutDown);
        finds("sign out", Command::SignOut);
        finds("корзина", Command::EmptyRecycleBin);
        finds("empty", Command::EmptyRecycleBin);
    }

    #[test]
    fn a_row_runs_its_command_and_says_it_is_one() {
        let restart = Command::Restart.item(Language::Russian);
        assert_eq!(restart.title, "Перезагрузка");
        assert_eq!(restart.subtitle, "Системная команда");
        assert_eq!(restart.action, Action::System(Command::Restart));
        assert_eq!(restart.kind, Kind::Command);
        assert!(restart.keywords.contains(&"Restart".to_string()));
        assert!(
            !restart.keywords.contains(&"Перезагрузка".to_string()),
            "the title"
        );
        let english = Command::Restart.item(Language::English);
        assert_eq!(
            (english.title.as_str(), english.subtitle.as_str()),
            ("Restart", "System command")
        );
    }

    #[test]
    fn every_command_but_lock_asks_first() {
        let asking: Vec<Command> = Command::ALL
            .into_iter()
            .filter(|command| command.asking().is_some())
            .collect();
        assert_eq!(asking, &Command::ALL[1..]);
        let said = Command::Restart
            .asking()
            .map(|text| Language::Russian.say(text));
        assert_eq!(
            said,
            Some("Нажмите Enter ещё раз, чтобы перезагрузить компьютер")
        );
        let said = Command::ShutDown
            .asking()
            .map(|text| Language::English.say(text));
        assert_eq!(said, Some("Press Enter again to shut down"));
    }

    #[test]
    fn hibernate_is_offered_only_when_windows_hibernates() {
        let titles = |hibernate| -> Vec<String> {
            let items = items(Language::English, hibernate);
            items.into_iter().map(|item| item.title).collect()
        };
        assert!(titles(true).contains(&"Hibernate".to_string()));
        assert!(!titles(false).contains(&"Hibernate".to_string()));
        assert_eq!(titles(false).len(), Command::ALL.len() - 1);
    }

    #[test]
    fn every_command_has_an_id_of_its_own() {
        let ids: std::collections::HashSet<ItemId> =
            Command::ALL.into_iter().map(Command::id).collect();
        assert_eq!(ids.len(), Command::ALL.len());
        assert_eq!(
            Command::EmptyRecycleBin.id(),
            ItemId("system:empty-recycle-bin".to_string())
        );
    }
}
