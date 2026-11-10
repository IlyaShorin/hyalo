# hyalo: notes for Claude

Spotlight-style launcher for Windows 11, written in Rust. Goals, architecture and roadmap are in `README.md`.

## Working rules

- Follow `/ub-mode` (installed globally) for every code change: failing test first, smallest edit, pure logic, small ordered commits that say why, what changed, and how it was proved.
- Platform glue (Win32, DirectX, COM) stays thin. Anything with a decision in it (layout math, text editing, query parsing, scoring) lives in a pure function with tests. Do not write fake tests for FFI glue.
- Test the glue the way Windows drives it: `test_desktop::on_hidden_desktop` runs a test on a thread attached to a hidden desktop (no flashing windows, no stolen focus); the `Launcher` driver in `window.rs` tests sends real messages (keys, characters, mouse) and takes stand-in `Services` (sources, file index, icons, places, runner, picks file, hotkeys, tray, config file, autostart keys); tray events go in as `WM_TRAY` messages; file tests walk real trees made with `scratch::Scratch` (under `%TEMP%`, so inside `AppData`: their files carry the AppData penalty, so compare scores with a neighbour's); `Renderer::read_frame` reads back the last presented frame so tests can check pixels. Never repaint before reading a frame: the test must see what the code put on screen. Window tests get rates from a stand-in source (`rates::tests::fixture()`, the bank's real answer of 10.11.2026); `http::get` is tested against a `TcpListener` server on 127.0.0.1 inside the test, never against cbr.ru.
- A test that touches the clipboard runs through `test_desktop::in_own_window_station`, in a child process with a window station of its own: the hidden desktop shares the user's clipboard, and a test must never overwrite what the user copied.
- The hidden desktop shares more with the user's than the clipboard. Global hotkeys are the whole session's: one held on the user's desktop is refused on the hidden one and the other way round, so window tests register through the `Keyboard` stand-in (`hotkeys::Hotkeys`), and only `hotkeys::System`'s own test registers a real one (`Ctrl+Alt+Shift+Win+F24`, for a moment). The hidden desktop has no taskbar, so `Shell_NotifyIcon` fails there: window tests use the `Taskbar` stand-in (`tray::Tray`), which also picks the menu's entry. Registry tests use keys of their own under `HKCU\Software\hyalo-test-<pid>-<test>` (`autostart::tests::Keys`, deleted on drop), never the real Run key. A launcher given a config path writes the default config there when it is missing: give tests a path inside a `Scratch` folder, never one outside it (a test once left `C:\nowhere\hyalo` behind).
- Drawing is pinned by reference images (`crates/hyalo/reference`, compared by `reference::assert_looks_like`). Change the look on purpose, look at the new frames, then accept them with `HYALO_BLESS=1 cargo test -p hyalo`. Never bless a frame you have not looked at.
- Before a commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `scripts/gates.sh`.
- Mutation testing on changed code: `scripts/gates.sh mutate --changed` (it reads `git status`: once committed, name the files instead). Goal: no survivors, no uncovered sites. Stock mutator fails on Windows; `~/code/unclebob/mutator` is on the `windows-support` branch, which fixes it. Do not run heavy builds while mutator runs: a mutant that times out counts as killed. For the launcher crate pass `--max-workers 8`: 24 workers linking at once have failed with LNK1201.
- Gate thresholds follow ub-mode: every function under CRAP 10, no duplicate candidates at similarity 0.8 or above.
- Check UI changes with your own eyes: run `target/debug/hyalo.exe`, then `powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes` (add `-Type "no{DOWN}"` to see the list) and read `target/snap.png`. On a dark desktop the glass looks solid; the stripes show whether it really blurs. `-Type` uses SendKeys, which go to whatever has the focus (with `-Stripes` that can be the stripes form, so nothing reaches the launcher); when the bar stays empty, send `WM_CHAR` to the `hyalo.launcher` window with `SendMessageW` instead (synchronous, so a slow debug search cannot lag the capture; `PostMessage` or an ANSI `SendMessage` will not do). A PowerShell script with Cyrillic in it needs a UTF-8 BOM. `snap.ps1 -Stripes` and `-Toggle` press a real Alt+Space, which other launchers that hook the keyboard (Flow Launcher) answer too, taking the focus and so hiding hyalo: show it with a second `hyalo.exe` instead (it asks the running one to show), then type with `WM_CHAR`. New tray icons start in the overflow behind `^`. The Store Notepad restores the user's tabs when it starts; leave its windows alone.
- Cargo is in `~/.cargo/bin`; a fresh shell may need `export PATH="$HOME/.cargo/bin:$PATH"`.
- The repository uses LF. Python in text mode on Windows writes CRLF: open files with `newline='\n'`. Shell heredocs here may eat a backslash (`'\\'` came out as `'\'`); edit Rust with backslashes through the editor tool.
- Search speed: `cargo bench -p hyalo-index` (a million synthetic names; `HYALO_BENCH_NAMES` for more). Keep every query under 20 ms in release.

