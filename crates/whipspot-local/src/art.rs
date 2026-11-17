//! Cover art of local tracks (D9): `local-art:<track uri>` resolves through the index to the
//! file's embedded picture, else a folder image (`cover`, `folder` or `front` with `.jpg`,
//! `.jpeg` or `.png`, any case) in the track's directory. JPEG and PNG only, chosen by magic
//! bytes; image bytes are capped at 20 MiB and the decoded size at 4096 x 4096, and a larger
//! than `MAX_ART_SIDE` result is box-downscaled to fit.

use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;
use whipspot_core::art::{ArtError, ArtImage, ArtSource, MAX_ART_SIDE};
use whipspot_core::provider::{BoxFuture, ImageRef};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::Shared;
use crate::ids::ART_PREFIX;
use crate::scan::is_cover_name;

/// Largest image file or embedded picture read.
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Largest decoded side; above it the image is refused.
pub const MAX_DECODE_SIDE: u32 = 4096;
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// png's own working buffers; the output buffer is checked separately.
const PNG_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RGBA_BYTES: usize = MAX_DECODE_SIDE as usize * MAX_DECODE_SIDE as usize * 4;
/// Most extended-XMP namespace strings a JPEG may hold anywhere in its bytes.
const MAX_JPEG_EXTENDED_XMP: usize = 64;
const EXTENDED_XMP_NAMESPACE: &[u8] = b"http://ns.adobe.com/xmp/extension/";

/// The `local-art:` art source. Holds the provider's catalog, so it always sees the newest scan.
pub struct LocalArt {
    pub(crate) shared: Arc<Shared>,
}

impl ArtSource for LocalArt {
    fn fetch<'a>(&'a self, image: &'a ImageRef) -> BoxFuture<'a, Result<ArtImage, ArtError>> {
        Box::pin(async move {
            let uri = image
                .url
                .strip_prefix(ART_PREFIX)
                .ok_or(ArtError::Refused)?;
            let (path, embedded) = {
                let catalog = self.shared.catalog();
                let t = catalog.by_uri(uri).ok_or(ArtError::Refused)?;
                (PathBuf::from(&t.entry.path), t.entry.has_picture)
            };
            let work = move || load(&path, embedded);
            match tokio::runtime::Handle::try_current() {
                Ok(h) => h
                    .spawn_blocking(work)
                    .await
                    .unwrap_or(Err(ArtError::Undecodable)),
                Err(_) => work(),
            }
        })
    }
}

/// The track's art: its embedded picture first (when the scan saw one), else a folder image.
fn load(track: &Path, embedded: bool) -> Result<ArtImage, ArtError> {
    let bytes = if embedded {
        embedded_picture(track)
    } else {
        None
    };
    let bytes = match bytes {
        Some(b) => b,
        None => folder_image(track).ok_or(ArtError::Unavailable)?,
    };
    decode(&bytes)
}

/// The front cover (else the first picture) of an audio file, at most [`MAX_IMAGE_BYTES`].
fn embedded_picture(track: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(track).ok()?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let tagged = Probe::new(BufReader::new(file))
            .options(ParseOptions::new().read_properties(false))
            .guess_file_type()
            .ok()?
            .read()
            .ok()?;
        let pictures: Vec<_> = tagged.tags().iter().flat_map(|t| t.pictures()).collect();
        let pick = pictures
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .or_else(|| pictures.first())?;
        let data = pick.data();
        (data.len() as u64 <= MAX_IMAGE_BYTES).then(|| data.to_vec())
    }))
    .ok()
    .flatten()
}

/// The bytes of the folder image next to `track`: `cover`, then `folder`, then `front`.
fn folder_image(track: &Path) -> Option<Vec<u8>> {
    let dir = track.parent()?;
    let mut found: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_cover_name(p))
        .map(|p| {
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let rank = ["cover", "folder", "front"]
                .iter()
                .position(|c| *c == stem)
                .unwrap_or(3);
            (rank, p)
        })
        .collect();
    found.sort();
    found.into_iter().find_map(|(_, p)| read_capped(&p))
}

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let mut out = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= MAX_IMAGE_BYTES).then_some(out)
}

/// Decode JPEG or PNG bytes (by magic) and fit them into `MAX_ART_SIDE`. A panic inside a
/// decoder is `Undecodable`.
pub fn decode(bytes: &[u8]) -> Result<ArtImage, ArtError> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ArtError::TooLarge);
    }
    let decoded = std::panic::catch_unwind(|| {
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            decode_jpeg(bytes)
        } else if bytes.starts_with(&PNG_SIGNATURE) {
            decode_png(bytes)
        } else {
            Err(ArtError::Undecodable)
        }
    })
    .unwrap_or(Err(ArtError::Undecodable))?;
    let (w, h, rgba) = decoded;
    let (w, h, rgba) = fit(w, h, rgba);
    ArtImage::new(w, h, rgba).ok_or(ArtError::Undecodable)
}

fn check_sides(w: usize, h: usize) -> Result<(), ArtError> {
    if w == 0 || h == 0 {
        return Err(ArtError::Undecodable);
    }
    if w > MAX_DECODE_SIDE as usize || h > MAX_DECODE_SIDE as usize {
        return Err(ArtError::TooLarge);
    }
    Ok(())
}

