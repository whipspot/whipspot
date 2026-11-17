//! The real librespot glue behind [`PlayerBackend`]: Session + Player + the event pump.
//!
//! * Player-only flow (no Spirc here, decision D3; the Connect speaker is a second stack in
//!   `connect::`, with its own Session, Player and Spirc, D55): `Session::connect` with an access
//!   token, `Player::new`, `load`.
//! * The `Player` is created on the first connect (it needs a `Session` up front) and reused
//!   afterwards through `set_session`, so its sink stays valid. A logout or a new login ends the
//!   session (`disconnect`: the player stops, the session shuts down, MD137); the player waits
//!   for the next one and loads nothing without it.
//! * The audio sink is built on librespot's player thread. [`SpotifyOutput::Core`]: a
//!   [`CoreSink`] over the engine's source, with its own player event channel (registered right
//!   after `Player::new`, before any load).
//! * Core hints (MD24): the event pump calls `source.end()` at `EndOfTrack` before forwarding it;
//!   `load` interrupts the engine unless it follows an `EndOfTrack` (a gapless continuation).
//! * Nothing here logs or formats a token; library errors are mapped to [`ConnectError`] and
//!   dropped.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use librespot_core::{Session, SessionConfig, SpotifyUri, authentication::Credentials};
use librespot_playback::{
    config::{Bitrate, PlayerConfig},
    mixer::NoOpVolume,
    player::{Player, PlayerEvent, PlayerEventChannel},
};
use tokio::sync::broadcast;

use crate::{
    core_sink::{CoreSink, EventSlot},
    events::map_player_event,
    player_host::{ConnectError, Error, PlayerBackend, SessionState},
    provider::{Quality, SpotifyOutput},
};
use whipspot_core::{engine::SourceWriter, events::UiEvent, provider::BoxFuture, secret::Secret};

/// How long to wait for the audio sink to appear after `Player::new`.
const SINK_READY_TIMEOUT: Duration = Duration::from_secs(10);

fn player_config(quality: Quality) -> PlayerConfig {
    PlayerConfig {
        gapless: true,
        normalisation: false,
        bitrate: bitrate(quality),
        position_update_interval: Some(Duration::from_millis(250)),
        ..PlayerConfig::default()
    }
}

/// librespot's bitrate for the cap the user chose (MD88).
fn bitrate(quality: Quality) -> Bitrate {
    match quality {
        Quality::Kbps96 => Bitrate::Bitrate96,
        Quality::Kbps160 => Bitrate::Bitrate160,
        Quality::Kbps320 => Bitrate::Bitrate320,
    }
}

#[derive(Default)]
struct State {
    session: Option<Session>,
    player: Option<Arc<Player>>,
}

pub struct LibrespotBackend {
    state: Mutex<State>,
    source: SourceWriter,
    events: broadcast::Sender<UiEvent>,
    /// Set by the event pump at `EndOfTrack`, consumed by the next `load`.
    ended: Arc<AtomicBool>,
    /// The bitrate cap of the player the next start creates (MD88).
    quality: Quality,
}

/// What the event pump does after one player event (MD24).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    None,
    /// The latest request's track ended: call `source.end()` before forwarding the event.
    End,
    /// A new play request id arrived. `manual` is true when the previous request did not end
    /// (a manual switch, not a gapless continuation).
    NewRequest {
        manual: bool,
    },
}

/// The pump's core hints (MD24, M3-F1): tracks the latest play request id and the `ended` flag.
///
/// `EndOfTrack` counts only for the latest request id: a late end of an already replaced request
/// neither sets `ended` nor asks for `source.end()`. A counted end sets `ended` (consumed by the
/// next `load`). `PlayRequestIdChanged` proves a new load reached librespot, so it records the
/// id and clears the flag: a later manual load then interrupts. A load right after a gapless
/// `EndOfTrack` has already consumed the flag in `load`, so the clear is a no-op on that path.
/// Every other event leaves the state alone.
pub(crate) struct TrackHints {
    latest: Option<u64>,
    /// The latest request's end was counted (separate from `ended`, which `load` consumes).
    end_seen: bool,
    ended: Arc<AtomicBool>,
}

impl TrackHints {
    pub(crate) fn new(ended: Arc<AtomicBool>) -> Self {
        TrackHints {
            latest: None,
            end_seen: false,
            ended,
        }
    }