## Windows specifics

- Target Windows 11 (22H2+) for the DWM system backdrop. Older systems get a solid background.
- The launcher process must never run elevated: launched apps would inherit elevation and drag-and-drop to normal windows breaks.
- Use the `windows` crate for Win32 and COM. Keep each `unsafe` block small, with a comment stating the invariant it relies on.

## Glossary

- **Launcher**: the UI process, `hyalo.exe`.
- **Indexer**: the Windows service that will hold the file index (a later step). Until then the launcher holds it.
- **Index**: every file and folder name the launcher can find (`hyalo-index`): names in one arena, folded names in another, one 16-byte **entry** per file or folder (its folder, where its names are, flags). Changed only by add, remove (with everything under it) and rename, so a walk or the NTFS journal can drive it.
- **Root**: an entry with no folder, such as `C:`; holds entries but is never a result.
- **Walk**: the files provider reading the fixed drives folder by folder with the user's rights. Lists junctions and symbolic links, never enters them.
- **Snapshot**: the index on disk (`%LOCALAPPDATA%\hyalo\files.idx`), checked where it is read; the folded names are made again on load.
- **Ceiling**: the most points a name could score, read cheaply from its folded text (rough) and its word starts; the search scores in full only names whose ceiling beats the rows already kept.
- **Places**: where a file lives, in points: favoured folders (Desktop, Documents, Downloads, projects) +3, buried ones (`node_modules`, `AppData`, caches, `Windows`, any name starting with a dot) −10.
- **Provider**: a source of results (apps, files, calculator, settings, windows). Runs off the UI thread and hands what it finds to the window piece by piece.
- **Catalog**: what the launcher can search right now: the items providers found, and their icons.
- **Item**: one row in the list (a "result" in the UI): id, title, subtitle, kind, the action Enter runs. Named `Item` so it does not clash with `std::result::Result`.
- **Pick**: choosing an item with Enter. Frecency counts picks per item and query.
- **Frecency**: a score from how often and how recently an item was picked for a query; a pick also counts, scaled down, while its query is still being typed. Halves every two weeks.
- **Match strength**: how a query fits a name, strongest first: exact, prefix, word start, acronym (starts of words, in order: `vsc`, `vscode`), substring. Letters compare case-insensitively, with `ё` as `е`.
- **Wrong layout**: a query typed with the other keyboard layout active (`мысщву` for `vscode`). It still matches, a little weaker.
- **Answer**: the calculator's row. A query with an operator or a function that reads as one whole expression gets one, first in the list; Enter copies it. A bare number gets none, so it still just searches. A query the calculator does not answer that is an amount and a currency (`100 usd`, `10$ в евро`) gets a conversion at the rates instead.
- **Rates**: the Bank of Russia's official exchange rates of one day: roubles per unit of each currency it publishes, from its daily XML (`XML_daily.asp`). Kept as the bank sent them in `%LOCALAPPDATA%\hyalo\rates.xml`; asked for again when older than today in Moscow, once a day when the bank has nothing newer.
- **Backdrop**: the blurred glass behind the window, drawn by DWM.
- **Reference image**: a PNG of a frame someone checked by eye; rendering tests compare fresh frames with it.
- **Config**: the user's settings in `%APPDATA%\hyalo\config.toml` (`hyalo_core::config` reads the text, `settings` moves it): the hotkey and the folders to exclude and to rank lower or higher. Written with the defaults when missing, watched while the launcher runs; a good save is put in force at once, a broken one keeps the last good settings (at start, the defaults) and becomes a notice.
- **Hotkey**: modifiers and one key that show and hide the launcher, named as the config writes it (`Ctrl+Shift+K`). Registered with Windows, which may refuse one another app holds; a refused hotkey leaves the one in force.
- **Notice**: what hyalo tells the user, quietly: a row under the empty search bar (never while typing) and the tray's tooltip; Enter on it opens the config. A problem in the config, or a hotkey Windows refused.
- **Tray**: hyalo's icon in the notification area and its menu (Show hyalo, Settings, Start with Windows, Exit), built from a list of lines. Added again when Explorer restarts (`TaskbarCreated`).
- **Excluded**: full paths the config leaves out of the index with everything under them. The walk never lists them; each run of the files provider takes them out of the live index before and after its walk.
- **Autostart**: the `hyalo` value in the current user's Run key, naming the exe with `--hidden`; Windows' own switch under `StartupApproved\Run` can turn it off. Off by default; a debug build never offers it.
- **Single instance**: the mutex `Local\hyalo.launcher`; a second start finds it held, asks the running launcher to show (`WM_SHOW`) and ends.
