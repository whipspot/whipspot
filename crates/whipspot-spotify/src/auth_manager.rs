//! Two independent logins behind one manager (decision D12):
//!
//! * **playback**: librespot's public client id, scope `streaming`, used only to open the librespot
//!   `Session` ([`PlaybackTokens`]);
//! * **web**: the user's own client id, library scopes, used only for `api.spotify.com`
//!   ([`WebTokens`]).
//!
//! They share nothing but the credential store (different accounts) and the rule that only one
//! interactive login runs at a time (both open the browser). Each slot has its own access-token
//! cache, its own single-flight refresh lock and its own login state; a failure in one slot never
//! clears or blocks the other.
//!
//! No token is ever formatted: values live in [`Secret`]; every error text is fixed and
//! whitelisted (no library error is forwarded).

use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    accounts,
    auth::{self, LoginError, Secret, Tokens, should_store_refresh, spotify_consts},
    web_auth::{self, TokenError},
};
use whipspot_core::credential_store::{CredentialStore, StoreError};

pub use whipspot_core::provider::{BoxFuture, model::LoginState};

/// A cached access token counts as expired this long before Spotify says it does.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------- errors and status

/// Auth failure. `Display` is fixed text, safe to show in a UI or a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    NotLoggedIn,
    AlreadyInProgress,
    InvalidClientId,
    LoginTimedOut,
    LoginDenied,
    PortInUse,
    /// Spotify rejected the client id or redirect URI.
    ClientRejected,
    Network,
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    /// Spotify's login service answered with an error status.
    ServiceError,
    CredentialStore,
    LoginFailed,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AuthError::NotLoggedIn => "not logged in",
            AuthError::AlreadyInProgress => "another login is already in progress",
            AuthError::InvalidClientId => "client id must be 32 hex characters",
            AuthError::LoginTimedOut => "login timed out",
            AuthError::LoginDenied => "login was not completed",
            AuthError::PortInUse => "the login port is already in use",
            AuthError::ClientRejected => "Spotify rejected the client id or redirect URI",
            AuthError::Network => "network error reaching Spotify",
            AuthError::RateLimited { .. } => "Spotify is rate-limiting login requests",
            AuthError::ServiceError => "Spotify's login service returned an error",
            AuthError::CredentialStore => "credential store access failed",
            AuthError::LoginFailed => "login failed",
        })
    }
}

impl std::error::Error for AuthError {}

impl From<StoreError> for AuthError {
    fn from(_: StoreError) -> Self {
        AuthError::CredentialStore
    }
}

/// Map a token-endpoint failure that is not `invalid_grant` handling (that is done by the slot).
fn from_token_error(e: &TokenError) -> AuthError {
    match e {
        TokenError::InvalidGrant => AuthError::NotLoggedIn,
        TokenError::InvalidClient => AuthError::ClientRejected,
        TokenError::RateLimited(s) => AuthError::RateLimited {
            retry_after_secs: *s,
        },
        TokenError::Rejected(_) | TokenError::BadResponse => AuthError::ServiceError,
        TokenError::Network => AuthError::Network,
    }
}

/// Classify an error from the blocking login functions without parsing text.
pub fn classify_login_error(e: &anyhow::Error) -> AuthError {
    match e.downcast_ref::<LoginError>() {
        Some(LoginError::Timeout) => AuthError::LoginTimedOut,
        Some(LoginError::Denied) => AuthError::LoginDenied,
        Some(LoginError::PortInUse) => AuthError::PortInUse,
        None => AuthError::LoginFailed,
    }
}

/// Contains no secret; safe to send to the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub playback: LoginState,
    pub web: LoginState,
    pub web_client_id_set: bool,
}

// ---------------------------------------------------------------- token sources

/// Something that can hand out a current access token.
pub trait TokenSource: Send + Sync {
    /// A cached token if it has more than 60 s left, else a refreshed one (single-flight).
    fn access_token(&self) -> BoxFuture<'_, Result<Secret, AuthError>>;
    /// Drop the cached token and refresh (used after a 401 or a failed connect).
    fn force_refresh(&self) -> BoxFuture<'_, Result<Secret, AuthError>>;
}

/// The playback login's token source. A distinct type from [`WebTokens`] so the compiler stops
/// either token being handed to the wrong consumer; both also implement [`TokenSource`].
#[derive(Clone)]
pub struct PlaybackTokens(Arc<Inner>);

/// The web login's token source (see [`PlaybackTokens`]).
#[derive(Clone)]
pub struct WebTokens(Arc<Inner>);

impl TokenSource for PlaybackTokens {
    fn access_token(&self) -> BoxFuture<'_, Result<Secret, AuthError>> {
        Box::pin(self.0.playback.access_token(false))
    }
    fn force_refresh(&self) -> BoxFuture<'_, Result<Secret, AuthError>> {
        Box::pin(self.0.playback.access_token(true))
    }
}

impl WebTokens {
    /// Whether the latest token response granted `scope`: `None` when the scopes are unknown
    /// (no response yet, or the response named none). The scope text is never logged or stored.
    pub fn has_scope(&self, scope: &str) -> Option<bool> {
        let g = self.0.web.granted.lock().unwrap();
        g.as_ref().map(|list| list.iter().any(|s| s == scope))
    }

    /// Forget that `scope` was granted (a write was refused with 403): a known list loses it,
    /// an unknown one becomes known-empty. The next token response replaces it.
    pub fn forget_scope(&self, scope: &str) {
        let mut g = self.0.web.granted.lock().unwrap();
        match g.as_mut() {
            Some(list) => list.retain(|s| s != scope),
            None => *g = Some(vec![]),
        }
    }
}

