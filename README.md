# hyalo

A fast, glassy Spotlight-style launcher for Windows 11. Press a hotkey, type a few letters, hit Enter: apps, files, folders, settings.

*hyalo* comes from Greek ὕαλος, "glass".

> **Status:** early prototype: finds and opens apps, files and folders, Windows' own settings pages and Control Panel items, answers arithmetic, converts currencies, locks, restarts or shuts down the PC (asking first); lives in the tray, with a TOML config applied as you save it and a settings window that edits it; speaks English or Russian; updates itself from GitHub Releases. Installing it the first time is still a copy of `hyalo.exe` into `%LOCALAPPDATA%\Programs\hyalo` by hand.

## Goals

- The window is on screen one frame after the hotkey. Zero CPU and GPU while hidden.
- Any file on any NTFS drive, instantly: read the MFT and follow the USN journal instead of crawling.
- Ranking that matches intent: apps above files, your folders above caches, results you pick rise.
- Frosted glass UI drawn natively (DWM Acrylic backdrop + Direct2D). No Electron, no WebView, no .NET.
- No in-process plugins. Extensibility through script commands that run out of process.

## Architecture (planned)

| Crate | Role |
|---|---|
| `hyalo` | Launcher: window, hotkey, tray, rendering, providers (apps, files, exchange rates, settings, windows), updating itself |
| `hyalo-core` | Items, matching (incl. wrong keyboard layout), ranking, frecency, location boosts, the calculator and currency answers, the table of Windows' settings and the system commands, the config and hotkey names, releases (versions, GitHub's answer, SHA-256 sums, when to ask again); later the query language and pipe protocol. Pure and tested |
| `hyalo-index` | The file index: every name once in an arena (and folded in another), 16-byte entries, search split over every core, a snapshot on disk. Pure: a directory walk and the drives' change notifications feed it now, the indexer service later |
| `hyalo-indexer` | Windows service: reads the MFT, follows the USN journal, answers queries over a named pipe restricted to the user |

The launcher never runs elevated. Only the indexer service does, because MFT access needs admin rights. Without the service the launcher falls back to walking directories (what it does today).

## Roadmap

**v0.1: daily driver**
- Global hotkey (Alt+Space by default, configurable)
- Apps: Start Menu shortcuts and Store/UWP apps, with icons
- Files and folders from every NTFS drive, live updates
- Ranking: match quality (prefix, word start, acronym, substring), type, location boosts and penalties, frecency
- Actions: open, open containing folder, run as admin, copy path
- Inline calculator
- Currency conversion as a calculator answer: any currency but the rouble converts to roubles (`100 usd`, `50 eur`), roubles convert to dollars (`400 rub`), and `in` or `в` names the target (`10 usd in eur`). Codes, symbols and words all work: `$100`, `100$`, `€`, `₽`, `руб`, `долларов`. Rates are the Bank of Russia's official daily rates (free, no key), cached on disk and refreshed daily, so it works offline with the last known rates and shows their date. Enter copies the result
- TOML config with hot reload, tray icon, autostart
- English and Russian, chosen in the settings; at first the language Windows is shown in
- Settings window in the same glass, for those who would rather not edit the file: every change is written to `config.toml` at once, keeping your comments and layout, and edits made to the file show up in the open window
- Installs for the current user without admin rights and updates itself from GitHub Releases: a quiet "new version" notice in the launcher and the tray menu, with the release notes; one click downloads the new build, checks it, swaps it in and restarts hyalo

**v0.2**
- Windows Settings pages (`ms-settings:`) and Control Panel items
- Web search prefixes (`g`, `gh`, `yt`, custom)
- Window switcher (`>`)
- System commands (lock, sleep, restart) with confirmation
- Filters: `ext:`, `in:`, `size:`, `mod:`

**Later**
- Preview pane
- On-demand content search (ripgrep in a chosen folder)
- Script commands

## Build

