//! Draws the launcher with Direct2D into a DirectComposition swap chain.
//!
//! The swap chain uses premultiplied alpha, so every pixel left transparent
//! shows the DWM backdrop (the blurred glass) underneath. On top of it we
//! paint a light tint, a sheen, a rim highlight, the search icon and the
//! query; under the bar, the results list: per row an icon, the title with
//! the matched letters highlighted, and a dim subtitle.
//!
//! Sizes are in device-independent pixels (DIPs); the device context
//! scales them to the monitor's DPI.
//!
//! The `windows` crate marks every COM call `unsafe`. The invariant all of
//! them rely on: each interface is a live COM object owned by `Renderer`,
//! used only from the UI thread that created it.

use std::collections::HashMap;
use std::ops::Range;

use hyalo_core::item::ItemId;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_ELLIPSE,
    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::core::Result;
use windows_numerics::Vector2;

use crate::apps::Icon;
use crate::layout::{BAR_HEIGHT, ICON_SIZE, ROW_HEIGHT, row_top};
use crate::surface::{
    DISPLAY_FONT, PREMULTIPLIED_BGRA, Surface, TEXT_FONT, rgba, text_format, trimmed,
};

/// What to draw.
pub struct Scene<'a> {
    pub query: &'a str,
    /// The results list under the bar; empty when there is none.
    pub rows: Vec<Row<'a>>,
    /// The selected row; ignored without rows.
    pub selected: usize,
}

/// One row of the results list.
pub struct Row<'a> {
    /// Keys the icon bitmaps the renderer keeps.
    pub id: &'a ItemId,
    pub title: &'a str,
    pub subtitle: &'a str,
    /// The letters the query matched, as UTF-16 ranges of `title`.
    pub highlights: Vec<Range<u32>>,
    pub icon: Option<&'a Icon>,
}

const PLACEHOLDER: &str = "Search apps, files, settings";
const QUERY_SIZE: f32 = 24.0;
const TITLE_SIZE: f32 = 15.0;
const SUBTITLE_SIZE: f32 = 12.0;
/// Matches the radius DWM uses for `DWMWCP_ROUND`.
const CORNER_RADIUS: f32 = 8.0;
/// Where the query, the titles and the subtitles start.
const TEXT_LEFT: f32 = 56.0;
const TEXT_RIGHT_PAD: f32 = 20.0;
const ICON_CENTER_X: f32 = 30.0;
/// The left edge of a row's icon.
const ICON_LEFT: f32 = 12.0;
/// The title line and the subtitle line: top edge in the row, height.
const TITLE_LINE: (f32, f32) = (4.0, 22.0);
const SUBTITLE_LINE: (f32, f32) = (25.0, 18.0);
/// The selected row's highlight: inset from the window's sides, radius.
const SELECTION_INSET: f32 = 6.0;
const SELECTION_RADIUS: f32 = 6.0;

/// Darkens the acrylic slightly so white text keeps its contrast.
const TINT: D2D1_COLOR_F = rgba(0.06, 0.06, 0.08, 0.28);
const TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.95);
const PLACEHOLDER_TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.42);
const ICON: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.6);
const CARET: D2D1_COLOR_F = rgba(0.45, 0.68, 1.0, 1.0);
const SEPARATOR: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.10);
const SELECTION: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.12);
const TITLE_TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.92);
const MATCHED_TEXT: D2D1_COLOR_F = rgba(0.55, 0.76, 1.0, 1.0);
const SUBTITLE_TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.5);

pub struct Renderer {
    surface: Surface,
    fonts: Fonts,
    /// Icons already on the GPU, by item.
    icons: HashMap<ItemId, ID2D1Bitmap1>,
}