impl TokenSource for WebTokens {
    fn access_token(&self) -> BoxFuture<'_, Result<Secret, AuthError>> {
        Box::pin(self.0.web.access_token(false))
    }
    fn force_refresh(&self) -> BoxFuture<'_, Result<Secret, AuthError>> {
        Box::pin(self.0.web.access_token(true))
    }
}

// ---------------------------------------------------------------- slot backends

/// The network side of one login (what differs between playback and web, and what tests fake).
pub trait SlotBackend: Send + Sync {
    /// Redeem `refresh_token`. `client_id` is `Some` for the web slot (stored by the user).
    fn refresh<'a>(
        &'a self,
        client_id: Option<&'a str>,
        refresh_token: &'a Secret,
    ) -> BoxFuture<'a, Result<Tokens, TokenError>>;

    /// Interactive login (opens the browser). `client_id` is `Some` for the web slot.
    fn login<'a>(&'a self, client_id: Option<&'a str>) -> BoxFuture<'a, Result<Tokens, AuthError>>;
}

struct PlaybackBackend;

impl SlotBackend for PlaybackBackend {
    fn refresh<'a>(
        &'a self,
        _client_id: Option<&'a str>,
        refresh_token: &'a Secret,
    ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
        Box::pin(web_auth::refresh(spotify_consts::CLIENT_ID, refresh_token))
    }

    fn login<'a>(
        &'a self,
        _client_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Tokens, AuthError>> {
        Box::pin(async {
            let timeout = Duration::from_secs(spotify_consts::LOGIN_TIMEOUT_SECS);
            tokio::task::spawn_blocking(move || {
                auth::login_blocking(spotify_consts::SCOPES_STREAMING, timeout)
            })
            .await
            .map_err(|_| AuthError::LoginFailed)?
            .map_err(|e| classify_login_error(&e))
        })
    }
}

struct WebBackend;

impl SlotBackend for WebBackend {
    fn refresh<'a>(
        &'a self,
        client_id: Option<&'a str>,
        refresh_token: &'a Secret,
    ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
        Box::pin(async move {
            let id = client_id.ok_or(TokenError::InvalidClient)?;
            web_auth::refresh(id, refresh_token).await
        })
    }

    fn login<'a>(&'a self, client_id: Option<&'a str>) -> BoxFuture<'a, Result<Tokens, AuthError>> {
        Box::pin(async move {
            let id = client_id.ok_or(AuthError::InvalidClientId)?.to_owned();
            let timeout = Duration::from_secs(web_auth::consts::LOGIN_TIMEOUT_SECS);
            let id2 = id.clone();
            let (code, verifier) = tokio::task::spawn_blocking(move || {
                web_auth::authorize_blocking(&id2, timeout, |url| {
                    // The URL holds the PKCE challenge and state only; no secret.
                    let _ = open::that_in_background(url);
                })
            })
            .await
            .map_err(|_| AuthError::LoginFailed)?
            .map_err(|e| classify_login_error(&e))?;
            web_auth::exchange_code(&id, &code, &verifier)
                .await
                .map_err(|e| from_token_error(&e))
        })
    }
}

// ---------------------------------------------------------------- slot

struct Cached {
    access: Secret,
    expires_at: Instant,
}

/// One login: its credential-store accounts, access-token cache, single-flight refresh lock and
/// last error.
struct Slot {
    store: Arc<dyn CredentialStore>,
    refresh_account: &'static str,
    client_id_account: Option<&'static str>,
    backend: Arc<dyn SlotBackend>,
    cache: Mutex<Option<Cached>>,
    /// The scopes of the latest token response (login exchange or refresh); `None` when no
    /// response has been seen, the response had no `scope` field, or the login is gone.
    granted: Mutex<Option<Vec<String>>>,
    refresh_lock: tokio::sync::Mutex<()>,
    last_error: Mutex<Option<String>>,
}

impl Slot {
    fn new(
        store: Arc<dyn CredentialStore>,
        refresh_account: &'static str,
        client_id_account: Option<&'static str>,
        backend: Arc<dyn SlotBackend>,
    ) -> Self {
        Slot {
            store,
            refresh_account,
            client_id_account,
            backend,
            cache: Mutex::new(None),
            granted: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
            last_error: Mutex::new(None),
        }
    }

    fn set_error(&self, e: Option<&AuthError>) {
        *self.last_error.lock().unwrap() = e.map(|e| e.to_string());
    }

    fn cached(&self) -> Option<Secret> {
        let g = self.cache.lock().unwrap();
        let c = g.as_ref()?;
        (c.expires_at > Instant::now() + EXPIRY_MARGIN).then(|| c.access.clone())
    }

    fn set_cache(&self, t: &Tokens) {
        *self.cache.lock().unwrap() = Some(Cached {
            access: t.access.clone(),
            expires_at: t.expires_at,
        });
        // An empty `scope` means the response did not say: unknown, not "none granted".
        *self.granted.lock().unwrap() = (!t.scopes.is_empty()).then(|| t.scopes.clone());
    }

    fn clear_login_state(&self) {
        *self.cache.lock().unwrap() = None;
        *self.granted.lock().unwrap() = None;
    }

