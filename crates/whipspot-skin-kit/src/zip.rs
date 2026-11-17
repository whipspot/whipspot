//! A read-only zip reader over a byte slice (D1, MD94). It reads the end record and the central
//! directory, nothing else, until an entry is asked for; methods are stored and deflate only.
//!
//! The archive is untrusted input (hard rule 5): every read is bounds-checked, sizes and the
//! method come from the central record, every cap is checked before an allocation, and an entry
//! name is only ever a lookup key (D6), never a path.

use std::cell::Cell;
use std::collections::HashMap;

use miniz_oxide::inflate::decompress_to_vec_with_limit;
use whipspot_core::skin::MAX_SKIN_BYTES;

/// Most central-directory entries an archive may declare.
pub const MAX_ENTRIES: usize = 1024;
/// Most bytes one entry may inflate to.
pub const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
/// Most bytes all reads of one [`Archive`] may inflate to, together.
pub const MAX_INFLATED_TOTAL: usize = 32 * 1024 * 1024;
/// Longest entry key kept, in bytes.
pub const MAX_KEY_BYTES: usize = 64;

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
/// The comment length is a `u16`, so the end record starts within this many bytes of the end.
const EOCD_SEARCH: usize = EOCD_LEN + 0xFFFF;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const CENTRAL_LEN: usize = 46;
const LOCAL_SIG: u32 = 0x0403_4b50;
const LOCAL_LEN: usize = 30;

/// Why a zip operation failed. No variant carries input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipError {
    /// The bytes are not a well-formed archive (or entry).
    Malformed,
    /// A cap was exceeded.
    TooLarge,
    /// A valid feature this reader does not handle (encryption, zip64, multi-disk, other methods).
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Stored,
    Deflate,
    /// An encrypted entry or a method other than stored and deflate.
    Other,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    method: Method,
    compressed: u32,
    size: u32,
    local_offset: u32,
}

