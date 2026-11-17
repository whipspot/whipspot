//! `.m3u` / `.m3u8` playlists (D7). A playlist is untrusted text: only its entries that resolve
//! to a file already in the index are kept, so a line never opens anything by itself.

use std::path::{Path, PathBuf};

use crate::ids::lexical_normalise;

/// A larger playlist file is not read.
pub const MAX_PLAYLIST_BYTES: u64 = 4 * 1024 * 1024;
/// Most entries kept from one playlist.
pub const MAX_PLAYLIST_ENTRIES: usize = 20_000;

/// Read the playlist at `path`; `None` when it cannot be read or is over the size cap.
pub fn read(path: &Path) -> Option<Vec<PathBuf>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PLAYLIST_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(parse(&bytes, path.parent()?))
}

/// The entries of a playlist whose directory is `dir`, as absolute, lexically normalised paths.
/// Comments (`#`), blank lines, URLs and entries that climb above the root are dropped. UTF-8,
/// with Latin-1 as the fallback for a plain `.m3u` that is not.
pub fn parse(bytes: &[u8], dir: &Path) -> Vec<PathBuf> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= MAX_PLAYLIST_ENTRIES {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.contains("://") {
            continue;
        }
        if line.chars().any(char::is_control) {
            continue;
        }
        let entry = Path::new(line);
        let joined = if entry.is_absolute() {
            entry.to_path_buf()
        } else {
            dir.join(entry)
        };
        if let Some(p) = lexical_normalise(&joined) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_entries_and_drops_the_rest() {
        let dir = std::env::temp_dir().join("m3u-test").join("lists");
        let bytes =
            "\u{feff}#EXTM3U\n#EXTINF:1,x\n../a/b.flac\r\n\nhttp://x.invalid/s.mp3\nc.wav\n";
        let got = parse(bytes.as_bytes(), &dir);
        let base = std::env::temp_dir().join("m3u-test");
        assert_eq!(
            got,
            [
                lexical_normalise(&base.join("a").join("b.flac")).unwrap(),
                lexical_normalise(&dir.join("c.wav")).unwrap()
            ]
        );
    }

    #[test]
    fn latin1_falls_back() {
        let dir = std::env::temp_dir();
        let got = parse(b"caf\xe9.flac\n", &dir);
        assert_eq!(got.len(), 1);
        assert!(got[0].to_string_lossy().ends_with("caf\u{e9}.flac"));
    }

    #[test]
    fn never_panics_on_noise() {
        let dir = std::env::temp_dir();
        for seed in 0..200u32 {
            let bytes: Vec<u8> = (0..256u32)
                .map(|i| (i.wrapping_mul(2_654_435_761).wrapping_add(seed * 97) >> 13) as u8)
                .collect();
            let _ = parse(&bytes, &dir);
        }
        let deep = "../".repeat(500) + "x.flac";
        let _ = parse(deep.as_bytes(), &dir);
    }
}
