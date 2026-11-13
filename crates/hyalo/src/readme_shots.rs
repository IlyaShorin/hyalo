//! The README's screenshots, step one: frames drawn by the real renderer
//! with the shell's real icons, over made-up rows (no file, window or
//! clipboard of the machine shows). Writes premultiplied RGBA PNGs to
//! `target/readme`; `scripts/readme-shots.ps1` then puts them over a
//! wallpaper into `docs/images`. Not run with the other tests: it needs
//! Notepad, Sticky Notes, OneNote, Cursor, Outlook, Terminal and Edge
//! installed for their icons.
//!
//!   cargo test -p hyalo -- --ignored readme_shots

use crate::apps::{Icon, icon_at};
use crate::com::Com;
use crate::highlight::utf16_ranges;
use crate::render::{Renderer, Row, Scene};
use crate::scratch::Scratch;
use crate::surface::{Behind, Canvas, Frame};
use crate::test_desktop::on_hidden_desktop;
use hyalo_core::item::ItemId;
use hyalo_core::text::Language;

const DPI: u32 = 144;
/// A row's icon at 150 %.
const SIZE: u32 = 48;

fn save(name: &str, frame: &Frame) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/readme");
    std::fs::create_dir_all(&dir).expect("dir");
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).expect("file");
    let mut encoder = png::Encoder::new(file, frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("header");
    let mut rgba = Vec::with_capacity(frame.width * frame.height * 4);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let [b, g, r, a] = frame.pixel(x, y);
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    writer.write_image_data(&rgba).expect("write");
}

fn draw(name: &str, scene: &Scene, rows: u32) {
    let height = if rows == 0 { 64 } else { 64 + rows * 48 + 16 };
    let canvas = Canvas::new();
    let mut renderer =
        Renderer::new(canvas.0, 720 * 3 / 2, height * 3 / 2, DPI, Behind::Glass).expect("r");
    renderer.draw(scene).expect("draw");
    save(name, &renderer.read_frame().expect("frame"));
}

/// The bar alone, with `query` typed and the caret after it.
fn bar(query: &str) -> Scene<'_> {
    let end = query.encode_utf16().count() as u32;
    Scene {
        query,
        placeholder: Language::English.say(hyalo_core::text::Text::Placeholder),
        caret: end,
        selection: end..end,
        rows: Vec::new(),
        selected: 0,
        asking: false,
    }
}

fn icon(path: &str) -> Icon {
    icon_at(path, SIZE).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn app(aumid: &str) -> Icon {
    icon(&format!(r"shell:AppsFolder\{aumid}"))
}

fn at(title: &str, query: &str) -> Vec<std::ops::Range<u32>> {
    let lower = title.to_lowercase();
    let start = lower.find(&query.to_lowercase()).expect("in the title");
    let chars = lower[..start].chars().count();
    let positions: Vec<usize> = (chars..chars + query.chars().count()).collect();
    utf16_ranges(title, &positions)
}

/// A screenshot copied, 320 by 180: sky over a hill, a sun in the sky.
fn landscape() -> hyalo_core::clipboard::picture::Pixels {
    let (width, height) = (320_u32, 180_u32);
    let pixel = |index: u32| {
        let (x, y) = ((index % width) as f32, (index / width) as f32);
        let hill = 120.0 - 30.0 * (x / 60.0).sin();
        let sun = (x - 240.0).hypot(y - 50.0) < 22.0;
        match (y > hill, sun) {
            (true, _) => [60, 150, 70, 255],
            (false, true) => [70, 210, 250, 255],
            (false, false) => [235, 190, 120, 255],
        }
    };
    hyalo_core::clipboard::picture::Pixels {
        width,
        height,
        bgra: (0..width * height).flat_map(pixel).collect(),
    }
}

fn one(item: &hyalo_core::item::Item) -> Row<'_> {
    Row {
        id: &item.id,
        title: &item.title,
        subtitle: item.subtitle.as_str().into(),
        highlights: Vec::new(),
        icon: None,
        glyph: crate::results::glyph(item.kind, &item.action),
    }
}

