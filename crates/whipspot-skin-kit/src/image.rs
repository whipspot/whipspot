//! PNG and JPEG decoding for skin art, under limits (hard rule 5): the sides and the RGBA size are
//! checked from the header before any pixel buffer exists, the output is RGBA8, and no library
//! error text escapes (hard rule 1). A decoder panic on hostile bytes is turned into
//! [`ImageError::Malformed`].
//!
//! Each decoder is behind its own cargo feature (`png`, `jpeg`); an engine enables the ones its
//! format uses.

#[cfg(any(feature = "png", feature = "jpeg"))]
use whipspot_core::art::MAX_ART_SIDE;
#[cfg(any(feature = "png", feature = "jpeg"))]
use whipspot_core::skin::SkinImage;

/// Why an image was not decoded. No variant carries input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// The bytes are not a well-formed image (or a side is zero).
    Malformed,
    /// A side is over `MAX_ART_SIDE`.
    TooLarge,
    /// The decoded size (`w * h * 4`) is over the caller's budget.
    OverBudget,
    /// A valid variant this decoder does not handle.
    Unsupported,
}

/// The sides are `1..=MAX_ART_SIDE` and `w * h * 4` is within the budget. Checked on header values,
/// before any allocation.
#[cfg(any(feature = "png", feature = "jpeg"))]
fn check_dimensions(w: u64, h: u64, max_rgba_bytes: usize) -> Result<(), ImageError> {
    if w == 0 || h == 0 {
        return Err(ImageError::Malformed);
    }
    let side = u64::from(MAX_ART_SIDE);
    if w > side || h > side {
        return Err(ImageError::TooLarge);
    }
    // At most 2048 * 2048 * 4, so this cannot overflow.
    if w * h * 4 > max_rgba_bytes as u64 {
        return Err(ImageError::OverBudget);
    }
    Ok(())
}

/// Runs a decoder; a panic inside it is `Malformed`.
#[cfg(any(feature = "png", feature = "jpeg"))]
fn guarded(f: impl FnOnce() -> Result<SkinImage, ImageError>) -> Result<SkinImage, ImageError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(Err(ImageError::Malformed))
}

/// Decodes a PNG to RGBA8 (straight alpha; palette, grey and 16-bit inputs are normalised).
#[cfg(feature = "png")]
pub fn decode_png(bytes: &[u8], max_rgba_bytes: usize) -> Result<SkinImage, ImageError> {
    guarded(|| png_inner(bytes, max_rgba_bytes))
}

#[cfg(feature = "png")]
fn png_inner(bytes: &[u8], max_rgba_bytes: usize) -> Result<SkinImage, ImageError> {
    /// png's own working buffers; it does not bound the caller's output buffer (checked below).
    const PNG_LIMIT_BYTES: usize = 16 * 1024 * 1024;

    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(bytes),
        png::Limits {
            bytes: PNG_LIMIT_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(png_error)?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_dimensions(u64::from(w), u64::from(h), max_rgba_bytes)?;
    let size = reader.output_buffer_size().ok_or(ImageError::TooLarge)?;
    // 8-bit output is at most 4 bytes a pixel, so this holds after the check above.
    if size as u64 > u64::from(w) * u64::from(h) * 4 {
        return Err(ImageError::Unsupported);
    }
    let mut buf = vec![0u8; size];
    let out = reader.next_frame(&mut buf).map_err(png_error)?;
    if out.width != w || out.height != h || out.bit_depth != png::BitDepth::Eight {
        return Err(ImageError::Unsupported);
    }
    let pixels = w as usize * h as usize;
    let src = buf.get(..out.buffer_size()).ok_or(ImageError::Malformed)?;
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
        _ => return Err(ImageError::Malformed),
    }
    SkinImage::new(w, h, rgba).ok_or(ImageError::Malformed)
}

#[cfg(feature = "png")]
fn png_error(e: png::DecodingError) -> ImageError {
    match e {
        png::DecodingError::LimitsExceeded => ImageError::TooLarge,
        _ => ImageError::Malformed,
    }
}

