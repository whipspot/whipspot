//! whipspot-local: the "Local" provider, the user's own audio files (D1). A folder scanner on a
//! std thread reads tags with lofty into a cached index (D5), favourites are a small user-data
//! file (D6), and the [`Library`] is served from an in-memory catalog (D7). Playback is one
//! decode thread (Symphonia) writing into the engine (`playback`); art is [`LocalArt`] (D9) and
//! the folder list is [`LocalFolders`] (D10).
//!
//! Every file under the music folders is untrusted input (rule 5): the walk follows no link, tags
//! that do not parse skip the file, and a track id is a lookup key, never a path (D4).

mod art;
mod favourites;
mod folders;
mod ids;
mod index;
mod library;
mod m3u;
mod playback;
mod scan;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use tokio::sync::broadcast;
use whipspot_core::engine::ReplayGain;
use whipspot_core::events::UiEvent;
use whipspot_core::folders::ScanStatus;
use whipspot_core::provider::{
    AlbumPage, AuthFailure, AuthFlow, BoxFuture, Capabilities, LibErrorKind, Library, LibraryError,
    LoginInput, Playback, PlaybackError, PlaylistPage, Provider, ProviderAuthStatus,
    ProviderContext, ProviderInfo, TrackPage,
};

use crate::favourites::Favourites;
use crate::library::{Catalog, TrackRec};
use crate::playback::{NOT_IN_LIBRARY, Player, Resolved};

pub use crate::art::{LocalArt, MAX_DECODE_SIDE, MAX_IMAGE_BYTES, decode as decode_art};
pub use crate::folders::LocalFolders;

pub use crate::favourites::FAVOURITES_FILE;
pub use crate::ids::{ART_SCHEME, is_track_uri};
pub use crate::index::{INDEX_FILE, MAX_ENTRIES};
pub use crate::library::{UNKNOWN_ALBUM, UNKNOWN_ARTIST};
pub use crate::scan::{Tags, read_tags_from_bytes};

pub const ID: &str = "local";
const NAME: &str = "Local";

/// Where the provider keeps its files and what it scans.
#[derive(Debug, Clone)]
pub struct LocalConfig {
    /// The music folders, in the user's order.
    pub folders: Vec<String>,
    /// The cache root: the library index lives here (safe to delete).
    pub cache_dir: PathBuf,
    /// App data: the favourites live here (user data).
    pub data_dir: PathBuf,
}

struct ScanState {
    folders: Vec<String>,
    running: bool,
    /// A rescan was asked for while one ran: run once more.
    again: bool,
    /// Files the last finished scan looked at.
    last_seen: u32,
}

struct Shared {
    cache_dir: PathBuf,
    catalog: RwLock<Arc<Catalog>>,
    favourites: Favourites,
    scan: Mutex<ScanState>,
    idle: Condvar,
    /// Files the running scan has looked at.
    seen: AtomicU32,
    /// The UI event sender: a finished scan pass says the library changed.
    events: broadcast::Sender<UiEvent>,
}

