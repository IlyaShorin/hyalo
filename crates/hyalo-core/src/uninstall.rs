//! Uninstalling an app Start lists: which uninstaller removes it, told
//! from what the shell says about the app and what the Uninstall keys
//! list, and how that uninstaller is started. Pure: the launcher reads
//! the shell and the registry, this decides.
//!
//! A packaged app (a Store app, or a desktop app in an MSIX package) is
//! removed through the package manager, unless its package is part of
//! Windows. Any other app is removed by the program its Uninstall entry
//! names, as Settings starts it. Its entry is, of those Programs and
//! Features lists: a Steam game's own; else the one whose icon is the
//! app's program; else the one installed in the deepest folder the
//! program is in, never a folder many apps share; else, when the app's
//! title names only one, the one named so, maybe with a version after
//! the name (`Everything 1.4.1.1032 (x64)` for Everything, which its MSI
//! installs with neither a folder nor an icon). An app of Windows' own
//! has none; neither has a link or a shell folder.

/// How an app is uninstalled.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Uninstall {
    /// Start the program the app's Uninstall entry names: it asks for
    /// elevation itself when it needs it.
    Program {
        /// The entry's name, which the row names when it asks.
        name: String,
        program: String,
        arguments: String,
    },
    /// Remove the app's package for the current user.
    Package {
        /// The app's title.
        name: String,
        full_name: String,
    },
}

impl Uninstall {
    /// What is uninstalled: the entry's name, or the app's.
    pub fn name(&self) -> &str {
        match self {
            Uninstall::Program { name, .. } | Uninstall::Package { name, .. } => name,
        }
    }
}

/// What the shell says about an app of the Applications folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct App {
    pub title: String,
    /// Its AppUserModelID, path or address.
    pub parsing_name: String,
    /// What it starts (`System.Link.TargetParsingPath`): a program's
    /// path, an address or a shell folder; empty when the shell says
    /// nothing.
    pub target: String,
    /// Its package's full name, for a packaged app; empty for others.
    pub package: String,
    /// Where its package is installed, for a packaged app.
    pub package_path: String,
}

/// One entry of the Uninstall keys: its key's name and its values, as
/// read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// The key's name: a product code for an MSI, `Steam App <n>` for a
    /// Steam game.
    pub key: String,
    /// `DisplayName`.
    pub name: String,
    /// `InstallLocation`.
    pub location: String,
    /// `DisplayIcon`: a file, maybe with an icon's index after a comma.
    pub icon: String,
    /// `UninstallString`: a command line.
    pub command: String,
    /// `WindowsInstaller` is 1: an MSI installed it.
    pub windows_installer: bool,
    /// `SystemComponent` is 1: Programs and Features does not list it.
    pub system_component: bool,
    /// `NoRemove` is 1: it may not be uninstalled.
    pub no_remove: bool,
    /// `ParentKeyName`: an update of another entry.
    pub parent: String,
}

impl Entry {
    /// Whether Programs and Features lists it with something that
    /// uninstalls it.
    fn listed(&self) -> bool {
        let removes = !self.command.trim().is_empty() || self.product().is_some();
        let shown = !self.system_component && self.parent.is_empty();
        !self.name.trim().is_empty() && removes && shown && !self.no_remove
    }

    /// Its MSI's product code: the key's name, for an entry an MSI made.
    fn product(&self) -> Option<&str> {
        (self.windows_installer && is_guid(&self.key)).then_some(self.key.as_str())
    }

    /// What uninstalls it: for an MSI, Windows Installer removing its
    /// product, as Programs and Features does (its own command may only
    /// change it); else its command.
    fn uninstall(&self) -> Uninstall {
        let (program, arguments) = match self.product() {
            Some(code) => ("msiexec.exe".to_string(), format!("/x {code}")),
            None => split(&self.command),
        };
        Uninstall::Program {
            name: self.name.trim().to_string(),
            program,
            arguments,
        }
    }
}

/// The folders on this machine the matching needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folders {
    /// Windows' own: what is in it is part of Windows.
    windows: Option<Path>,
    /// Folders many apps are installed in, so none is an app's own:
    /// Program Files, the profile, AppData and the like.
    shared: Vec<Path>,
}

