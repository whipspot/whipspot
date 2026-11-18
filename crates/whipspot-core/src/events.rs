//! Events the player host sends to the UI.
//!
//! Wire format (serde): an object with `type` (camelCase) and camelCase fields, e.g.
//! `{"type":"playing","uri":"spotify:track:...","positionMs":1234}`. The TypeScript counterpart
//! is `BackendEvent` in `src/backend.ts` (it ignores fields it does not know).
//!
//! `message` is always one of this crate's fixed texts, never a library error and never a token.

use serde::Serialize;

use crate::provider::Track;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiEvent {
    Loading {
        uri: String,
        position_ms: u32,
    },
    Playing {
        uri: String,
        position_ms: u32,
    },
    Paused {
        uri: String,
        position_ms: u32,
    },
    Stopped {
        uri: String,
    },
    Seeked {
        uri: String,
        position_ms: u32,
    },
    /// Periodic position (a provider's position updates) and position corrections.
    Position {
        uri: String,
        position_ms: u32,
    },
    TrackChanged {
        uri: String,
        duration_ms: u32,
    },
    EndOfTrack {
        uri: String,
    },
    /// The provider is about to need the next track (time to preload it for a gapless start).
    PreloadNext {
        uri: String,
    },
    /// The provider cannot play this track (region, removed, ...). The frontend decides what next.
    Unavailable {
        uri: String,
    },
    /// The provider's library is not what it was (a scan finished, a sync brought changes):
    /// `provider` is its id. A front-end showing that library loads it again. Any provider may
    /// send it; one that never does keeps its lists as first loaded.
    LibraryChanged {
        provider: String,
    },
    /// A remote (another device of the user's) took control of `provider`'s playback: the
    /// player stops what plays, gives the engine to the provider's `Remote::owner` and forwards
    /// its transport buttons there until `RemoteEnded` or a local play.
    RemoteStarted {
        provider: String,
    },
    /// The remote's current track (only while `provider` is remote-controlled).
    RemoteTrack {
        provider: String,
        track: Track,
    },
    /// The remote's play state for `uri` (only while `provider` is remote-controlled).
    RemoteState {
        provider: String,
        uri: String,
        state: RemotePlay,
        position_ms: u32,
    },
    /// The remote set the volume (0..=100); applied, never sent back.
    RemoteVolume {
        provider: String,
        volume: u8,
    },
    /// Remote control of `provider` ended (the remote let go or moved elsewhere).
    RemoteEnded {
        provider: String,
    },
    /// `provider`'s login status changed by itself (a background service started, failed or
    /// stopped): the player reads its status again and the Accounts view shows the new one.
    AuthChanged {
        provider: String,
    },
    /// The provider's session was found dead; the host reconnects on the next command.
    SessionLost,
    Error {
        message: String,
    },
}

/// A remote session's play state ([`UiEvent::RemoteState`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemotePlay {
    Loading,
    Playing,
    Paused,
    Stopped,
}

impl UiEvent {
    pub fn error(message: impl Into<String>) -> Self {
        UiEvent::Error {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn json(e: &UiEvent) -> String {
        serde_json::to_string(e).unwrap()
    }

    #[test]
    fn serialisation_snapshot() {
        let cases: Vec<(UiEvent, String)> = vec![
            (
                UiEvent::Loading {
                    uri: URI.into(),
                    position_ms: 0,
                },
                format!(r#"{{"type":"loading","uri":"{URI}","positionMs":0}}"#),
            ),
            (
                UiEvent::Playing {
                    uri: URI.into(),
                    position_ms: 1234,
                },
                format!(r#"{{"type":"playing","uri":"{URI}","positionMs":1234}}"#),
            ),
            (
                UiEvent::Paused {
                    uri: URI.into(),
                    position_ms: 5,
                },
                format!(r#"{{"type":"paused","uri":"{URI}","positionMs":5}}"#),
            ),
            (
                UiEvent::Stopped { uri: URI.into() },
                format!(r#"{{"type":"stopped","uri":"{URI}"}}"#),
            ),
            (
                UiEvent::Seeked {
                    uri: URI.into(),
                    position_ms: 9,
                },
                format!(r#"{{"type":"seeked","uri":"{URI}","positionMs":9}}"#),
            ),
            (
                UiEvent::Position {
                    uri: URI.into(),
                    position_ms: 250,
                },
                format!(r#"{{"type":"position","uri":"{URI}","positionMs":250}}"#),
            ),
            (
                UiEvent::TrackChanged {
                    uri: URI.into(),
                    duration_ms: 180000,
                },
                format!(r#"{{"type":"trackChanged","uri":"{URI}","durationMs":180000}}"#),
            ),
            (
                UiEvent::EndOfTrack { uri: URI.into() },
                format!(r#"{{"type":"endOfTrack","uri":"{URI}"}}"#),
            ),
            (
                UiEvent::PreloadNext { uri: URI.into() },
                format!(r#"{{"type":"preloadNext","uri":"{URI}"}}"#),
            ),
            (
                UiEvent::Unavailable { uri: URI.into() },
                format!(r#"{{"type":"unavailable","uri":"{URI}"}}"#),
            ),
            (
                UiEvent::LibraryChanged {
                    provider: "local".into(),
                },
                r#"{"type":"libraryChanged","provider":"local"}"#.to_string(),
            ),
            (
                UiEvent::RemoteStarted {
                    provider: "spotify".into(),
                },
                r#"{"type":"remoteStarted","provider":"spotify"}"#.to_string(),
            ),
            (
                UiEvent::RemoteTrack {
                    provider: "spotify".into(),
                    track: Track {
                        uri: URI.into(),
                        name: "Song".into(),
                        artist: "Artist".into(),
                        album: "Album".into(),
                        duration_ms: 180000,
                        images: vec![],
                        year: Some(1999),
                        album_ref: None,
                        artists: vec![],
                    },
                },
                format!(
                    r#"{{"type":"remoteTrack","provider":"spotify","track":{{"uri":"{URI}","name":"Song","artist":"Artist","album":"Album","durationMs":180000,"images":[],"year":1999}}}}"#
                ),
            ),
            (
                UiEvent::RemoteState {
                    provider: "spotify".into(),
                    uri: URI.into(),
                    state: RemotePlay::Paused,
                    position_ms: 42,
                },
                format!(
                    r#"{{"type":"remoteState","provider":"spotify","uri":"{URI}","state":"paused","positionMs":42}}"#
                ),
            ),
            (
                UiEvent::RemoteVolume {
                    provider: "spotify".into(),
                    volume: 40,
                },
                r#"{"type":"remoteVolume","provider":"spotify","volume":40}"#.to_string(),
            ),
            (
                UiEvent::RemoteEnded {
                    provider: "spotify".into(),
                },
                r#"{"type":"remoteEnded","provider":"spotify"}"#.to_string(),
            ),
            (
                UiEvent::AuthChanged {
                    provider: "spotify".into(),
                },
                r#"{"type":"authChanged","provider":"spotify"}"#.to_string(),
            ),
            (
                UiEvent::SessionLost,
                r#"{"type":"sessionLost"}"#.to_string(),
            ),
            (
                UiEvent::error("could not start a Spotify session"),
                r#"{"type":"error","message":"could not start a Spotify session"}"#.to_string(),
            ),
        ];
        for (ev, want) in cases {
            assert_eq!(json(&ev), want);
        }
    }
}
