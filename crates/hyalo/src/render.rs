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
use std::mem::ManuallyDrop;
use std::ops::Range;

use hyalo_core::item::ItemId;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1RenderTarget,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::core::{Interface, PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::apps::Icon;
use crate::layout::{BAR_HEIGHT, ICON_SIZE, ROW_HEIGHT, row_top, scale};

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

const TRANSPARENT: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.0);
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

/// Present's sync interval: 0, do not wait for a vertical blank. A
/// composition swap chain cannot tear, DWM composes at the display's pace
/// anyway, and waiting made a burst of 120 redraws take 492 ms instead of
/// 30 to 45 ms. No test pins this: under load (mutator's parallel
/// workers), wall-clock time could not tell the two values apart.
const SYNC_INTERVAL: u32 = 0;

/// Every bitmap here, the swap chain's and the icons, holds premultiplied
/// BGRA.
const PREMULTIPLIED_BGRA: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT {
    format: DXGI_FORMAT_B8G8R8A8_UNORM,
    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
};

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

pub struct Renderer {
    swap_chain: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    fonts: Fonts,
    // Kept alive so the composition tree stays attached to the window.
    _composition: (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    width: u32,
    height: u32,
    dpi: u32,
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
        let d3d = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let swap_chain = create_swap_chain(&d3d, width, height)?;
        let dc = create_device_context(&dxgi_device)?;
        // Grayscale: ClearType needs an opaque target, and this one is not.
        unsafe { dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
        let composition = compose(hwnd, &dxgi_device, &swap_chain)?;
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let fonts = Fonts::new(&dwrite)?;
        let renderer = Self {
            swap_chain,
            dc,
            dwrite,
            fonts,
            _composition: composition,
            width,
            height,
            dpi,
            icons: HashMap::new(),
        };
        renderer.bind_target()?;
        Ok(renderer)
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<()> {
        if (width, height, dpi) == (self.width, self.height, self.dpi) {
            return Ok(());
        }
        unsafe {
            self.dc.SetTarget(None);
            self.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        (self.width, self.height, self.dpi) = (width, height, dpi);
        self.bind_target()
    }

    pub fn draw(&mut self, scene: &Scene) -> Result<()> {
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&TRANSPARENT));
        }
        // EndDraw even when drawing failed, so the next frame can begin.
        let drawn = self.draw_scene(scene);
        unsafe { self.dc.EndDraw(None, None)? };
        drawn?;
        unsafe { self.swap_chain.Present(SYNC_INTERVAL, DXGI_PRESENT(0)).ok() }
    }

