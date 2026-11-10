//! What the launcher tells the user, quietly: a row under the empty bar
//! (never while typing, so Enter still opens what was searched for), and
//! the tray's tooltip. Enter on it opens the config, where the fix is.

use std::path::Path;

use hyalo_core::config::Problem;
use hyalo_core::hotkey::Hotkey;
use hyalo_core::item::{Action, Item, ItemId, Kind};

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
    /// What the notice says, in a line.
    pub fn said(&self) -> String {
        match self {
            Notice::Config(problem) => format!("config.toml, {problem}"),
            Notice::Hotkey {
                wanted,
                kept: Some(kept),
            } => format!("{wanted} is taken: {kept} still shows hyalo"),
            Notice::Hotkey { wanted, kept: None } => format!("{wanted} is taken by another app"),
        }
    }

    /// The row the launcher lists for it; Enter opens `config`.
    pub fn item(&self, config: &Path) -> Item {
        let advice = match self {
            Notice::Config(_) => "hyalo keeps its last good settings",
            Notice::Hotkey { .. } => "Choose another hotkey",
        };
        Item {
            id: ItemId("hyalo:notice".to_string()),
            title: self.said(),
            subtitle: format!("{advice} · Enter to open config.toml"),
            kind: Kind::Notice,
            action: Action::Open(config.to_string_lossy().into_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey(text: &str) -> Hotkey {
        Hotkey::parse(text).expect("a hotkey")
    }

    fn problem() -> Notice {
        Notice::Config(Problem {
            line: 2,
            message: "unknown field `hotkye`".to_string(),
        })
    }

    const CONFIG: &str = r"C:\Users\Me\AppData\Roaming\hyalo\config.toml";

    #[test]
    fn a_problem_in_the_config_names_its_line() {
        assert_eq!(
            problem().said(),
            "config.toml, line 2: unknown field `hotkye`"
        );
        let item = problem().item(Path::new(CONFIG));
        assert_eq!(item.title, problem().said());
        assert!(
            item.subtitle.contains("last good settings"),
            "{}",
            item.subtitle
        );
        assert!(
            item.subtitle.contains("Enter to open config.toml"),
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
            refused.said(),
            "Ctrl+K is taken: Alt+Space still shows hyalo"
        );
        let alone = Notice::Hotkey {
            wanted: Hotkey::default(),
            kept: None,
        };
        assert_eq!(alone.said(), "Alt+Space is taken by another app");
        let item = alone.item(Path::new(CONFIG));
        assert!(
            item.subtitle.contains("another hotkey"),
            "{}",
            item.subtitle
        );
    }

    #[test]
    fn enter_opens_the_config_and_the_row_is_a_notice() {
        let item = problem().item(Path::new(CONFIG));
        assert_eq!(item.action, Action::Open(CONFIG.to_string()));
        assert_eq!(item.kind, Kind::Notice);
        assert_eq!(item.id, ItemId("hyalo:notice".to_string()));
        assert_eq!(item.path(), None, "not a file to show in its folder");
    }
}
