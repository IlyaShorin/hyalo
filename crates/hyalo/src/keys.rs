//! What a key press asks the launcher to do, and what that does with the
//! selected row. Pure: the window reads the key and the modifiers held,
//! this decides.

use hyalo_core::item::{Action, Item};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
};

/// A key that does something besides typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Hide,
    Up,
    Down,
    /// Open the selected row; as administrator with Ctrl+Shift held.
    Open {
        as_admin: bool,
    },
    /// Show the selected file or folder in its folder: Ctrl+Enter.
    Reveal,
    /// Copy the selected file's or folder's full path: Ctrl+C with no
    /// text selected in the bar (the field says which keys copy).
    Copy,
}

/// Which modifier keys are held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub ctrl: bool,
    pub shift: bool,
}

/// The command `key` gives with `held` modifiers; `None` for keys that
/// type (those arrive again as characters) or edit the query.
pub fn command(key: VIRTUAL_KEY, held: Held) -> Option<Command> {
    match key {
        VK_ESCAPE => Some(Command::Hide),
        VK_UP => Some(Command::Up),
        VK_DOWN => Some(Command::Down),
        VK_RETURN => Some(enter(held)),
        _ => None,
    }
}

/// Enter opens, Ctrl+Enter shows in the folder, Ctrl+Shift+Enter opens as
/// administrator.
fn enter(held: Held) -> Command {
    match (held.ctrl, held.shift) {
        (true, true) => Command::Open { as_admin: true },
        (true, false) => Command::Reveal,
        _ => Command::Open { as_admin: false },
    }
}

/// What `command` does with `item`, and whether as administrator; `None`
/// when it does nothing with it: only files and folders are shown in
/// their folder or have a path to copy, and moving or hiding acts on no
/// row.
pub fn action(command: Command, item: &Item) -> Option<(Action, bool)> {
    match command {
        Command::Open { as_admin } => Some((item.action.clone(), as_admin)),
        Command::Reveal => Some((Action::Reveal(item.path()?.to_string()), false)),
        Command::Copy => Some((Action::CopyText(item.path()?.to_string()), false)),
        Command::Hide | Command::Up | Command::Down => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::item::{ItemId, Kind};
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_A, VK_SPACE};

    const NONE: Held = Held {
        ctrl: false,
        shift: false,
    };

    const CTRL: Held = Held {
        ctrl: true,
        shift: false,
    };

    #[test]
    fn escape_hides_and_the_arrows_move() {
        assert_eq!(command(VK_ESCAPE, NONE), Some(Command::Hide));
        assert_eq!(command(VK_UP, NONE), Some(Command::Up));
        assert_eq!(command(VK_DOWN, NONE), Some(Command::Down));
    }

    #[test]
    fn enter_opens() {
        let open = Some(Command::Open { as_admin: false });
        assert_eq!(command(VK_RETURN, NONE), open);
    }

    #[test]
    fn ctrl_shift_enter_opens_as_administrator() {
        let both = Held {
            ctrl: true,
            shift: true,
        };
        let admin = Some(Command::Open { as_admin: true });
        assert_eq!(command(VK_RETURN, both), admin);
    }

    #[test]
    fn shift_enter_opens_normally() {
        let shift = Held {
            ctrl: false,
            shift: true,
        };
        let open = Some(Command::Open { as_admin: false });
        assert_eq!(command(VK_RETURN, shift), open);
    }

    #[test]
    fn ctrl_enter_shows_the_row_in_its_folder() {
        assert_eq!(command(VK_RETURN, CTRL), Some(Command::Reveal));
    }

    #[test]
    fn ctrl_c_is_the_fields_to_decide() {
        use windows::Win32::UI::Input::KeyboardAndMouse::VK_C;
        assert_eq!(command(VK_C, CTRL), None);
        assert_eq!(command(VK_C, NONE), None);
    }

    #[test]
    fn letters_and_space_are_typing_not_commands() {
        assert_eq!(command(VK_A, NONE), None);
        assert_eq!(command(VK_SPACE, NONE), None);
        assert_eq!(command(VK_A, CTRL), None);
    }

    fn app() -> Item {
        Item {
            id: ItemId("Microsoft.Windows.Explorer".to_string()),
            title: "File Explorer".to_string(),
            subtitle: "App".to_string(),
            kind: Kind::App,
            action: Action::Open(r"shell:AppsFolder\Microsoft.Windows.Explorer".to_string()),
        }
    }

    #[test]
    fn opening_runs_the_rows_own_action() {
        let opened = action(Command::Open { as_admin: true }, &app());
        assert_eq!(opened, Some((app().action, true)));
    }

    #[test]
    fn a_file_is_shown_in_its_folder_and_its_path_copied() {
        let path = r"C:\Users\Me\notes.txt";
        let file = Item::file(path.to_string(), false);
        let shown = action(Command::Reveal, &file);
        assert_eq!(shown, Some((Action::Reveal(path.to_string()), false)));
        let copied = action(Command::Copy, &file);
        assert_eq!(copied, Some((Action::CopyText(path.to_string()), false)));
    }

    #[test]
    fn an_app_has_no_folder_to_show_or_path_to_copy() {
        assert_eq!(action(Command::Reveal, &app()), None);
        assert_eq!(action(Command::Copy, &app()), None);
    }

    #[test]
    fn moving_and_hiding_act_on_no_row() {
        for command in [Command::Hide, Command::Up, Command::Down] {
            assert_eq!(action(command, &app()), None, "{command:?}");
        }
    }
}
