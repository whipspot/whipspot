//! Seeded mutation fuzzing of the network adapter's parsers (D2, H31): LRCLIB's JSON, the LRC and
//! plain-lyrics text, and the JPEG and PNG decoders. Every input is synthetic or canned (hard
//! rule 2: no network call). Ordinary `cargo test`; see `whipspot_testkit::rng` for
//! `WHIPSPOT_FUZZ_ITERS` and `WHIPSPOT_FUZZ_SEED`. Not coverage-guided.
//!
//! Properties: nothing panics (the harness prints the seed and iteration, then resumes the
//! panic); lyrics results respect the line and character caps and the time order; an `Ok` image
//! has sides in `1..=MAX_ART_SIDE` and exactly `w * h * 4` bytes.

use whipspot_core::art::MAX_ART_SIDE;
use whipspot_core::lyrics::{LyricLine, Lyrics, LyricsError, MAX_LYRIC_CHARS, MAX_LYRIC_LINES};
use whipspot_testkit::rng::{Case, SplitMix64, fuzz, mutate};

use crate::decode::decode_sync;
use crate::lrc::{parse_plain, parse_synced};
use crate::lrclib::parse;

/// Reports how many inputs got past the parser and asserts a floor (skipped for short runs).
fn assert_reach(name: &str, reached: u64, total: u64, min_percent: u64) {
    eprintln!("fuzz {name}: {reached} of {total} inputs parsed");
    if total >= 200 {
        assert!(
            reached * 100 >= total * min_percent,
            "{name}: only {reached} of {total} inputs got past the parser (floor {min_percent}%)"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Lyrics
// ---------------------------------------------------------------------------------------------

/// The most a parsed time can be: `999:59.999`.
const MAX_AT_MS: u64 = (999 * 60 + 59) * 1000 + 999;

const LRC_DICT: &[&[u8]] = &[
    b"[",
    b"]",
    b"<",
    b">",
    b"[00:00.00]",
    b"[99:59.999]",
    b"[999:59.9999]",
    b"[00:60.00]",
    b"[ar:x]",
    b"<00:01.00>",
    b"\r\n",
    b"\n",
    b"\xe2\x80\xae",
    b"\xff",
    b"\0",
];

const JSON_DICT: &[&[u8]] = &[
    b"\"syncedLyrics\"",
    b"\"plainLyrics\"",
    b"\"instrumental\"",
    b"null",
    b"true",
    b"false",
    b"[",
    b"]",
    b"{",
    b"}",
    b":",
    b",",
    b"\\u0000",
    b"\\ud800",
    b"\\n",
    b"\"",
    b"1e999",
    b"-0",
];

fn lrc_corpus() -> Vec<Vec<u8>> {
    let texts = [
        "[ar:Artist]\r\n[ti:Title]\r\n[00:12.34]hello <00:12.50>world\r\n[00:15.00]second line\n",
        "[00:01.00][00:05.00]chorus\n[01:02]x\n[00:01.5]a\n[00:01.123]b\n[99:59.999]end\n",
        "no tags at all\njust text\n\n\nmore\n",
        "[00:10.00]\n[00:11.00]   \n[00:12.00]\u{202e}rtl \u{0}nul\n[00:13.00]\u{1F600} emoji\n",
        "[00:30.00]b\n[00:10.00]a\n[00:20.00]c\n",
    ];
    texts.iter().map(|t| t.as_bytes().to_vec()).collect()
}

/// A text of many lines, over the input-line and kept-line caps.
fn big_lrc(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("[{:02}:{:02}.00]line {i}\n", (i / 60) % 100, i % 60))
        .collect::<String>()
        .into_bytes()
}

fn check_synced(lines: &[LyricLine]) {
    assert!(lines.len() <= MAX_LYRIC_LINES, "{} lines", lines.len());
    for pair in lines.windows(2) {
        assert!(pair[0].at_ms <= pair[1].at_ms, "lines out of time order");
    }
    for l in lines {
        assert!(
            l.at_ms <= MAX_AT_MS,
            "time {} over the format's maximum",
            l.at_ms
        );
        assert!(
            l.text.chars().count() <= MAX_LYRIC_CHARS,
            "line over the char cap"
        );
    }
}

fn check_plain(lines: &[String]) {
    assert!(lines.len() <= MAX_LYRIC_LINES, "{} lines", lines.len());
    for l in lines {
        assert!(
            l.chars().count() <= MAX_LYRIC_CHARS,
            "line over the char cap"
        );
    }
}

#[test]
fn fuzz_lrc_synced_and_plain() {
    let corpus = lrc_corpus();
    let big = big_lrc(7000);
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("lrc", 0x4C52_0001, 50_000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut bytes = if rng.one_in(60) {
            big.clone()
        } else {
            rng.pick(&corpus).clone()
        };
        mutate(&mut bytes, rng, LRC_DICT);
        case.set_input_len(bytes.len());
        let text = String::from_utf8_lossy(&bytes);
        let synced = parse_synced(&text);
        reached += u64::from(!synced.is_empty());
        check_synced(&synced);
        check_plain(&parse_plain(&text));
    });
    assert_reach("lrc", reached, total, 20);
}

#[test]
fn lrc_line_caps_hold_on_a_huge_input() {
    // The unmutated large inputs: the work and the result are bounded by the caps.
    for lines in [1_000, 5_000, 7_000, 50_000] {
        let text = String::from_utf8(big_lrc(lines)).unwrap();
        let synced = parse_synced(&text);
        assert!(!synced.is_empty());
        check_synced(&synced);
        check_plain(&parse_plain(&text));
    }
}

fn lrclib_corpus() -> Vec<Vec<u8>> {
    let synced = "[00:01.00]one\n[00:02.00]two\n[00:03.00]three";
    let docs = [
        serde_json::json!({"id": 1, "trackName": "t", "artistName": "a", "duration": 200,
            "instrumental": false, "plainLyrics": "one\ntwo\nthree", "syncedLyrics": synced}),
        serde_json::json!({"id": 2, "instrumental": true, "plainLyrics": null, "syncedLyrics": null}),
        serde_json::json!({"plainLyrics": "only plain\n\nlines\n"}),
        serde_json::json!({"syncedLyrics": 42, "plainLyrics": ["not", "a", "string"]}),
        serde_json::json!({"syncedLyrics": "[ar:x]\nno times", "plainLyrics": "fallback"}),
        serde_json::json!({"syncedLyrics": synced, "extra": {"deep": [[[[[[1]]]]]]}}),
        serde_json::json!([1, 2, 3]),
    ];
    docs.iter().map(|d| d.to_string().into_bytes()).collect()
}

#[test]
fn fuzz_lrclib_response_parse() {
    let corpus = lrclib_corpus();
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("lrclib", 0x4C52_0002, 50_000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut bytes = rng.pick(&corpus).clone();
        if !rng.one_in(8) {
            mutate(&mut bytes, rng, JSON_DICT);
        }
        if rng.one_in(40) {
            // Nesting far past the JSON reader's depth limit.
            let depth = 100 + rng.below(400);
            bytes = [vec![b'['; depth], vec![b']'; depth]].concat();
        }
        case.set_input_len(bytes.len());
        match parse(&bytes) {
            Ok(lyrics) => {
                reached += 1;
                match &lyrics {
                    Lyrics::Synced(lines) => {
                        assert!(!lines.is_empty(), "an empty Synced result");
                        check_synced(lines);
                    }
                    Lyrics::Plain(lines) => {
                        assert!(!lines.is_empty(), "an empty Plain result");
                        check_plain(lines);
                    }
                    Lyrics::Instrumental | Lyrics::None => {}
                }
            }
            Err(e) => assert_eq!(e, LyricsError::BadResponse),
        }
    });
    assert_reach("lrclib", reached, total, 8);
}

