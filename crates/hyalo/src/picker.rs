//! Asking the user for a folder: the system's folder picker, a modal
//! dialog with a message loop of its own, so window tests hand the
//! settings window a stand-in. What can be checked without showing it is
//! tested here; showing it is checked by hand.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HSTRING, Result};

use crate::com::Com;

/// Asks the user for a folder.
pub trait Picker {
    /// The full path of the folder the user picks in a dialog over
    /// `owner`, titled `title`; `None` when they cancel it.
    fn pick(&self, owner: HWND, title: &str) -> Option<String>;
}

/// The folder picker Windows has: `IFileOpenDialog` picking folders.
pub struct System;

impl Picker for System {
    fn pick(&self, owner: HWND, title: &str) -> Option<String> {
        Dialog::new(title).ok()?.show(owner)
    }
}

/// The dialog, and COM for as long as it lives: the dialog is released
/// first, as fields drop in order.
struct Dialog {
    dialog: IFileOpenDialog,
    _com: Com,
}

impl Dialog {
    /// A dialog titled `title` that picks one folder of the file system,
    /// one that is there.
    fn new(title: &str) -> Result<Dialog> {
        let com = Com::init()?;
        // SAFETY: the dialog is a live COM object of this thread.
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let options = dialog.GetOptions()?;
            dialog
                .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
            dialog.SetTitle(&HSTRING::from(title))?;
            Ok(Dialog { dialog, _com: com })
        }
    }

    /// Shows the dialog over `owner` until the user picks a folder or
    /// cancels.
    fn show(&self, owner: HWND) -> Option<String> {
        // SAFETY: a live dialog of this thread; Show returns once it closes.
        let shown = unsafe { self.dialog.Show(Some(owner)) };
        chosen(&self.dialog, shown)
    }
}

/// The folder picked in `dialog`, once `shown` says the user picked one;
/// cancelling is an error too, the one most often seen.
fn chosen(dialog: &IFileOpenDialog, shown: Result<()>) -> Option<String> {
    let picked = shown.and_then(|()| unsafe { dialog.GetResult() });
    match picked.and_then(|item| path(&item)) {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("hyalo: no folder picked: {error}");
            None
        }
    }
}

/// The full path of the file system item `item`.
fn path(item: &IShellItem) -> Result<String> {
    // SAFETY: the text GetDisplayName gives is ours: read once, freed once.
    unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let text = name.to_string();
        CoTaskMemFree(Some(name.0.cast()));
        Ok(text?)
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::UI::Shell::SHCreateItemFromParsingName;
    use windows::core::HRESULT;

    use super::*;
    use crate::scratch::Scratch;

    #[test]
    fn the_dialog_picks_one_folder_of_the_file_system_that_is_there() {
        let dialog = Dialog::new("Rank a folder higher").expect("a dialog");
        let options = unsafe { dialog.dialog.GetOptions() }.expect("its options");
        let wanted = FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
        assert_eq!(options & wanted, wanted);
    }

    #[test]
    fn a_folder_picked_is_its_full_path() {
        let _com = Com::init().expect("COM");
        let scratch = Scratch::new("picker-path").with(&["Some Folder\\"]);
        let folder = scratch.path("Some Folder");
        let item: IShellItem =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(&folder), None) }.expect("an item");
        assert_eq!(path(&item).expect("a path"), folder);
    }

    #[test]
    fn a_dialog_cancelled_or_never_shown_picks_nothing() {
        let dialog = Dialog::new("Leave a folder out of the index").expect("a dialog");
        let cancelled = Err(HRESULT::from_win32(ERROR_CANCELLED.0).into());
        assert_eq!(chosen(&dialog.dialog, cancelled), None);
        assert_eq!(
            chosen(&dialog.dialog, Ok(())),
            None,
            "nothing picked before it shows"
        );
    }
}