    fn draw_scene(&mut self, scene: &Scene) -> Result<()> {
        let scale = scale(self.dpi);
        let (w, h) = (self.width as f32 / scale, self.height as f32 / scale);
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

    fn bind_target(&self) -> Result<()> {
        let dpi = self.dpi as f32;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: PREMULTIPLIED_BGRA,
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        unsafe {
            let surface: IDXGISurface = self.swap_chain.GetBuffer(0)?;
            let bitmap = self
                .dc
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dc.SetTarget(&bitmap);
            self.dc.SetDpi(dpi, dpi);
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
        let tint = self.solid(TINT)?;
        let sheen =
            self.vertical_gradient(h, rgba(1.0, 1.0, 1.0, 0.09), rgba(1.0, 1.0, 1.0, 0.0))?;
        let rim_light =
            self.vertical_gradient(h, rgba(1.0, 1.0, 1.0, 0.38), rgba(1.0, 1.0, 1.0, 0.10))?;
        unsafe {
            self.dc.FillRectangle(&full, &tint);
            self.dc.FillRectangle(&full, &sheen);
            self.dc.DrawRoundedRectangle(&rim, &rim_light, 1.0, None);
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
        let brush = self.solid(ICON)?;
        unsafe {
            self.dc.DrawEllipse(&lens, &brush, 2.0, None);
            self.dc
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
        let layout = self.layout(text, &self.fonts.query, 0.0, BAR_HEIGHT)?;
        self.draw_text(&layout, TEXT_LEFT, 0.0, color)?;
        let caret_x = TEXT_LEFT + self.typed_width(query)? + 1.0;
        self.draw_caret(caret_x)
    }

    fn typed_width(&self, query: &str) -> Result<f32> {
        let layout = self.layout(query, &self.fonts.query, 0.0, BAR_HEIGHT)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics.widthIncludingTrailingWhitespace)
    }

    fn draw_caret(&self, x: f32) -> Result<()> {
        let half = QUERY_SIZE * 0.55;
        let brush = self.solid(CARET)?;
        unsafe {
            self.dc.DrawLine(
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
        let brush = self.solid(SEPARATOR)?;
        unsafe {
            self.dc.DrawLine(
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
        let brush = self.solid(SELECTION)?;
        unsafe { self.dc.FillRoundedRectangle(&selection, &brush) };
        Ok(())
    }

    fn draw_row(&mut self, row: &Row, top: f32, w: f32) -> Result<()> {
        if let Some(icon) = row.icon {
            self.draw_icon(row.id, icon, top)?;
        }
        let width = w - TEXT_LEFT - TEXT_RIGHT_PAD;
        let (title_top, title_height) = TITLE_LINE;
        let title = self.layout(row.title, &self.fonts.title, width, title_height)?;
        self.highlight(&title, &row.highlights)?;
        self.draw_text(&title, TEXT_LEFT, top + title_top, TITLE_TEXT)?;
        let (subtitle_top, subtitle_height) = SUBTITLE_LINE;
        let subtitle = self.layout(row.subtitle, &self.fonts.subtitle, width, subtitle_height)?;
        self.draw_text(&subtitle, TEXT_LEFT, top + subtitle_top, SUBTITLE_TEXT)
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
            self.dc
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
        let bitmap = unsafe { self.dc.CreateBitmap(size, pixels, icon.width * 4, &props)? };
        self.icons.insert(id.clone(), bitmap.clone());
        Ok(bitmap)
    }

    /// Marks the matched letters: semibold, in the accent colour.
    fn highlight(&self, layout: &IDWriteTextLayout, ranges: &[Range<u32>]) -> Result<()> {
        let brush = self.solid(MATCHED_TEXT)?;
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

    fn draw_text(
        &self,
        layout: &IDWriteTextLayout,
        x: f32,
        y: f32,
        color: D2D1_COLOR_F,
    ) -> Result<()> {
        let brush = self.solid(color)?;
        let options = D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
        unsafe {
            self.dc
                .DrawTextLayout(Vector2 { X: x, Y: y }, layout, &brush, options);
        }
        Ok(())
    }

    fn layout(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        max_width: f32,
        height: f32,
    ) -> Result<IDWriteTextLayout> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dwrite
                .CreateTextLayout(&utf16, format, max_width, height)
        }
    }

    fn solid(&self, color: D2D1_COLOR_F) -> Result<ID2D1Brush> {
        unsafe { self.dc.CreateSolidColorBrush(&color, None)?.cast() }
    }

    fn vertical_gradient(
        &self,
        h: f32,
        top: D2D1_COLOR_F,
        bottom: D2D1_COLOR_F,
    ) -> Result<ID2D1Brush> {
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: top,
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: bottom,
            },
        ];
        let line = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 { X: 0.0, Y: 0.0 },
            endPoint: Vector2 { X: 0.0, Y: h },
        };
        unsafe {
            // The plain render-target overload; the device-context one wants
            // colour spaces and buffer precision we do not need.
            let stops = ID2D1RenderTarget::CreateGradientStopCollection(
                &self.dc,
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            )?;
            self.dc
                .CreateLinearGradientBrush(&line, None, &stops)?
                .cast()
        }
    }
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory) -> Result<Self> {
        let display = w!("Segoe UI Variable Display");
        let text = w!("Segoe UI Variable Text");
        Ok(Self {
            query: text_format(dwrite, display, QUERY_SIZE)?,
            title: trimmed(dwrite, text_format(dwrite, text, TITLE_SIZE)?)?,
            subtitle: trimmed(dwrite, text_format(dwrite, text, SUBTITLE_SIZE)?)?,
        })
    }
}

/// One line of text, centred in the height of its layout.
fn text_format(dwrite: &IDWriteFactory, family: PCWSTR, size: f32) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
    }
}

/// Text that does not fit its layout's width ends in an ellipsis.
fn trimmed(dwrite: &IDWriteFactory, format: IDWriteTextFormat) -> Result<IDWriteTextFormat> {
    let trimming = DWRITE_TRIMMING {
        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
        ..Default::default()
    };
    unsafe {
        let ellipsis = dwrite.CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(&trimming, &ellipsis)?;
    }
    Ok(format)
}

fn create_d3d_device() -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(windows::core::Error::empty)
}