Requires the Rust MSVC toolchain and Visual Studio Build Tools with the C++ workload.

```sh
cargo run -p hyalo
```

What the prototype does:

- Alt+Space (or the hotkey the config names) shows or hides the glass search bar; it also hides when it loses focus, and on Esc or Alt+F4, which hide it and never quit hyalo (Exit in the tray menu does). Starting hyalo again shows the one that runs: there is only ever one.
- The search bar edits like any Windows text field: Left and Right, Ctrl+Left and Ctrl+Right by word, Home and End, with Shift to select; Backspace and Delete, with Ctrl a word; Ctrl+A selects all; typing or pasting replaces the selection. Ctrl+C copies the text selected in the bar (with nothing selected, the selected row's path, as below), Ctrl+X cuts, Ctrl+V pastes one line (line breaks become spaces); Ctrl+Insert, Shift+Delete and Shift+Insert do the same. A click places the caret, dragging selects, Shift+click selects up to there, a double-click selects a word and a third click everything. A query longer than the bar scrolls with its caret. The settings window's name fields edit the same way.
- Drag the launcher by its glass (the search icon, the room right of the query, the edges, the padding round the list) to put it somewhere else on the screen; it stops at the edges, so its longest list always fits. It shows there from then on, after a restart too, on whichever monitor it opens on (`%LOCALAPPDATA%\hyalo\position.txt` keeps the place, not the config). A double-click on the glass puts it back in the middle.
- hyalo sits in the notification area with a glass lens for an icon (new icons start in the overflow behind `^`; drag it onto the taskbar to keep it in sight). A click on it shows or hides the launcher; a right click opens a menu: Show hyalo (with its hotkey), Install hyalo 0.0.2 (when a newer release is out), Settings (opens the settings window), Start with Windows, Exit. Exit is how to quit: it takes the icon away and saves the picks.
- hyalo speaks English or Russian: the settings window, the tray menu and its tooltip, the notices, the rows' second lines (Calculator, the bank's rates, App) and the search bar's placeholder. The config's `language` chooses; without one hyalo speaks the language Windows is shown in, and writes that one in a config it creates. Changing it, in the file or in the settings window, applies at once. Names stay as they are (keys, files, apps, the config's settings), and so do the TOML reader's own messages about a broken file.
- Start with Windows writes the `hyalo` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, naming this `hyalo.exe` with `--hidden`, so it starts in the tray without showing the launcher. Windows' own switch (Settings > Apps > Startup) is respected: switched off there, the menu shows it unchecked; checking it in the menu switches it back on. It is off until you turn it on, and a debug build never offers it.
- The settings live in `%APPDATA%\hyalo\config.toml`, written with every setting at its default and a comment on each the first time hyalo starts. Saved changes apply at once, no restart. A mistake (a typo in a name, a key hyalo does not know, a path in double quotes with its backslashes read as escapes) keeps the settings in force and is shown, with its line, under the empty search bar and in the tray icon's tooltip; Enter on that row opens the settings window, which shows the problem. A hotkey Windows refuses, because another app holds it, is shown the same way, and the one before keeps working. The settings:

  | Setting | Default | What it does |
  |---|---|---|
  | `hotkey` | `"Alt+Space"` | Shows and hides the launcher: Ctrl, Alt, Shift or Win and one key, joined by `+` (`Ctrl+Shift+K`, `Win+F1`). A letter, digit or other key you type needs Ctrl, Alt or Win; F1 to F24 do without |
  | `language` | Windows' own (`"en"` or `"ru"`) | What hyalo says things in: `"en"` for English, `"ru"` for Russian. Left out, the language Windows is shown in |
  | `check_for_updates` | `true` | Looks for a newer release on GitHub once a day and tells of it; `false` stops looking. Nothing is installed until you click Install |
  | `[folders] exclude` | `['C:\Windows\WinSxS']` | Full paths left out of the file index with everything in them, also when made later; one added is taken out of the index at once, one removed is walked and comes back at once |
  | `[folders] rank_lower` | `['target']` | Folders whose files rank lower, besides the built-in ones: a name, wherever such a folder is, or a full path |
  | `[folders] rank_higher` | `[]` | Folders whose files rank higher, besides Desktop, Documents, Downloads and the project folders: a name or a full path |

  Why these defaults, from the index of a real machine (1 963 824 names): `WinSxS` held 267 639 of them, a seventh, every copy of every system file an update brought; `notepad.exe` listed nine copies that scored the same as `C:\Windows\notepad.exe`. Cargo's and Maven's build output lives in `target` folders; hyalo's own `target\llvm-cov-target\...\incremental` folders scored like real ones for `hyalo`. Write paths in single quotes, so that backslashes stay as they are.
- The settings window (Settings in the tray menu, Enter on a notice, or `hyalo.exe --settings`, which opens it in the hyalo that runs) shows every setting of `config.toml` and changes it, dark over Mica: the hotkey (click the field and press the keys; a bare F1 to F24 will do, Esc keeps the one you have, Alt+F4 still closes the window, and one Windows refuses is said so in place and never written), Start with Windows (a switch; a debug build has none), the language (English | Русский; the window and its title switch at once), and the three lists of folders, each entry with a × to take it out, Add folder… for the system's folder picker, and for the two lists that rank, a field for a bare folder name such as `target`, and Updates: the version you have and the switch for checking, and, when a newer release is out, first on the page, its version, its notes and Install (a debug build has no Updates). Tab and Shift+Tab move a visible ring from control to control, Space and Enter use it, Esc closes the window, the wheel scrolls it when it is too short. There is only ever one: opening it again brings it forward.
- There is no Save button: each change is written to `config.toml` at once, into the text on disk as it is at that moment, and only the value that changed: your comments, the order of the settings and the way you lay out the lists stay as they are (an entry is written in single quotes, on a line of its own in a list written one entry per line). The file is replaced whole, through a temporary file beside it, so hyalo never reads half of one. The file stays the one place the settings live: what you save in a text editor shows up in the open window, and what you are typing in one of its fields stays. While the file does not read (half-edited in Notepad, say), the window says what is wrong and where, changes nothing, and offers Open config.toml.
- hyalo updates itself from this repository's GitHub Releases. It asks GitHub for the latest release (`api.github.com/repos/IlyaShorin/hyalo/releases/latest`, one small request, user agent `hyalo/<version>`, nothing of yours) when it starts and when the launcher shows, at most once a day; a try that fails (offline, GitHub down) is silent and tried again an hour later. A release newer than the one running is a quiet notice under the empty search bar and in the tray icon's tooltip, and an Install line in the tray menu; Enter on the notice opens the settings window with the release first: the version you have, its notes, Install. Install downloads the release's `hyalo.exe` and `hyalo.exe.sha256`, refuses an exe that does not match its SHA-256 (it says so, and the version you have keeps running), puts the new exe in place of the running one, which stays beside it as `hyalo.exe.old` (in place of any `hyalo.exe.old` there was), starts it in the tray and quits. The new one waits for the old one to end, takes over (the same path, so starting with Windows still starts it) and removes `hyalo.exe.old`; one that cannot open its window puts the old one back and starts it. All of it with your own rights, in the folder hyalo runs from. A debug build never looks.
- Typing lists up to eight matching apps (Start Menu desktop apps and Store apps, from `shell:AppsFolder`) with their icons, the matched letters highlighted. `ё` and `е` count as the same letter.
- Windows' own settings are listed too: 65 pages of the Settings app (`ms-settings:`) and 23 classic Control Panel items and dialogs (Device Manager, Environment Variables, Programs and Features, Power Options, Sound, Network Connections, System Properties, Credential Manager, Fonts, Mouse, Region, User Accounts, File Explorer Options, Disk Management…), each with its icon and a second line saying where it lives (Settings / Параметры Windows, Control Panel / Панель управления); Enter opens it. Each goes by its name in English and in Russian, whatever language hyalo speaks, and by the words people use for it: `bluetooth`, `display`, `звук`, `обновлени`, `default apps`, `wifi`, `яркость`, `uninstall`, `переменные среды`, `path`, `device manager`. The row shows the name in hyalo's language. They rank like apps, so a page does not drown under files, and rise with picks like anything else. They come from a table in hyalo rather than from the shell's Control Panel folder, which names its items only in the language Windows is shown in and lacks several of the most wanted; the icons do come from the shell.
- System commands: Lock, Sleep, Hibernate (only when Windows has hibernation turned on), Restart, Shut down, Sign out and Empty Recycle Bin, named in English or Russian and found by either (`lock`, `блок`, `sleep`, `перезагр`, `reboot`, `выкл`, `sign out`, `корзина`), each with its Windows 11 symbol. Lock runs at Enter. The others ask first: Enter turns the row's second line into a question in amber, "Press Enter again to restart" / "Нажмите Enter ещё раз, чтобы перезагрузить компьютер", and only the next Enter, or a second click on the row, does it. Anything else calls it off: any other key, typing, a click elsewhere, pointing at another row, hiding the launcher. Emptying the bin does not ask a second time: the row already did.
- Arithmetic gets its answer as the first row: `+ - * /`, `^`, parentheses, `sqrt`, `round`, `abs`, decimals with a point or a comma (`2,5*3` answers `7,5`). `%` is per cent (`200+10%` is 220) unless an operand follows it (`7%3` is 1). Enter copies the answer.
- Currencies convert the same way, at the Bank of Russia's official rates: `100 usd` answers in roubles (`8 275.36 ₽`), `400 rub` in dollars, and `in` or `в` names another target (`10 usd in eur`, `10$ в евро`). The amount and the currency go either way round, with or without a space, in any case: any code the bank publishes, a sign (`$ € ₽ £ ₸ ₺ ₴ ₹`; not `¥`, which is the yen and the yuan alike) or a word for the rouble, dollar, euro or yuan (`руб`, `долларов`, `евро`, `юаней`). A code or word typed in the wrong layout works too (`100 гыв`), and the amount may be arithmetic (`(100+50) usd`). Results have two decimals, more when small (`0.0121 $`); the second line names the rates' date. Enter copies the result.
- The rates come from cbr.ru (one small request, user agent `hyalo/<version>`, through the system's proxy settings) and are kept in `%LOCALAPPDATA%\hyalo\rates.xml`, so a start without a network converts with the last rates. A start, and showing the launcher at most hourly, asks the bank again when the rates are older than today, once a day when it has nothing newer (weekends, holidays).
- Files and folders from every fixed drive are listed too, with their shell icons and their folder as the second line. Apps rank above files that match as well; files under Desktop, Documents, Downloads and the project folders in your profile (`code`, `projects`, `source`, `src`, `dev`, `repos`, `git`) rank a little higher, files under `node_modules`, `AppData`, caches, `Windows` and folders whose names start with a dot (`.git`, `.vscode`) lower.
- The files come from a walk of the fixed drives with your own rights (no admin), and stay current: each drive is watched from before the walk with its own change notifications (`ReadDirectoryChangesExW` over the whole drive: names, with their attributes), so a file or folder made, renamed, moved or deleted shows up, or goes, within a second. A folder moved in whole is walked; one renamed takes its files along. A watch that falls behind (a burst of changes too big for its megabyte of room) or loses its drive says so, and the drives are walked again. The first walk fills the list as it goes; the index is saved to `%LOCALAPPDATA%\hyalo\files.idx` after each walk, at most every half hour while files change, and when hyalo stops searching it, so the next start finds files at once, then walks again for what changed while hyalo was not running. Junctions and symbolic links are listed but never followed; `$Recycle.Bin`, `System Volume Information` and the other `$` folders at a drive's root are left out, and so is what the config excludes.
- Up and Down move the selection, or point at a row; Enter or a click opens it, Ctrl+Shift+Enter opens it as administrator, Ctrl+Enter shows a file or folder in its folder, Ctrl+C with no text selected in the bar copies its full path, Esc hides the launcher.
- Picks are remembered per query (frecency) in `%APPDATA%\hyalo\frecency.json`, so what you open often rises, files included.

## Releases

A pushed tag `v*` runs `.github/workflows/release.yml`: it fails unless the tag names the version in the workspace `Cargo.toml` (the version the exe knows it is), builds the release `hyalo.exe` on Windows, writes its SHA-256 beside it in `hyalo.exe.sha256` (as `sha256sum` writes it), and publishes a GitHub Release with both, its notes taken from the tag's message. Every hyalo older than it offers it from then on.

```sh
# set version = "0.0.2" in Cargo.toml, run the gates, commit, push, then:
git tag -a v0.0.2 -m "What is new" -m "* one line per change"
git push origin v0.0.2
```

Or from this machine, with the same checks and the same two assets: `scripts/release.ps1` builds the commit checked out (committed and pushed), writes `target\release-dist\hyalo.exe` and `hyalo.exe.sha256`, and creates the release with `gh`, which puts the tag on that commit; the workflow that tag starts finds the release out and publishes nothing. `-DryRun` does everything but talk to GitHub.

```sh
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Tag v0.0.2 -NotesFile notes.md -DryRun
powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Tag v0.0.2 -NotesFile notes.md
```

## Development

Changes follow [ub-mode](https://github.com/unclebob/ub-mode): test first, smallest edit, pure logic behind thin platform glue, small commits. Quality gates use Uncle Bob's [crapper](https://github.com/unclebob/crapper), [dryer](https://github.com/unclebob/dryer) and [mutator](https://github.com/unclebob/mutator):

```sh
scripts/gates.sh          # CRAP < 10 and duplication < 0.8
scripts/gates.sh mutate   # mutation testing
cargo bench -p hyalo-index                            # search a million made-up names
HYALO_BENCH_NAMES=3000000 cargo bench -p hyalo-index  # or as many as you like
```

The script expects the tools checked out side by side under `~/code/unclebob` (override with `UNCLEBOB_TOOLS`), each with its own `.venv`. Stock mutator does not run on Windows yet: it builds its workers from symlinks, which Windows refuses without Developer Mode, and stops timed-out tests with a POSIX-only call. The fixes (copies where links are refused, a warm-up build per worker, process-tree kill) are on the `windows-support` branch of the local mutator clone and not upstream yet.

The icon is drawn by a script, each size on its own; it writes `crates/hyalo/assets/hyalo.ico` and a preview of every size on dark and light ground in `target/icon-preview.png`. `build.rs` puts the icon into the executable as a resource, with no resource compiler:

```sh
powershell -ExecutionPolicy Bypass -File scripts/icon.ps1
```

To check the look, run the launcher and capture it over bright colour stripes; the script shows it by starting `hyalo.exe` again, so it works whatever the hotkey:

```sh
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes                   # writes target/snap.png
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes -Type "no{DOWN}"   # types first, shows the list
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Settings                  # opens and captures the settings window
```

Rendering tests compare frames, pixel by pixel, with reference images in `crates/hyalo/reference`. A failing test writes what it saw to `target/frames`. The images keep the frames' premultiplied colours, which a viewer shows too dark; `scripts/frames.ps1 -Files a.png,b.png` writes them over a dark backdrop to `target/look` for looking. After a deliberate change to the look, check the new frames by eye, then accept them:

```sh
HYALO_BLESS=1 cargo test -p hyalo
```

## Prior art

[Heimdall](https://github.com/Kryzern/Heimdall) (the inspiration), [Everything](https://www.voidtools.com/), [Flow Launcher](https://www.flowlauncher.com/), Raycast, macOS Spotlight.

## License

MIT
