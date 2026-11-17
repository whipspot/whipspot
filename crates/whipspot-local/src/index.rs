//! The library index cache (D5): `library-index.json` in the cache root, safe to delete. Any
//! problem reading it (missing, too big, not JSON, another version) means "no index": the next
//! scan rebuilds it. Nothing here is ever shown as an error.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use whipspot_core::text::display_text;

use crate::ids::{key_is_under, path_key};
use crate::scan::{MAX_PLAYLISTS, is_indexed_extension, is_playlist_extension};

pub const INDEX_FILE: &str = "library-index.json";
pub const INDEX_VERSION: u32 = 1;
/// Most entries an index holds; files past it are skipped.
pub const MAX_ENTRIES: usize = 200_000;
/// A larger index file is not read (about 2.5 KiB per entry at the cap).
const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;
/// Longest tag text kept, in characters.
pub const MAX_TEXT_CHARS: usize = 200;

/// One audio file as the last scan saw it. Tag texts are display-clean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The absolute path as the folder walk produced it.
    pub path: String,
    /// Modification time in nanoseconds since the Unix epoch (0 when unknown).
    pub mtime: u64,
    pub size: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album_artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub track_no: Option<u32>,
    #[serde(default)]
    pub disc_no: Option<u32>,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub rg_track_db: Option<f64>,
    #[serde(default)]
    pub rg_track_peak: Option<f64>,
    #[serde(default)]
    pub rg_album_db: Option<f64>,
    #[serde(default)]
    pub rg_album_peak: Option<f64>,
    /// The file carries an embedded picture.
    #[serde(default)]
    pub has_picture: bool,
    /// Its directory holds a cover image (`cover`, `folder` or `front`), as of the last scan.
    #[serde(default)]
    pub folder_art: bool,
}

impl Entry {
    /// The entry with every text display-clean and every number in range: an index file is read
    /// back as untrusted as the tags it came from.
    pub fn sanitised(mut self) -> Entry {
        let clean = |s: Option<String>| {
            s.map(|s| display_text(&s, MAX_TEXT_CHARS))
                .filter(|s| !s.is_empty())
        };
        self.title = clean(self.title);
        self.artist = clean(self.artist);
        self.album_artist = clean(self.album_artist);
        self.album = clean(self.album);
        self.rg_track_db = self.rg_track_db.filter(|g| valid_gain(*g));
        self.rg_album_db = self.rg_album_db.filter(|g| valid_gain(*g));
        self.rg_track_peak = self.rg_track_peak.filter(|p| valid_peak(*p));
        self.rg_album_peak = self.rg_album_peak.filter(|p| valid_peak(*p));
        self
    }
}

/// A ReplayGain value worth applying: finite, within +-60 dB.
pub fn valid_gain(db: f64) -> bool {
    db.is_finite() && db.abs() <= 60.0
}

/// A ReplayGain peak: finite, positive, at most 100 (40 dB over full scale).
pub fn valid_peak(peak: f64) -> bool {
    peak.is_finite() && peak > 0.0 && peak <= 100.0
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    folders: Vec<String>,
    entries: Vec<Entry>,
    /// The playlist files the scan found (read again at every load). Optional in version 1.
    #[serde(default)]
    playlists: Vec<String>,
}

/// What a loaded index gives back.
#[derive(Debug, Default)]
pub struct Loaded {
    pub entries: Vec<Entry>,
    pub playlists: Vec<String>,
}

#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

pub fn index_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(INDEX_FILE)
}

/// The entries of the index in `cache_dir` that lie inside one of `folders` and have an indexed
/// extension, at most [`MAX_ENTRIES`], sanitised, and its playlists inside `folders` (at most
/// [`MAX_PLAYLISTS`]). Empty on any problem with the file.
pub fn load(cache_dir: &Path, folders: &[String]) -> Loaded {
    let Some(bytes) = read_capped(&index_path(cache_dir)) else {
        return Loaded::default();
    };
    parse(&bytes, folders)
}

