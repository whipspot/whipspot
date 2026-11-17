//! A small BMP decoder (D2, MD95) for the bitmaps inside a skin archive: untrusted input, so every
//! read is bounds-checked, arithmetic on header fields is checked in `u64`, and no pixel buffer is
//! allocated before the output size has passed the caller's budget.
//!
//! Supported: `BM` files with a 40, 52, 56, 108 or 124 byte header; 1-, 4- and 8-bit palettised
//! and 24- and 32-bit `BI_RGB`; 8-bit RLE. Everything else is `Unsupported`, which the engine
//! treats as a missing asset.

use whipspot_core::art::{ArtImage, MAX_ART_SIDE};

const FILE_HEADER: usize = 14;
const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;

/// Why a bitmap was not decoded. No variant carries input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmpError {
    /// The bytes are not a well-formed bitmap.
    Malformed,
    /// A side is over `MAX_ART_SIDE`.
    TooLarge,
    /// The decoded size is over the caller's budget.
    OverBudget,
    /// A valid variant this decoder does not handle.
    Unsupported,
}

struct Header {
    width: u32,
    height: u32,
    top_down: bool,
    bpp: u16,
    rle8: bool,
    colours_used: u32,
    palette_at: usize,
    data_at: usize,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, BmpError> {
    let s = at
        .checked_add(2)
        .and_then(|e| b.get(at..e))
        .ok_or(BmpError::Malformed)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, BmpError> {
    let s = at
        .checked_add(4)
        .and_then(|e| b.get(at..e))
        .ok_or(BmpError::Malformed)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn i32_at(b: &[u8], at: usize) -> Result<i32, BmpError> {
    u32_at(b, at).map(|v| v as i32)
}

/// The header checks shared by [`decode`] and [`dimensions`], including the side cap.
fn parse_header(b: &[u8]) -> Result<Header, BmpError> {
    if b.get(..2) != Some(b"BM") {
        return Err(BmpError::Malformed);
    }
    let data_at = u32_at(b, 10)? as usize;
    let header_size = u32_at(b, FILE_HEADER)?;
    if !matches!(header_size, 40 | 52 | 56 | 108 | 124) {
        return Err(BmpError::Unsupported);
    }
    if b.len() < FILE_HEADER + header_size as usize {
        return Err(BmpError::Malformed);
    }
    let width = i32_at(b, 18)?;
    let height = i32_at(b, 22)?;
    if width <= 0 || height == 0 || height == i32::MIN {
        return Err(BmpError::Malformed);
    }
    if u16_at(b, 26)? != 1 {
        return Err(BmpError::Malformed);
    }
    let bpp = u16_at(b, 28)?;
    let compression = u32_at(b, 30)?;
    let colours_used = u32_at(b, 46)?;
    let top_down = height < 0;
    let rle8 = match (compression, bpp) {
        (BI_RGB, 1 | 4 | 8 | 24 | 32) => false,
        (BI_RLE8, 8) => true,
        (BI_RLE8, _) => return Err(BmpError::Malformed),
        // RLE4, bit fields, 16-bit and anything unknown.
        _ => return Err(BmpError::Unsupported),
    };
    if rle8 && top_down {
        return Err(BmpError::Malformed);
    }
    let (width, height) = (width as u32, height.unsigned_abs());
    if width > MAX_ART_SIDE || height > MAX_ART_SIDE {
        return Err(BmpError::TooLarge);
    }
    Ok(Header {
        width,
        height,
        top_down,
        bpp,
        rle8,
        colours_used,
        palette_at: FILE_HEADER + header_size as usize,
        data_at,
    })
}

/// The bitmap's size from its header alone (same checks as [`decode`] up to the budget).
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), BmpError> {
    let h = parse_header(bytes)?;
    Ok((h.width, h.height))
}

/// Decodes a bitmap to RGBA8. `max_rgba_bytes` bounds `width * height * 4`; it is checked before
/// any pixel buffer exists.
pub fn decode(bytes: &[u8], max_rgba_bytes: usize) -> Result<ArtImage, BmpError> {
    let h = parse_header(bytes)?;
    let (w, ht) = (h.width as usize, h.height as usize);
    let out_bytes = u64::from(h.width) * u64::from(h.height) * 4;
    if out_bytes > max_rgba_bytes as u64 {
        return Err(BmpError::OverBudget);
    }

    let mut palette: Vec<[u8; 3]> = Vec::new();
    if h.bpp <= 8 {
        let max = 1u32 << h.bpp;
        let count = if h.colours_used == 0 {
            max
        } else {
            h.colours_used
        };
        if count > max {
            return Err(BmpError::Malformed);
        }
        let end = h.palette_at as u64 + u64::from(count) * 4;
        if end > bytes.len() as u64 {
            return Err(BmpError::Malformed);
        }
        for i in 0..count as usize {
            let p = h.palette_at + i * 4;
            palette.push([bytes[p + 2], bytes[p + 1], bytes[p]]);
        }
    }
    if h.data_at < h.palette_at || h.data_at > bytes.len() {
        return Err(BmpError::Malformed);
    }
    let data = &bytes[h.data_at..];
    let lookup = |i: u8| palette.get(usize::from(i)).copied().unwrap_or([0, 0, 0]);

    let mut rgba = vec![0u8; out_bytes as usize];
    // File row `r` (0 = first stored) lands on output row `r` when top-down, else the mirror.
    let out_row = |r: usize| if h.top_down { r } else { ht - 1 - r };

    if h.rle8 {
        rle8(data, w, ht, &mut rgba, &lookup)?;
    } else {
        let stride = (u64::from(h.width) * u64::from(h.bpp)).div_ceil(32) * 4;
        let need = stride
            .checked_mul(u64::from(h.height))
            .ok_or(BmpError::Malformed)?;
        if need > data.len() as u64 {
            return Err(BmpError::Malformed);
        }
        let stride = stride as usize;
        for r in 0..ht {
            let row = &data[r * stride..(r + 1) * stride];
            let base = out_row(r) * w * 4;
            for x in 0..w {
                let px: [u8; 3] = match h.bpp {
                    1 => lookup((row[x / 8] >> (7 - x % 8)) & 1),
                    4 => lookup((row[x / 2] >> (4 * (1 - x % 2))) & 0xF),
                    8 => lookup(row[x]),
                    24 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3]],
                    _ => [row[x * 4 + 2], row[x * 4 + 1], row[x * 4]],
                };
                let o = base + x * 4;
                rgba[o..o + 3].copy_from_slice(&px);
                rgba[o + 3] = 255;
            }
        }
    }
    ArtImage::new(h.width, h.height, rgba).ok_or(BmpError::Malformed)
}

