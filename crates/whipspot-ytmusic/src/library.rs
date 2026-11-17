//! The library of the user's YouTube Music account, over InnerTube (D6): liked songs, playlists,
//! saved albums, the tracks of a playlist or album, song search, and likes.
//!
//! Every call is one or two InnerTube requests, made on a blocking thread. A list is paged by
//! continuation: the token that leads to offset N of a list is kept (per list and offset), so a
//! caller that asks for the next page by the offset it was given costs one request. An offset
//! that has no kept token (the cache was cleared by a login or a logout) is found again by
//! walking the list from its start, at most [`MAX_REWALK_PAGES`] pages.
//!
//! The liked-songs list is the playlist `LM` ("Liked Music"), the list a like adds to. (The
//! plan named `FEmusic_liked_videos`; that is the library's Songs tab, which ytmusicapi
//! documents as not including liked videos.)
//!
//! Every error is a fixed string.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use whipspot_core::provider::{
    AlbumPage, BoxFuture, LibErrorKind, Library, LibraryError, PlaylistPage, Track, TrackPage,
};
use whipspot_core::text::display_text;

use crate::auth::{MSG_COOKIES_EXPIRED, YtAuth};
use crate::ids;
use crate::innertube::{InnerTube, TubeError};
use crate::parse::{self, AlbumHeader, Continuation, Page, TrackRow};

/// Shown for every call while no cookies are stored.
pub const MSG_IMPORT_COOKIES: &str = "Import your YouTube cookies first";
const MSG_NETWORK: &str = "Could not reach YouTube Music";
const MSG_STATUS: &str = "YouTube Music answered with an error";
const MSG_SLOW_DOWN: &str = "YouTube Music asked to slow down";
const MSG_SHAPE: &str = "YouTube Music's answer was not readable";
const MSG_INVALID: &str = "Invalid id";
const MSG_NO_POSITION: &str = "That position is not in the list";
const MSG_INTERNAL: &str = "Internal error";

/// A list stops after this many rows, as Spotify's does.
pub const MAX_ROWS: u32 = 2000;
/// The most pages walked to find an offset whose token is not kept.
pub const MAX_REWALK_PAGES: usize = 20;
const MAX_QUERY_CHARS: usize = 200;
const MAX_LIKES: usize = 20_000;
const MAX_ALBUM_HEADERS: usize = 64;
/// InnerTube's filter for song results (`EgWKAQ` + songs + spelling correction on).
const SONGS_FILTER: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";

/// One list the library pages through.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ListKey {
    Playlists,
    Albums,
    Liked,
    Playlist(String),
    Album(String),
    Search(String),
}

impl ListKey {
    /// The first request of the list: the endpoint and its body.
    fn first_request(&self) -> (&'static str, Value) {
        match self {
            ListKey::Playlists => ("browse", json!({"browseId": "FEmusic_liked_playlists"})),
            ListKey::Albums => ("browse", json!({"browseId": "FEmusic_liked_albums"})),
            ListKey::Liked => (
                "browse",
                json!({"browseId": format!("VL{}", ids::LIKED_PLAYLIST)}),
            ),
            ListKey::Playlist(id) => ("browse", json!({"browseId": format!("VL{id}")})),
            ListKey::Album(id) => ("browse", json!({"browseId": id})),
            ListKey::Search(q) => ("search", json!({"query": q, "params": SONGS_FILTER})),
        }
    }
}

#[derive(Default)]
struct State {
    /// The login the caches below belong to.
    generation: u64,
    tokens: HashMap<(ListKey, u32), Continuation>,
    likes: HashMap<String, bool>,
    album_headers: HashMap<String, AlbumHeader>,
}

struct Core {
    tube: Arc<InnerTube>,
    auth: YtAuth,
    state: Mutex<State>,
}

/// The [`Library`] of the provider. Cheap to clone.
#[derive(Clone)]
pub struct YtLibrary(Arc<Core>);

fn map_tube(e: TubeError) -> LibraryError {
    match e {
        TubeError::NoCookies => LibraryError::new(LibErrorKind::WebNotLoggedIn, MSG_IMPORT_COOKIES),
        TubeError::CookiesExpired => {
            LibraryError::new(LibErrorKind::WebNotLoggedIn, MSG_COOKIES_EXPIRED)
        }
        TubeError::RateLimited => LibraryError::new(LibErrorKind::RateLimited, MSG_SLOW_DOWN),
        TubeError::Status => LibraryError::new(LibErrorKind::Http, MSG_STATUS),
        TubeError::Http(_) => LibraryError::new(LibErrorKind::Network, MSG_NETWORK),
        TubeError::Unreadable => LibraryError::new(LibErrorKind::UnexpectedShape, MSG_SHAPE),
        TubeError::BadEndpoint => LibraryError::new(LibErrorKind::InvalidArgument, MSG_INVALID),
    }
}

