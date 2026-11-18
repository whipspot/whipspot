//! [`SpotifyProvider`]: the neutral [`Provider`] over the existing `AuthManager`, `PlayerHost`
//! and `WebApi`. All validation, retry, rate-gating and credential store logic stays in those types;
//! the trait impls (in `auth_flow.rs`, `playback.rs`, `library.rs`) only delegate and map errors to
//! fixed whitelisted texts.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::{
    auth_manager::{AuthManager, SlotBackend},
    connect::{
        ConnectHost, ConnectOptions, HostCfg,
        backend::{ConnectBackend, LibrespotConnect},
        mixer::{ConnectMixer, to_level},
    },
    player_host::PlayerHost,
    session::LibrespotBackend,
    watch::{LibraryWatch, TIMER_PERIOD},
    web_auth,
    webapi::WebApi,
};
use whipspot_core::{
    credential_store::CredentialStore,
    engine::SourceWriter,
    events::UiEvent,
    provider::{
        AuthFlow, AuthStepInfo, Capabilities, InputSpec, Library, Playback, Provider,
        ProviderContext, ProviderInfo, Remote, StepHint,
    },
};

pub const ID: &str = "spotify";
pub const STEP_PLAYBACK: &str = "playback";
pub const STEP_WEB: &str = "web";
/// The Connect speaker's on/off step (D7); present only on a provider built `with_connect`.
pub const STEP_CONNECT: &str = "connect";
pub const INPUT_CLIENT_ID: &str = "clientId";
/// The hosts Spotify's image URLs use (A1); an art source fetches only from these and their
/// subdomains.
pub(crate) const IMAGE_HOSTS: &[&str] = &["scdn.co", "spotifycdn.com"];

/// Where Spotify's audio goes (D37). The core engine is the only output since the rodio fallback
/// was deleted (MD116); the enum stays so the constructors keep their shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SpotifyOutput {
    /// The core engine (`CoreSink`), the default.
    #[default]
    Core,
}

/// The streaming bitrate cap the user allows (MD88). The composition root maps the UI's choice to
/// one of these; the next player start uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Up to 96 kbps.
    Kbps96,
    /// Up to 160 kbps.
    Kbps160,
    /// Up to 320 kbps, the default.
    #[default]
    Kbps320,
}

impl Quality {
    /// The cap in kbps.
    pub(crate) fn kbps(self) -> u32 {
        match self {
            Quality::Kbps96 => 96,
            Quality::Kbps160 => 160,
            Quality::Kbps320 => 320,
        }
    }
}

pub struct SpotifyProvider {
    pub(super) auth: AuthManager,
    pub(super) host: Arc<PlayerHost>,
    /// Only ever holds the web login's token.
    pub(super) api: Arc<WebApi>,
    /// Notices a library changed elsewhere (D12); sends `LibraryChanged`.
    pub(super) watch: Arc<LibraryWatch>,
    /// What `host` is built on, kept so [`SpotifyProvider::with_quality`] can rebuild it.
    source: SourceWriter,
    events: broadcast::Sender<UiEvent>,
    output: SpotifyOutput,
    /// The bitrate cap the next player start uses (MD88).
    pub(super) quality: Quality,
    /// The Connect speaker, when built [`SpotifyProvider::with_connect`].
    pub(super) connect: Option<Arc<ConnectHost>>,
}

impl SpotifyProvider {
    /// The real thing on the core output. Touches neither the network nor the credential store.
    pub fn new(store: Arc<dyn CredentialStore>, ctx: ProviderContext) -> Self {
        Self::new_with_output(store, ctx, SpotifyOutput::Core)
    }

    /// The real thing on the chosen output.
    pub fn new_with_output(
        store: Arc<dyn CredentialStore>,
        ctx: ProviderContext,
        output: SpotifyOutput,
    ) -> Self {
        Self::with_auth(AuthManager::new(store), ctx, output)
    }

    /// Custom network backends for the two logins (tests).
    pub fn with_backends(
        store: Arc<dyn CredentialStore>,
        playback: Arc<dyn SlotBackend>,
        web: Arc<dyn SlotBackend>,
        ctx: ProviderContext,
    ) -> Self {
        Self::with_auth(
            AuthManager::with_backends(store, playback, web),
            ctx,
            SpotifyOutput::Core,
        )
    }