/// Decodes RLE8 into `rgba` (bottom-up, as the format is). Pixels past the row's end are dropped;
/// pixels never written stay transparent black; a missing end-of-bitmap is accepted; a stream that
/// stops in the middle of an instruction is malformed.
fn rle8(
    data: &[u8],
    w: usize,
    ht: usize,
    rgba: &mut [u8],
    lookup: &dyn Fn(u8) -> [u8; 3],
) -> Result<(), BmpError> {
    let (mut x, mut y) = (0usize, 0usize);
    let put = |x: usize, y: usize, idx: u8, rgba: &mut [u8]| {
        if x < w && y < ht {
            let o = ((ht - 1 - y) * w + x) * 4;
            rgba[o..o + 3].copy_from_slice(&lookup(idx));
            rgba[o + 3] = 255;
        }
    };
    let mut at = 0usize;
    while at < data.len() && y < ht {
        let n = usize::from(data[at]);
        let v = *data.get(at + 1).ok_or(BmpError::Malformed)?;
        at += 2;
        if n > 0 {
            // Work per pair is bounded by the row: pixels past it are dropped, `x` still advances.
            for i in 0..n.min(w.saturating_sub(x)) {
                put(x + i, y, v, rgba);
            }
            x += n;
            continue;
        }
        match v {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                let dx = usize::from(*data.get(at).ok_or(BmpError::Malformed)?);
                let dy = usize::from(*data.get(at + 1).ok_or(BmpError::Malformed)?);
                at += 2;
                x += dx;
                y += dy;
            }
            run => {
                let run = usize::from(run);
                let run_data = data.get(at..at + run).ok_or(BmpError::Malformed)?;
                for (i, idx) in run_data.iter().take(w.saturating_sub(x)).enumerate() {
                    put(x + i, y, *idx, rgba);
                }
                x += run;
                // Runs are padded to an even byte count.
                at += run + (run & 1);
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{BmpSpec, bmp_indexed, bmp_rgb, bmp_rle8};

    const BUDGET: usize = 16 * 1024 * 1024;
    const BLACK: [u8; 3] = [0, 0, 0];
    const RED: [u8; 3] = [200, 0, 0];
    const GREEN: [u8; 3] = [0, 200, 0];
    const BLUE: [u8; 3] = [0, 0, 200];
    const WHITE: [u8; 3] = [255, 255, 255];

    fn px(img: &ArtImage, x: usize, y: usize) -> [u8; 4] {
        let o = (y * img.width() as usize + x) * 4;
        img.rgba()[o..o + 4].try_into().unwrap()
    }

    fn opaque(c: [u8; 3]) -> [u8; 4] {
        [c[0], c[1], c[2], 255]
    }

    fn err(bytes: &[u8]) -> BmpError {
        decode(bytes, BUDGET).expect_err("must not decode")
    }

    /// A valid 4x2 8-bit file to damage.
    fn base() -> BmpSpec {
        let mut s = BmpSpec::new(4, 2, 8);
        s.palette = vec![BLACK, RED, GREEN, BLUE];
        s.data = vec![0, 1, 2, 3, 3, 2, 1, 0];
        s
    }

    #[test]
    fn eight_bit_bottom_up_and_top_down() {
        let rows = vec![vec![1u8, 2, 3], vec![3u8, 2, 1]];
        let pal = [BLACK, RED, GREEN, BLUE];
        for top_down in [false, true] {
            let img = decode(&bmp_indexed(8, &pal, &rows, top_down), BUDGET).unwrap();
            assert_eq!((img.width(), img.height()), (3, 2));
            assert_eq!(px(&img, 0, 0), opaque(RED));
            assert_eq!(px(&img, 2, 0), opaque(BLUE));
            assert_eq!(px(&img, 0, 1), opaque(BLUE));
            assert_eq!(px(&img, 2, 1), opaque(RED));
        }
    }

    #[test]
    fn one_and_four_bit() {
        let rows = vec![vec![1u8, 0, 1, 1, 0, 0, 0, 1, 1, 0], vec![0u8; 10]];
        let img = decode(&bmp_indexed(1, &[BLACK, WHITE], &rows, false), BUDGET).unwrap();
        assert_eq!((img.width(), img.height()), (10, 2));
        for (x, want) in rows[0].iter().enumerate() {
            let c = if *want == 1 { WHITE } else { BLACK };
            assert_eq!(px(&img, x, 0), opaque(c), "x={x}");
            assert_eq!(px(&img, x, 1), opaque(BLACK));
        }
        let pal: Vec<[u8; 3]> = (0..16u8).map(|i| [i * 16, i, 255 - i]).collect();
        let rows = vec![vec![15u8, 0, 7, 9, 3], vec![1u8, 2, 3, 4, 5]];
        let img = decode(&bmp_indexed(4, &pal, &rows, true), BUDGET).unwrap();
        for (y, row) in rows.iter().enumerate() {
            for (x, i) in row.iter().enumerate() {
                assert_eq!(px(&img, x, y), opaque(pal[usize::from(*i)]));
            }
        }
    }

    #[test]
    fn twenty_four_bit_pads_rows() {
        // 3 pixels * 3 bytes = 9 bytes, padded to 12.
        let rows = vec![vec![RED, GREEN, BLUE], vec![BLUE, RED, GREEN]];
        for top_down in [false, true] {
            let bytes = bmp_rgb(24, &rows, top_down);
            let img = decode(&bytes, BUDGET).unwrap();
            assert_eq!((img.width(), img.height()), (3, 2));
            for (y, row) in rows.iter().enumerate() {
                for (x, c) in row.iter().enumerate() {
                    assert_eq!(px(&img, x, y), opaque(*c));
                }
            }
        }
    }

    #[test]
    fn thirty_two_bit_alpha_is_forced_opaque() {
        let rows = vec![vec![RED, GREEN], vec![BLUE, WHITE]];
        let img = decode(&bmp_rgb(32, &rows, false), BUDGET).unwrap();
        assert_eq!(px(&img, 1, 0), opaque(GREEN));
        assert_eq!(px(&img, 1, 1), opaque(WHITE));
    }

    #[test]
    fn extended_header_sizes_decode() {
        for hs in [40, 52, 56, 108, 124] {
            let mut s = base();
            s.header_size = hs;
            let img = decode(&s.bytes(), BUDGET).unwrap();
            assert_eq!(px(&img, 1, 1), opaque(RED), "header {hs}");
        }
    }

    #[test]
    fn index_past_the_palette_is_opaque_black() {
        let mut s = BmpSpec::new(2, 1, 8);
        s.palette = vec![RED, GREEN];
        s.data = vec![1, 200, 0, 0];
        let img = decode(&s.bytes(), BUDGET).unwrap();
        assert_eq!(px(&img, 0, 0), opaque(GREEN));
        assert_eq!(px(&img, 1, 0), opaque(BLACK));
    }

    #[test]
    fn colours_used_zero_means_the_full_palette() {
        let mut s = BmpSpec::new(2, 1, 1);
        s.palette = vec![RED, GREEN];
        s.colours_used = Some(0);
        s.data = vec![0b0100_0000, 0, 0, 0];
        let img = decode(&s.bytes(), BUDGET).unwrap();
        assert_eq!(px(&img, 0, 0), opaque(RED));
        assert_eq!(px(&img, 1, 0), opaque(GREEN));
    }

    fn rle_stream(with_end: bool) -> Vec<u8> {
        let mut v = vec![
            // y=0: three of index 1, two of index 2, end of line.
            3, 1, 2, 2, 0, 0, //
            // y=1: absolute run of 3 (odd, so one pad byte), delta right by 1, one of index 3.
            0, 3, 1, 2, 3, 0, 0, 2, 1, 0, 1, 3, 0, 0, //
            // y=2 skipped by a delta down by 1; y=3: seven of index 2 (two fall off the row).
            0, 2, 0, 1, 7, 2,
        ];
        if with_end {
            v.extend_from_slice(&[0, 1]);
        }
        v
    }

    #[test]
    fn rle8_with_every_escape() {
        let pal = [BLACK, RED, GREEN, BLUE];
        for with_end in [true, false] {
            let img = decode(&bmp_rle8(5, 4, &pal, &rle_stream(with_end)), BUDGET).unwrap();
            let clear = [0u8; 4];
            // Output rows run top to bottom: y=3, 2, 1, 0 of the stream.
            let want = [
                [opaque(GREEN); 5],
                [clear; 5],
                [
                    opaque(RED),
                    opaque(GREEN),
                    opaque(BLUE),
                    clear,
                    opaque(BLUE),
                ],
                [
                    opaque(RED),
                    opaque(RED),
                    opaque(RED),
                    opaque(GREEN),
                    opaque(GREEN),
                ],
            ];
            for (y, row) in want.iter().enumerate() {
                for (x, c) in row.iter().enumerate() {
                    assert_eq!(px(&img, x, y), *c, "x={x} y={y} end={with_end}");
                }
            }
        }
    }

    #[test]
    fn rle8_runs_longer_than_the_row_decode_as_before() {
        let pal = [BLACK, RED, GREEN, BLUE];
        let mut stream = vec![
            // y=0: an encoded run of 255 on a 2-px row, end of line.
            255, 1, 0, 0, //
            // y=1: an absolute run of 200 (even, no pad), end of line.
            0, 200,
        ];
        stream.extend((0..200u32).map(|i| if i < 2 { 2 } else { 3 }));
        stream.extend_from_slice(&[0, 0]);
        // y=2: two of index 3, end of bitmap.
        stream.extend_from_slice(&[2, 3, 0, 1]);
        let img = decode(&bmp_rle8(2, 3, &pal, &stream), BUDGET).unwrap();
        let want: Vec<u8> = [BLUE, BLUE, GREEN, GREEN, RED, RED]
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect();
        assert_eq!(img.rgba(), &want[..]);
    }

    #[test]
    fn rle8_instruction_cut_short_is_malformed() {
        let pal = [BLACK, RED];
        for tail in [&[3u8][..], &[0, 2, 1][..], &[0, 4, 1, 1][..]] {
            assert_eq!(
                err(&bmp_rle8(5, 2, &pal, tail)),
                BmpError::Malformed,
                "{tail:?}"
            );
        }
    }

    #[test]
    fn header_errors() {
        assert_eq!(err(&[]), BmpError::Malformed);
        assert_eq!(err(b"BM"), BmpError::Malformed);
        let mut bytes = base().bytes();
        bytes[0] = b'X';
        assert_eq!(err(&bytes), BmpError::Malformed);
        for hs in [12, 64, 41] {
            let mut s = base();
            s.header_size = hs;
            assert_eq!(err(&s.bytes()), BmpError::Unsupported, "header {hs}");
        }
        // Header says 40 bytes but the file ends first.
        assert_eq!(err(&base().bytes()[..30]), BmpError::Malformed);

        let mut s = base();
        s.width = 0;
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        let mut s = base();
        s.width = -4;
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        for h in [0, i32::MIN] {
            let mut s = base();
            s.height = h;
            assert_eq!(err(&s.bytes()), BmpError::Malformed, "height {h}");
        }
        let mut s = base();
        s.planes = 2;
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
    }

    #[test]
    fn unsupported_variants() {
        let mut s = base();
        s.bpp = 16;
        assert_eq!(err(&s.bytes()), BmpError::Unsupported);
        for c in [2, 3, 4, 5, 99] {
            let mut s = base();
            s.compression = c;
            assert_eq!(err(&s.bytes()), BmpError::Unsupported, "compression {c}");
        }
        let mut s = BmpSpec::new(2, 1, 32);
        s.compression = 3;
        assert_eq!(err(&s.bytes()), BmpError::Unsupported);
    }

    #[test]
    fn rle_needs_eight_bits_and_a_bottom_up_height() {
        let mut s = base();
        s.compression = 1;
        s.bpp = 24;
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        let mut s = base();
        s.compression = 1;
        s.height = -2;
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
    }

    #[test]
    fn sides_over_the_cap_are_too_large() {
        for (w, h) in [(2049, 1), (1, 2049), (1, -2049), (i32::MAX, 1)] {
            let s = BmpSpec::new(w, h, 8);
            assert_eq!(err(&s.bytes()), BmpError::TooLarge, "{w}x{h}");
            assert_eq!(dimensions(&s.bytes()), Err(BmpError::TooLarge));
        }
    }

    #[test]
    fn palette_and_pixels_must_lie_in_the_input() {
        let mut s = base();
        s.colours_used = Some(257);
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        let mut s = BmpSpec::new(8, 1, 1);
        s.palette = vec![RED, GREEN, BLUE];
        s.colours_used = Some(3);
        s.data = vec![0, 0, 0, 0];
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        // Palette cut off.
        let bytes = base().bytes();
        assert_eq!(err(&bytes[..14 + 40 + 6]), BmpError::Malformed);
        // Pixels cut off, and a data offset past the end.
        assert_eq!(err(&bytes[..bytes.len() - 1]), BmpError::Malformed);
        let mut s = base();
        s.data_offset = Some(10_000);
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
        let mut s = base();
        s.data_offset = Some(20);
        assert_eq!(err(&s.bytes()), BmpError::Malformed);
    }

    #[test]
    fn budget_is_checked_before_pixel_data() {
        let header_only = BmpSpec::new(2048, 2048, 1).bytes();
        assert_eq!(
            decode(&header_only, BUDGET - 1).err(),
            Some(BmpError::OverBudget)
        );
        assert_eq!(
            decode(&header_only, BUDGET).err(),
            Some(BmpError::Malformed)
        );
        assert_eq!(dimensions(&header_only), Ok((2048, 2048)));
        // A valid file over a small budget.
        let bytes = base().bytes();
        assert_eq!(
            decode(&bytes, 4 * 2 * 4 - 1).err(),
            Some(BmpError::OverBudget)
        );
        assert!(decode(&bytes, 4 * 2 * 4).is_ok());
        assert_eq!(dimensions(&bytes), Ok((4, 2)));
    }

    #[test]
    fn every_prefix_is_handled() {
        let eight = base().bytes();
        let rle = bmp_rle8(5, 4, &[BLACK, RED, GREEN, BLUE], &rle_stream(true));
        let rgb = bmp_rgb(24, &[vec![RED, GREEN, BLUE]], false);
        for bytes in [eight, rle, rgb] {
            for n in 0..=bytes.len() {
                let _ = decode(&bytes[..n], BUDGET);
                let _ = dimensions(&bytes[..n]);
            }
        }
    }
}