/// Most marker segments a JPEG may hold before it is refused, and most of them APP1 (where
/// metadata, extended XMP included, lives). A real skin picture has a handful of each.
#[cfg(feature = "jpeg")]
const MAX_JPEG_MARKERS: usize = 1024;
#[cfg(feature = "jpeg")]
const MAX_JPEG_APP1: usize = 64;

/// Most extended-XMP namespace strings a JPEG may hold anywhere in its bytes.
#[cfg(feature = "jpeg")]
const MAX_JPEG_EXTENDED_XMP: usize = 64;
#[cfg(feature = "jpeg")]
const EXTENDED_XMP_NAMESPACE: &[u8] = b"http://ns.adobe.com/xmp/extension/";

/// Counts the extended-XMP namespace strings anywhere in `b`, stopping once the count passes
/// `MAX_JPEG_EXTENDED_XMP`, and says whether it stayed within it. The decoder keeps one segment
/// per APP1 that begins with this string and walks all of them after every marker, so this bound
/// makes its work linear. It does not parse the format, so no marker trick can hide a segment
/// from it (unlike the marker walk below, which the decoder's own loop can read differently).
#[cfg(feature = "jpeg")]
fn jpeg_extended_xmp_sane(b: &[u8]) -> bool {
    let needle = EXTENDED_XMP_NAMESPACE;
    let mut count = 0usize;
    for (i, &c) in b.iter().enumerate() {
        if c == needle[0] && b[i..].starts_with(needle) {
            count += 1;
            if count > MAX_JPEG_EXTENDED_XMP {
                return false;
            }
        }
    }
    true
}

