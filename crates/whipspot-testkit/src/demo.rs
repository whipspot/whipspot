//! A provider with invented content and a synthetic tone, for the demo app and for anything that
//! wants a logged-in library with audio flowing through the audio engine (MD20). It never
//! touches the network, the credential store, app data or an audio device.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use whipspot_core::engine::{SourceInfo, SourceWriter, StreamFormat};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{
    Album, AlbumDetail, AlbumKind, AlbumPage, Artist, ArtistPage, AuthFailure, AuthFlow,
    AuthStepInfo, AuthStepStatus, BoxFuture, Capabilities, EntityRef, InputSpec, LibErrorKind,
    Library, LibraryError, LoginInput, MAX_POPULAR, Playback, PlaybackError, Playlist,
    PlaylistPage, Provider, ProviderAuthStatus, ProviderContext, ProviderInfo, Readable,
    ReleaseGroup, StepHint, Track, TrackPage,
};

use crate::fake_art::demo_images;

const PAGE: usize = 25;
const RATE: u32 = 44_100;
const RATE_HZ: f64 = RATE as f64;
/// 20 ms of stereo frames at 44.1 kHz.
const BLOCK_FRAMES: usize = 882;
/// How long the tone thread waits while nothing plays or after a failed write.
const IDLE: Duration = Duration::from_millis(20);
/// The label the demo reports as its source format.
const SOURCE_LABEL: &str = "Generated tone, 44.1 kHz";

/// What the demo's tone is, as the engine is told.
const TONE: StreamFormat = StreamFormat {
    rate: RATE,
    channels: 2,
    source: SourceInfo {
        codec: "PCM (generated tone)",
        bits: None,
        bitrate_kbps: None,
        lossless: true,
        replay_gain: None,
    },
};

const TITLE_A: [&str; 8] = [
    "Hollow", "Glass", "Amber", "Quiet", "Lantern", "Tidal", "Velvet", "Orbit",
];
const TITLE_B: [&str; 8] = [
    "Meridian", "Harbour", "Garden", "Signal", "Ember", "Canyon", "Summer", "Circuit",
];
const ARTISTS: [&str; 6] = [
    "Velmora",
    "Quintar Oak",
    "Brask Lanterns",
    "Mirelle Fen",
    "The Ostrane Set",
    "Oruna Vell",
];
const ALBUM_NAMES: [&str; 12] = [
    "Low Tide Sessions",
    "Paper Harbour",
    "North Lamp",
    "Slow Orchard",
    "Copper Fold",
    "Field Notes",
    "Salt Road",
    "Blue Hour Tapes",
    "Kestrel Hours",
    "Glass Atlas",
    "Thin Ice Club",
    "Moth Light",
];
/// Playlists 4 to 12: name and track count (8 to 30).
const EXTRA_PLAYLISTS: [(&str, u32); 9] = [
    ("Rainy Commute", 20),
    ("Study Hall", 14),
    ("Late Kitchen", 26),
    ("Sunday Walk", 9),
    ("Road Trip Mix", 30),
    ("Gym Loop", 22),
    ("Deep Focus", 28),
    ("Coffee Table", 17),
    ("Night Shift", 12),
];
/// Albums 3 to 12: track counts (8 to 30); names come from `ALBUM_NAMES`.
const EXTRA_ALBUM_TRACKS: [u32; 10] = [16, 11, 20, 8, 25, 14, 18, 22, 12, 28];

/// Releases of `ARTISTS[EXTRA_ARTIST]` that are on no list of the library: they only show on the
/// artist's page and open from it (name, kind, year, tracks). They give that page a discography
/// with singles and a name too long for a tile.
const EXTRA_ARTIST: usize = 5;
const EXTRA_RELEASES: [(&str, AlbumKind, u16, u32); 4] = [
    ("Glass Hours", AlbumKind::Album, 2004, 9),
    ("Lantern Walk (with Velmora)", AlbumKind::Single, 1989, 2),
    ("North Lamp Live", AlbumKind::Single, 2001, 6),
    (
        "The Complete Harbour Recordings, Volume Two (Remastered Deluxe Edition)",
        AlbumKind::Album,
        1998,
        12,
    ),
];

