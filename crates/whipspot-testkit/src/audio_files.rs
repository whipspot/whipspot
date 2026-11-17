//! Audio files built in code, byte for byte the same on every run (no clock, no randomness): a
//! WAV writer, a FLAC writer (verbatim subframes only, with tags and an optional picture), a
//! playlist writer, a tiny PNG, a hostile MP3 ([`id3_bomb_mp3`]) and a [`TempMusicDir`] that
//! lays a small library out on disk.
//! For tests of anything that reads the user's music files; real encoders are not needed.
//!
//! The FLAC writer does no compression: its job is a stream every decoder accepts, with exact
//! sample values, tags and a picture. Its MD5 field is zero ("not computed"), which the format
//! allows.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A deterministic test signal: `n` frames of `ch` interleaved channels at `bits` bits, a sine
/// plus noise from a fixed LCG, with the positive and the negative full-scale value at fixed
/// places so the extremes are covered. Values lie in the `bits`-bit signed range.
pub fn signal(n: usize, ch: usize, bits: u32) -> Vec<i32> {
    assert!((2..=32).contains(&bits), "signal: bits is 2..=32");
    let max = ((1i64 << (bits - 1)) - 1) as f64;
    let mut seed = 12345u64;
    (0..n * ch)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let noise = ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            let s = 0.7 * (i as f64 * 0.01).sin() + 0.2 * noise;
            if i == 7 {
                max as i32
            } else if i == 9 {
                (-max - 1.0) as i32
            } else {
                (s * max) as i32
            }
        })
        .collect()
}

/// A PCM WAV file: `samples` are interleaved signed integers of `bits` (16, 24 or 32) bits.
pub fn wav(rate: u32, bits: u16, channels: u16, samples: &[i32]) -> Vec<u8> {
    assert!(
        matches!(bits, 16 | 24 | 32),
        "wav: bits is 16, 24 or 32 (8-bit WAV is unsigned)"
    );
    assert!(channels >= 1, "wav: at least one channel");
    assert!(
        samples.len() % usize::from(channels) == 0,
        "wav: whole frames"
    );
    let bytes = usize::from(bits / 8);
    let data_len = samples.len() * bytes;
    let mut v = Vec::with_capacity(44 + data_len);
    v.extend(b"RIFF");
    v.extend(((36 + data_len) as u32).to_le_bytes());
    v.extend(b"WAVE");
    v.extend(b"fmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes()); // PCM
    v.extend(channels.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * u32::from(channels) * bytes as u32).to_le_bytes());
    v.extend((channels * bytes as u16).to_le_bytes());
    v.extend(bits.to_le_bytes());
    v.extend(b"data");
    v.extend((data_len as u32).to_le_bytes());
    for s in samples {
        v.extend(&s.to_le_bytes()[..bytes]);
    }
    v
}

/// A picture for a FLAC PICTURE block (front cover). `width` and `height` are the image's own.
pub struct FlacPicture<'a> {
    pub mime: &'a str,
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// Frames of this many samples per channel (the last one is shorter).
const FLAC_BLOCK: usize = 4096;

/// MSB-first bit writer.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u64,
    used: u32,
}

impl Bits {
    /// The low `n` bits of `value` (`n <= 32` here).
    fn put(&mut self, value: u64, n: u32) {
        debug_assert!(n <= 32 && value >> n == 0);
        self.acc = (self.acc << n) | value;
        self.used += n;
        while self.used >= 8 {
            self.used -= 8;
            self.out.push((self.acc >> self.used) as u8);
        }
        self.acc &= (1u64 << self.used) - 1;
    }

    /// Zero bits up to the next byte boundary.
    fn align(&mut self) {
        if self.used > 0 {
            self.put(0, 8 - self.used);
        }
    }
}

fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in bytes {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in bytes {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The FLAC "UTF-8" coding of a frame number.
fn utf8_number(n: u64) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let limits = [
        (0x800u64, 1usize),
        (0x10000, 2),
        (0x200000, 3),
        (0x4000000, 4),
        (0x8000_0000, 5),
    ];
    let extra = limits
        .iter()
        .find(|(limit, _)| n < *limit)
        .map_or(6, |&(_, e)| e);
    let prefix = !(0xFFu8 >> (extra + 1));
    let mut out = vec![prefix | (n >> (6 * extra)) as u8];
    for i in (0..extra).rev() {
        out.push(0x80 | ((n >> (6 * i)) & 0x3F) as u8);
    }
    out
}

fn flac_bps_code(bits: u16) -> u8 {
    match bits {
        8 => 0b001,
        12 => 0b010,
        16 => 0b100,
        20 => 0b101,
        24 => 0b110,
        _ => panic!("flac: bits is 8, 12, 16, 20 or 24"),
    }
}

/// One frame with verbatim subframes. `block` is interleaved.
fn flac_frame(number: u64, block: &[i32], rate: u32, bits: u16, channels: u16) -> Vec<u8> {
    let ch = usize::from(channels);
    let n = block.len() / ch;
    let (bs_code, bs_extra): (u8, Vec<u8>) = match n {
        FLAC_BLOCK => (0b1100, vec![]),
        1..=256 => (0b0110, vec![(n - 1) as u8]),
        _ => (0b0111, ((n - 1) as u16).to_be_bytes().to_vec()),
    };
    let (sr_code, sr_extra): (u8, Vec<u8>) = match rate {
        88_200 => (0b0001, vec![]),
        176_400 => (0b0010, vec![]),
        192_000 => (0b0011, vec![]),
        8_000 => (0b0100, vec![]),
        16_000 => (0b0101, vec![]),
        22_050 => (0b0110, vec![]),
        24_000 => (0b0111, vec![]),
        32_000 => (0b1000, vec![]),
        44_100 => (0b1001, vec![]),
        48_000 => (0b1010, vec![]),
        96_000 => (0b1011, vec![]),
        r if r <= 65_535 => (0b1101, (r as u16).to_be_bytes().to_vec()),
        r if r % 10 == 0 && r / 10 <= 65_535 => (0b1110, ((r / 10) as u16).to_be_bytes().to_vec()),
        _ => panic!("flac: this sample rate has no frame header code"),
    };
    let mut frame = vec![0xFF, 0xF8, (bs_code << 4) | sr_code];
    frame.push((((channels - 1) as u8) << 4) | (flac_bps_code(bits) << 1));
    frame.extend(utf8_number(number));
    frame.extend(bs_extra);
    frame.extend(sr_extra);
    frame.push(crc8(&frame));

    let mask = (1u64 << bits) - 1;
    let mut body = Bits::default();
    for c in 0..ch {
        body.put(0b0000_0010, 8); // padding bit, VERBATIM, no wasted bits
        for i in 0..n {
            let s = block[i * ch + c];
            debug_assert!(
                i64::from(s) >= -(1i64 << (bits - 1)) && i64::from(s) < (1i64 << (bits - 1)),
                "flac: a sample is out of range for {bits} bits"
            );
            body.put(s as u32 as u64 & mask, u32::from(bits));
        }
    }
    body.align();
    frame.extend(body.out);
    let crc = crc16(&frame);
    frame.extend(crc.to_be_bytes());
    frame
}

fn flac_block_header(last: bool, kind: u8, len: usize) -> [u8; 4] {
    assert!(len < 1 << 24, "flac: a metadata block is under 16 MiB");
    let l = (len as u32).to_be_bytes();
    [(u8::from(last) << 7) | kind, l[1], l[2], l[3]]
}

/// A FLAC file of verbatim subframes: STREAMINFO (MD5 zero), a VORBIS_COMMENT block when `tags`
/// is not empty, a PICTURE block when `picture` is given, then the audio. `samples` are
/// interleaved signed integers of `bits` (8, 12, 16, 20 or 24) bits.
pub fn flac(
    rate: u32,
    bits: u16,
    channels: u16,
    samples: &[i32],
    tags: &[(&str, &str)],
    picture: Option<&FlacPicture<'_>>,
) -> Vec<u8> {
    assert!((1..=8).contains(&channels), "flac: 1 to 8 channels");
    assert!(rate > 0 && rate < 1 << 20, "flac: the rate fits 20 bits");
    assert!(
        samples.len() % usize::from(channels) == 0,
        "flac: whole frames"
    );
    let total = (samples.len() / usize::from(channels)) as u64;
    assert!(total < 1 << 36, "flac: the length fits 36 bits");

    let mut info = Bits::default();
    info.put(FLAC_BLOCK as u64, 16);
    info.put(FLAC_BLOCK as u64, 16);
    info.put(0, 24); // smallest frame: unknown
    info.put(0, 24); // largest frame: unknown
    info.put(u64::from(rate), 20);
    info.put(u64::from(channels - 1), 3);
    info.put(u64::from(bits - 1), 5);
    info.put(total >> 32, 4);
    info.put(total & 0xFFFF_FFFF, 32);
    for _ in 0..4 {
        info.put(0, 32); // MD5: not computed
    }
    assert_eq!(info.out.len(), 34);

    let mut blocks: Vec<(u8, Vec<u8>)> = vec![(0, info.out)];
    if !tags.is_empty() {
        let vendor = b"whipspot-testkit";
        let mut v = Vec::new();
        v.extend((vendor.len() as u32).to_le_bytes());
        v.extend(vendor);
        v.extend((tags.len() as u32).to_le_bytes());
        for (key, value) in tags {
            let entry = format!("{key}={value}");
            v.extend((entry.len() as u32).to_le_bytes());
            v.extend(entry.as_bytes());
        }
        blocks.push((4, v));
    }
    if let Some(p) = picture {
        let mut v = Vec::new();
        v.extend(3u32.to_be_bytes()); // front cover
        v.extend((p.mime.len() as u32).to_be_bytes());
        v.extend(p.mime.as_bytes());
        v.extend(0u32.to_be_bytes()); // no description
        v.extend(p.width.to_be_bytes());
        v.extend(p.height.to_be_bytes());
        v.extend(24u32.to_be_bytes());
        v.extend(0u32.to_be_bytes()); // not indexed
        v.extend((p.data.len() as u32).to_be_bytes());
        v.extend(p.data);
        blocks.push((6, v));
    }

    let mut out = b"fLaC".to_vec();
    let count = blocks.len();
    for (i, (kind, body)) in blocks.into_iter().enumerate() {
        out.extend(flac_block_header(i + 1 == count, kind, body.len()));
        out.extend(body);
    }
    let per_frame = FLAC_BLOCK * usize::from(channels);
    for (number, block) in samples.chunks(per_frame).enumerate() {
        out.extend(flac_frame(number as u64, block, rate, bits, channels));
    }
    out
}

/// An extended playlist file: `#EXTM3U`, then the `lines` (entries and comments), LF-terminated.
pub fn m3u(lines: &[&str]) -> Vec<u8> {
    let mut text = String::from("#EXTM3U\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text.into_bytes()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
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

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// A solid-colour PNG (8-bit RGB, stored deflate blocks: valid, not small).
pub fn png_rgb(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    assert!((1..=4096).contains(&width) && (1..=4096).contains(&height));
    let mut row = vec![0u8]; // filter: none
    for _ in 0..width {
        row.extend(rgb);
    }
    let raw = row.repeat(height as usize);
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    z.extend(adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend((body.len() as u32).to_be_bytes());
        let mut crc_input = kind.to_vec();
        crc_input.extend(body);
        out.extend(&crc_input);
        out.extend(crc32(&crc_input).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// A zlib bomb: a valid zlib stream that inflates to `1 + 258 * repeats` zero bytes from about
/// `13 * repeats / 8` bytes (one fixed-Huffman block: a literal 0, then length-258 copies at
/// distance 1).
pub fn zlib_zeros(repeats: usize) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Bits {
        /// `count` bits of `value`, least significant first (deflate's packing order).
        fn put(&mut self, value: u32, count: u32) {
            for i in 0..count {
                self.acc |= ((value >> i) & 1) << self.n;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        /// A Huffman code of `count` bits, most significant first.
        fn code(&mut self, code: u32, count: u32) {
            for i in (0..count).rev() {
                self.put((code >> i) & 1, 1);
            }
        }
    }
    let mut w = Bits {
        out: vec![0x78, 0x01],
        acc: 0,
        n: 0,
    };
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE = fixed Huffman
    w.code(0x30, 8); // literal 0
    for _ in 0..repeats {
        w.code(0xC5, 8); // length symbol 285: 258
        w.code(0, 5); // distance symbol 0: 1
    }
    w.code(0, 7); // end of block
    if w.n > 0 {
        w.out.push(w.acc as u8);
    }
    // Adler-32 of `len` zero bytes: a stays 1, b grows by 1 per byte.
    let len = 1 + 258 * repeats as u64;
    let b = (len % 65_521) as u32;
    let mut out = w.out;
    out.extend(((b << 16) | 1).to_be_bytes());
    out
}

fn synchsafe(n: u32) -> [u8; 4] {
    assert!(n < 1 << 28);
    [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ]
}

/// An MP3-shaped file whose ID3v2.4 tag holds a compressed GEOB frame that inflates to about
/// `inflated_mib` MiB of zeros (a zlib bomb, the security review's S1), then a TIT2 frame with
/// `title`, then a few silent MPEG-1 Layer III frames. A reader that inflates the frame
/// allocates the whole payload; one that skips compressed frames still reads the title.
pub fn id3_bomb_mp3(title: &str, inflated_mib: usize) -> Vec<u8> {
    let repeats = inflated_mib * 1024 * 1024 / 258;
    let z = zlib_zeros(repeats);
    let inflated = 1 + 258 * repeats as u32;
    let mut frames = Vec::new();
    // GEOB: flags compressed (0x08) + data length indicator (0x01).
    frames.extend(b"GEOB");
    frames.extend(synchsafe(4 + z.len() as u32));
    frames.extend([0x00, 0x09]);
    frames.extend(synchsafe(inflated));
    frames.extend(&z);
    // TIT2, UTF-8.
    frames.extend(b"TIT2");
    frames.extend(synchsafe(1 + title.len() as u32));
    frames.extend([0x00, 0x00]);
    frames.push(0x03);
    frames.extend(title.as_bytes());

    let mut out = b"ID3".to_vec();
    out.extend([0x04, 0x00, 0x00]);
    out.extend(synchsafe(frames.len() as u32));
    out.extend(frames);
    // MPEG-1 Layer III, 128 kbps, 44.1 kHz, no padding: 417-byte frames.
    for _ in 0..20 {
        let mut f = vec![0u8; 417];
        f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend(f);
    }
    out
}
/// One track of the fixture album.
pub struct FixtureTrack {
    pub file: &'static str,
    pub title: &'static str,
    pub frames: usize,
    /// The ReplayGain tags, as written.
    pub track_gain: &'static str,
    pub track_peak: &'static str,
}

/// The fixture album: 44.1 kHz, 16-bit, stereo, tagged with title, artist, album artist, album,
/// track and disc number, year and ReplayGain.
pub const ALBUM_ARTIST: &str = "Fixture Artist";
pub const ALBUM_TITLE: &str = "Fixture Album";
pub const ALBUM_GAIN: &str = "-6.00 dB";
pub const ALBUM_PEAK: &str = "0.999969";
pub const ALBUM_TRACKS: [FixtureTrack; 3] = [
    FixtureTrack {
        file: "01 First.flac",
        title: "First Song",
        frames: 44_100,
        track_gain: "-6.50 dB",
        track_peak: "0.987654",
    },
    FixtureTrack {
        file: "02 Second.flac",
        title: "Second Song",
        frames: 66_150,
        track_gain: "-7.25 dB",
        track_peak: "0.500000",
    },
    FixtureTrack {
        file: "03 Third.flac",
        title: "Third Song",
        frames: 88_200,
        track_gain: "-5.00 dB",
        track_peak: "1.000000",
    },
];
/// The playlist's entry that points out of the tree (the file does not exist).
pub const OUTSIDE_ENTRY: &str = "../../not-in-the-library/outside.flac";

static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

/// A small music tree in a fresh directory under the OS temp dir, removed when dropped (the
/// directory is created by this struct and checked absent first).
///
/// ```text
/// <root>/Fixture Artist/Fixture Album/01 First.flac .. 03 Third.flac   tagged, ReplayGain,
///                                                                      a cover in the first
/// <root>/loose.wav                 44.1 kHz 16-bit stereo, no tags (0.25 s)
/// <root>/long.wav                  8 kHz 16-bit mono, 40 s (a load at 30 s lands inside it)
/// <root>/ignored.opus              not audio (an extension the library does not index)
/// <root>/playlists/mix.m3u         first track, loose.wav and one entry outside the tree
/// ```
pub struct TempMusicDir {
    pub root: PathBuf,
    pub album_dir: PathBuf,
    pub tracks: [PathBuf; 3],
    pub loose_wav: PathBuf,
    pub long_wav: PathBuf,
    pub opus: PathBuf,
    pub playlist: PathBuf,
}

impl TempMusicDir {
    pub fn new() -> TempMusicDir {
        let root = std::env::temp_dir().join(format!(
            "whipspot-music-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::SeqCst)
        ));
        assert!(!root.exists(), "{root:?} is not there yet");
        let album_dir = root.join(ALBUM_ARTIST).join(ALBUM_TITLE);
        std::fs::create_dir_all(&album_dir).expect("create the fixture album directory");
        std::fs::create_dir_all(root.join("playlists")).expect("create the playlists directory");

        let cover = png_rgb(4, 4, [200, 30, 60]);
        let picture = FlacPicture {
            mime: "image/png",
            width: 4,
            height: 4,
            data: &cover,
        };
        let tracks = [0usize, 1, 2].map(|i| {
            let t = &ALBUM_TRACKS[i];
            let number = (i + 1).to_string();
            let tags = [
                ("TITLE", t.title),
                ("ARTIST", ALBUM_ARTIST),
                ("ALBUMARTIST", ALBUM_ARTIST),
                ("ALBUM", ALBUM_TITLE),
                ("TRACKNUMBER", number.as_str()),
                ("DISCNUMBER", "1"),
                ("DATE", "1999"),
                ("REPLAYGAIN_TRACK_GAIN", t.track_gain),
                ("REPLAYGAIN_TRACK_PEAK", t.track_peak),
                ("REPLAYGAIN_ALBUM_GAIN", ALBUM_GAIN),
                ("REPLAYGAIN_ALBUM_PEAK", ALBUM_PEAK),
            ];
            let bytes = flac(
                44_100,
                16,
                2,
                &signal(t.frames, 2, 16),
                &tags,
                (i == 0).then_some(&picture),
            );
            let path = album_dir.join(t.file);
            std::fs::write(&path, bytes).expect("write a fixture track");
            path
        });

        let loose_wav = root.join("loose.wav");
        std::fs::write(&loose_wav, wav(44_100, 16, 2, &signal(11_025, 2, 16)))
            .expect("write loose.wav");
        let long_wav = root.join("long.wav");
        std::fs::write(&long_wav, wav(8_000, 16, 1, &signal(8_000 * 40, 1, 16)))
            .expect("write long.wav");
        let opus = root.join("ignored.opus");
        std::fs::write(&opus, b"OggS not really opus").expect("write ignored.opus");
        let playlist = root.join("playlists").join("mix.m3u");
        let first = format!("../{ALBUM_ARTIST}/{ALBUM_TITLE}/{}", ALBUM_TRACKS[0].file);
        std::fs::write(&playlist, m3u(&[&first, "../loose.wav", OUTSIDE_ENTRY]))
            .expect("write mix.m3u");

        TempMusicDir {
            root,
            album_dir,
            tracks,
            loose_wav,
            long_wav,
            opus,
            playlist,
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Default for TempMusicDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempMusicDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::{FormatOptions, TrackType, probe::Hint};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    use super::*;

    /// (rate, bits per sample, frames from the container, interleaved samples as f64).
    fn decode(bytes: Vec<u8>, ext: &str) -> (u32, Option<u32>, Option<u64>, Vec<f64>) {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(ext);
        let mut format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("the probe accepts the fixture");
        let track = format
            .default_track(TrackType::Audio)
            .expect("an audio track");
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .expect("audio parameters")
            .clone();
        let (id, frames) = (track.id, track.num_frames);
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .expect("a decoder");
        let (mut out, mut buf) = (Vec::new(), Vec::<f64>::new());
        while let Some(packet) = format.next_packet().expect("a packet or the end") {
            if packet.track_id != id {
                continue;
            }
            let decoded = decoder.decode(&packet).expect("a decoded packet");
            buf.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut buf);
            out.extend_from_slice(&buf);
        }
        (
            params.sample_rate.unwrap_or(0),
            params.bits_per_sample,
            frames,
            out,
        )
    }

    fn assert_exact(what: &str, src: &[i32], bits: u32, got: &[f64]) {
        let scale = (1i64 << (bits - 1)) as f64;
        assert_eq!(got.len(), src.len(), "{what}: sample count");
        let bad = src
            .iter()
            .zip(got)
            .position(|(s, g)| f64::from(*s) / scale != *g);
        assert_eq!(bad, None, "{what}: first sample that differs");
    }

    #[test]
    fn audio_files_flac_decodes_bit_exactly_at_16_44_24_96_24_192_and_24_384() {
        for (rate, bits) in [
            (44_100u32, 16u16),
            (96_000, 24),
            (192_000, 24),
            (384_000, 24),
        ] {
            // Two and a bit blocks: whole frames, a short last frame, several frame numbers.
            let frames = FLAC_BLOCK * 2 + 777;
            let src = signal(frames, 2, u32::from(bits));
            let bytes = flac(rate, bits, 2, &src, &[], None);
            let (r, b, n, got) = decode(bytes, "flac");
            let what = format!("flac {rate}/{bits}");
            assert_eq!((r, b), (rate, Some(u32::from(bits))), "{what}: format");
            assert_eq!(n, Some(frames as u64), "{what}: n_frames");
            assert_exact(&what, &src, u32::from(bits), &got);
        }
    }

    #[test]
    fn audio_files_flac_mono_one_short_block_and_an_odd_rate_decode_exactly() {
        for (rate, frames) in [
            (8_000u32, 100usize),
            (8_000, 300),
            (22_050, 1),
            (12_345, 5000),
        ] {
            let src = signal(frames, 1, 16);
            let (r, _, n, got) = decode(flac(rate, 16, 1, &src, &[], None), "flac");
            assert_eq!(
                (r, n),
                (rate, Some(frames as u64)),
                "flac mono {rate}/{frames}"
            );
            assert_exact("flac mono", &src, 16, &got);
        }
    }

    #[test]
    fn audio_files_flac_with_tags_and_a_picture_still_decodes_and_lays_its_blocks_out() {
        let cover = png_rgb(4, 4, [1, 2, 3]);
        let picture = FlacPicture {
            mime: "image/png",
            width: 4,
            height: 4,
            data: &cover,
        };
        let src = signal(5000, 2, 16);
        let bytes = flac(
            44_100,
            16,
            2,
            &src,
            &[("TITLE", "A"), ("ARTIST", "B")],
            Some(&picture),
        );
        let (_, _, n, got) = decode(bytes.clone(), "flac");
        assert_eq!(n, Some(5000));
        assert_exact("flac with tags", &src, 16, &got);

        // Walk the metadata blocks: STREAMINFO first, then comments, then the picture; only the
        // last has the last-block flag; the first frame follows with its sync code.
        assert_eq!(&bytes[..4], b"fLaC");
        let (mut at, mut kinds) = (4usize, Vec::new());
        loop {
            let (last, kind) = (bytes[at] & 0x80 != 0, bytes[at] & 0x7F);
            let len = usize::from(bytes[at + 1]) << 16
                | usize::from(bytes[at + 2]) << 8
                | usize::from(bytes[at + 3]);
            kinds.push(kind);
            at += 4 + len;
            if last {
                break;
            }
        }
        assert_eq!(kinds, [0, 4, 6]);
        assert_eq!(bytes[at..at + 2], [0xFF, 0xF8]);
        let text = String::from_utf8_lossy(&bytes[..at]);
        assert!(text.contains("TITLE=A") && text.contains("ARTIST=B"));
    }

    #[test]
    fn audio_files_wav_decodes_bit_exactly_at_16_44_24_192_and_32_384() {
        for (rate, bits) in [(44_100u32, 16u16), (192_000, 24), (384_000, 32)] {
            let src = signal(rate as usize / 4, 2, u32::from(bits));
            let (r, b, n, got) = decode(wav(rate, bits, 2, &src), "wav");
            let what = format!("wav {rate}/{bits}");
            assert_eq!((r, b), (rate, Some(u32::from(bits))), "{what}: format");
            assert_eq!(n, Some(u64::from(rate / 4)), "{what}: n_frames");
            assert_exact(&what, &src, u32::from(bits), &got);
        }
    }

    #[test]
    fn audio_files_output_is_byte_identical_across_runs() {
        let src = signal(9000, 2, 24);
        assert_eq!(signal(9000, 2, 24), src);
        assert_eq!(wav(96_000, 24, 2, &src), wav(96_000, 24, 2, &src));
        let tags = [("TITLE", "T")];
        assert_eq!(
            flac(96_000, 24, 2, &src, &tags, None),
            flac(96_000, 24, 2, &src, &tags, None)
        );
        assert_eq!(png_rgb(3, 3, [9, 8, 7]), png_rgb(3, 3, [9, 8, 7]));
        assert_eq!(m3u(&["a", "b"]), b"#EXTM3U\na\nb\n");
        // Two trees hold the same bytes (only their root differs).
        let (a, b) = (TempMusicDir::new(), TempMusicDir::new());
        assert_ne!(a.root, b.root);
        for (x, y) in [
            (&a.tracks[0], &b.tracks[0]),
            (&a.tracks[2], &b.tracks[2]),
            (&a.loose_wav, &b.loose_wav),
            (&a.long_wav, &b.long_wav),
            (&a.playlist, &b.playlist),
        ] {
            assert_eq!(
                std::fs::read(x).unwrap(),
                std::fs::read(y).unwrap(),
                "{x:?}"
            );
        }
    }

    #[test]
    fn audio_files_temp_music_dir_holds_what_it_promises_and_cleans_up() {
        let dir = TempMusicDir::new();
        let root = dir.root.clone();
        for (i, t) in ALBUM_TRACKS.iter().enumerate() {
            assert!(dir.tracks[i].ends_with(t.file));
            let (rate, bits, frames, _) = decode(std::fs::read(&dir.tracks[i]).unwrap(), "flac");
            assert_eq!(
                (rate, bits, frames),
                (44_100, Some(16), Some(t.frames as u64))
            );
        }
        let (rate, _, frames, _) = decode(std::fs::read(&dir.long_wav).unwrap(), "wav");
        assert_eq!((rate, frames), (8_000, Some(8_000 * 40)));
        assert!(dir.opus.is_file() && dir.loose_wav.is_file());
        let list = std::fs::read_to_string(&dir.playlist).unwrap();
        assert!(list.starts_with("#EXTM3U\n") && list.contains(OUTSIDE_ENTRY));
        assert!(
            std::fs::read(&dir.tracks[0])
                .unwrap()
                .windows(8)
                .any(|w| w == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            "the first track carries the cover"
        );
        drop(dir);
        assert!(!root.exists(), "the tree is removed on drop");
    }
}
