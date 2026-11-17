//! Image decoding under limits (MD64): JPEG and PNG only, chosen by magic bytes, sides checked
//! before any output buffer is allocated.

use std::io::Cursor;
use std::sync::Arc;

use whipspot_core::art::{ArtError, ArtImage, MAX_ART_SIDE};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// png's own working buffers; it does not bound the caller's output buffer (checked below).
const PNG_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// The largest RGBA8 output allowed.
const MAX_RGBA_BYTES: usize = MAX_ART_SIDE as usize * MAX_ART_SIDE as usize * 4;

/// Decodes on a blocking thread; a panic inside a decoder is `Undecodable`.
pub(crate) async fn decode(bytes: Arc<Vec<u8>>) -> Result<ArtImage, ArtError> {
    tokio::task::spawn_blocking(move || decode_sync(&bytes))
        .await
        .unwrap_or(Err(ArtError::Undecodable))
}

/// Sniffs the format by magic bytes; anything else is `Undecodable`.
pub(crate) fn decode_sync(bytes: &[u8]) -> Result<ArtImage, ArtError> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(&PNG_SIGNATURE) {
        decode_png(bytes)
    } else {
        Err(ArtError::Undecodable)
    }
}

fn decode_jpeg(bytes: &[u8]) -> Result<ArtImage, ArtError> {
    // The decoder's own limit can only say "format error", so it is opened to the format's
    // ceiling (u16) and the real limit is checked here, on the header, before any output
    // buffer exists: an over-limit side is TooLarge, not Undecodable.
    let ceiling = usize::from(u16::MAX);
    let options = DecoderOptions::default()
        .set_max_width(ceiling)
        .set_max_height(ceiling)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|_| ArtError::Undecodable)?;
    let (w, h) = decoder.dimensions().ok_or(ArtError::Undecodable)?;
    if w == 0 || h == 0 {
        return Err(ArtError::Undecodable);
    }
    if w > MAX_ART_SIDE as usize || h > MAX_ART_SIDE as usize {
        return Err(ArtError::TooLarge);
    }
    if decoder.output_colorspace() != Some(ColorSpace::RGBA) {
        return Err(ArtError::Undecodable);
    }
    let len = decoder.output_buffer_size().ok_or(ArtError::Undecodable)?;
    if len != w * h * 4 || len > MAX_RGBA_BYTES {
        return Err(ArtError::Undecodable);
    }
    let mut out = vec![0u8; len];
    decoder
        .decode_into(&mut out)
        .map_err(|_| ArtError::Undecodable)?;
    ArtImage::new(w as u32, h as u32, out).ok_or(ArtError::Undecodable)
}