pub(crate) fn parse(bytes: &[u8], folders: &[String]) -> Loaded {
    // The version first, so another version's shape is never parsed as this one.
    match serde_json::from_slice::<VersionOnly>(bytes) {
        Ok(v) if v.version == INDEX_VERSION => {}
        _ => return Loaded::default(),
    }
    let Ok(file) = serde_json::from_slice::<IndexFile>(bytes) else {
        return Loaded::default();
    };
    let folder_keys: Vec<String> = folders.iter().map(|f| path_key(f)).collect();
    let inside = |p: &str| {
        let key = path_key(p);
        folder_keys.iter().any(|f| key_is_under(&key, f))
    };
    let entries = file
        .entries
        .into_iter()
        .filter(|e| is_indexed_extension(Path::new(&e.path)) && inside(&e.path))
        .take(MAX_ENTRIES)
        .map(Entry::sanitised)
        .collect();
    let playlists = file
        .playlists
        .into_iter()
        .filter(|p| is_playlist_extension(Path::new(p)) && inside(p))
        .take(MAX_PLAYLISTS)
        .collect();
    Loaded { entries, playlists }
}

fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INDEX_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// Write the index (a temp file, then a rename). Failures are ignored: it is a cache.
pub fn save(cache_dir: &Path, folders: &[String], entries: &[Entry], playlists: &[String]) {
    let file = IndexFile {
        version: INDEX_VERSION,
        folders: folders.to_vec(),
        entries: entries.iter().take(MAX_ENTRIES).cloned().collect(),
        playlists: playlists.iter().take(MAX_PLAYLISTS).cloned().collect(),
    };
    let Ok(bytes) = serde_json::to_vec(&file) else {
        return;
    };
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = write_atomic(&index_path(cache_dir), &bytes);
}

/// Write `bytes` to `path` through a sibling temp file and a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            path: path.to_owned(),
            mtime: 1,
            size: 2,
            title: Some("T".into()),
            artist: None,
            album_artist: None,
            album: None,
            track_no: Some(1),
            disc_no: None,
            year: None,
            duration_ms: 3,
            rg_track_db: Some(-6.5),
            rg_track_peak: None,
            rg_album_db: None,
            rg_album_peak: None,
            has_picture: false,
            folder_art: false,
        }
    }

    fn json(version: u32, entries: &[Entry]) -> Vec<u8> {
        serde_json::to_vec(&IndexFile {
            version,
            folders: vec![],
            entries: entries.to_vec(),
            playlists: vec!["/m/p.m3u".into(), "/other/q.m3u8".into(), "/m/r.txt".into()],
        })
        .unwrap()
    }

    #[test]
    fn round_trips_and_filters_by_folder_and_extension() {
        let folders = vec!["/m".to_owned()];
        let entries = [
            entry("/m/a.flac"),
            entry("/m/b.txt"),
            entry("/other/c.flac"),
            entry("/m/sub/d.WAV"),
        ];
        let loaded = parse(&json(1, &entries), &folders);
        assert_eq!(loaded.playlists, ["/m/p.m3u"]);
        let got = loaded.entries;
        let paths: Vec<_> = got.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/m/a.flac", "/m/sub/d.WAV"]);
        assert_eq!(got[0], entries[0]);
    }

    #[test]
    fn a_bad_file_is_no_index() {
        let folders = vec!["/m".to_owned()];
        let good = json(1, &[entry("/m/a.flac")]);
        assert_eq!(parse(&good, &folders).entries.len(), 1);
        assert!(
            parse(&json(2, &[entry("/m/a.flac")]), &folders)
                .entries
                .is_empty()
        );
        assert!(parse(&good[..good.len() / 2], &folders).entries.is_empty());
        assert!(parse(b"", &folders).entries.is_empty());
        assert!(
            parse(&[0xff, 0x00, 0x13, 0x37], &folders)
                .entries
                .is_empty()
        );
        assert!(parse(br#"{"version":1}"#, &folders).entries.is_empty());
    }

    #[test]
    fn sanitises_what_it_reads() {
        let mut e = entry("/m/a.flac");
        e.title = Some("a\u{202E}b\n".into());
        e.artist = Some("   ".into());
        e.rg_track_db = Some(1e9);
        e.rg_track_peak = Some(-1.0);
        let got = parse(&json(1, &[e]), &["/m".to_owned()]).entries;
        assert_eq!(got[0].title.as_deref(), Some("ab"));
        assert_eq!(got[0].artist, None);
        assert_eq!(got[0].rg_track_db, None);
        assert_eq!(got[0].rg_track_peak, None);
    }

    #[test]
    fn caps_the_entry_count() {
        let entries: Vec<Entry> = (0..MAX_ENTRIES + 5)
            .map(|i| entry(&format!("/m/{i}.flac")))
            .collect();
        assert_eq!(
            parse(&json(1, &entries), &["/m".to_owned()]).entries.len(),
            MAX_ENTRIES
        );
    }
}