    pub(crate) fn on_event(&mut self, ev: &PlayerEvent) -> Hint {
        match ev {
            PlayerEvent::EndOfTrack {
                play_request_id, ..
            } if self.latest == Some(*play_request_id) => {
                self.end_seen = true;
                self.ended.store(true, Ordering::SeqCst);
                Hint::End
            }
            PlayerEvent::PlayRequestIdChanged { play_request_id } => {
                let manual = !self.end_seen;
                self.latest = Some(*play_request_id);
                self.end_seen = false;
                self.ended.store(false, Ordering::SeqCst);
                Hint::NewRequest { manual }
            }
            _ => Hint::None,
        }
    }
}

/// Build a `Player` on `session` whose sink is a [`CoreSink`] writing into `source`, and wait for
/// the sink to appear. Returns the player and a fresh event channel; the sink holds its own,
/// registered before any load. Both stacks (local and Connect) use it. Must run inside a tokio
/// runtime.
pub(crate) async fn build_player(
    quality: Quality,
    session: Session,
    source: SourceWriter,
) -> Result<(Arc<Player>, PlayerEventChannel), ConnectError> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let slot = EventSlot::default();
    let sink_slot = slot.clone();
    let player = Player::new(
        player_config(quality),
        session,
        Box::new(NoOpVolume),
        move || {
            // Runs on the player thread.
            let sink = CoreSink::new(source, sink_slot);
            let _ = ready_tx.send(());
            Box::new(sink)
        },
    );
    // Registered before any load command, so the sink sees every event in order.
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(player.get_player_event_channel());
    let ready =
        tokio::task::spawn_blocking(move || ready_rx.recv_timeout(SINK_READY_TIMEOUT)).await;
    if !matches!(ready, Ok(Ok(()))) {
        return Err(ConnectError::NoAudio);
    }
    let channel = player.get_player_event_channel();
    Ok((player, channel))
}

impl LibrespotBackend {
    pub fn new(
        source: SourceWriter,
        _output: SpotifyOutput,
        events: broadcast::Sender<UiEvent>,
    ) -> Self {
        LibrespotBackend {
            state: Mutex::new(State::default()),
            source,
            events,
            ended: Arc::new(AtomicBool::new(false)),
            quality: Quality::default(),
        }
    }

    /// The bitrate cap for the player this backend starts. Set before the first command: the
    /// player is created once, on the first connect.
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }

    fn player(&self) -> Option<Arc<Player>> {
        self.state.lock().unwrap().player.clone()
    }

    /// The player while a session is attached: none after [`PlayerBackend::disconnect`] ended
    /// the account's session (S1), so nothing loads or fetches through it.
    fn live_player(&self) -> Option<Arc<Player>> {
        let state = self.state.lock().unwrap();
        state.session.as_ref().and(state.player.clone())
    }

    /// Create the player and wait for its sink. Must run inside a tokio runtime.
    async fn start_player(&self, session: Session) -> Result<Arc<Player>, ConnectError> {
        let (player, mut channel) =
            build_player(self.quality, session, self.source.clone()).await?;
        let tx = self.events.clone();
        let (source, ended) = (self.source.clone(), self.ended.clone());
        tokio::spawn(async move {
            let mut hints = TrackHints::new(ended);
            while let Some(ev) = channel.recv().await {
                // Before the UI event: the control loop's next `load` must see the end.
                if hints.on_event(&ev) == Hint::End {
                    source.end();
                }
                if let Some(ui) = map_player_event(&ev) {
                    // No receivers is fine.
                    let _ = tx.send(ui);
                }
            }
        });
        Ok(player)
    }
}

impl PlayerBackend for LibrespotBackend {
    fn session_state(&self) -> SessionState {
        match &self.state.lock().unwrap().session {
            None => SessionState::None,
            Some(s) if s.is_invalid() => SessionState::Invalid,
            Some(_) => SessionState::Valid,
        }
    }

    fn connect<'a>(&'a self, access: &'a Secret) -> BoxFuture<'a, Result<(), ConnectError>> {
        Box::pin(async move {
            let session = Session::new(SessionConfig::default(), None);
            session
                .connect(Credentials::with_access_token(access.expose()), false)
                .await
                .map_err(|_| ConnectError::Failed)?;
            let existing = self.player();
            match existing {
                Some(player) => player.set_session(session.clone()),
                None => {
                    let player = self.start_player(session.clone()).await?;
                    self.state.lock().unwrap().player = Some(player);
                }
            }
            self.state.lock().unwrap().session = Some(session);
            Ok(())
        })
    }

