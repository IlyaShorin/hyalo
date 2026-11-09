//! What a key press asks the launcher to do. Pure: the window reads the
//! key and the modifiers held, this decides.

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
}

/// Which modifier keys are held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub ctrl: bool,
    pub shift: bool,
}

/// The command `key` gives with `held` modifiers; `None` for keys that
/// only type (those arrive again as characters).
pub fn command(key: VIRTUAL_KEY, held: Held) -> Option<Command> {
    match key {
        VK_ESCAPE => Some(Command::Hide),
        VK_UP => Some(Command::Up),
        VK_DOWN => Some(Command::Down),
        VK_RETURN => Some(Command::Open {
            as_admin: held.ctrl && held.shift,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_A, VK_SPACE};

    const NONE: Held = Held {
        ctrl: false,
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
    fn ctrl_or_shift_alone_opens_normally() {
        let open = Some(Command::Open { as_admin: false });
        let ctrl = Held {
            ctrl: true,
            shift: false,
        };
        let shift = Held {
            ctrl: false,
            shift: true,
        };
        assert_eq!(command(VK_RETURN, ctrl), open);
        assert_eq!(command(VK_RETURN, shift), open);
    }

    #[test]
    fn letters_and_space_are_typing_not_commands() {
        assert_eq!(command(VK_A, NONE), None);
        assert_eq!(command(VK_SPACE, NONE), None);
    }
}