/// The id of the album at `ALBUM_NAMES[idx]`.
fn album_id(idx: usize) -> String {
    format!("demo-album-{}", idx + 1)
}

/// The uri of the album at `ALBUM_NAMES[idx]`.
fn album_uri(idx: usize) -> String {
    format!("demo:album:{}", idx + 1)
}

/// The uri of `ARTISTS[idx]`.
fn artist_uri(idx: usize) -> String {
    format!("demo:artist:{idx}")
}

fn artist_ref(idx: usize) -> EntityRef {
    EntityRef {
        uri: artist_uri(idx),
        name: ARTISTS[idx].to_owned(),
    }
}

fn album_ref(idx: usize) -> EntityRef {
    EntityRef {
        uri: album_uri(idx),
        name: ALBUM_NAMES[idx].to_owned(),
    }
}

/// The index of the artist an album is by.
fn album_artist(idx: usize) -> usize {
    match idx {
        0 => 0,
        1 => 5,
        _ => idx % ARTISTS.len(),
    }
}

/// How the album is listed on its own artist's page.
fn album_kind(idx: usize) -> AlbumKind {
    match idx % 4 {
        2 => AlbumKind::Single,
        3 => AlbumKind::Compilation,
        _ => AlbumKind::Album,
    }
}

/// The artist an album also features, when it does: it lists the album as "appears on".
fn album_guest(idx: usize) -> Option<usize> {
    (idx % 5 == 4).then(|| (album_artist(idx) + 1) % ARTISTS.len())
}

/// The index of the demo album a uri names (`demo:album:<n>`).
fn parse_album_uri(uri: &str) -> Option<usize> {
    let n: usize = uri.strip_prefix("demo:album:")?.parse().ok()?;
    (1..=ALBUM_NAMES.len() + EXTRA_RELEASES.len())
        .contains(&n)
        .then(|| n - 1)
}

/// The index of the demo artist a uri names (`demo:artist:<i>`).
fn parse_artist_uri(uri: &str) -> Option<usize> {
    let i: usize = uri.strip_prefix("demo:artist:")?.parse().ok()?;
    (i < ARTISTS.len()).then_some(i)
}

/// An invented release year, 1990..=2024, by album index.
fn album_year(idx: usize) -> u16 {
    1990 + ((idx * 7 + 3) % 35) as u16
}

/// Name `t` after `ALBUM_NAMES[idx]` and give it that album's covers and year.
fn set_album(t: &mut Track, idx: usize) {
    t.album = ALBUM_NAMES[idx].to_owned();
    t.images = demo_images(&album_id(idx));
    t.year = Some(album_year(idx));
    t.album_ref = Some(album_ref(idx));
}

/// A demo track `n` (uri `demo:track:<n>`); names and artists are invented words.
fn track(n: u32) -> Track {
    let i = n as usize;
    let mut t = Track {
        uri: format!("demo:track:{n}"),
        name: format!(
            "{} {}",
            TITLE_A[i % TITLE_A.len()],
            TITLE_B[(i / TITLE_A.len()) % TITLE_B.len()]
        ),
        artist: ARTISTS[i % ARTISTS.len()].to_owned(),
        album: String::new(),
        duration_ms: 120_000 + (u64::from(n) * 7_919) % 240_000,
        images: vec![],
        year: None,
        artists: vec![artist_ref(i % ARTISTS.len())],
        album_ref: None,
    };
    set_album(&mut t, i % ALBUM_NAMES.len());
    t
}

/// `count` new tracks with the next numbers; `album` (an index into `ALBUM_NAMES`) puts them
/// all on that album when given.
fn take(next: &mut u32, count: u32, album: Option<usize>) -> Vec<Track> {
    (0..count)
        .map(|_| {
            *next += 1;
            let mut t = track(*next);
            if let Some(idx) = album {
                set_album(&mut t, idx);
            }
            t
        })
        .collect()
}

