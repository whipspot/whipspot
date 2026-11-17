//! Spotify Web API client for the library picker (decisions D12, D14, D15, D16) and the like
//! heart (D50, MD71): reads plus saving and removing tracks on `/me/library`.
//!
//! * Uses only the web login's token ([`WebTokens`]); the playback token cannot be passed (distinct
//!   types, see the `compile_fail` doctest on [`WebApi::new`]). The token travels only in the
//!   `Authorization` header and is never logged, formatted or put in an error.
//! * Talks only to `https://api.spotify.com/v1/{me, me/playlists, me/tracks, me/albums,
//!   playlists/{id}/items, albums/{id}/tracks, search}` with `limit=50` (search: 10, the most
//!   Spotify allows now), plus `/me/library` and `/me/library/contains` for likes; no `fields=`
//!   filter, no batch, `/tracks/{id}` or the old `/playlists/{id}/tracks`. Search is `type=track`
//!   only. Redirects are never followed.
//! * Strictly one request at a time per call, on demand, paged by offset computed here.
//! * First 429: the [`gate::RateGate`] closes; every later call fails without touching the network
//!   until the cooldown ends. 401: one forced token refresh and one retry of that request. No
//!   sleeping, no retry loop anywhere.

pub mod gate;
pub mod model;

use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;

use crate::{
    auth::Secret,
    auth_manager::{AuthError, TokenSource, WebTokens},
    uri::{is_spotify_id, parse_track_uri},
    web_auth::consts::SCOPE_LIBRARY_MODIFY,
};
use gate::{RateGate, cooldown_secs};
pub use model::{
    Album, AlbumPage, Fingerprint, MAX_ROWS, MAX_TEXT_CHARS, Playlist, PlaylistPage, Readable,
    SEARCH_MAX_ROWS, Track, TrackPage, UnexpectedShape,
};

/// Items per request (the documented maximum for the endpoints used).
pub const PAGE_SIZE: u32 = 50;
/// Search results per request (Spotify lowered the maximum from 50 to 10).
pub const SEARCH_PAGE_SIZE: u32 = 10;
const API_BASE: &str = "https://api.spotify.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// A page is at most ~100 KB in practice; anything larger is not a page.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Spotify URIs per `/me/library/contains` call (the documented maximum).
const LIKED_BATCH: usize = 40;
/// Pages of `/me/playlists` a fingerprint reads at most (50 playlists each).
const FINGERPRINT_MAX_PAGES: u32 = 20;

/// Library failure. `Display` is fixed text, safe for a UI; no body or URL is ever included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Gate closed (a 429 earlier, or just now). No request was sent for this call after a 429.
    RateLimited {
        retry_after_secs: u64,
    },
    /// No web login, or Spotify rejected the token twice.
    WebNotLoggedIn,
    /// 403 on a playlist's items: not owned by the user / not collaborative (dev-mode limit).
    NotReadable,
    /// 403 on `/me`, `/me/playlists` or `/me/tracks`: the app itself has no access.
    AccessDenied,
    /// The playlist id is not 22 base62 characters.
    InvalidId,
    /// Offset beyond the row cap.
    InvalidOffset,
    /// Empty, over-long or control-character search text.
    InvalidQuery,
    /// 2xx but not a page / `/me` object.
    UnexpectedShape,
    /// Any other non-2xx status (including a 3xx, which is never followed).
    Http(u16),
    Network,
    CredentialStore,
    /// The login service refused the refresh for another reason.
    AuthFailed,
}

impl Error {
    /// Stable machine-readable name for the UI.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::RateLimited { .. } => "rateLimited",
            Error::WebNotLoggedIn => "webNotLoggedIn",
            Error::NotReadable => "notReadable",
            Error::AccessDenied => "accessDenied",
            Error::InvalidId | Error::InvalidOffset | Error::InvalidQuery => "invalidArgument",
            Error::UnexpectedShape => "unexpectedShape",
            Error::Http(_) => "http",
            Error::Network => "network",
            Error::CredentialStore => "credentialStore",
            Error::AuthFailed => "authFailed",
        }
    }

    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Error::RateLimited { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::RateLimited { retry_after_secs } => {
                write!(f, "Spotify is rate-limiting this app. Try again in about {retry_after_secs} s.")
            }
            Error::WebNotLoggedIn => f.write_str("the Spotify library is not connected"),
            Error::NotReadable => f.write_str(
                "Spotify doesn't let this app read this playlist (it is not owned by you or shared as collaborative).",
            ),
            Error::AccessDenied => f.write_str(
                "Spotify refused this app's access. Check that the app owner still has Premium, then reconnect.",
            ),
            Error::InvalidId => f.write_str("not a Spotify playlist or album id"),
            Error::InvalidQuery => f.write_str("not a valid search text"),
            Error::InvalidOffset => f.write_str("offset is out of range"),
            Error::UnexpectedShape => f.write_str("unexpected response from Spotify"),
            Error::Http(code) => write!(f, "Spotify returned HTTP {code}"),
            Error::Network => f.write_str("network error reaching Spotify"),
            Error::CredentialStore => f.write_str("credential store access failed"),
            Error::AuthFailed => f.write_str("Spotify did not accept the library login"),
        }
    }
}

