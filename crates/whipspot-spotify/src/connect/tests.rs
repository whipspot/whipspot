//! The Connect host on a fake backend: no network, no librespot session, paused time.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

use librespot_playback::mixer::Mixer;

use super::{
    backend::fake::{FakeConnect, FakeSpirc},
    mixer::{DEFAULT_LEVEL, to_level},
    *,
};
use crate::{
    accounts,
    auth::{Secret, Tokens},
    auth_manager::SlotBackend,
    player_host::fake::FakeBackend,
    provider::{STEP_PLAYBACK, STEP_WEB, SpotifyProvider},
    web_auth::TokenError,
};
use whipspot_core::{
    credential_store::{CredentialStore, StoreError},
    engine::Engine,
    provider::{AuthFlow, LoginInput, Provider},
};
use whipspot_testkit::{MemoryStore, run_provider_contract, run_remote_contract};

const NAME: &str = "Whipspot on PC";
const SHOWN: &str = "Shown as Whipspot on PC in your Spotify apps";

/// Playback login backend: each refresh hands out a new token (`access-1`, `access-2`, ...).
#[derive(Default)]
struct Tok {
    refreshes: AtomicUsize,
    login_error: Mutex<Option<AuthError>>,
}

fn tokens(access: &str) -> Tokens {
    Tokens {
        access: Secret::new(access),
        refresh: Secret::new("refresh-from-backend"),
        expires_at: std::time::Instant::now() + Duration::from_secs(3600),
        scopes: vec![],
    }
}

impl SlotBackend for Tok {
    fn refresh<'a>(
        &'a self,
        _: Option<&'a str>,
        _: &'a Secret,
    ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
        Box::pin(async move {
            let n = self.refreshes.fetch_add(1, SeqCst) + 1;
            Ok(tokens(&format!("access-{n}")))
        })
    }
    fn login<'a>(&'a self, _: Option<&'a str>) -> BoxFuture<'a, Result<Tokens, AuthError>> {
        Box::pin(async move {
            if let Some(e) = self.login_error.lock().unwrap().take() {
                return Err(e);
            }
            Ok(tokens("access-from-login"))
        })
    }
}

/// A [`MemoryStore`] whose writes can be made to fail while reads and deletes still work.
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
    p: Arc<SpotifyProvider>,
    fake: Arc<FakeConnect>,
    store: Arc<FailingWrites>,
    pb: Arc<Tok>,
    rx: broadcast::Receiver<UiEvent>,
}

impl Rig {
    fn host(&self) -> &ConnectHost {
        self.p.connect.as_deref().expect("connect")
    }
    /// RemoteEnded events since the last call.
    fn ended(&mut self) -> usize {
        let mut n = 0;
        while let Ok(ev) = self.rx.try_recv() {
            if let UiEvent::RemoteEnded { provider } = ev {
                assert_eq!(provider, "spotify");
                n += 1;
            }
        }
        n
    }
    /// AuthChanged events since the last call (other events are dropped).
    fn auth_changes(&mut self) -> usize {
        let mut n = 0;
        while let Ok(ev) = self.rx.try_recv() {
            if let UiEvent::AuthChanged { provider } = ev {
                assert_eq!(provider, "spotify");
                n += 1;
            }
        }
        n
    }
    async fn connect_step(&self) -> AuthStepStatus {
        let status = self.p.status().await.unwrap();
        assert_eq!(status.steps.len(), 3);
        status.steps[2].clone()
    }
}

/// A provider with Connect on a fake backend; `logged_in` stores a Playback login.
fn rig_with(enabled: bool, logged_in: bool, fake: FakeConnect) -> Rig {
    let store = Arc::new(FailingWrites::default());
    if logged_in {
        store
            .inner
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("r"))
            .unwrap();
    }
    let (events, rx) = broadcast::channel(256);
    let source = Engine::new(Default::default(), None).source;
    let ctx = ProviderContext { source, events };
    let fake = Arc::new(fake);
    let pb = Arc::new(Tok::default());
    let mixer = Arc::new(ConnectMixer::new(DEFAULT_LEVEL, ctx.events.clone()));
    let p = SpotifyProvider::with_backends(
        store.clone(),
        pb.clone(),
        Arc::new(Tok::default()),
        ctx.clone(),
    )
    .with_player_backend(Arc::new(FakeBackend::default()))
    .with_connect_backend(
        ConnectOptions {
            enabled,
            name: NAME.into(),
            device_id: "ab".repeat(20),
            volume: 50,
            context: ctx.for_owner(OWNER),
        },
        fake.clone(),
        mixer,
    );
    Rig {
        p: Arc::new(p),
        fake,
        store,
        pb,
        rx,
    }
}