/// The text formats: the query, a row's title, a row's subtitle.
struct Fonts {
    query: IDWriteTextFormat,
    title: IDWriteTextFormat,
    subtitle: IDWriteTextFormat,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32) -> Result<Self> {
        let surface = Surface::new(hwnd, width, height, dpi)?;
        let fonts = Fonts::new(&surface.dwrite)?;
        Ok(Self {
            surface,
            fonts,
            icons: HashMap::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<()> {
        self.surface.resize(width, height, dpi)
    }

    pub fn draw(&mut self, scene: &Scene) -> Result<()> {
        self.surface.begin();
        let drawn = self.draw_scene(scene);
        self.surface.end(drawn)
    }

    fn draw_scene(&mut self, scene: &Scene) -> Result<()> {
        let (w, h) = self.surface.size();
        self.draw_glass(w, h)?;
        self.draw_search_icon()?;
        self.draw_query(scene.query)?;
        if scene.rows.is_empty() {
            return Ok(());
        }
        self.draw_separator(w)?;
        self.draw_selection(scene.selected, w)?;
        for (index, row) in scene.rows.iter().enumerate() {
            self.draw_row(row, row_top(index), w)?;
        }
        Ok(())
    }

    fn draw_glass(&self, w: f32, h: f32) -> Result<()> {
        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: w,
            bottom: h,
        };
        let rim = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: w - 0.5,
                bottom: h - 0.5,
            },
            radiusX: CORNER_RADIUS - 0.5,
            radiusY: CORNER_RADIUS - 0.5,
        };
        let tint = self.surface.solid(TINT)?;
        let sheen = self.surface.vertical_gradient(
            h,
            rgba(1.0, 1.0, 1.0, 0.09),
            rgba(1.0, 1.0, 1.0, 0.0),
        )?;
        let rim_light = self.surface.vertical_gradient(
            h,
            rgba(1.0, 1.0, 1.0, 0.38),
            rgba(1.0, 1.0, 1.0, 0.10),
        )?;
        unsafe {
            self.surface.dc.FillRectangle(&full, &tint);
            self.surface.dc.FillRectangle(&full, &sheen);
            self.surface
                .dc
                .DrawRoundedRectangle(&rim, &rim_light, 1.0, None);
        }
        Ok(())
    }

    fn draw_search_icon(&self) -> Result<()> {
        let center = Vector2 {
            X: ICON_CENTER_X - 2.0,
            Y: BAR_HEIGHT / 2.0 - 2.0,
        };
        let lens = D2D1_ELLIPSE {
            point: center,
            radiusX: 8.0,
            radiusY: 8.0,
        };
        let handle_start = Vector2 {
            X: center.X + 5.8,
            Y: center.Y + 5.8,
        };
        let handle_end = Vector2 {
            X: center.X + 11.0,
            Y: center.Y + 11.0,
        };
        let brush = self.surface.solid(ICON)?;
        unsafe {
            self.surface.dc.DrawEllipse(&lens, &brush, 2.0, None);
            self.surface
                .dc
                .DrawLine(handle_start, handle_end, &brush, 2.4, None);
        }
        Ok(())
    }

    /// The query or the placeholder, and the caret after what is typed.
    /// No wrapping and leading alignment: the text runs on from its
    /// start, so its layout needs no width.
    fn draw_query(&self, query: &str) -> Result<()> {
        let (text, color) = match query {
            "" => (PLACEHOLDER, PLACEHOLDER_TEXT),
            typed => (typed, TEXT),
        };
        let layout = self
            .surface
            .layout(text, &self.fonts.query, 0.0, BAR_HEIGHT)?;
        self.surface.draw_text(&layout, TEXT_LEFT, 0.0, color)?;
        let caret_x = TEXT_LEFT + self.typed_width(query)? + 1.0;
        self.draw_caret(caret_x)
    }

    fn typed_width(&self, query: &str) -> Result<f32> {
        let layout = self
            .surface
            .layout(query, &self.fonts.query, 0.0, BAR_HEIGHT)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics.widthIncludingTrailingWhitespace)
    }

    fn draw_caret(&self, x: f32) -> Result<()> {
        let half = QUERY_SIZE * 0.55;
        let brush = self.surface.solid(CARET)?;
        unsafe {
            self.surface.dc.DrawLine(
                Vector2 {
                    X: x,
                    Y: BAR_HEIGHT / 2.0 - half,
                },
                Vector2 {
                    X: x,
                    Y: BAR_HEIGHT / 2.0 + half,
                },
                &brush,
                2.0,
                None,
            );
        }
        Ok(())
    }

    /// A hairline between the bar and the list.
    fn draw_separator(&self, w: f32) -> Result<()> {
        let y = BAR_HEIGHT - 0.5;
        let brush = self.surface.solid(SEPARATOR)?;
        unsafe {
            self.surface.dc.DrawLine(
                Vector2 { X: 0.0, Y: y },
                Vector2 { X: w, Y: y },
                &brush,
                1.0,
                None,
            );
        }
        Ok(())
    }

    fn draw_selection(&self, index: usize, w: f32) -> Result<()> {
        let top = row_top(index);
        let selection = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: SELECTION_INSET,
                top,
                right: w - SELECTION_INSET,
                bottom: top + ROW_HEIGHT,
            },
            radiusX: SELECTION_RADIUS,
            radiusY: SELECTION_RADIUS,
        };
        let brush = self.surface.solid(SELECTION)?;
        unsafe { self.surface.dc.FillRoundedRectangle(&selection, &brush) };
        Ok(())
    }

    fn draw_row(&mut self, row: &Row, top: f32, w: f32) -> Result<()> {
        if let Some(icon) = row.icon {
            self.draw_icon(row.id, icon, top)?;
        }
        let width = w - TEXT_LEFT - TEXT_RIGHT_PAD;
        let (title_top, title_height) = TITLE_LINE;
        let title = self
            .surface
            .layout(row.title, &self.fonts.title, width, title_height)?;
        self.highlight(&title, &row.highlights)?;
        self.surface
            .draw_text(&title, TEXT_LEFT, top + title_top, TITLE_TEXT)?;
        let (subtitle_top, subtitle_height) = SUBTITLE_LINE;
        let subtitle =
            self.surface
                .layout(row.subtitle, &self.fonts.subtitle, width, subtitle_height)?;
        self.surface
            .draw_text(&subtitle, TEXT_LEFT, top + subtitle_top, SUBTITLE_TEXT)
    }

    /// The icon, centred in the row's height.
    fn draw_icon(&mut self, id: &ItemId, icon: &Icon, top: f32) -> Result<()> {
        let bitmap = self.icon_bitmap(id, icon)?;
        let y = top + (ROW_HEIGHT - ICON_SIZE) / 2.0;
        let place = D2D_RECT_F {
            left: ICON_LEFT,
            top: y,
            right: ICON_LEFT + ICON_SIZE,
            bottom: y + ICON_SIZE,
        };
        let smooth = D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC;
        unsafe {
            self.surface
                .dc
                .DrawBitmap(&bitmap, Some(&place), 1.0, smooth, None, None)
        };
        Ok(())
    }

    /// The icon as a Direct2D bitmap, uploaded the first time it is drawn.
    fn icon_bitmap(&mut self, id: &ItemId, icon: &Icon) -> Result<ID2D1Bitmap1> {
        if let Some(bitmap) = self.icons.get(id) {
            return Ok(bitmap.clone());
        }
        let size = D2D_SIZE_U {
            width: icon.width,
            height: icon.height,
        };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: PREMULTIPLIED_BGRA,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            ..Default::default()
        };
        let pixels = Some(icon.bgra.as_ptr().cast());
        // SAFETY: `pixels` holds `height` rows of `width * 4` bytes, which
        // CreateBitmap copies before it returns.
        let bitmap = unsafe {
            self.surface
                .dc
                .CreateBitmap(size, pixels, icon.width * 4, &props)?
        };
        self.icons.insert(id.clone(), bitmap.clone());
        Ok(bitmap)
    }

    /// Marks the matched letters: semibold, in the accent colour.
    fn highlight(&self, layout: &IDWriteTextLayout, ranges: &[Range<u32>]) -> Result<()> {
        let brush = self.surface.solid(MATCHED_TEXT)?;
        for range in ranges {
            let range = DWRITE_TEXT_RANGE {
                startPosition: range.start,
                length: range.end - range.start,
            };
            unsafe {
                layout.SetFontWeight(DWRITE_FONT_WEIGHT_SEMI_BOLD, range)?;
                layout.SetDrawingEffect(&brush, range)?;
            }
        }
        Ok(())
    }
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory) -> Result<Self> {
        let normal = DWRITE_FONT_WEIGHT_NORMAL;
        Ok(Self {
            query: text_format(dwrite, DISPLAY_FONT, QUERY_SIZE, normal)?,
            title: trimmed(dwrite, text_format(dwrite, TEXT_FONT, TITLE_SIZE, normal)?)?,
            subtitle: trimmed(
                dwrite,
                text_format(dwrite, TEXT_FONT, SUBTITLE_SIZE, normal)?,
            )?,
        })
    }
}

