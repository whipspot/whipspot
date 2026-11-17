//! Mapping from librespot's `PlayerEvent` to the provider-neutral [`UiEvent`].

use librespot_playback::player::PlayerEvent;

use whipspot_core::events::UiEvent;

/// Translate a librespot event; `None` for events the UI does not use.
pub fn map_player_event(ev: &PlayerEvent) -> Option<UiEvent> {
    let uri = |id: &librespot_core::SpotifyUri| id.to_uri().ok();
    Some(match ev {
        PlayerEvent::Loading {
            track_id,
            position_ms,
            ..
        } => UiEvent::Loading {
            uri: uri(track_id)?,
            position_ms: *position_ms,
        },
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => UiEvent::Playing {
            uri: uri(track_id)?,
            position_ms: *position_ms,
        },
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => UiEvent::Paused {
            uri: uri(track_id)?,
            position_ms: *position_ms,
        },
        PlayerEvent::Stopped { track_id, .. } => UiEvent::Stopped {
            uri: uri(track_id)?,
        },
        PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        } => UiEvent::Seeked {
            uri: uri(track_id)?,
            position_ms: *position_ms,
        },
        PlayerEvent::PositionChanged {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        } => UiEvent::Position {
            uri: uri(track_id)?,
            position_ms: *position_ms,
        },
        PlayerEvent::TrackChanged { audio_item } => UiEvent::TrackChanged {
            uri: uri(&audio_item.track_id)?,
            duration_ms: audio_item.duration_ms,
        },
        PlayerEvent::EndOfTrack { track_id, .. } => UiEvent::EndOfTrack {
            uri: uri(track_id)?,
        },
        PlayerEvent::Unavailable { track_id, .. } => UiEvent::Unavailable {
            uri: uri(track_id)?,
        },
        PlayerEvent::TimeToPreloadNextTrack { track_id, .. } => UiEvent::PreloadNext {
            uri: uri(track_id)?,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyUri;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn id() -> SpotifyUri {
        SpotifyUri::from_uri(URI).unwrap()
    }

    #[test]
    fn maps_librespot_events() {
        let m = |e: PlayerEvent| map_player_event(&e);
        assert_eq!(
            m(PlayerEvent::Playing {
                play_request_id: 1,
                track_id: id(),
                position_ms: 7
            }),
            Some(UiEvent::Playing {
                uri: URI.into(),
                position_ms: 7
            })
        );
        assert_eq!(
            m(PlayerEvent::PositionChanged {
                play_request_id: 1,
                track_id: id(),
                position_ms: 250
            }),
            Some(UiEvent::Position {
                uri: URI.into(),
                position_ms: 250
            })
        );
        assert_eq!(
            m(PlayerEvent::PositionCorrection {
                play_request_id: 1,
                track_id: id(),
                position_ms: 300
            }),
            Some(UiEvent::Position {
                uri: URI.into(),
                position_ms: 300
            })
        );
        assert_eq!(
            m(PlayerEvent::EndOfTrack {
                play_request_id: 1,
                track_id: id()
            }),
            Some(UiEvent::EndOfTrack { uri: URI.into() })
        );
        assert_eq!(
            m(PlayerEvent::Unavailable {
                play_request_id: 1,
                track_id: id()
            }),
            Some(UiEvent::Unavailable { uri: URI.into() })
        );
        assert_eq!(
            m(PlayerEvent::Stopped {
                play_request_id: 1,
                track_id: id()
            }),
            Some(UiEvent::Stopped { uri: URI.into() })
        );
        assert_eq!(
            m(PlayerEvent::TimeToPreloadNextTrack {
                play_request_id: 1,
                track_id: id()
            }),
            Some(UiEvent::PreloadNext { uri: URI.into() })
        );
        // Events the UI does not use.
        assert_eq!(m(PlayerEvent::VolumeChanged { volume: 3 }), None);
        assert_eq!(m(PlayerEvent::Preloading { track_id: id() }), None);
        assert_eq!(
            m(PlayerEvent::SessionDisconnected {
                connection_id: "c".into(),
                user_name: "u".into()
            }),
            None
        );
    }
}