    fn with_auth(auth: AuthManager, ctx: ProviderContext, output: SpotifyOutput) -> Self {
        // The copied pipeline (`ctx.audio`) is never used: Spotify feeds the engine's source.
        let ProviderContext { source, events, .. } = ctx;
        let quality = Quality::default();
        let host = Arc::new(host_for(&auth, &source, &events, output, quality));
        let api = Arc::new(WebApi::new(auth.web()));
        let watch = LibraryWatch::new(api.clone(), events.clone());
        SpotifyProvider {
            auth,
            host,
            api,
            watch,
            source,
            events,
            output,
            quality,
            connect: None,
        }
    }

    /// The bitrate cap of the next player start (MD88). Rebuilds the host, which connects only on
    /// its first command: call it before the provider is shared.
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self.host = Arc::new(host_for(
            &self.auth,
            &self.source,
            &self.events,
            self.output,
            quality,
        ));
        if let Some(c) = &self.connect {
            c.set_quality(quality);
        }
        self
    }

    /// Look for library changes made elsewhere every 30 minutes, beginning now (D12, D13); the
    /// focus check and the Connect session's hints work without it. Spawns the timer: call
    /// inside a tokio runtime.
    pub fn with_library_watch(self) -> Self {
        self.watch.spawn_timer(TIMER_PERIOD);
        self
    }

    /// Make Whipspot a Spotify Connect speaker (D1): its own librespot session, player and
    /// Spirc, writing into the engine through `opts.context` (built with
    /// `ctx.for_owner(connect::OWNER)`), at this provider's quality. It runs while
    /// `opts.enabled` (the `connect` step) and a Playback login is stored. Spawns its
    /// supervisor: call inside a tokio runtime.
    pub fn with_connect(self, opts: ConnectOptions) -> Self {
        let mixer = Arc::new(ConnectMixer::new(
            to_level(opts.volume),
            opts.context.events.clone(),
        ));
        let watch = self.watch.clone();
        let backend = Arc::new(LibrespotConnect::new(
            opts.context.source.clone(),
            opts.context.events.clone(),
            mixer.clone(),
            Arc::new(move || watch.hint()),
        ));
        self.with_connect_backend(opts, backend, mixer)
    }

    /// [`SpotifyProvider::with_connect`] over a given backend (tests).
    pub(crate) fn with_connect_backend(
        mut self,
        opts: ConnectOptions,
        backend: Arc<dyn ConnectBackend>,
        mixer: Arc<ConnectMixer>,
    ) -> Self {
        let host = ConnectHost::spawn(
            self.auth.playback(),
            backend,
            mixer,
            opts.context.events,
            HostCfg {
                enabled: opts.enabled,
                name: opts.name,
                device_id: opts.device_id,
                quality: self.quality,
            },
        );
        self.connect = Some(host);
        self
    }

    /// End the Connect speaker for good (app exit): the controlling app sees it go inactive and
    /// its session ends. Does nothing without Connect. Bounded by the shutdown wait (about 3 s,
    /// longer only while a start is in flight).
    pub async fn shutdown_connect(&self) {
        if let Some(c) = &self.connect {
            c.shutdown().await;
        }
    }
}

#[cfg(test)]
impl SpotifyProvider {
    /// The provider over a scripted player backend (tests).
    pub(crate) fn with_player_backend(
        mut self,
        backend: Arc<dyn crate::player_host::PlayerBackend>,
    ) -> Self {
        self.host = Arc::new(PlayerHost::with_backend(
            self.auth.playback(),
            backend,
            self.events.clone(),
        ));
        self
    }
}

/// The host over the engine's source on `output`, whose player starts at `quality`. The same host
/// as `PlayerHost::new` builds, with the quality handed to the session backend.
fn host_for(
    auth: &AuthManager,
    source: &SourceWriter,
    events: &broadcast::Sender<UiEvent>,
    output: SpotifyOutput,
    quality: Quality,
) -> PlayerHost {
    let backend =
        LibrespotBackend::new(source.clone(), output, events.clone()).with_quality(quality);
    PlayerHost::with_backend(auth.playback(), Arc::new(backend), events.clone())
}

