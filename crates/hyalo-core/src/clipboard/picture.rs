//! Bitmaps on the clipboard (`CF_DIB`: a `BITMAPINFOHEADER` or a later
//! header, then the rows) read into pixels and written back, and the
//! small picture a row shows of one.

/// An image's pixels: blue, green, red, alpha, top row first, rows
/// unpadded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// The size of a `BITMAPINFOHEADER`, the oldest header a `CF_DIB` has.
const INFO: usize = 40;

/// `BI_RGB`, `BI_BITFIELDS` and `BI_ALPHABITFIELDS`: rows as they are,
/// or with masks saying where each colour is.
const RGB: u32 = 0;
const BITFIELDS: u32 = 3;
const ALPHABITFIELDS: u32 = 6;

/// Where blue, green, red and alpha are in a 32-bit pixel when no masks
/// say otherwise.
const MASKS: [u32; 4] = [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000];

/// The pixels of `dib`, a `CF_DIB`, with alpha straight; `None` for a
/// bitmap that does not read, or one of a palette or 16 bits, which
/// Windows hands over only from the oldest apps.
pub fn read(dib: &[u8]) -> Option<Pixels> {
    let layout = layout(dib)?;
    let size = layout.stride.checked_mul(layout.rows)?;
    let data = dib.get(layout.start..layout.start.checked_add(size)?)?;
    Some(Pixels {
        width: layout.width as u32,
        height: layout.rows as u32,
        bgra: opaque_unless_said(layout.pixels(data)),
    })
}

/// Where a bitmap's rows are, and how to read each pixel.
struct Layout {
    width: usize,
    rows: usize,
    /// The top row comes first (a height below 0), not the bottom one.
    top_down: bool,
    /// The bytes of a pixel: 3, or 4.
    bytes: usize,
    /// The bytes from one row to the next, padded to four.
    stride: usize,
    masks: [u32; 4],
    /// Where the rows start.
    start: usize,
}

/// `N` little-endian words of `bytes` from `at` on.
fn words<const N: usize>(bytes: &[u8], at: usize) -> Option<[u32; N]> {
    let (words, _) = bytes.get(at..at.checked_add(N * 4)?)?.as_chunks::<4>();
    Some(std::array::from_fn(|index| {
        u32::from_le_bytes(words[index])
    }))
}

/// How the bitmap `dib` lies, if it is one of 24 bits as they are or of
/// 32 bits as they are or masked, with a header no older than
/// `BITMAPINFOHEADER`; a colour table, if any, is stepped over.
fn layout(dib: &[u8]) -> Option<Layout> {
    let [header, width, height, planes_and_bits, compression] = words::<5>(dib, 0)?;
    let [used] = words::<1>(dib, 32)?;
    let (header, width, height) = (header as usize, width as i32, height as i32);
    let bits = planes_and_bits >> 16;
    let known = matches!((bits, compression), (32, _) | (24, RGB));
    if !known || header < INFO || width <= 0 || height == 0 {
        return None;
    }
    let (masks, after) = masks(dib, header, compression)?;
    let width = width as usize;
    Some(Layout {
        width,
        rows: height.unsigned_abs() as usize,
        top_down: height.is_negative(),
        bytes: bits as usize / 8,
        stride: (width * bits as usize).div_ceil(32) * 4,
        masks,
        start: header + after + used as usize * 4,
    })
}

impl Layout {
    /// The pixels of `data`, the rows, top row first.
    fn pixels(&self, data: &[u8]) -> Vec<u8> {
        let mut bgra = Vec::new();
        for row in 0..self.rows {
            let row = if self.top_down {
                row
            } else {
                self.rows - 1 - row
            };
            let line = &data[row * self.stride..][..self.width * self.bytes];
            bgra.extend(
                line.chunks_exact(self.bytes)
                    .flat_map(|one| self.pixel(one)),
            );
        }
        bgra
    }

    /// One pixel's blue, green, red and alpha: 24 bits are opaque, 32
    /// are where the masks say.
    fn pixel(&self, one: &[u8]) -> [u8; 4] {
        match *one {
            [blue, green, red] => [blue, green, red, u8::MAX],
            _ => masked(one, self.masks),
        }
    }
}

