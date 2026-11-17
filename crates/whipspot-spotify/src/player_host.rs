//! The playback side: validates commands, keeps a librespot session alive and forwards player
//! events. Knows nothing about Tauri.
//!
//! * Only [`PlayerHost::load`] opens a session: token -> (re)connect -> command (the same path
//!   serves an explicit login's first track). A failed connect is retried exactly once with a
//!   force-refreshed token, then [`Error::SessionUnavailable`].
//! * `pause`/`stop`/`preload` act on the player if there is one and never connect. `play`/`seek` also never
//!   start the first session (nothing is loaded; they are no-ops), and only reconnect a session
//!   that died after a track was loaded. So a transport button pressed before any track was
//!   loaded costs no credential store read, token refresh or audio-device initialisation.
//! * The host uses only the playback login ([`PlaybackTokens`]); the web login is a different
//!   type and cannot be passed in:
//!
//! ```compile_fail
//! use std::sync::Arc;
//! use whipspot_spotify::{SpotifyOutput, auth_manager::AuthManager, player_host::PlayerHost};
//! use whipspot_testkit::MemoryStore;
//! let auth = AuthManager::new(Arc::new(MemoryStore::default()));
//! let source = whipspot_core::engine::Engine::new(Default::default(), None).source;
//! let (events, _) = tokio::sync::broadcast::channel(1);
//! let _ = PlayerHost::new(auth.web(), source, SpotifyOutput::Core, events); // WebTokens is not PlaybackTokens
//! ```
//!
//! The same snippet with `auth.playback()` compiles (constructing a host connects to nothing):
//!
//! ```
//! use std::sync::Arc;
//! use whipspot_spotify::{SpotifyOutput, auth_manager::AuthManager, player_host::PlayerHost};
//! use whipspot_testkit::MemoryStore;
//! let auth = AuthManager::new(Arc::new(MemoryStore::default()));
//! let source = whipspot_core::engine::Engine::new(Default::default(), None).source;
//! let (events, _) = tokio::sync::broadcast::channel(1);
//! let _host = PlayerHost::new(auth.playback(), source, SpotifyOutput::Core, events);
//! ```

use std::{fmt, sync::Arc};

use tokio::sync::broadcast;

use crate::{
    auth::Secret,
    auth_manager::{AuthError, BoxFuture, PlaybackTokens, TokenSource},
    provider::SpotifyOutput,
    session::LibrespotBackend,
    uri::parse_track_uri,
};
use whipspot_core::{engine::SourceWriter, events::UiEvent};

/// Buffered UI events before a slow receiver lags.
pub const EVENT_CAPACITY: usize = 256;

/// A command failed. `Display` is fixed text, safe to show; no library error is forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidUri,
    NotLoggedIn,
    /// Any other auth failure (network, rate limit, credential store ...), with its whitelisted text.
    Auth(AuthError),
    /// Connecting failed twice (second time with a fresh token).
    SessionUnavailable,
    /// No usable audio output device.
    AudioUnavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidUri => f.write_str("not a Spotify track"),
            Error::NotLoggedIn => f.write_str("not logged in to Spotify for playback"),
            Error::Auth(e) => e.fmt(f),
            Error::SessionUnavailable => f.write_str("could not start a Spotify session"),
            Error::AudioUnavailable => f.write_str("no audio output device available"),
        }
    }
}

impl std::error::Error for Error {}