impl Folders {
    /// `windows` and the `shared` folders, as full paths; one that is not
    /// is left out.
    pub fn new<'a>(windows: &str, shared: impl IntoIterator<Item = &'a str>) -> Folders {
        Folders {
            windows: Path::parse(windows),
            shared: shared.into_iter().filter_map(Path::parse).collect(),
        }
    }

    /// Whether `path` is part of Windows.
    fn windows(&self, path: &Path) -> bool {
        self.windows
            .as_ref()
            .is_some_and(|windows| path.within(windows))
    }

    /// Whether `folder` could be one app's own: not a drive, not shared.
    fn own(&self, folder: &Path) -> bool {
        !folder.is_drive() && !self.shared.contains(folder)
    }
}

/// What uninstalls `app`, of what the Uninstall keys list in `entries`;
/// `None` when it cannot be told for sure, or when the app is part of
/// Windows.
pub fn find(app: &App, entries: &[Entry], folders: &Folders) -> Option<Uninstall> {
    if !app.package.is_empty() {
        return packaged(app, folders);
    }
    let listed: Vec<&Entry> = entries.iter().filter(|entry| entry.listed()).collect();
    let found = steam(app, &listed).or_else(|| match Path::parse(&app.target) {
        Some(target) if folders.windows(&target) => None,
        Some(target) => by_icon(&target, &listed)
            .or_else(|| by_folder(&target, &listed, folders))
            .or_else(|| by_name(&app.title, &listed)),
        None if app.target.trim().is_empty() => by_name(&app.title, &listed),
        None => None,
    });
    found.map(Entry::uninstall)
}

/// A packaged app's package, removed for the current user, unless it is
/// installed in Windows' folder (the system's own, which Windows does
/// not let go) or nobody knows where.
fn packaged(app: &App, folders: &Folders) -> Option<Uninstall> {
    let place = Path::parse(&app.package_path);
    let removable = place.is_some_and(|place| !folders.windows(&place));
    removable.then(|| Uninstall::Package {
        name: app.title.clone(),
        full_name: app.package.clone(),
    })
}

/// A Steam game's entry: Steam lists each game it installed as
/// `Steam App <number>`, and Start opens it at
/// `steam://rungameid/<number>`.
fn steam<'a>(app: &App, listed: &[&'a Entry]) -> Option<&'a Entry> {
    let game = app.parsing_name.strip_prefix("steam://rungameid/")?;
    let key = format!("Steam App {game}");
    listed.iter().copied().find(|entry| entry.key == key)
}

/// The entry whose icon is `target`, the app's program.
fn by_icon<'a>(target: &Path, listed: &[&'a Entry]) -> Option<&'a Entry> {
    let icon = |entry: &&Entry| Path::parse(icon_file(&entry.icon));
    listed
        .iter()
        .copied()
        .find(|entry| icon(entry).as_ref() == Some(target))
}

/// The entry installed in the deepest folder `target` is in, of the
/// folders that can be one app's own.
fn by_folder<'a>(target: &Path, listed: &[&'a Entry], folders: &Folders) -> Option<&'a Entry> {
    let located = listed.iter().copied().filter_map(|entry| {
        let folder = Path::parse(&entry.location)?;
        let holds = folders.own(&folder) && target.within(&folder);
        holds.then_some((folder.0.len(), entry))
    });
    located
        .max_by_key(|&(depth, _)| depth)
        .map(|(_, entry)| entry)
}

/// The one entry named `title`, maybe with a version after it; `None`
/// when none is, or more than one.
fn by_name<'a>(title: &str, listed: &[&'a Entry]) -> Option<&'a Entry> {
    let mut named = listed
        .iter()
        .copied()
        .filter(|entry| names(&entry.name, title));
    let first = named.next()?;
    named.next().is_none().then_some(first)
}

/// Whether the entry named `name` is the app titled `title`: the same
/// name in any case, or the name and then a version (a digit, `v` and a
/// digit, or something in brackets).
fn names(name: &str, title: &str) -> bool {
    let (name, title) = (name.trim().to_lowercase(), title.trim().to_lowercase());
    if title.is_empty() || name == title {
        return !title.is_empty();
    }
    let Some(rest) = name
        .strip_prefix(&title)
        .and_then(|rest| rest.strip_prefix(' '))
    else {
        return false;
    };
    let version = rest.strip_prefix('v').unwrap_or(rest);
    rest.starts_with('(') || version.starts_with(|letter: char| letter.is_ascii_digit())
}