fn playlist(
    id: &str,
    name: &str,
    readable: Readable,
    tracks: Vec<Track>,
) -> (Playlist, Vec<Track>) {
    let p = Playlist {
        id: id.into(),
        name: name.into(),
        readable,
        track_count: Some(tracks.len() as u32),
        images: demo_images(id),
    };
    (p, tracks)
}

fn album(idx: usize, artist: &str, tracks: Vec<Track>) -> (Album, Vec<Track>) {
    let id = album_id(idx);
    let a = Album {
        uri: album_uri(idx),
        name: ALBUM_NAMES[idx].into(),
        artist: artist.into(),
        track_count: Some(tracks.len() as u32),
        images: demo_images(&id),
        year: Some(album_year(idx)),
        id,
        artists: vec![artist_ref(album_artist(idx))],
        kind: album_kind(idx),
    };
    (a, tracks)
}

/// The extra release `i` (see [`EXTRA_RELEASES`]) with `count` tracks numbered from `next`: its
/// tracks credit its artist, and every third one a second artist too.
fn extra_release(next: &mut u32, i: usize) -> (Album, Vec<Track>) {
    let (name, kind, year, count) = EXTRA_RELEASES[i];
    let idx = ALBUM_NAMES.len() + i;
    let id = album_id(idx);
    let me = artist_ref(EXTRA_ARTIST);
    let guest = artist_ref((EXTRA_ARTIST + 1) % ARTISTS.len());
    let album_ref = EntityRef {
        uri: album_uri(idx),
        name: name.to_owned(),
    };
    let tracks = (0..count)
        .map(|k| {
            let mut t = take(next, 1, None).remove(0);
            t.album = name.to_owned();
            t.images = demo_images(&id);
            t.year = Some(year);
            t.album_ref = Some(album_ref.clone());
            t.artists = vec![me.clone()];
            if k % 3 == 2 {
                t.artists.push(guest.clone());
            }
            t.artist = t
                .artists
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            t
        })
        .collect::<Vec<_>>();
    let album = Album {
        uri: album_uri(idx),
        name: name.to_owned(),
        artist: ARTISTS[EXTRA_ARTIST].to_owned(),
        track_count: Some(count),
        images: demo_images(&id),
        year: Some(year),
        id,
        artists: vec![me],
        kind,
    };
    (album, tracks)
}

/// The demo's whole library, built once.
struct Content {
    liked: Vec<Track>,
    playlists: Vec<(Playlist, Vec<Track>)>,
    albums: Vec<(Album, Vec<Track>)>,
    /// Releases that are on no list (see [`EXTRA_RELEASES`]), indexed after the albums.
    extras: Vec<(Album, Vec<Track>)>,
    /// Every track above, for search.
    all: Vec<Track>,
}

impl Content {
    fn build() -> Self {
        let mut n = 0;
        let liked = take(&mut n, 40, None);
        let night = take(&mut n, 18, None);
        let old = take(&mut n, 15, None);
        let morning = take(&mut n, 24, None);
        let low_tide = take(&mut n, 12, Some(0));
        let paper = take(&mut n, 30, Some(1));
        let mut playlists = vec![
            playlist("demo-playlist-1", "Night Drive", Readable::Readable, night),
            playlist("demo-playlist-2", "Old Demos", Readable::Unreadable, old),
            playlist(
                "demo-playlist-3",
                "Morning Set",
                Readable::Readable,
                morning,
            ),
        ];
        for (i, (name, count)) in EXTRA_PLAYLISTS.into_iter().enumerate() {
            let id = format!("demo-playlist-{}", i + 4);
            let tracks = take(&mut n, count, None);
            playlists.push(playlist(&id, name, Readable::Readable, tracks));
        }
        let mut albums = vec![album(0, ARTISTS[0], low_tide), album(1, ARTISTS[5], paper)];
        for (i, count) in EXTRA_ALBUM_TRACKS.into_iter().enumerate() {
            let idx = i + 2;
            let tracks = take(&mut n, count, Some(idx));
            albums.push(album(idx, ARTISTS[idx % ARTISTS.len()], tracks));
        }
        let extras = (0..EXTRA_RELEASES.len())
            .map(|i| extra_release(&mut n, i))
            .collect();
        let all: Vec<Track> = liked
            .iter()
            .cloned()
            .chain(playlists.iter().flat_map(|(_, t)| t.iter().cloned()))
            .chain(albums.iter().flat_map(|(_, t)| t.iter().cloned()))
            .collect();
        Self {
            liked,
            playlists,
            albums,
            extras,
            all,
        }
    }

