//! The in-memory catalog built from the index (D7): tracks with their ids, albums grouped by tag,
//! playlists from `.m3u` files, search. Built off the UI thread (the scan) and swapped in whole.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use whipspot_core::provider::{
    Album, AlbumPage, ImageRef, LibErrorKind, LibraryError, Playlist, PlaylistPage, Readable,
    Track, TrackPage,
};
use whipspot_core::text::display_text;

use crate::ids::{ALBUM_PREFIX, ART_PREFIX, IdMinter, PLAYLIST_PREFIX, TRACK_PREFIX, path_key};
use crate::index::{Entry, MAX_TEXT_CHARS};
use crate::m3u;

/// Rows per page, like the other providers' pages.
pub const PAGE: usize = 100;
pub const UNKNOWN_ARTIST: &str = "Unknown artist";
pub const UNKNOWN_ALBUM: &str = "Unknown album";
pub const UNKNOWN_TITLE: &str = "Unknown title";
const UNTITLED_PLAYLIST: &str = "Playlist";
/// Longest search query used, in characters.
const MAX_QUERY_CHARS: usize = 200;

pub struct TrackRec {
    pub entry: Entry,
    pub track: Track,
    key: String,
    title_lc: String,
    artist_lc: String,
    album_lc: String,
}

struct AlbumRec {
    album: Album,
    tracks: Vec<usize>,
}

struct PlaylistRec {
    playlist: Playlist,
    tracks: Vec<usize>,
}

#[derive(Default)]
pub struct Catalog {
    /// In album order (the albums' order, each album's tracks in disc/track order).
    pub tracks: Vec<TrackRec>,
    by_uri: HashMap<String, usize>,
    by_key: HashMap<String, usize>,
    albums: Vec<AlbumRec>,
    album_by_id: HashMap<String, usize>,
    playlists: Vec<PlaylistRec>,
    playlist_by_id: HashMap<String, usize>,
    /// The playlist files the catalog was built from.
    pub playlist_paths: Vec<String>,
}

fn not_found(what: &'static str) -> LibraryError {
    LibraryError::new(LibErrorKind::InvalidArgument, what)
}

/// The art ref of the track `uri`.
fn art_ref(uri: &str) -> ImageRef {
    ImageRef {
        url: format!("{ART_PREFIX}{uri}"),
        width: None,
        height: None,
    }
}

/// The file name of `path` without its extension, display-clean; `None` when that is empty.
fn file_stem(path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_string_lossy().into_owned();
    Some(display_text(&stem, MAX_TEXT_CHARS)).filter(|s| !s.is_empty())
}

