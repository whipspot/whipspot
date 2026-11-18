//! Fakes shared by the provider's tests (steps 6, 7 and 8 use them).
//!
//! `FakeYt` is an `Http` that plays YouTube Music for one synthetic account: it answers InnerTube
//! posts by endpoint and browse id in the shapes ytmusicapi's parsers read, with continuations of
//! both kinds, and it serves canned bytes to GETs (a fake tool release, later fake audio). Nothing
//! here is captured from a real account; every id, title and thumbnail is made up.
//!
//! The account:
//! - 2 playlists, `PLAYLIST_ONE` and `PLAYLIST_TWO`, each with 3 tracks (and a "New playlist"
//!   button that comes first in the grid, as in the real answer);
//! - 1 saved album, `ALBUM`, with 3 tracks;
//! - 5 releases in all (`RELEASES`), each an album page, 3 artists with artist pages (the second
//!   has a "More" grid for its albums), and an album saved or not through its audio playlist
//!   (`like/like` with `target.playlistId`); one release names no audio playlist;
//! - 3 liked tracks (`like/like` and `like/removelike` change them);
//! - a search that matches the title of any of the 10 songs.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pgp::composed::{
    ArmorOptions, DetachedSignature, KeyType, SecretKeyParamsBuilder, SignedPublicKey,
    SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::ser::Serialize;
use pgp::types::Password;
use rand::thread_rng;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use whipspot_core::engine::{Engine, EngineConfig};
use whipspot_core::provider::{LoginInput, Provider};
use whipspot_testkit::MemoryStore;
use whipspot_ytmusic::auth::{INPUT_PATH, STEP_COOKIES, STEP_HELPER};
use whipspot_ytmusic::http::{Cap, ContentRange, Http, HttpError, RangeResponse, Response};
use whipspot_ytmusic::run::{Output, RunError, Runner};
use whipspot_ytmusic::tools::{Managed, QJS_RELEASE_BASE, ToolEnv, Trust, YTDLP_RELEASE_BASE};
use whipspot_ytmusic::{YtDeps, YtMusicProvider};

pub const PLAYLIST_ONE: &str = "PLfake0000000000001";
pub const PLAYLIST_TWO: &str = "PLfake0000000000002";
pub const ALBUM: &str = "MPREb_fakealbum01";
pub const SENTINEL_COOKIE: &str = "SENTINEL-COOKIE-VALUE-123";
pub const SENTINEL_SAPISID: &str = "SENTINEL-SAPISID-456";

pub fn playlist_uri(id: &str) -> String {
    format!("ytmusic:playlist:{id}")
}

pub fn album_uri(id: &str) -> String {
    format!("ytmusic:album:{id}")
}

pub fn track_uri(id: &str) -> String {
    format!("ytmusic:track:{id}")
}

// ---- the catalog ------------------------------------------------------------------------

pub struct Song {
    pub id: &'static str,
    pub title: &'static str,
    pub artists: &'static [(&'static str, &'static str)],
    pub album: Option<(&'static str, &'static str)>,
    pub duration: &'static str,
}

const AR_A: (&str, &str) = ("Artist A", "UCartistA0000000000000001");
const AR_B: (&str, &str) = ("Artist B", "UCartistB0000000000000002");
const AR_ALB: (&str, &str) = ("Album Artist", "UCalbumArtist00000000003");

pub const SONGS: [Song; 10] = [
    Song {
        id: "trackAAAA01",
        title: "Moonlit Drive",
        artists: &[AR_A],
        album: Some(("First Record", "MPREb_other000001")),
        duration: "3:32",
    },
    Song {
        id: "trackAAAA02",
        title: "Paper Boats",
        artists: &[AR_A, AR_B],
        album: Some(("First Record", "MPREb_other000001")),
        duration: "4:05",
    },
    Song {
        id: "trackAAAA03",
        title: "Quiet Rooms",
        artists: &[AR_B],
        album: Some(("Second Record", "MPREb_other000002")),
        duration: "2:58",
    },
    Song {
        id: "trackAAAA04",
        title: "Night Bus",
        artists: &[AR_A],
        album: Some(("Second Record", "MPREb_other000002")),
        duration: "1:01:05",
    },
    Song {
        id: "trackAAAA05",
        title: "Glass Harbour",
        artists: &[AR_B],
        album: Some(("Third Record", "MPREb_other000003")),
        duration: "3:10",
    },
    Song {
        id: "trackAAAA06",
        title: "Tin Whistle",
        artists: &[AR_A],
        album: Some(("Third Record", "MPREb_other000003")),
        duration: "0:45",
    },
    Song {
        id: "trackAAAA07",
        title: "Album Opener",
        artists: &[AR_ALB],
        album: Some(("Fake Album", ALBUM)),
        duration: "3:00",
    },
    Song {
        id: "trackAAAA08",
        title: "Album Middle Night",
        artists: &[AR_ALB],
        album: Some(("Fake Album", ALBUM)),
        duration: "4:00",
    },
    Song {
        id: "trackAAAA09",
        title: "Album Closer",
        artists: &[AR_ALB],
        album: Some(("Fake Album", ALBUM)),
        duration: "5:00",
    },
    Song {
        id: "trackAAAA10",
        title: "Liked Only",
        artists: &[AR_B],
        album: Some(("Fourth Record", "MPREb_other000004")),
        duration: "2:30",
    },
];