fn rig(enabled: bool, logged_in: bool) -> Rig {
    rig_with(enabled, logged_in, FakeConnect::default())
}

/// Let every task run that does not wait for time.
async fn idle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

fn none() -> LoginInput {
    LoginInput::default()
}

fn calls(s: &FakeSpirc) -> Vec<String> {
    s.calls()
}

#[tokio::test(start_paused = true)]
async fn connect_starts_only_when_turned_on_and_logged_in() {
    // Off, logged in: nothing starts.
    let r = rig(false, true);
    idle().await;
    assert!(r.fake.tokens().is_empty());
    let step = r.connect_step().await;
    assert_eq!(step.state, LoginState::LoggedOut);
    assert_eq!(step.action.as_deref(), Some("Turn on"));
    assert_eq!(step.detail, None);
    // Turned on: it starts on the Playback token, with the name, id and quality given.
    let status = r.p.login(STEP_CONNECT, &none()).await.unwrap();
    assert_eq!(status.steps[2].state, LoginState::LoggedIn);
    assert_eq!(status.steps[2].action.as_deref(), Some("Turn off"));
    idle().await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
    assert_eq!(
        r.fake.cfgs(),
        [StartCfg {
            name: NAME.into(),
            device_id: "ab".repeat(20),
            quality: Quality::Kbps320,
        }]
    );
    assert_eq!(r.connect_step().await.detail.as_deref(), Some(SHOWN));

    // On, logged out: nothing starts until a Playback login.
    let r = rig(true, false);
    idle().await;
    assert!(r.fake.tokens().is_empty());
    let step = r.connect_step().await;
    assert_eq!(step.state, LoginState::LoggedIn);
    assert_eq!(step.detail.as_deref(), Some("Log in to Playback first"));
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert!(r.fake.tokens().is_empty());
    r.p.login(STEP_PLAYBACK, &none()).await.unwrap();
    idle().await;
    assert_eq!(r.fake.tokens(), ["access-from-login"]);
}

#[tokio::test(start_paused = true)]
async fn a_playback_login_restarts_connect_on_the_new_token() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    r.ended();
    r.p.login(STEP_PLAYBACK, &none()).await.unwrap();
    // The old Spirc was shut down and closed before the login returned.
    assert_eq!(calls(&first), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    idle().await;
    assert_eq!(r.fake.tokens(), ["access-1", "access-from-login"]);
    assert!(calls(&r.fake.last()).is_empty());
    assert_eq!(r.connect_step().await.detail.as_deref(), Some(SHOWN));
}

#[tokio::test(start_paused = true)]
async fn a_playback_logout_stops_connect_and_ends_remote_once() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    r.ended();
    r.p.logout(STEP_PLAYBACK).await.unwrap();
    assert_eq!(calls(&first), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(r.fake.tokens(), ["access-1"], "no retry without a login");
    assert_eq!(r.ended(), 0);
    let step = r.connect_step().await;
    assert_eq!(step.state, LoginState::LoggedIn, "still turned on");
    assert_eq!(step.detail.as_deref(), Some("Log in to Playback first"));
}

// The S3 class (MD137): a Playback login whose credential-store write fails clears the login,
// and Connect, which ran on the old one, ends with it.
#[tokio::test(start_paused = true)]
async fn a_playback_login_whose_store_write_fails_stops_connect() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    r.ended();
    r.store.fail.store(true, SeqCst);
    assert_eq!(
        r.p.login(STEP_PLAYBACK, &none()).await.unwrap_err().0,
        "credential store access failed"
    );
    r.store.fail.store(false, SeqCst);
    assert_eq!(calls(&first), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    assert_eq!(r.store.inner.peek(accounts::PLAYBACK_REFRESH), None);
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
}

