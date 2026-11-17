//! The folder walk and the tag reader. Every file is untrusted input (rule 5): the walk follows
//! no symlink or junction below a folder, depth and counts are capped, and a file whose tags do
//! not read is skipped without a word.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::UNIX_EPOCH;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};

use crate::ids::path_key;
use crate::index::{Entry, MAX_ENTRIES, valid_gain, valid_peak};

/// The extensions the library indexes (D3); `.opus` and the rest are skipped.
const AUDIO_EXTENSIONS: [&str; 10] = [
    "flac", "wav", "aif", "aiff", "m4a", "mp4", "mp3", "ogg", "oga", "caf",
];
const PLAYLIST_EXTENSIONS: [&str; 2] = ["m3u", "m3u8"];
/// The stems of a folder image (D9), with [`IMAGE_EXTENSIONS`].
const COVER_STEMS: [&str; 3] = ["cover", "folder", "front"];
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
/// Most playlist files a scan keeps.
pub const MAX_PLAYLISTS: usize = 10_000;
/// Deepest directory level walked below a folder.
const MAX_DEPTH: usize = 64;

fn has_extension(path: &Path, set: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| set.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

pub fn is_indexed_extension(path: &Path) -> bool {
    has_extension(path, &AUDIO_EXTENSIONS)
}

pub fn is_playlist_extension(path: &Path) -> bool {
    has_extension(path, &PLAYLIST_EXTENSIONS)
}

/// Whether `path` names a folder image: `cover`, `folder` or `front` with `.jpg`, `.jpeg` or
/// `.png`, in any case.
pub fn is_cover_name(path: &Path) -> bool {
    has_extension(path, &IMAGE_EXTENSIONS)
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| COVER_STEMS.iter().any(|c| s.eq_ignore_ascii_case(c)))
}

/// What a walk found.
#[derive(Default)]
pub struct Walked {
    /// Audio files with an indexed extension, at most [`MAX_ENTRIES`].
    pub audio: Vec<PathBuf>,
    /// Playlist files, at most [`MAX_PLAYLISTS`].
    pub playlists: Vec<PathBuf>,
    /// Keys ([`path_key`]) of the directories holding a folder image.
    pub cover_dirs: HashSet<String>,
}

/// Walk `folders` (each followed itself, nothing below it that is a symlink or junction). Hidden
/// entries (a leading dot, such as macOS `._` files) are skipped; unreadable directories too.
pub fn walk(folders: &[String]) -> Walked {
    let mut out = Walked::default();
    let mut seen_roots: Vec<String> = Vec::new();
    for folder in folders {
        let key = path_key(folder);
        if seen_roots.contains(&key) {
            continue;
        }
        seen_roots.push(key);
        let mut stack = vec![(PathBuf::from(folder), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            let mut has_cover = false;
            for item in read.flatten() {
                let name = item.file_name();
                if name.to_str().is_none_or(|n| n.starts_with('.')) {
                    // Hidden, or not valid Unicode (an id key and the UI need text).
                    continue;
                }
                // `DirEntry::file_type` does not follow links; on Windows a junction is a
                // symlink here too.
                let Ok(kind) = item.file_type() else {
                    continue;
                };
                if kind.is_symlink() {
                    continue;
                }
                let path = item.path();
                if kind.is_dir() {
                    if depth < MAX_DEPTH {
                        stack.push((path, depth + 1));
                    }
                } else if kind.is_file() {
                    if is_indexed_extension(&path) {
                        if out.audio.len() < MAX_ENTRIES {
                            out.audio.push(path);
                        }
                    } else if is_playlist_extension(&path) {
                        if out.playlists.len() < MAX_PLAYLISTS {
                            out.playlists.push(path);
                        }
                    } else if is_cover_name(&path) {
                        has_cover = true;
                    }
                }
            }
            if has_cover {
                out.cover_dirs.insert(path_key(&dir.to_string_lossy()));
            }
        }
    }
    out
}

/// Modification time (ns since the epoch, 0 when unknown) and size of a file.
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    Some((mtime, meta.len()))
}