fn song(id: &str) -> Option<&'static Song> {
    SONGS.iter().find(|s| s.id == id)
}

const PLAYLISTS: [(&str, &str, [&str; 3]); 2] = [
    (
        PLAYLIST_ONE,
        "Road Trip",
        ["trackAAAA01", "trackAAAA02", "trackAAAA03"],
    ),
    (
        PLAYLIST_TWO,
        "Evening",
        ["trackAAAA04", "trackAAAA05", "trackAAAA06"],
    ),
];
/// An album or single that has a page.
pub struct Release {
    pub id: &'static str,
    pub title: &'static str,
    /// "Album" or "Single", as the type run says.
    pub label: &'static str,
    pub year: &'static str,
    /// The audio playlist a save targets; `None`: the page names none.
    pub audio_playlist: Option<&'static str>,
}

pub const RELEASES: [Release; 5] = [
    Release {
        id: ALBUM,
        title: "Fake Album",
        label: "Album",
        year: "2019",
        audio_playlist: Some("OLAK5uy_fakealbum01"),
    },
    Release {
        id: "MPREb_other000001",
        title: "First Record",
        label: "Album",
        year: "2020",
        audio_playlist: Some("OLAK5uy_other000001"),
    },
    Release {
        id: "MPREb_other000002",
        title: "Second Record",
        label: "Album",
        year: "2021",
        audio_playlist: Some("OLAK5uy_other000002"),
    },
    Release {
        id: "MPREb_other000003",
        title: "Third Record",
        label: "Album",
        year: "2022",
        audio_playlist: None,
    },
    Release {
        id: "MPREb_other000004",
        title: "Fourth Record",
        label: "Single",
        year: "2023",
        audio_playlist: Some("OLAK5uy_other000004"),
    },
];

pub const ARTISTS: [(&str, &str); 3] = [AR_A, AR_B, AR_ALB];

pub fn release(id: &str) -> Option<&'static Release> {
    RELEASES.iter().find(|r| r.id == id)
}

/// The artists of the songs on a release, in order, once each.
fn release_artists(id: &str) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for s in SONGS.iter().filter(|s| s.album.is_some_and(|a| a.1 == id)) {
        for a in s.artists {
            if !out.contains(a) {
                out.push(*a);
            }
        }
    }
    out
}

/// The releases an artist has songs on.
fn artist_releases(uc: &str) -> Vec<&'static Release> {
    RELEASES
        .iter()
        .filter(|r| release_artists(r.id).iter().any(|a| a.1 == uc))
        .collect()
}

pub fn artist_uri(uc: &str) -> String {
    format!("ytmusic:artist:{uc}")
}

/// What the carousel of albums shows of an artist that has more than this many.
const CAROUSEL_SHOWS: usize = 2;

/// An artist's release in a carousel or a grid: the type, then the year; no artist (the page's own).
fn release_item(r: &Release) -> Value {
    json!({"musicTwoRowItemRenderer": {
        "title": {"runs": [{"text": r.title, "navigationEndpoint": {"browseEndpoint": {"browseId": r.id}}}]},
        "subtitle": {"runs": [{"text": r.label}, {"text": " • "}, {"text": r.year}]},
        "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(r.id)}}},
    }})
}

// ---- JSON builders (the shapes ytmusicapi reads) ---------------------------------------------

fn thumbs(seed: &str) -> Value {
    json!([
        {"url": format!("https://lh3.googleusercontent.com/{seed}=w60-h60"), "width": 60, "height": 60},
        {"url": format!("https://lh3.googleusercontent.com/{seed}=w226-h226"), "width": 226, "height": 226},
        {"url": format!("https://i.ytimg.com/vi/{seed}/sddefault.jpg"), "width": 640, "height": 480},
        {"url": format!("https://evil.example/{seed}.jpg"), "width": 1200, "height": 1200},
        {"url": format!("http://lh3.googleusercontent.com/{seed}=w1200"), "width": 1200, "height": 1200},
    ])
}

fn artist_run(a: (&str, &str)) -> Value {
    json!({
        "text": a.0,
        "navigationEndpoint": {"browseEndpoint": {
            "browseId": a.1,
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {
                "pageType": "MUSIC_PAGE_TYPE_ARTIST"}}}}
    })
}