// F9: a Playback login that changed nothing leaves Connect running.
#[tokio::test(start_paused = true)]
async fn a_playback_login_that_changed_nothing_leaves_connect_alone() {
    let mut r = rig(true, true);
    idle().await;
    r.ended();
    *r.pb.login_error.lock().unwrap() = Some(AuthError::LoginDenied);
    assert!(r.p.login(STEP_PLAYBACK, &none()).await.is_err());
    idle().await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
    assert!(calls(&r.fake.last()).is_empty());
    assert_eq!(r.ended(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_unexpected_end_retries_with_the_backoff() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    // The next four starts fail, the fifth works.
    r.fake
        .results
        .lock()
        .unwrap()
        .extend([Err(backend::StartError); 4]);
    first.end();
    idle().await;
    let base = Instant::now();
    assert_eq!(
        calls(&first),
        ["close"],
        "an end by itself is closed, not shut down"
    );
    assert_eq!(r.ended(), 1);
    assert_eq!(
        r.host().step(&LoginState::LoggedIn).detail.as_deref(),
        Some("Could not reach Spotify; trying again")
    );
    let margin = Duration::from_millis(50);
    for (n, at) in [5u64, 20, 80, 380, 680].into_iter().enumerate() {
        let at = base + Duration::from_secs(at);
        tokio::time::sleep_until(at - margin).await;
        assert_eq!(r.fake.tokens().len(), n + 1, "before retry {}", n + 1);
        tokio::time::sleep_until(at + margin).await;
        assert_eq!(r.fake.tokens().len(), n + 2, "at retry {}", n + 1);
    }
    // Every retry asked for a fresh token.
    assert_eq!(
        r.fake.tokens(),
        [
            "access-1", "access-2", "access-3", "access-4", "access-5", "access-6"
        ]
    );
    assert_eq!(
        r.host().step(&LoginState::LoggedIn).detail.as_deref(),
        Some(SHOWN)
    );
    // Failed starts had no run to end.
    assert_eq!(r.ended(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_long_run_starts_the_backoff_again() {
    let r = rig(true, true);
    idle().await;
    r.fake.last().end();
    tokio::time::sleep(Duration::from_millis(5100)).await;
    assert_eq!(r.fake.tokens().len(), 2);
    // Up for over ten minutes, then gone: the first wait again (5 s, not 15 s).
    tokio::time::sleep(Duration::from_secs(601)).await;
    r.fake.last().end();
    idle().await;
    tokio::time::sleep(Duration::from_millis(5100)).await;
    assert_eq!(r.fake.tokens().len(), 3);
    // Gone at once: the next wait is 15 s.
    r.fake.last().end();
    idle().await;
    tokio::time::sleep(Duration::from_secs(14)).await;
    assert_eq!(r.fake.tokens().len(), 3);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(r.fake.tokens().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn not_logged_in_stops_the_retries() {
    let r = rig(true, true);
    idle().await;
    // The stored login goes away under the running Connect (the in-memory token stays).
    r.store.inner.delete(accounts::PLAYBACK_REFRESH).unwrap();
    r.fake.last().end();
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(r.fake.tokens(), ["access-1"], "the retry found no login");
    assert_eq!(
        r.host().step(&LoginState::LoggedIn).detail.as_deref(),
        Some("Log in to Playback first")
    );
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
    // The next login starts it again.
    r.p.login(STEP_PLAYBACK, &none()).await.unwrap();
    idle().await;
    assert_eq!(r.fake.tokens(), ["access-1", "access-from-login"]);
}

#[tokio::test(start_paused = true)]
async fn a_phase_change_is_announced_so_the_status_line_follows() {
    // Starting at spawn, then running: one announcement, and the step says so.
    let mut r = rig(true, true);
    idle().await;
    assert_eq!(r.auth_changes(), 1, "starting to running");
    assert_eq!(
        r.connect_step().await.detail.as_deref(),
        Some(format!("Shown as {NAME} in your Spotify apps").as_str())
    );
    // An unexpected end: running to retrying, then running again.
    r.fake.last().end();
    idle().await;
    assert_eq!(r.auth_changes(), 1, "running to retrying");
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(r.auth_changes(), 1, "retrying to running");
    // Nothing changes: nothing is announced.
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(r.auth_changes(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_start_in_flight_is_closed_when_the_login_ends() {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let fake = FakeConnect::default();
    *fake.gate.lock().unwrap() = Some((entered.clone(), release.clone()));
    let mut r = rig_with(true, true, fake);
    entered.notified().await;
    assert_eq!(r.connect_step().await.detail.as_deref(), Some("Starting"));
    let p = r.p.clone();
    let logout = tokio::spawn(async move { p.logout(STEP_PLAYBACK).await.map(|_| ()) });
    idle().await;
    assert!(
        !logout.is_finished(),
        "the logout waits for the start in flight"
    );
    release.notify_one();
    logout.await.unwrap().unwrap();
    // The run the start returned was never shown: shut down, closed, ended once.
    assert_eq!(calls(&r.fake.last()), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    assert!(r.host().control().is_none());
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
}

#[tokio::test(start_paused = true)]
async fn a_stuck_spirc_is_closed_after_the_shutdown_wait() {
    let r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    first.stuck.store(true, SeqCst);
    let t = Instant::now();
    r.p.logout(STEP_PLAYBACK).await.unwrap();
    assert!(t.elapsed() >= SHUTDOWN_WAIT && t.elapsed() < SHUTDOWN_WAIT * 2);
    assert_eq!(calls(&first), ["shutdown", "close"]);
}

#[tokio::test(start_paused = true)]
async fn the_connect_step_turns_connect_off_and_on() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    r.ended();
    // Its button while on turns it off.
    let s = r.p.login(STEP_CONNECT, &none()).await.unwrap();
    assert_eq!(s.steps[2].state, LoginState::LoggedOut);
    assert_eq!(s.steps[2].action.as_deref(), Some("Turn on"));
    assert_eq!(calls(&first), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    assert!(!r.host().enabled());
    // And on again: a new start.
    r.p.login(STEP_CONNECT, &none()).await.unwrap();
    idle().await;
    assert_eq!(r.fake.tokens().len(), 2);
    // Its logout turns it off; twice is fine.
    for _ in 0..2 {
        let s = r.p.logout(STEP_CONNECT).await.unwrap();
        assert_eq!(s.steps[2].state, LoginState::LoggedOut);
    }
    assert_eq!(calls(&r.fake.last()), ["shutdown", "close"]);
    // Neither touched the logins.
    assert!(r.store.inner.peek(accounts::PLAYBACK_REFRESH).is_some());
    // Inputs are refused.
    let input = LoginInput([("x".to_owned(), "y".to_owned())].into());
    assert_eq!(
        r.p.login(STEP_CONNECT, &input).await.unwrap_err().0,
        "unexpected login input"
    );
}

#[tokio::test]
async fn without_connect_the_step_does_not_exist() {
    let store = Arc::new(MemoryStore::default());
    let (events, _) = broadcast::channel(8);
    let source = Engine::new(Default::default(), None).source;
    let p = SpotifyProvider::new(store, ProviderContext { source, events });
    assert!(p.remote().is_none());
    assert_eq!(p.info().auth_steps.len(), 2);
    assert_eq!(
        p.login(STEP_CONNECT, &none()).await.unwrap_err().0,
        "unknown login step"
    );
    assert_eq!(
        p.logout(STEP_CONNECT).await.unwrap_err().0,
        "unknown login step"
    );
    assert_eq!(p.status().await.unwrap().steps.len(), 2);
    p.shutdown_connect().await;
}

#[tokio::test(start_paused = true)]
async fn info_and_status_list_the_connect_step() {
    let r = rig(true, true);
    idle().await;
    let info = r.p.info();
    let ids: Vec<_> = info.auth_steps.iter().map(|s| s.id).collect();
    assert_eq!(ids, [STEP_PLAYBACK, STEP_WEB, STEP_CONNECT]);
    assert_eq!(info.auth_steps[2].label, "Connect speaker");
    assert!(info.auth_steps[2].inputs.is_empty());
    let json = serde_json::to_string(&r.p.status().await.unwrap()).unwrap();
    assert_eq!(
        json,
        format!(
            r#"{{"steps":[{{"id":"playback","state":"loggedIn"}},{{"id":"web","state":"loggedOut"}},{{"id":"connect","state":"loggedIn","action":"Turn off","detail":"{SHOWN}"}}]}}"#
        )
    );
}

#[tokio::test(start_paused = true)]
async fn remote_commands_reach_spirc() {
    let r = rig(true, true);
    idle().await;
    let remote = r.p.remote().expect("remote");
    assert_eq!(remote.owner(), "spotify.connect");
    for cmd in [
        RemoteCommand::Play,
        RemoteCommand::Pause,
        RemoteCommand::Next,
        RemoteCommand::Previous,
        RemoteCommand::Seek(5000),
        RemoteCommand::Volume(40),
    ] {
        remote.command(cmd).await.unwrap();
    }
    // Spirc has the level in flight already: not sent again.
    remote.command(RemoteCommand::Volume(40)).await.unwrap();
    remote.release().await;
    assert_eq!(
        calls(&r.fake.last()),
        [
            "play",
            "pause",
            "next",
            "prev",
            "seek 5000",
            &format!("volume {}", to_level(40)),
            "disconnect true",
            "pause player",
        ]
    );
    // The mixer's level is Spirc's to set (this fake Spirc never applies it).
    assert_eq!(r.host().mixer.volume(), DEFAULT_LEVEL);
}

#[tokio::test(start_paused = true)]
async fn a_release_pauses_the_player_when_spirc_went_inactive_unseen() {
    // librespot-connect 0.8.0 can go inactive with no `SessionDisconnected` (a failed
    // `became_inactive` request, a cluster update without a cluster) while its player plays on;
    // it then drops every command. The core is still in remote mode, so Stop releases.
    let r = rig(true, true);
    idle().await;
    let spirc = r.fake.last();
    spirc.inactive.store(true, SeqCst);
    let remote = r.p.remote().expect("remote");
    remote.command(RemoteCommand::Pause).await.unwrap();
    remote.release().await;
    // The disconnect is dropped like the pause; the player is paused all the same.
    assert_eq!(
        calls(&spirc),
        ["dropped pause", "dropped disconnect true", "pause player"]
    );
}

#[tokio::test(start_paused = true)]
async fn remote_commands_without_a_session_do_nothing() {
    let r = rig(false, true);
    idle().await;
    let remote = r.p.remote().expect("remote");
    remote.command(RemoteCommand::Play).await.unwrap();
    remote.command(RemoteCommand::Volume(10)).await.unwrap();
    remote.release().await;
    assert!(r.fake.runs().is_empty());
    // With no session the level is the next start's initial one.
    assert_eq!(r.host().mixer.volume(), to_level(10));
}

#[tokio::test(start_paused = true)]
async fn shutdown_ends_connect_for_good() {
    let mut r = rig(true, true);
    idle().await;
    let first = r.fake.last();
    r.ended();
    r.p.shutdown_connect().await;
    assert_eq!(calls(&first), ["shutdown", "close"]);
    assert_eq!(r.ended(), 1);
    // Nothing starts again, and later changes return at once.
    r.p.login(STEP_PLAYBACK, &none()).await.unwrap();
    r.p.logout(STEP_PLAYBACK).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(r.fake.tokens(), ["access-1"]);
    assert!(r.host().enabled(), "the setting is kept");
}

#[tokio::test]
async fn connect_passes_the_remote_and_provider_contracts() {
    // Off, and on (the shipped default): the `connect` step is a switch, so it may start on.
    for enabled in [false, true] {
        let r = rig(enabled, false);
        assert!(
            r.p.info()
                .auth_steps
                .iter()
                .any(|s| s.id == STEP_CONNECT && s.switch),
            "the connect step is a switch"
        );
        run_remote_contract(r.p.clone()).await;
        run_provider_contract(r.p.clone()).await;
    }
}
