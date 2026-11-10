# hyalo

A fast, glassy Spotlight-style launcher for Windows 11. Press a hotkey, type a few letters, hit Enter: apps, files, folders, settings.

*hyalo* comes from Greek ὕαλος, "glass".

> **Status:** early prototype: finds and opens apps, files and folders, answers arithmetic, converts currencies. Nothing to install yet.

## Goals

- The window is on screen one frame after the hotkey. Zero CPU and GPU while hidden.
- Any file on any NTFS drive, instantly: read the MFT and follow the USN journal instead of crawling.
- Ranking that matches intent: apps above files, your folders above caches, results you pick rise.
- Frosted glass UI drawn natively (DWM Acrylic backdrop + Direct2D). No Electron, no WebView, no .NET.
- No in-process plugins. Extensibility through script commands that run out of process.

## Architecture (planned)

| Crate | Role |
|---|---|
| `hyalo` | Launcher: window, hotkey, tray, rendering, providers (apps, files, exchange rates, settings, windows) |
| `hyalo-core` | Items, matching (incl. wrong keyboard layout), ranking, frecency, location boosts, the calculator and currency answers; later the query language and pipe protocol. Pure and tested |
| `hyalo-index` | The file index: every name once in an arena (and folded in another), 16-byte entries, search split over every core, a snapshot on disk. Pure: a directory walk feeds it now, the indexer service later |
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

- Alt+Space shows or hides the glass search bar; it also hides when it loses focus.
- Typing lists up to eight matching apps (Start Menu desktop apps and Store apps, from `shell:AppsFolder`) with their icons, the matched letters highlighted. `ё` and `е` count as the same letter.
- Arithmetic gets its answer as the first row: `+ - * /`, `^`, parentheses, `sqrt`, `round`, `abs`, decimals with a point or a comma (`2,5*3` answers `7,5`). `%` is per cent (`200+10%` is 220) unless an operand follows it (`7%3` is 1). Enter copies the answer.
- Currencies convert the same way, at the Bank of Russia's official rates: `100 usd` answers in roubles (`8 275.36 ₽`), `400 rub` in dollars, and `in` or `в` names another target (`10 usd in eur`, `10$ в евро`). The amount and the currency go either way round, with or without a space, in any case: any code the bank publishes, a sign (`$ € ₽ £ ₸ ₺ ₴ ₹`; not `¥`, which is the yen and the yuan alike) or a word for the rouble, dollar, euro or yuan (`руб`, `долларов`, `евро`, `юаней`). A code or word typed in the wrong layout works too (`100 гыв`), and the amount may be arithmetic (`(100+50) usd`). Results have two decimals, more when small (`0.0121 $`); the second line names the rates' date. Enter copies the result.
- The rates come from cbr.ru (one small request, user agent `hyalo`, through the system's proxy settings) and are kept in `%LOCALAPPDATA%\hyalo\rates.xml`, so a start without a network converts with the last rates. A start, and showing the launcher at most hourly, asks the bank again when the rates are older than today, once a day when it has nothing newer (weekends, holidays).
- Files and folders from every fixed drive are listed too, with their shell icons and their folder as the second line. Apps rank above files that match as well; files under Desktop, Documents, Downloads and the project folders in your profile (`code`, `projects`, `source`, `src`, `dev`, `repos`, `git`) rank a little higher, files under `node_modules`, `AppData`, caches, `Windows` and folders whose names start with a dot (`.git`, `.vscode`) lower.
- The files come from a walk of the fixed drives with your own rights (no admin). The first walk fills the list as it goes; the index is saved to `%LOCALAPPDATA%\hyalo\files.idx`, so the next start finds files at once, and showing the launcher walks again after 20 minutes. Junctions and symbolic links are listed but never followed; `$Recycle.Bin`, `System Volume Information` and the other `$` folders at a drive's root are left out.
- Up and Down move the selection, or point at a row; Enter or a click opens it, Ctrl+Shift+Enter opens it as administrator, Ctrl+Enter shows a file or folder in its folder, Ctrl+C copies its full path, Esc hides the launcher.
- Picks are remembered per query (frecency) in `%APPDATA%\hyalo\frecency.json`, so what you open often rises, files included.

There is no tray icon yet, so stop it with `taskkill /IM hyalo.exe`.

## Development

Changes follow [ub-mode](https://github.com/unclebob/ub-mode): test first, smallest edit, pure logic behind thin platform glue, small commits. Quality gates use Uncle Bob's [crapper](https://github.com/unclebob/crapper), [dryer](https://github.com/unclebob/dryer) and [mutator](https://github.com/unclebob/mutator):

```sh
scripts/gates.sh          # CRAP < 10 and duplication < 0.8
scripts/gates.sh mutate   # mutation testing
cargo bench -p hyalo-index                            # search a million made-up names
HYALO_BENCH_NAMES=3000000 cargo bench -p hyalo-index  # or as many as you like
```

The script expects the tools checked out side by side under `~/code/unclebob` (override with `UNCLEBOB_TOOLS`), each with its own `.venv`. Stock mutator does not run on Windows yet: it builds its workers from symlinks, which Windows refuses without Developer Mode, and stops timed-out tests with a POSIX-only call. The fixes (copies where links are refused, a warm-up build per worker, process-tree kill) are on the `windows-support` branch of the local mutator clone and not upstream yet.

To check the look, run the launcher and capture it over bright colour stripes:

```sh
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes                   # writes target/snap.png
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes -Type "no{DOWN}"   # types first, shows the list
```

Rendering tests compare frames, pixel by pixel, with reference images in `crates/hyalo/reference`. A failing test writes what it saw to `target/frames`. After a deliberate change to the look, check the new frames by eye, then accept them:

```sh
HYALO_BLESS=1 cargo test -p hyalo
```

## Prior art

[Heimdall](https://github.com/Kryzern/Heimdall) (the inspiration), [Everything](https://www.voidtools.com/), [Flow Launcher](https://www.flowlauncher.com/), Raycast, macOS Spotlight.

## License

MIT