    fn status(&self) -> LoginState {
        if let Some(message) = self.last_error.lock().unwrap().clone() {
            return LoginState::Error { message };
        }
        if self.cached().is_some() {
            return LoginState::LoggedIn;
        }
        let present = |account: &str| self.store.get(account);
        let mut accounts = vec![self.refresh_account];
        accounts.extend(self.client_id_account);
        for account in accounts {
            match present(account) {
                Ok(Some(_)) => {}
                Ok(None) => return LoginState::LoggedOut,
                Err(_) => {
                    return LoginState::Error {
                        message: AuthError::CredentialStore.to_string(),
                    };
                }
            }
        }
        LoginState::LoggedIn
    }

    async fn access_token(&self, force: bool) -> Result<Secret, AuthError> {
        if force {
            *self.cache.lock().unwrap() = None;
        } else if let Some(t) = self.cached() {
            return Ok(t);
        }
        let _g = self.refresh_lock.lock().await;
        // Someone else may have refreshed while we waited for the lock.
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        self.refresh_locked().await
    }

    /// Caller holds `refresh_lock`.
    async fn refresh_locked(&self) -> Result<Secret, AuthError> {
        let store_err = || {
            let e = AuthError::CredentialStore;
            self.set_error(Some(&e));
            e
        };
        let refresh = match self.store.get(self.refresh_account) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(AuthError::NotLoggedIn),
            Err(_) => return Err(store_err()),
        };
        let client_id = match self.client_id_account {
            None => None,
            Some(account) => match self.store.get(account) {
                Ok(Some(id)) => Some(id.expose().to_owned()),
                Ok(None) => return Err(AuthError::NotLoggedIn),
                Err(_) => return Err(store_err()),
            },
        };
        match self.backend.refresh(client_id.as_deref(), &refresh).await {
            Ok(t) => {
                // Spotify may not rotate the refresh token: an empty one keeps the stored value.
                let persisted = !should_store_refresh(t.refresh.expose())
                    || self.store.set(self.refresh_account, &t.refresh).is_ok();
                self.set_error((!persisted).then_some(&AuthError::CredentialStore));
                self.set_cache(&t);
                Ok(t.access)
            }
            Err(TokenError::InvalidGrant) => {
                // The stored login is dead. Drop only this slot's refresh token (the public web
                // client id stays so the connect card can prefill it).
                self.clear_login_state();
                match self.store.delete(self.refresh_account) {
                    Ok(_) => Err(AuthError::NotLoggedIn),
                    Err(_) => Err(store_err()),
                }
            }
            Err(e) => Err(from_token_error(&e)),
        }
    }

    /// Run the interactive login and persist it. Nothing is written unless the login succeeded,
    /// so a timeout or denial leaves the previous state untouched.
    async fn login(&self, client_id: Option<&str>) -> Result<(), AuthError> {
        self.set_error(None);
        let result = self.login_inner(client_id).await;
        if let Err(e) = &result {
            self.set_error(Some(e));
        }
        result
    }

    /// The login state changes only on `Ok` (the new login is stored and cached) and on
    /// `Err(CredentialStore)` (the write failed: the cache is cleared and the refresh token
    /// deleted, best effort); every other error returns before anything is touched. The
    /// playback login's `AuthFlow` ends the player's session on exactly those two (S3, F9): an
    /// error added after a change must be one of them, or be added to that match.
    async fn login_inner(&self, client_id: Option<&str>) -> Result<(), AuthError> {
        let tokens = self.backend.login(client_id).await?;
        if !should_store_refresh(tokens.refresh.expose()) {
            return Err(AuthError::LoginFailed);
        }
        // Serialise with an in-flight refresh so it cannot overwrite the new login afterwards.
        let _g = self.refresh_lock.lock().await;
        let write = || -> Result<(), StoreError> {
            self.store.set(self.refresh_account, &tokens.refresh)?;
            if let (Some(account), Some(id)) = (self.client_id_account, client_id) {
                self.store.set(account, &Secret::new(id))?;
            }
            Ok(())
        };
        if write().is_err() {
            // Do not leave a refresh token paired with the wrong client id.
            let _ = self.store.delete(self.refresh_account);
            self.clear_login_state();
            return Err(AuthError::CredentialStore);
        }
        self.set_cache(&tokens);
        Ok(())
    }

    async fn logout(&self) -> Result<(), AuthError> {
        let _g = self.refresh_lock.lock().await;
        self.clear_login_state();
        let mut result = Ok(());
        let mut accounts = vec![self.refresh_account];
        accounts.extend(self.client_id_account);
        for account in accounts {
            if self.store.delete(account).is_err() {
                result = Err(AuthError::CredentialStore);
            }
        }
        self.set_error(result.as_ref().err());
        result
    }
}

// ---------------------------------------------------------------- manager

struct Inner {
    playback: Slot,
    web: Slot,
    /// Both logins open the browser: only one runs at a time.
    login_busy: AtomicBool,
}

struct LoginGuard<'a>(&'a AtomicBool);

impl<'a> LoginGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| LoginGuard(flag))
    }
}

impl Drop for LoginGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct AuthManager(Arc<Inner>);