/// The file of a `DisplayIcon`: in quotes, or without an icon's index
/// after a comma (`C:\app.exe,0`, `"C:\app.exe",-101`).
fn icon_file(icon: &str) -> &str {
    let icon = icon.trim();
    if let Some(quoted) = icon.strip_prefix('"') {
        return quoted.split_once('"').map_or(quoted, |(file, _)| file);
    }
    match icon.rsplit_once(',') {
        Some((file, index)) if is_index(index) => file,
        _ => icon,
    }
}

/// Whether `text` is an icon's index: digits, maybe after a minus.
fn is_index(text: &str) -> bool {
    let digits = text.trim().strip_prefix('-').unwrap_or(text.trim());
    !digits.is_empty() && digits.chars().all(|letter| letter.is_ascii_digit())
}

/// Whether `key` is a GUID in braces, as an MSI's product code is.
fn is_guid(key: &str) -> bool {
    let Some(inner) = key.strip_prefix('{').and_then(|key| key.strip_suffix('}')) else {
        return false;
    };
    let groups: Vec<&str> = inner.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    let hex = groups
        .iter()
        .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()));
    lengths == [8, 4, 4, 4, 12] && hex
}

/// A command line split into its program and the arguments after it:
/// the program in quotes, or up to the first `.exe` a space or the end
/// follows (an unquoted path may have spaces in it), or up to the first
/// space (`winget uninstall …`).
pub fn split(command: &str) -> (String, String) {
    let command = command.trim();
    if let Some(quoted) = command.strip_prefix('"') {
        let (program, arguments) = quoted.split_once('"').unwrap_or((quoted, ""));
        return (program.to_string(), arguments.trim().to_string());
    }
    let lower = command.to_ascii_lowercase();
    let mut exes = lower.match_indices(".exe").map(|(at, _)| at + ".exe".len());
    let after_exe =
        exes.find(|&end| command[end..].starts_with([' ', '\t']) || end == command.len());
    let end = after_exe
        .or_else(|| command.find([' ', '\t']))
        .unwrap_or(command.len());
    (
        command[..end].to_string(),
        command[end..].trim().to_string(),
    )
}

/// A full path as paths compare: letters in one case, backslashes, no
/// quotes around it and no backslash at its end; `C:` for a drive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Path(String);

impl Path {
    /// `text` as a path, if it is a full one: a drive's (`C:\…`, `C:/…`)
    /// or a share's (`\\server\…`).
    fn parse(text: &str) -> Option<Path> {
        let text = text.trim().trim_matches('"').replace('/', "\\");
        let path = text.trim_end_matches('\\').to_lowercase();
        let mut letters = path.chars();
        let drive = letters
            .next()
            .is_some_and(|letter| letter.is_ascii_alphabetic())
            && letters.next() == Some(':')
            && letters.next().is_none_or(|letter| letter == '\\');
        (drive || path.starts_with("\\\\")).then_some(Path(path))
    }

    /// Whether this is in `folder`, or is it.
    fn within(&self, folder: &Path) -> bool {
        let inside = self.0.strip_prefix(&folder.0);
        inside.is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
    }

