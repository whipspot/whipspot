//! The Connect player's event pump: librespot's `PlayerEvent`s as the core's remote events, and
//! the engine hints (MD24, step 3's [`TrackHints`]).
//!
//! * `SessionConnected` (Spirc became the active device) is `RemoteStarted`, `SessionDisconnected`
//!   `RemoteEnded`. Before `RemoteStarted` goes out the mixer forgets its levels in flight
//!   ([`ConnectMixer::reset`]): Spirc drops every command while it is not active, so a level sent
//!   in a handoff's gap never comes back, and the core sends its takeover level only after
//!   `RemoteStarted`, which must reach Spirc (D5).
//! * `TrackChanged` is `RemoteTrack`: Spotify's cloud chose it, so its texts are cleaned with
//!   `display_text` and capped, and only https covers on Spotify's image hosts are kept.
//! * The play state events are `RemoteState`; `Seeked` and `PositionCorrection` carry the last
//!   known state, `PositionChanged` (sent only while playing) is Playing.
//! * A new play request whose predecessor did not end interrupts the engine (a skip: no tail of
//!   the old track); the latest request's `EndOfTrack` ends the track in the engine (gapless).
//!
//! Nothing here logs; a payload field other than the ones named above is never read.

use std::sync::{Arc, atomic::AtomicBool};

use librespot_core::SpotifyUri;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use tokio::sync::broadcast;
use url::Url;

use whipspot_core::{
    engine::SourceWriter,
    events::{RemotePlay, UiEvent},
    provider::model::{ImageRef, MAX_IMAGES, Track},
    text::display_text,
};

use super::mixer::ConnectMixer;
use crate::{
    provider::{ID, IMAGE_HOSTS},
    session::{Hint, TrackHints},
    webapi::model::MAX_TEXT_CHARS,
};

/// Longest cover URL kept (as `parse_images`).
const MAX_IMAGE_URL_BYTES: usize = 2048;
/// Largest cover side kept as known (as `parse_images`).
const MAX_IMAGE_SIDE: i32 = 10_000;
/// Most artist names joined into one line.
const MAX_ARTISTS: usize = 20;

/// What the engine is told before the event goes out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Audio {
    None,
    /// `source.interrupt()`: a manual switch.
    Interrupt,
    /// `source.end()`: the latest request's track ended.
    End,
}

/// The mapping state of one Connect player.
pub(crate) struct Pump {
    hints: TrackHints,
    /// The last play state seen, for the events that carry none.
    last: RemotePlay,
    /// The host's mixer: its levels in flight end at each activation.
    mixer: Arc<ConnectMixer>,
}

impl Pump {
    pub(crate) fn new(mixer: Arc<ConnectMixer>) -> Self {
        Pump {
            // The `ended` flag is the local stack's (its `load` consumes it); unused here.
            hints: TrackHints::new(Arc::new(AtomicBool::new(false))),
            last: RemotePlay::Loading,
            mixer,
        }
    }

    pub(crate) fn on_event(&mut self, ev: &PlayerEvent) -> (Audio, Option<UiEvent>) {
        let audio = match self.hints.on_event(ev) {
            Hint::End => Audio::End,
            Hint::NewRequest { manual: true } => Audio::Interrupt,
            Hint::NewRequest { manual: false } | Hint::None => Audio::None,
        };
        (audio, self.map(ev))
    }