type Rgba = (u32, u32, Vec<u8>);

/// Counts the extended-XMP namespace strings anywhere in `b`, stopping once the count passes
/// `MAX_JPEG_EXTENDED_XMP`, and says whether it stayed within it. zune-jpeg keeps one segment per
/// APP1 that begins with this string and walks all of them after every marker (quadratic in
/// their count), so this bound makes its work linear. It does not parse the format, so no marker
/// trick can hide a segment from it. Copied from `whipspot-skin-kit`'s `jpeg_extended_xmp_sane`
/// (an adapter may not depend on the skin engines' helper crate).
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

fn decode_jpeg(bytes: &[u8]) -> Result<Rgba, ArtError> {
    if !jpeg_extended_xmp_sane(bytes) {
        return Err(ArtError::Undecodable);
    }
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
    check_sides(w, h)?;
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
    Ok((w as u32, h as u32, out))
}

fn decode_png(bytes: &[u8]) -> Result<Rgba, ArtError> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: PNG_LIMIT_BYTES,
        },
    );
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| ArtError::Undecodable)?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_sides(w as usize, h as usize)?;
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
    Ok((w, h, rgba))
}

/// `rgba` (w x h) box-downscaled so its longest side is at most `MAX_ART_SIDE`; unchanged when
/// it fits. Each output pixel is the mean of the source pixels its box covers.
fn fit(w: u32, h: u32, rgba: Vec<u8>) -> Rgba {
    let longest = w.max(h);
    if longest <= MAX_ART_SIDE {
        return (w, h, rgba);
    }
    let scale = |side: u32| {
        ((u64::from(side) * u64::from(MAX_ART_SIDE) + u64::from(longest) / 2) / u64::from(longest))
            .max(1) as u32
    };
    let (nw, nh) = (scale(w), scale(h));
    let (w, h, nw_, nh_) = (w as usize, h as usize, nw as usize, nh as usize);
    let mut out = Vec::with_capacity(nw_ * nh_ * 4);
    for oy in 0..nh_ {
        let (y0, y1) = (oy * h / nh_, ((oy + 1) * h / nh_).max(oy * h / nh_ + 1));
        for ox in 0..nw_ {
            let (x0, x1) = (ox * w / nw_, ((ox + 1) * w / nw_).max(ox * w / nw_ + 1));
            let mut sum = [0u64; 4];
            for y in y0..y1.min(h) {
                let row = &rgba[(y * w + x0) * 4..(y * w + x1.min(w)) * 4];
                for px in row.chunks_exact(4) {
                    for c in 0..4 {
                        sum[c] += u64::from(px[c]);
                    }
                }
            }
            let n = ((y1.min(h) - y0) * (x1.min(w) - x0)).max(1) as u64;
            for s in sum {
                out.push(((s + n / 2) / n) as u8);
            }
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn jpeg_with_many_extended_xmp_segments_is_refused_before_the_decoder() {
        // Many never-completed extended-XMP segments (S2): refused by the byte count, fast.
        let mut j = vec![0xff, 0xd8];
        for _ in 0..20_000 {
            extended_xmp_segment(&mut j);
        }
        j.extend_from_slice(&[0xff, 0xd9]);
        assert!(!jpeg_extended_xmp_sane(&j));
        let t = std::time::Instant::now();
        assert_eq!(decode_jpeg(&j), Err(ArtError::Undecodable));
        // Liveness bound, loose for loaded hosts (the decoder itself would take seconds).
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        // At the limit the count passes; one more fails.
        let mut few = vec![0xff, 0xd8];
        for _ in 0..MAX_JPEG_EXTENDED_XMP {
            extended_xmp_segment(&mut few);
        }
        assert!(jpeg_extended_xmp_sane(&few));
        extended_xmp_segment(&mut few);
        assert!(!jpeg_extended_xmp_sane(&few));
    }

    #[test]
    fn a_compressed_id3_frame_is_never_inflated_by_the_art_path() {
        // S1: the art fetch runs lofty again on the file; the zlib bomb is skipped there too.
        let dir =
            std::env::temp_dir().join(format!("whipspot-local-art-bomb-{}", std::process::id()));
        assert!(!dir.exists());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bomb.mp3");
        std::fs::write(
            &path,
            whipspot_testkit::audio_files::id3_bomb_mp3("Bomb", 64),
        )
        .unwrap();
        let t = std::time::Instant::now();
        let picture = embedded_picture(&path);
        let elapsed = t.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(picture.is_none());
        // Liveness bound, loose for loaded hosts.
        assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
    }

    #[test]
    fn fit_keeps_small_and_shrinks_large() {
        let (w, h, px) = fit(3, 2, vec![7; 24]);
        assert_eq!((w, h, px.len()), (3, 2, 24));
        let (w, h, px) = fit(4096, 1024, vec![200; 4096 * 1024 * 4]);
        assert_eq!((w, h), (2048, 512));
        assert_eq!(px.len(), 2048 * 512 * 4);
        assert!(px.iter().all(|&v| v == 200));
        let (w, h, _) = fit(3000, 2049, vec![0; 3000 * 2049 * 4]);
        assert!(w == 2048 && (1398..=2048).contains(&h), "{w}x{h}");
    }
}