    /// The album (or extra release) at `idx` of the uris' numbering.
    fn release(&self, idx: usize) -> Option<&(Album, Vec<Track>)> {
        match idx.checked_sub(self.albums.len()) {
            None => self.albums.get(idx),
            Some(i) => self.extras.get(i),
        }
    }
}

/// One page of `all` from `offset`: the items, the next offset (`None` at the end) and the total.
fn page<T: Clone>(all: &[T], offset: u32) -> (Vec<T>, Option<u32>, Option<u32>) {
    let start = (offset as usize).min(all.len());
    let end = (start + PAGE).min(all.len());
    let next = (end < all.len()).then_some(end as u32);
    (all[start..end].to_vec(), next, Some(all.len() as u32))
}

fn unknown_id() -> LibraryError {
    LibraryError::new(LibErrorKind::InvalidArgument, "unknown id")
}

/// A button text and a status line for the "play" step (see [`DemoProvider::with_helper_status`]).
type Helper = (&'static str, &'static str);

fn auth_status(logged_in: bool, helper: Option<Helper>) -> ProviderAuthStatus {
    let state = if logged_in {
        LoginState::LoggedIn
    } else {
        LoginState::LoggedOut
    };
    ProviderAuthStatus {
        steps: vec![
            AuthStepStatus {
                id: "play".into(),
                state: state.clone(),
                action: helper.map(|(a, _)| a.to_owned()),
                detail: helper.map(|(_, d)| d.to_owned()),
            },
            AuthStepStatus {
                id: "library".into(),
                state,
                action: None,
                detail: None,
            },
        ],
    }
}

/// The demo provider. Both login steps flip one shared switch; the library answers only while
/// it is on. Dropping it stops the tone thread.
pub struct DemoProvider {
    content: Content,
    logged_in: AtomicBool,
    tone: Arc<Tone>,
    source: SourceWriter,
    /// The saved tracks, by uri; starts with the 40 liked ones.
    saved: Mutex<HashSet<String>>,
    /// Whether the login lets likes change (see `read_only_likes`).
    likes_writable: bool,
    /// An action button text and a status line on the "play" step; none by default.
    helper: Option<Helper>,
    /// Keeps the event channel open; the demo emits no events.
    _ctx: ProviderContext,
}

/// What the provider tells its tone thread.
struct Tone {
    /// Set while the tone should be written (play, load with play, not pause/stop).
    playing: AtomicBool,
    /// A load asked for a new track: the thread starts one before its next write.
    new_track: AtomicBool,
    /// Cleared on drop; the tone thread exits when it sees this.
    alive: AtomicBool,
}

impl DemoProvider {
    /// Starts the tone thread on `ctx`'s source writer; the thread runs until the provider drops.
    pub fn new(ctx: ProviderContext, logged_in: bool) -> Self {
        let tone_state = Arc::new(Tone {
            playing: AtomicBool::new(false),
            new_track: AtomicBool::new(true),
            alive: AtomicBool::new(true),
        });
        let source = ctx.source.clone();
        let (writer, state) = (source.clone(), tone_state.clone());
        let _ = std::thread::spawn(move || tone(&writer, &state));
        let content = Content::build();
        let saved = content.liked.iter().map(|t| t.uri.clone()).collect();
        Self {
            content,
            logged_in: AtomicBool::new(logged_in),
            tone: tone_state,
            source,
            saved: Mutex::new(saved),
            likes_writable: true,
            helper: None,
            _ctx: ctx,
        }
    }

