//! Ids (D4): FNV-1a 64 over a stable key, as 16 lowercase hex digits. An id is a lookup key into
//! the in-memory index only; nothing ever turns one back into a path.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub const TRACK_PREFIX: &str = "local:track:";
pub const ALBUM_PREFIX: &str = "album:";
/// The uri of an album: `local:album:<16 hex>` (the hex of its [`ALBUM_PREFIX`] id).
pub const ALBUM_URI_PREFIX: &str = "local:album:";
/// The uri of an artist: `local:artist:<16 hex>`, minted from the normalised name.
pub const ARTIST_URI_PREFIX: &str = "local:artist:";
pub const PLAYLIST_PREFIX: &str = "playlist:";
/// The art ref of a track: `local-art:<track uri>` (D9).
pub const ART_PREFIX: &str = "local-art:";
/// The scheme of [`ART_PREFIX`]: what the art router registers [`crate::LocalArt`] under.
pub const ART_SCHEME: &str = "local-art";

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hands out ids of one kind: the same key gets the same id; a key whose hash is taken by another
/// key is rehashed with a counter until it is free. Feed keys in a stable order (sorted) so a
/// collision resolves the same way on every build.
#[derive(Default)]
pub struct IdMinter {
    by_key: HashMap<String, String>,
    taken: HashMap<u64, ()>,
}

impl IdMinter {
    pub fn id(&mut self, key: &str) -> String {
        if let Some(id) = self.by_key.get(key) {
            return id.clone();
        }
        let mut h = fnv1a64(key.as_bytes());
        let mut counter = 0u32;
        while self.taken.contains_key(&h) {
            counter += 1;
            h = fnv1a64(format!("{key}\u{0}{counter}").as_bytes());
        }
        self.taken.insert(h, ());
        let id = format!("{h:016x}");
        self.by_key.insert(key.to_owned(), id.clone());
        id
    }
}

/// Whether `uri` has the shape `local:track:<16 lowercase hex>`.
pub fn is_track_uri(uri: &str) -> bool {
    uri.strip_prefix(TRACK_PREFIX).is_some_and(is_hex16)
}

/// Whether `uri` has the shape `local:artist:<16 lowercase hex>`.
pub fn is_artist_uri(uri: &str) -> bool {
    uri.strip_prefix(ARTIST_URI_PREFIX).is_some_and(is_hex16)
}

/// The album id (`album:<16 hex>`) of the album uri `local:album:<16 hex>`; `None` for any other
/// shape.
pub fn album_id_of_uri(uri: &str) -> Option<String> {
    let hex = uri.strip_prefix(ALBUM_URI_PREFIX).filter(|h| is_hex16(h))?;
    Some(format!("{ALBUM_PREFIX}{hex}"))
}

/// The album uri of the album id `album:<hex>`.
pub fn album_uri_of_id(id: &str) -> String {
    format!("local:{id}")
}

fn is_hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The comparison key of a path: slashes forward, no trailing separator, and on Windows (whose
/// file systems ignore case) lowercased. Two paths naming the same file give the same key.
pub fn path_key(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = if cfg!(windows) { p.to_lowercase() } else { p };
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        p
    } else {
        trimmed.to_owned()
    }
}

/// Whether the file `key` lies inside the folder `folder_key` (both from [`path_key`]).
pub fn key_is_under(key: &str, folder_key: &str) -> bool {
    key.len() > folder_key.len()
        && key.starts_with(folder_key)
        && (folder_key.ends_with('/') || key.as_bytes()[folder_key.len()] == b'/')
}

/// `path` with `.` and `..` resolved lexically (no file system access, no symlink resolution).
/// `None` when a `..` would climb above the root.
pub fn lexical_normalise(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    return None;
                }
                out.pop();
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_matches_the_reference_values() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn minter_is_stable_and_resolves_collisions() {
        let mut m = IdMinter::default();
        let a = m.id("x");
        assert_eq!(m.id("x"), a);
        assert_eq!(a, format!("{:016x}", fnv1a64(b"x")));
        // Force a collision: take the hash of "y" first under another key.
        let mut m = IdMinter::default();
        m.taken.insert(fnv1a64(b"y"), ());
        let y = m.id("y");
        assert_ne!(y, format!("{:016x}", fnv1a64(b"y")));
        assert!(is_hex16(&y));
        assert_eq!(m.id("y"), y);
    }

    #[test]
    fn track_uri_shape() {
        assert!(is_track_uri("local:track:0123456789abcdef"));
        for bad in [
            "",
            "local:track:",
            "local:track:0123456789ABCDEF",
            "local:track:0123456789abcde",
            "local:track:0123456789abcdef0",
            "spotify:track:0123456789abcdef",
            "local:track:../../etc/passwd",
        ] {
            assert!(!is_track_uri(bad), "{bad:?}");
        }
    }

    #[test]
    fn keys_and_containment() {
        assert_eq!(path_key("/music/"), "/music");
        assert_eq!(path_key("/"), "/");
        assert!(key_is_under("/music/a.flac", "/music"));
        assert!(!key_is_under("/musicx/a.flac", "/music"));
        assert!(!key_is_under("/music", "/music"));
        assert!(key_is_under("/a", "/"));
        if cfg!(windows) {
            assert_eq!(path_key(r"C:\Music\A.flac"), "c:/music/a.flac");
        }
    }

    #[test]
    fn lexical_normalise_resolves_dots() {
        let p = lexical_normalise(Path::new("/a/b/../c/./d")).unwrap();
        assert_eq!(p, PathBuf::from("/a/c/d"));
        assert_eq!(lexical_normalise(Path::new("/..")), None);
        assert_eq!(lexical_normalise(Path::new("a/../..")), None);
    }

    #[test]
    fn the_art_scheme_is_the_art_prefix_without_its_colon() {
        assert_eq!(ART_PREFIX, format!("{ART_SCHEME}:"));
    }
}