fn album_run(a: (&str, &str)) -> Value {
    json!({
        "text": a.0,
        "navigationEndpoint": {"browseEndpoint": {
            "browseId": a.1,
            "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {
                "pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}
    })
}

fn artist_runs(artists: &[(&str, &str)]) -> Vec<Value> {
    let mut runs = Vec::new();
    for (i, a) in artists.iter().enumerate() {
        if i > 0 {
            runs.push(json!({"text": " & "}));
        }
        runs.push(artist_run(*a));
    }
    runs
}

#[derive(Clone, Copy, PartialEq)]
pub enum Layout {
    /// A playlist or the liked songs: title, artists, album, a fixed duration column.
    Playlist,
    /// An album page: title and a duration; the header names the album.
    Album,
    /// A search result: title, then one column of "Song • artists • album • duration".
    Search,
}

pub fn row(s: &Song, layout: Layout, liked: bool) -> Value {
    let like = if liked { "LIKE" } else { "INDIFFERENT" };
    let title_col = json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{
        "text": s.title,
        "navigationEndpoint": {"watchEndpoint": {"videoId": s.id}}}]}}});
    let mut flex = vec![title_col];
    let mut row = json!({
        "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
            "playNavigationEndpoint": {"watchEndpoint": {"videoId": s.id}}}}}},
        "menu": {"menuRenderer": {"topLevelButtons": [{"likeButtonRenderer": {"likeStatus": like}}]}},
    });
    match layout {
        Layout::Playlist => {
            flex.push(json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": artist_runs(s.artists)}}}));
            if let Some(a) = s.album {
                flex.push(json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [album_run(a)]}}}));
            }
            row["fixedColumns"] = json!([{"musicResponsiveListItemFixedColumnRenderer": {
                "text": {"runs": [{"text": s.duration}]}}}]);
            row["thumbnail"] =
                json!({"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(s.id)}}});
        }
        Layout::Album => {
            row["fixedColumns"] = json!([{"musicResponsiveListItemFixedColumnRenderer": {
                "text": {"runs": [{"text": s.duration}]}}}]);
        }
        Layout::Search => {
            let mut runs = vec![json!({"text": "Song"}), json!({"text": " • "})];
            runs.extend(artist_runs(s.artists));
            if let Some(a) = s.album {
                runs.push(json!({"text": " • "}));
                runs.push(album_run(a));
            }
            runs.push(json!({"text": " • "}));
            runs.push(json!({"text": s.duration}));
            flex.push(
                json!({"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": runs}}}),
            );
            row["thumbnail"] =
                json!({"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(s.id)}}});
        }
    }
    row["flexColumns"] = Value::Array(flex);
    json!({"musicResponsiveListItemRenderer": row})
}

fn new_continuation_item(token: &str) -> Value {
    json!({"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": token}}}})
}

fn playlist_grid_item(id: &str, name: &str, count: usize) -> Value {
    json!({"musicTwoRowItemRenderer": {
        "title": {"runs": [{"text": name, "navigationEndpoint": {"browseEndpoint": {"browseId": format!("VL{id}")}}}]},
        "subtitle": {"runs": [{"text": "Playlist"}, {"text": " • "}, {"text": format!("{count} songs")}]},
        "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(id)}}},
    }})
}

fn new_playlist_button() -> Value {
    json!({"musicTwoRowItemRenderer": {
        "title": {"runs": [{"text": "New playlist"}]},
        "navigationEndpoint": {"createPlaylistEndpoint": {}},
    }})
}

fn album_grid_item() -> Value {
    json!({"musicTwoRowItemRenderer": {
        "title": {"runs": [{"text": "Fake Album", "navigationEndpoint": {"browseEndpoint": {"browseId": ALBUM}}}]},
        "subtitle": {"runs": [{"text": "Album"}, {"text": " • "}, artist_run(AR_ALB), {"text": " • "}, {"text": "2019"}]},
        "thumbnailRenderer": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs("album01")}}},
    }})
}

// ---- the fake service ---------------------------------------------------------------------------

/// What the next POST answers instead of the account's own answer.
pub enum Script {
    /// Answers as the account would.
    Pass,
    Json(Value),
    Raw(u16, Vec<u8>),
    Status(u16),
    Error(HttpError),
}

/// One POST the fake saw.
#[derive(Debug, Clone)]
pub struct PostCall {
    pub url: String,
    pub endpoint: String,
    pub body: Value,
    pub headers: Vec<(String, String)>,
}

impl PostCall {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct Account {
    /// Video ids, newest first.
    liked: Vec<String>,
    /// Extra playlists made by a test: id, name, track ids.
    extra_playlists: Vec<(String, String, Vec<String>)>,
    /// Audio playlist ids of the albums the account saved.
    saved_albums: Vec<String>,
}

/// What a ranged GET of served audio does wrong, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFault {
    /// 403: the signed link expired.
    Forbidden,
    /// Half of the asked bytes, as if the connection closed.
    Truncate,
    /// A `Content-Range` whose start is off by one.
    LieStart,
    /// A `Content-Range` whose total is not the file's size.
    LieTotal,
    /// No `Content-Range` header at all.
    NoRangeHeader,
    /// 200 instead of 206.
    Whole,
}

#[derive(Default)]
struct Media {
    /// The audio served, by URL without its query.
    bodies: HashMap<String, Vec<u8>>,
    /// One entry per ranged request, in order: `None` serves it as asked.
    script: VecDeque<Option<MediaFault>>,
    /// Every ranged request: the URL and the range.
    requests: Vec<(String, (u64, u64))>,
}

pub struct FakeYt {
    media: Mutex<Media>,
    account: Mutex<Account>,
    page_size: AtomicUsize,
    script: Mutex<VecDeque<Script>>,
    posts: Mutex<Vec<PostCall>>,
    gets: Mutex<HashMap<String, Response>>,
    gets_seen: Mutex<Vec<String>>,
}

impl FakeYt {
    pub fn new() -> Arc<FakeYt> {
        Arc::new(FakeYt {
            media: Mutex::new(Media::default()),
            account: Mutex::new(Account {
                liked: vec![
                    "trackAAAA10".into(),
                    "trackAAAA01".into(),
                    "trackAAAA07".into(),
                ],
                extra_playlists: Vec::new(),
                saved_albums: Vec::new(),
            }),
            page_size: AtomicUsize::new(2),
            script: Mutex::new(VecDeque::new()),
            posts: Mutex::new(Vec::new()),
            gets: Mutex::new(HashMap::new()),
            gets_seen: Mutex::new(Vec::new()),
        })
    }