impl std::error::Error for Error {}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Error", 3)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.serialize_field("retryAfterSecs", &self.retry_after_secs())?;
        st.end()
    }
}

fn from_auth(e: &AuthError) -> Error {
    match e {
        AuthError::NotLoggedIn => Error::WebNotLoggedIn,
        AuthError::Network => Error::Network,
        AuthError::CredentialStore => Error::CredentialStore,
        _ => Error::AuthFailed,
    }
}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub struct WebApi {
    http: reqwest::Client,
    tokens: WebTokens,
    base: String,
    gate: RateGate,
    clock: Clock,
    /// The logged-in user's id once asked for, and a generation bumped by
    /// [`WebApi::forget_user`] so an answer that was in flight across a logout is not kept.
    me: Mutex<(u64, Option<String>)>,
}

impl WebApi {
    /// Only the web login's token source is accepted: the playback token cannot reach this API.
    ///
    /// ```compile_fail
    /// use std::sync::Arc;
    /// use whipspot_spotify::{auth_manager::AuthManager, webapi::WebApi};
    /// use whipspot_testkit::MemoryStore;
    /// let auth = AuthManager::new(Arc::new(MemoryStore::default()));
    /// let _ = WebApi::new(auth.playback());
    /// ```
    pub fn new(tokens: WebTokens) -> Self {
        Self::build(tokens, API_BASE.to_owned(), Arc::new(Instant::now), false)
    }

    /// Test constructor: plain-HTTP mock server, injected clock, no system proxy.
    #[cfg(test)]
    fn for_test(tokens: WebTokens, base: String, clock: Clock) -> Self {
        Self::build(tokens, base, clock, true)
    }

    fn build(tokens: WebTokens, base: String, clock: Clock, no_proxy: bool) -> Self {
        let mut b = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if no_proxy {
            b = b.no_proxy();
        }
        WebApi {
            http: b.build().expect("reqwest client"),
            tokens,
            base,
            gate: RateGate::default(),
            clock,
            me: Mutex::new((0, None)),
        }
    }

    /// The current user's id (one request, then cached until [`WebApi::forget_user`]).
    pub async fn me(&self) -> Result<String, Error> {
        let generation = {
            let me = self.me.lock().unwrap();
            if let Some(id) = me.1.clone() {
                return Ok(id);
            }
            me.0
        };
        let v = self.get("/me", Forbidden::AccessDenied).await?;
        let id = model::parse_me(&v).ok_or(Error::UnexpectedShape)?;
        let mut me = self.me.lock().unwrap();
        if me.0 == generation {
            me.1 = Some(id.clone());
        }
        Ok(id)
    }

    /// Forget the cached user id: the web login ended or changed (it may be another account).
    pub fn forget_user(&self) {
        let mut me = self.me.lock().unwrap();
        me.0 += 1;
        me.1 = None;
    }