impl AuthManager {
    /// The real thing: librespot's client id for playback, the user's own for the Web API.
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self::with_backends(store, Arc::new(PlaybackBackend), Arc::new(WebBackend))
    }

    /// Custom network backends (tests).
    pub fn with_backends(
        store: Arc<dyn CredentialStore>,
        playback: Arc<dyn SlotBackend>,
        web: Arc<dyn SlotBackend>,
    ) -> Self {
        AuthManager(Arc::new(Inner {
            playback: Slot::new(store.clone(), accounts::PLAYBACK_REFRESH, None, playback),
            web: Slot::new(
                store,
                accounts::WEB_REFRESH,
                Some(accounts::WEB_CLIENT_ID),
                web,
            ),
            login_busy: AtomicBool::new(false),
        }))
    }

    pub fn playback(&self) -> PlaybackTokens {
        PlaybackTokens(self.0.clone())
    }

    pub fn web(&self) -> WebTokens {
        WebTokens(self.0.clone())
    }

    pub fn status(&self) -> AuthStatus {
        let web_client_id_set =
            matches!(self.0.web.store.get(accounts::WEB_CLIENT_ID), Ok(Some(_)));
        AuthStatus {
            playback: self.0.playback.status(),
            web: self.0.web.status(),
            web_client_id_set,
        }
    }

    /// Interactive playback login (scope `streaming` only). Blocks for up to the login timeout.
    pub async fn login_playback(&self) -> Result<(), AuthError> {
        let _g = LoginGuard::acquire(&self.0.login_busy).ok_or(AuthError::AlreadyInProgress)?;
        self.0.playback.login(None).await
    }

    /// Interactive web login with the user's own client id; the id is validated before anything
    /// else happens.
    pub async fn login_web(&self, client_id: &str) -> Result<(), AuthError> {
        if !web_auth::is_client_id(client_id) {
            return Err(AuthError::InvalidClientId);
        }
        let _g = LoginGuard::acquire(&self.0.login_busy).ok_or(AuthError::AlreadyInProgress)?;
        self.0.web.login(Some(client_id)).await
    }

    /// The stored web client id (public, not a secret) so the connect card can prefill it. Fails
    /// closed: a stored value that is not a valid client id reads as "none".
    pub fn web_client_id(&self) -> Result<Option<String>, AuthError> {
        let stored = self.0.web.store.get(accounts::WEB_CLIENT_ID)?;
        Ok(stored
            .map(|s| s.expose().to_owned())
            .filter(|id| web_auth::is_client_id(id)))
    }

    pub async fn logout_playback(&self) -> Result<(), AuthError> {
        self.0.playback.logout().await
    }

    /// Deletes the web refresh token and the stored client id.
    pub async fn logout_web(&self) -> Result<(), AuthError> {
        self.0.web.logout().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering::SeqCst},
    };
    use tokio::sync::Notify;
    use whipspot_testkit::MemoryStore;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    /// Scripted backend. Tokens look like `<name>-access-<n>` so tests can tell slots apart.
    struct Fake {
        name: &'static str,
        refreshes: AtomicUsize,
        logins: AtomicUsize,
        /// Seconds until the issued access token expires.
        expires_in: Mutex<u64>,
        /// Refresh token returned by refresh/login ("" = Spotify did not rotate).
        rotated: Mutex<String>,
        refresh_delay: Duration,
        refresh_errors: Mutex<VecDeque<TokenError>>,
        login_errors: Mutex<VecDeque<AuthError>>,
        /// Scopes named in the token responses (empty = the response had no `scope` field).
        scopes: Mutex<Vec<String>>,
        /// When set, `login` signals `entered` then waits for `release`.
        gate: Option<(Arc<Notify>, Arc<Notify>)>,
    }

    impl Fake {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Fake {
                name,
                refreshes: AtomicUsize::new(0),
                logins: AtomicUsize::new(0),
                expires_in: Mutex::new(3600),
                rotated: Mutex::new(String::new()),
                refresh_delay: Duration::from_millis(20),
                refresh_errors: Mutex::default(),
                login_errors: Mutex::default(),
                scopes: Mutex::default(),
                gate: None,
            })
        }

        fn gated(name: &'static str, entered: Arc<Notify>, release: Arc<Notify>) -> Arc<Self> {
            let mut f = Arc::try_unwrap(Self::new(name)).ok().unwrap();
            f.gate = Some((entered, release));
            Arc::new(f)
        }

        fn tokens(&self, n: usize) -> Tokens {
            Tokens {
                access: Secret::new(format!("{}-access-{n}", self.name)),
                refresh: Secret::new(self.rotated.lock().unwrap().clone()),
                expires_at: Instant::now() + Duration::from_secs(*self.expires_in.lock().unwrap()),
                scopes: self.scopes.lock().unwrap().clone(),
            }
        }
    }

    impl SlotBackend for Fake {
        fn refresh<'a>(
            &'a self,
            _client_id: Option<&'a str>,
            _refresh_token: &'a Secret,
        ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
            Box::pin(async move {
                let n = self.refreshes.fetch_add(1, SeqCst) + 1;
                tokio::time::sleep(self.refresh_delay).await;
                if let Some(e) = self.refresh_errors.lock().unwrap().pop_front() {
                    return Err(e);
                }
                Ok(self.tokens(n))
            })
        }

        fn login<'a>(
            &'a self,
            _client_id: Option<&'a str>,
        ) -> BoxFuture<'a, Result<Tokens, AuthError>> {
            Box::pin(async move {
                let n = self.logins.fetch_add(1, SeqCst) + 1;
                if let Some((entered, release)) = &self.gate {
                    entered.notify_one();
                    release.notified().await;
                }
                if let Some(e) = self.login_errors.lock().unwrap().pop_front() {
                    return Err(e);
                }
                let mut t = self.tokens(100 + n);
                t.refresh = Secret::new(format!("{}-refresh-from-login", self.name));
                Ok(t)
            })
        }
    }

    struct Rig {
        store: Arc<MemoryStore>,
        pb: Arc<Fake>,
        web: Arc<Fake>,
        mgr: AuthManager,
    }

    fn rig_with(pb: Arc<Fake>, web: Arc<Fake>) -> Rig {
        let store = Arc::new(MemoryStore::default());
        let mgr = AuthManager::with_backends(store.clone(), pb.clone(), web.clone());
        Rig {
            store,
            pb,
            web,
            mgr,
        }
    }

    fn rig() -> Rig {
        rig_with(Fake::new("pb"), Fake::new("web"))
    }

    fn seed_both(r: &Rig) {
        let s = |a: &str, v: &str| r.store.set(a, &Secret::new(v)).unwrap();
        s(accounts::PLAYBACK_REFRESH, "pb-refresh-0");
        s(accounts::WEB_REFRESH, "web-refresh-0");
        s(accounts::WEB_CLIENT_ID, ID);
    }

    async fn tok(t: &impl TokenSource) -> Result<String, AuthError> {
        t.access_token().await.map(|s| s.expose().to_owned())
    }

    #[tokio::test]
    async fn cache_hit_serves_without_refresh() {
        let r = rig();
        seed_both(&r);
        let p = r.mgr.playback();
        assert_eq!(tok(&p).await.unwrap(), "pb-access-1");
        assert_eq!(tok(&p).await.unwrap(), "pb-access-1");
        assert_eq!(r.pb.refreshes.load(SeqCst), 1);
        assert_eq!(r.web.refreshes.load(SeqCst), 0);
    }

    #[tokio::test]
    async fn token_inside_the_expiry_margin_is_refreshed_each_time() {
        let r = rig();
        seed_both(&r);
        *r.pb.expires_in.lock().unwrap() = 30; // < 60 s margin: never a cache hit
        let p = r.mgr.playback();
        assert_eq!(tok(&p).await.unwrap(), "pb-access-1");
        assert_eq!(tok(&p).await.unwrap(), "pb-access-2");
        assert_eq!(r.pb.refreshes.load(SeqCst), 2);
        // Just outside the margin it is cached.
        *r.pb.expires_in.lock().unwrap() = 120;
        assert_eq!(tok(&p).await.unwrap(), "pb-access-3");
        assert_eq!(tok(&p).await.unwrap(), "pb-access-3");
        assert_eq!(r.pb.refreshes.load(SeqCst), 3);
    }

    #[tokio::test]
    async fn force_refresh_replaces_the_cache() {
        let r = rig();
        seed_both(&r);
        let p = r.mgr.playback();
        assert_eq!(tok(&p).await.unwrap(), "pb-access-1");
        assert_eq!(p.force_refresh().await.unwrap().expose(), "pb-access-2");
        assert_eq!(tok(&p).await.unwrap(), "pb-access-2");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ten_concurrent_callers_per_slot_cause_one_refresh_each() {
        let r = rig();
        seed_both(&r);
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let p = r.mgr.playback();
            tasks.push(tokio::spawn(async move { tok(&p).await }));
            let w = r.mgr.web();
            tasks.push(tokio::spawn(async move { tok(&w).await }));
        }
        let mut seen = Vec::new();
        for t in tasks {
            seen.push(t.await.unwrap().unwrap());
        }
        assert_eq!(r.pb.refreshes.load(SeqCst), 1);
        assert_eq!(r.web.refreshes.load(SeqCst), 1);
        assert_eq!(seen.iter().filter(|s| *s == "pb-access-1").count(), 10);
        assert_eq!(seen.iter().filter(|s| *s == "web-access-1").count(), 10);
    }

    #[tokio::test]
    async fn rotated_refresh_token_is_stored_empty_one_keeps_the_old() {
        let r = rig();
        seed_both(&r);
        let p = r.mgr.playback();
        // No rotation: stored value unchanged.
        p.force_refresh().await.unwrap();
        assert_eq!(
            r.store.peek(accounts::PLAYBACK_REFRESH).as_deref(),
            Some("pb-refresh-0")
        );
        // Rotation: stored value replaced.
        *r.pb.rotated.lock().unwrap() = "pb-refresh-1".into();
        p.force_refresh().await.unwrap();
        assert_eq!(
            r.store.peek(accounts::PLAYBACK_REFRESH).as_deref(),
            Some("pb-refresh-1")
        );
        // The web slot's items were never touched.
        assert_eq!(
            r.store.peek(accounts::WEB_REFRESH).as_deref(),
            Some("web-refresh-0")
        );
    }

    #[tokio::test]
    async fn web_invalid_grant_clears_web_refresh_only() {
        let r = rig();
        seed_both(&r);
        r.web
            .refresh_errors
            .lock()
            .unwrap()
            .push_back(TokenError::InvalidGrant);
        assert_eq!(tok(&r.mgr.web()).await, Err(AuthError::NotLoggedIn));
        assert_eq!(r.store.peek(accounts::WEB_REFRESH), None);
        // The public client id stays so the card can prefill it; playback is untouched.
        assert_eq!(r.store.peek(accounts::WEB_CLIENT_ID).as_deref(), Some(ID));
        assert_eq!(
            r.store.peek(accounts::PLAYBACK_REFRESH).as_deref(),
            Some("pb-refresh-0")
        );
        assert_eq!(tok(&r.mgr.playback()).await.unwrap(), "pb-access-1");
        let st = r.mgr.status();
        assert_eq!(st.web, LoginState::LoggedOut);
        assert_eq!(st.playback, LoginState::LoggedIn);
        assert!(st.web_client_id_set);
    }

    #[tokio::test]
    async fn playback_invalid_grant_clears_playback_only() {
        let r = rig();
        seed_both(&r);
        r.pb.refresh_errors
            .lock()
            .unwrap()
            .push_back(TokenError::InvalidGrant);
        assert_eq!(tok(&r.mgr.playback()).await, Err(AuthError::NotLoggedIn));
        assert_eq!(r.store.peek(accounts::PLAYBACK_REFRESH), None);
        assert_eq!(
            r.store.peek(accounts::WEB_REFRESH).as_deref(),
            Some("web-refresh-0")
        );
        assert_eq!(tok(&r.mgr.web()).await.unwrap(), "web-access-1");
    }

    #[tokio::test]
    async fn transient_errors_clear_nothing() {
        let r = rig();
        seed_both(&r);
        for (te, expect) in [
            (TokenError::Network, AuthError::Network),
            (
                TokenError::RateLimited(Some(26)),
                AuthError::RateLimited {
                    retry_after_secs: Some(26),
                },
            ),
            (TokenError::Rejected(503), AuthError::ServiceError),
            (TokenError::InvalidClient, AuthError::ClientRejected),
        ] {
            r.web.refresh_errors.lock().unwrap().push_back(te);
            assert_eq!(tok(&r.mgr.web()).await, Err(expect));
            assert_eq!(
                r.store.peek(accounts::WEB_REFRESH).as_deref(),
                Some("web-refresh-0")
            );
        }
        // A failure is not a login state: the next call simply works.
        assert_eq!(r.mgr.status().web, LoginState::LoggedIn);
        assert_eq!(tok(&r.mgr.web()).await.unwrap(), "web-access-5");
    }

    #[tokio::test]
    async fn missing_credentials_mean_not_logged_in_without_network() {
        let r = rig();
        assert_eq!(tok(&r.mgr.playback()).await, Err(AuthError::NotLoggedIn));
        // Web needs both the refresh token and the client id.
        r.store
            .set(accounts::WEB_REFRESH, &Secret::new("x"))
            .unwrap();
        assert_eq!(tok(&r.mgr.web()).await, Err(AuthError::NotLoggedIn));
        assert_eq!(r.mgr.status().web, LoginState::LoggedOut);
        assert_eq!(
            r.pb.refreshes.load(SeqCst) + r.web.refreshes.load(SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn store_failure_is_an_error_not_logged_out() {
        let r = rig();
        seed_both(&r);
        r.store.set_failing(true);
        assert_eq!(
            tok(&r.mgr.playback()).await,
            Err(AuthError::CredentialStore)
        );
        assert_eq!(
            r.mgr.status().playback,
            LoginState::Error {
                message: "credential store access failed".into()
            }
        );
        r.store.set_failing(false);
        assert_eq!(tok(&r.mgr.playback()).await.unwrap(), "pb-access-1");
        assert_eq!(r.mgr.status().playback, LoginState::LoggedIn);
    }

    #[tokio::test]
    async fn rotated_token_that_cannot_be_stored_is_reported() {
        struct FailSet(MemoryStore);
        impl CredentialStore for FailSet {
            fn get(&self, a: &str) -> Result<Option<Secret>, StoreError> {
                self.0.get(a)
            }
            fn set(&self, _: &str, _: &Secret) -> Result<(), StoreError> {
                Err(StoreError)
            }
            fn delete(&self, a: &str) -> Result<bool, StoreError> {
                self.0.delete(a)
            }
        }
        let inner = MemoryStore::default();
        inner
            .set(accounts::PLAYBACK_REFRESH, &Secret::new("old"))
            .unwrap();
        let pb = Fake::new("pb");
        *pb.rotated.lock().unwrap() = "new".into();
        let mgr = AuthManager::with_backends(Arc::new(FailSet(inner)), pb, Fake::new("web"));
        // The access token still works; the failure to persist the rotation is not silent.
        assert_eq!(tok(&mgr.playback()).await.unwrap(), "pb-access-1");
        assert!(matches!(mgr.status().playback, LoginState::Error { .. }));
    }

    #[tokio::test]
    async fn logout_playback_leaves_web_items_and_logout_web_removes_both_web_items() {
        let r = rig();
        seed_both(&r);
        tok(&r.mgr.playback()).await.unwrap();
        r.mgr.logout_playback().await.unwrap();
        assert_eq!(r.store.peek(accounts::PLAYBACK_REFRESH), None);
        assert_eq!(
            r.store.peek(accounts::WEB_REFRESH).as_deref(),
            Some("web-refresh-0")
        );
        assert_eq!(r.store.peek(accounts::WEB_CLIENT_ID).as_deref(), Some(ID));
        // The cached playback token is gone too.
        assert_eq!(tok(&r.mgr.playback()).await, Err(AuthError::NotLoggedIn));
        r.mgr.logout_web().await.unwrap();
        assert_eq!(r.store.peek(accounts::WEB_REFRESH), None);
        assert_eq!(r.store.peek(accounts::WEB_CLIENT_ID), None);
        assert!(!r.mgr.status().web_client_id_set);
    }

    #[tokio::test]
    async fn login_persists_and_failure_leaves_previous_state_untouched() {
        let r = rig();
        seed_both(&r);
        // Denied web login: nothing changes in the store, the other slot is unaffected.
        r.web
            .login_errors
            .lock()
            .unwrap()
            .push_back(AuthError::LoginDenied);
        assert_eq!(r.mgr.login_web(ID).await, Err(AuthError::LoginDenied));
        assert_eq!(
            r.store.peek(accounts::WEB_REFRESH).as_deref(),
            Some("web-refresh-0")
        );
        assert_eq!(
            r.mgr.status().web,
            LoginState::Error {
                message: "login was not completed".into()
            }
        );
        assert_eq!(r.mgr.status().playback, LoginState::LoggedIn);
        // Successful web login with a different id replaces both items.
        let other = "00000000000000000000000000000abc";
        r.mgr.login_web(other).await.unwrap();
        assert_eq!(
            r.store.peek(accounts::WEB_REFRESH).as_deref(),
            Some("web-refresh-from-login")
        );
        assert_eq!(
            r.store.peek(accounts::WEB_CLIENT_ID).as_deref(),
            Some(other)
        );
        assert_eq!(r.mgr.status().web, LoginState::LoggedIn);
        // The login also primed the cache (second login attempt = n 2): no refresh needed.
        assert_eq!(tok(&r.mgr.web()).await.unwrap(), "web-access-102");
        assert_eq!(r.web.refreshes.load(SeqCst), 0);
        // Playback login likewise; it never touches the client id item.
        r.mgr.login_playback().await.unwrap();
        assert_eq!(
            r.store.peek(accounts::PLAYBACK_REFRESH).as_deref(),
            Some("pb-refresh-from-login")
        );
        assert_eq!(
            r.store.peek(accounts::WEB_CLIENT_ID).as_deref(),
            Some(other)
        );
    }

    #[tokio::test]
    async fn login_without_a_refresh_token_is_a_failure() {
        struct NoRefresh;
        impl SlotBackend for NoRefresh {
            fn refresh<'a>(
                &'a self,
                _: Option<&'a str>,
                _: &'a Secret,
            ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
                Box::pin(async { Err(TokenError::Network) })
            }
            fn login<'a>(&'a self, _: Option<&'a str>) -> BoxFuture<'a, Result<Tokens, AuthError>> {
                Box::pin(async {
                    Ok(Tokens {
                        access: Secret::new("a"),
                        refresh: Secret::new(""),
                        expires_at: Instant::now() + Duration::from_secs(3600),
                        scopes: vec![],
                    })
                })
            }
        }
        let store = Arc::new(MemoryStore::default());
        let mgr = AuthManager::with_backends(store.clone(), Arc::new(NoRefresh), Fake::new("w"));
        assert_eq!(mgr.login_playback().await, Err(AuthError::LoginFailed));
        assert_eq!(store.peek(accounts::PLAYBACK_REFRESH), None);
    }

    #[tokio::test]
    async fn failed_store_write_during_login_is_reported() {
        let r = rig();
        seed_both(&r);
        r.store.set_failing(true);
        assert_eq!(
            r.mgr.login_playback().await,
            Err(AuthError::CredentialStore)
        );
        r.store.set_failing(false);
        // The store was failing for the rollback delete too; the old item is still intact.
        assert_eq!(
            r.store.peek(accounts::PLAYBACK_REFRESH).as_deref(),
            Some("pb-refresh-0")
        );
    }

    #[tokio::test]
    async fn concurrent_logins_across_slots_allow_exactly_one() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let r = rig_with(
            Fake::gated("pb", entered.clone(), release.clone()),
            Fake::new("web"),
        );
        let mgr = r.mgr.clone();
        let first = tokio::spawn(async move { mgr.login_playback().await });
        entered.notified().await; // the playback login is now in flight
        assert_eq!(r.mgr.login_web(ID).await, Err(AuthError::AlreadyInProgress));
        assert_eq!(
            r.mgr.login_playback().await,
            Err(AuthError::AlreadyInProgress)
        );
        assert_eq!(r.web.logins.load(SeqCst), 0);
        release.notify_one();
        first.await.unwrap().unwrap();
        // The guard is released: a new login can start.
        r.mgr.login_web(ID).await.unwrap();
        assert_eq!(r.web.logins.load(SeqCst), 1);
        assert_eq!(r.pb.logins.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn client_id_validation_table() {
        let r = rig();
        let cases: &[(&str, bool)] = &[
            (&ID[..31], false),
            (&format!("{ID}0"), false),
            ("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz", false),
            ("", false),
            (" 0123456789abcdef0123456789abcde", false),
            ("0123456789abcdef0123456789abcdef\n", false),
            ("0123456789ABCDEF0123456789ABCDEF", true),
            (ID, true),
        ];
        for (id, valid) in cases {
            let res = r.mgr.login_web(id).await;
            if *valid {
                assert_eq!(res, Ok(()), "{id:?}");
            } else {
                assert_eq!(res, Err(AuthError::InvalidClientId), "{id:?}");
            }
        }
        // Only the two valid ids reached the backend.
        assert_eq!(r.web.logins.load(SeqCst), 2);
    }

    #[test]
    fn login_error_classification() {
        use anyhow::anyhow;
        let t = anyhow::Error::new(LoginError::Timeout).context("no callback within 120s");
        assert_eq!(classify_login_error(&t), AuthError::LoginTimedOut);
        let d = anyhow::Error::new(LoginError::Denied).context("Spotify reported an error: x");
        assert_eq!(classify_login_error(&d), AuthError::LoginDenied);
        let p = anyhow::Error::new(LoginError::PortInUse);
        assert_eq!(classify_login_error(&p), AuthError::PortInUse);
        assert_eq!(
            classify_login_error(&anyhow!("login failed: whatever")),
            AuthError::LoginFailed
        );
    }

    #[tokio::test]
    async fn status_json_snapshot_and_no_secrets() {
        let r = rig();
        let json = |m: &AuthManager| serde_json::to_string(&m.status()).unwrap();
        assert_eq!(
            json(&r.mgr),
            r#"{"playback":{"state":"loggedOut"},"web":{"state":"loggedOut"},"webClientIdSet":false}"#
        );
        seed_both(&r);
        r.store
            .set(
                accounts::PLAYBACK_REFRESH,
                &Secret::new("AQDsupersecretrefresh"),
            )
            .unwrap();
        tok(&r.mgr.playback()).await.unwrap();
        let s = json(&r.mgr);
        assert_eq!(
            s,
            r#"{"playback":{"state":"loggedIn"},"web":{"state":"loggedIn"},"webClientIdSet":true}"#
        );
        r.store.set_failing(true);
        r.mgr.web().force_refresh().await.unwrap_err();
        let s = json(&r.mgr);
        assert!(
            s.contains(r#""web":{"state":"error","message":"credential store access failed"}"#),
            "{s}"
        );
        for secret in ["supersecret", "AQD", "-access-", "refresh-0", "Bearer", ID] {
            assert!(!s.contains(secret), "{secret} leaked in {s}");
        }
    }

    #[test]
    fn web_client_id_is_returned_only_when_valid() {
        let r = rig();
        assert_eq!(r.mgr.web_client_id(), Ok(None));
        r.store
            .set(accounts::WEB_CLIENT_ID, &Secret::new(ID))
            .unwrap();
        assert_eq!(r.mgr.web_client_id(), Ok(Some(ID.to_owned())));
        for bad in ["nothex", "0123456789abcdef0123456789abcde", "<script>"] {
            r.store
                .set(accounts::WEB_CLIENT_ID, &Secret::new(bad))
                .unwrap();
            assert_eq!(r.mgr.web_client_id(), Ok(None), "{bad}");
        }
        r.store.set_failing(true);
        assert_eq!(r.mgr.web_client_id(), Err(AuthError::CredentialStore));
    }

    #[test]
    fn error_texts_are_fixed_and_carry_no_values() {
        let e = AuthError::RateLimited {
            retry_after_secs: Some(26),
        };
        assert!(!e.to_string().contains("26"));
        assert!(
            !format!("{:?}", AuthError::CredentialStore)
                .to_lowercase()
                .contains("token")
        );
    }

    fn set_scopes(f: &Fake, scopes: &[&str]) {
        *f.scopes.lock().unwrap() = scopes.iter().map(|s| (*s).to_owned()).collect();
    }

    /// S2: a token response without a `scope` field leaves the granted scopes unknown, not empty.
    #[tokio::test]
    async fn an_absent_scope_field_means_unknown() {
        let r = rig();
        seed_both(&r);
        let web = r.mgr.web();
        assert_eq!(web.has_scope("user-library-modify"), None);
        tok(&web).await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), None);
        // A response that names scopes makes them known.
        set_scopes(&r.web, &["user-library-read", "user-library-modify"]);
        web.force_refresh().await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), Some(true));
        assert_eq!(web.has_scope("playlist-read-private"), Some(false));
        // A later response without the field returns to unknown.
        set_scopes(&r.web, &[]);
        web.force_refresh().await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), None);
    }

    /// S3: a login with new scopes replaces the granted list; logout clears it; a refresh failure
    /// that is not invalid_grant keeps it.
    #[tokio::test]
    async fn a_login_replaces_the_granted_scopes_and_logout_clears_them() {
        let r = rig();
        seed_both(&r);
        let web = r.mgr.web();
        set_scopes(&r.web, &["user-library-read"]);
        tok(&web).await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), Some(false));
        // A transient refresh error changes nothing.
        r.web
            .refresh_errors
            .lock()
            .unwrap()
            .push_back(TokenError::Network);
        assert!(web.force_refresh().await.is_err());
        assert_eq!(web.has_scope("user-library-read"), Some(true));
        // The re-login names the new scope.
        set_scopes(&r.web, &["user-library-read", "user-library-modify"]);
        r.mgr.login_web(ID).await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), Some(true));
        // `forget_scope` drops it from a known list, and makes an unknown list known-empty.
        web.forget_scope("user-library-modify");
        assert_eq!(web.has_scope("user-library-modify"), Some(false));
        assert_eq!(web.has_scope("user-library-read"), Some(true));
        r.mgr.logout_web().await.unwrap();
        assert_eq!(web.has_scope("user-library-read"), None);
        web.forget_scope("user-library-modify");
        assert_eq!(web.has_scope("user-library-modify"), Some(false));
    }

    /// An invalid_grant (the stored login is dead) forgets the granted scopes too.
    #[tokio::test]
    async fn invalid_grant_clears_the_granted_scopes() {
        let r = rig();
        seed_both(&r);
        let web = r.mgr.web();
        set_scopes(&r.web, &["user-library-modify"]);
        tok(&web).await.unwrap();
        assert_eq!(web.has_scope("user-library-modify"), Some(true));
        r.web
            .refresh_errors
            .lock()
            .unwrap()
            .push_back(TokenError::InvalidGrant);
        assert_eq!(
            web.force_refresh().await.map(|_| ()),
            Err(AuthError::NotLoggedIn)
        );
        assert_eq!(web.has_scope("user-library-modify"), None);
    }

    // The type system keeps the two logins apart: see the `compile_fail` doctest on
    // `PlayerHost::new` (a `WebTokens` cannot construct a `PlayerHost`).
}
