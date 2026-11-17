//! Builders for synthetic bitmaps and archives (hard rule 4: no real skin or asset is committed;
//! everything a test parses is generated). Behind the `synth` feature, and in the kit's own tests.
//! Both builders take raw field overrides so a test can write a malformed file.

use miniz_oxide::deflate::compress_to_vec;

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

// ---------------------------------------------------------------------------------------------
// Bitmaps
// ---------------------------------------------------------------------------------------------

/// A bitmap file in pieces; [`BmpSpec::bytes`] writes it. Every field can be set to a bad value.
#[derive(Clone)]
pub struct BmpSpec {
    pub width: i32,
    pub height: i32,
    pub planes: u16,
    pub bpp: u16,
    pub compression: u32,
    /// DIB header size: 40, 52, 56, 108 or 124 write that many bytes (fields beyond 40 are zero).
    pub header_size: u32,
    /// `None` writes the palette's length.
    pub colours_used: Option<u32>,
    pub palette: Vec<[u8; 3]>,
    /// The pixel bytes exactly as stored.
    pub data: Vec<u8>,
    /// `None` puts the pixels right after the palette.
    pub data_offset: Option<u32>,
}

impl BmpSpec {
    pub fn new(width: i32, height: i32, bpp: u16) -> BmpSpec {
        BmpSpec {
            width,
            height,
            planes: 1,
            bpp,
            compression: 0,
            header_size: 40,
            colours_used: None,
            palette: Vec::new(),
            data: Vec::new(),
            data_offset: None,
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let palette_bytes = self.palette.len() as u32 * 4;
        let default_offset = 14 + self.header_size + palette_bytes;
        let mut v = Vec::new();
        v.extend_from_slice(b"BM");
        put32(&mut v, default_offset + self.data.len() as u32);
        put32(&mut v, 0);
        put32(&mut v, self.data_offset.unwrap_or(default_offset));
        put32(&mut v, self.header_size);
        put32(&mut v, self.width as u32);
        put32(&mut v, self.height as u32);
        put16(&mut v, self.planes);
        put16(&mut v, self.bpp);
        put32(&mut v, self.compression);
        put32(&mut v, self.data.len() as u32);
        put32(&mut v, 2835);
        put32(&mut v, 2835);
        put32(
            &mut v,
            self.colours_used.unwrap_or(self.palette.len() as u32),
        );
        put32(&mut v, 0);
        // Header bytes beyond the 40 of the basic header (masks, colour space).
        v.resize(14 + self.header_size as usize, 0);
        for c in &self.palette {
            v.extend_from_slice(&[c[2], c[1], c[0], 0]);
        }
        v.extend_from_slice(&self.data);
        v
    }
}

/// Packs `rows` (top row first, one index per pixel) at 1, 4 or 8 bits, rows padded to 4 bytes,
/// stored bottom-up unless `top_down`.
fn pack_indexed(bpp: u16, rows: &[Vec<u8>], top_down: bool) -> Vec<u8> {
    let mut stored: Vec<&Vec<u8>> = rows.iter().collect();
    if !top_down {
        stored.reverse();
    }
    let mut out = Vec::new();
    for row in stored {
        let mut bytes = vec![0u8; (row.len() * usize::from(bpp)).div_ceil(32) * 4];
        for (x, idx) in row.iter().enumerate() {
            match bpp {
                1 => bytes[x / 8] |= (idx & 1) << (7 - x % 8),
                4 => bytes[x / 2] |= (idx & 0xF) << (4 * (1 - x % 2)),
                _ => bytes[x] = *idx,
            }
        }
        out.extend_from_slice(&bytes);
    }
    out
}

/// A 1-, 4- or 8-bit palettised bitmap from index rows (top row first).
pub fn bmp_indexed(bpp: u16, palette: &[[u8; 3]], rows: &[Vec<u8>], top_down: bool) -> Vec<u8> {
    let height = rows.len() as i32;
    let mut s = BmpSpec::new(
        rows[0].len() as i32,
        if top_down { -height } else { height },
        bpp,
    );
    s.palette = palette.to_vec();
    s.data = pack_indexed(bpp, rows, top_down);
    s.bytes()
}

/// A 24- or 32-bit bitmap from RGB rows (top row first); 32-bit stores alpha 0 (the decoder
/// must force it opaque).
pub fn bmp_rgb(bpp: u16, rows: &[Vec<[u8; 3]>], top_down: bool) -> Vec<u8> {
    let height = rows.len() as i32;
    let width = rows[0].len();
    let mut s = BmpSpec::new(width as i32, if top_down { -height } else { height }, bpp);
    let mut stored: Vec<&Vec<[u8; 3]>> = rows.iter().collect();
    if !top_down {
        stored.reverse();
    }
    for row in stored {
        let mut bytes = Vec::new();
        for c in row {
            bytes.extend_from_slice(&[c[2], c[1], c[0]]);
            if bpp == 32 {
                bytes.push(0);
            }
        }
        bytes.resize((width * usize::from(bpp)).div_ceil(32) * 4, 0);
        s.data.extend_from_slice(&bytes);
    }
    s.bytes()
}

/// An 8-bit RLE bitmap (always bottom-up) whose pixel stream is `rle` as given.
pub fn bmp_rle8(width: i32, height: i32, palette: &[[u8; 3]], rle: &[u8]) -> Vec<u8> {
    let mut s = BmpSpec::new(width, height, 8);
    s.compression = 1;
    s.palette = palette.to_vec();
    s.data = rle.to_vec();
    s.bytes()
}

// ---------------------------------------------------------------------------------------------
// Archives
// ---------------------------------------------------------------------------------------------

/// One archive entry. `payload` is written as is (already deflated when `method` is 8).
#[derive(Clone)]
pub struct ZEntry {
    pub name: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub payload: Vec<u8>,
    /// Declared uncompressed size.
    pub size: u32,
    /// Declared compressed size in the central record; `None` writes the payload length.
    pub csize: Option<u32>,
    pub local_sig: u32,
    /// `None` writes the real offset of the local header.
    pub local_offset: Option<u32>,
    /// The disk the central record says the entry starts on.
    pub disk_start: u16,
}

impl ZEntry {
    pub fn stored(name: &str, data: &[u8]) -> ZEntry {
        ZEntry {
            name: name.as_bytes().to_vec(),
            method: 0,
            flags: 0,
            payload: data.to_vec(),
            size: data.len() as u32,
            csize: None,
            local_sig: 0x0403_4b50,
            local_offset: None,
            disk_start: 0,
        }
    }