    /// How many entries of a list one answer holds (default 2, so every list of the account
    /// takes more than one page).
    pub fn set_page_size(&self, n: usize) {
        self.page_size.store(n.max(1), Ordering::SeqCst);
    }

    /// Adds a playlist of `n` generated tracks (n <= 99), named `Long`.
    pub fn add_long_playlist(&self, id: &str, n: usize) {
        let tracks = (1..=n).map(|i| format!("longtrack{i:02}")).collect();
        self.account.lock().unwrap().extra_playlists.push((
            id.to_owned(),
            "Long".to_owned(),
            tracks,
        ));
    }

    pub fn script(&self, s: Script) {
        self.script.lock().unwrap().push_back(s);
    }

    /// Lets `n` posts through, then makes the next one answer `s`.
    pub fn script_after(&self, n: usize, s: Script) {
        let mut q = self.script.lock().unwrap();
        for _ in 0..n {
            q.push_back(Script::Pass);
        }
        q.push_back(s);
    }

    pub fn posts(&self) -> Vec<PostCall> {
        self.posts.lock().unwrap().clone()
    }

    pub fn posts_to(&self, endpoint: &str) -> Vec<PostCall> {
        self.posts()
            .into_iter()
            .filter(|p| p.endpoint == endpoint)
            .collect()
    }

    pub fn clear_posts(&self) {
        self.posts.lock().unwrap().clear();
    }

    pub fn liked_ids(&self) -> Vec<String> {
        self.account.lock().unwrap().liked.clone()
    }

    /// The audio playlist ids of the albums the account saved.
    pub fn saved_albums(&self) -> Vec<String> {
        self.account.lock().unwrap().saved_albums.clone()
    }

    pub fn serve(&self, url: &str, body: &[u8]) {
        self.gets.lock().unwrap().insert(
            url.to_owned(),
            Response {
                status: 200,
                body: body.to_vec(),
            },
        );
    }

    /// Serves `bytes` by range on `url` (and on any URL that differs from it only in its query).
    pub fn serve_media(&self, url: &str, bytes: &[u8]) {
        let key = url.split('?').next().unwrap_or(url).to_owned();
        self.media
            .lock()
            .unwrap()
            .bodies
            .insert(key, bytes.to_vec());
    }

    /// Faults for the next ranged requests, one entry each (`None`: serve it as asked).
    pub fn script_media(&self, faults: &[Option<MediaFault>]) {
        self.media
            .lock()
            .unwrap()
            .script
            .extend(faults.iter().copied());
    }

    /// The ranged requests so far: URL and inclusive range.
    pub fn media_requests(&self) -> Vec<(String, (u64, u64))> {
        self.media.lock().unwrap().requests.clone()
    }

    pub fn gets_seen(&self) -> Vec<String> {
        self.gets_seen.lock().unwrap().clone()
    }

    fn size(&self) -> usize {
        self.page_size.load(Ordering::SeqCst)
    }

    fn is_liked(&self, id: &str) -> bool {
        self.account.lock().unwrap().liked.iter().any(|l| l == id)
    }

    /// The rows of a track list, by its token name.
    fn track_rows(&self, kind: &str, arg: &str) -> Option<Vec<Value>> {
        let liked = |id: &str| self.is_liked(id);
        let rows_of = |ids: &[String], layout: Layout| -> Vec<Value> {
            ids.iter()
                .filter_map(|id| song(id).map(|s| row(s, layout, liked(id))))
                .collect()
        };
        match kind {
            "liked" => {
                // Copy the ids first: `rows_of` asks the account again for each row.
                let ids = self.account.lock().unwrap().liked.clone();
                Some(rows_of(&ids, Layout::Playlist))
            }
            "pl" => {
                if let Some((_, _, tracks)) = PLAYLISTS.iter().find(|(id, _, _)| *id == arg) {
                    let ids: Vec<String> = tracks.iter().map(|s| (*s).to_owned()).collect();
                    return Some(rows_of(&ids, Layout::Playlist));
                }
                let acct = self.account.lock().unwrap();
                let (_, _, tracks) = acct.extra_playlists.iter().find(|(id, _, _)| id == arg)?;
                // Generated tracks have no catalog entry: make a row for each.
                Some(tracks.iter().map(|id| generated_row(id)).collect())
            }
            "album" if release(arg).is_some() => {
                let ids: Vec<String> = SONGS
                    .iter()
                    .filter(|s| s.album.is_some_and(|a| a.1 == arg))
                    .map(|s| s.id.to_owned())
                    .collect();
                Some(rows_of(&ids, Layout::Album))
            }
            "s" => {
                let q = String::from_utf8(unhex(arg)?).ok()?.to_lowercase();
                Some(
                    SONGS
                        .iter()
                        .filter(|s| s.title.to_lowercase().contains(&q))
                        .map(|s| row(s, Layout::Search, liked(s.id)))
                        .collect(),
                )
            }
            _ => None,
        }
    }