#[cfg(test)]
pub use crate::surface::Frame;

#[cfg(test)]
impl Renderer {
    /// Copies the last presented frame to the CPU.
    pub fn read_frame(&self) -> Result<Frame> {
        self.surface.read_frame()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::utf16_ranges;
    use crate::reference::assert_looks_like;
    use crate::surface::Canvas;
    use crate::test_desktop::on_hidden_desktop;

    impl Scene<'static> {
        /// The bar alone, with `query` typed.
        fn bar(query: &'static str) -> Self {
            Scene {
                query,
                rows: Vec::new(),
                selected: 0,
            }
        }
    }

    /// `scene` drawn at `width` x `height` pixels and `dpi`.
    fn drawn(scene: &Scene, (width, height): (u32, u32), dpi: u32) -> Frame {
        let canvas = Canvas::new();
        let mut renderer = Renderer::new(canvas.0, width, height, dpi).expect("a renderer");
        renderer.draw(scene).expect("draw");
        renderer.read_frame().expect("read the frame back")
    }

    #[test]
    fn the_empty_bar_shows_the_placeholder_and_the_caret() {
        on_hidden_desktop(|| {
            let frame = drawn(&Scene::bar(""), (720, 64), 96);
            assert_looks_like("bar-empty-100", &frame);
        });
    }

    #[test]
    fn a_typed_query_at_125_percent() {
        on_hidden_desktop(|| {
            let frame = drawn(&Scene::bar("notepad"), (900, 80), 120);
            assert_looks_like("bar-typed-125", &frame);
        });
    }