impl Provider for SpotifyProvider {
    fn info(&self) -> ProviderInfo {
        let mut auth_steps = vec![
            AuthStepInfo {
                id: STEP_PLAYBACK,
                label: "Playback",
                inputs: vec![],
                hint: Some(StepHint {
                    text: "Playing music needs a Spotify Premium login.",
                    copy: None,
                }),
                switch: false,
            },
            AuthStepInfo {
                id: STEP_WEB,
                label: "Library",
                inputs: vec![InputSpec {
                    name: INPUT_CLIENT_ID,
                    label: "Client ID",
                    pattern: "^[0-9a-fA-F]{32}$",
                    format: "32 hex characters",
                    max_len: 32,
                }],
                hint: Some(StepHint {
                    text: "Browsing your playlists needs your own Spotify app. Create one in the Spotify \
                           developer dashboard (the app owner needs Premium) and add this redirect URI:",
                    copy: Some(web_auth::consts::REDIRECT_URI),
                }),
                switch: false,
            },
        ];
        if self.connect.is_some() {
            auth_steps.push(AuthStepInfo {
                id: STEP_CONNECT,
                label: "Connect speaker",
                inputs: vec![],
                hint: None,
                switch: true,
            });
        }
        ProviderInfo {
            id: ID,
            name: "Spotify",
            glyph: whipspot_core::provider::ProviderGlyph::Stream,
            caps: Capabilities {
                library: true,
                pcm_tap: true,
                eq: true,
                search: true,
                likes: true,
                explore: true,
                save_albums: true,
            },
            auth_steps,
        }
    }

    fn auth(&self) -> &dyn AuthFlow {
        self
    }

    fn playback(&self) -> &dyn Playback {
        self
    }

    fn library(&self) -> Option<&dyn Library> {
        Some(self)
    }

