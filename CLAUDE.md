# hyalo: notes for Claude

Spotlight-style launcher for Windows 11, written in Rust. Goals, architecture and roadmap are in `README.md`.

## Working rules

- Follow `/ub-mode` (installed globally) for every code change: failing test first, smallest edit, pure logic, small ordered commits that say why, what changed, and how it was proved.
- Platform glue (Win32, DirectX, COM) stays thin. Anything with a decision in it (layout math, text editing, query parsing, scoring) lives in a pure function with tests. Do not write fake tests for FFI glue.
- Test the glue the way Windows drives it: `test_desktop::on_hidden_desktop` runs a test on a thread attached to a hidden desktop (no flashing windows, no stolen focus); the `Launcher` driver in `window.rs` tests sends real messages and takes stand-in `Services` (provider, runner, picks file); `Renderer::read_frame` reads back the last presented frame so tests can check pixels. Never repaint before reading a frame: the test must see what the code put on screen.
- Drawing is pinned by reference images (`crates/hyalo/reference`, compared by `reference::assert_looks_like`). Change the look on purpose, look at the new frames, then accept them with `HYALO_BLESS=1 cargo test -p hyalo`. Never bless a frame you have not looked at.
- Before a commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `scripts/gates.sh`.
- Mutation testing on changed code: `scripts/gates.sh mutate --changed`. Goal: no survivors, no uncovered sites. Stock mutator fails on Windows; `~/code/unclebob/mutator` is on the `windows-support` branch, which fixes it. Do not run heavy builds while mutator runs: a mutant that times out counts as killed.
- Gate thresholds follow ub-mode: every function under CRAP 10, no duplicate candidates at similarity 0.8 or above.
- Check UI changes with your own eyes: run `target/debug/hyalo.exe`, then `powershell -ExecutionPolicy Bypass -File scripts/snap.ps1 -Stripes` (add `-Type "no{DOWN}"` to see the list) and read `target/snap.png`. On a dark desktop the glass looks solid; the stripes show whether it really blurs.
- Cargo is in `~/.cargo/bin`; a fresh shell may need `export PATH="$HOME/.cargo/bin:$PATH"`.

## Windows specifics

- Target Windows 11 (22H2+) for the DWM system backdrop. Older systems get a solid background.
- The launcher process must never run elevated: launched apps would inherit elevation and drag-and-drop to normal windows breaks.
- Use the `windows` crate for Win32 and COM. Keep each `unsafe` block small, with a comment stating the invariant it relies on.

## Glossary

- **Launcher**: the UI process, `hyalo.exe`.
- **Indexer**: the Windows service that holds the file index.
- **Provider**: a source of results (apps, files, calculator, settings, windows). Runs off the UI thread and hands what it finds to the window piece by piece.
- **Catalog**: what the launcher can search right now: the items providers found, and their icons.
- **Item**: one row in the list (a "result" in the UI): id, title, subtitle, kind, the action Enter runs. Named `Item` so it does not clash with `std::result::Result`.
- **Pick**: choosing an item with Enter. Frecency counts picks per item and query.
- **Frecency**: a score from how often and how recently an item was picked for a query; a pick also counts, scaled down, while its query is still being typed. Halves every two weeks.
- **Match strength**: how a query fits a name, strongest first: exact, prefix, word start, acronym (starts of words, in order: `vsc`, `vscode`), substring.
- **Wrong layout**: a query typed with the other keyboard layout active (`мысщву` for `vscode`). It still matches, a little weaker.
- **Backdrop**: the blurred glass behind the window, drawn by DWM.
- **Reference image**: a PNG of a frame someone checked by eye; rendering tests compare fresh frames with it.