/// `bgra` as it is, unless its alpha is 0 throughout: a 32-bit bitmap
/// whose alpha is all 0 says nothing of alpha, and is opaque.
fn opaque_unless_said(mut bgra: Vec<u8>) -> Vec<u8> {
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    if pixels.iter().all(|one| one[3] == 0) {
        pixels.iter_mut().for_each(|one| one[3] = u8::MAX);
    }
    bgra
}

/// Where each colour is in a 32-bit pixel of `dib`, each mask 8 bits,
/// and how many bytes of masks follow the header; `None` for a
/// compression that is not masks or rows as they are.
fn masks(dib: &[u8], header: usize, compression: u32) -> Option<([u32; 4], usize)> {
    let count = match compression {
        RGB => return Some((MASKS, 0)),
        BITFIELDS => 3,
        ALPHABITFIELDS => 4,
        _ => return None,
    };
    // Red, green, blue, then alpha: in a later header, or following the
    // oldest one. A bitmap that reads has as many bytes after them.
    let [red, green, blue, last] = words::<4>(dib, INFO)?;
    let alpha = if count == 4 || header >= INFO + 16 {
        last
    } else {
        0
    };
    let after = usize::from(header == INFO) * count * 4;
    let masks = [blue, green, red, alpha];
    let fits = |mask: u32| mask == 0 || mask >> mask.trailing_zeros() == 0xFF;
    masks
        .iter()
        .all(|&mask| fits(mask))
        .then_some((masks, after))
}

/// A 32-bit pixel's blue, green, red and alpha, where `masks` says.
fn masked(four: &[u8], masks: [u32; 4]) -> [u8; 4] {
    let pixel = u32::from_le_bytes([four[0], four[1], four[2], four[3]]);
    masks.map(|mask| match mask {
        0 => 0,
        mask => ((pixel & mask) >> mask.trailing_zeros()) as u8,
    })
}

/// `pixels` as a `CF_DIB`: the oldest header, 32 bits a pixel, alpha in
/// the top byte, the bottom row first, as most apps read one.
pub fn write(pixels: &Pixels) -> Vec<u8> {
    let size = pixels.bgra.len() as u32;
    let mut dib = Vec::new();
    let header = [
        INFO as u32,
        pixels.width,
        pixels.height,
        1 | 32 << 16,
        RGB,
        size,
        0,
        0,
        0,
        0,
    ];
    dib.extend(header.iter().flat_map(|word| word.to_le_bytes()));
    let rows = pixels.bgra.chunks_exact(pixels.width as usize * 4).rev();
    rows.for_each(|row| dib.extend_from_slice(row));
    dib
}

/// A row's picture of `pixels`: `side` pixels square, the image shrunk
/// to fit inside with its shape kept (never grown), in the middle,
/// clear around it; each pixel the average of those it covers, its
/// colour premultiplied by its alpha, as Direct2D draws it.
pub fn thumbnail(pixels: &Pixels, side: u32) -> Pixels {
    let (width, height) = (pixels.width as usize, pixels.height as usize);
    let scale = (side as f64 / width.max(height) as f64).min(1.0);
    let fit = |length: usize| ((length as f64 * scale).round() as usize).max(1);
    let (fitted_width, fitted_height) = (fit(width), fit(height));
    let side = side as usize;
    let (left, top) = ((side - fitted_width) / 2, (side - fitted_height) / 2);
    let mut bgra = vec![0; side * side * 4];
    for y in 0..fitted_height {
        let rows = covered(y, fitted_height, height);
        for x in 0..fitted_width {
            let at = ((top + y) * side + left + x) * 4;
            let average = average(pixels, covered(x, fitted_width, width), rows.clone());
            bgra[at..at + 4].copy_from_slice(&average);
        }
    }
    Pixels {
        width: side as u32,
        height: side as u32,
        bgra,
    }
}

/// The pixels of a side `from` long that pixel `at` of it shrunk to
/// `to` covers: at least one.
fn covered(at: usize, to: usize, from: usize) -> std::ops::Range<usize> {
    let start = at * from / to;
    start..(at + 1) * from / to
}