impl Shared {
    fn scan_state(&self) -> MutexGuard<'_, ScanState> {
        self.scan.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn catalog(&self) -> Arc<Catalog> {
        self.catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// The local-files provider. Always ready (no login).
pub struct LocalProvider {
    shared: Arc<Shared>,
    player: Player,
}

impl LocalProvider {
    /// Load the index (the library is ready at once from it) and start a background rescan.
    pub fn new(config: LocalConfig, ctx: ProviderContext) -> LocalProvider {
        let p = Self::from_index(config, ctx);
        p.rescan();
        p
    }

    /// Load the index only; nothing is scanned until [`LocalProvider::rescan`].
    pub fn from_index(config: LocalConfig, ctx: ProviderContext) -> LocalProvider {
        let loaded = index::load(&config.cache_dir, &config.folders);
        let catalog = Catalog::build(loaded.entries, loaded.playlists);
        let shared = Arc::new(Shared {
            cache_dir: config.cache_dir,
            catalog: RwLock::new(Arc::new(catalog)),
            favourites: Favourites::open(&config.data_dir),
            scan: Mutex::new(ScanState {
                folders: config.folders,
                running: false,
                again: false,
                last_seen: 0,
            }),
            idle: Condvar::new(),
            seen: AtomicU32::new(0),
            events: ctx.events.clone(),
        });
        let ctx = ctx.for_owner(ID);
        LocalProvider {
            shared,
            player: Player::new(ctx.source, ctx.events),
        }
    }

    /// Start a scan of every folder on a background thread. Returns at once; a scan asked for
    /// while one runs makes it run once more.
    pub fn rescan(&self) {
        rescan(&self.shared);
    }

    pub fn scan_status(&self) -> ScanStatus {
        scan_status(&self.shared)
    }

    /// Wait until no scan runs, at most `timeout`; true when idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let state = self.shared.scan_state();
        let (state, _) = self
            .shared
            .idle
            .wait_timeout_while(state, timeout, |s| s.running)
            .unwrap_or_else(|e| e.into_inner());
        !state.running
    }

    /// The music folders of this provider (shared with it).
    pub fn folders(&self) -> Arc<LocalFolders> {
        Arc::new(LocalFolders {
            shared: self.shared.clone(),
        })
    }

    /// The `local-art:` art source over this provider's index.
    pub fn art(&self) -> Arc<LocalArt> {
        Arc::new(LocalArt {
            shared: self.shared.clone(),
        })
    }

    /// The track `uri` as playback opens it; `None` when the index does not have it.
    fn resolve(&self, uri: &str) -> Option<Resolved> {
        if !is_track_uri(uri) {
            return None;
        }
        self.shared.catalog().by_uri(uri).map(resolved)
    }
}

fn resolved(t: &TrackRec) -> Resolved {
    let e = &t.entry;
    let replay_gain = (e.rg_track_db.is_some() || e.rg_album_db.is_some()).then_some(ReplayGain {
        track_db: e.rg_track_db,
        track_peak: e.rg_track_peak,
        album_db: e.rg_album_db,
        album_peak: e.rg_album_peak,
    });
    Resolved {
        uri: t.track.uri.clone(),
        path: PathBuf::from(&e.path),
        replay_gain,
    }
}

/// Start a scan unless one runs (then it runs once more).
fn rescan(shared: &Arc<Shared>) {
    let mut state = shared.scan_state();
    if state.running {
        state.again = true;
        return;
    }
    state.running = true;
    state.again = false;
    let worker = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("whipspot-local-scan".into())
        .spawn(move || scan_loop(&worker));
    if spawned.is_err() {
        state.running = false;
        shared.idle.notify_all();
    }
}

fn scan_status(shared: &Shared) -> ScanStatus {
    let state = shared.scan_state();
    let files_seen = if state.running {
        shared.seen.load(Ordering::Relaxed)
    } else {
        state.last_seen
    };
    ScanStatus {
        scanning: state.running,
        files_seen,
        tracks: u32::try_from(shared.catalog().track_count()).unwrap_or(u32::MAX),
    }
}

/// Marks the scan finished if the thread ends by a panic, so `scanning` never sticks. On a normal
/// end the loop does it under the same lock that sees `again`, so no rescan is lost.
struct PanicFinish<'a> {
    shared: &'a Shared,
    armed: bool,
}

impl Drop for PanicFinish<'_> {
    fn drop(&mut self) {
        if self.armed {
            finish(&mut self.shared.scan_state(), self.shared);
        }
    }
}

fn finish(state: &mut ScanState, shared: &Shared) {
    state.running = false;
    state.last_seen = shared.seen.load(Ordering::Relaxed);
    shared.idle.notify_all();
}

