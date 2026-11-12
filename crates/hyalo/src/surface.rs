//! A window's picture: Direct2D drawing into a DirectComposition swap
//! chain with premultiplied alpha, so every pixel left transparent shows
//! the DWM backdrop underneath. The launcher and the settings window each
//! draw on one.
//!
//! Sizes are in device-independent pixels (DIPs); the device context
//! scales them to the monitor's DPI.
//!
//! The `windows` crate marks every COM call `unsafe`. The invariant all of
//! them rely on: each interface is a live COM object owned by `Surface`,
//! used only from the UI thread that created it.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
    D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1, ID2D1RenderTarget,
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
    DWRITE_FONT_WEIGHT, DWRITE_HIT_TEST_METRICS, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
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
use windows::core::{BOOL, Interface, PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::layout::scale;

const TRANSPARENT: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.0);

/// Present's sync interval: 0, do not wait for a vertical blank. A
/// composition swap chain cannot tear, DWM composes at the display's pace
/// anyway, and waiting made a burst of 120 redraws take 492 ms instead of
/// 30 to 45 ms. No test pins this: under load (mutator's parallel
/// workers), wall-clock time could not tell the two values apart.
const SYNC_INTERVAL: u32 = 0;

/// Every bitmap here, the swap chain's and the icons, holds premultiplied
/// BGRA.
pub const PREMULTIPLIED_BGRA: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT {
    format: DXGI_FORMAT_B8G8R8A8_UNORM,
    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
};

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// A font family to draw in, and the one to draw in instead where
/// Windows does not have it.
#[derive(Clone, Copy, Debug)]
pub struct Family {
    pub wanted: PCWSTR,
    pub instead: PCWSTR,
}

impl Family {
    /// The family to draw in: the one wanted, if Windows has it. A family
    /// DirectWrite does not find is drawn in whatever it finds for each
    /// letter, and a symbol font's symbols as nothing at all.
    fn installed(self, dwrite: &IDWriteFactory) -> Result<PCWSTR> {
        let mut collection = None;
        // SAFETY: `collection` is a live Option the call fills.
        unsafe { dwrite.GetSystemFontCollection(&mut collection, false)? };
        let collection = collection.ok_or_else(windows::core::Error::empty)?;
        let (mut index, mut exists) = (0, BOOL::default());
        // SAFETY: `wanted` is a nul-terminated literal; `index` and
        // `exists` are live and filled by the call.
        unsafe { collection.FindFamilyName(self.wanted, &mut index, &mut exists)? };
        Ok(if exists.as_bool() {
            self.wanted
        } else {
            self.instead
        })
    }
}

/// The families text is drawn in: the display cut for large text, the
/// text cut for the rest. Segoe UI Variable came with Windows 11; Windows
/// 10 has Segoe UI.
pub const DISPLAY_FONT: Family = Family {
    wanted: w!("Segoe UI Variable Display"),
    instead: w!("Segoe UI"),
};
pub const TEXT_FONT: Family = Family {
    wanted: w!("Segoe UI Variable Text"),
    instead: w!("Segoe UI"),
};

/// What a window's picture is drawn over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Behind {
    /// The glass DWM draws behind the window: what the picture leaves
    /// clear shows it.
    Glass,
    /// Nothing but the desktop, where DWM gives no glass (Windows 10,
    /// Windows 11 before 22H2): the picture starts from a solid colour.
    Desktop,
}

impl Behind {
    /// What DWM's answer to the backdrop a window asked for means.
    pub fn of(answer: Result<()>) -> Self {
        match answer {
            Ok(()) => Self::Glass,
            Err(_) => Self::Desktop,
        }
    }

    /// The colour a frame starts from: clear over the glass, else `solid`.
    pub fn ground(self, solid: D2D1_COLOR_F) -> D2D1_COLOR_F {
        match self {
            Self::Glass => TRANSPARENT,
            Self::Desktop => solid,
        }
    }
}

pub struct Surface {
    swap_chain: IDXGISwapChain1,
    pub dc: ID2D1DeviceContext,
    pub dwrite: IDWriteFactory,
    // Kept alive so the composition tree stays attached to the window.
    _composition: (
        IDCompositionDevice,
        IDCompositionTarget,
        IDCompositionVisual,
    ),
    width: u32,
    height: u32,
    dpi: u32,
    /// The colour each frame starts from.
    ground: D2D1_COLOR_F,
}

