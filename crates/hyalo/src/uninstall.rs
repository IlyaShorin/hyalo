//! Uninstalling apps: what the Uninstall keys list, the folders telling
//! an app's uninstaller needs, and removing a package. Which uninstaller
//! removes which app is decided in `hyalo_core::uninstall`; starting a
//! program uninstaller is `launch`'s.
//!
//! No test reads the real Uninstall keys or removes a real package: the
//! keys are read through `entries_under`, tested on keys of the test's
//! own, and the apps provider takes what it lists as a stand-in; `remove`
//! is asked only for a package no machine has. Window tests see an
//! uninstall reach their stand-in runner.

use hyalo_core::uninstall::{Entry, Folders};
use windows::Management::Deployment::PackageManager;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
    FOLDERID_Profile, FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesCommon,
    FOLDERID_ProgramFilesCommonX86, FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData,
    FOLDERID_UserProfiles, FOLDERID_UserProgramFiles, FOLDERID_Windows, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath,
};
use windows::core::{GUID, HSTRING, Result};

use crate::com::keep_multithreaded;
use crate::registry::{self, STRING};

/// Where Programs and Features finds what it lists: the machine's 64-bit
/// and 32-bit programs, then the current user's.
pub const KEYS: [(HKEY, &str); 3] = [
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
];

/// Every entry the Uninstall keys list. Reads the registry, about 0.1 s
/// for a few hundred entries: call it off the UI thread.
pub fn entries() -> Vec<Entry> {
    let each = KEYS.iter().map(|&(root, key)| entries_under(root, key));
    each.flatten().collect()
}

/// An entry per subkey of `key` under `root`; none when it is not there.
pub fn entries_under(root: HKEY, key: &str) -> Vec<Entry> {
    let subkeys = registry::subkeys(root, key).into_iter();
    subkeys.map(|name| entry(root, key, name)).collect()
}

/// The entry of subkey `name` of `key` under `root`: what it says, a
/// value that is not there read as empty, or as not set.
fn entry(root: HKEY, key: &str, name: String) -> Entry {
    let path = format!(r"{key}\{name}");
    let text = |value| registry::value(root, &path, value, STRING);
    let text = |value| text(value).map_or_else(String::new, |bytes| registry::text(&bytes));
    let set = |value| registry::value(root, &path, value, RRF_RT_REG_DWORD) == Some(ONE.to_vec());
    Entry {
        key: name,
        name: text("DisplayName"),
        location: text("InstallLocation"),
        icon: text("DisplayIcon"),
        command: text("UninstallString"),
        windows_installer: set("WindowsInstaller"),
        system_component: set("SystemComponent"),
        no_remove: set("NoRemove"),
        parent: text("ParentKeyName"),
    }
}

/// A DWORD of 1, as the registry holds it.
const ONE: [u8; 4] = 1_u32.to_le_bytes();

/// Removes the package `full_name` for the current user, as Start's
/// Uninstall does, and waits until it is gone: call it off the UI thread.
pub fn remove(full_name: &str) -> Result<()> {
    keep_multithreaded()?;
    let removing = PackageManager::new()?.RemovePackageAsync(&HSTRING::from(full_name))?;
    // A removal that failed still hands back its result, with the error
    // in it.
    removing.join()?.ExtendedErrorCode()?.ok()
}

/// The folders many apps are installed in, none of them one app's own.
const SHARED: [GUID; 13] = [
    FOLDERID_ProgramFiles,
    FOLDERID_ProgramFilesX86,
    FOLDERID_ProgramFilesCommon,
    FOLDERID_ProgramFilesCommonX86,
    FOLDERID_ProgramData,
    FOLDERID_UserProfiles,
    FOLDERID_Profile,
    FOLDERID_LocalAppData,
    FOLDERID_RoamingAppData,
    FOLDERID_UserProgramFiles,
    FOLDERID_Desktop,
    FOLDERID_Documents,
    FOLDERID_Downloads,
];

/// This machine's folders the matching needs: Windows' own, and those
/// many apps are installed in.
pub fn folders() -> Folders {
    let windows = known(&FOLDERID_Windows).unwrap_or_default();
    let shared: Vec<String> = SHARED.iter().filter_map(known).collect();
    Folders::new(&windows, shared.iter().map(String::as_str))
}

