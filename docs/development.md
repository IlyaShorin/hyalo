# Developing hyalo

How hyalo is built, tested and released, what it aims at, and what may come next. For what it does and how to use it, see the [README](../README.md) and the [guide](guide.md).

## Goals

- The window is on screen one frame after the hotkey. Zero CPU and GPU while hidden.
- Any file on any NTFS drive, instantly: read the MFT and follow the USN journal instead of crawling (the indexer service: 2.7 s for 2.3 million names here).
- Ranking that matches intent: apps above files, your folders above caches, results you pick rise.
- Frosted glass UI drawn natively (DWM Acrylic backdrop + Direct2D). No Electron, no WebView, no .NET.
- No in-process plugins. Extensibility through script commands that run out of process.

## Architecture

| Crate | Role |
|---|---|
| `hyalo` | Launcher: window, hotkey, tray, rendering, providers (apps and their uninstallers, files, exchange rates, settings, windows, what is copied), updating itself |
| `hyalo-core` | Items, matching (incl. wrong keyboard layout), ranking, frecency, location boosts, the calculator and currency answers, web search prefixes, which windows the switcher lists, the table of Windows' settings and the system commands, which uninstaller removes an app, the clipboard history (what it keeps, what an app marks private, how each entry shows as a row, the bitmaps, files and HTML it reads), the config and hotkey names, releases (versions, GitHub's answer, SHA-256 sums, when to ask again); later the query language and pipe protocol. Pure and tested |
| `hyalo-index` | The file index: every name once in an arena (and folded in another), 16-byte entries, search split over every core, a snapshot on disk. Pure: a directory walk and the drives' change notifications feed it, or the indexer service, by record number |
| `hyalo-indexer` | The indexer service, linked into hyalo.exe: reads NTFS master file tables (`mft`, `listing`, pure) and USN journals (`usn`, pure) through a `Disk` (`ntfs`, the real one), sends each volume over a named pipe only the system and its users may open (`server`, `client`, the protocol in `hyalo_core::pipe`), and installs, runs and removes itself through the service manager (`scm`, `manager`, `service`) |

The launcher never runs elevated. Only the indexer service does, because MFT access needs admin rights: it is hyalo.exe copied to Program Files and run by the service manager with `--indexer`, installed by hyalo.exe started through the UAC prompt with `--indexer-install <SID>`. It keeps no index and writes nothing: it sends each volume's files by record number, then what each changed file is now, and the launcher keeps the index. Without the service the launcher walks directories and watches them, as before, root by root.

## Roadmap

**v0.1: daily driver** (done, in 0.2.0)
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

**v0.2** (done, in 0.2.0)
- Windows Settings pages (`ms-settings:`) and Control Panel items
- Web search prefixes (`g`, `gh`, `yt`, `w`, `ya`, your own), all listed in the config
- Window switcher (`>`)
- System commands (lock, sleep, restart) with confirmation
- Filters: `ext:`, `in:`, `size:`, `mod:`

**Since** (done, in 0.2.5)
- Uninstall an app from its row: Shift+Delete, then Enter
- A menu on every row (right click, the Menu key or Shift+F10): what the row can do, each with the keys that do it

**In 0.2.8**
- Clipboard history under the empty search bar: text, pictures and files, kept in memory only; Enter copies an entry again

**Later**
- Preview pane
- On-demand content search (ripgrep in a chosen folder)
- Script commands

## Building

Requires the Rust MSVC toolchain (stable, pinned by `rust-toolchain.toml`) and Visual Studio Build Tools with the C++ workload. `.cargo/config.toml` links the C runtime statically, so the exe needs nothing installed beside it.

```sh
cargo run -p hyalo                 # a debug build: never installs itself, never updates
cargo build --release -p hyalo     # target/release/hyalo.exe
cargo test --workspace
```

Window tests run on a hidden desktop, so they flash nothing on screen and take no focus; tests that touch the clipboard run in a window station of their own, so they never overwrite what you copied. `CLAUDE.md` holds the full set of rules the tests follow.

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
cargo test --release -p hyalo -- --ignored --nocapture service_   # time the installed indexer service
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

## README screenshots

The pictures in `docs/images` are hyalo's own frames, drawn by its renderer with the shell's real icons over made-up rows, then put over a drawn wallpaper with the glass blurred under them, so no file, window or clipboard of the machine shows. After a change to the look:

```sh
cargo test -p hyalo -- --ignored readme_shots                           # frames to target/readme
powershell -STA -ExecutionPolicy Bypass -File scripts/readme-shots.ps1  # pictures to docs/images
```

Look at each picture before committing it. The comparison with Everything is in [benchmarks.md](benchmarks.md), with how it was measured.