/// The central directory of one archive plus a running total of inflated bytes.
pub struct Archive<'a> {
    bytes: &'a [u8],
    entries: HashMap<String, Entry>,
    inflated: Cell<usize>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The lookup key of an entry name (D6): the ASCII-lowercased part after the last `/` or `\`,
/// 1..=64 bytes. `None` for a non-ASCII name, an empty part (a directory) or a long one.
pub fn key_of(name: &[u8]) -> Option<String> {
    if !name.is_ascii() {
        return None;
    }
    let tail = name
        .rsplit(|b| *b == b'/' || *b == b'\\')
        .next()
        .unwrap_or(name);
    if tail.is_empty() || tail.len() > MAX_KEY_BYTES {
        return None;
    }
    // ASCII checked above, so this is valid UTF-8.
    Some(String::from_utf8_lossy(tail).to_ascii_lowercase())
}

/// The end record's position: the last signature (scanning backwards) whose comment length ends
/// exactly at the end of the input, else the last one whose comment fits.
fn find_eocd(b: &[u8]) -> Option<usize> {
    if b.len() < EOCD_LEN {
        return None;
    }
    let last = b.len() - EOCD_LEN;
    let first = b.len().saturating_sub(EOCD_SEARCH);
    let mut fits = None;
    for p in (first..=last).rev() {
        if u32_at(b, p) != Some(EOCD_SIG) {
            continue;
        }
        let Some(clen) = u16_at(b, p + 20) else {
            continue;
        };
        let end = p + EOCD_LEN + usize::from(clen);
        if end == b.len() {
            return Some(p);
        }
        if end < b.len() && fits.is_none() {
            fits = Some(p);
        }
    }
    fits
}

impl<'a> Archive<'a> {
    /// Reads the end record and the central directory. Inflates nothing.
    pub fn parse(bytes: &'a [u8]) -> Result<Archive<'a>, ZipError> {
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(ZipError::TooLarge);
        }
        let eocd = find_eocd(bytes).ok_or(ZipError::Malformed)?;
        let rd16 = |at| u16_at(bytes, eocd + at).ok_or(ZipError::Malformed);
        let rd32 = |at| u32_at(bytes, eocd + at).ok_or(ZipError::Malformed);
        let (disk, cd_disk) = (rd16(4)?, rd16(6)?);
        let (on_disk, total) = (rd16(8)?, rd16(10)?);
        let (cd_size, cd_offset) = (rd32(12)?, rd32(16)?);
        if disk != 0 || cd_disk != 0 {
            return Err(ZipError::Unsupported);
        }
        if total == 0xFFFF || cd_size == u32::MAX || cd_offset == u32::MAX {
            return Err(ZipError::Unsupported);
        }
        if on_disk != total {
            return Err(ZipError::Malformed);
        }
        let total = usize::from(total);
        if total > MAX_ENTRIES {
            return Err(ZipError::TooLarge);
        }
        let cd_end = u64::from(cd_offset) + u64::from(cd_size);
        if cd_end > eocd as u64 {
            return Err(ZipError::Malformed);
        }
        let dir = &bytes[cd_offset as usize..cd_end as usize];

        let mut entries: HashMap<String, Entry> = HashMap::new();
        let mut at = 0usize;
        for _ in 0..total {
            if u32_at(dir, at) != Some(CENTRAL_SIG) {
                return Err(ZipError::Malformed);
            }
            let field16 = |o: usize| u16_at(dir, at + o).ok_or(ZipError::Malformed);
            let field32 = |o: usize| u32_at(dir, at + o).ok_or(ZipError::Malformed);
            let flags = field16(8)?;
            let method = field16(10)?;
            let compressed = field32(20)?;
            let size = field32(24)?;
            let name_len = usize::from(field16(28)?);
            let extra_len = usize::from(field16(30)?);
            let comment_len = usize::from(field16(32)?);
            let disk_start = field16(34)?;
            let local_offset = field32(42)?;
            if disk_start != 0 {
                return Err(ZipError::Unsupported);
            }
            if compressed == u32::MAX || size == u32::MAX || local_offset == u32::MAX {
                return Err(ZipError::Unsupported);
            }
            let name_at = at + CENTRAL_LEN;
            let name = dir
                .get(name_at..name_at + name_len)
                .ok_or(ZipError::Malformed)?;
            let next = name_at + name_len + extra_len + comment_len;
            if next > dir.len() {
                return Err(ZipError::Malformed);
            }
            at = next;
            let Some(key) = key_of(name) else { continue };
            let method = if flags & 0x0041 != 0 {
                Method::Other
            } else {
                match method {
                    0 => Method::Stored,
                    8 => Method::Deflate,
                    _ => Method::Other,
                }
            };
            // The first entry in central-directory order wins a duplicate key (D6).
            entries.entry(key).or_insert(Entry {
                method,
                compressed,
                size,
                local_offset,
            });
        }
        Ok(Archive {
            bytes,
            entries,
            inflated: Cell::new(0),
        })
    }

    /// Whether an entry with this key (any case) exists in the central directory.
    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(&key.to_ascii_lowercase())
    }

    /// Every entry key, in ascending byte order (deterministic). At most [`MAX_ENTRIES`] long.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.entries.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// The entry's bytes. `None` when there is no such key. Counts the declared size against
    /// [`MAX_INFLATED_TOTAL`] before inflating.
    pub fn read(&self, key: &str) -> Option<Result<Vec<u8>, ZipError>> {
        let entry = self.entries.get(&key.to_ascii_lowercase())?;
        Some(self.read_entry(entry))
    }

    fn read_entry(&self, e: &Entry) -> Result<Vec<u8>, ZipError> {
        if e.method == Method::Other {
            return Err(ZipError::Unsupported);
        }
        let size = e.size as usize;
        if size > MAX_ENTRY_BYTES {
            return Err(ZipError::TooLarge);
        }
        let total = self.inflated.get().saturating_add(size);
        if total > MAX_INFLATED_TOTAL {
            return Err(ZipError::TooLarge);
        }
        self.inflated.set(total);

        let b = self.bytes;
        let off = e.local_offset as usize;
        if u32_at(b, off) != Some(LOCAL_SIG) {
            return Err(ZipError::Malformed);
        }
        // Only the local header's own name and extra lengths are used (D1).
        let name_len = u16_at(b, off + 26).ok_or(ZipError::Malformed)?;
        let extra_len = u16_at(b, off + 28).ok_or(ZipError::Malformed)?;
        let start = off as u64 + LOCAL_LEN as u64 + u64::from(name_len) + u64::from(extra_len);
        let end = start + u64::from(e.compressed);
        if end > b.len() as u64 {
            return Err(ZipError::Malformed);
        }
        let data = &b[start as usize..end as usize];
        match e.method {
            Method::Stored => {
                if e.compressed != e.size {
                    return Err(ZipError::Malformed);
                }
                Ok(data.to_vec())
            }
            Method::Deflate => {
                if size == 0 {
                    return Ok(Vec::new());
                }
                let out =
                    decompress_to_vec_with_limit(data, size).map_err(|_| ZipError::Malformed)?;
                if out.len() != size {
                    return Err(ZipError::Malformed);
                }
                Ok(out)
            }
            Method::Other => Err(ZipError::Unsupported),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{ZEntry, ZipSpec, zip_deflated, zip_stored};

    const MIB: usize = 1024 * 1024;

    fn read(a: &Archive, key: &str) -> Vec<u8> {
        a.read(key).expect("entry exists").expect("entry reads")
    }

    fn spec(entries: Vec<ZEntry>) -> ZipSpec {
        ZipSpec {
            entries,
            ..ZipSpec::default()
        }
    }

    #[test]
    fn stored_and_deflated_round_trip() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        for bytes in [
            zip_stored(&[("a.bin", &data), ("b.txt", b"hello")]),
            zip_deflated(&[("a.bin", &data), ("b.txt", b"hello")]),
        ] {
            let a = Archive::parse(&bytes).unwrap();
            assert_eq!(read(&a, "a.bin"), data);
            assert_eq!(read(&a, "b.txt"), b"hello");
            assert!(a.has("a.bin") && !a.has("c.bin"));
            assert!(a.read("c.bin").is_none());
        }
    }

    #[test]
    fn empty_entries_read_as_empty() {
        for bytes in [
            zip_stored(&[("e.bin", b"")]),
            zip_deflated(&[("e.bin", b"")]),
        ] {
            let a = Archive::parse(&bytes).unwrap();
            assert_eq!(read(&a, "e.bin"), b"");
        }
    }

    #[test]
    fn keys_ignore_folders_and_case() {
        let bytes = zip_stored(&[
            ("Skin/MAIN.BMP", b"one"),
            ("a\\b\\Cbuttons.bmp", b"two"),
            ("../main2.bmp", b"three"),
        ]);
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(read(&a, "main.bmp"), b"one");
        assert_eq!(read(&a, "MAIN.bmp"), b"one");
        assert_eq!(read(&a, "cbuttons.bmp"), b"two");
        assert_eq!(read(&a, "main2.bmp"), b"three");
        assert!(!a.has("skin/main.bmp"));
        assert!(!a.has("../main2.bmp"));
    }

    #[test]
    fn key_of_table() {
        assert_eq!(key_of(b"Skin/MAIN.BMP").as_deref(), Some("main.bmp"));
        assert_eq!(key_of(b"a\\b\\Main.bmp").as_deref(), Some("main.bmp"));
        assert_eq!(key_of(b"../main.bmp").as_deref(), Some("main.bmp"));
        assert_eq!(key_of(b"dir/"), None);
        assert_eq!(key_of(b""), None);
        assert_eq!(key_of("caf\u{e9}.bmp".as_bytes()), None);
        assert!(key_of(&[b'a'; 64]).is_some());
        assert_eq!(key_of(&[b'a'; 65]), None);
    }

    #[test]
    fn duplicate_key_first_wins() {
        let bytes = zip_stored(&[("x/main.bmp", b"first"), ("main.BMP", b"second")]);
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(read(&a, "main.bmp"), b"first");
    }

    #[test]
    fn non_ascii_name_is_skipped() {
        let bytes = zip_stored(&[("m\u{e4}in.bmp", b"x"), ("ok.bmp", b"y")]);
        let a = Archive::parse(&bytes).unwrap();
        assert!(!a.has("m\u{e4}in.bmp") && !a.has("main.bmp"));
        assert_eq!(read(&a, "ok.bmp"), b"y");
    }

    #[test]
    fn too_many_entries() {
        let make = |n: usize| {
            spec(
                (0..n)
                    .map(|i| ZEntry::stored(&format!("f{i}.bin"), b""))
                    .collect(),
            )
            .bytes()
        };
        assert_eq!(
            Archive::parse(&make(MAX_ENTRIES + 1)).err(),
            Some(ZipError::TooLarge)
        );
        assert!(Archive::parse(&make(MAX_ENTRIES)).is_ok());
    }

    #[test]
    fn declared_size_over_the_cap_is_refused_without_inflating() {
        let mut e = ZEntry::deflated("big.bin", b"not a deflate stream at all");
        e.size = (4 * MIB + 1) as u32;
        let bytes = spec(vec![e]).bytes();
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(a.read("big.bin").unwrap().err(), Some(ZipError::TooLarge));
        // Exactly at the cap is accepted.
        let zeros = vec![0u8; 4 * MIB];
        let bytes = zip_deflated(&[("ok.bin", &zeros)]);
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(read(&a, "ok.bin").len(), 4 * MIB);
    }

    #[test]
    fn deflate_bomb_is_malformed() {
        // 64 MiB of zeros declared as 1 MiB: the inflate limit stops it.
        let zeros = vec![0u8; 64 * MIB];
        let mut e = ZEntry::deflated("bomb.bin", &zeros);
        e.size = MIB as u32;
        let bytes = spec(vec![e]).bytes();
        assert!(bytes.len() < MIB);
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(a.read("bomb.bin").unwrap().err(), Some(ZipError::Malformed));
    }

    #[test]
    fn short_stream_for_declared_size_is_malformed() {
        let mut e = ZEntry::deflated("s.bin", &[7u8; 100]);
        e.size = 200;
        let bytes = spec(vec![e]).bytes();
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(a.read("s.bin").unwrap().err(), Some(ZipError::Malformed));
    }

    #[test]
    fn stored_needs_equal_sizes() {
        let mut e = ZEntry::stored("s.bin", b"abcdef");
        e.size = 5;
        let bytes = spec(vec![e]).bytes();
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(a.read("s.bin").unwrap().err(), Some(ZipError::Malformed));
    }

    #[test]
    fn inflated_total_is_capped_per_archive() {
        let zeros = vec![0u8; 4 * MIB];
        let names: Vec<String> = (0..9).map(|i| format!("z{i}.bin")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &zeros[..])).collect();
        let bytes = zip_deflated(&entries);
        assert!(bytes.len() < MAX_SKIN_BYTES as usize);
        let a = Archive::parse(&bytes).unwrap();
        for n in &names[..8] {
            assert_eq!(read(&a, n).len(), 4 * MIB);
        }
        assert_eq!(a.read(&names[8]).unwrap().err(), Some(ZipError::TooLarge));
    }

    #[test]
    fn encrypted_and_unknown_methods_are_unsupported() {
        let mut enc = ZEntry::stored("enc.bin", b"abc");
        enc.flags = 1;
        let mut odd = ZEntry::stored("odd.bin", b"abc");
        odd.method = 12;
        let ok = ZEntry::stored("ok.bin", b"abc");
        let bytes = spec(vec![enc, odd, ok]).bytes();
        let a = Archive::parse(&bytes).unwrap();
        assert!(a.has("enc.bin"));
        assert_eq!(
            a.read("enc.bin").unwrap().err(),
            Some(ZipError::Unsupported)
        );
        assert_eq!(
            a.read("odd.bin").unwrap().err(),
            Some(ZipError::Unsupported)
        );
        assert_eq!(read(&a, "ok.bin"), b"abc");
    }

    #[test]
    fn zip64_and_multi_disk_are_unsupported() {
        let base = spec(vec![ZEntry::stored("a.bin", b"x")]);
        let unsupported = |s: &ZipSpec| {
            assert_eq!(
                Archive::parse(&s.bytes()).err(),
                Some(ZipError::Unsupported)
            )
        };
        let mut s = base.clone();
        s.count = Some(0xFFFF);
        unsupported(&s);
        let mut s = base.clone();
        s.cd_offset = Some(u32::MAX);
        unsupported(&s);
        let mut s = base.clone();
        s.cd_size = Some(u32::MAX);
        unsupported(&s);
        let mut s = base.clone();
        s.disk = 1;
        unsupported(&s);
        let mut s = base.clone();
        s.entries[0].csize = Some(u32::MAX);
        unsupported(&s);
        // An entry that starts on another disk fails the whole archive, whatever its name.
        let mut s = base;
        s.entries.push(ZEntry::stored("b.bin", b"y"));
        s.entries[1].disk_start = 1;
        unsupported(&s);
        s.entries[1].name = b"dir/".to_vec();
        unsupported(&s);
    }

    #[test]
    fn structural_damage_is_malformed() {
        let base = spec(vec![ZEntry::stored("a.bin", b"payload")]);
        let malformed_parse =
            |s: &ZipSpec| assert_eq!(Archive::parse(&s.bytes()).err(), Some(ZipError::Malformed));
        let malformed_read = |s: &ZipSpec| {
            let bytes = s.bytes();
            let a = Archive::parse(&bytes).unwrap();
            assert_eq!(a.read("a.bin").unwrap().err(), Some(ZipError::Malformed));
        };
        // Central directory past the end record.
        let mut s = base.clone();
        s.cd_offset = Some(10_000);
        malformed_parse(&s);
        let mut s = base.clone();
        s.cd_size = Some(10_000);
        malformed_parse(&s);
        // More entries declared than the directory holds.
        let mut s = base.clone();
        s.count = Some(5);
        malformed_parse(&s);
        // Local offset past the end, wrong local signature, data past the end.
        let mut s = base.clone();
        s.entries[0].local_offset = Some(10_000);
        malformed_read(&s);
        let mut s = base.clone();
        s.entries[0].local_sig = 0x1234_5678;
        malformed_read(&s);
        let mut s = base;
        s.entries[0].csize = Some(5_000);
        s.entries[0].size = 5_000;
        malformed_read(&s);
    }

    #[test]
    fn end_record_inside_a_comment_is_skipped() {
        // The comment holds a fake end record signature followed by text.
        let mut comment = b"hello ".to_vec();
        comment.extend_from_slice(&EOCD_SIG.to_le_bytes());
        comment.extend_from_slice(&[0u8; 18]);
        comment.extend_from_slice(b" and some more text after it");
        let mut s = spec(vec![ZEntry::stored("a.bin", b"real")]);
        s.comment = comment;
        let bytes = s.bytes();
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(read(&a, "a.bin"), b"real");
    }

    #[test]
    fn size_limits_on_the_input() {
        assert!(Archive::parse(&[]).is_err());
        assert!(Archive::parse(&[0u8; 21]).is_err());
        assert!(Archive::parse(b"PK\x05\x06").is_err());
        let big = vec![0u8; MAX_SKIN_BYTES as usize + 1];
        assert_eq!(Archive::parse(&big).err(), Some(ZipError::TooLarge));
    }

    #[test]
    fn every_prefix_and_flipped_byte_is_handled() {
        let bytes = zip_deflated(&[("a.bin", &[1u8; 300]), ("dir/b.bin", b"second entry")]);
        let touch = |b: &[u8]| {
            if let Ok(a) = Archive::parse(b) {
                for key in ["a.bin", "b.bin"] {
                    let _ = a.read(key);
                }
            }
        };
        for n in 0..=bytes.len() {
            touch(&bytes[..n]);
        }
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0xFF;
            touch(&b);
        }
    }
}