    fn map(&mut self, ev: &PlayerEvent) -> Option<UiEvent> {
        let provider = || ID.to_owned();
        let state = |last: &mut RemotePlay, s: RemotePlay, id: &SpotifyUri, ms: u32| {
            *last = s;
            Some(UiEvent::RemoteState {
                provider: provider(),
                uri: id.to_uri().ok()?,
                state: s,
                position_ms: ms,
            })
        };
        match ev {
            PlayerEvent::SessionConnected { .. } => {
                // What Spirc was sent while it was not active it dropped: forgotten before the
                // core hears of the takeover, so the level it then sends is weighed against
                // Spirc's own.
                self.mixer.reset();
                Some(UiEvent::RemoteStarted {
                    provider: provider(),
                })
            }
            PlayerEvent::SessionDisconnected { .. } => Some(UiEvent::RemoteEnded {
                provider: provider(),
            }),
            PlayerEvent::TrackChanged { audio_item } => Some(UiEvent::RemoteTrack {
                provider: provider(),
                track: to_track(audio_item)?,
            }),
            PlayerEvent::Loading {
                track_id,
                position_ms,
                ..
            } => state(&mut self.last, RemotePlay::Loading, track_id, *position_ms),
            PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::PositionChanged {
                track_id,
                position_ms,
                ..
            } => state(&mut self.last, RemotePlay::Playing, track_id, *position_ms),
            PlayerEvent::Paused {
                track_id,
                position_ms,
                ..
            } => state(&mut self.last, RemotePlay::Paused, track_id, *position_ms),
            PlayerEvent::Stopped { track_id, .. } => {
                state(&mut self.last, RemotePlay::Stopped, track_id, 0)
            }
            PlayerEvent::Seeked {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::PositionCorrection {
                track_id,
                position_ms,
                ..
            } => {
                let last = self.last;
                state(&mut self.last, last, track_id, *position_ms)
            }
            _ => None,
        }
    }
}

/// Run the pump on the Connect player's own event channel until the player is gone.
pub(crate) async fn run(
    mut channel: PlayerEventChannel,
    source: SourceWriter,
    events: broadcast::Sender<UiEvent>,
    mixer: Arc<ConnectMixer>,
) {
    let mut pump = Pump::new(mixer);
    while let Some(ev) = channel.recv().await {
        let (audio, ui) = pump.on_event(&ev);
        // Before the UI event, as the local pump does (MD24).
        match audio {
            Audio::None => {}
            Audio::Interrupt => source.interrupt(),
            Audio::End => source.end(),
        }
        if let Some(ui) = ui {
            // No receivers is fine.
            let _ = events.send(ui);
        }
    }
}

/// Text from Spotify's cloud made safe to draw and capped (as the Web API's `text`): the work on
/// a giant string is bounded first.
fn clean(s: &str) -> String {
    let prefix: String = s.chars().take(MAX_TEXT_CHARS * 4).collect();
    display_text(&prefix, MAX_TEXT_CHARS)
}

/// The remote's track as a core [`Track`]; `None` when its id has no URI.
pub(crate) fn to_track(item: &AudioItem) -> Option<Track> {
    let uri = item.track_id.to_uri().ok()?;
    let (artist, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => {
            let names: Vec<&str> = artists
                .0
                .iter()
                .take(MAX_ARTISTS)
                .map(|a| a.name.as_str())
                .collect();
            (names.join(", "), album.as_str())
        }
        UniqueFields::Episode { show_name, .. } => (show_name.clone(), ""),
        UniqueFields::Local { artists, album, .. } => (
            artists.clone().unwrap_or_default(),
            album.as_deref().unwrap_or(""),
        ),
    };
    Some(Track {
        uri,
        name: clean(&item.name),
        artist: clean(&artist),
        album: clean(album),
        duration_ms: u64::from(item.duration_ms),
        images: covers(item),
        year: None,
    })
}

/// The `parse_images` rule plus the host check: a URL of 1..=2,048 bytes, https, whose host is
/// one of [`IMAGE_HOSTS`] or under one; at most [`MAX_IMAGES`], in order.
fn covers(item: &AudioItem) -> Vec<ImageRef> {
    let side = |n: i32| (1..=MAX_IMAGE_SIDE).contains(&n).then_some(n as u32);
    item.covers
        .iter()
        .filter(|c| allowed_image(&c.url))
        .map(|c| ImageRef {
            url: c.url.clone(),
            width: side(c.width),
            height: side(c.height),
        })
        .take(MAX_IMAGES)
        .collect()
}

fn allowed_image(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_IMAGE_URL_BYTES || !url.starts_with("https://") {
        return false;
    }
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && IMAGE_HOSTS
            .iter()
            .any(|h| host == *h || host.strip_suffix(h).is_some_and(|r| r.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::mixer::{DEFAULT_LEVEL, to_level};
    use librespot_core::date::Date;
    use librespot_metadata::{
        artist::{ArtistRole, ArtistWithRole, ArtistsWithRole},
        audio::{AudioFiles, item::CoverImage},
        image::ImageSize,
    };

    const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
    const EPISODE: &str = "spotify:episode:512ojhOuo1ktJprKbVcKyQ";

    /// A mixer whose reports go nowhere.
    fn mixer() -> Arc<ConnectMixer> {
        Arc::new(ConnectMixer::new(DEFAULT_LEVEL, broadcast::channel(1).0))
    }

    fn id(uri: &str) -> SpotifyUri {
        SpotifyUri::from_uri(uri).unwrap()
    }

    fn cover(url: &str, w: i32) -> CoverImage {
        CoverImage {
            url: url.into(),
            size: ImageSize::DEFAULT,
            width: w,
            height: w,
        }
    }

    fn item(uri: &str, unique_fields: UniqueFields, covers: Vec<CoverImage>) -> AudioItem {
        AudioItem {
            track_id: id(uri),
            uri: uri.into(),
            files: AudioFiles::default(),
            name: "Song\u{202E}\nTwo".into(),
            covers,
            language: vec![],
            duration_ms: 180_000,
            is_explicit: false,
            availability: Ok(()),
            alternatives: None,
            unique_fields,
        }
    }

    fn artist(name: &str) -> ArtistWithRole {
        ArtistWithRole {
            id: id("spotify:artist:0OdUWJ0sBjDrqHygGUXeCF"),
            name: name.into(),
            role: ArtistRole::ARTIST_ROLE_MAIN_ARTIST,
        }
    }

    fn track_item(covers: Vec<CoverImage>) -> AudioItem {
        item(
            TRACK,
            UniqueFields::Track {
                artists: ArtistsWithRole(vec![artist("A"), artist("B\tC")]),
                album: "Album".into(),
                album_artists: vec![],
                popularity: 0,
                number: 1,
                disc_number: 1,
            },
            covers,
        )
    }

    fn state(uri: &str, state: RemotePlay, position_ms: u32) -> Option<UiEvent> {
        Some(UiEvent::RemoteState {
            provider: "spotify".into(),
            uri: uri.into(),
            state,
            position_ms,
        })
    }

    #[test]
    fn a_track_maps_with_clean_texts_and_only_spotify_covers() {
        let covers = vec![
            cover("https://i.scdn.co/image/ab67616d0000b273", 640),
            // A foreign host, http, a look-alike host and user info are all dropped.
            cover("https://evil.example/image/x", 640),
            cover("http://i.scdn.co/image/y", 300),
            cover("https://notscdn.co/image/z", 300),
            cover("https://user@i.scdn.co/image/u", 300),
            cover("https://image-cdn-fa.spotifycdn.com/image/w", 0),
            cover("https://scdn.co.evil.example/image/v", 300),
        ];
        let mut p = Pump::new(mixer());
        let (audio, ev) = p.on_event(&PlayerEvent::TrackChanged {
            audio_item: Box::new(track_item(covers)),
        });
        assert_eq!(audio, Audio::None);
        assert_eq!(
            ev,
            Some(UiEvent::RemoteTrack {
                provider: "spotify".into(),
                track: Track {
                    uri: TRACK.into(),
                    name: "Song Two".into(),
                    artist: "A, B C".into(),
                    album: "Album".into(),
                    duration_ms: 180_000,
                    images: vec![
                        ImageRef {
                            url: "https://i.scdn.co/image/ab67616d0000b273".into(),
                            width: Some(640),
                            height: Some(640),
                        },
                        ImageRef {
                            url: "https://image-cdn-fa.spotifycdn.com/image/w".into(),
                            width: None,
                            height: None,
                        },
                    ],
                    year: None,
                },
            })
        );
    }

    #[test]
    fn an_episode_keeps_its_uri_and_shows_its_show() {
        let ep = item(
            EPISODE,
            UniqueFields::Episode {
                description: "never read".into(),
                publish_time: Date::now_utc(),
                show_name: "The Show".into(),
            },
            vec![],
        );
        let t = to_track(&ep).unwrap();
        assert_eq!(t.uri, EPISODE);
        assert_eq!(t.artist, "The Show");
        assert_eq!(t.album, "");
        assert!(t.images.is_empty());
    }

    #[test]
    fn long_texts_and_many_covers_are_capped() {
        let mut it = track_item(
            (0..10)
                .map(|i| cover(&format!("https://i.scdn.co/image/{i}"), 64))
                .collect(),
        );
        it.name = "x".repeat(10_000);
        let t = to_track(&it).unwrap();
        assert_eq!(t.name.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(t.images.len(), MAX_IMAGES);
        it.covers = vec![cover(
            &format!("https://i.scdn.co/{}", "a".repeat(3000)),
            64,
        )];
        assert!(to_track(&it).unwrap().images.is_empty());
    }

    #[test]
    fn session_and_state_events_map_to_remote_events() {
        let mut p = Pump::new(mixer());
        let mut m = |e: PlayerEvent| p.on_event(&e).1;
        assert_eq!(
            m(PlayerEvent::SessionConnected {
                connection_id: "c".into(),
                user_name: "u".into()
            }),
            Some(UiEvent::RemoteStarted {
                provider: "spotify".into()
            })
        );
        let t = || id(TRACK);
        assert_eq!(
            m(PlayerEvent::Loading {
                play_request_id: 1,
                track_id: t(),
                position_ms: 5
            }),
            state(TRACK, RemotePlay::Loading, 5)
        );
        assert_eq!(
            m(PlayerEvent::Playing {
                play_request_id: 1,
                track_id: t(),
                position_ms: 6
            }),
            state(TRACK, RemotePlay::Playing, 6)
        );
        assert_eq!(
            m(PlayerEvent::PositionChanged {
                play_request_id: 1,
                track_id: t(),
                position_ms: 250
            }),
            state(TRACK, RemotePlay::Playing, 250)
        );
        assert_eq!(
            m(PlayerEvent::Paused {
                play_request_id: 1,
                track_id: t(),
                position_ms: 300
            }),
            state(TRACK, RemotePlay::Paused, 300)
        );
        // A seek while paused stays paused.
        assert_eq!(
            m(PlayerEvent::Seeked {
                play_request_id: 1,
                track_id: t(),
                position_ms: 9000
            }),
            state(TRACK, RemotePlay::Paused, 9000)
        );
        assert_eq!(
            m(PlayerEvent::PositionCorrection {
                play_request_id: 1,
                track_id: t(),
                position_ms: 9100
            }),
            state(TRACK, RemotePlay::Paused, 9100)
        );
        assert_eq!(
            m(PlayerEvent::Stopped {
                play_request_id: 1,
                track_id: t()
            }),
            state(TRACK, RemotePlay::Stopped, 0)
        );
        // The episode's own uri.
        assert_eq!(
            m(PlayerEvent::Playing {
                play_request_id: 2,
                track_id: id(EPISODE),
                position_ms: 0
            }),
            state(EPISODE, RemotePlay::Playing, 0)
        );
        assert_eq!(
            m(PlayerEvent::SessionDisconnected {
                connection_id: "c".into(),
                user_name: "u".into()
            }),
            Some(UiEvent::RemoteEnded {
                provider: "spotify".into()
            })
        );
        // The rest is not the core's (volume comes through the mixer).
        assert_eq!(m(PlayerEvent::VolumeChanged { volume: 3 }), None);
        assert_eq!(
            m(PlayerEvent::EndOfTrack {
                play_request_id: 2,
                track_id: id(EPISODE)
            }),
            None
        );
        assert_eq!(
            m(PlayerEvent::TimeToPreloadNextTrack {
                play_request_id: 2,
                track_id: t()
            }),
            None
        );
    }

    #[test]
    fn a_skip_interrupts_and_the_latest_end_ends() {
        let mut p = Pump::new(mixer());
        let mut a = |e: PlayerEvent| p.on_event(&e).0;
        let changed = |id| PlayerEvent::PlayRequestIdChanged {
            play_request_id: id,
        };
        let end = |id| PlayerEvent::EndOfTrack {
            play_request_id: id,
            track_id: SpotifyUri::from_uri(TRACK).unwrap(),
        };
        assert_eq!(a(changed(1)), Audio::Interrupt);
        // A skip: id 2 with no end of id 1.
        assert_eq!(a(changed(2)), Audio::Interrupt);
        // A stale end of id 1 does nothing (M3-F1).
        assert_eq!(a(end(1)), Audio::None);
        assert_eq!(a(end(2)), Audio::End);
        // Gapless: the next request after an end does not interrupt.
        assert_eq!(a(changed(3)), Audio::None);
    }

    #[test]
    fn an_activation_forgets_the_levels_in_flight_before_remote_started() {
        let m = mixer();
        let mut p = Pump::new(m.clone());
        // The core set 40 in a handoff's gap: Spirc, no longer active, drops it.
        let v = m.forward(40).expect("sent");
        assert_eq!(v, to_level(40));
        // The events of a session in progress leave it in flight (a slider drag spans them).
        let t = || id(TRACK);
        for ev in [
            PlayerEvent::PlayRequestIdChanged { play_request_id: 1 },
            PlayerEvent::Loading {
                play_request_id: 1,
                track_id: t(),
                position_ms: 0,
            },
            PlayerEvent::TrackChanged {
                audio_item: Box::new(track_item(vec![])),
            },
            PlayerEvent::Playing {
                play_request_id: 1,
                track_id: t(),
                position_ms: 0,
            },
            PlayerEvent::PositionChanged {
                play_request_id: 1,
                track_id: t(),
                position_ms: 1000,
            },
            PlayerEvent::Seeked {
                play_request_id: 1,
                track_id: t(),
                position_ms: 9000,
            },
            PlayerEvent::Paused {
                play_request_id: 1,
                track_id: t(),
                position_ms: 9000,
            },
            PlayerEvent::VolumeChanged { volume: 3 },
            PlayerEvent::EndOfTrack {
                play_request_id: 1,
                track_id: t(),
            },
        ] {
            p.on_event(&ev);
            assert_eq!(m.forward(40), None, "still in flight after {ev:?}");
        }
        // Spirc becomes the active device again: the dropped level is forgotten by the time
        // RemoteStarted goes out, so the core's takeover level (the same 40) is sent.
        let (_, ev) = p.on_event(&PlayerEvent::SessionConnected {
            connection_id: "c".into(),
            user_name: "u".into(),
        });
        assert_eq!(
            ev,
            Some(UiEvent::RemoteStarted {
                provider: "spotify".into()
            })
        );
        assert_eq!(m.forward(40), Some(v));
    }
}