// ---------------------------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------------------------

fn random_bytes(rng: &mut SplitMix64, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rng.fill(&mut v);
    v
}

/// A smooth-ish image: gradients with some noise, so the encoder makes real entropy-coded data.
fn pixels(rng: &mut SplitMix64, w: usize, h: usize, channels: usize) -> Vec<u8> {
    let noise = random_bytes(rng, w * h * channels);
    (0..w * h * channels)
        .map(|i| {
            ((i / channels % w) * 6 + (i / channels / w) * 3 + usize::from(noise[i] >> 5)) as u8
        })
        .collect()
}

fn jpeg_corpus(rng: &mut SplitMix64) -> Vec<Vec<u8>> {
    use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
    let mut out = Vec::new();
    for (w, h, color, channels, progressive, sampling) in [
        (
            16u16,
            8u16,
            ColorType::Rgb,
            3usize,
            false,
            SamplingFactor::F_2_2,
        ),
        (8, 8, ColorType::Luma, 1, false, SamplingFactor::F_1_1),
        (40, 24, ColorType::Rgb, 3, true, SamplingFactor::F_1_1),
        (33, 17, ColorType::Rgb, 3, false, SamplingFactor::F_2_1),
        (24, 24, ColorType::Rgba, 4, false, SamplingFactor::F_1_1),
    ] {
        let px = pixels(rng, usize::from(w), usize::from(h), channels);
        let mut bytes = Vec::new();
        let mut enc = Encoder::new(&mut bytes, 85);
        enc.set_progressive(progressive);
        enc.set_sampling_factor(sampling);
        enc.encode(&px, w, h, color).expect("encodes");
        out.push(bytes);
    }
    out
}