fn decode_png(bytes: &[u8]) -> Result<ArtImage, ArtError> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: PNG_LIMIT_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| ArtError::Undecodable)?;
    // png's Limits do not bound the output buffer the caller allocates: check the sides from
    // the header first.
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 {
        return Err(ArtError::Undecodable);
    }
    if w > MAX_ART_SIDE || h > MAX_ART_SIDE {
        return Err(ArtError::TooLarge);
    }
    let size = reader.output_buffer_size().ok_or(ArtError::TooLarge)?;
    if size > MAX_RGBA_BYTES {
        return Err(ArtError::TooLarge);
    }
    let mut buf = vec![0u8; size];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|_| ArtError::Undecodable)?;
    if out.width != w || out.height != h || out.bit_depth != png::BitDepth::Eight {
        return Err(ArtError::Undecodable);
    }
    let pixels = w as usize * h as usize;
    let src = buf.get(..out.buffer_size()).ok_or(ArtError::Undecodable)?;
    let mut rgba = Vec::with_capacity(pixels * 4);
    match out.color_type {
        png::ColorType::Grayscale if src.len() >= pixels => {
            for &g in &src[..pixels] {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha if src.len() >= pixels * 2 => {
            for p in src[..pixels * 2].chunks_exact(2) {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        png::ColorType::Rgb if src.len() >= pixels * 3 => {
            for p in src[..pixels * 3].chunks_exact(3) {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::Rgba if src.len() >= pixels * 4 => {
            rgba.extend_from_slice(&src[..pixels * 4]);
        }
        _ => return Err(ArtError::Undecodable),
    }
    ArtImage::new(w, h, rgba).ok_or(ArtError::Undecodable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(color: jpeg_encoder::ColorType, w: u16, h: u16, px: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(px, w, h, color)
            .unwrap();
        out
    }

    fn png_bytes(
        color: png::ColorType,
        depth: png::BitDepth,
        w: u32,
        h: u32,
        data: &[u8],
        palette: Option<(&[u8], &[u8])>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(depth);
            if let Some((plte, trns)) = palette {
                enc.set_palette(plte.to_vec());
                enc.set_trns(trns.to_vec());
            }
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn d1_jpeg_rgb_and_grayscale_give_rgba_of_the_right_size() {
        let rgb: Vec<u8> = (0..16 * 8).flat_map(|_| [200u8, 30, 30]).collect();
        let img = decode_sync(&jpeg(jpeg_encoder::ColorType::Rgb, 16, 8, &rgb)).unwrap();
        assert_eq!((img.width(), img.height()), (16, 8));
        assert_eq!(img.rgba().len(), 16 * 8 * 4);
        let p = &img.rgba()[..4];
        assert!(p[0] > 150 && p[1] < 80 && p[2] < 80 && p[3] == 255, "{p:?}");

        let gray = vec![128u8; 8 * 8];
        let img = decode_sync(&jpeg(jpeg_encoder::ColorType::Luma, 8, 8, &gray)).unwrap();
        assert_eq!((img.width(), img.height()), (8, 8));
        assert_eq!(img.rgba().len(), 8 * 8 * 4);
        let p = &img.rgba()[..4];
        assert!((i32::from(p[0]) - 128).abs() < 8 && p[0] == p[1] && p[1] == p[2] && p[3] == 255);
    }

    #[test]
    fn d2_png_kinds_expand_to_rgba8() {
        use png::{BitDepth::*, ColorType::*};
        let img = decode_sync(&png_bytes(Grayscale, Eight, 2, 1, &[10, 200], None)).unwrap();
        assert_eq!(img.rgba(), [10, 10, 10, 255, 200, 200, 200, 255]);
        let img = decode_sync(&png_bytes(
            GrayscaleAlpha,
            Eight,
            2,
            1,
            &[10, 99, 200, 7],
            None,
        ))
        .unwrap();
        assert_eq!(img.rgba(), [10, 10, 10, 99, 200, 200, 200, 7]);
        let img = decode_sync(&png_bytes(Rgb, Eight, 2, 1, &[1, 2, 3, 4, 5, 6], None)).unwrap();
        assert_eq!(img.rgba(), [1, 2, 3, 255, 4, 5, 6, 255]);
        let img = decode_sync(&png_bytes(
            Rgba,
            Eight,
            1,
            2,
            &[1, 2, 3, 4, 5, 6, 7, 8],
            None,
        ))
        .unwrap();
        assert_eq!(img.rgba(), [1, 2, 3, 4, 5, 6, 7, 8]);
        // Palette with a tRNS entry: index 0 half transparent, index 1 opaque.
        let img = decode_sync(&png_bytes(
            Indexed,
            Eight,
            2,
            1,
            &[0, 1],
            Some((&[9, 8, 7, 1, 2, 3], &[128])),
        ))
        .unwrap();
        assert_eq!(img.rgba(), [9, 8, 7, 128, 1, 2, 3, 255]);
        // 16-bit samples are reduced to their high byte.
        let img = decode_sync(&png_bytes(
            Rgb,
            Sixteen,
            1,
            1,
            &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc],
            None,
        ))
        .unwrap();
        assert_eq!(img.rgba(), [0x12, 0x56, 0x9a, 255]);
    }

    #[test]
    fn d3_truncations_and_junk_never_panic() {
        let rgb: Vec<u8> = (0..8 * 8).flat_map(|i| [i as u8, 90, 200]).collect();
        let j = jpeg(jpeg_encoder::ColorType::Rgb, 8, 8, &rgb);
        let p = png_bytes(png::ColorType::Rgb, png::BitDepth::Eight, 8, 8, &rgb, None);
        for whole in [&j, &p] {
            for n in 0..whole.len() {
                let _ = decode_sync(&whole[..n]);
            }
            // Flipped bytes: still no panic.
            for i in (0..whole.len()).step_by(7) {
                let mut m = whole.clone();
                m[i] ^= 0xff;
                let _ = decode_sync(&m);
            }
        }
        assert!(decode_sync(&j).is_ok() && decode_sync(&p).is_ok());
        // Pseudo-random bytes, with and without a valid magic prefix.
        let mut x = 0x1234_5678u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        };
        for _ in 0..200 {
            let junk: Vec<u8> = (0..300).map(|_| next()).collect();
            let _ = decode_sync(&junk);
            let _ = decode_sync(&[&[0xff, 0xd8, 0xff][..], &junk].concat());
            let _ = decode_sync(&[&PNG_SIGNATURE[..], &junk].concat());
        }
        assert_eq!(decode_sync(&[]), Err(ArtError::Undecodable));
        assert_eq!(
            decode_sync(b"GIF89a\x01\x00\x01\x00"),
            Err(ArtError::Undecodable)
        );
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn d4_oversize_sides_are_too_large_before_allocation() {
        // JPEG: patch the SOF0 height and width to 4096.
        let rgb = vec![0u8; 8 * 8 * 3];
        let mut j = jpeg(jpeg_encoder::ColorType::Rgb, 8, 8, &rgb);
        let mut i = 2;
        while i + 4 <= j.len() {
            assert_eq!(j[i], 0xff, "marker at {i}");
            let marker = j[i + 1];
            let len = usize::from(j[i + 2]) << 8 | usize::from(j[i + 3]);
            if marker == 0xc0 {
                j[i + 5..i + 9].copy_from_slice(&[0x10, 0x00, 0x10, 0x00]);
                break;
            }
            i += 2 + len;
        }
        assert_eq!(decode_sync(&j), Err(ArtError::TooLarge));

        // PNG: patch IHDR to 50000 x 50000 and recompute its CRC.
        let mut p = png_bytes(
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            8,
            8,
            &rgb[..192],
            None,
        );
        p[16..20].copy_from_slice(&50_000u32.to_be_bytes());
        p[20..24].copy_from_slice(&50_000u32.to_be_bytes());
        let crc = crc32(&p[12..29]);
        p[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(decode_sync(&p), Err(ArtError::TooLarge));
        // Just over the limit, valid otherwise.
        p[16..20].copy_from_slice(&(MAX_ART_SIDE + 1).to_be_bytes());
        p[20..24].copy_from_slice(&1u32.to_be_bytes());
        let crc = crc32(&p[12..29]);
        p[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(decode_sync(&p), Err(ArtError::TooLarge));
    }

    #[tokio::test]
    async fn the_async_wrapper_decodes_and_maps_failures() {
        let p = png_bytes(
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            1,
            1,
            &[5],
            None,
        );
        assert!(decode(Arc::new(p)).await.is_ok());
        assert_eq!(
            decode(Arc::new(b"junk".to_vec())).await,
            Err(ArtError::Undecodable)
        );
    }
}