    /// Whether this is a drive's root, `C:`.
    fn is_drive(&self) -> bool {
        self.0.len() == 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This machine's folders, as the launcher finds them.
    fn folders() -> Folders {
        let shared = [
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\Program Files\Common Files",
            r"C:\ProgramData",
            r"C:\Users\me",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me\AppData\Roaming",
            r"C:\Users\me\AppData\Local\Programs",
        ];
        Folders::new(r"C:\WINDOWS", shared)
    }

    /// A desktop app titled `title` whose shortcut starts `target`.
    fn app(title: &str, target: &str) -> App {
        App {
            title: title.to_string(),
            parsing_name: target.to_string(),
            target: target.to_string(),
            ..App::default()
        }
    }

    fn entry(key: &str, name: &str, command: &str) -> Entry {
        Entry {
            key: key.to_string(),
            name: name.to_string(),
            command: command.to_string(),
            ..Entry::default()
        }
    }

    /// Entries as this machine's Uninstall keys list them.
    fn entries() -> Vec<Entry> {
        let everything = "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}";
        vec![
            Entry {
                windows_installer: true,
                ..entry(
                    everything,
                    "Everything 1.4.1.1032 (x64)",
                    &format!("MsiExec.exe /X{everything}"),
                )
            },
            Entry {
                location: r"C:\Program Files\Google\Chrome\Application".to_string(),
                icon: r"C:\Program Files\Google\Chrome\Application\chrome.exe,0".to_string(),
                ..entry(
                    "Google Chrome",
                    "Google Chrome",
                    r#""C:\Program Files\Google\Chrome\Application\157.0.8103.51\Installer\setup.exe" --uninstall --channel=stable --system-level"#,
                )
            },
            Entry {
                location: r"C:\Program Files\cursor\".to_string(),
                icon: r"C:\Program Files\cursor\Cursor.exe".to_string(),
                ..entry(
                    "{D7D7D7D7-7D7D-7D7D-7D7D-7D7D7D7D7D7D}}_is1",
                    "Cursor",
                    r#""C:\Program Files\cursor\unins000.exe""#,
                )
            },
            Entry {
                location: r"C:\Users\me\AppData\Local\Discord".to_string(),
                ..entry(
                    "Discord",
                    "Discord",
                    r"C:\Users\me\AppData\Local\Discord\Update.exe --uninstall",
                )
            },
            Entry {
                location: r"C:\Program Files\Microsoft Office".to_string(),
                icon: r"C:\Program Files\Common Files\Microsoft Shared\ClickToRun\OfficeClickToRun.exe".to_string(),
                ..entry(
                    "O365HomePremRetail - en-us",
                    "Microsoft 365 - en-us",
                    r#""C:\Program Files\Common Files\Microsoft Shared\ClickToRun\OfficeClickToRun.exe" scenario=install scenariosubtype=ARP"#,
                )
            },
            Entry {
                location: r"C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine"
                    .to_string(),
                ..entry(
                    "Steam App 431960",
                    "Wallpaper Engine",
                    r#""C:\Program Files (x86)\Steam\steam.exe" steam://uninstall/431960"#,
                )
            },
            Entry {
                windows_installer: true,
                system_component: true,
                ..entry(
                    "{9D09C9EF-A57C-422A-B79A-CC922079653A}",
                    "Python 3.12.10 Core Interpreter (64-bit)",
                    "MsiExec.exe /I{9D09C9EF-A57C-422A-B79A-CC922079653A}",
                )
            },
        ]
    }

    fn found(app: &App) -> Option<Uninstall> {
        find(app, &entries(), &folders())
    }

    fn program(name: &str, program: &str, arguments: &str) -> Option<Uninstall> {
        Some(Uninstall::Program {
            name: name.to_string(),
            program: program.to_string(),
            arguments: arguments.to_string(),
        })
    }

    #[test]
    fn everything_from_its_msi_is_found_by_its_name_and_removed_by_windows_installer() {
        let everything = app("Everything", r"C:\Program Files\Everything\Everything.exe");
        assert_eq!(
            found(&everything),
            program(
                "Everything 1.4.1.1032 (x64)",
                "msiexec.exe",
                "/x {B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}"
            )
        );
    }

    #[test]
    fn an_app_whose_program_is_its_entrys_icon_runs_that_entrys_command() {
        let chrome = app(
            "Google Chrome",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        );
        assert_eq!(
            found(&chrome),
            program(
                "Google Chrome",
                r"C:\Program Files\Google\Chrome\Application\157.0.8103.51\Installer\setup.exe",
                "--uninstall --channel=stable --system-level"
            )
        );
        let cursor = app("Cursor", r"C:\PROGRAM FILES\Cursor\cursor.exe");
        let cursor = found(&cursor).expect("Cursor's uninstaller");
        assert_eq!(cursor.name(), "Cursor", "paths in any case");
    }

    #[test]
    fn an_app_in_an_entrys_folder_is_that_entrys_and_a_suites_app_is_the_suites() {
        let discord = app("Discord", r"C:\Users\me\AppData\Local\Discord\Discord.exe");
        assert_eq!(
            found(&discord),
            program(
                "Discord",
                r"C:\Users\me\AppData\Local\Discord\Update.exe",
                "--uninstall"
            )
        );
        let word = app(
            "Word",
            r"C:\Program Files\Microsoft Office\root\Office16\WINWORD.EXE",
        );
        let word = found(&word).expect("Microsoft 365's uninstaller");
        assert_eq!(word.name(), "Microsoft 365 - en-us");
    }

    #[test]
    fn the_deepest_folder_wins_and_a_folder_many_apps_share_is_nobodys() {
        let location = |key: &str, location: &str| Entry {
            location: location.to_string(),
            ..entry(key, key, &format!(r"{location}\uninstall.exe"))
        };
        let entries = [
            location("Vendor tools", r"C:\Tools\Vendor"),
            location("Vendor editor", r"C:\Tools\Vendor\Editor"),
            location("Careless", r"C:\Program Files"),
            location("Rooted", "C:/"),
        ];
        let editor = app("Editor", r"C:\Tools\Vendor\Editor\bin\editor.exe");
        let editor = find(&editor, &entries, &folders()).expect("the editor's");
        assert_eq!(editor.name(), "Vendor editor");
        let other = app("Other", r"C:\Program Files\Other\other.exe");
        assert_eq!(find(&other, &entries, &folders()), None);
        let anywhere = app("Anywhere", r"C:\anywhere.exe");
        assert_eq!(find(&anywhere, &entries, &folders()), None);
        let next_door = app("Next door", r"C:\Tools\Vendor Two\two.exe");
        assert_eq!(
            find(&next_door, &entries, &folders()),
            None,
            "not a folder in it"
        );
    }

    #[test]
    fn a_steam_game_is_found_by_its_number() {
        let game = App {
            title: "Wallpaper Engine: Something Else".to_string(),
            ..app("", "steam://rungameid/431960")
        };
        let game = found(&game).expect("the game's");
        assert_eq!(
            game,
            Uninstall::Program {
                name: "Wallpaper Engine".to_string(),
                program: r"C:\Program Files (x86)\Steam\steam.exe".to_string(),
                arguments: "steam://uninstall/431960".to_string(),
            }
        );
        assert_eq!(found(&app("Wallpaper Engine", "steam://rungameid/1")), None);
    }

    #[test]
    fn a_name_alone_must_be_the_titles_maybe_with_a_version_and_only_one() {
        let named = |names: &[&str], title: &str| {
            let entries: Vec<Entry> = names
                .iter()
                .map(|name| entry(name, name, "u.exe"))
                .collect();
            let app = app(title, r"D:\Apps\x\x.exe");
            find(&app, &entries, &folders()).map(|found| found.name().to_string())
        };
        for name in [
            "Audacity",
            "AUDACITY",
            "Audacity 3.7.8",
            "Audacity v3.7",
            "Audacity (x64)",
        ] {
            assert_eq!(named(&[name], "Audacity"), Some(name.to_string()), "{name}");
        }
        for name in [
            "Audacity Portable",
            "Audacityx",
            "Audacity v",
            "Audacious 3",
        ] {
            assert_eq!(named(&[name], "Audacity"), None, "{name}");
        }
        assert_eq!(named(&["Bun", "Bun 1.2"], "Bun"), None, "two");
        assert_eq!(named(&["Bun"], ""), None, "no title");
        assert_eq!(named(&[""], ""), None);
    }

    #[test]
    fn an_app_the_shell_knows_no_program_of_goes_by_its_name() {
        let everything = app("Everything", "");
        let everything = found(&everything).expect("by its name");
        assert_eq!(everything.name(), "Everything 1.4.1.1032 (x64)");
    }

    #[test]
    fn windows_own_apps_links_and_shell_folders_have_none() {
        let everything = entry("Everything", "Everything", "u.exe");
        let named = |target: &str| {
            let app = app("Everything", target);
            find(&app, std::slice::from_ref(&everything), &folders())
        };
        assert_eq!(
            named(r"C:\Windows\System32\everything.exe"),
            None,
            "Windows'"
        );
        assert_eq!(named("::{52205FD8-5DFB-447D-801A-D0B52F2E83E1}"), None);
        assert_eq!(named("https://www.voidtools.com/"), None);
        assert!(named(r"D:\everything.exe").is_some(), "elsewhere");
    }

    #[test]
    fn what_programs_and_features_does_not_list_uninstalls_nothing() {
        let python = app(
            "Python 3.12.10 Core Interpreter (64-bit)",
            r"C:\Python\python.exe",
        );
        assert_eq!(found(&python), None, "a system component");
        let unlisted = [
            Entry {
                no_remove: true,
                ..entry("a", "Tool", "u.exe")
            },
            Entry {
                parent: "Tool".to_string(),
                ..entry("b", "Tool", "u.exe")
            },
            entry("c", "Tool", " "),
            entry("d", " ", "u.exe"),
        ];
        let tool = app("Tool", r"D:\Tool\tool.exe");
        for each in unlisted {
            // In the app's folder, so listed it would be found.
            let each = Entry {
                location: r"D:\Tool".to_string(),
                ..each
            };
            let found = find(&tool, std::slice::from_ref(&each), &folders());
            assert_eq!(found, None, "{each:?}");
        }
        let listed = Entry {
            location: r"D:\Tool".to_string(),
            ..entry("e", "Tool kit", "u.exe")
        };
        assert!(
            find(&tool, &[listed], &folders()).is_some(),
            "by its folder"
        );
    }

    #[test]
    fn an_msi_without_a_product_code_runs_its_command() {
        let named = Entry {
            windows_installer: true,
            ..entry("Tool", "Tool", "MsiExec.exe /X{tool}")
        };
        let found = find(&app("Tool", r"D:\Tool\tool.exe"), &[named], &folders());
        assert_eq!(found, program("Tool", "MsiExec.exe", "/X{tool}"));
        let bare = Entry {
            windows_installer: true,
            ..entry("{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}", "Tool", "")
        };
        let found = find(&app("Tool", r"D:\Tool\tool.exe"), &[bare], &folders());
        let removed = "/x {B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}";
        assert_eq!(found, program("Tool", "msiexec.exe", removed), "no command");
    }

    /// A packaged app installed in `path`.
    fn packaged(title: &str, path: &str) -> App {
        App {
            title: title.to_string(),
            parsing_name: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".to_string(),
            package: "Microsoft.WindowsCalculator_11.2607.0.0_x64__8wekyb3d8bbwe".to_string(),
            package_path: path.to_string(),
            ..App::default()
        }
    }

    #[test]
    fn a_packaged_app_has_its_package_removed_unless_it_is_part_of_windows() {
        let calculator = packaged(
            "Calculator",
            r"C:\Program Files\WindowsApps\Microsoft.WindowsCalculator_11.2607.0.0_x64__8wekyb3d8bbwe",
        );
        assert_eq!(
            found(&calculator),
            Some(Uninstall::Package {
                name: "Calculator".to_string(),
                full_name: "Microsoft.WindowsCalculator_11.2607.0.0_x64__8wekyb3d8bbwe".to_string(),
            })
        );
        let settings = packaged("Settings", r"C:\Windows\ImmersiveControlPanel");
        assert_eq!(found(&settings), None);
        let start = packaged("Get Started", r"C:\WINDOWS\SystemApps\Client.CBS");
        assert_eq!(found(&start), None);
        assert_eq!(found(&packaged("Nowhere", "")), None, "no place known");
    }

    #[test]
    fn a_packaged_app_is_never_matched_with_an_entry() {
        let mut calculator = packaged("Everything", r"C:\Windows\SystemApps\x");
        calculator.target = r"C:\Program Files\Everything\Everything.exe".to_string();
        assert_eq!(found(&calculator), None);
    }

    #[test]
    fn without_knowing_windows_folder_nothing_counts_as_windows() {
        let folders = Folders::new("", []);
        let app = packaged("Settings", r"C:\Windows\ImmersiveControlPanel");
        assert!(find(&app, &[], &folders).is_some());
    }

    #[test]
    fn an_icons_file_comes_without_its_index_and_quotes() {
        assert_eq!(icon_file(r"C:\a\app.exe,0"), r"C:\a\app.exe");
        assert_eq!(icon_file(r#""C:\a b\app.exe",-101"#), r"C:\a b\app.exe");
        assert_eq!(icon_file(r#""C:\a b\app.exe""#), r"C:\a b\app.exe");
        assert_eq!(icon_file(r#""C:\a b\app.exe"#), r"C:\a b\app.exe");
        assert_eq!(icon_file(r"C:\a,b\app.ico"), r"C:\a,b\app.ico");
        assert_eq!(icon_file(r"C:\a\app.exe,"), r"C:\a\app.exe,");
        assert_eq!(icon_file(r"C:\a\app.exe,-"), r"C:\a\app.exe,-");
        assert_eq!(icon_file(r" C:\a\app.exe , 2 "), r"C:\a\app.exe ");
    }

    #[test]
    fn a_product_code_is_a_guid_in_braces() {
        assert!(is_guid("{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}"));
        assert!(is_guid("{b5f500bb-4625-445d-a2e7-e7fe5e1e7e85}"));
        for key in [
            "B5F500BB-4625-445D-A2E7-E7FE5E1E7E85",
            "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85",
            "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E8}",
            "{B5F500BB-4625-445D-A2E7E7FE5E1E7E85}",
            "{G5F500BB-4625-445D-A2E7-E7FE5E1E7E85}",
            "{D7D7D7D7-7D7D-7D7D-7D7D-7D7D7D7D7D7D}}_is1",
        ] {
            assert!(!is_guid(key), "{key}");
        }
    }

    #[test]
    fn a_command_splits_into_its_program_and_its_arguments() {
        let split = |command| {
            let (program, arguments) = split(command);
            (program, arguments)
        };
        let both = |program: &str, arguments: &str| (program.to_string(), arguments.to_string());
        assert_eq!(
            split(r#""C:\Program Files\Git\unins000.exe""#),
            both(r"C:\Program Files\Git\unins000.exe", "")
        );
        assert_eq!(
            split(r#"  "C:\a b\Update.exe"   --uninstall  "#),
            both(r"C:\a b\Update.exe", "--uninstall")
        );
        assert_eq!(split(r#""C:\a b\u.exe"#), both(r"C:\a b\u.exe", ""));
        assert_eq!(
            split(r"C:\Program Files\Some App\Uninstall.EXE /S"),
            both(r"C:\Program Files\Some App\Uninstall.EXE", "/S")
        );
        assert_eq!(
            split(r"C:\app.exe.d\run.exe	/x"),
            both(r"C:\app.exe.d\run.exe", "/x")
        );
        assert_eq!(split(r"C:\Tools\u.exe"), both(r"C:\Tools\u.exe", ""));
        assert_eq!(
            split("MsiExec.exe /X{B5F500BB}"),
            both("MsiExec.exe", "/X{B5F500BB}")
        );
        assert_eq!(
            split("winget uninstall --product-code uv"),
            both("winget", "uninstall --product-code uv")
        );
        assert_eq!(split("uninstall"), both("uninstall", ""));
        assert_eq!(split(""), both("", ""));
    }

    #[test]
    fn paths_compare_in_any_case_with_either_slash_and_quotes_or_not() {
        let path = |text| Path::parse(text).map(|path| path.0);
        assert_eq!(
            path(r#""C:/Riot Games/League of Legends/""#),
            Some(r"c:\riot games\league of legends".to_string())
        );
        assert_eq!(path(r"C:\"), Some("c:".to_string()));
        assert_eq!(path("c:"), Some("c:".to_string()));
        assert_eq!(
            path(r"\\server\share\app"),
            Some(r"\\server\share\app".to_string())
        );
        for not in [
            "",
            "app.exe",
            r"Program Files\app",
            "1:\\x",
            "C:x",
            "steam://rungameid/1",
            "::{x}",
        ] {
            assert_eq!(path(not), None, "{not}");
        }
        let (folder, inside) = (
            Path::parse(r"C:\a").expect("a"),
            Path::parse(r"C:\A\b").expect("b"),
        );
        assert!(inside.within(&folder) && folder.within(&folder));
        assert!(!folder.within(&inside));
        assert!(Path::parse("C:").expect("a drive").is_drive());
        assert!(!folder.is_drive());
    }
}