    pub fn deflated(name: &str, data: &[u8]) -> ZEntry {
        ZEntry {
            method: 8,
            payload: compress_to_vec(data, 6),
            ..ZEntry::stored(name, data)
        }
    }
}

/// An archive in pieces; [`ZipSpec::bytes`] writes it.
#[derive(Clone, Default)]
pub struct ZipSpec {
    pub entries: Vec<ZEntry>,
    pub comment: Vec<u8>,
    /// `None` writes the number of entries.
    pub count: Option<u16>,
    pub cd_offset: Option<u32>,
    pub cd_size: Option<u32>,
    pub disk: u16,
}

impl ZipSpec {
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        let mut offsets = Vec::new();
        for e in &self.entries {
            offsets.push(v.len() as u32);
            put32(&mut v, e.local_sig);
            put16(&mut v, 20);
            put16(&mut v, e.flags);
            put16(&mut v, e.method);
            put32(&mut v, 0);
            put32(&mut v, 0);
            put32(&mut v, e.payload.len() as u32);
            put32(&mut v, e.size);
            put16(&mut v, e.name.len() as u16);
            put16(&mut v, 0);
            v.extend_from_slice(&e.name);
            v.extend_from_slice(&e.payload);
        }
        let cd_start = v.len() as u32;
        for (e, off) in self.entries.iter().zip(&offsets) {
            put32(&mut v, 0x0201_4b50);
            put16(&mut v, 20);
            put16(&mut v, 20);
            put16(&mut v, e.flags);
            put16(&mut v, e.method);
            put32(&mut v, 0);
            put32(&mut v, 0);
            put32(&mut v, e.csize.unwrap_or(e.payload.len() as u32));
            put32(&mut v, e.size);
            put16(&mut v, e.name.len() as u16);
            put16(&mut v, 0);
            put16(&mut v, 0);
            put16(&mut v, e.disk_start);
            put16(&mut v, 0);
            put32(&mut v, 0);
            put32(&mut v, e.local_offset.unwrap_or(*off));
            v.extend_from_slice(&e.name);
        }
        let cd_len = v.len() as u32 - cd_start;
        let count = self.count.unwrap_or(self.entries.len() as u16);
        put32(&mut v, 0x0605_4b50);
        put16(&mut v, self.disk);
        put16(&mut v, 0);
        put16(&mut v, count);
        put16(&mut v, count);
        put32(&mut v, self.cd_size.unwrap_or(cd_len));
        put32(&mut v, self.cd_offset.unwrap_or(cd_start));
        put16(&mut v, self.comment.len() as u16);
        v.extend_from_slice(&self.comment);
        v
    }
}

/// An archive of deflated entries (name, bytes).
pub fn zip_deflated(entries: &[(&str, &[u8])]) -> Vec<u8> {
    ZipSpec {
        entries: entries
            .iter()
            .map(|(n, d)| ZEntry::deflated(n, d))
            .collect(),
        ..ZipSpec::default()
    }
    .bytes()
}

/// An archive of stored entries (name, bytes).
pub fn zip_stored(entries: &[(&str, &[u8])]) -> Vec<u8> {
    ZipSpec {
        entries: entries.iter().map(|(n, d)| ZEntry::stored(n, d)).collect(),
        ..ZipSpec::default()
    }
    .bytes()
}