    fn load(&self, uri: &str, play: bool, position_ms: u32) -> Result<(), Error> {
        let id = SpotifyUri::from_uri(uri).map_err(|_| Error::InvalidUri)?;
        if !matches!(id, SpotifyUri::Track { .. }) {
            return Err(Error::InvalidUri);
        }
        let player = self.live_player().ok_or(Error::SessionUnavailable)?;
        if !self.ended.swap(false, Ordering::SeqCst) {
            // Not a gapless continuation: silence the old track now; the sink's in-band flush at
            // `TrackChanged` acknowledges it.
            self.source.interrupt();
        }
        player.load(id, play, position_ms);
        Ok(())
    }

    fn preload(&self, uri: &str) -> Result<(), Error> {
        let id = SpotifyUri::from_uri(uri).map_err(|_| Error::InvalidUri)?;
        if !matches!(id, SpotifyUri::Track { .. }) {
            return Err(Error::InvalidUri);
        }
        if let Some(p) = self.live_player() {
            p.preload(id);
        }
        Ok(())
    }

    fn disconnect(&self) {
        let (session, player) = {
            let mut state = self.state.lock().unwrap();
            (state.session.take(), state.player.clone())
        };
        // Stopped first, so nothing more is fetched or played through the session; the player
        // and its sink stay for the next session (`set_session`).
        if let Some(p) = player {
            p.stop();
        }
        if let Some(s) = session {
            s.shutdown();
        }
        // The next load is not a gapless continuation of anything.
        self.ended.store(false, Ordering::SeqCst);
    }

    fn play(&self) {
        if let Some(p) = self.player() {
            p.play();
        }
    }

    fn pause(&self) {
        if let Some(p) = self.player() {
            p.pause();
        }
    }

    fn stop(&self) {
        if let Some(p) = self.player() {
            p.stop();
        }
    }

    fn seek(&self, position_ms: u32) {
        if let Some(p) = self.player() {
            p.seek(position_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_config_asks_for_320_kbps() {
        assert_eq!(player_config(Quality::Kbps320).bitrate, Bitrate::Bitrate320);
    }

    #[test]
    fn player_config_asks_for_160_kbps() {
        assert_eq!(player_config(Quality::Kbps160).bitrate, Bitrate::Bitrate160);
    }

    #[test]
    fn player_config_asks_for_96_kbps() {
        assert_eq!(player_config(Quality::Kbps96).bitrate, Bitrate::Bitrate96);
    }

    fn track() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").unwrap()
    }

    fn end(id: u64) -> PlayerEvent {
        PlayerEvent::EndOfTrack {
            play_request_id: id,
            track_id: track(),
        }
    }

    fn changed(id: u64) -> PlayerEvent {
        PlayerEvent::PlayRequestIdChanged {
            play_request_id: id,
        }
    }

    fn hints() -> (TrackHints, Arc<AtomicBool>) {
        let ended = Arc::new(AtomicBool::new(false));
        (TrackHints::new(ended.clone()), ended)
    }

    #[test]
    fn end_of_latest_request_counts() {
        let (mut h, ended) = hints();
        h.on_event(&changed(1));
        assert_eq!(h.on_event(&end(1)), Hint::End);
        assert!(ended.load(Ordering::SeqCst));
    }

    #[test]
    fn stale_end_of_track_is_ignored() {
        let (mut h, ended) = hints();
        h.on_event(&changed(1));
        h.on_event(&changed(2));
        assert_eq!(h.on_event(&end(1)), Hint::None);
        assert!(!ended.load(Ordering::SeqCst));
    }

    #[test]
    fn end_then_new_request_clears_ended() {
        let (mut h, ended) = hints();
        h.on_event(&changed(1));
        assert_eq!(h.on_event(&end(1)), Hint::End);
        assert!(ended.load(Ordering::SeqCst));
        h.on_event(&changed(2));
        assert!(!ended.load(Ordering::SeqCst));
    }

    #[test]
    fn new_request_signal_is_manual_only_without_an_end() {
        let (mut h, _) = hints();
        h.on_event(&changed(1));
        // Manual switch: id 2 arrives with no end for id 1.
        assert_eq!(h.on_event(&changed(2)), Hint::NewRequest { manual: true });
        // Gapless: id 2 ends, then id 3.
        h.on_event(&end(2));
        assert_eq!(h.on_event(&changed(3)), Hint::NewRequest { manual: false });
        // The next one is manual again.
        assert_eq!(h.on_event(&changed(4)), Hint::NewRequest { manual: true });
    }

    #[test]
    fn unrelated_event_leaves_state() {
        let (mut h, ended) = hints();
        h.on_event(&changed(3));
        h.on_event(&end(3));
        let stopped = PlayerEvent::Stopped {
            play_request_id: 3,
            track_id: track(),
        };
        assert_eq!(h.on_event(&stopped), Hint::None);
        assert!(ended.load(Ordering::SeqCst));
    }
}