#[test]
#[ignore]
fn readme_frames() {
    on_hidden_desktop(|| {
        let _com = Com::init().expect("COM");
        let demo = Scratch::new("readme").with(&[
            "Meeting notes.docx",
            "Release notes.md",
            "Notes\\",
            "Q3 report.pdf",
            "Q3 report.xlsx",
            "Roadmap.pptx",
        ]);
        let english = Language::English;

        // "note": apps first, then files and a folder.
        let notepad = app("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App");
        let sticky = app("Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe!App");
        let onenote = app("Microsoft.Office.ONENOTE.EXE.15");
        let docx = icon(&demo.path("Meeting notes.docx"));
        let md = icon(&demo.path("Release notes.md"));
        let folder = icon(&demo.path("Notes"));
        let rows = [
            ("Notepad", "App", &notepad),
            ("Sticky Notes", "App", &sticky),
            ("OneNote", "App", &onenote),
            ("Meeting notes.docx", r"C:\Users\you\Documents\Work", &docx),
            ("Release notes.md", r"C:\Users\you\code\hyalo", &md),
            ("Notes", r"C:\Users\you\Documents", &folder),
        ];
        let ids: Vec<ItemId> = rows.iter().map(|(t, _, _)| ItemId(t.to_string())).collect();
        let scene = Scene {
            rows: rows
                .iter()
                .zip(&ids)
                .map(|(&(title, subtitle, icon), id)| Row {
                    id,
                    title,
                    subtitle: subtitle.into(),
                    highlights: at(title, "note"),
                    icon: Some(icon),
                    glyph: None,
                })
                .collect(),
            selected: 0,
            ..bar("note")
        };
        draw("search", &scene, 6);

        // Answers: arithmetic, a currency, a web search.
        let engines = hyalo_core::web::defaults(english);
        let rates = crate::rates::tests::fixture();
        for (name, query) in [
            ("calculator", "(1920 + 1080) * 15%"),
            ("currency", "100 usd in eur"),
            ("web", "g rust lifetimes"),
        ] {
            let item = crate::results::answer(query, Some(&rates), &engines, english)
                .unwrap_or_else(|| panic!("an answer for {query}"));
            let scene = Scene {
                rows: vec![one(&item)],
                selected: 0,
                ..bar(query)
            };
            draw(name, &scene, 1);
        }

        // Open windows after ">".
        let windows = [
            (
                "README.md - hyalo - Cursor",
                "Cursor",
                app("Anysphere.Cursor"),
            ),
            (
                "Inbox - Outlook",
                "Outlook",
                app("Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows"),
            ),
            (
                "Downloads",
                "File Explorer",
                app("Microsoft.Windows.Explorer"),
            ),
            (
                "Windows PowerShell",
                "Terminal",
                app("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"),
            ),
            (
                "Rust lifetimes - Microsoft Edge",
                "Microsoft Edge",
                app("MSEdge"),
            ),
        ];
        let items: Vec<_> = (1..)
            .zip(&windows)
            .map(|(at, (title, app, _))| hyalo_core::switcher::item(at, title, app))
            .collect();
        let found = crate::results::Results::windows("", &items);
        let scene = Scene {
            rows: found
                .rows()
                .iter()
                .zip(&windows)
                .map(|(listed, (_, _, icon))| Row {
                    icon: Some(icon),
                    ..one(&listed.item)
                })
                .collect(),
            selected: 0,
            ..bar(">")
        };
        draw("windows", &scene, 5);

        // The clipboard history under the empty bar.
        {
            use hyalo_core::clipboard::{Content, Copied, History, picture};
            use hyalo_core::frecency::Timestamp;
            const NOW: u64 = 1_800_000_000;
            let text = |text: &str| Content::Text {
                text: text.to_string(),
                html: None,
                rtf: None,
            };
            let files = ["Q3 report.pdf", "Q3 report.xlsx", "Roadmap.pptx"]
                .map(|name| format!(r"C:\Users\you\Documents\Work\{name}"))
                .to_vec();
            let copies = [
                (Content::Files(files), "File Explorer", NOW - 2 * 3600),
                (text("cargo build --release"), "Terminal", NOW - 25 * 60),
                (
                    Content::Image {
                        width: 1920,
                        height: 1080,
                        png: vec![1],
                    },
                    "Snipping Tool",
                    NOW - 6 * 60,
                ),
                (
                    text(
                        "Sounds good, let's ship it on Friday.\r\nI'll write the release notes tonight.\r\n",
                    ),
                    "Outlook",
                    NOW - 40,
                ),
            ];
            let mut history = History::default();
            for (content, app, at) in copies {
                let copied = Copied {
                    content,
                    app: app.to_string(),
                    at: Timestamp(at),
                };
                history.add(copied).expect("kept");
            }
            let items: Vec<_> = history
                .entries()
                .iter()
                .map(|entry| entry.item(Timestamp(NOW), english))
                .collect();
            let found = crate::results::Results::copied("", &items);
            let shot = picture::thumbnail(&landscape(), SIZE);
            let shot = Icon {
                width: shot.width,
                height: shot.height,
                bgra: shot.bgra,
            };
            let pdf = icon(&demo.path("Q3 report.pdf"));
            let pictures = [None, Some(&shot), None, Some(&pdf)];
            let scene = Scene {
                rows: found
                    .rows()
                    .iter()
                    .zip(pictures)
                    .map(|(listed, icon)| Row {
                        icon,
                        ..one(&listed.item)
                    })
                    .collect(),
                selected: 0,
                ..bar("")
            };
            draw("clipboard", &scene, 4);
        }

        // System commands, the first asking.
        let commands = [
            ("Restart", "Press Enter again to restart", '\u{E777}'),
            ("Shut down", "System command", '\u{E7E8}'),
            ("Sleep", "System command", '\u{E708}'),
            ("Lock", "System command", '\u{E72E}'),
            ("Sign out", "System command", '\u{F3B1}'),
        ];
        let ids = commands.map(|(title, _, _)| ItemId(format!("system:{title}")));
        let scene = Scene {
            rows: commands
                .iter()
                .zip(&ids)
                .map(|(&(title, subtitle, glyph), id)| Row {
                    id,
                    title,
                    subtitle: subtitle.into(),
                    highlights: Vec::new(),
                    icon: None,
                    glyph: Some(glyph),
                })
                .collect(),
            selected: 0,
            asking: true,
            ..bar("restart")
        };
        draw("commands", &scene, 5);

        // The settings window, its first page, at 150 %.
        let written = hyalo_core::config::written(hyalo_core::config::DEFAULT).expect("good");
        let mut panel = crate::panel::Panel::new(Ok(written), Some(true), english);
        let canvas = Canvas::new();
        let painter =
            crate::paint::Painter::new(canvas.0, 960, 1290, DPI, Behind::Glass).expect("a painter");
        let (view, tall) = painter.view();
        let laid = crate::page::lay_out(&panel.page(), view, &|text| painter.measure(text));
        panel.fit(laid.height, tall);
        painter
            .draw(&laid, panel.scroll(), &|target| panel.look(target))
            .expect("draw");
        save("settings", &painter.read_frame().expect("frame"));
    });
}