    fn grid_items(&self, kind: &str, arg: &str) -> Option<Vec<Value>> {
        match kind {
            // The "More" grid of an artist's releases: `arg` is the channel id and `a` or `s`.
            "rel" => {
                let (uc, group) = arg.rsplit_once('_')?;
                let single = group == "s";
                Some(
                    artist_releases(uc)
                        .into_iter()
                        .filter(|r| (r.label == "Single") == single)
                        .map(release_item)
                        .collect(),
                )
            }
            "pls" => {
                let mut items = vec![new_playlist_button()];
                for (id, name, tracks) in PLAYLISTS {
                    items.push(playlist_grid_item(id, name, tracks.len()));
                }
                let acct = self.account.lock().unwrap();
                for (id, name, tracks) in &acct.extra_playlists {
                    items.push(playlist_grid_item(id, name, tracks.len()));
                }
                Some(items)
            }
            "alb" => Some(vec![album_grid_item()]),
            _ => None,
        }
    }

    /// A page of track rows from `start`, as the `contents` of a shelf (new-style continuation).
    fn track_shelf_items(&self, kind: &str, arg: &str, start: usize) -> Option<Vec<Value>> {
        let rows = self.track_rows(kind, arg)?;
        let size = self.size();
        let end = (start + size).min(rows.len());
        let mut items: Vec<Value> = rows.get(start..end)?.to_vec();
        if end < rows.len() {
            items.push(new_continuation_item(&format!("{kind}.{arg}.{end}")));
        }
        Some(items)
    }

    fn browse(&self, body: &Value, url: &str) -> Response {
        // A continuation of the newer kind.
        if let Some(token) = body.get("continuation").and_then(Value::as_str) {
            let mut parts = token.splitn(3, '.');
            let (Some(kind), Some(arg), Some(start)) = (parts.next(), parts.next(), parts.next())
            else {
                return status(400);
            };
            let Ok(start) = start.parse::<usize>() else {
                return status(400);
            };
            return match self.track_shelf_items(kind, arg, start) {
                Some(items) => ok(json!({"onResponseReceivedActions": [{
                    "appendContinuationItemsAction": {"continuationItems": items}}]})),
                None => status(400),
            };
        }
        // A continuation of the older kind.
        if let Some(token) = query_param(url, "ctoken") {
            let mut parts = token.splitn(3, '.');
            let (Some(kind), Some(arg), Some(start)) = (parts.next(), parts.next(), parts.next())
            else {
                return status(400);
            };
            let Ok(start) = start.parse::<usize>() else {
                return status(400);
            };
            return match self.grid_page(kind, arg, start) {
                Some((items, next)) => ok(json!({"continuationContents": {"gridContinuation": {
                    "items": items, "continuations": next}}})),
                None => status(400),
            };
        }
        let Some(browse_id) = body.get("browseId").and_then(Value::as_str) else {
            return status(400);
        };
        match browse_id {
            "FEmusic_liked_playlists" => self.grid_first("pls", "x"),
            "FEmusic_liked_albums" => self.grid_first("alb", "x"),
            "VLLM" => self.track_first("liked", "x", None),
            id if release(id).is_some() => {
                self.track_first("album", id, Some(self.album_header(id)))
            }
            id if id.starts_with("UC") => match self.artist_response(id) {
                Some(v) => ok(v),
                None => status(400),
            },
            // An artist's "More" grid: `MPAD<channel id>`, the group in the params.
            id if id.starts_with("MPAD") => {
                let group = match body.get("params").and_then(Value::as_str) {
                    Some("pAlbums") => "a",
                    Some("pSingles") => "s",
                    _ => return status(400),
                };
                self.grid_first("rel", &format!("{}_{group}", &id[4..]))
            }
            id => match id.strip_prefix("VL") {
                Some(pid) => self.track_first("pl", pid, None),
                None => status(400),
            },
        }
    }

    /// A page of a grid and the `continuations` array that leads to the next page.
    fn grid_page(&self, kind: &str, arg: &str, start: usize) -> Option<(Vec<Value>, Value)> {
        let all = self.grid_items(kind, arg)?;
        let end = (start + self.size()).min(all.len());
        let items = all.get(start..end)?.to_vec();
        let next = if end < all.len() {
            json!([{"nextContinuationData": {"continuation": format!("{kind}.{arg}.{end}")}}])
        } else {
            Value::Null
        };
        Some((items, next))
    }

