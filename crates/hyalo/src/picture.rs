//! Pictures as PNG, through the Windows Imaging Component: how the
//! clipboard history keeps a copied image small, and makes it pixels
//! again to put back. COM must be initialized on the calling thread.

use hyalo_core::clipboard::picture::Pixels;
use windows::Win32::Foundation::E_OUTOFMEMORY;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapEncoder, IWICFormatConverter, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{Error, Result};

/// The imaging component's factory.
fn factory() -> Result<IWICImagingFactory> {
    // SAFETY: COM is initialized on this thread, the caller's promise.
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

/// `pixels`, alpha straight, as PNG.
pub fn encode(pixels: &Pixels) -> Result<Vec<u8>> {
    // SAFETY: a plain call; the stream is ours.
    let stream = unsafe { SHCreateMemStream(None) }.ok_or_else(|| Error::from(E_OUTOFMEMORY))?;
    // SAFETY: COM calls on objects made here.
    unsafe {
        let encoder = factory()?.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        write_frame(&encoder, pixels)?;
        encoder.Commit()?;
    }
    read_all(&stream)
}

/// Writes `pixels` as the one frame of `encoder`.
fn write_frame(encoder: &IWICBitmapEncoder, pixels: &Pixels) -> Result<()> {
    let (mut frame, mut options) = (None, None);
    // SAFETY: COM calls on objects made here; the pixels are `width` by
    // `height` of 4 bytes, as the frame is told.
    unsafe {
        encoder.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.ok_or_else(|| Error::from(E_OUTOFMEMORY))?;
        frame.Initialize(options.as_ref())?;
        frame.SetSize(pixels.width, pixels.height)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(pixels.height, pixels.width * 4, &pixels.bgra)?;
        frame.Commit()
    }
}

/// Everything `stream` holds, from its start.
fn read_all(stream: &IStream) -> Result<Vec<u8>> {
    let mut size = 0_u64;
    // SAFETY: COM calls on a live stream; the buffer is as long as the
    // count the read is given.
    unsafe {
        stream.Seek(0, STREAM_SEEK_END, Some(&mut size))?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0_u8; size as usize];
        let mut read = 0_u32;
        let length = bytes.len() as u32;
        stream
            .Read(bytes.as_mut_ptr().cast(), length, Some(&mut read))
            .ok()?;
        bytes.truncate(read as usize);
        Ok(bytes)
    }
}

/// The pixels of the PNG `png`, alpha straight.
pub fn decode(png: &[u8]) -> Result<Pixels> {
    let factory = factory()?;
    let converter = converted(&factory, png)?;
    let (mut width, mut height) = (0, 0);
    // SAFETY: COM calls on a live converter; the buffer holds `width` by
    // `height` pixels of 4 bytes, as the copy is told.
    unsafe {
        converter.GetSize(&mut width, &mut height)?;
        let mut bgra = vec![0_u8; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut bgra)?;
        Ok(Pixels {
            width,
            height,
            bgra,
        })
    }
}

/// The first frame of the PNG `png`, as 32-bit BGRA.
fn converted(factory: &IWICImagingFactory, png: &[u8]) -> Result<IWICFormatConverter> {
    // SAFETY: COM calls on objects made here; the stream reads `png`,
    // which the converter does not outlive: it is read at once.
    unsafe {
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(png)?;
        let cache = WICDecodeMetadataCacheOnDemand;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), cache)?;
        let converter = factory.CreateFormatConverter()?;
        let (dither, palette) = (WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom);
        let format = &GUID_WICPixelFormat32bppBGRA;
        converter.Initialize(&decoder.GetFrame(0)?, format, dither, None, 0.0, palette)?;
        Ok(converter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::Com;

    #[test]
    fn pixels_made_png_come_back_the_same_and_smaller_when_plain() {
        let _com = Com::init().expect("COM");
        let (width, height) = (300_u32, 200_u32);
        let mut bgra = Vec::new();
        for y in 0..height {
            for x in 0..width {
                bgra.extend([x as u8, y as u8, 200, if x < 10 { 128 } else { 255 }]);
            }
        }
        let pixels = Pixels {
            width,
            height,
            bgra,
        };
        let png = encode(&pixels).expect("encode");
        assert_eq!(&png[1..4], b"PNG");
        assert!(png.len() < pixels.bgra.len() / 4, "{} bytes", png.len());
        assert_eq!(decode(&png).expect("decode"), pixels);
    }

    #[test]
    fn what_is_no_png_does_not_decode() {
        let _com = Com::init().expect("COM");
        assert!(decode(b"not a picture").is_err());
        assert!(decode(&[]).is_err());
    }
}