    /// A test icon: a disc of one opaque colour on transparency.
    fn disc(side: u32, [blue, green, red]: [u8; 3]) -> Icon {
        let middle = side as f32 / 2.0;
        let pixel = |index: u32| {
            let x = (index % side) as f32 + 0.5 - middle;
            let y = (index / side) as f32 + 0.5 - middle;
            match x.hypot(y) < middle - 1.0 {
                true => [blue, green, red, 255],
                false => [0, 0, 0, 0],
            }
        };
        Icon {
            width: side,
            height: side,
            bgra: (0..side * side).flat_map(pixel).collect(),
        }
    }

    #[test]
    fn lists_rows_under_the_bar_at_150_percent() {
        on_hidden_desktop(|| {
            let ids = ["notepad", "sticky", "long"].map(|id| ItemId(id.to_string()));
            let (blue, green) = (disc(48, [240, 160, 40]), disc(48, [90, 200, 80]));
            let rows = || {
                vec![
                    Row {
                        id: &ids[0],
                        title: "Notepad",
                        subtitle: "App",
                        highlights: utf16_ranges("Notepad", &[0, 1, 2, 3]),
                        icon: Some(&blue),
                    },
                    Row {
                        id: &ids[1],
                        title: "Sticky Notes",
                        subtitle: "App",
                        highlights: utf16_ranges("Sticky Notes", &[7, 8, 9, 10]),
                        icon: Some(&green),
                    },
                    Row {
                        id: &ids[2],
                        title: "A name so long that it cannot fit in the window and has to end in an ellipsis just before the right edge",
                        subtitle: r"C:\Program Files\Some Vendor\Some Product\a path long enough that it, too, has to be cut short with an ellipsis\bin\app.exe",
                        highlights: Vec::new(),
                        icon: None,
                    },
                ]
            };
            let canvas = Canvas::new();
            let mut renderer = Renderer::new(canvas.0, 1080, 336, 144).expect("a renderer");
            for _ in 0..2 {
                let scene = Scene {
                    query: "note",
                    rows: rows(),
                    selected: 1,
                };
                renderer.draw(&scene).expect("draw");
                let frame = renderer.read_frame().expect("read the frame back");
                assert_looks_like("list-150", &frame);
            }
        });
    }

    #[test]
    fn an_answer_row_with_grouped_digits_at_100_percent() {
        on_hidden_desktop(|| {
            let id = ItemId("hyalo:calculator".to_string());
            let scene = Scene {
                query: "1234567+0,5",
                rows: vec![Row {
                    id: &id,
                    title: "1\u{A0}234\u{A0}567,5",
                    subtitle: "Calculator · Enter to copy",
                    highlights: Vec::new(),
                    icon: None,
                }],
                selected: 0,
            };
            let frame = drawn(&scene, (720, 128), 96);
            assert_looks_like("answer-100", &frame);
        });
    }

    #[test]
    fn a_notice_under_the_empty_bar_at_100_percent() {
        on_hidden_desktop(|| {
            let id = ItemId("hyalo:notice".to_string());
            let scene = Scene {
                query: "",
                rows: vec![Row {
                    id: &id,
                    title: "config.toml, line 2: unknown field `hotkye`, expected `hotkey` or `folders`",
                    subtitle: "hyalo keeps its last good settings · Enter to open config.toml",
                    highlights: Vec::new(),
                    icon: None,
                }],
                selected: 0,
            };
            let frame = drawn(&scene, (720, 128), 96);
            assert_looks_like("notice-100", &frame);
        });
    }

    #[test]
    fn a_currency_row_shows_its_signs_at_100_percent() {
        on_hidden_desktop(|| {
            let id = ItemId("hyalo:currency".to_string());
            let scene = Scene {
                query: "100 usd",
                rows: vec![Row {
                    id: &id,
                    title: "8\u{A0}275.36\u{A0}₽",
                    subtitle: "100 USD → RUB · Bank of Russia, 10.11.2026 · Enter to copy",
                    highlights: Vec::new(),
                    icon: None,
                }],
                selected: 0,
            };
            let frame = drawn(&scene, (720, 128), 96);
            assert_looks_like("currency-100", &frame);
        });
    }

    #[test]
    fn a_resized_renderer_draws_at_its_new_size_and_scale() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let mut renderer = Renderer::new(canvas.0, 720, 64, 96).expect("a renderer");
            renderer.draw(&Scene::bar("notepad")).expect("draw");
            renderer.resize(900, 80, 120).expect("resize");
            renderer.draw(&Scene::bar("notepad")).expect("draw again");
            let frame = renderer.read_frame().expect("read the frame back");
            assert_looks_like("bar-typed-125", &frame);
        });
    }
}