impl Catalog {
    /// The catalog of `entries` (any order, duplicates by path dropped) and the playlist files
    /// `playlist_paths` (read here; one that fails to read is left out).
    pub fn build(mut entries: Vec<Entry>, playlist_paths: Vec<String>) -> Catalog {
        let mut keyed: Vec<(String, Entry)> =
            entries.drain(..).map(|e| (path_key(&e.path), e)).collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        keyed.dedup_by(|a, b| a.0 == b.0);

        // Ids are minted in path-key order so a collision resolves the same way every time.
        let mut track_ids = IdMinter::default();
        let minted: Vec<(String, String, Entry)> = keyed
            .into_iter()
            .map(|(key, e)| (format!("{TRACK_PREFIX}{}", track_ids.id(&key)), key, e))
            .collect();

        // Group into albums by (album artist or artist, album), lowercased.
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for (i, (_, _, e)) in minted.iter().enumerate() {
            let artist = e
                .album_artist
                .as_deref()
                .or(e.artist.as_deref())
                .unwrap_or(UNKNOWN_ARTIST)
                .to_lowercase();
            let album = e.album.as_deref().unwrap_or(UNKNOWN_ALBUM).to_lowercase();
            groups.entry((artist, album)).or_default().push(i);
        }
        let mut album_ids = IdMinter::default();
        let grouped: Vec<(String, Vec<usize>)> = groups
            .into_iter()
            .map(|((artist, album), mut members)| {
                members.sort_by(|&a, &b| {
                    let (ea, eb) = (&minted[a].2, &minted[b].2);
                    (ea.disc_no.unwrap_or(1), ea.track_no.unwrap_or(u32::MAX))
                        .cmp(&(eb.disc_no.unwrap_or(1), eb.track_no.unwrap_or(u32::MAX)))
                        .then_with(|| minted[a].1.cmp(&minted[b].1))
                });
                let id = album_ids.id(&format!("{artist}\u{1}{album}"));
                (format!("{ALBUM_PREFIX}{id}"), members)
            })
            .collect();
        // BTreeMap order is (artist, album) already; keep it.
        let mut slots: Vec<Option<(String, String, Entry)>> =
            minted.into_iter().map(Some).collect();
        let mut cat = Catalog::default();
        for (album_id, members) in grouped {
            let mut idxs = Vec::with_capacity(members.len());
            for m in members {
                let Some((uri, key, entry)) = slots[m].take() else {
                    continue;
                };
                let rec = track_rec(uri, key, entry);
                idxs.push(cat.tracks.len());
                cat.tracks.push(rec);
            }
            let Some(&first) = idxs.first() else {
                continue;
            };
            let head = &cat.tracks[first];
            let artist = head
                .entry
                .album_artist
                .clone()
                .unwrap_or_else(|| head.track.artist.clone());
            let images = idxs
                .iter()
                .map(|&i| &cat.tracks[i].track)
                .find(|t| !t.images.is_empty())
                .map(|t| t.images.clone())
                .unwrap_or_default();
            let album = Album {
                id: album_id.clone(),
                name: head.track.album.clone(),
                artist,
                track_count: u32::try_from(idxs.len()).ok(),
                images,
                year: idxs.iter().find_map(|&i| cat.tracks[i].track.year),
            };
            cat.album_by_id.insert(album_id, cat.albums.len());
            cat.albums.push(AlbumRec {
                album,
                tracks: idxs,
            });
        }
        for (i, t) in cat.tracks.iter().enumerate() {
            cat.by_uri.insert(t.track.uri.clone(), i);
            cat.by_key.insert(t.key.clone(), i);
        }

        let mut paths: Vec<(String, String)> = playlist_paths
            .into_iter()
            .map(|p| (path_key(&p), p))
            .collect();
        paths.sort_by(|a, b| a.0.cmp(&b.0));
        paths.dedup_by(|a, b| a.0 == b.0);
        let mut playlist_ids = IdMinter::default();
        for (key, path) in paths {
            let id = format!("{PLAYLIST_PREFIX}{}", playlist_ids.id(&key));
            let Some(lines) = m3u::read(Path::new(&path)) else {
                continue;
            };
            let mut seen = HashSet::new();
            let tracks: Vec<usize> = lines
                .iter()
                .filter_map(|p| cat.by_key.get(&path_key(&p.to_string_lossy())).copied())
                .filter(|i| seen.insert(*i))
                .collect();
            let images = tracks
                .iter()
                .map(|&i| &cat.tracks[i].track)
                .find(|t| !t.images.is_empty())
                .map(|t| t.images.clone())
                .unwrap_or_default();
            let playlist = Playlist {
                id: id.clone(),
                name: file_stem(&path).unwrap_or_else(|| UNTITLED_PLAYLIST.to_owned()),
                readable: Readable::Readable,
                track_count: u32::try_from(tracks.len()).ok(),
                images,
            };
            cat.playlist_by_id.insert(id, cat.playlists.len());
            cat.playlists.push(PlaylistRec { playlist, tracks });
            cat.playlist_paths.push(path);
        }
        cat
    }

    /// The entries, for the next scan to reuse and the index to save.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.tracks.iter().map(|t| &t.entry)
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// The track `uri` (an exact id; never parsed as a path).
    pub fn by_uri(&self, uri: &str) -> Option<&TrackRec> {
        self.by_uri.get(uri).map(|&i| &self.tracks[i])
    }

    pub fn playlists_page(&self, offset: u32) -> PlaylistPage {
        let (items, next_offset, total) = page(&self.playlists, offset, |p| p.playlist.clone());
        PlaylistPage {
            items,
            skipped: 0,
            next_offset,
            total,
        }
    }