    /// One page of the user's playlists (the synthetic "Liked Songs" entry is the UI's).
    pub async fn playlists(&self, offset: u32) -> Result<PlaylistPage, Error> {
        check_offset(offset)?;
        let me = self.me().await?;
        let v = self
            .get(
                &format!("/me/playlists?limit={PAGE_SIZE}&offset={offset}"),
                Forbidden::AccessDenied,
            )
            .await?;
        model::parse_playlist_page(&v, offset, &me)
            .map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// What a look at the library compares ([`Fingerprint`]): every playlist's id and
    /// `snapshot_id` (at most [`FINGERPRINT_MAX_PAGES`] pages) and the liked songs' total and
    /// newest `added_at` (one item). Each request goes through the gate and the body cap like
    /// any other; a failure of any of them fails the whole call.
    pub async fn fingerprint(&self) -> Result<Fingerprint, Error> {
        let mut playlists = Vec::new();
        let mut offset = 0;
        for _ in 0..FINGERPRINT_MAX_PAGES {
            let v = self
                .get(
                    &format!("/me/playlists?limit={PAGE_SIZE}&offset={offset}"),
                    Forbidden::AccessDenied,
                )
                .await?;
            let (stamps, next) = model::parse_playlist_stamps(&v, offset)
                .map_err(|UnexpectedShape| Error::UnexpectedShape)?;
            playlists.extend(stamps);
            match next {
                Some(n) => offset = n,
                None => break,
            }
        }
        playlists.sort();
        let v = self
            .get("/me/tracks?limit=1", Forbidden::AccessDenied)
            .await?;
        let (liked_total, liked_newest) =
            model::parse_liked_stamp(&v).map_err(|UnexpectedShape| Error::UnexpectedShape)?;
        Ok(Fingerprint {
            playlists,
            liked_total,
            liked_newest,
        })
    }

    /// One page of the user's liked songs.
    pub async fn liked_tracks(&self, offset: u32) -> Result<TrackPage, Error> {
        check_offset(offset)?;
        let v = self
            .get(
                &format!("/me/tracks?limit={PAGE_SIZE}&offset={offset}"),
                Forbidden::AccessDenied,
            )
            .await?;
        model::parse_track_page(&v, offset).map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// One page of the user's saved albums (`user-library-read`).
    pub async fn saved_albums(&self, offset: u32) -> Result<AlbumPage, Error> {
        check_offset(offset)?;
        let v = self
            .get(
                &format!("/me/albums?limit={PAGE_SIZE}&offset={offset}"),
                Forbidden::AccessDenied,
            )
            .await?;
        model::parse_album_page(&v, offset).map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// One page of an album's tracks. `id` is validated before it reaches a URL path. The tracks
    /// carry no album name (the caller knows which album it asked for).
    pub async fn album_tracks(&self, id: &str, offset: u32) -> Result<TrackPage, Error> {
        if !is_spotify_id(id) {
            return Err(Error::InvalidId);
        }
        check_offset(offset)?;
        let v = self
            .get(
                &format!("/albums/{id}/tracks?limit={PAGE_SIZE}&offset={offset}"),
                Forbidden::AccessDenied,
            )
            .await?;
        model::parse_album_tracks_page(&v, offset).map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// One page of track search results. The text is trimmed and validated, then percent-encoded.
    pub async fn search_tracks(&self, query: &str, offset: u32) -> Result<TrackPage, Error> {
        let q = query.trim();
        if q.is_empty() || q.chars().count() > MAX_TEXT_CHARS || q.chars().any(char::is_control) {
            return Err(Error::InvalidQuery);
        }
        if offset >= SEARCH_MAX_ROWS {
            return Err(Error::InvalidOffset);
        }
        let enc: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
        let v = self
            .get(
                &format!("/search?type=track&q={enc}&limit={SEARCH_PAGE_SIZE}&offset={offset}"),
                Forbidden::AccessDenied,
            )
            .await?;
        model::parse_search_page(&v, offset).map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// One page of a playlist's items. `id` is validated before it reaches a URL path.
    pub async fn playlist_items(&self, id: &str, offset: u32) -> Result<TrackPage, Error> {
        if !is_spotify_id(id) {
            return Err(Error::InvalidId);
        }
        check_offset(offset)?;
        let v = self
            .get(
                &format!("/playlists/{id}/items?limit={PAGE_SIZE}&offset={offset}"),
                Forbidden::NotReadable,
            )
            .await?;
        model::parse_track_page(&v, offset).map_err(|UnexpectedShape| Error::UnexpectedShape)
    }

    /// One GET with the gate, the token and the single 401 retry. `path` starts with `/`.
    async fn get(&self, path: &str, on_403: Forbidden) -> Result<Value, Error> {
        let resp = self.send(reqwest::Method::GET, path, on_403).await?;
        read_json(resp).await
    }

    /// Which of `uris` are saved (liked), one bool per uri in the same order. A uri that is not
    /// exactly `spotify:track:<22 base62>` is answered false and never sent. At most 40 uris go
    /// in one request.
    pub async fn liked(&self, uris: &[String]) -> Result<Vec<bool>, Error> {
        let mut out = vec![false; uris.len()];
        let valid: Vec<usize> = (0..uris.len())
            .filter(|&i| parse_track_uri(&uris[i]).is_some_and(|c| c == uris[i]))
            .collect();
        for batch in valid.chunks(LIKED_BATCH) {
            let joined = batch
                .iter()
                .map(|&i| encode_uri(&uris[i]))
                .collect::<Vec<_>>()
                .join(",");
            let v = self
                .get(
                    &format!("/me/library/contains?uris={joined}"),
                    Forbidden::AccessDenied,
                )
                .await?;
            let answers = v.as_array().ok_or(Error::UnexpectedShape)?;
            if answers.len() != batch.len() {
                return Err(Error::UnexpectedShape);
            }
            for (&i, a) in batch.iter().zip(answers) {
                out[i] = a.as_bool().ok_or(Error::UnexpectedShape)?;
            }
        }
        Ok(out)
    }

    /// Save (`liked`) or remove one track. An invalid uri or a login known to lack
    /// `user-library-modify` fails with no request. A 403 on the write forgets the scope while the
    /// granted scopes are unknown (so the heart reads as not writable until the next login);
    /// with the scope known to be granted the refusal is reported and the scope stays.
    pub async fn set_liked(&self, uri: &str, liked: bool) -> Result<(), Error> {
        if parse_track_uri(uri).is_none_or(|c| c != uri) {
            return Err(Error::InvalidId);
        }
        if self.tokens.has_scope(SCOPE_LIBRARY_MODIFY) == Some(false) {
            return Err(Error::AccessDenied);
        }
        let method = if liked {
            reqwest::Method::PUT
        } else {
            reqwest::Method::DELETE
        };
        let r = self
            .send(
                method,
                &format!("/me/library?uris={}", encode_uri(uri)),
                Forbidden::AccessDenied,
            )
            .await;
        match r {
            // The response body is ignored.
            Ok(_) => Ok(()),
            Err(Error::AccessDenied) => {
                if self.tokens.has_scope(SCOPE_LIBRARY_MODIFY).is_none() {
                    self.tokens.forget_scope(SCOPE_LIBRARY_MODIFY);
                }
                Err(Error::AccessDenied)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether a like change can work now: false once the login is known to lack
    /// `user-library-modify`.
    pub fn can_change_likes(&self) -> bool {
        self.tokens.has_scope(SCOPE_LIBRARY_MODIFY) != Some(false)
    }

    /// One request with the gate, the bearer token and the single 401 retry; returns the response
    /// of a 2xx. A write carries an empty body with `Content-Length: 0`. `path` starts with `/`.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        on_403: Forbidden,
    ) -> Result<reqwest::Response, Error> {
        let url = format!("{}{path}", self.base);
        // Token for the retry after a 401 (the first pass asks the token source).
        let mut forced: Option<Secret> = None;
        // At most two passes: the request and, after a 401, one retry with a refreshed token.
        for attempt in 0..2 {
            // Gate first: while it is closed not even a token refresh touches the network.
            self.gate
                .check((self.clock)())
                .map_err(|retry_after_secs| Error::RateLimited { retry_after_secs })?;
            let bearer = match forced.take() {
                Some(t) => t,
                None => self
                    .tokens
                    .access_token()
                    .await
                    .map_err(|e| self.auth_failed(&e))?,
            };
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(bearer.expose());
            if method != reqwest::Method::GET {
                req = req.header(reqwest::header::CONTENT_LENGTH, "0");
            }
            let resp = req.send().await.map_err(|_| Error::Network)?;
            let status = resp.status().as_u16();
            match status {
                200..=299 => return Ok(resp),
                401 if attempt == 0 => {
                    forced = Some(
                        self.tokens
                            .force_refresh()
                            .await
                            .map_err(|e| self.auth_failed(&e))?,
                    );
                }
                401 => return Err(Error::WebNotLoggedIn),
                429 => {
                    let header = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok());
                    let secs = cooldown_secs(header);
                    self.gate.block((self.clock)(), secs);
                    return Err(Error::RateLimited {
                        retry_after_secs: secs,
                    });
                }
                403 => {
                    return Err(match on_403 {
                        Forbidden::NotReadable => Error::NotReadable,
                        Forbidden::AccessDenied => Error::AccessDenied,
                    });
                }
                other => return Err(Error::Http(other)),
            }
        }
        Err(Error::WebNotLoggedIn)
    }

    /// A token-endpoint 429 also closes the gate (do not loop on it); everything else maps over.
    fn auth_failed(&self, e: &AuthError) -> Error {
        if let AuthError::RateLimited { retry_after_secs } = e {
            let secs = match retry_after_secs {
                Some(n) if *n >= 1 => (*n).min(gate::MAX_COOLDOWN_SECS),
                _ => gate::DEFAULT_COOLDOWN_SECS,
            };
            self.gate.block((self.clock)(), secs);
            return Error::RateLimited {
                retry_after_secs: secs,
            };
        }
        from_auth(e)
    }
}

/// What a 403 means for the endpoint being called.
#[derive(Clone, Copy)]
enum Forbidden {
    NotReadable,
    AccessDenied,
}

/// A validated `spotify:track:<id>` percent-encoded for a query value (`:` becomes `%3A`).
fn encode_uri(uri: &str) -> String {
    url::form_urlencoded::byte_serialize(uri.as_bytes()).collect()
}

fn check_offset(offset: u32) -> Result<(), Error> {
    if offset < MAX_ROWS {
        Ok(())
    } else {
        Err(Error::InvalidOffset)
    }
}

/// Read a bounded body and parse it. Any failure is `UnexpectedShape` / `Network`, never a body.
async fn read_json(mut resp: reqwest::Response) -> Result<Value, Error> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| Error::Network)? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(Error::UnexpectedShape);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::UnexpectedShape)
}

#[cfg(test)]
mod tests;
