//! Favourites (D6): `favourites.json` under app data, user data. `{version: 1, paths: [...]}`
//! keyed by absolute path so a rescan keeps them, written through a temp file and a rename. A file
//! of another version, or one that does not parse, reads as empty and is left exactly as it is
//! until a favourite changes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ids::path_key;
use crate::index::write_atomic;

pub const FAVOURITES_FILE: &str = "favourites.json";
pub const FAVOURITES_VERSION: u32 = 1;
/// Most favourites kept.
const MAX_FAVOURITES: usize = 200_000;
/// A larger favourites file is not read.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct FavFile {
    version: u32,
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// Why a favourite could not be saved. The text is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveFailed;

pub struct Favourites {
    file: PathBuf,
    state: Mutex<State>,
}

struct State {
    /// The paths in the order they were added (oldest first).
    paths: Vec<String>,
    /// Their [`path_key`]s.
    keys: HashSet<String>,
}

impl State {
    fn new(paths: Vec<String>) -> State {
        let keys = paths.iter().map(|p| path_key(p)).collect();
        State { paths, keys }
    }
}

impl Favourites {
    /// The favourites in `data_dir` (nothing is written here).
    pub fn open(data_dir: &Path) -> Favourites {
        let file = data_dir.join(FAVOURITES_FILE);
        let state = State::new(read(&file));
        Favourites {
            file,
            state: Mutex::new(state),
        }
    }

    /// The favourite paths, newest first.
    pub fn newest_first(&self) -> Vec<String> {
        let mut v = self.lock().paths.clone();
        v.reverse();
        v
    }

    pub fn contains(&self, path: &str) -> bool {
        self.lock().keys.contains(&path_key(path))
    }

    /// Add or remove `path`, then write the file. The list in memory changes only when the write
    /// succeeds.
    pub fn set(&self, path: &str, liked: bool) -> Result<(), SaveFailed> {
        let mut state = self.lock();
        let key = path_key(path);
        if liked == state.keys.contains(&key) {
            return Ok(());
        }
        let mut next = state.paths.clone();
        if liked {
            if next.len() >= MAX_FAVOURITES {
                return Err(SaveFailed);
            }
            next.push(path.to_owned());
        } else {
            next.retain(|p| path_key(p) != key);
        }
        let bytes = serde_json::to_vec_pretty(&FavFile {
            version: FAVOURITES_VERSION,
            paths: next.clone(),
        })
        .map_err(|_| SaveFailed)?;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|_| SaveFailed)?;
        }
        write_atomic(&self.file, &bytes).map_err(|_| SaveFailed)?;
        *state = State::new(next);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn read(file: &Path) -> Vec<String> {
    let Ok(meta) = std::fs::metadata(file) else {
        return Vec::new();
    };
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> Vec<String> {
    match serde_json::from_slice::<VersionOnly>(bytes) {
        Ok(v) if v.version == FAVOURITES_VERSION => {}
        _ => return Vec::new(),
    }
    let Ok(file) = serde_json::from_slice::<FavFile>(bytes) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut keys = HashSet::new();
    for p in file.paths {
        if out.len() >= MAX_FAVOURITES {
            break;
        }
        if !p.is_empty() && !p.chars().any(char::is_control) && keys.insert(path_key(&p)) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keeps_good_paths_once() {
        let got = parse(br#"{"version":1,"paths":["/a.flac","","/b\n.flac","/a.flac","/c.wav"]}"#);
        assert_eq!(got, ["/a.flac", "/c.wav"]);
        assert!(parse(br#"{"version":2,"paths":["/a.flac"]}"#).is_empty());
        assert!(parse(b"{\"version\":1,\"paths\":[").is_empty());
        assert!(parse(&[0x00, 0xff, 0x10]).is_empty());
    }
}