    /// Make likes read-only, as a login without the right consent is: `can_change_likes` is
    /// false and `set_liked` is refused.
    pub fn read_only_likes(mut self) -> Self {
        self.likes_writable = false;
        self
    }

    /// Give the "play" step an action button and a status line, as a provider with a helper
    /// tool to install has: both fixed strings.
    pub fn with_helper_status(mut self, action: &'static str, detail: &'static str) -> Self {
        self.helper = Some((action, detail));
        self
    }

    /// The releases of artist `a` in `group`: its own albums, singles and compilations, and the
    /// albums it appears on (those list the album's own artist, with the kind `AppearsOn`).
    fn releases(&self, a: usize, group: ReleaseGroup) -> Vec<Album> {
        let own = |k: &[AlbumKind]| {
            let mut v = self
                .content
                .albums
                .iter()
                .enumerate()
                .filter(|(i, (alb, _))| album_artist(*i) == a && k.contains(&alb.kind))
                .map(|(_, (alb, _))| alb.clone())
                .collect::<Vec<_>>();
            if a == EXTRA_ARTIST {
                v.extend(
                    self.content
                        .extras
                        .iter()
                        .filter(|(alb, _)| k.contains(&alb.kind))
                        .map(|(alb, _)| alb.clone()),
                );
            }
            v
        };
        let guest = || {
            self.content
                .albums
                .iter()
                .enumerate()
                .filter(|(i, _)| album_guest(*i) == Some(a))
                .map(|(_, (alb, _))| Album {
                    kind: AlbumKind::AppearsOn,
                    ..alb.clone()
                })
                .collect::<Vec<_>>()
        };
        use AlbumKind::{Album as Lp, Compilation, Single};
        match group {
            ReleaseGroup::Albums => own(&[Lp, Compilation]),
            ReleaseGroup::Singles => own(&[Single]),
            ReleaseGroup::AppearsOn => guest(),
            ReleaseGroup::All => {
                let mut all = own(&[Lp, Compilation, Single]);
                all.extend(guest());
                all
            }
        }
    }

    fn require_login(&self) -> Result<(), LibraryError> {
        if self.logged_in.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(LibraryError::new(
                LibErrorKind::WebNotLoggedIn,
                "not logged in",
            ))
        }
    }
}

impl Drop for DemoProvider {
    fn drop(&mut self) {
        self.tone.alive.store(false, Ordering::Relaxed);
    }
}

/// 220, 440 and 1320 Hz with a slow wobble between 0.8 and 1.0; the peak stays below 0.7.
fn chord(t: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    let wobble = 0.9 + 0.1 * (tau * 0.25 * t).sin();
    let sum = 0.3 * (tau * 220.0 * t).sin()
        + 0.25 * (tau * 440.0 * t).sin()
        + 0.15 * (tau * 1320.0 * t).sin();
    sum * wobble
}

/// Writes the chord to the engine in 20 ms blocks while `playing` is set; the engine's full
/// ring paces it. Ends when `alive` is cleared. A failed write (no output, a stalled or failed
/// device) waits a moment and tries again.
fn tone(source: &SourceWriter, state: &Tone) {
    let mut frame: u64 = 0;
    let mut block = vec![0.0f64; BLOCK_FRAMES * 2];
    while state.alive.load(Ordering::Relaxed) {
        if !state.playing.load(Ordering::Relaxed) {
            std::thread::sleep(IDLE);
            continue;
        }
        if state.new_track.swap(false, Ordering::Relaxed) && source.start_track(TONE).is_err() {
            state.new_track.store(true, Ordering::Relaxed);
            std::thread::sleep(IDLE);
            continue;
        }
        for (i, pair) in block.chunks_exact_mut(2).enumerate() {
            let v = chord((frame + i as u64) as f64 / RATE_HZ);
            pair[0] = v;
            pair[1] = v;
        }
        match source.write(&block) {
            Ok(()) => frame += BLOCK_FRAMES as u64,
            Err(_) => {
                // Without a track (the engine was reopened) the next pass starts one.
                state.new_track.store(true, Ordering::Relaxed);
                std::thread::sleep(IDLE);
            }
        }
    }
}