    fn grid_first(&self, kind: &str, arg: &str) -> Response {
        let Some((items, next)) = self.grid_page(kind, arg, 0) else {
            return status(400);
        };
        let mut grid = json!({"items": items});
        if !next.is_null() {
            grid["continuations"] = next;
        }
        ok(
            json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
            "content": {"sectionListRenderer": {"contents": [{"gridRenderer": grid}]}}}}]}}}),
        )
    }

    fn track_first(&self, kind: &str, arg: &str, header: Option<Value>) -> Response {
        let Some(items) = self.track_shelf_items(kind, arg, 0) else {
            return status(400);
        };
        let shelf_key = if kind == "album" {
            "musicShelfRenderer"
        } else {
            "musicPlaylistShelfRenderer"
        };
        let mut two_column = json!({"secondaryContents": {"sectionListRenderer": {"contents": [
            {(shelf_key): {"contents": items}}]}}});
        if let Some(h) = header {
            two_column["tabs"] = json!([{"tabRenderer": {"content": {"sectionListRenderer": {
                "contents": [{"musicResponsiveHeaderRenderer": h}]}}}}]);
        }
        ok(json!({"contents": {"twoColumnBrowseResultsRenderer": two_column}}))
    }

    /// The header of a release's album page: its title, type and year, the artists of its songs,
    /// and the play and save buttons (the save one says what a press does).
    fn album_header(&self, id: &str) -> Value {
        let r = release(id).expect("a known release");
        let mut buttons = Vec::new();
        if let Some(pl) = r.audio_playlist {
            let saved = self
                .account
                .lock()
                .unwrap()
                .saved_albums
                .iter()
                .any(|p| p == pl);
            buttons.push(
                json!({"musicPlayButtonRenderer": {"playNavigationEndpoint": {
                "watchEndpoint": {"videoId": "trackAAAA01", "playlistId": pl}}}}),
            );
            buttons.push(
                json!({"toggleButtonRenderer": {"defaultServiceEndpoint": {"likeEndpoint": {
                "status": if saved { "INDIFFERENT" } else { "LIKE" },
                "target": {"playlistId": pl}}}}}),
            );
        }
        let seed = if id == ALBUM { "album01" } else { id };
        json!({
            "title": {"runs": [{"text": r.title}]},
            "subtitle": {"runs": [{"text": r.label}, {"text": " • "}, {"text": r.year}]},
            "straplineTextOne": {"runs": artist_runs(&release_artists(id))},
            "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(seed)}}},
            "buttons": buttons,
        })
    }

    /// An artist page: the header, the songs, and a carousel for each group the artist has. The
    /// albums carousel of an artist with more albums than `CAROUSEL_SHOWS` shows that many and
    /// links to the "More" grid.
    fn artist_response(&self, uc: &str) -> Option<Value> {
        let (name, _) = ARTISTS.iter().find(|a| a.1 == uc)?;
        let rows: Vec<Value> = SONGS
            .iter()
            .filter(|s| s.artists.iter().any(|a| a.1 == uc))
            .map(|s| {
                // The page's own artist is left out of the album artist's rows.
                let layout = if uc == AR_ALB.1 {
                    Layout::Album
                } else {
                    Layout::Playlist
                };
                row(s, layout, self.is_liked(s.id))
            })
            .collect();
        let mut sections = vec![json!({"musicShelfRenderer": {"contents": rows}})];
        let releases = artist_releases(uc);
        let carousel = |title: &str, items: Vec<Value>, more: Option<&str>| {
            let mut run = json!({"text": title});
            if let Some(params) = more {
                run["navigationEndpoint"] = json!({"browseEndpoint": {
                    "browseId": format!("MPAD{uc}"), "params": params}});
            }
            json!({"musicCarouselShelfRenderer": {
                "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [run]}}},
                "contents": items}})
        };
        let albums: Vec<&Release> = releases
            .iter()
            .copied()
            .filter(|r| r.label == "Album")
            .collect();
        if !albums.is_empty() {
            let more = albums.len() > CAROUSEL_SHOWS;
            let items = albums
                .iter()
                .take(CAROUSEL_SHOWS)
                .map(|r| release_item(r))
                .collect();
            sections.push(carousel("Albums", items, more.then_some("pAlbums")));
        }
        let singles: Vec<&Release> = releases
            .iter()
            .copied()
            .filter(|r| r.label == "Single")
            .collect();
        if !singles.is_empty() {
            let items = singles.iter().map(|r| release_item(r)).collect();
            sections.push(carousel("Singles & EPs", items, None));
        }
        Some(json!({
            "header": {"musicImmersiveHeaderRenderer": {
                "title": {"runs": [{"text": name}]},
                "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": thumbs(uc)}}},
            }},
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
                "content": {"sectionListRenderer": {"contents": sections}}}}]}},
        }))
    }

    fn search(&self, body: &Value, url: &str) -> Response {
        let size = self.size();
        // Continuation of the older kind: the query is in the body, the position in the token.
        let (query, start) = match query_param(url, "ctoken") {
            Some(token) => {
                let start = token
                    .rsplit('.')
                    .next()
                    .and_then(|s| s.parse::<usize>().ok());
                (body.get("query").and_then(Value::as_str), start)
            }
            None => (body.get("query").and_then(Value::as_str), Some(0)),
        };
        let (Some(query), Some(start)) = (query, start) else {
            return status(400);
        };
        let continuing = query_param(url, "ctoken").is_some();
        let arg = hex(query.as_bytes());
        let Some(rows) = self.track_rows("s", &arg) else {
            return status(400);
        };
        if rows.is_empty() {
            return ok(
                json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{"tabRenderer": {
                "content": {"sectionListRenderer": {"contents": [{"itemSectionRenderer": {"contents": []}}]}}}}]}}}),
            );
        }
        let end = (start + size).min(rows.len());
        let items: Vec<Value> = rows.get(start..end).unwrap_or(&[]).to_vec();
        let next = (end < rows.len())
            .then(|| json!([{"nextContinuationData": {"continuation": format!("s.{arg}.{end}")}}]));
        let mut shelf = json!({"contents": items});
        if let Some(n) = next {
            shelf["continuations"] = n;
        }
        if continuing {
            return ok(json!({"continuationContents": {"musicShelfContinuation": shelf}}));
        }
        ok(
            json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{"tabRenderer": {
            "content": {"sectionListRenderer": {"contents": [{"musicShelfRenderer": shelf}]}}}}]}}}),
        )
    }

    fn like(&self, body: &Value, liked: bool) -> Response {
        if let Some(pl) = body.pointer("/target/playlistId").and_then(Value::as_str) {
            if !RELEASES.iter().any(|r| r.audio_playlist == Some(pl)) {
                return status(400);
            }
            let mut acct = self.account.lock().unwrap();
            acct.saved_albums.retain(|p| p != pl);
            if liked {
                acct.saved_albums.push(pl.to_owned());
            }
            return ok(json!({"responseContext": {}}));
        }
        let Some(id) = body.pointer("/target/videoId").and_then(Value::as_str) else {
            return status(400);
        };
        if song(id).is_none() {
            return status(400);
        }
        let mut acct = self.account.lock().unwrap();
        acct.liked.retain(|l| l != id);
        if liked {
            acct.liked.insert(0, id.to_owned());
        }
        ok(json!({"responseContext": {}}))
    }
}

