//! Provider-neutral API: the traits a music provider (Spotify, later others) implements, the
//! models that cross to the webview, and the registry that routes by provider id.
//!
//! This module must not import librespot or any provider crate (the boundary script
//! enforces it). Traits are object-safe and use [`BoxFuture`] so providers live in an
//! `Arc<dyn Provider>`; there is no `async_trait`.
//!
//! Volume, EQ and balance are deliberately not part of [`Playback`]: they are app-level audio
//! controls applied by the audio engine, and a provider declares in [`Capabilities`] whether it
//! feeds the engine at all.

pub mod error;
pub mod model;
pub mod registry;

#[cfg(test)]
pub(crate) mod fake;

use tokio::sync::broadcast;

use crate::engine::SourceWriter;
use crate::events::UiEvent;

/// Boxed, sendable future: the async style of every provider trait (no `async_trait`).
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
pub use error::{AuthFailure, LibErrorKind, LibraryError, PlaybackError};
pub use model::{
    Album, AlbumPage, AuthStepInfo, AuthStepStatus, Capabilities, ImageRef, InputSpec, LoginInput,
    MAX_IMAGES, Playlist, PlaylistPage, ProviderAuthStatus, ProviderGlyph, ProviderInfo, Readable,
    StepHint, Track, TrackPage,
};
pub use registry::{ProviderRegistry, RegisterError};

/// What the app hands each provider at construction: the engine's source writer and the one UI
/// event sender.
#[derive(Clone)]
pub struct ProviderContext {
    /// Where a provider with decoded PCM writes it (the audio engine, MD22).
    pub source: SourceWriter,
    pub events: broadcast::Sender<UiEvent>,
}

impl ProviderContext {
    /// The context for the provider `id`: its source handle writes only while `id` owns the
    /// engine (D13), so a late call from a provider that was switched away from never touches
    /// the next one's audio.
    pub fn for_owner(&self, id: &str) -> ProviderContext {
        ProviderContext {
            source: self.source.for_owner(id),
            events: self.events.clone(),
        }
    }
}

/// One music provider. Held as `Arc<dyn Provider>`.
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    fn auth(&self) -> &dyn AuthFlow;
    fn playback(&self) -> &dyn Playback;
    /// `None` exactly when `info().caps.library` is false (the registry checks this).
    fn library(&self) -> Option<&dyn Library>;
    /// The hosts its image URLs use; an art source fetches only from these (and their
    /// subdomains). None by default.
    fn image_hosts(&self) -> &'static [&'static str] {
        &[]
    }
    /// The provider's remote control, when something outside Whipspot (another device of the
    /// user's) can choose what it plays. `None` by default.
    fn remote(&self) -> Option<&dyn Remote> {
        None
    }
}

/// A remote-controlled session of a provider: the remote chooses the tracks, the player shows
/// them (`UiEvent::RemoteStarted` .. `RemoteEnded`) and forwards its transport buttons here.
/// Every call runs on the player's transport task, in order with loads and stops.
pub trait Remote: Send + Sync {
    /// The engine owner its audio writes as ([`ProviderContext::for_owner`]); never the id of
    /// a provider, so a remote takeover and the next local load each switch the owner (D13).
    fn owner(&self) -> &str;
    /// Forward one transport command to the remote session. With no session active it does
    /// nothing (Ok or an error), at once.
    fn command(&self, cmd: RemoteCommand) -> BoxFuture<'_, Result<(), PlaybackError>>;
    /// Give remote control up (the controlling device sees this one go inactive). With no
    /// session active it does nothing, at once.
    fn release(&self) -> BoxFuture<'_, ()>;
}

/// A transport command forwarded to a [`Remote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCommand {
    Play,
    Pause,
    Next,
    Previous,
    /// Position in ms.
    Seek(u32),
    /// 0..=100.
    Volume(u8),
}

/// Login/logout, as an ordered list of steps (see [`ProviderInfo::auth_steps`]).
pub trait AuthFlow: Send + Sync {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>>;
    /// Run one login step. `input` carries only the keys the step declares.
    fn login<'a>(
        &'a self,
        step: &'a str,
        input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>>;
    fn logout<'a>(
        &'a self,
        step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>>;
    /// The stored, non-secret value of one login input (e.g. a client id) so the UI can prefill
    /// it. `None` when nothing is stored or the provider has no such input.
    fn prefill<'a>(
        &'a self,
        _step: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AuthFailure>> {
        Box::pin(async { Ok(None) })
    }
}

/// Transport of the provider's current track. `uri` is the provider's own track URI; the
/// provider's validator is authoritative.
pub trait Playback: Send + Sync {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>>;
    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>>;
    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>>;
    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>>;
    fn seek(&self, position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>>;
    /// The stream's format as the provider can honestly state it (codec, bitrate); shown to the
    /// user as is.
    fn source_format(&self) -> Option<String> {
        None
    }
    /// Prepare `uri` to follow the current track gaplessly (MD35). The default does nothing.
    fn preload<'a>(&'a self, _uri: &'a str) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Read-only library browsing. One request per call.
pub trait Library: Send + Sync {
    fn playlists(&self, offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>>;
    fn liked_tracks(&self, offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>>;
    fn playlist_items<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>>;
    fn albums(&self, offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>>;
    fn album_tracks<'a>(
        &'a self,
        id: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>>;
    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>>;
    /// Whether each of `uris` is saved (liked): one bool per uri, in the same order. Only when
    /// `caps.likes`.
    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        let _ = uris;
        Box::pin(async { Err(likes_unsupported()) })
    }
    /// Save (`liked`) or remove `uri`. Only when `caps.likes`.
    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        let _ = (uri, liked);
        Box::pin(async { Err(likes_unsupported()) })
    }
    /// Whether a like change can work now (the login allows it). Advisory: the service decides.
    fn can_change_likes(&self) -> bool {
        false
    }
    /// Look whether the library changed since the last look and, if it did, send
    /// [`crate::events::UiEvent::LibraryChanged`] yourself. A hint: it may do nothing, and a
    /// failure is the provider's to swallow. The default returns at once.
    fn check_changes(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn likes_unsupported() -> LibraryError {
    LibraryError::new(LibErrorKind::InvalidArgument, "likes are not supported")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn traits_are_object_safe() {
        let p: Arc<dyn Provider> = Arc::new(fake::FakeProvider::new("fake", true));
        let _: &dyn AuthFlow = p.auth();
        let _: &dyn Playback = p.playback();
        assert!(p.library().is_some());
    }
}