impl From<AuthError> for Error {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::NotLoggedIn => Error::NotLoggedIn,
            other => Error::Auth(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Never connected.
    None,
    /// Connected once and since invalidated (network drop, Spotify closed it).
    Invalid,
    Valid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// The audio output could not be created. Retrying with a new token cannot help.
    NoAudio,
    /// Login or network failure.
    Failed,
}

/// What the host needs from the librespot layer (faked in tests).
pub trait PlayerBackend: Send + Sync {
    fn session_state(&self) -> SessionState;
    /// Open a new session with `access` and attach it to the player (creating the player on first
    /// use, `set_session` afterwards, so the PCM tap stays valid across reconnects).
    fn connect<'a>(&'a self, access: &'a Secret) -> BoxFuture<'a, Result<(), ConnectError>>;
    /// `uri` is already validated as `spotify:track:<id>`.
    fn load(&self, uri: &str, play: bool, position_ms: u32) -> Result<(), Error>;
    fn play(&self);
    fn pause(&self);
    fn stop(&self);
    fn seek(&self, position_ms: u32);
    /// Queue `uri` (validated) to follow the current track gaplessly. Only acts on an existing
    /// player with a session; never connects.
    fn preload(&self, uri: &str) -> Result<(), Error>;
    /// End the account's session: stop the player and shut the session down, so the state is
    /// [`SessionState::None`] and nothing plays through it again. The player itself (and its
    /// sink on the engine's source) stays for the next session. Never connects.
    fn disconnect(&self);
}

pub struct PlayerHost {
    tokens: PlaybackTokens,
    backend: Arc<dyn PlayerBackend>,
    events: broadcast::Sender<UiEvent>,
    /// Serialises (re)connects so concurrent commands open one session.
    connect_lock: tokio::sync::Mutex<()>,
}

impl PlayerHost {
    /// Build the real host on the engine's source, the chosen output and the event sender.
    /// Nothing connects until the first command. The player is created once and kept across
    /// reconnects, so the sink (and with it the engine feed or the fallback's tap) stays valid.
    pub fn new(
        tokens: PlaybackTokens,
        source: SourceWriter,
        output: SpotifyOutput,
        events: broadcast::Sender<UiEvent>,
    ) -> PlayerHost {
        let backend = Arc::new(LibrespotBackend::new(source, output, events.clone()));
        Self::with_backend(tokens, backend, events)
    }