fn shape() -> LibraryError {
    LibraryError::new(LibErrorKind::UnexpectedShape, MSG_SHAPE)
}

fn invalid() -> LibraryError {
    LibraryError::new(LibErrorKind::InvalidArgument, MSG_INVALID)
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Core {
    /// Fails with the login error if no cookies are stored, and drops what was cached for an
    /// earlier login.
    fn begin(&self) -> Result<(), LibraryError> {
        match self.auth.cookies() {
            Ok(Some(_)) => {}
            _ => return Err(map_tube(TubeError::NoCookies)),
        }
        let generation = self.auth.cookies_generation();
        let mut st = lock(&self.state);
        if st.generation != generation {
            *st = State {
                generation,
                ..State::default()
            };
        }
        Ok(())
    }

    fn request(&self, list: &ListKey, cont: Option<&Continuation>) -> Result<Value, LibraryError> {
        let (endpoint, body) = list.first_request();
        let answer = match cont {
            None => self.tube.post(endpoint, body),
            Some(Continuation::Next(t)) => self.tube.post(endpoint, json!({"continuation": t})),
            Some(Continuation::Legacy(t)) => self.tube.post_legacy_continuation(endpoint, body, t),
        };
        answer.map_err(map_tube)
    }

    /// The token that leads to `offset` of `list`: the kept one, else found by walking the list
    /// from its start.
    fn token_for<T>(
        &self,
        list: &ListKey,
        offset: u32,
        parse: &dyn Fn(&Value) -> Option<Page<T>>,
    ) -> Result<Continuation, LibraryError> {
        if let Some(t) = lock(&self.state).tokens.get(&(list.clone(), offset)) {
            return Ok(t.clone());
        }
        let (mut at, mut cont) = (0u32, None::<Continuation>);
        for _ in 0..MAX_REWALK_PAGES {
            let resp = self.request(list, cont.as_ref())?;
            let page = parse(&resp).ok_or_else(shape)?;
            let Some(next) = page.next else { break };
            at = at.saturating_add(u32::try_from(page.raw).unwrap_or(u32::MAX));
            lock(&self.state)
                .tokens
                .insert((list.clone(), at), next.clone());
            if at == offset {
                return Ok(next);
            }
            if at > offset {
                break;
            }
            cont = Some(next);
        }
        Err(LibraryError::new(
            LibErrorKind::InvalidArgument,
            MSG_NO_POSITION,
        ))
    }

    /// One page of `list` at `offset`, and the offset after it (if there is a next page).
    fn page<T>(
        &self,
        list: &ListKey,
        offset: u32,
        parse: &dyn Fn(&Value) -> Option<Page<T>>,
    ) -> Result<(Page<T>, Option<u32>, bool), LibraryError> {
        self.begin()?;
        let cont = if offset == 0 {
            None
        } else {
            Some(self.token_for(list, offset, parse)?)
        };
        let resp = self.request(list, cont.as_ref())?;
        let page = parse(&resp).ok_or_else(shape)?;
        let step = u32::try_from(page.raw).unwrap_or(u32::MAX);
        let after = offset.saturating_add(step);
        let (next_offset, truncated) = match &page.next {
            Some(_) if after >= MAX_ROWS => (None, true),
            Some(token) => {
                lock(&self.state)
                    .tokens
                    .insert((list.clone(), after), token.clone());
                (Some(after), false)
            }
            None => (None, false),
        };
        Ok((page, next_offset, truncated))
    }

    fn note_likes(&self, rows: &[TrackRow]) {
        let mut st = lock(&self.state);
        for row in rows {
            let (Some(like), Some(id)) = (row.like, ids::parse_track_uri(&row.track.uri)) else {
                continue;
            };
            if st.likes.len() >= MAX_LIKES {
                st.likes.clear();
            }
            st.likes.insert(id.to_owned(), like);
        }
    }

    fn track_page(
        &self,
        list: ListKey,
        offset: u32,
        album: Option<&str>,
    ) -> Result<TrackPage, LibraryError> {
        let parse = |resp: &Value| {
            let header = album.and_then(|id| match parse::album_header(resp) {
                Some(h) => {
                    let mut st = lock(&self.state);
                    if st.album_headers.len() >= MAX_ALBUM_HEADERS {
                        st.album_headers.clear();
                    }
                    st.album_headers.insert(id.to_owned(), h.clone());
                    Some(h)
                }
                None => lock(&self.state).album_headers.get(id).cloned(),
            });
            let page = if matches!(list, ListKey::Search(_)) {
                parse::search_page(resp)?
            } else {
                parse::track_page(resp, header.as_ref())?
            };
            self.note_likes(&page.items);
            Some(page)
        };
        let (page, next_offset, truncated) = self.page(&list, offset, &parse)?;
        let items: Vec<Track> = page.items.into_iter().map(|r| r.track).collect();
        Ok(TrackPage {
            items,
            skipped: page.skipped,
            next_offset,
            total: None,
            truncated,
        })
    }

    fn playlists(&self, offset: u32) -> Result<PlaylistPage, LibraryError> {
        let (page, next_offset, _) =
            self.page(&ListKey::Playlists, offset, &|r| parse::playlists_page(r))?;
        Ok(PlaylistPage {
            items: page.items,
            skipped: page.skipped,
            next_offset,
            total: None,
        })
    }

    fn albums(&self, offset: u32) -> Result<AlbumPage, LibraryError> {
        let (page, next_offset, _) =
            self.page(&ListKey::Albums, offset, &|r| parse::albums_page(r))?;
        Ok(AlbumPage {
            items: page.items,
            skipped: page.skipped,
            next_offset,
            total: None,
        })
    }

    fn playlist_items(&self, id: &str, offset: u32) -> Result<TrackPage, LibraryError> {
        let id = ids::parse_playlist_uri(id).ok_or_else(invalid)?;
        self.track_page(ListKey::Playlist(id.to_owned()), offset, None)
    }

    fn album_tracks(&self, id: &str, offset: u32) -> Result<TrackPage, LibraryError> {
        let id = ids::parse_album_uri(id).ok_or_else(invalid)?;
        self.track_page(ListKey::Album(id.to_owned()), offset, Some(id))
    }

    fn search(&self, query: &str, offset: u32) -> Result<TrackPage, LibraryError> {
        self.begin()?;
        let q = display_text(query, MAX_QUERY_CHARS);
        if q.is_empty() {
            return Ok(TrackPage {
                items: Vec::new(),
                skipped: 0,
                next_offset: None,
                total: None,
                truncated: false,
            });
        }
        self.track_page(ListKey::Search(q), offset, None)
    }

    fn liked(&self, uris: &[String]) -> Result<Vec<bool>, LibraryError> {
        self.begin()?;
        let st = lock(&self.state);
        Ok(uris
            .iter()
            .map(|u| {
                ids::parse_track_uri(u)
                    .and_then(|id| st.likes.get(id))
                    .copied()
                    .unwrap_or(false)
            })
            .collect())
    }

    fn set_liked(&self, uri: &str, liked: bool) -> Result<(), LibraryError> {
        let id = ids::parse_track_uri(uri).ok_or_else(invalid)?;
        self.begin()?;
        let endpoint = if liked {
            "like/like"
        } else {
            "like/removelike"
        };
        self.tube
            .post(endpoint, json!({"target": {"videoId": id}}))
            .map_err(map_tube)?;
        let mut st = lock(&self.state);
        if st.likes.len() >= MAX_LIKES {
            st.likes.clear();
        }
        st.likes.insert(id.to_owned(), liked);
        Ok(())
    }
}

impl YtLibrary {
    pub fn new(tube: Arc<InnerTube>, auth: YtAuth) -> YtLibrary {
        YtLibrary(Arc::new(Core {
            tube,
            auth,
            state: Mutex::new(State::default()),
        }))
    }
}

/// Runs `work` on a blocking thread: it waits on the network.
fn blocking<T: Send + 'static>(
    core: Arc<Core>,
    work: impl FnOnce(&Core) -> Result<T, LibraryError> + Send + 'static,
) -> BoxFuture<'static, Result<T, LibraryError>> {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || work(&core))
            .await
            .unwrap_or_else(|_| {
                Err(LibraryError::new(
                    LibErrorKind::UnexpectedShape,
                    MSG_INTERNAL,
                ))
            })
    })
}

impl Library for YtLibrary {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        blocking(self.0.clone(), move |c| c.playlists(offset))
    }

    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        blocking(self.0.clone(), move |c| {
            c.track_page(ListKey::Liked, offset, None)
        })
    }

    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let id = id.to_owned();
        blocking(self.0.clone(), move |c| c.playlist_items(&id, offset))
    }

    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        blocking(self.0.clone(), move |c| c.albums(offset))
    }

    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let id = id.to_owned();
        blocking(self.0.clone(), move |c| c.album_tracks(&id, offset))
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        let query = query.to_owned();
        blocking(self.0.clone(), move |c| c.search(&query, offset))
    }

    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        let uris = uris.to_vec();
        blocking(self.0.clone(), move |c| c.liked(&uris))
    }

    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        let uri = uri.to_owned();
        blocking(self.0.clone(), move |c| c.set_liked(&uri, liked))
    }

    fn can_change_likes(&self) -> bool {
        self.0.auth.cookies_usable()
    }
}
