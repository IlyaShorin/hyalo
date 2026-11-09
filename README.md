# hyalo

A fast, glassy Spotlight-style launcher for Windows 11. Press a hotkey, type a few letters, hit Enter: apps, files, folders, settings.

*hyalo* comes from Greek ὕαλος, "glass".

> **Status:** early prototype. Nothing to install yet.

## Goals

- The window is on screen one frame after the hotkey. Zero CPU and GPU while hidden.
- Any file on any NTFS drive, instantly: read the MFT and follow the USN journal instead of crawling.
- Ranking that matches intent: apps above files, your folders above caches, results you pick rise.
- Frosted glass UI drawn natively (DWM Acrylic backdrop + Direct2D). No Electron, no WebView, no .NET.
- No in-process plugins. Extensibility through script commands that run out of process.

## Architecture (planned)

| Crate | Role |
|---|---|
| `hyalo` | Launcher: window, hotkey, tray, rendering, providers (apps, calculator, settings, windows) |
| `hyalo-core` | Index, query language, scoring, pipe protocol. Pure and tested |
| `hyalo-indexer` | Windows service: reads the MFT, follows the USN journal, answers queries over a named pipe restricted to the user |

The launcher never runs elevated. Only the indexer service does, because MFT access needs admin rights. Without the service the launcher falls back to walking directories.

## Roadmap

**v0.1: daily driver**
- Global hotkey (Alt+Space by default, configurable)
- Apps: Start Menu shortcuts and Store/UWP apps, with icons
- Files and folders from every NTFS drive, live updates
- Ranking: match quality (prefix, word start, acronym, substring), type, location boosts and penalties, frecency
- Actions: open, open containing folder, run as admin, copy path
- Inline calculator
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

The prototype is only the glass search bar: Alt+Space toggles it, Esc or Enter hides it, it hides when it loses focus. There is no tray icon yet, so stop it with `taskkill /IM hyalo.exe`.

## Development

Changes follow [ub-mode](https://github.com/unclebob/ub-mode): test first, smallest edit, pure logic behind thin platform glue, small commits. Quality gates use Uncle Bob's [crapper](https://github.com/unclebob/crapper), [dryer](https://github.com/unclebob/dryer) and [mutator](https://github.com/unclebob/mutator):

```sh
scripts/gates.sh          # CRAP < 10 and duplication < 0.8
scripts/gates.sh mutate   # mutation testing
```

The script expects the tools checked out side by side under `~/code/unclebob` (override with `UNCLEBOB_TOOLS`), each with its own `.venv`. Stock mutator does not run on Windows yet: it builds its workers from symlinks, which Windows refuses without Developer Mode, and stops timed-out tests with a POSIX-only call. The fixes (copies where links are refused, a warm-up build per worker, process-tree kill) are on the `windows-support` branch of the local mutator clone and not upstream yet.

To check the look, run the launcher and capture it over bright colour stripes:

```sh
powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes   # writes target/snap.png
```

## Prior art

[Heimdall](https://github.com/Kryzern/Heimdall) (the inspiration), [Everything](https://www.voidtools.com/), [Flow Launcher](https://www.flowlauncher.com/), Raycast, macOS Spotlight.

## License

MIT