fn create_swap_chain(d3d: &ID3D11Device, width: u32, height: u32) -> Result<IDXGISwapChain1> {
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        ..Default::default()
    };
    unsafe {
        let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        factory.CreateSwapChainForComposition(d3d, &desc, None)
    }
}

fn create_device_context(dxgi_device: &IDXGIDevice) -> Result<ID2D1DeviceContext> {
    unsafe {
        let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        factory
            .CreateDevice(dxgi_device)?
            .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
    }
}

fn compose(
    hwnd: HWND,
    dxgi_device: &IDXGIDevice,
    swap_chain: &IDXGISwapChain1,
) -> Result<(
    IDCompositionDevice,
    IDCompositionTarget,
    IDCompositionVisual,
)> {
    unsafe {
        let device: IDCompositionDevice = DCompositionCreateDevice(dxgi_device)?;
        let target = device.CreateTargetForHwnd(hwnd, true)?;
        let visual = device.CreateVisual()?;
        visual.SetContent(swap_chain)?;
        target.SetRoot(&visual)?;
        device.Commit()?;
        Ok((device, target, visual))
    }
}

#[cfg(test)]
pub use readback::Frame;

/// Reading frames back from the GPU, so tests can check what is drawn.
#[cfg(test)]
mod readback {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, ID3D11Device, ID3D11Texture2D,
    };
    use windows::core::Result;

    use super::Renderer;

    /// One frame: premultiplied BGRA pixels, row by row.
    pub struct Frame {
        pub width: usize,
        pub height: usize,
        bgra: Vec<u8>,
    }

    impl Frame {
        /// Blue, green, red and alpha of one pixel.
        pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
            let at = (y * self.width + x) * 4;
            [
                self.bgra[at],
                self.bgra[at + 1],
                self.bgra[at + 2],
                self.bgra[at + 3],
            ]
        }
    }

    impl Renderer {
        /// Copies the last presented frame to the CPU.
        ///
        /// In a flip-model swap chain buffer 0 is always the next back
        /// buffer; with two buffers, buffer 1 is the one just presented.
        /// `FLIP_SEQUENTIAL` keeps it readable.
        pub fn read_frame(&self) -> Result<Frame> {
            // One mip level, one array slice: the texture's only
            // subresource, mapped and unmapped by this index.
            let subresource = 0;
            unsafe {
                let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(1)?;
                let device: ID3D11Device = self.swap_chain.GetDevice()?;
                let staging = staging_copy(&device, &buffer)?;
                let context = device.GetImmediateContext()?;
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context.Map(&staging, subresource, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let (width, height) = (self.width as usize, self.height as usize);
                let pitch = mapped.RowPitch as usize;
                // SAFETY: Map succeeded, so until Unmap pData points to
                // `height` rows of `pitch` bytes, each at least `width`
                // pixels.
                let row = |index: usize| {
                    let start = mapped.pData.cast::<u8>().add(index * pitch);
                    std::slice::from_raw_parts(start, width * 4)
                };
                let bgra = (0..height).flat_map(row).copied().collect();
                context.Unmap(&staging, subresource);
                Ok(Frame {
                    width,
                    height,
                    bgra,
                })
            }
        }
    }

    /// A CPU-readable copy of `texture`.
    fn staging_copy(device: &ID3D11Device, texture: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        let mut staging = None;
        unsafe {
            texture.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(windows::core::Error::empty)?;
            device
                .GetImmediateContext()?
                .CopyResource(&staging, texture);
            Ok(staging)
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP,
    };

    use super::*;
    use crate::highlight::utf16_ranges;
    use crate::reference::assert_looks_like;
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

    /// A bare window for a renderer to draw into, destroyed after the test.
    struct Canvas(HWND);

    impl Canvas {
        fn new() -> Self {
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP,
                    w!("STATIC"),
                    None,
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    None,
                    None,
                )
            };
            Self(hwnd.expect("a window"))
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
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
    fn the_picture_is_composed_above_the_window_content() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let _renderer = Renderer::new(canvas.0, 720, 64, 96).expect("a renderer");
            // A window has one topmost composition target; the renderer's
            // should be it, so a second one is refused.
            let device: IDCompositionDevice =
                unsafe { DCompositionCreateDevice(None) }.expect("a composition device");
            let second = unsafe { device.CreateTargetForHwnd(canvas.0, true) };
            let refused = second.expect_err("the topmost target is taken");
            assert_eq!(refused.code(), DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED);
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