    pub fn albums_page(&self, offset: u32) -> AlbumPage {
        let (items, next_offset, total) = page(&self.albums, offset, |a| a.album.clone());
        AlbumPage {
            items,
            skipped: 0,
            next_offset,
            total,
        }
    }

    pub fn album_tracks(&self, id: &str, offset: u32) -> Result<TrackPage, LibraryError> {
        let &i = self
            .album_by_id
            .get(id)
            .ok_or_else(|| not_found("this album is not in the library"))?;
        Ok(self.track_page(&self.albums[i].tracks, offset))
    }

    pub fn playlist_items(&self, id: &str, offset: u32) -> Result<TrackPage, LibraryError> {
        let &i = self
            .playlist_by_id
            .get(id)
            .ok_or_else(|| not_found("this playlist is not in the library"))?;
        Ok(self.track_page(&self.playlists[i].tracks, offset))
    }

    /// Case-insensitive substring search on title, artist and album: title matches first, then
    /// artist, then album, each in catalog order. An empty query finds nothing.
    pub fn search(&self, query: &str, offset: u32) -> TrackPage {
        let q: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
        let q = q.to_lowercase();
        if q.is_empty() {
            return self.track_page(&[], offset);
        }
        let mut ranked: [Vec<usize>; 3] = Default::default();
        for (i, t) in self.tracks.iter().enumerate() {
            if t.title_lc.contains(&q) {
                ranked[0].push(i);
            } else if t.artist_lc.contains(&q) {
                ranked[1].push(i);
            } else if t.album_lc.contains(&q) {
                ranked[2].push(i);
            }
        }
        let all: Vec<usize> = ranked.concat();
        self.track_page(&all, offset)
    }

    /// The tracks of `paths` (favourites, newest first) that are indexed, as a page.
    pub fn tracks_of_paths(&self, paths: &[String], offset: u32) -> TrackPage {
        let mut seen = HashSet::new();
        let idxs: Vec<usize> = paths
            .iter()
            .filter_map(|p| self.by_key.get(&path_key(p)).copied())
            .filter(|i| seen.insert(*i))
            .collect();
        self.track_page(&idxs, offset)
    }

    fn track_page(&self, idxs: &[usize], offset: u32) -> TrackPage {
        let (items, next_offset, total) = page(idxs, offset, |&i| self.tracks[i].track.clone());
        TrackPage {
            items,
            skipped: 0,
            next_offset,
            total,
            truncated: false,
        }
    }
}

/// One page of `all` from `offset`: the items, the next offset (`None` at the end) and the total.
fn page<T, U>(all: &[T], offset: u32, f: impl Fn(&T) -> U) -> (Vec<U>, Option<u32>, Option<u32>) {
    let start = (offset as usize).min(all.len());
    let end = (start + PAGE).min(all.len());
    let items = all[start..end].iter().map(f).collect();
    let next = (end < all.len()).then(|| u32::try_from(end).ok()).flatten();
    (items, next, u32::try_from(all.len()).ok())
}

fn track_rec(uri: String, key: String, entry: Entry) -> TrackRec {
    let name = entry
        .title
        .clone()
        .or_else(|| file_stem(&entry.path))
        .unwrap_or_else(|| UNKNOWN_TITLE.to_owned());
    let artist = entry
        .artist
        .clone()
        .or_else(|| entry.album_artist.clone())
        .unwrap_or_else(|| UNKNOWN_ARTIST.to_owned());
    let album = entry
        .album
        .clone()
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_owned());
    let images = if entry.has_picture || entry.folder_art {
        vec![art_ref(&uri)]
    } else {
        Vec::new()
    };
    let track = Track {
        uri,
        name,
        artist,
        album,
        duration_ms: entry.duration_ms,
        images,
        year: entry.year,
    };
    TrackRec {
        title_lc: track.name.to_lowercase(),
        artist_lc: track.artist.to_lowercase(),
        album_lc: track.album.to_lowercase(),
        entry,
        track,
        key,
    }
}