/// Where the known folder `id` is; `None` when it has no place here.
fn known(id: &GUID) -> Option<String> {
    // SAFETY: the path the call allocates is read once, then freed once.
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let text = String::from_utf16_lossy(path.as_wide());
        CoTaskMemFree(Some(path.0.cast()));
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::TestKey;
    use hyalo_core::uninstall::{App, find};

    #[test]
    fn each_subkey_is_an_entry_with_what_it_says() {
        let test = TestKey::new("uninstall-entries");
        let everything = r"Uninstall\{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}";
        test.text(everything, "DisplayName", "Everything 1.4.1.1032 (x64)");
        test.expandable(
            everything,
            "UninstallString",
            r"%SystemRoot%\System32\MsiExec.exe /X{B5F500BB}",
        );
        test.number(everything, "WindowsInstaller", 1);
        test.number(everything, "NoRemove", 0);
        let python = r"Uninstall\Python";
        test.text(python, "DisplayName", "Python Core");
        test.text(python, "InstallLocation", r"C:\Python");
        test.text(python, "DisplayIcon", r"C:\Python\python.exe,0");
        test.number(python, "SystemComponent", 1);
        test.number(python, "NoRemove", 1);
        test.text(python, "ParentKeyName", "Python");
        let mut entries = entries_under(HKEY_CURRENT_USER, &test.under("Uninstall"));
        entries.sort_by(|one, other| one.key.cmp(&other.key));
        let root = std::env::var("SystemRoot").expect("SystemRoot is set");
        assert_eq!(
            entries,
            [
                Entry {
                    key: "Python".to_string(),
                    name: "Python Core".to_string(),
                    location: r"C:\Python".to_string(),
                    icon: r"C:\Python\python.exe,0".to_string(),
                    system_component: true,
                    no_remove: true,
                    parent: "Python".to_string(),
                    ..Entry::default()
                },
                Entry {
                    key: "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}".to_string(),
                    name: "Everything 1.4.1.1032 (x64)".to_string(),
                    command: format!(r"{root}\System32\MsiExec.exe /X{{B5F500BB}}"),
                    windows_installer: true,
                    ..Entry::default()
                },
            ]
        );
    }

    #[test]
    fn removing_a_package_that_is_not_there_fails_and_removes_nothing() {
        let error = remove("hyalo.test.NoSuchPackage_1.0.0.0_x64__0000000000000")
            .expect_err("no such package");
        // ERROR_INSTALL_PACKAGE_NOT_FOUND, as an HRESULT.
        assert_eq!(error.code().0 as u32, 0x8007_3CF1, "{error}");
    }

    #[test]
    fn a_key_that_is_not_there_lists_nothing() {
        let test = TestKey::new("uninstall-missing");
        assert!(entries_under(HKEY_CURRENT_USER, &test.under("Uninstall")).is_empty());
    }

    #[test]
    fn programs_and_features_keys_are_the_machines_both_views_and_the_users() {
        let keys: Vec<String> = KEYS.iter().map(|(_, key)| key.to_lowercase()).collect();
        assert_eq!(
            keys,
            [
                r"software\microsoft\windows\currentversion\uninstall",
                r"software\wow6432node\microsoft\windows\currentversion\uninstall",
                r"software\microsoft\windows\currentversion\uninstall",
            ]
        );
        let roots: Vec<HKEY> = KEYS.iter().map(|&(root, _)| root).collect();
        assert_eq!(
            roots,
            [HKEY_LOCAL_MACHINE, HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
        );
    }

    /// A packaged app installed in `path`.
    fn packaged(path: String) -> App {
        App {
            title: "App".to_string(),
            package: "App_1.0.0.0_x64__0000000000000".to_string(),
            package_path: path,
            ..App::default()
        }
    }

    /// The environment variable `name`, which Windows sets.
    fn variable(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is set"))
    }

    #[test]
    fn this_machines_windows_folder_is_windows_own_and_program_files_is_shared() {
        let folders = folders();
        let system = packaged(format!(r"{}\SystemApps\App", variable("SystemRoot")));
        assert_eq!(find(&system, &[], &folders), None, "Windows' own");
        let store = packaged(format!(r"{}\WindowsApps\App", variable("ProgramFiles")));
        assert!(find(&store, &[], &folders).is_some());
        for shared in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "LOCALAPPDATA",
            "APPDATA",
            "USERPROFILE",
        ] {
            let folder = variable(shared);
            let careless = Entry {
                key: "Careless".to_string(),
                name: "Careless".to_string(),
                location: folder.clone(),
                command: "u.exe".to_string(),
                ..Entry::default()
            };
            let app = App {
                title: "Other".to_string(),
                target: format!(r"{folder}\Other\other.exe"),
                ..App::default()
            };
            assert_eq!(find(&app, &[careless], &folders), None, "{shared}");
        }
    }
}