/// The average of `pixels` over `columns` and `rows`, premultiplied.
fn average(
    pixels: &Pixels,
    columns: std::ops::Range<usize>,
    rows: std::ops::Range<usize>,
) -> [u8; 4] {
    let mut sums = [0u64; 4];
    let count = (columns.len() * rows.len()) as u64;
    for y in rows {
        for x in columns.clone() {
            let at = (y * pixels.width as usize + x) * 4;
            let one = &pixels.bgra[at..at + 4];
            let alpha = u64::from(one[3]);
            for channel in 0..3 {
                sums[channel] += u64::from(one[channel]) * alpha / 255;
            }
            sums[3] += alpha;
        }
    }
    sums.map(|sum| ((sum + count / 2) / count) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 by 2 image: blue, green / red, half-clear white.
    fn four() -> Pixels {
        Pixels {
            width: 2,
            height: 2,
            bgra: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
            ],
        }
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        let dib = write(&four());
        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(&dib[..4], [40, 0, 0, 0]);
        assert_eq!(
            &dib[8..12],
            [2, 0, 0, 0],
            "bottom row first: a height above 0"
        );
        assert_eq!(&dib[12..16], [1, 0, 32, 0], "one plane, 32 bits");
        assert_eq!(&dib[16..20], [0; 4], "rows as they are");
        assert_eq!(&dib[20..24], [16, 0, 0, 0], "the bytes of the rows");
        assert_eq!(&dib[24..40], [0; 16], "no resolution, no colour table");
        assert_eq!(
            &dib[40..44],
            [0, 0, 255, 255],
            "the bottom row's first: red"
        );
        assert_eq!(read(&dib), Some(four()));
    }

    /// The oldest header for a `width` by `height` bitmap of `bits`.
    fn info(width: i32, height: i32, bits: u32, compression: u32) -> Vec<u8> {
        let header = [
            40,
            width as u32,
            height as u32,
            1 | bits << 16,
            compression,
            0,
            0,
            0,
            0,
            0,
        ];
        header.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn rows_of_24_bits_are_padded_to_four_bytes_and_opaque() {
        let mut dib = info(1, -2, 24, RGB);
        dib.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        let read = read(&dib).expect("a bitmap");
        assert_eq!((read.width, read.height), (1, 2));
        assert_eq!(read.bgra, [1, 2, 3, 255, 4, 5, 6, 255], "top row first");
    }

    #[test]
    fn a_32_bit_bitmap_with_no_alpha_at_all_is_opaque_and_one_with_some_keeps_it() {
        let mut none = info(2, 1, 32, RGB);
        none.extend([1, 2, 3, 0, 4, 5, 6, 0]);
        assert_eq!(
            read(&none).expect("a bitmap").bgra,
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
        let mut some = info(2, 1, 32, RGB);
        some.extend([1, 2, 3, 0, 4, 5, 6, 9]);
        assert_eq!(
            read(&some).expect("a bitmap").bgra,
            [1, 2, 3, 0, 4, 5, 6, 9]
        );
    }

    #[test]
    fn masks_after_the_oldest_header_or_inside_a_later_one_say_where_colours_are() {
        // Red in the low byte, blue in the third, as some apps write it.
        let swapped = [0xFFu32, 0xFF00, 0xFF_0000];
        let mut after = info(1, 1, 32, BITFIELDS);
        after.extend(swapped.iter().flat_map(|mask| mask.to_le_bytes()));
        after.extend([10, 20, 30, 40]);
        assert_eq!(read(&after).expect("a bitmap").bgra, [30, 20, 10, 255]);
        let mut later = info(1, 1, 32, BITFIELDS);
        later[0] = 124;
        let masks = [0xFF_0000u32, 0xFF00, 0xFF, 0xFF00_0000];
        later.extend(masks.iter().flat_map(|mask| mask.to_le_bytes()));
        later.resize(124, 0);
        later.extend([10, 20, 30, 40]);
        assert_eq!(read(&later).expect("a bitmap").bgra, [10, 20, 30, 40]);
        let mut alpha = info(1, 1, 32, ALPHABITFIELDS);
        let four = [0xFF_0000u32, 0xFF00, 0xFF, 0xFF00_0000];
        alpha.extend(four.iter().flat_map(|mask| mask.to_le_bytes()));
        alpha.extend([10, 20, 30, 40]);
        assert_eq!(read(&alpha).expect("a bitmap").bgra, [10, 20, 30, 40]);
    }

    #[test]
    fn a_header_with_four_masks_says_where_alpha_is() {
        let mut v3 = info(1, 1, 32, BITFIELDS);
        v3[0] = 56;
        let masks = [0xFF_0000u32, 0xFF00, 0xFF, 0xFF00_0000];
        v3.extend(masks.iter().flat_map(|mask| mask.to_le_bytes()));
        v3.extend([10, 20, 30, 40]);
        assert_eq!(read(&v3).expect("a bitmap").bgra, [10, 20, 30, 40]);
    }

    #[test]
    fn a_colour_table_is_stepped_over() {
        let mut dib = info(1, 1, 32, RGB);
        dib[32] = 2;
        dib.extend([0; 8]);
        dib.extend([7, 8, 9, 255]);
        assert_eq!(read(&dib).expect("a bitmap").bgra, [7, 8, 9, 255]);
    }

    #[test]
    fn what_does_not_read_is_nothing() {
        let mut short = info(2, 2, 32, RGB);
        short.extend([0; 15]);
        assert_eq!(read(&short), None, "a byte short");
        let mut palette = info(1, 1, 8, RGB);
        palette.extend([0; 4]);
        assert_eq!(read(&palette), None);
        let mut sixteen = info(1, 1, 16, RGB);
        sixteen.extend([0; 4]);
        assert_eq!(read(&sixteen), None);
        let mut packed = info(1, 1, 32, 1);
        packed.extend([0; 4]);
        assert_eq!(read(&packed), None, "run-length");
        let mut odd = info(1, 1, 32, BITFIELDS);
        odd.extend(
            [0xF0u32, 0xFF00, 0xFF_0000]
                .iter()
                .flat_map(|mask| mask.to_le_bytes()),
        );
        odd.extend([0; 4]);
        assert_eq!(read(&odd), None, "a mask of 4 bits");
        let mut small = info(1, 1, 32, RGB);
        small[0] = 12;
        assert_eq!(read(&small), None, "a core header");
        assert_eq!(read(&info(0, 1, 32, RGB)), None);
        assert_eq!(read(&info(1, 0, 32, RGB)), None);
        assert_eq!(read(&[]), None);
    }

    #[test]
    fn a_thumbnail_keeps_the_shape_in_the_middle_of_a_clear_square() {
        // 4 wide, 2 high: left half red, right half blue.
        let red = [0, 0, 255, 255];
        let blue = [255, 0, 0, 255];
        let row = [red, red, blue, blue].concat();
        let wide = Pixels {
            width: 4,
            height: 2,
            bgra: [row.clone(), row].concat(),
        };
        let clear = [0; 4];
        let square = thumbnail(&wide, 4);
        let row = [red, red, blue, blue].concat();
        let rows = [clear.repeat(4), row.clone(), row, clear.repeat(4)].concat();
        assert_eq!(square.bgra, rows, "4 wide, 2 high, in the middle");
        let small = thumbnail(&wide, 2);
        assert_eq!((small.width, small.height), (2, 2));
        assert_eq!(
            small.bgra,
            [red, blue, clear, clear].concat(),
            "2 wide, 1 high"
        );
        let line = Pixels {
            width: 100,
            height: 1,
            bgra: red.repeat(100),
        };
        let thin = thumbnail(&line, 4);
        let second = &thin.bgra[4 * 4..8 * 4];
        assert_eq!(
            second,
            red.repeat(4),
            "a pixel high at least, in the middle"
        );
        let big = thumbnail(&wide, 8);
        assert_eq!(
            &big.bgra[(3 * 8 + 2) * 4..(3 * 8 + 3) * 4],
            red,
            "never grown"
        );
        assert_eq!(&big.bgra[(3 * 8 + 1) * 4..(3 * 8 + 2) * 4], clear);
    }

    #[test]
    fn a_thumbnail_averages_what_it_covers_premultiplied() {
        let one = thumbnail(&four(), 1);
        // Blue 255, green 255, red 255 and white at half: each colour is
        // (255 + 128) / 4 premultiplied, alpha (3 × 255 + 128) / 4.
        assert_eq!(one.bgra, [96, 96, 96, 223]);
    }
}