fn png_corpus(rng: &mut SplitMix64) -> Vec<Vec<u8>> {
    use png::{BitDepth, ColorType};
    let mut out = Vec::new();
    for (color, depth, w, h) in [
        (ColorType::Grayscale, BitDepth::Eight, 9u32, 5u32),
        (ColorType::Grayscale, BitDepth::One, 17, 4),
        (ColorType::GrayscaleAlpha, BitDepth::Eight, 6, 6),
        (ColorType::Rgb, BitDepth::Eight, 12, 7),
        (ColorType::Rgb, BitDepth::Sixteen, 5, 5),
        (ColorType::Rgba, BitDepth::Eight, 8, 8),
        (ColorType::Indexed, BitDepth::Four, 11, 6),
        (ColorType::Indexed, BitDepth::Eight, 10, 10),
    ] {
        let channels = match color {
            ColorType::Grayscale | ColorType::Indexed => 1,
            ColorType::GrayscaleAlpha => 2,
            ColorType::Rgb => 3,
            ColorType::Rgba => 4,
        };
        let bits = channels * depth as usize;
        let row = (w as usize * bits).div_ceil(8);
        let data = random_bytes(rng, row * h as usize);
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, w, h);
            enc.set_color(color);
            enc.set_depth(depth);
            if color == ColorType::Indexed {
                let entries = 1usize << (depth as usize);
                enc.set_palette(random_bytes(rng, entries * 3));
                enc.set_trns(random_bytes(rng, entries.min(8)));
            }
            let mut writer = enc.write_header().expect("header");
            writer.write_image_data(&data).expect("data");
        }
        out.push(bytes);
    }
    out
}

/// The CRC-32 of a PNG chunk (type and data), bitwise: slow but five lines.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Rewrites every chunk's CRC so a mutated chunk is not rejected by its checksum alone.
fn fix_png_crcs(png: &mut [u8]) {
    let mut at = 8usize;
    while at + 12 <= png.len() {
        let len = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]) as usize;
        let Some(end) = at.checked_add(8).and_then(|d| d.checked_add(len)) else {
            return;
        };
        if end + 4 > png.len() {
            return;
        }
        let crc = crc32(&png[at + 4..end]);
        png[end..end + 4].copy_from_slice(&crc.to_be_bytes());
        at = end + 4;
    }
}

fn check_image(bytes: &[u8]) -> bool {
    match decode_sync(bytes) {
        Ok(img) => {
            let (w, h) = (img.width(), img.height());
            assert!(
                (1..=MAX_ART_SIDE).contains(&w) && (1..=MAX_ART_SIDE).contains(&h),
                "decoded side out of range"
            );
            assert_eq!(img.rgba().len() as u64, u64::from(w) * u64::from(h) * 4);
            true
        }
        Err(_) => false,
    }
}

#[test]
fn fuzz_image_decode_jpeg() {
    let corpus = jpeg_corpus(&mut SplitMix64::new(1));
    for (i, bytes) in corpus.iter().enumerate() {
        assert!(
            check_image(bytes),
            "corpus jpeg {i} decodes before mutation"
        );
    }
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("jpeg", 0x4A50_0001, 20_000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut bytes = rng.pick(&corpus).clone();
        mutate(
            &mut bytes,
            rng,
            &[&[0xff, 0xc0], &[0xff, 0xc2], &[0xff, 0xdb], &[0xff, 0xd9]],
        );
        case.set_input_len(bytes.len());
        reached += u64::from(check_image(&bytes));
    });
    assert_reach("jpeg", reached, total, 8);
}

#[test]
fn fuzz_image_decode_png() {
    let corpus = png_corpus(&mut SplitMix64::new(2));
    for (i, bytes) in corpus.iter().enumerate() {
        assert!(check_image(bytes), "corpus png {i} decodes before mutation");
    }
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("png", 0x504E_0001, 30_000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut bytes = rng.pick(&corpus).clone();
        mutate(
            &mut bytes,
            rng,
            &[b"IHDR", b"IDAT", b"PLTE", b"tRNS", b"IEND", b"acTL"],
        );
        // Mostly keep the checksums valid, so the mutation reaches the decoder proper.
        if !rng.one_in(5) {
            fix_png_crcs(&mut bytes);
        }
        case.set_input_len(bytes.len());
        reached += u64::from(check_image(&bytes));
    });
    assert_reach("png", reached, total, 5);
}

#[test]
fn fuzz_image_decode_garbage_with_valid_magic() {
    // Random bytes behind each magic number: the sniffing passes, the decoders must refuse.
    fuzz("magic", 0x4D47_0001, 10_000, |case: &mut Case| {
        let rng = &mut case.rng;
        let mut bytes = if rng.one_in(2) {
            vec![0xff, 0xd8, 0xff]
        } else {
            vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        };
        let n = rng.below(300);
        bytes.extend(random_bytes(rng, n));
        case.set_input_len(bytes.len());
        check_image(&bytes);
    });
}