fn generated_row(id: &str) -> Value {
    json!({"musicResponsiveListItemRenderer": {
        "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
            "playNavigationEndpoint": {"watchEndpoint": {"videoId": id}}}}}},
        "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": format!("Long {id}")}]}}}],
        "fixedColumns": [{"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "1:00"}]}}}],
    }})
}

fn ok(v: Value) -> Response {
    Response {
        status: 200,
        body: serde_json::to_vec(&v).unwrap(),
    }
}

fn status(code: u16) -> Response {
    Response {
        status: code,
        body: b"{}".to_vec(),
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
}

impl Http for FakeYt {
    fn get(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _range: Option<(u64, u64)>,
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        self.gets_seen.lock().unwrap().push(url.to_owned());
        self.gets
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or(HttpError::Network)
    }

    fn get_range(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: (u64, u64),
        cap: Cap,
    ) -> Result<RangeResponse, HttpError> {
        let key = url.split('?').next().unwrap_or(url);
        let mut media = self.media.lock().unwrap();
        let Some(bytes) = media.bodies.get(key).cloned() else {
            drop(media);
            let r = self.get(url, headers, Some(range), cap)?;
            return Ok(RangeResponse {
                status: r.status,
                body: r.body,
                content_range: None,
            });
        };
        media.requests.push((url.to_owned(), range));
        let fault = media.script.pop_front().flatten();
        drop(media);
        let (a, b) = range;
        let len = bytes.len() as u64;
        let refuse = |status| {
            Ok(RangeResponse {
                status,
                body: Vec::new(),
                content_range: None,
            })
        };
        if fault == Some(MediaFault::Forbidden) {
            return refuse(403);
        }
        if a >= len {
            return refuse(416);
        }
        let end = b.min(len - 1);
        let mut body = bytes[a as usize..=end as usize].to_vec();
        let mut cr = Some(ContentRange {
            start: a,
            end,
            total: Some(len),
        });
        let mut status = 206;
        match fault {
            Some(MediaFault::Truncate) => {
                body.truncate(body.len() / 2);
                cr = None;
            }
            Some(MediaFault::LieStart) => cr.as_mut().unwrap().start = a + 1,
            Some(MediaFault::LieTotal) => cr.as_mut().unwrap().total = Some(len + 1000),
            Some(MediaFault::NoRangeHeader) => cr = None,
            Some(MediaFault::Whole) => status = 200,
            Some(MediaFault::Forbidden) | None => {}
        }
        Ok(RangeResponse {
            status,
            body,
            content_range: cr,
        })
    }

    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        let endpoint = url
            .split_once("/youtubei/v1/")
            .map(|(_, rest)| rest.split('?').next().unwrap_or("").to_owned())
            .unwrap_or_default();
        let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        self.posts.lock().unwrap().push(PostCall {
            url: url.to_owned(),
            endpoint: endpoint.clone(),
            body: parsed.clone(),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        });
        let scripted = self.script.lock().unwrap().pop_front();
        if let Some(s) = scripted.filter(|s| !matches!(s, Script::Pass)) {
            return match s {
                Script::Pass => unreachable!(),
                Script::Json(v) => Ok(ok(v)),
                Script::Raw(code, bytes) => Ok(Response {
                    status: code,
                    body: bytes,
                }),
                Script::Status(code) => Ok(status(code)),
                Script::Error(e) => Err(e),
            };
        }
        let has_login = headers
            .iter()
            .any(|(n, v)| n.eq_ignore_ascii_case("authorization") && v.starts_with("SAPISIDHASH "))
            && headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("cookie") && v.contains("SAPISID="));
        if !has_login {
            return Ok(status(401));
        }
        Ok(match endpoint.as_str() {
            "browse" => self.browse(&parsed, url),
            "search" => self.search(&parsed, url),
            "like/like" => self.like(&parsed, true),
            "like/removelike" => self.like(&parsed, false),
            _ => status(404),
        })
    }
}

// ---- a fake yt-dlp, a fake release, a rig -------------------------------------------------------

/// `yt-dlp --version` answers "2026.10.26".
pub struct FakeRunner {
    pub calls: Mutex<Vec<Vec<String>>>,
}

impl Runner for FakeRunner {
    fn run(
        &self,
        _program: &Path,
        args: &[OsString],
        _timeout: Duration,
    ) -> Result<Output, RunError> {
        self.calls.lock().unwrap().push(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        );
        Ok(Output {
            code: Some(0),
            stdout: b"2026.10.26\n".to_vec(),
            stderr: Vec::new(),
        })
    }
}