impl Surface {
    /// A picture `width` by `height` pixels, at least one each way: a
    /// minimized window has no size, and a swap chain must have one. Its
    /// frames start from `ground`.
    pub fn new(
        hwnd: HWND,
        width: u32,
        height: u32,
        dpi: u32,
        ground: D2D1_COLOR_F,
    ) -> Result<Self> {
        let (width, height) = (width.max(1), height.max(1));
        let d3d = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d.cast()?;
        let swap_chain = create_swap_chain(&d3d, width, height)?;
        let dc = create_device_context(&dxgi_device)?;
        // Grayscale: ClearType needs an opaque target, and this one is not.
        unsafe { dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
        let composition = compose(hwnd, &dxgi_device, &swap_chain)?;
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let surface = Self {
            swap_chain,
            dc,
            dwrite,
            _composition: composition,
            width,
            height,
            dpi,
            ground,
        };
        surface.bind_target()?;
        Ok(surface)
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
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

    /// The picture's size in DIPs.
    pub fn size(&self) -> (f32, f32) {
        let scale = scale(self.dpi);
        (self.width as f32 / scale, self.height as f32 / scale)
    }

    /// Starts a frame, cleared to its ground.
    pub fn begin(&self) {
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&self.ground));
        }
    }

    /// Ends the frame begun, whether drawing it went well (`drawn`) or
    /// not, so the next can begin, and shows it if it did.
    pub fn end(&self, drawn: Result<()>) -> Result<()> {
        unsafe { self.dc.EndDraw(None, None)? };
        drawn?;
        unsafe { self.swap_chain.Present(SYNC_INTERVAL, DXGI_PRESENT(0)).ok() }
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

    pub fn draw_text(
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

    pub fn layout(
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

    pub fn solid(&self, color: D2D1_COLOR_F) -> Result<ID2D1Brush> {
        unsafe { self.dc.CreateSolidColorBrush(&color, None)?.cast() }
    }

    pub fn vertical_gradient(
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

/// Behind the letters selected in a field: the launcher's blue,
/// see-through.
pub const SELECTED_TEXT: D2D1_COLOR_F = rgba(0.30, 0.52, 1.0, 0.45);

/// How far from the start of the one-line `layout` the caret before the
/// UTF-16 unit `at` goes, in DIPs.
pub fn caret_x(layout: &IDWriteTextLayout, at: u32) -> Result<f32> {
    let (mut x, mut y) = (0.0, 0.0);
    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
    unsafe { layout.HitTestTextPosition(at, false, &mut x, &mut y, &mut metrics)? };
    Ok(x)
}

/// How wide the one-line `layout` is, spaces at its end included.
pub fn line_width(layout: &IDWriteTextLayout) -> Result<f32> {
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics)? };
    Ok(metrics.widthIncludingTrailingWhitespace)
}

/// Where a click `x` DIPs from the start of `layout`, a line of text, puts
/// the caret: a UTF-16 position. Past the middle of a letter is after it.
/// How high the click is tells nothing on one line: DirectWrite takes the
/// nearest line to a point above or below it.
pub fn hit(layout: &IDWriteTextLayout, x: f32) -> Result<u32> {
    let (mut trailing, mut inside) = (BOOL::default(), BOOL::default());
    let mut hit = DWRITE_HIT_TEST_METRICS::default();
    unsafe { layout.HitTestPoint(x, 0.0, &mut trailing, &mut inside, &mut hit)? };
    let after = match trailing.as_bool() {
        true => hit.length,
        false => 0,
    };
    Ok(hit.textPosition + after)
}

/// One line of text in `family` at `size` and `weight`, centred in the
/// height of its layout.
pub fn text_format(
    dwrite: &IDWriteFactory,
    family: Family,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    let family = family.installed(dwrite)?;
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            weight,
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
pub fn trimmed(dwrite: &IDWriteFactory, format: IDWriteTextFormat) -> Result<IDWriteTextFormat> {
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
pub use readback::{Canvas, Frame};

/// Reading frames back from the GPU, so tests can check what is drawn.
#[cfg(test)]
mod readback {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, ID3D11Device, ID3D11Texture2D,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DestroyWindow, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP,
    };
    use windows::core::{Result, w};

    use super::Surface;

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

    impl Surface {
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

    /// A bare window for a surface to draw into, destroyed after the test.
    pub struct Canvas(pub HWND);

    impl Canvas {
        pub fn new() -> Self {
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_NOREDIRECTIONBITMAP,
                    w!("STATIC"),
                    None,
                    WS_POPUP,
                    // Where it is and how big do not matter: the surface
                    // has a size of its own.
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
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
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED;

    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

    use super::*;
    use crate::test_desktop::on_hidden_desktop;

    #[test]
    fn the_picture_is_composed_above_the_window_content() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let _surface = Surface::new(canvas.0, 720, 64, 96, TRANSPARENT).expect("a surface");
            // A window has one topmost composition target; the surface's
            // should be it, so a second one is refused.
            let device: IDCompositionDevice =
                unsafe { DCompositionCreateDevice(None) }.expect("a composition device");
            let second = unsafe { device.CreateTargetForHwnd(canvas.0, true) };
            let refused = second.expect_err("the topmost target is taken");
            assert_eq!(refused.code(), DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED);
        });
    }

    #[test]
    fn its_size_in_dips_is_its_pixels_over_the_scale() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let mut surface = Surface::new(canvas.0, 720, 64, 96, TRANSPARENT).expect("a surface");
            assert_eq!(surface.size(), (720.0, 64.0));
            surface.resize(900, 300, 144).expect("resize");
            assert_eq!(surface.size(), (600.0, 200.0));
            let frame = {
                surface.begin();
                surface.end(Ok(())).expect("a frame");
                surface.read_frame().expect("read back")
            };
            assert_eq!((frame.width, frame.height), (900, 300));
            assert_eq!(
                frame.pixel(450, 150),
                [0, 0, 0, 0],
                "cleared to transparent"
            );
        });
    }

    #[test]
    fn over_the_desktop_a_frame_starts_from_the_solid_colour_over_the_glass_clear() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let first_pixel = |behind: Behind| {
                let ground = behind.ground(rgba(0.5, 0.5, 0.5, 1.0));
                let surface = Surface::new(canvas.0, 64, 64, 96, ground).expect("a surface");
                surface.begin();
                surface.end(Ok(())).expect("a frame");
                surface.read_frame().expect("read back").pixel(32, 32)
            };
            assert_eq!(first_pixel(Behind::Glass), [0, 0, 0, 0]);
            let [b, g, r, a] = first_pixel(Behind::Desktop);
            assert_eq!(a, 255, "opaque");
            assert!(
                [b, g, r].iter().all(|c| c.abs_diff(128) <= 1),
                "{b} {g} {r}"
            );
        });
    }

    #[test]
    fn behind_a_backdrop_dwm_refused_is_the_desktop() {
        assert_eq!(Behind::of(Ok(())), Behind::Glass);
        let refused = Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_INVALIDARG,
        ));
        assert_eq!(Behind::of(refused), Behind::Desktop);
    }

    /// The family `format` draws in.
    fn family_of(format: &IDWriteTextFormat) -> String {
        let mut name = vec![0; unsafe { format.GetFontFamilyNameLength() } as usize + 1];
        unsafe { format.GetFontFamilyName(&mut name) }.expect("the family's name");
        String::from_utf16_lossy(&name[..name.len() - 1])
    }

    #[test]
    fn text_is_drawn_in_the_family_wanted_where_windows_has_it_else_in_the_one_instead() {
        let dwrite: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("DirectWrite");
        let normal = DWRITE_FONT_WEIGHT_NORMAL;
        let format = |family| text_format(&dwrite, family, 14.0, normal).expect("a format");
        // Windows 11, which the tests run on, has the families wanted.
        assert_eq!(family_of(&format(TEXT_FONT)), "Segoe UI Variable Text");
        assert_eq!(
            family_of(&format(DISPLAY_FONT)),
            "Segoe UI Variable Display"
        );
        // Windows 10 has not.
        let missing = Family {
            wanted: w!("Segoe UI Variable Text, as Windows 10 has it not"),
            instead: w!("Segoe UI"),
        };
        assert_eq!(family_of(&format(missing)), "Segoe UI");
    }

    #[test]
    fn a_surface_asked_for_no_size_keeps_one_pixel_each_way() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let mut surface = Surface::new(canvas.0, 0, 0, 96, TRANSPARENT).expect("a surface");
            assert_eq!(surface.size(), (1.0, 1.0));
            surface.resize(300, 200, 96).expect("resize");
            surface.resize(0, 0, 96).expect("minimized");
            assert_eq!(surface.size(), (1.0, 1.0));
            surface.begin();
            surface.end(Ok(())).expect("a frame");
        });
    }

    #[test]
    fn a_frame_that_failed_to_draw_is_not_shown_and_the_next_one_draws() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let surface = Surface::new(canvas.0, 64, 64, 96, TRANSPARENT).expect("a surface");
            surface.begin();
            let failed = surface.end(Err(windows::core::Error::empty()));
            assert!(failed.is_err());
            surface.begin();
            surface.end(Ok(())).expect("the next frame");
        });
    }
}