    fn image_hosts(&self) -> &'static [&'static str] {
        IMAGE_HOSTS
    }

    fn remote(&self) -> Option<&dyn Remote> {
        self.connect.as_deref().map(|c| c as &dyn Remote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
        },
        time::{Duration, Instant},
    };

    use tokio::sync::{Notify, broadcast};

    use crate::{
        accounts,
        auth::{Secret, Tokens},
        auth_manager::AuthError,
        web_auth::TokenError,
        webapi,
    };
    use whipspot_core::credential_store::StoreError;
    use whipspot_core::provider::{
        AuthFailure, LibErrorKind, LibraryError, LoginInput, model::LoginState,
    };
    use whipspot_testkit::MemoryStore;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct Counting {
        refreshes: AtomicUsize,
        logins: AtomicUsize,
        /// The next login's failure (one login), as the browser step would report it.
        login_error: Mutex<Option<AuthError>>,
        /// When set, a login says it entered (the first `Notify`) and waits in the browser step
        /// until released (the second).
        gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    }

    fn tokens(access: &str) -> Tokens {
        Tokens {
            access: Secret::new(access),
            refresh: Secret::new("refresh-from-backend"),
            expires_at: Instant::now() + Duration::from_secs(3600),
            scopes: vec![],
        }
    }

    impl SlotBackend for Counting {
        fn refresh<'a>(
            &'a self,
            _: Option<&'a str>,
            _: &'a Secret,
        ) -> whipspot_core::provider::BoxFuture<'a, Result<Tokens, TokenError>> {
            Box::pin(async move {
                self.refreshes.fetch_add(1, SeqCst);
                Ok(tokens("access-from-refresh"))
            })
        }
        fn login<'a>(
            &'a self,
            _: Option<&'a str>,
        ) -> whipspot_core::provider::BoxFuture<'a, Result<Tokens, AuthError>> {
            Box::pin(async move {
                self.logins.fetch_add(1, SeqCst);
                let gate = self.gate.lock().unwrap().clone();
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                if let Some(e) = self.login_error.lock().unwrap().take() {
                    return Err(e);
                }
                Ok(tokens("access-from-login"))
            })
        }
    }

    /// A [`MemoryStore`] whose writes can be made to fail while reads and deletes still work
    /// (a Keychain or Credential Manager write refused).
    #[derive(Default)]
    struct FailingWrites {
        inner: MemoryStore,
        fail: AtomicBool,
    }

    impl CredentialStore for FailingWrites {
        fn get(&self, account: &str) -> Result<Option<Secret>, StoreError> {
            self.inner.get(account)
        }
        fn set(&self, account: &str, value: &Secret) -> Result<(), StoreError> {
            if self.fail.load(SeqCst) {
                return Err(StoreError);
            }
            self.inner.set(account, value)
        }
        fn delete(&self, account: &str) -> Result<bool, StoreError> {
            self.inner.delete(account)
        }
    }

    struct Rig {
        p: SpotifyProvider,
        pb: Arc<Counting>,
        web: Arc<Counting>,
        store: Arc<MemoryStore>,
    }

    fn rig() -> Rig {
        let store = Arc::new(MemoryStore::default());
        let pb = Arc::new(Counting::default());
        let web = Arc::new(Counting::default());
        let (events, _) = broadcast::channel(8);
        let source = whipspot_core::engine::Engine::new(Default::default(), None).source;
        let p = SpotifyProvider::with_backends(
            store.clone(),
            pb.clone(),
            web.clone(),
            ProviderContext { source, events },
        );
        Rig { p, pb, web, store }
    }

    fn input(pairs: &[(&str, &str)]) -> LoginInput {
        LoginInput(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn source_label_states_the_cap_the_provider_was_given() {
        assert_eq!(
            rig().p.playback().source_format().as_deref(),
            Some("Lossy (Ogg Vorbis or MP3), up to 320 kbps")
        );
        assert_eq!(
            rig()
                .p
                .with_quality(Quality::Kbps160)
                .playback()
                .source_format()
                .as_deref(),
            Some("Lossy (Ogg Vorbis or MP3), up to 160 kbps")
        );
    }

    #[test]
    fn info_declares_the_spotify_shape() {
        let json = serde_json::to_string(&rig().p.info()).unwrap();
        assert_eq!(
            json,
            r#"{"id":"spotify","name":"Spotify","glyph":"stream","caps":{"library":true,"pcmTap":true,"eq":true,"search":true,"likes":true,"explore":true,"saveAlbums":true},"authSteps":[{"id":"playback","label":"Playback","inputs":[],"hint":{"text":"Playing music needs a Spotify Premium login."}},{"id":"web","label":"Library","inputs":[{"name":"clientId","label":"Client ID","pattern":"^[0-9a-fA-F]{32}$","format":"32 hex characters","maxLen":32}],"hint":{"text":"Browsing your playlists needs your own Spotify app. Create one in the Spotify developer dashboard (the app owner needs Premium) and add this redirect URI:","copy":"http://127.0.0.1:8899/callback"}}]}"#
        );
        assert!(rig().p.library().is_some());
        assert_eq!(rig().p.image_hosts(), ["scdn.co", "spotifycdn.com"]);
    }

    #[test]
    fn every_web_api_error_keeps_its_wire_json() {
        use webapi::Error as E;
        let all = [
            E::RateLimited {
                retry_after_secs: 30,
            },
            E::WebNotLoggedIn,
            E::NotReadable,
            E::AccessDenied,
            E::InvalidId,
            E::InvalidOffset,
            E::InvalidQuery,
            E::UnexpectedShape,
            E::Http(503),
            E::Network,
            E::CredentialStore,
            E::AuthFailed,
        ];
        for e in all {
            let old = serde_json::to_string(&e).unwrap();
            let new = serde_json::to_string(&LibraryError::from(e.clone())).unwrap();
            assert_eq!(new, old, "{e:?}");
        }
        let l = LibraryError::from(E::RateLimited {
            retry_after_secs: 30,
        });
        assert_eq!(l.kind, LibErrorKind::RateLimited);
        assert_eq!(l.retry_after_secs, Some(30));
        assert_eq!(
            serde_json::to_string(&LibraryError::from(E::InvalidQuery)).unwrap(),
            r#"{"kind":"invalidArgument","message":"not a valid search text","retryAfterSecs":null}"#
        );
    }

    #[tokio::test]
    async fn login_web_rejects_bad_client_ids_before_any_work() {
        let r = rig();
        let bad = [
            "",
            "0123456789abcdef0123456789abcde",   // 31
            "0123456789abcdef0123456789abcdef2", // 33
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            " 0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef\n",
        ];
        for id in bad {
            assert_eq!(
                r.p.login(STEP_WEB, &input(&[(INPUT_CLIENT_ID, id)]))
                    .await
                    .unwrap_err(),
                AuthFailure("client id must be 32 hex characters".into()),
                "{id:?}"
            );
        }
        // A missing id fails the same way.
        assert_eq!(
            r.p.login(STEP_WEB, &LoginInput::default())
                .await
                .unwrap_err(),
            AuthFailure("client id must be 32 hex characters".into())
        );
        assert_eq!(r.web.logins.load(SeqCst) + r.pb.logins.load(SeqCst), 0);
        assert_eq!(r.store.peek(accounts::WEB_CLIENT_ID), None);
        // A valid id goes through; the status shows web logged in and the id is prefillable.
        let ok = r.p.login(STEP_WEB, &input(&[(INPUT_CLIENT_ID, ID)])).await;
        assert_eq!(r.web.logins.load(SeqCst), 1);
        let ok = ok.unwrap();
        assert_eq!(ok.steps[1].state, LoginState::LoggedIn);
        assert_eq!(
            r.p.prefill(STEP_WEB, INPUT_CLIENT_ID)
                .await
                .unwrap()
                .as_deref(),
            Some(ID)
        );
    }

    #[tokio::test]
    async fn unknown_steps_and_inputs_fail_closed() {
        let r = rig();
        for step in ["", "Web", "web ", "spotify", "playback\n"] {
            let e = AuthFailure("unknown login step".into());
            assert_eq!(
                r.p.login(step, &LoginInput::default()).await.unwrap_err(),
                e
            );
            assert_eq!(r.p.logout(step).await.unwrap_err(), e);
        }
        assert_eq!(
            r.p.prefill("playback", INPUT_CLIENT_ID).await.unwrap_err(),
            AuthFailure("unknown login step".into())
        );
        assert_eq!(
            r.p.prefill(STEP_WEB, "other").await.unwrap_err(),
            AuthFailure("unknown login step".into())
        );
        let unexpected = AuthFailure("unexpected login input".into());
        assert_eq!(
            r.p.login(STEP_PLAYBACK, &input(&[("x", "y")]))
                .await
                .unwrap_err(),
            unexpected
        );
        assert_eq!(
            r.p.login(STEP_WEB, &input(&[(INPUT_CLIENT_ID, ID), ("extra", "1")]))
                .await
                .unwrap_err(),
            unexpected
        );
        assert_eq!(r.web.logins.load(SeqCst) + r.pb.logins.load(SeqCst), 0);
        assert_eq!(r.store.peek(accounts::WEB_CLIENT_ID), None);
    }

    #[tokio::test]
    async fn status_output_has_no_secret() {
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("AQDsupersecret"))
            .unwrap();
        r.store
            .set(accounts::WEB_REFRESH, &Secret::new("AQDothersecret"))
            .unwrap();
        r.store
            .set(accounts::WEB_CLIENT_ID, &Secret::new(ID))
            .unwrap();
        let new = serde_json::to_string(&r.p.status().await.unwrap()).unwrap();
        for leak in ["secret", "AQD", ID, "Bearer"] {
            assert!(!new.contains(leak), "{leak} in {new}");
        }
        assert_eq!(
            new,
            r#"{"steps":[{"id":"playback","state":"loggedIn"},{"id":"web","state":"loggedIn"}]}"#
        );
    }

    // S1 (M10 security review): the Playback card's logout and login end the session the player
    // used; after the logout a load fails without connecting, after the login it connects with
    // the login's token.
    #[tokio::test]
    async fn playback_logout_and_login_end_the_session() {
        const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let backend = Arc::new(crate::player_host::fake::FakeBackend::default());
        let p = r.p.with_player_backend(backend.clone());
        p.playback().load(TRACK, true, 0).await.unwrap();
        p.auth().logout(STEP_PLAYBACK).await.unwrap();
        assert_eq!(
            p.playback().load(TRACK, true, 0).await.unwrap_err().0,
            "not logged in to Spotify for playback"
        );
        p.auth().login(STEP_PLAYBACK, &input(&[])).await.unwrap();
        p.playback().load(TRACK, true, 0).await.unwrap();
        assert_eq!(
            backend.connects(),
            ["access-from-refresh", "access-from-login"]
        );
        assert_eq!(
            backend.calls(),
            [
                format!("load {TRACK} true 0"),
                "disconnect".into(),
                "disconnect".into(),
                format!("load {TRACK} true 0"),
            ]
        );
    }

    // S3 (M10 security review, round 3): a Playback login whose credential-store write fails
    // deletes the stored login, and it ends the session the player used: nothing keeps playing
    // on the previous account, the next load fails without connecting.
    #[tokio::test]
    async fn a_playback_login_whose_store_write_fails_ends_the_session() {
        const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        let store = Arc::new(FailingWrites::default());
        store
            .inner
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let (events, _) = broadcast::channel(8);
        let source = whipspot_core::engine::Engine::new(Default::default(), None).source;
        let backend = Arc::new(crate::player_host::fake::FakeBackend::default());
        let p = SpotifyProvider::with_backends(
            store.clone(),
            Arc::new(Counting::default()),
            Arc::new(Counting::default()),
            ProviderContext { source, events },
        )
        .with_player_backend(backend.clone());
        p.playback().load(TRACK, true, 0).await.unwrap();
        store.fail.store(true, SeqCst);
        assert_eq!(
            p.auth()
                .login(STEP_PLAYBACK, &input(&[]))
                .await
                .unwrap_err()
                .0,
            "credential store access failed"
        );
        store.fail.store(false, SeqCst);
        assert_eq!(store.inner.peek(accounts::PLAYBACK_REFRESH), None);
        assert_eq!(
            p.playback().load(TRACK, true, 0).await.unwrap_err().0,
            "not logged in to Spotify for playback"
        );
        assert_eq!(backend.connects(), ["access-from-refresh"]);
        assert_eq!(
            backend.calls(),
            [format!("load {TRACK} true 0"), "disconnect".into()]
        );
    }

    // F9: a Playback login that changed nothing (not completed in the browser, timed out, the
    // login port in use, failed before anything was stored) leaves the session alone: it still
    // runs on the login that is stored, and each next load uses it without connecting again.
    #[tokio::test]
    async fn a_playback_login_that_changed_nothing_leaves_the_session_alone() {
        const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let backend = Arc::new(crate::player_host::fake::FakeBackend::default());
        let p = r.p.with_player_backend(backend.clone());
        p.playback().load(TRACK, true, 0).await.unwrap();
        // The stored login as the refresh for that load left it.
        let kept = r.store.peek(accounts::PLAYBACK_REFRESH);
        assert!(kept.is_some());
        let failures = [
            (AuthError::LoginDenied, "login was not completed"),
            (AuthError::LoginTimedOut, "login timed out"),
            (AuthError::PortInUse, "the login port is already in use"),
            (AuthError::LoginFailed, "login failed"),
        ];
        let n = failures.len();
        for (e, text) in failures {
            *r.pb.login_error.lock().unwrap() = Some(e);
            assert_eq!(
                p.auth()
                    .login(STEP_PLAYBACK, &input(&[]))
                    .await
                    .unwrap_err()
                    .0,
                text
            );
            assert_eq!(r.store.peek(accounts::PLAYBACK_REFRESH), kept, "{text}");
            p.playback().load(TRACK, true, 0).await.unwrap();
            assert_eq!(backend.connects(), ["access-from-refresh"], "{text}");
        }
        assert_eq!(r.pb.logins.load(SeqCst), n);
        assert_eq!(backend.calls(), vec![format!("load {TRACK} true 0"); n + 1]);
    }

    // F9: a Playback login refused because a Library login holds the browser (each card's Log
    // in has its own pending state, so the Playback one stays clickable) attempts nothing and
    // leaves the session alone; the Library login, when it completes, leaves it alone too.
    #[tokio::test]
    async fn a_playback_login_refused_while_another_runs_leaves_the_session_alone() {
        const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *r.web.gate.lock().unwrap() = Some((entered.clone(), release.clone()));
        let backend = Arc::new(crate::player_host::fake::FakeBackend::default());
        let p = r.p.with_player_backend(backend.clone());
        p.playback().load(TRACK, true, 0).await.unwrap();
        let kept = r.store.peek(accounts::PLAYBACK_REFRESH);
        let web_input = input(&[(INPUT_CLIENT_ID, ID)]);
        let library = p.auth().login(STEP_WEB, &web_input);
        let playback = async {
            // The Library login is in the browser step: this one is refused at once.
            entered.notified().await;
            let refused = p
                .auth()
                .login(STEP_PLAYBACK, &input(&[]))
                .await
                .unwrap_err()
                .0;
            p.playback().load(TRACK, true, 0).await.unwrap();
            let seen = (backend.connects(), backend.calls());
            release.notify_one();
            (refused, seen)
        };
        let (library, (refused, (connects, calls))) = tokio::join!(library, playback);
        assert_eq!(refused, "another login is already in progress");
        assert_eq!(r.pb.logins.load(SeqCst), 0);
        assert_eq!(connects, ["access-from-refresh"]);
        assert_eq!(calls, vec![format!("load {TRACK} true 0"); 2]);
        assert_eq!(library.unwrap().steps[1].state, LoginState::LoggedIn);
        assert_eq!(r.web.logins.load(SeqCst), 1);
        assert_eq!(r.store.peek(accounts::PLAYBACK_REFRESH), kept);
        p.playback().load(TRACK, true, 0).await.unwrap();
        assert_eq!(backend.connects(), ["access-from-refresh"]);
        assert_eq!(backend.calls(), vec![format!("load {TRACK} true 0"); 3]);
    }

    // S3's class on the logout side: a Playback logout whose store delete fails still ends the
    // session (S1 made it so; this keeps it).
    #[tokio::test]
    async fn a_playback_logout_whose_store_delete_fails_still_ends_the_session() {
        const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let backend = Arc::new(crate::player_host::fake::FakeBackend::default());
        let p = r.p.with_player_backend(backend.clone());
        p.playback().load(TRACK, true, 0).await.unwrap();
        r.store.set_failing(true);
        assert_eq!(
            p.auth().logout(STEP_PLAYBACK).await.unwrap_err().0,
            "credential store access failed"
        );
        r.store.set_failing(false);
        assert_eq!(
            backend.calls(),
            [format!("load {TRACK} true 0"), "disconnect".into()]
        );
    }

    #[tokio::test]
    async fn logout_clears_the_step() {
        let r = rig();
        r.store
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
        let s = r.p.logout(STEP_PLAYBACK).await.unwrap();
        assert_eq!(s.steps[0].state, LoginState::LoggedOut);
        assert_eq!(r.store.peek(accounts::PLAYBACK_REFRESH), None);
    }

    #[tokio::test]
    async fn load_error_texts_are_fixed_and_validated_first() {
        let r = rig();
        // Logged out: the fixed text, no network.
        let e =
            r.p.load("spotify:track:4uLU6hMCjMI75M1A2tKUQC", true, 0)
                .await
                .unwrap_err();
        assert_eq!(e.to_string(), "not logged in to Spotify for playback");
        for bad in [
            "",
            "spotify:track:short",
            "spotify:episode:4uLU6hMCjMI75M1A2tKUQC",
        ] {
            assert_eq!(
                r.p.load(bad, true, 0).await.unwrap_err().to_string(),
                "not a Spotify track",
                "{bad:?}"
            );
        }
        assert_eq!(r.pb.refreshes.load(SeqCst) + r.pb.logins.load(SeqCst), 0);
        // No session ever opened: transport calls are no-ops, never a connect.
        r.p.play().await.unwrap();
        r.p.pause().await.unwrap();
        r.p.stop().await.unwrap();
        r.p.seek(1000).await.unwrap();
    }

    #[tokio::test]
    async fn library_arguments_are_validated_before_any_network_or_token_work() {
        let r = rig();
        let lib = r.p.library().unwrap();
        let bad = |e: LibraryError| {
            assert_eq!(e.kind, LibErrorKind::InvalidArgument, "{e}");
        };
        for id in [
            "",
            "short",
            "../x",
            "a/b",
            "%2e%2e",
            "1QkGCYuU2hrwb32Ku0nXYg\n",
        ] {
            bad(lib.playlist_items(id, 0).await.unwrap_err());
            bad(lib.album_tracks(id, 0).await.unwrap_err());
        }
        let id = "1QkGCYuU2hrwb32Ku0nXYg";
        bad(lib.playlist_items(id, u32::MAX).await.unwrap_err());
        bad(lib.album_tracks(id, 2000).await.unwrap_err());
        bad(lib.liked_tracks(2000).await.unwrap_err());
        bad(lib.playlists(2000).await.unwrap_err());
        bad(lib.albums(2000).await.unwrap_err());
        for q in ["", "   ", "a\nb", &"x".repeat(201)] {
            bad(lib.search(q, 0).await.unwrap_err());
        }
        bad(lib.search("ok", 2000).await.unwrap_err());
        assert_eq!(r.web.refreshes.load(SeqCst) + r.web.logins.load(SeqCst), 0);
        // A valid request with no web login reaches the token step and reports the fixed error.
        let e = lib.playlists(0).await.unwrap_err();
        assert_eq!(e.kind, LibErrorKind::WebNotLoggedIn);
        assert_eq!(r.web.refreshes.load(SeqCst), 0);
    }
    /// A library watch over a scripted fingerprint source, wired to the provider's own events.
    struct Scripted(Mutex<Vec<crate::webapi::Fingerprint>>, AtomicUsize);

    impl crate::watch::FingerprintSource for Scripted {
        fn fingerprint(
            &self,
        ) -> whipspot_core::provider::BoxFuture<'_, Result<crate::webapi::Fingerprint, webapi::Error>>
        {
            Box::pin(async move {
                self.1.fetch_add(1, SeqCst);
                let mut v = self.0.lock().unwrap();
                Ok(if v.len() > 1 {
                    v.remove(0)
                } else {
                    v[0].clone()
                })
            })
        }
    }

    fn stamp(total: u32) -> crate::webapi::Fingerprint {
        crate::webapi::Fingerprint {
            playlists: vec![],
            liked_total: total,
            liked_newest: None,
        }
    }

    #[tokio::test]
    async fn check_changes_announces_a_changed_library_once_and_a_library_login_change_resets() {
        let mut r = rig();
        let (events, mut rx) = broadcast::channel(8);
        let fake = Arc::new(Scripted(
            Mutex::new(vec![stamp(1), stamp(2), stamp(3)]),
            AtomicUsize::new(0),
        ));
        r.p.watch = crate::watch::LibraryWatch::new(fake.clone(), events);
        let lib = r.p.library().unwrap();
        // Each look sees a different library: only the first look of a login finds nothing to
        // compare with. (A look inside the focus gap is skipped, so the login clears the gap.)
        lib.check_changes().await;
        assert_eq!(fake.1.load(SeqCst), 1);
        assert!(rx.try_recv().is_err());
        // A library login ends the baseline: the next look is a first one again.
        r.p.login(STEP_WEB, &input(&[(INPUT_CLIENT_ID, ID)]))
            .await
            .unwrap();
        lib.check_changes().await;
        assert_eq!(fake.1.load(SeqCst), 2);
        assert!(rx.try_recv().is_err());
        // The same for a library logout.
        r.p.logout(STEP_WEB).await.unwrap();
        lib.check_changes().await;
        assert_eq!(fake.1.load(SeqCst), 3);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn check_changes_without_a_library_login_sends_nothing_and_asks_nobody() {
        let r = rig();
        r.p.library().unwrap().check_changes().await;
        assert_eq!(r.web.refreshes.load(SeqCst) + r.web.logins.load(SeqCst), 0);
    }
}