impl Provider for DemoProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "demo",
            name: "Demo",
            glyph: whipspot_core::provider::ProviderGlyph::Stream,
            caps: Capabilities {
                library: true,
                pcm_tap: true,
                eq: false,
                search: true,
                likes: true,
                explore: true,
                save_albums: true,
            },
            auth_steps: vec![
                AuthStepInfo {
                    id: "play",
                    label: "Playback",
                    inputs: vec![],
                    hint: None,
                    switch: false,
                },
                AuthStepInfo {
                    id: "library",
                    label: "Library",
                    inputs: vec![InputSpec {
                        name: "key",
                        label: "Access key",
                        pattern: "^[a-z]{4}$",
                        format: "4 letters",
                        max_len: 4,
                    }],
                    hint: Some(StepHint {
                        text: "Any four letters work in the demo.",
                        copy: Some("demo"),
                    }),
                    switch: false,
                },
            ],
        }
    }
    fn auth(&self) -> &dyn AuthFlow {
        self
    }
    fn playback(&self) -> &dyn Playback {
        self
    }
    fn library(&self) -> Option<&dyn Library> {
        Some(self)
    }
}

impl AuthFlow for DemoProvider {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            Ok(auth_status(
                self.logged_in.load(Ordering::Relaxed),
                self.helper,
            ))
        })
    }
    fn login<'a>(
        &'a self,
        step: &'a str,
        input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            match step {
                "play" => {}
                "library" if input.get("key").is_some_and(|k| !k.is_empty()) => {}
                "library" => return Err(AuthFailure("Enter the access key.".into())),
                _ => return Err(AuthFailure("Unknown login step.".into())),
            }
            self.logged_in.store(true, Ordering::Relaxed);
            Ok(auth_status(true, self.helper))
        })
    }
    fn logout<'a>(
        &'a self,
        step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            if !matches!(step, "play" | "library") {
                return Err(AuthFailure("Unknown login step.".into()));
            }
            self.logged_in.store(false, Ordering::Relaxed);
            Ok(auth_status(false, self.helper))
        })
    }
    fn prefill<'a>(
        &'a self,
        step: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AuthFailure>> {
        Box::pin(
            async move { Ok((step == "library" && input == "key").then(|| "demo".to_owned())) },
        )
    }
}

impl Playback for DemoProvider {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        _position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            if !uri.starts_with("demo:") {
                return Err(PlaybackError("not a demo track".into()));
            }
            // Drop what the old track left in the engine, then start the new one.
            self.source.interrupt();
            self.tone.new_track.store(true, Ordering::Relaxed);
            self.tone.playing.store(play, Ordering::Relaxed);
            if play {
                self.source.resume();
            } else {
                self.source.pause();
            }
            Ok(())
        })
    }
    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.source.resume();
            self.tone.playing.store(true, Ordering::Relaxed);
            Ok(())
        })
    }
    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.tone.playing.store(false, Ordering::Relaxed);
            self.source.pause();
            Ok(())
        })
    }
    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.tone.playing.store(false, Ordering::Relaxed);
            self.source.pause();
            Ok(())
        })
    }
    fn seek(&self, _position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async { Ok(()) })
    }
    fn source_format(&self) -> Option<String> {
        Some(SOURCE_LABEL.to_owned())
    }
}