fn scan_loop(shared: &Shared) {
    let mut guard = PanicFinish {
        shared,
        armed: true,
    };
    let mut folders = shared.scan_state().folders.clone();
    loop {
        shared.seen.store(0, Ordering::Relaxed);
        let previous: HashMap<String, index::Entry> = shared
            .catalog()
            .entries()
            .map(|e| (ids::path_key(&e.path), e.clone()))
            .collect();
        let walked = scan::walk(&folders);
        let entries = scan::entries(&walked, &previous, &shared.seen);
        drop(previous);
        let playlists: Vec<String> = walked
            .playlists
            .iter()
            .filter_map(|p| p.to_str().map(str::to_owned))
            .collect();
        let catalog = Catalog::build(entries, playlists);
        let all: Vec<index::Entry> = catalog.entries().cloned().collect();
        index::save(&shared.cache_dir, &folders, &all, &catalog.playlist_paths);
        *shared.catalog.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(catalog);
        // The lists a front-end shows are stale now (none listening is fine).
        let _ = shared.events.send(UiEvent::LibraryChanged {
            provider: ID.to_owned(),
        });
        let mut state = shared.scan_state();
        if !state.again {
            finish(&mut state, shared);
            guard.armed = false;
            return;
        }
        state.again = false;
        folders = state.folders.clone();
    }
}

impl Provider for LocalProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID,
            name: NAME,
            glyph: whipspot_core::provider::ProviderGlyph::Files,
            caps: Capabilities {
                library: true,
                pcm_tap: true,
                eq: true,
                search: true,
                likes: true,
            },
            auth_steps: vec![],
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

const NO_LOGIN: &str = "this provider has no login";

impl AuthFlow for LocalProvider {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Ok(ProviderAuthStatus { steps: vec![] }) })
    }
    fn login<'a>(
        &'a self,
        _step: &'a str,
        _input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Err(AuthFailure(NO_LOGIN.to_owned())) })
    }
    fn logout<'a>(
        &'a self,
        _step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Err(AuthFailure(NO_LOGIN.to_owned())) })
    }
}

fn failed(text: &'static str) -> PlaybackError {
    PlaybackError(text.to_owned())
}

impl Playback for LocalProvider {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            let track = self.resolve(uri).ok_or_else(|| failed(NOT_IN_LIBRARY))?;
            self.player
                .load(track, play, position_ms)
                .await
                .map_err(failed)
        })
    }
    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.play().map_err(failed);
        Box::pin(async move { r })
    }
    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.pause().map_err(failed);
        Box::pin(async move { r })
    }
    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.stop().map_err(failed);
        Box::pin(async move { r })
    }
    fn seek(&self, position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.seek(position_ms).map_err(failed);
        Box::pin(async move { r })
    }
    fn source_format(&self) -> Option<String> {
        self.player.label()
    }
    fn preload<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            let track = self.resolve(uri).ok_or_else(|| failed(NOT_IN_LIBRARY))?;
            self.player.preload(track).map_err(failed)
        })
    }
}

const NOT_SAVED: &str = "the favourites could not be saved";

impl Library for LocalProvider {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        let page = self.shared.catalog().playlists_page(offset);
        Box::pin(async move { Ok(page) })
    }
    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        let paths = self.shared.favourites.newest_first();
        let page = self.shared.catalog().tracks_of_paths(&paths, offset);
        Box::pin(async move { Ok(page) })
    }
    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let result = self.shared.catalog().playlist_items(id, offset);
        Box::pin(async move { result })
    }
    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        let page = self.shared.catalog().albums_page(offset);
        Box::pin(async move { Ok(page) })
    }
    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let result = self.shared.catalog().album_tracks(id, offset);
        Box::pin(async move { result })
    }
    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let page = self.shared.catalog().search(query, offset);
        Box::pin(async move { Ok(page) })
    }
    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        let catalog = self.shared.catalog();
        let answer = uris
            .iter()
            .map(|u| {
                catalog
                    .by_uri(u)
                    .is_some_and(|t| self.shared.favourites.contains(&t.entry.path))
            })
            .collect();
        Box::pin(async move { Ok(answer) })
    }
    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        Box::pin(async move {
            let catalog = self.shared.catalog();
            let Some(t) = catalog.by_uri(uri) else {
                return Err(LibraryError::new(
                    LibErrorKind::InvalidArgument,
                    NOT_IN_LIBRARY,
                ));
            };
            self.shared
                .favourites
                .set(&t.entry.path, liked)
                .map_err(|_| LibraryError::new(LibErrorKind::AccessDenied, NOT_SAVED))
        })
    }
    fn can_change_likes(&self) -> bool {
        true
    }
}
