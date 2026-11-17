//! [`Library`] for Spotify: delegates to [`WebApi`], which validates arguments before any token
//! or network use. [`webapi::Error`] maps to [`LibraryError`] with the same wire kind, text and
//! retry seconds.

use crate::{provider::SpotifyProvider, webapi};
use whipspot_core::provider::{
    AlbumPage, BoxFuture, LibErrorKind, Library, LibraryError, PlaylistPage, TrackPage,
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

impl Library for SpotifyProvider {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.playlists(offset).await?) })
    }

    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.liked_tracks(offset).await?) })
    }

    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.playlist_items(id, offset).await?) })
    }

    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.saved_albums(offset).await?) })
    }

    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.album_tracks(id, offset).await?) })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async move { Ok(self.api.search_tracks(query, offset).await?) })
    }

    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        Box::pin(async move { Ok(self.api.liked(uris).await?) })
    }

    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        Box::pin(async move { Ok(self.api.set_liked(uri, liked).await?) })
    }

    fn can_change_likes(&self) -> bool {
        self.api.can_change_likes()
    }

    fn check_changes(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.watch.check_limited())
    }
}
