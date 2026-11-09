//! Draws the launcher with Direct2D into a DirectComposition swap chain.
//!
//! The swap chain uses premultiplied alpha, so every pixel left transparent
//! shows the DWM backdrop (the blurred glass) underneath. On top of it we
//! paint a light tint, a sheen, a rim highlight, the search icon and text.
//!
//! The `windows` crate marks every COM call `unsafe`. The invariant all of
//! them rely on: each interface is a live COM object owned by `Renderer`,
//! used only from the UI thread that created it.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE,
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Brush,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1RenderTarget,
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
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
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
use windows::core::{Interface, Result, w};
use windows_numerics::Vector2;

/// What to draw.
pub struct Scene<'a> {
    pub query: &'a str,
}

const PLACEHOLDER: &str = "Search apps, files, settings";
const FONT_SIZE: f32 = 24.0;
/// Matches the radius DWM uses for `DWMWCP_ROUND`.
const CORNER_RADIUS: f32 = 8.0;
const TEXT_LEFT: f32 = 56.0;
const TEXT_RIGHT_PAD: f32 = 20.0;
const ICON_CENTER_X: f32 = 30.0;

const TRANSPARENT: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.0);
/// Darkens the acrylic slightly so white text keeps its contrast.
const TINT: D2D1_COLOR_F = rgba(0.06, 0.06, 0.08, 0.28);
const TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.95);
const PLACEHOLDER_TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.42);
const ICON: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.6);
const CARET: D2D1_COLOR_F = rgba(0.45, 0.68, 1.0, 1.0);

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

pub struct Renderer {
    swap_chain: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    // Kept alive so the composition tree stays attached to the window.
    _composition: (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    width: u32,
    height: u32,
    dpi: u32,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32) -> Result<Self> {
        let d3d = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let swap_chain = create_swap_chain(&d3d, width, height)?;
        let dc = create_device_context(&dxgi_device)?;
        let composition = compose(hwnd, &dxgi_device, &swap_chain)?;
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let text_format = create_text_format(&dwrite)?;
        let renderer = Self {
            swap_chain,
            dc,
            dwrite,
            text_format,
            _composition: composition,
            width,
            height,
            dpi,
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

    pub fn draw(&self, scene: &Scene) -> Result<()> {
        let scale = self.dpi as f32 / 96.0;
        let (w, h) = (self.width as f32 / scale, self.height as f32 / scale);
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&TRANSPARENT));
        }
        self.draw_glass(w, h)?;
        self.draw_search_icon(h)?;
        self.draw_query(scene.query, w, h)?;
        unsafe {
            self.dc.EndDraw(None, None)?;
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    fn bind_target(&self) -> Result<()> {
        let dpi = self.dpi as f32;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
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

    fn draw_search_icon(&self, h: f32) -> Result<()> {
        let center = Vector2 {
            X: ICON_CENTER_X - 2.0,
            Y: h / 2.0 - 2.0,
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

    fn draw_query(&self, query: &str, w: f32, h: f32) -> Result<()> {
        let (text, color) = match query {
            "" => (PLACEHOLDER, PLACEHOLDER_TEXT),
            typed => (typed, TEXT),
        };
        let layout = self.layout(text, w - TEXT_LEFT - TEXT_RIGHT_PAD, h)?;
        let brush = self.solid(color)?;
        let origin = Vector2 {
            X: TEXT_LEFT,
            Y: 0.0,
        };
        unsafe {
            self.dc.DrawTextLayout(
                origin,
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
        }
        let caret_x = TEXT_LEFT + self.typed_width(query, w, h)? + 1.0;
        self.draw_caret(caret_x, h)
    }

    fn typed_width(&self, query: &str, w: f32, h: f32) -> Result<f32> {
        let layout = self.layout(query, w, h)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics.widthIncludingTrailingWhitespace)
    }

    fn draw_caret(&self, x: f32, h: f32) -> Result<()> {
        let half = FONT_SIZE * 0.55;
        let brush = self.solid(CARET)?;
        unsafe {
            self.dc.DrawLine(
                Vector2 {
                    X: x,
                    Y: h / 2.0 - half,
                },
                Vector2 {
                    X: x,
                    Y: h / 2.0 + half,
                },
                &brush,
                2.0,
                None,
            );
        }
        Ok(())
    }

    fn layout(&self, text: &str, max_width: f32, h: f32) -> Result<IDWriteTextLayout> {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dwrite
                .CreateTextLayout(&utf16, &self.text_format, max_width, h)
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

fn create_text_format(dwrite: &IDWriteFactory) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            w!("Segoe UI Variable Display"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            FONT_SIZE,
            w!(""),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
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
            unsafe {
                let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(1)?;
                let device: ID3D11Device = self.swap_chain.GetDevice()?;
                let staging = staging_copy(&device, &buffer)?;
                let context = device.GetImmediateContext()?;
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let (width, height) = (self.width as usize, self.height as usize);
                let mut bgra = Vec::with_capacity(width * height * 4);
                for row in 0..height {
                    // SAFETY: Map succeeded, so pData points to `height`
                    // rows of RowPitch bytes, each at least `width` pixels.
                    let start = mapped
                        .pData
                        .cast::<u8>()
                        .add(row * mapped.RowPitch as usize);
                    bgra.extend_from_slice(std::slice::from_raw_parts(start, width * 4));
                }
                context.Unmap(&staging, 0);
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
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP,
    };

    use super::*;
    use crate::reference::assert_looks_like;
    use crate::test_desktop::on_hidden_desktop;

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
        let renderer = Renderer::new(canvas.0, width, height, dpi).expect("a renderer");
        renderer.draw(scene).expect("draw");
        renderer.read_frame().expect("read the frame back")
    }

    #[test]
    fn the_empty_bar_shows_the_placeholder_and_the_caret() {
        on_hidden_desktop(|| {
            let frame = drawn(&Scene { query: "" }, (720, 64), 96);
            assert_looks_like("bar-empty-100", &frame);
        });
    }

    #[test]
    fn a_typed_query_at_125_percent() {
        on_hidden_desktop(|| {
            let frame = drawn(&Scene { query: "notepad" }, (900, 80), 120);
            assert_looks_like("bar-typed-125", &frame);
        });
    }

    #[test]
    fn a_resized_renderer_draws_at_its_new_size_and_scale() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let mut renderer = Renderer::new(canvas.0, 720, 64, 96).expect("a renderer");
            renderer.draw(&Scene { query: "notepad" }).expect("draw");
            renderer.resize(900, 80, 120).expect("resize");
            renderer
                .draw(&Scene { query: "notepad" })
                .expect("draw again");
            let frame = renderer.read_frame().expect("read the frame back");
            assert_looks_like("bar-typed-125", &frame);
        });
    }
}
