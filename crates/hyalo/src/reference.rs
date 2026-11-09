//! Reference images: what a frame looked like when a person last checked
//! it, kept as PNG files in `crates/hyalo/reference`. A test compares a
//! fresh frame with its reference pixel by pixel, so any change to what is
//! drawn, or where, fails it.
//!
//! The PNG holds the frame's premultiplied colours as they are, so an
//! image viewer shows translucent parts a little dark. After a deliberate
//! change to the look, check the new frames with your own eyes, then
//! accept them with `HYALO_BLESS=1 cargo test`.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use crate::render::Frame;

/// How far a channel may drift (out of 255) before its pixel counts as
/// changed: room for driver rounding, far below anything a person sees.
const TOLERANCE: u8 = 3;

/// Fails unless `frame` matches the reference image `name`. A failure
/// writes the frame to `target/frames/<name>.png` for a look.
pub fn assert_looks_like(name: &str, frame: &Frame) {
    let file = format!("{name}.png");
    let reference = manifest_dir().join("reference").join(&file);
    let actual = rgba(frame);
    let size = (frame.width, frame.height);
    if std::env::var_os("HYALO_BLESS").is_some() {
        write(&reference, size, &actual);
        return;
    }
    let (expected_size, expected) = read(&reference);
    let changed = changed_pixels(&actual, &expected);
    if size != expected_size || changed > 0 {
        let seen = manifest_dir().join("../../target/frames").join(&file);
        write(&seen, size, &actual);
        panic!(
            "{name}: {changed} pixels differ, frame {size:?} against {expected_size:?}; \
             the frame is in {}",
            seen.display()
        );
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The frame as RGBA rows, read through `Frame::pixel`.
fn rgba(frame: &Frame) -> Vec<u8> {
    let points = (0..frame.height).flat_map(|y| (0..frame.width).map(move |x| (x, y)));
    points
        .flat_map(|(x, y)| {
            let [blue, green, red, alpha] = frame.pixel(x, y);
            [red, green, blue, alpha]
        })
        .collect()
}

/// Pixels with a channel further than `TOLERANCE` from the other image;
/// every pixel counts as changed when the sizes differ.
fn changed_pixels(one: &[u8], other: &[u8]) -> usize {
    if one.len() != other.len() {
        return one.len().max(other.len()) / 4;
    }
    let pixels = one.chunks(4).zip(other.chunks(4));
    pixels
        .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > TOLERANCE))
        .count()
}

fn read(path: &Path) -> ((usize, usize), Vec<u8>) {
    let file = File::open(path).unwrap_or_else(|error| {
        panic!(
            "no reference {} ({error}); make it with HYALO_BLESS=1",
            path.display()
        )
    });
    let mut reader = png::Decoder::new(BufReader::new(file))
        .read_info()
        .expect("a PNG");
    let mut rgba = vec![0; reader.output_buffer_size().expect("a size that fits")];
    let info = reader.next_frame(&mut rgba).expect("the image");
    rgba.truncate(info.buffer_size());
    ((info.width as usize, info.height as usize), rgba)
}

fn write(path: &Path, (width, height): (usize, usize), rgba: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("make the folder");
    let file = File::create(path).expect("create the PNG");
    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(rgba).expect("the pixels");
}