/// Build the entries for `walked.audio`: an entry of `previous` (by path key) whose mtime and
/// size are unchanged is reused, the rest have their tags read. Files that fail are skipped.
/// `seen` counts the files looked at.
pub fn entries(walked: &Walked, previous: &HashMap<String, Entry>, seen: &AtomicU32) -> Vec<Entry> {
    let mut out = Vec::with_capacity(walked.audio.len());
    for path in &walked.audio {
        seen.fetch_add(1, Ordering::Relaxed);
        let Some(text) = path.to_str() else {
            continue;
        };
        let Some((mtime, size)) = stamp(path) else {
            continue;
        };
        if size == 0 {
            continue;
        }
        let key = path_key(text);
        let folder_art = path
            .parent()
            .is_some_and(|d| walked.cover_dirs.contains(&path_key(&d.to_string_lossy())));
        let entry = match previous.get(&key) {
            Some(e) if e.mtime == mtime && e.size == size && e.path == text => Some(e.clone()),
            _ => read_file(path, text, mtime, size),
        };
        if let Some(mut e) = entry {
            e.folder_art = folder_art;
            out.push(e);
        }
    }
    out
}

fn read_file(path: &Path, text: &str, mtime: u64, size: u64) -> Option<Entry> {
    let file = fs::File::open(path).ok()?;
    let tags = read_tags_caught(BufReader::new(file), path)?;
    Some(tags.into_entry(text.to_owned(), mtime, size))
}

/// What the tag reader takes from a file.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<u16>,
    pub duration_ms: u64,
    pub rg_track_db: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_db: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub has_picture: bool,
}

impl Tags {
    fn into_entry(self, path: String, mtime: u64, size: u64) -> Entry {
        Entry {
            path,
            mtime,
            size,
            title: self.title,
            artist: self.artist,
            album_artist: self.album_artist,
            album: self.album,
            track_no: self.track_no,
            disc_no: self.disc_no,
            year: self.year,
            duration_ms: self.duration_ms,
            rg_track_db: self.rg_track_db,
            rg_track_peak: self.rg_track_peak,
            rg_album_db: self.rg_album_db,
            rg_album_peak: self.rg_album_peak,
            has_picture: self.has_picture,
            folder_art: false,
        }
        .sanitised()
    }
}

/// [`read_tags`] with a panic in the parser turned into a skipped file (rule 5). The fuzz test
/// calls [`read_tags`] itself so a panic there still fails it.
fn read_tags_caught<R: Read + Seek>(reader: R, path: &Path) -> Option<Tags> {
    let caf = has_extension(path, &["caf"]);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        if caf {
            read_caf(reader)
        } else {
            read_tags(reader)
        }
    }))
    .ok()
    .flatten()
}

/// The tags of an audio file in `reader`, or `None` when it is not one of the formats the
/// library plays (by content, whatever its extension) or does not parse.
pub fn read_tags<R: Read + Seek>(reader: R) -> Option<Tags> {
    let options = ParseOptions::new().read_cover_art(true);
    let probe = Probe::new(reader).options(options).guess_file_type().ok()?;
    if !matches!(
        probe.file_type(),
        Some(
            FileType::Flac
                | FileType::Wav
                | FileType::Aiff
                | FileType::Mp4
                | FileType::Mpeg
                | FileType::Vorbis
        )
    ) {
        return None;
    }
    let file = probe.read().ok()?;
    Some(tags_of(&file))
}

/// Core Audio files: lofty reads no CAF, so the file is kept untagged when its header is CAF's.
fn read_caf<R: Read + Seek>(mut reader: R) -> Option<Tags> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic).ok()?;
    (&magic[..4] == b"caff" && magic[4..6] == [0, 1]).then(Tags::default)
}

/// [`read_tags`] over bytes in memory (the fuzz test).
pub fn read_tags_from_bytes(bytes: &[u8]) -> Option<Tags> {
    read_tags(Cursor::new(bytes))
}