pub struct TestKey {
    pub secret: SignedSecretKey,
    pub armored_public: String,
}

pub fn test_key() -> TestKey {
    let mut params = SecretKeyParamsBuilder::default();
    params
        .key_type(KeyType::Ed25519Legacy)
        .can_certify(true)
        .can_sign(true)
        .primary_user_id("Whipspot test key <test@example.invalid>".into());
    let secret = params.build().unwrap().generate(thread_rng()).unwrap();
    let armored_public = SignedPublicKey::from(secret.clone())
        .to_armored_string(ArmorOptions::default())
        .unwrap();
    TestKey {
        secret,
        armored_public,
    }
}

pub fn sign(key: &TestKey, data: &[u8]) -> Vec<u8> {
    DetachedSignature::sign_binary_data(
        thread_rng(),
        &key.secret.primary_key,
        &Password::empty(),
        HashAlgorithm::Sha256,
        data,
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// Serves a signed fake release of yt-dlp and QuickJS-ng on `yt`, and returns the `ToolEnv` that
/// trusts it (an empty one otherwise: no yt-dlp on PATH, no set path).
pub fn release_env(yt: &FakeYt, tools_dir: std::path::PathBuf) -> ToolEnv {
    let key = test_key();
    let ytdlp = b"yt-dlp test build".to_vec();
    let qjs = b"qjs test build".to_vec();
    let sums = format!("{}  yt-dlp-test\n", hex(&Sha256::digest(&ytdlp)));
    yt.serve(
        &format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS"),
        sums.as_bytes(),
    );
    yt.serve(
        &format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS.sig"),
        &sign(&key, sums.as_bytes()),
    );
    yt.serve(&format!("{YTDLP_RELEASE_BASE}/yt-dlp-test"), &ytdlp);
    yt.serve(&format!("{QJS_RELEASE_BASE}/qjs-test"), &qjs);
    let mut env = ToolEnv::new(tools_dir);
    env.trust = Trust::Custom(key.armored_public.clone());
    env.managed = Some(Managed {
        ytdlp_asset: "yt-dlp-test".into(),
        qjs_asset: "qjs-test".into(),
        qjs_sha256: Sha256::digest(&qjs).into(),
    });
    env
}

/// A cookies.txt with a login and a few other cookies.
pub fn cookies_txt() -> String {
    format!(
        "# Netscape HTTP Cookie File\n\
         .youtube.com\tTRUE\t/\tTRUE\t1900000000\tSAPISID\t{SENTINEL_SAPISID}\n\
         #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1900000000\t__Secure-3PSID\t{SENTINEL_COOKIE}\n\
         .google.com\tTRUE\t/\tTRUE\t0\tSID\tOTHER-DOMAIN-SECRET\n"
    )
}

/// The provider over a `FakeYt`, a memory store and a fake runner, nothing logged in.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub store: Arc<MemoryStore>,
    pub yt: Arc<FakeYt>,
    pub runner: Arc<FakeRunner>,
    pub provider: Arc<YtMusicProvider>,
    pub cookies_path: std::path::PathBuf,
}

impl Rig {
    pub fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let yt = FakeYt::new();
        let env = release_env(&yt, dir.path().join("tools"));
        let store = Arc::new(MemoryStore::default());
        let runner = Arc::new(FakeRunner {
            calls: Mutex::new(Vec::new()),
        });
        let provider = Arc::new(YtMusicProvider::new(YtDeps {
            http: yt.clone(),
            runner: runner.clone(),
            env,
            store: store.clone(),
            source: Engine::new(EngineConfig::default(), None).source,
            events: tokio::sync::broadcast::channel(16).0,
        }));
        let cookies_path = dir.path().join("cookies.txt");
        std::fs::write(&cookies_path, cookies_txt()).unwrap();
        Rig {
            dir,
            store,
            yt,
            runner,
            provider,
            cookies_path,
        }
    }

    pub fn cookies_input(&self) -> LoginInput {
        let mut input = LoginInput::default();
        input.0.insert(
            INPUT_PATH.to_owned(),
            self.cookies_path.to_string_lossy().into_owned(),
        );
        input
    }

    /// The two logins of the session contract: the helper (a fake install) and the cookies.
    pub fn logins(&self) -> Vec<(&'static str, LoginInput)> {
        vec![
            (STEP_HELPER, LoginInput::default()),
            (STEP_COOKIES, self.cookies_input()),
        ]
    }

    /// A login the contract must refuse: the cookies step with a file that is not there.
    pub fn bad_login(&self) -> (&'static str, LoginInput) {
        let mut input = LoginInput::default();
        input.0.insert(
            INPUT_PATH.to_owned(),
            self.dir
                .path()
                .join("no-such-file.txt")
                .to_string_lossy()
                .into_owned(),
        );
        (STEP_COOKIES, input)
    }

    /// Logs the cookies in (the helper is not needed for the library).
    pub async fn login_cookies(&self) {
        self.provider
            .auth()
            .login(STEP_COOKIES, &self.cookies_input())
            .await
            .expect("cookies login");
    }

    pub async fn login_all(&self) {
        for (step, input) in self.logins() {
            self.provider
                .auth()
                .login(step, &input)
                .await
                .expect("login");
        }
    }
}