impl Library for DemoProvider {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let all: Vec<Playlist> = self
                .content
                .playlists
                .iter()
                .map(|(p, _)| p.clone())
                .collect();
            let (items, next_offset, total) = page(&all, offset);
            Ok(PlaylistPage {
                items,
                skipped: 0,
                next_offset,
                total,
            })
        })
    }
    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let (items, next_offset, total) = page(&self.content.liked, offset);
            Ok(TrackPage {
                items,
                skipped: 0,
                next_offset,
                total,
                truncated: false,
            })
        })
    }
    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let (p, tracks) = self
                .content
                .playlists
                .iter()
                .find(|(p, _)| p.id == id)
                .ok_or_else(unknown_id)?;
            if p.readable == Readable::Unreadable {
                return Err(LibraryError::new(
                    LibErrorKind::NotReadable,
                    "playlist not readable",
                ));
            }
            let (items, next_offset, total) = page(tracks, offset);
            Ok(TrackPage {
                items,
                skipped: 0,
                next_offset,
                total,
                truncated: false,
            })
        })
    }
    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let all: Vec<Album> = self.content.albums.iter().map(|(a, _)| a.clone()).collect();
            let (items, next_offset, total) = page(&all, offset);
            Ok(AlbumPage {
                items,
                skipped: 0,
                next_offset,
                total,
            })
        })
    }
    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let (_, tracks) = self
                .content
                .albums
                .iter()
                .chain(&self.content.extras)
                .find(|(a, _)| a.id == id)
                .ok_or_else(unknown_id)?;
            let (items, next_offset, total) = page(tracks, offset);
            Ok(TrackPage {
                items,
                skipped: 0,
                next_offset,
                total,
                truncated: false,
            })
        })
    }
    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let q = query.to_lowercase();
            let hits: Vec<Track> = self
                .content
                .all
                .iter()
                .filter(|t| {
                    t.name.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q)
                })
                .cloned()
                .collect();
            let (items, next_offset, total) = page(&hits, offset);
            Ok(TrackPage {
                items,
                skipped: 0,
                next_offset,
                total,
                truncated: false,
            })
        })
    }
    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let saved = self.saved.lock().unwrap();
            Ok(uris.iter().map(|u| saved.contains(u)).collect())
        })
    }
    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            if !self.likes_writable {
                return Err(LibraryError::new(
                    LibErrorKind::AccessDenied,
                    "likes are read-only",
                ));
            }
            let mut saved = self.saved.lock().unwrap();
            if liked {
                saved.insert(uri.to_owned());
            } else {
                saved.remove(uri);
            }
            Ok(())
        })
    }
    fn can_change_likes(&self) -> bool {
        self.likes_writable
    }
    fn artist<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<ArtistPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let a = parse_artist_uri(uri).ok_or_else(unknown_id)?;
            let mine = artist_uri(a);
            let popular: Vec<Track> = self
                .content
                .all
                .iter()
                .filter(|t| t.artists.iter().any(|r| r.uri == mine))
                .take(MAX_POPULAR)
                .cloned()
                .collect();
            let all = self.releases(a, ReleaseGroup::All);
            let (items, next_offset, total) = page(&all, 0);
            Ok(ArtistPage {
                artist: Artist {
                    uri: mine,
                    name: ARTISTS[a].to_owned(),
                    images: demo_images(&format!("demo-artist-{a}")),
                },
                popular,
                releases: AlbumPage {
                    items,
                    skipped: 0,
                    next_offset,
                    total,
                },
            })
        })
    }
    fn album<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<AlbumDetail, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let idx = parse_album_uri(uri).ok_or_else(unknown_id)?;
            let (album, tracks) = self.content.release(idx).ok_or_else(unknown_id)?;
            let (items, next_offset, total) = page(tracks, 0);
            Ok(AlbumDetail {
                album: album.clone(),
                tracks: TrackPage {
                    items,
                    skipped: 0,
                    next_offset,
                    total,
                    truncated: false,
                },
            })
        })
    }
    fn artist_releases<'a>(
        &'a self,
        uri: &'a str,
        group: ReleaseGroup,
        offset: u32,
    ) -> BoxFuture<'a, Result<AlbumPage, LibraryError>> {
        Box::pin(async move {
            self.require_login()?;
            let a = parse_artist_uri(uri).ok_or_else(unknown_id)?;
            let all = self.releases(a, group);
            let (items, next_offset, total) = page(&all, offset);
            Ok(AlbumPage {
                items,
                skipped: 0,
                next_offset,
                total,
            })
        })
    }
}
