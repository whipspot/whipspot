//! [`Library`] for Spotify: delegates to [`WebApi`], which validates arguments before any token
//! or network use. [`webapi::Error`] maps to [`LibraryError`] with the same wire kind, text and
//! retry seconds.

use crate::{provider::SpotifyProvider, webapi};
use whipspot_core::provider::{
    AlbumDetail, AlbumPage, ArtistPage, BoxFuture, LibErrorKind, Library, LibraryError,
    PlaylistPage, ReleaseGroup, TrackPage,
};

impl From<webapi::Error> for LibraryError {
    fn from(e: webapi::Error) -> Self {
        use webapi::Error as E;
        let kind = match &e {
            E::RateLimited { .. } => LibErrorKind::RateLimited,
            E::WebNotLoggedIn => LibErrorKind::WebNotLoggedIn,
            E::NotReadable => LibErrorKind::NotReadable,
            E::AccessDenied => LibErrorKind::AccessDenied,
            E::InvalidId | E::InvalidOffset | E::InvalidQuery => LibErrorKind::InvalidArgument,
            E::UnexpectedShape => LibErrorKind::UnexpectedShape,
            E::Http(_) => LibErrorKind::Http,
            E::Network => LibErrorKind::Network,
            E::CredentialStore => LibErrorKind::CredentialStore,
            E::AuthFailed => LibErrorKind::AuthFailed,
        };
        LibraryError {
            kind,
            message: e.to_string(),
            retry_after_secs: e.retry_after_secs(),
        }
    }
}

/// Logs a failed library call with what was asked and the error's kind and text (fixed texts),
/// then maps it over.
fn logged<T>(
    r: Result<T, webapi::Error>,
    what: impl FnOnce() -> String,
) -> Result<T, LibraryError> {
    r.map_err(|e| {
        log::warn!("spotify library: {} failed: {} ({e})", what(), e.kind());
        e.into()
    })
}

impl Library for SpotifyProvider {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.playlists(offset).await, || {
                format!("playlists offset {offset}")
            })
        })
    }

    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.liked_tracks(offset).await, || {
                format!("liked tracks offset {offset}")
            })
        })
    }

    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.playlist_items(id, offset).await, || {
                format!("playlist {id} items offset {offset}")
            })
        })
    }

    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.saved_albums(offset).await, || {
                format!("saved albums offset {offset}")
            })
        })
    }

    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.album_tracks(id, offset).await, || {
                format!("album {id} tracks offset {offset}")
            })
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.search_tracks(query, offset).await, || {
                format!("search offset {offset}")
            })
        })
    }

    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        Box::pin(async move {
            logged(self.api.liked(uris).await, || {
                format!("liked check of {} uris", uris.len())
            })
        })
    }

    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        Box::pin(async move {
            logged(self.api.set_liked(uri, liked).await, || {
                format!("set liked {uri} {liked}")
            })
        })
    }

    fn can_change_likes(&self) -> bool {
        self.api.can_change_likes()
    }

    fn artist<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<ArtistPage, LibraryError>> {
        Box::pin(async move { logged(self.api.artist_page(uri).await, || format!("artist {uri}")) })
    }

    fn album<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<AlbumDetail, LibraryError>> {
        Box::pin(async move { logged(self.api.album(uri).await, || format!("album {uri}")) })
    }

    fn artist_releases<'a>(
        &'a self,
        uri: &'a str,
        group: ReleaseGroup,
        offset: u32,
    ) -> BoxFuture<'a, Result<AlbumPage, LibraryError>> {
        Box::pin(async move {
            logged(self.api.artist_releases(uri, group, offset).await, || {
                format!("artist {uri} releases {group:?} offset {offset}")
            })
        })
    }

    fn check_changes(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.watch.check_limited())
    }
}