/// Walks the markers of a JPEG in linear time, following each segment's length and skipping
/// entropy-coded data, and says whether the count of markers and of APP1 segments is sane. The
/// decoder reassembles extended XMP after every marker in time quadratic in the segment count, so
/// hostile bytes are bounded here before it sees them. A damaged length simply ends the walk;
/// the decoder judges the rest.
#[cfg(feature = "jpeg")]
fn jpeg_markers_sane(b: &[u8]) -> bool {
    let n = b.len();
    let (mut i, mut markers, mut app1) = (2usize, 0usize, 0usize);
    while i < n {
        if b[i] != 0xff {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && b[j] == 0xff {
            j += 1;
        }
        let Some(&m) = b.get(j) else { break };
        // A stuffed zero byte and a restart marker are entropy data, not segments.
        if m == 0x00 || (0xd0..=0xd7).contains(&m) {
            i = j + 1;
            continue;
        }
        markers += 1;
        if markers > MAX_JPEG_MARKERS {
            return false;
        }
        if m == 0xd9 {
            break;
        }
        if m == 0x01 || m == 0xd8 {
            i = j + 1;
            continue;
        }
        if m == 0xe1 {
            app1 += 1;
            if app1 > MAX_JPEG_APP1 {
                return false;
            }
        }
        let (Some(&hi), Some(&lo)) = (b.get(j + 1), b.get(j + 2)) else {
            break;
        };
        let len = usize::from(u16::from_be_bytes([hi, lo]));
        if len < 2 {
            return false;
        }
        i = j + 1 + len;
    }
    true
}

/// Decodes a JPEG to RGBA8 (opaque).
#[cfg(feature = "jpeg")]
pub fn decode_jpeg(bytes: &[u8], max_rgba_bytes: usize) -> Result<SkinImage, ImageError> {
    guarded(|| jpeg_inner(bytes, max_rgba_bytes))
}

#[cfg(feature = "jpeg")]
fn jpeg_inner(bytes: &[u8], max_rgba_bytes: usize) -> Result<SkinImage, ImageError> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    if !jpeg_extended_xmp_sane(bytes) || !jpeg_markers_sane(bytes) {
        return Err(ImageError::Malformed);
    }
    // The decoder's own limit can only say "format error", so it is opened to the format's
    // ceiling (u16) and the real limit is checked here, on the header, before any output
    // buffer exists: an over-limit side is TooLarge, not Malformed.
    let ceiling = usize::from(u16::MAX);
    let options = DecoderOptions::default()
        .set_max_width(ceiling)
        .set_max_height(ceiling)
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|_| ImageError::Malformed)?;
    let (w, h) = decoder.dimensions().ok_or(ImageError::Malformed)?;
    check_dimensions(w as u64, h as u64, max_rgba_bytes)?;
    if decoder.output_colorspace() != Some(ColorSpace::RGBA) {
        return Err(ImageError::Unsupported);
    }
    let len = decoder.output_buffer_size().ok_or(ImageError::Malformed)?;
    if len != w * h * 4 {
        return Err(ImageError::Unsupported);
    }
    let mut out = vec![0u8; len];
    decoder
        .decode_into(&mut out)
        .map_err(|_| ImageError::Malformed)?;
    SkinImage::new(w as u32, h as u32, out).ok_or(ImageError::Malformed)
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "png", feature = "jpeg"))]
    use super::*;

    #[cfg(feature = "png")]
    fn png_bytes(
        w: u32,
        h: u32,
        colour: png::ColorType,
        depth: png::BitDepth,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(colour);
            enc.set_depth(depth);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[cfg(feature = "png")]
    #[test]
    fn png_round_trips_rgba_and_keeps_alpha() {
        let data: Vec<u8> = (0..4 * 3 * 2).map(|i| (i * 11) as u8).collect();
        let bytes = png_bytes(3, 2, png::ColorType::Rgba, png::BitDepth::Eight, &data);
        let img = decode_png(&bytes, 1 << 20).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        assert_eq!(img.rgba(), &data[..]);
    }

    #[cfg(feature = "png")]
    #[test]
    fn png_rgb_and_grey_become_opaque_rgba() {
        let rgb = png_bytes(
            2,
            1,
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            &[1, 2, 3, 4, 5, 6],
        );
        assert_eq!(
            decode_png(&rgb, 1 << 20).unwrap().rgba(),
            &[1, 2, 3, 255, 4, 5, 6, 255]
        );
        let grey = png_bytes(
            2,
            1,
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            &[9, 200],
        );
        assert_eq!(
            decode_png(&grey, 1 << 20).unwrap().rgba(),
            &[9, 9, 9, 255, 200, 200, 200, 255]
        );
    }

    #[cfg(feature = "png")]
    #[test]
    fn png_header_over_the_side_cap_is_refused_before_allocation() {
        // 4096 x 4096 one-bit grey: about 2 MiB raw, a few KiB compressed.
        let data = vec![0u8; 4096 * 4096 / 8];
        let bytes = png_bytes(
            4096,
            4096,
            png::ColorType::Grayscale,
            png::BitDepth::One,
            &data,
        );
        assert_eq!(decode_png(&bytes, usize::MAX), Err(ImageError::TooLarge));
    }

    #[cfg(feature = "png")]
    #[test]
    fn png_over_budget_and_garbage() {
        let data = vec![7u8; 4 * 4 * 4];
        let bytes = png_bytes(4, 4, png::ColorType::Rgba, png::BitDepth::Eight, &data);
        assert!(decode_png(&bytes, 4 * 4 * 4).is_ok());
        assert_eq!(
            decode_png(&bytes, 4 * 4 * 4 - 1),
            Err(ImageError::OverBudget)
        );
        assert_eq!(decode_png(&[], 1 << 20), Err(ImageError::Malformed));
        assert_eq!(
            decode_png(b"not a png at all", 1 << 20),
            Err(ImageError::Malformed)
        );
        for cut in 0..bytes.len() {
            // Every truncation is an error or a decode, never a panic.
            let _ = decode_png(&bytes[..cut], 1 << 20);
        }
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn jpeg_garbage_is_malformed() {
        assert_eq!(decode_jpeg(&[], 1 << 20), Err(ImageError::Malformed));
        assert_eq!(
            decode_jpeg(b"definitely not a jpeg", 1 << 20),
            Err(ImageError::Malformed)
        );
        assert_eq!(
            decode_jpeg(&[0xff, 0xd8], 1 << 20),
            Err(ImageError::Malformed)
        );
        assert_eq!(
            decode_jpeg(&[0xff, 0xd8, 0xff, 0xe0, 0x00], 1 << 20),
            Err(ImageError::Malformed)
        );
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn jpeg_with_a_flood_of_app1_segments_is_refused_before_decoding() {
        let mut bytes = vec![0xff, 0xd8];
        for _ in 0..200 {
            // An APP1 segment: marker, length 10, eight payload bytes.
            bytes.extend_from_slice(&[0xff, 0xe1, 0x00, 0x0a, 1, 2, 3, 4, 5, 6, 7, 8]);
        }
        bytes.extend_from_slice(&[0xff, 0xd9]);
        assert!(!jpeg_markers_sane(&bytes));
        assert_eq!(decode_jpeg(&bytes, 1 << 20), Err(ImageError::Malformed));
        // A handful is fine, and so is the encoder's own output.
        let few = &bytes[..2 + 12 * 8];
        assert!(jpeg_markers_sane(few));
    }

    #[cfg(feature = "jpeg")]
    fn extended_xmp_segment(j: &mut Vec<u8>) {
        // APP1: namespace, NUL, 32-byte GUID, total size 1000, offset 0, no data.
        let len = (2 + EXTENDED_XMP_NAMESPACE.len() + 1 + 40) as u16;
        j.extend_from_slice(&[0xff, 0xe1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(EXTENDED_XMP_NAMESPACE);
        j.push(0);
        j.extend_from_slice(&[b'A'; 32]);
        j.extend_from_slice(&1000u32.to_be_bytes());
        j.extend_from_slice(&0u32.to_be_bytes());
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn jpeg_hiding_extended_xmp_behind_a_restart_marker_is_refused() {
        // The security review's trick: the decoder reads RST0 with a length of 6 and skips
        // `FF FE FF FF`; a walk that takes RST as length-less reads a COM of 65535 bytes and
        // jumps over the extended-XMP segments the decoder then parses one by one.
        let mut j = vec![0xff, 0xd8];
        let start = j.len();
        j.extend_from_slice(&[0xff, 0xd0, 0x00, 0x06, 0xff, 0xfe, 0xff, 0xff]);
        let land = start + 6 + 65535;
        while j.len() + 79 <= land - 42 {
            extended_xmp_segment(&mut j);
        }
        let pad = land - j.len();
        j.extend_from_slice(&[0xff, 0xfe]);
        j.extend_from_slice(&((pad - 2) as u16).to_be_bytes());
        j.extend(std::iter::repeat_n(b'C', pad - 4));
        j.extend_from_slice(&[0xff, 0xd9]);
        // The marker walk is fooled; the byte count is not.
        assert!(jpeg_markers_sane(&j));
        assert!(!jpeg_extended_xmp_sane(&j));
        assert_eq!(decode_jpeg(&j, 1 << 20), Err(ImageError::Malformed));
        // At the limit the count passes.
        let mut few = vec![0xff, 0xd8];
        for _ in 0..MAX_JPEG_EXTENDED_XMP {
            extended_xmp_segment(&mut few);
        }
        assert!(jpeg_extended_xmp_sane(&few));
        extended_xmp_segment(&mut few);
        assert!(!jpeg_extended_xmp_sane(&few));
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn jpeg_decodes_to_opaque_rgba_and_obeys_the_budget() {
        let mut bytes = Vec::new();
        let px = vec![128u8; 8 * 8 * 3];
        jpeg_encoder::Encoder::new(&mut bytes, 90)
            .encode(&px, 8, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let img = decode_jpeg(&bytes, 8 * 8 * 4).unwrap();
        assert_eq!((img.width(), img.height()), (8, 8));
        assert!(img.rgba().chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(
            decode_jpeg(&bytes, 8 * 8 * 4 - 1),
            Err(ImageError::OverBudget)
        );
        for cut in 0..bytes.len() {
            let _ = decode_jpeg(&bytes[..cut], 1 << 20);
        }
    }
}