    /// Host over a custom backend (tests). `events` must be the sender the backend publishes to.
    pub fn with_backend(
        tokens: PlaybackTokens,
        backend: Arc<dyn PlayerBackend>,
        events: broadcast::Sender<UiEvent>,
    ) -> PlayerHost {
        PlayerHost {
            tokens,
            backend,
            events,
            connect_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Every call returns an independent receiver of all later events. A slow receiver lags
    /// (loses old events) instead of blocking the player.
    pub fn subscribe(&self) -> broadcast::Receiver<UiEvent> {
        self.events.subscribe()
    }

    fn emit(&self, e: UiEvent) {
        let _ = self.events.send(e);
    }

    /// Token -> session. On failure also publishes an `error` event so a UI that is only
    /// listening to events can leave its "waiting" state; the command still returns the error.
    async fn ensure_ready(&self) -> Result<(), Error> {
        if self.backend.session_state() == SessionState::Valid {
            return Ok(());
        }
        let _g = self.connect_lock.lock().await;
        let result = self.connect_locked().await;
        if let Err(e) = &result {
            self.emit(UiEvent::error(e.to_string()));
        }
        result
    }

    /// For commands that only make sense once a track has been loaded: `Ok(false)` when no session
    /// was ever opened (nothing to control, nothing is touched), otherwise as [`Self::ensure_ready`]
    /// (a session that died is replaced).
    async fn ensure_existing_session(&self) -> Result<bool, Error> {
        if self.backend.session_state() == SessionState::None {
            return Ok(false);
        }
        self.ensure_ready().await.map(|()| true)
    }

    async fn connect_locked(&self) -> Result<(), Error> {
        match self.backend.session_state() {
            SessionState::Valid => return Ok(()),
            SessionState::Invalid => self.emit(UiEvent::SessionLost),
            SessionState::None => {}
        }
        let token = self.tokens.access_token().await?;
        match self.backend.connect(&token).await {
            Ok(()) => return Ok(()),
            Err(ConnectError::NoAudio) => return Err(Error::AudioUnavailable),
            Err(ConnectError::Failed) => {}
        }
        // One retry with a token that is certainly fresh.
        let token = self.tokens.force_refresh().await?;
        match self.backend.connect(&token).await {
            Ok(()) => Ok(()),
            Err(ConnectError::NoAudio) => Err(Error::AudioUnavailable),
            Err(ConnectError::Failed) => Err(Error::SessionUnavailable),
        }
    }

    /// Load a track (`spotify:track:<22 base62>`, or a bare id). An invalid URI is rejected
    /// before any token or network use.
    pub async fn load(&self, uri: &str, play: bool, position_ms: u32) -> Result<(), Error> {
        let uri = parse_track_uri(uri).ok_or(Error::InvalidUri)?;
        self.ensure_ready().await?;
        self.backend.load(&uri, play, position_ms)
    }

    /// Resume. No session yet means nothing is loaded: a no-op, never a connect.
    pub async fn play(&self) -> Result<(), Error> {
        if !self.ensure_existing_session().await? {
            return Ok(());
        }
        self.backend.play();
        Ok(())
    }

    /// Never connects: the player (if any) pauses, otherwise there is nothing to pause. A dead
    /// session still reaches its player so buffered audio stops.
    pub async fn pause(&self) -> Result<(), Error> {
        self.backend.pause();
        Ok(())
    }

    /// Never connects, as [`PlayerHost::pause`].
    pub async fn stop(&self) -> Result<(), Error> {
        self.backend.stop();
        Ok(())
    }

    /// As [`PlayerHost::play`]: a no-op without a session.
    pub async fn seek(&self, position_ms: u32) -> Result<(), Error> {
        if !self.ensure_existing_session().await? {
            return Ok(());
        }
        self.backend.seek(position_ms);
        Ok(())
    }

    /// The playback login ended or changed (logout, or a login that may be another account):
    /// stop and end the session, so the next command connects with the current login's token or
    /// fails `NotLoggedIn` (S1). Serialised with connects, so no command keeps the old session.
    pub async fn end_session(&self) {
        let _g = self.connect_lock.lock().await;
        self.backend.disconnect();
    }

    /// Prepare `uri` to follow the current track gaplessly. An invalid URI is rejected; without
    /// a player this does nothing: never a token, connect or session.
    pub async fn preload(&self, uri: &str) -> Result<(), Error> {
        let uri = parse_track_uri(uri).ok_or(Error::InvalidUri)?;
        self.backend.preload(&uri)
    }
}

/// A scripted [`PlayerBackend`] for the crate's tests: records calls and connect tokens, never
/// touches the network.
#[cfg(test)]
pub(crate) mod fake {
    use super::{BoxFuture, ConnectError, Error, PlayerBackend, Secret, SessionState};
    use std::{collections::VecDeque, sync::Mutex, time::Duration};

    #[derive(Default)]
    pub(crate) struct FakeBackend {
        pub(crate) state: Mutex<Option<SessionState>>,
        /// Access tokens `connect` was called with.
        pub(crate) connects: Mutex<Vec<String>>,
        pub(crate) results: Mutex<VecDeque<Result<(), ConnectError>>>,
        pub(crate) calls: Mutex<Vec<String>>,
    }

    impl FakeBackend {
        pub(crate) fn set_state(&self, s: SessionState) {
            *self.state.lock().unwrap() = Some(s);
        }
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        pub(crate) fn connects(&self) -> Vec<String> {
            self.connects.lock().unwrap().clone()
        }
    }

    impl PlayerBackend for FakeBackend {
        fn session_state(&self) -> SessionState {
            self.state.lock().unwrap().unwrap_or(SessionState::None)
        }
        fn connect<'a>(&'a self, access: &'a Secret) -> BoxFuture<'a, Result<(), ConnectError>> {
            Box::pin(async move {
                self.connects
                    .lock()
                    .unwrap()
                    .push(access.expose().to_owned());
                tokio::time::sleep(Duration::from_millis(20)).await;
                let r = self.results.lock().unwrap().pop_front().unwrap_or(Ok(()));
                if r.is_ok() {
                    self.set_state(SessionState::Valid);
                }
                r
            })
        }
        fn load(&self, uri: &str, play: bool, pos: u32) -> Result<(), Error> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("load {uri} {play} {pos}"));
            Ok(())
        }
        fn play(&self) {
            self.calls.lock().unwrap().push("play".into());
        }
        fn pause(&self) {
            self.calls.lock().unwrap().push("pause".into());
        }
        fn stop(&self) {
            self.calls.lock().unwrap().push("stop".into());
        }
        fn seek(&self, ms: u32) {
            self.calls.lock().unwrap().push(format!("seek {ms}"));
        }
        fn preload(&self, uri: &str) -> Result<(), Error> {
            self.calls.lock().unwrap().push(format!("preload {uri}"));
            Ok(())
        }
        fn disconnect(&self) {
            self.calls.lock().unwrap().push("disconnect".into());
            *self.state.lock().unwrap() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBackend;
    use super::*;
    use crate::{
        accounts,
        auth::Tokens,
        auth_manager::{AuthManager, SlotBackend},
        web_auth::TokenError,
    };
    use std::{
        sync::atomic::{AtomicUsize, Ordering::SeqCst},
        time::{Duration, Instant},
    };
    use whipspot_core::credential_store::CredentialStore;
    use whipspot_testkit::MemoryStore;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    /// Issues `<name>-access-<n>` tokens and counts calls.
    struct Tokener {
        name: &'static str,
        refreshes: AtomicUsize,
    }

    impl SlotBackend for Tokener {
        fn refresh<'a>(
            &'a self,
            _: Option<&'a str>,
            _: &'a Secret,
        ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
            Box::pin(async move {
                let n = self.refreshes.fetch_add(1, SeqCst) + 1;
                Ok(Tokens {
                    access: Secret::new(format!("{}-access-{n}", self.name)),
                    refresh: Secret::new(""),
                    expires_at: Instant::now() + Duration::from_secs(3600),
                    scopes: vec![],
                })
            })
        }
        /// A login (perhaps another account) issues `<name>-login-access`.
        fn login<'a>(&'a self, _: Option<&'a str>) -> BoxFuture<'a, Result<Tokens, AuthError>> {
            Box::pin(async move {
                Ok(Tokens {
                    access: Secret::new(format!("{}-login-access", self.name)),
                    refresh: Secret::new(format!("{}-login-refresh", self.name)),
                    expires_at: Instant::now() + Duration::from_secs(3600),
                    scopes: vec![],
                })
            })
        }
    }

    struct Rig {
        auth: AuthManager,
        host: Arc<PlayerHost>,
        backend: Arc<FakeBackend>,
        pb: Arc<Tokener>,
        web: Arc<Tokener>,
        events: broadcast::Receiver<UiEvent>,
    }

    fn rig(logged_in: bool) -> Rig {
        let store = Arc::new(MemoryStore::default());
        if logged_in {
            store
                .set(accounts::PLAYBACK_REFRESH, &Secret::new("pb-refresh"))
                .unwrap();
        }
        // The web login is permanently missing: nothing in the host may depend on it.
        let pb = Arc::new(Tokener {
            name: "pb",
            refreshes: AtomicUsize::new(0),
        });
        let web = Arc::new(Tokener {
            name: "web",
            refreshes: AtomicUsize::new(0),
        });
        let auth = AuthManager::with_backends(store, pb.clone(), web.clone());
        let backend = Arc::new(FakeBackend::default());
        let (tx, _) = broadcast::channel(64);
        let host = Arc::new(PlayerHost::with_backend(
            auth.playback(),
            backend.clone(),
            tx,
        ));
        let events = host.subscribe();
        Rig {
            auth,
            host,
            backend,
            pb,
            web,
            events,
        }
    }

    fn drain(rx: &mut broadcast::Receiver<UiEvent>) -> Vec<UiEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn invalid_uris_are_rejected_before_any_token_or_network_use() {
        let r = rig(true);
        let bad = [
            "",
            "spotify:track:",
            "spotify:track:short",
            "spotify:episode:4uLU6hMCjMI75M1A2tKUQC",
            "spotify:track:4uLU6hMCjMI75M1A2tKUQC:x",
            "spotify:track:4uLU6hMCjMI75M1A2tKUQC\n",
            " spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
            "spotify:track:../../etc/passwd/xxxxx",
        ];
        for b in bad {
            assert_eq!(
                r.host.load(b, true, 0).await,
                Err(Error::InvalidUri),
                "{b:?}"
            );
        }
        assert_eq!(r.pb.refreshes.load(SeqCst), 0);
        assert!(r.backend.connects().is_empty());
        assert!(r.backend.calls().is_empty());
    }

    #[tokio::test]
    async fn first_command_connects_once_then_reuses_the_session() {
        let r = rig(true);
        r.host.load(URI, true, 1500).await.unwrap();
        r.host.pause().await.unwrap();
        r.host.play().await.unwrap();
        r.host.seek(42_000).await.unwrap();
        r.host.stop().await.unwrap();
        assert_eq!(r.backend.connects(), ["pb-access-1"]);
        assert_eq!(
            r.backend.calls(),
            [
                format!("load {URI} true 1500"),
                "pause".into(),
                "play".into(),
                "seek 42000".into(),
                "stop".into()
            ]
        );
        // The host never asked the web login for anything.
        assert_eq!(r.web.refreshes.load(SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_commands_open_one_session() {
        let r = rig(true);
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let h = r.host.clone();
            tasks.push(tokio::spawn(async move { h.load(URI, true, 0).await }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert_eq!(r.backend.connects().len(), 1);
        assert_eq!(r.backend.calls().len(), 10);
    }

    #[tokio::test]
    async fn dead_session_is_announced_and_replaced() {
        let mut r = rig(true);
        r.host.load(URI, true, 0).await.unwrap();
        drain(&mut r.events);
        r.backend.set_state(SessionState::Invalid);
        r.host.play().await.unwrap();
        assert_eq!(drain(&mut r.events), [UiEvent::SessionLost]);
        assert_eq!(r.backend.connects().len(), 2);
    }

    #[tokio::test]
    async fn connect_failure_retries_once_with_a_fresh_token_then_gives_up() {
        let r = rig(true);
        r.backend
            .results
            .lock()
            .unwrap()
            .extend([Err(ConnectError::Failed), Ok(())]);
        r.host.load(URI, true, 0).await.unwrap();
        // First attempt used the first token, the retry a force-refreshed (new) one.
        assert_eq!(r.backend.connects(), ["pb-access-1", "pb-access-2"]);

        let mut r2 = rig(true);
        r2.backend.results.lock().unwrap().extend([
            Err(ConnectError::Failed),
            Err(ConnectError::Failed),
            Ok(()),
        ]);
        assert_eq!(
            r2.host.load(URI, true, 0).await,
            Err(Error::SessionUnavailable)
        );
        assert_eq!(r2.backend.connects().len(), 2, "exactly one retry");
        assert!(r2.backend.calls().is_empty());
        assert_eq!(
            drain(&mut r2.events),
            [UiEvent::error("could not start a Spotify session")]
        );
    }

    #[tokio::test]
    async fn missing_audio_device_is_not_retried() {
        let mut r = rig(true);
        r.backend
            .results
            .lock()
            .unwrap()
            .push_back(Err(ConnectError::NoAudio));
        assert_eq!(
            r.host.load(URI, true, 0).await,
            Err(Error::AudioUnavailable)
        );
        assert_eq!(r.backend.connects().len(), 1);
        assert_eq!(
            drain(&mut r.events),
            [UiEvent::error("no audio output device available")]
        );
    }

    #[tokio::test]
    async fn logged_out_load_fails_without_connecting_and_publishes_an_error() {
        let mut r = rig(false);
        assert_eq!(r.host.load(URI, true, 0).await, Err(Error::NotLoggedIn));
        assert!(r.backend.connects().is_empty());
        assert_eq!(
            drain(&mut r.events),
            [UiEvent::error("not logged in to Spotify for playback")]
        );
    }

    /// Transport buttons pressed before any session exists must not open one: no credential store read
    /// (a logged-in and a logged-out rig both), no token refresh, no connect (= no audio device),
    /// no event.
    #[tokio::test]
    async fn transport_commands_without_a_session_never_connect() {
        for logged_in in [true, false] {
            let mut r = rig(logged_in);
            assert_eq!(r.backend.session_state(), SessionState::None);
            r.host.play().await.unwrap();
            r.host.pause().await.unwrap();
            r.host.stop().await.unwrap();
            r.host.seek(5_000).await.unwrap();
            r.host.preload(URI).await.unwrap();
            assert_eq!(
                r.host.preload("spotify:album:x").await,
                Err(Error::InvalidUri)
            );
            assert!(r.backend.connects().is_empty(), "logged_in={logged_in}");
            assert_eq!(r.pb.refreshes.load(SeqCst), 0, "logged_in={logged_in}");
            assert_eq!(r.web.refreshes.load(SeqCst), 0);
            assert_eq!(r.backend.session_state(), SessionState::None);
            assert!(drain(&mut r.events).is_empty(), "no error notice either");
            // play/seek have nothing to act on; pause/stop/preload reach the (absent) player,
            // which no-ops.
            assert_eq!(
                r.backend.calls(),
                ["pause".to_owned(), "stop".into(), format!("preload {URI}")]
            );
        }
    }

    #[tokio::test]
    async fn dead_session_still_receives_pause_and_stop_but_is_not_reconnected_for_them() {
        let mut r = rig(true);
        r.host.load(URI, true, 0).await.unwrap();
        drain(&mut r.events);
        r.backend.set_state(SessionState::Invalid);
        r.host.pause().await.unwrap();
        r.host.stop().await.unwrap();
        assert_eq!(r.backend.connects().len(), 1);
        assert!(drain(&mut r.events).is_empty());
        assert_eq!(
            r.backend.calls(),
            [format!("load {URI} true 0"), "pause".into(), "stop".into()]
        );
    }

    // S1 (M10 security review): a playback logout ends the session; nothing plays through it
    // and nothing reconnects without a login. A new login (perhaps another account) connects
    // once, with its own token.
    #[tokio::test]
    async fn logout_ends_the_session_and_a_new_login_connects_with_its_own_token() {
        let r = rig(true);
        r.host.load(URI, true, 0).await.unwrap();
        r.auth.logout_playback().await.unwrap();
        r.host.end_session().await;
        assert_eq!(r.backend.session_state(), SessionState::None);
        assert_eq!(r.host.load(URI, true, 0).await, Err(Error::NotLoggedIn));
        r.host.play().await.unwrap();
        r.host.seek(1_000).await.unwrap();
        assert_eq!(r.backend.connects(), ["pb-access-1"]);
        assert_eq!(
            r.backend.calls(),
            [format!("load {URI} true 0"), "disconnect".into()]
        );
        r.auth.login_playback().await.unwrap();
        r.host.end_session().await;
        r.host.load(URI, true, 0).await.unwrap();
        assert_eq!(r.backend.connects(), ["pb-access-1", "pb-login-access"]);
        assert_eq!(
            r.pb.refreshes.load(SeqCst),
            1,
            "the login's token, not a refresh"
        );
    }

    #[tokio::test]
    async fn every_subscriber_gets_each_event_once() {
        let r = rig(true);
        // Two subscriptions each see an event exactly once.
        let mut s1 = r.host.subscribe();
        let mut s2 = r.host.subscribe();
        r.host.emit(UiEvent::error("x"));
        assert_eq!(drain(&mut s1), [UiEvent::error("x")]);
        assert_eq!(drain(&mut s2), [UiEvent::error("x")]);
    }

    #[test]
    fn error_texts_are_fixed() {
        let e = Error::Auth(AuthError::RateLimited {
            retry_after_secs: Some(30),
        });
        assert_eq!(e.to_string(), "Spotify is rate-limiting login requests");
    }
}