fn tags_of(file: &TaggedFile) -> Tags {
    let duration_ms = u64::try_from(file.properties().duration().as_millis()).unwrap_or(u64::MAX);
    let has_picture = file.tags().iter().any(|t| t.picture_count() > 0);
    let tag = file.primary_tag().or_else(|| file.first_tag());
    let Some(tag) = tag else {
        return Tags {
            duration_ms,
            has_picture,
            ..Tags::default()
        };
    };
    let text = |s: Option<std::borrow::Cow<'_, str>>| s.map(|s| s.into_owned());
    let string = |k: ItemKey| tag.get_string(k).map(str::to_owned);
    let number = |k: ItemKey| parse_float(tag.get_string(k)?);
    Tags {
        title: text(tag.title()),
        artist: text(tag.artist()),
        album_artist: string(ItemKey::AlbumArtist),
        album: text(tag.album()),
        track_no: tag.track().filter(|n| *n > 0),
        disc_no: tag.disk().filter(|n| *n > 0),
        year: year_of(tag),
        duration_ms,
        rg_track_db: number(ItemKey::ReplayGainTrackGain).filter(|g| valid_gain(*g)),
        rg_track_peak: number(ItemKey::ReplayGainTrackPeak).filter(|p| valid_peak(*p)),
        rg_album_db: number(ItemKey::ReplayGainAlbumGain).filter(|g| valid_gain(*g)),
        rg_album_peak: number(ItemKey::ReplayGainAlbumPeak).filter(|p| valid_peak(*p)),
        has_picture,
    }
}

fn year_of(tag: &Tag) -> Option<u16> {
    let from_date = tag.date().map(|d| d.year);
    let from_text = || {
        let s = tag
            .get_string(ItemKey::Year)
            .or_else(|| tag.get_string(ItemKey::RecordingDate))?;
        let digits: String = s.trim().chars().take(4).collect();
        (digits.len() == 4).then(|| digits.parse::<u16>().ok())?
    };
    from_date
        .or_else(from_text)
        .filter(|y| (1..=9999).contains(y))
}

/// A ReplayGain text such as `-6.50 dB` or `0.987654` as a number.
pub fn parse_float(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s
        .strip_suffix("dB")
        .or_else(|| s.strip_suffix("db"))
        .or_else(|| s.strip_suffix("DB"))
        .unwrap_or(s)
        .trim();
    if s.is_empty() || s.len() > 32 {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_sets() {
        assert!(is_indexed_extension(Path::new("a/b.FLAC")));
        assert!(is_indexed_extension(Path::new("b.caf")));
        assert!(!is_indexed_extension(Path::new("b.opus")));
        assert!(!is_indexed_extension(Path::new("flac")));
        assert!(is_playlist_extension(Path::new("x.M3U8")));
        assert!(is_cover_name(Path::new("Folder.JPG")));
        assert!(is_cover_name(Path::new("front.png")));
        assert!(!is_cover_name(Path::new("back.png")));
        assert!(!is_cover_name(Path::new("cover.gif")));
    }

    #[test]
    fn gains_parse() {
        assert_eq!(parse_float("-6.50 dB"), Some(-6.5));
        assert_eq!(parse_float(" +2.1dB "), Some(2.1));
        assert_eq!(parse_float("0.987654"), Some(0.987654));
        assert_eq!(parse_float("NaN"), None);
        assert_eq!(parse_float("inf dB"), None);
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("dB"), None);
    }

    #[test]
    fn caf_needs_its_header() {
        let mut good = b"caff\x00\x01\x00\x00".to_vec();
        good.extend([0; 16]);
        assert!(read_caf(Cursor::new(good)).is_some());
        assert!(read_caf(Cursor::new(b"caff".to_vec())).is_none());
        assert!(read_caf(Cursor::new(b"RIFF\x00\x01\x00\x00".to_vec())).is_none());
    }
}
