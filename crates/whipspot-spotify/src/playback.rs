//! [`Playback`] for Spotify: delegates to [`PlayerHost`], which validates the URI before any
//! token or network use. Errors cross as the host's fixed `Display` texts.

use crate::{player_host::Error, provider::SpotifyProvider};
use whipspot_core::provider::{BoxFuture, Playback, PlaybackError};

fn fail(e: Error) -> PlaybackError {
    PlaybackError(e.to_string())
}

impl Playback for SpotifyProvider {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.load(uri, play, position_ms).await.map_err(fail) })
    }

    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.play().await.map_err(fail) })
    }

    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.pause().await.map_err(fail) })
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.stop().await.map_err(fail) })
    }

    fn seek(&self, position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.seek(position_ms).await.map_err(fail) })
    }

    fn preload<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move { self.host.preload(uri).await.map_err(fail) })
    }

    /// The cap the user chose (MD88). librespot may fall back to MP3 or a lower bitrate than the
    /// cap, so the label says "up to" and no more.
    fn source_format(&self) -> Option<String> {
        Some(format!(
            "Lossy (Ogg Vorbis or MP3), up to {} kbps",
            self.quality.kbps()
        ))
    }
}
