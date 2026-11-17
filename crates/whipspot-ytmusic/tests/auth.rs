//! The logins of the YouTube Music provider and its InnerTube client: the cookies file, the
//! stream helper's states, `SAPISIDHASH`, and that no secret reaches a text. A fake store, `Http`
//! and `Runner` and an empty `ToolEnv` stand in for the machine: nothing is fetched or run.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use pgp::composed::{
    ArmorOptions, DetachedSignature, KeyType, SecretKeyParamsBuilder, SignedPublicKey,
    SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::ser::Serialize;
use pgp::types::Password;
use rand::thread_rng;
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use whipspot_core::credential_store::{CredentialStore, StoreError};
use whipspot_core::engine::{Engine, EngineConfig};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{AuthStepStatus, LoginInput, Provider, ProviderAuthStatus};
use whipspot_core::secret::Secret;
use whipspot_testkit::{MemoryStore, run_provider_contract, run_provider_playback_contract};
use whipspot_ytmusic::auth::{
    ACCOUNT_PREFIX, COOKIES_ACCOUNT, COOKIES_COUNT_ACCOUNT, COOKIES_MAX_PARTS, COOKIES_PART_CHARS,
    HELPER_OFF_FILE, INPUT_PATH, MSG_COOKIES_EXPIRED, MSG_INSTALL_YOURSELF, STEP_COOKIES,
    STEP_HELPER, cookies_part_account,
};
use whipspot_ytmusic::cookies::{Jar, MAX_FILE_BYTES};
use whipspot_ytmusic::http::{Cap, Http, HttpError, Response};
use whipspot_ytmusic::innertube::{InnerTube, TubeError, client_version, sapisid_hash};
use whipspot_ytmusic::run::{Output, RunError, Runner};
use whipspot_ytmusic::tools::{
    Managed, QJS_RELEASE_BASE, QJS_VERSION, ToolEnv, Trust, YTDLP_RELEASE_BASE,
};
use whipspot_ytmusic::{YtDeps, YtMusicProvider};

const SENTINEL_COOKIE: &str = "SENTINEL-COOKIE-VALUE-123";
const SENTINEL_SAPISID: &str = "SENTINEL-SAPISID-456";
const YTDLP_FILE: &str = if cfg!(windows) {
    "yt-dlp.exe"
} else {
    "yt-dlp"
};
const T0_SECS: u64 = 1_800_000_000;

// ---- fakes ------------------------------------------------------------------------------

/// One POST the fake saw.
#[derive(Debug, Clone)]
struct Post {
    url: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Post {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct FakeHttp {
    gets: Mutex<HashMap<String, Response>>,
    gets_seen: Mutex<Vec<String>>,
    /// The answers to POSTs, in order; the last repeats.
    post_status: Mutex<Vec<u16>>,
    posts: Mutex<Vec<Post>>,
}

impl FakeHttp {
    fn serve(&self, url: &str, body: &[u8]) {
        self.gets.lock().unwrap().insert(
            url.to_owned(),
            Response {
                status: 200,
                body: body.to_vec(),
            },
        );
    }

    fn posts(&self) -> Vec<Post> {
        self.posts.lock().unwrap().clone()
    }

    fn gets_seen(&self) -> Vec<String> {
        self.gets_seen.lock().unwrap().clone()
    }

    fn answer_posts_with(&self, statuses: &[u16]) {
        *self.post_status.lock().unwrap() = statuses.to_vec();
    }
}

impl Http for FakeHttp {
    fn get(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _range: Option<(u64, u64)>,
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        self.gets_seen.lock().unwrap().push(url.to_owned());
        self.gets
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or(HttpError::Network)
    }

    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        self.posts.lock().unwrap().push(Post {
            url: url.to_owned(),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: serde_json::from_slice(body).unwrap(),
        });
        let mut statuses = self.post_status.lock().unwrap();
        let status = if statuses.len() > 1 {
            statuses.remove(0)
        } else {
            statuses.first().copied().unwrap_or(200)
        };
        Ok(Response {
            status,
            body: br#"{"ok":true}"#.to_vec(),
        })
    }
}

/// `yt-dlp --version` answers "2026.10.26".
struct FakeRunner {
    calls: Mutex<Vec<Vec<String>>>,
}

impl Runner for FakeRunner {
    fn run(
        &self,
        _program: &Path,
        args: &[OsString],
        _timeout: Duration,
    ) -> Result<Output, RunError> {
        self.calls.lock().unwrap().push(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        );
        Ok(Output {
            code: Some(0),
            stdout: b"2026.10.26\n".to_vec(),
            stderr: Vec::new(),
        })
    }
}

struct TestKey {
    secret: SignedSecretKey,
    armored_public: String,
}

fn test_key() -> TestKey {
    let mut params = SecretKeyParamsBuilder::default();
    params
        .key_type(KeyType::Ed25519Legacy)
        .can_certify(true)
        .can_sign(true)
        .primary_user_id("Whipspot test key <test@example.invalid>".into());
    let secret = params.build().unwrap().generate(thread_rng()).unwrap();
    let armored_public = SignedPublicKey::from(secret.clone())
        .to_armored_string(ArmorOptions::default())
        .unwrap();
    TestKey {
        secret,
        armored_public,
    }
}

fn sign(key: &TestKey, data: &[u8]) -> Vec<u8> {
    DetachedSignature::sign_binary_data(
        thread_rng(),
        &key.secret.primary_key,
        &Password::empty(),
        HashAlgorithm::Sha256,
        data,
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---- a store with the Windows limit -----------------------------------------------------

/// The most bytes of one value the Windows Credential Manager takes (as UTF-16).
const WINDOWS_BLOB_LIMIT: usize = 2560;

/// A [`MemoryStore`] that refuses a value over [`WINDOWS_BLOB_LIMIT`] as UTF-16 (as the real
/// Windows store does) and can be made to fail one account's write.
#[derive(Default)]
struct WindowsLikeStore {
    inner: MemoryStore,
    fail_set_of: Mutex<Option<String>>,
}

impl WindowsLikeStore {
    fn fail_set_of(&self, account: &str) {
        *self.fail_set_of.lock().unwrap() = Some(account.to_owned());
    }

    fn stop_failing(&self) {
        *self.fail_set_of.lock().unwrap() = None;
    }
}

impl CredentialStore for WindowsLikeStore {
    fn get(&self, account: &str) -> Result<Option<Secret>, StoreError> {
        self.inner.get(account)
    }

    fn set(&self, account: &str, value: &Secret) -> Result<(), StoreError> {
        if value.expose().encode_utf16().count() * 2 > WINDOWS_BLOB_LIMIT {
            return Err(StoreError);
        }
        if self.fail_set_of.lock().unwrap().as_deref() == Some(account) {
            return Err(StoreError);
        }
        self.inner.set(account, value)
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        self.inner.delete(account)
    }
}

/// Every account of the cookies the store holds: the count, the parts (a few past the most a set
/// can have) and the account of the first build.
fn cookie_accounts(store: &WindowsLikeStore) -> Vec<String> {
    let mut names = vec![COOKIES_ACCOUNT.to_owned(), COOKIES_COUNT_ACCOUNT.to_owned()];
    names.extend((0..COOKIES_MAX_PARTS + 8).map(cookies_part_account));
    names.retain(|n| store.get(n).unwrap().is_some());
    names
}

fn assert_no_cookie_accounts(store: &WindowsLikeStore) {
    assert_eq!(cookie_accounts(store), Vec::<String>::new());
}

/// The cookies as stored: the parts, joined.
fn stored_cookies(store: &WindowsLikeStore) -> String {
    let count: usize = store
        .get(COOKIES_COUNT_ACCOUNT)
        .unwrap()
        .expect("a count")
        .expose()
        .parse()
        .unwrap();
    (0..count)
        .map(|i| {
            store
                .get(&cookies_part_account(i))
                .unwrap()
                .expect("a part")
                .expose()
                .to_owned()
        })
        .collect()
}

// ---- the rig ----------------------------------------------------------------------------

struct Rig {
    dir: tempfile::TempDir,
    store: Arc<WindowsLikeStore>,
    http: Arc<FakeHttp>,
    runner: Arc<FakeRunner>,
    provider: Arc<YtMusicProvider>,
}

#[derive(Clone, Copy, PartialEq)]
enum Env {
    /// Finds nothing; the compiled-in pins (managed install on this target).
    Empty,
    /// Finds nothing, and the target has no managed install.
    Unsupported,
    /// A yt-dlp of the user's is on PATH.
    OwnYtDlp,
    /// `WHIPSPOT_YTDLP` names a file that is not there.
    BadEnvPath,
    /// Finds nothing; the install is a fake one: a test key and test assets, served by the fake
    /// `Http`.
    FakeRelease,
}

impl Rig {
    fn new(kind: Env) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools");
        let http = Arc::new(FakeHttp::default());
        let mut env = ToolEnv::new(tools_dir);
        match kind {
            Env::Empty => {}
            Env::Unsupported => env.managed = None,
            Env::BadEnvPath => {
                env.env_path = Some(dir.path().join("no-such-yt-dlp"));
            }
            Env::OwnYtDlp => {
                let own = dir.path().join("their-yt-dlp");
                std::fs::write(&own, b"x").unwrap();
                env.path_lookup = Box::new(move |_| Some(own.clone()));
            }
            Env::FakeRelease => {
                let key = test_key();
                let ytdlp = b"yt-dlp test build".to_vec();
                let qjs = b"qjs test build".to_vec();
                let sums = format!("{}  yt-dlp-test\n", hex(&Sha256::digest(&ytdlp)));
                http.serve(
                    &format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS"),
                    sums.as_bytes(),
                );
                http.serve(
                    &format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS.sig"),
                    &sign(&key, sums.as_bytes()),
                );
                http.serve(&format!("{YTDLP_RELEASE_BASE}/yt-dlp-test"), &ytdlp);
                http.serve(&format!("{QJS_RELEASE_BASE}/qjs-test"), &qjs);
                env.trust = Trust::Custom(key.armored_public.clone());
                env.managed = Some(Managed {
                    ytdlp_asset: "yt-dlp-test".into(),
                    qjs_asset: "qjs-test".into(),
                    qjs_sha256: Sha256::digest(&qjs).into(),
                });
            }
        }
        let store = Arc::new(WindowsLikeStore::default());
        let runner = Arc::new(FakeRunner {
            calls: Mutex::new(Vec::new()),
        });
        let provider = Arc::new(YtMusicProvider::new(YtDeps {
            http: http.clone(),
            runner: runner.clone(),
            env,
            store: store.clone(),
            source: Engine::new(EngineConfig::default(), None).source,
            events: tokio::sync::broadcast::channel(16).0,
        }));
        Rig {
            dir,
            store,
            http,
            runner,
            provider,
        }
    }

    fn tools_dir(&self) -> PathBuf {
        self.dir.path().join("tools")
    }

    /// Writes a file under the rig's scratch folder and returns its path.
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    async fn status(&self) -> ProviderAuthStatus {
        self.provider.auth().status().await.unwrap()
    }

    async fn step(&self, id: &str) -> AuthStepStatus {
        self.status()
            .await
            .steps
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
    }

    async fn login_cookies(&self, path: &Path) -> Result<ProviderAuthStatus, String> {
        let mut input = LoginInput::default();
        input
            .0
            .insert(INPUT_PATH.to_owned(), path.to_string_lossy().into_owned());
        self.provider
            .auth()
            .login(STEP_COOKIES, &input)
            .await
            .map_err(|e| e.0)
    }

    async fn login_helper(&self) -> Result<ProviderAuthStatus, String> {
        self.provider
            .auth()
            .login(STEP_HELPER, &LoginInput::default())
            .await
            .map_err(|e| e.0)
    }
}

/// A cookies.txt: a login, the user's other sites, junk lines.
fn cookies_txt(sapisid_line: &str) -> String {
    format!(
        "# Netscape HTTP Cookie File\n\
         # This is a generated file!\n\
         {sapisid_line}\n\
         #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1900000000\t__Secure-3PSID\t{SENTINEL_COOKIE}\n\
         music.youtube.com\tFALSE\t/\tTRUE\t0\tVISITOR\tv1\n\
         accounts.youtube.com\tFALSE\t/\tTRUE\t0\tHOSTONLYELSEWHERE\tz\n\
         .google.com\tTRUE\t/\tTRUE\t0\tSID\tOTHER-DOMAIN-SECRET\n\
         .evilyoutube.com\tTRUE\t/\tTRUE\t0\tX\tevil-lookalike\n\
         .youtube.com.evil.example\tTRUE\t/\tTRUE\t0\tY\tevil-suffix\n\
         .youtube.com\tTRUE\t/\tTRUE\t1\tEXPIRED\told\n\
         not a cookie line\n\
         .youtube.com\tTRUE\t/\tTRUE\t0\tBADVALUE\ta;b\n\
         .youtube.com\tTRUE\t/\tTRUE\t0\tonly\tsix\tfields\textra\textra\textra\n"
    )
}

fn sapisid_line(value: &str) -> String {
    format!(".youtube.com\tTRUE\t/\tTRUE\t1900000000\tSAPISID\t{value}")
}

fn good_cookies() -> String {
    cookies_txt(&sapisid_line(SENTINEL_SAPISID))
}

// ---- contract 5: the auth cases -----------------------------------------------------------

#[tokio::test]
async fn the_provider_contract_passes_on_the_auth_only_provider() {
    let rig = Rig::new(Env::Empty);
    let info = rig.provider.info();
    assert_eq!(info.id, "ytmusic");
    assert_eq!(info.name, "YouTube Music");
    assert!(info.caps.library && rig.provider.library().is_some());
    assert_eq!(
        info.auth_steps.iter().map(|s| s.id).collect::<Vec<_>>(),
        ["helper", "cookies"]
    );
    assert_eq!(info.auth_steps[1].inputs[0].max_len, 1024);
    run_provider_contract(rig.provider.clone()).await;
    // Playback with nothing set up: a load fails with a fixed text, a transport call is a no-op.
    run_provider_playback_contract(rig.provider.clone(), None, "ytmusic:track:dQw4w9WgXcQ").await;
    let err = rig
        .provider
        .playback()
        .load("x", true, 0)
        .await
        .unwrap_err();
    assert_eq!(err.0, "this is not a YouTube Music track");
    let err = rig
        .provider
        .playback()
        .load("ytmusic:track:dQw4w9WgXcQ", true, 0)
        .await
        .unwrap_err();
    // (The provider contract above logged every step out, so the helper is off.)
    assert_eq!(err.0, "Turn on the stream helper in Accounts first");
    rig.provider.playback().seek(0).await.unwrap();
}

#[tokio::test]
async fn every_step_starts_logged_out() {
    let rig = Rig::new(Env::Empty);
    let status = rig.status().await;
    assert_eq!(status.steps.len(), 2);
    assert!(
        status
            .steps
            .iter()
            .all(|s| s.state == LoginState::LoggedOut)
    );
    let helper = &status.steps[0];
    if cfg!(any(windows, target_os = "macos")) {
        assert_eq!(helper.action.as_deref(), Some("Install"));
        assert_eq!(helper.detail, None);
    }
    assert_eq!(status.steps[1].action, None);
    assert_eq!(rig.status().await, status, "the same twice");
    assert!(
        rig.runner.calls.lock().unwrap().is_empty(),
        "nothing was run"
    );
}

#[tokio::test]
async fn a_set_path_that_is_not_a_file_says_so_and_offers_no_install() {
    let rig = Rig::new(Env::BadEnvPath);
    let helper = rig.step(STEP_HELPER).await;
    assert_eq!(helper.state, LoginState::LoggedOut);
    assert_eq!(helper.action, None, "an install cannot change the set path");
    assert_eq!(
        helper.detail.as_deref(),
        Some("WHIPSPOT_YTDLP does not name a yt-dlp file")
    );
    assert!(rig.http.gets_seen().is_empty(), "nothing was fetched");
}

#[tokio::test]
async fn a_target_without_a_managed_install_says_so() {
    let rig = Rig::new(Env::Unsupported);
    let helper = rig.step(STEP_HELPER).await;
    assert_eq!(helper.state, LoginState::LoggedOut);
    assert_eq!(helper.action, None);
    assert_eq!(helper.detail.as_deref(), Some("Install yt-dlp yourself"));
    assert_eq!(MSG_INSTALL_YOURSELF, "Install yt-dlp yourself");
    assert_eq!(
        rig.login_helper().await.unwrap_err(),
        "Install yt-dlp yourself"
    );
    // The failure is shown as the state, with the same text.
    assert_eq!(
        rig.step(STEP_HELPER).await.state,
        LoginState::Error {
            message: "Install yt-dlp yourself".into()
        }
    );
}

#[tokio::test]
async fn a_missing_cookies_file_is_refused_and_everything_stays_logged_out() {
    let rig = Rig::new(Env::Empty);
    let missing = rig.dir.path().join("no-such-cookies.txt");
    assert_eq!(
        rig.login_cookies(&missing).await.unwrap_err(),
        "Cookie file not found"
    );
    assert!(
        rig.status()
            .await
            .steps
            .iter()
            .all(|s| s.state == LoginState::LoggedOut)
    );
    assert_no_cookie_accounts(&rig.store);
    // No path, a directory, a control character, a path too long.
    let mut input = LoginInput::default();
    assert!(
        rig.provider
            .auth()
            .login(STEP_COOKIES, &input)
            .await
            .is_err()
    );
    for bad in [
        "",
        "   ",
        "\"\"",
        "a\u{0}b",
        "line\nbreak",
        &"x".repeat(2000),
    ] {
        input.0.insert(INPUT_PATH.into(), bad.to_owned());
        assert!(
            rig.provider
                .auth()
                .login(STEP_COOKIES, &input)
                .await
                .is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(
        rig.login_cookies(rig.dir.path()).await.unwrap_err(),
        "That path is not a file"
    );
    assert_no_cookie_accounts(&rig.store);
}

#[tokio::test]
async fn helper_then_cookies_leave_both_logged_in_and_status_matches_the_last_answer() {
    let rig = Rig::new(Env::FakeRelease);
    let after_helper = rig.login_helper().await.unwrap();
    let helper = &after_helper.steps[0];
    assert_eq!(helper.state, LoginState::LoggedIn);
    // The managed copy: its version and the runtime's, and an Update button.
    assert_eq!(
        helper.detail.as_deref(),
        Some(format!("yt-dlp 2026.10.26, QuickJS-ng {QJS_VERSION}").as_str())
    );
    assert_eq!(helper.action.as_deref(), Some("Update"));
    assert_eq!(after_helper.steps[1].state, LoginState::LoggedOut);
    assert!(rig.tools_dir().join(YTDLP_FILE).is_file());

    let file = rig.file("cookies.txt", &good_cookies());
    let after_cookies = rig.login_cookies(&file).await.unwrap();
    assert!(
        after_cookies
            .steps
            .iter()
            .all(|s| s.state == LoginState::LoggedIn)
    );
    assert_eq!(rig.status().await, after_cookies);
    // The helper's version is read once per file, not on every status.
    assert_eq!(rig.runner.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn logging_out_both_leaves_both_logged_out_and_deletes_no_binary() {
    let rig = Rig::new(Env::FakeRelease);
    rig.login_helper().await.unwrap();
    rig.login_cookies(&rig.file("c.txt", &good_cookies()))
        .await
        .unwrap();

    let after = rig.provider.auth().logout(STEP_HELPER).await.unwrap();
    assert_eq!(after.steps[0].state, LoginState::LoggedOut);
    assert_eq!(after.steps[1].state, LoginState::LoggedIn);
    assert!(rig.tools_dir().join(HELPER_OFF_FILE).is_file());
    assert!(
        rig.tools_dir().join(YTDLP_FILE).is_file(),
        "no binary is deleted"
    );

    let after = rig.provider.auth().logout(STEP_COOKIES).await.unwrap();
    assert!(after.steps.iter().all(|s| s.state == LoginState::LoggedOut));
    assert_no_cookie_accounts(&rig.store);
    assert_eq!(rig.status().await, after);

    // The next login removes the flag; the managed copy is still there and only updated.
    let again = rig.login_helper().await.unwrap();
    assert_eq!(again.steps[0].state, LoginState::LoggedIn);
    assert!(!rig.tools_dir().join(HELPER_OFF_FILE).exists());
}

#[tokio::test]
async fn an_unknown_step_or_input_is_an_error_with_fixed_text() {
    let rig = Rig::new(Env::Empty);
    let login = rig
        .provider
        .auth()
        .login("nope", &LoginInput::default())
        .await
        .unwrap_err();
    let logout = rig.provider.auth().logout("nope").await.unwrap_err();
    assert_eq!(login.0, "Unknown login step");
    assert_eq!(logout.0, "Unknown login step");

    let mut extra = LoginInput::default();
    extra.0.insert("client_id".into(), "x".into());
    assert_eq!(
        rig.provider
            .auth()
            .login(STEP_HELPER, &extra)
            .await
            .unwrap_err()
            .0,
        "Unknown login input"
    );
    extra.0.insert(INPUT_PATH.into(), "x".into());
    assert_eq!(
        rig.provider
            .auth()
            .login(STEP_COOKIES, &extra)
            .await
            .unwrap_err()
            .0,
        "Unknown login input"
    );
    assert!(rig.http.gets_seen().is_empty() && rig.runner.calls.lock().unwrap().is_empty());
}

// ---- the helper step -----------------------------------------------------------------------

#[tokio::test]
async fn the_users_own_yt_dlp_is_used_and_nothing_is_downloaded() {
    let rig = Rig::new(Env::OwnYtDlp);
    let before = rig.step(STEP_HELPER).await;
    assert_eq!(
        before.state,
        LoginState::LoggedIn,
        "a yt-dlp that resolves is logged in"
    );
    assert_eq!(before.detail.as_deref(), Some("Your yt-dlp 2026.10.26"));
    assert_eq!(before.action, None);
    let after = rig.login_helper().await.unwrap();
    assert_eq!(after.steps[0], before);
    // Not the checksum list and no binary: the user's copy is theirs.
    assert!(rig.http.gets_seen().is_empty());
    assert!(!rig.tools_dir().join(YTDLP_FILE).exists());
}

#[tokio::test]
async fn a_failed_install_is_an_error_with_a_fixed_text_until_one_works() {
    // The fake `Http` has nothing to serve.
    let rig = Rig::new(Env::Empty);
    let err = rig.login_helper().await.unwrap_err();
    assert_eq!(err, "Download failed: Network error");
    let helper = rig.step(STEP_HELPER).await;
    assert_eq!(
        helper.state,
        LoginState::Error {
            message: err.clone()
        }
    );
    assert_eq!(helper.action.as_deref(), Some("Install"));
    // Switching it off clears the error.
    rig.provider.auth().logout(STEP_HELPER).await.unwrap();
    assert_eq!(rig.step(STEP_HELPER).await.state, LoginState::LoggedOut);
}

#[tokio::test]
async fn the_helper_off_flag_is_what_the_provider_reads() {
    let rig = Rig::new(Env::OwnYtDlp);
    assert!(rig.provider.auth_state().helper_enabled());
    rig.provider.auth().logout(STEP_HELPER).await.unwrap();
    assert!(!rig.provider.auth_state().helper_enabled());
    assert_eq!(rig.step(STEP_HELPER).await.state, LoginState::LoggedOut);
    rig.login_helper().await.unwrap();
    assert!(rig.provider.auth_state().helper_enabled());
}

// ---- contracts 2 and 3: the cookies file ----------------------------------------------------

#[tokio::test]
async fn a_file_without_a_login_cookie_is_refused_and_nothing_is_stored() {
    let rig = Rig::new(Env::Empty);
    let lines = [
        // no SAPISID at all
        cookies_txt(".youtube.com\tTRUE\t/\tTRUE\t0\tVISITOR\tv"),
        // one on a foreign domain
        cookies_txt(".google.com\tTRUE\t/\tTRUE\t0\tSAPISID\tforeign"),
        cookies_txt(".evilyoutube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tlookalike"),
        // an empty value
        cookies_txt(".youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\t"),
        // empty and comments only
        String::new(),
        "# Netscape HTTP Cookie File\n".to_owned(),
    ];
    for text in lines {
        let file = rig.file("c.txt", &text);
        assert_eq!(
            rig.login_cookies(&file).await.unwrap_err(),
            "No YouTube login in that file",
            "{text:?}"
        );
        assert_no_cookie_accounts(&rig.store);
        assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedOut);
    }
    // Not text at all.
    let bin = rig.dir.path().join("bin");
    std::fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(
        rig.login_cookies(&bin).await.unwrap_err(),
        "That is not a cookies.txt file"
    );
}

#[tokio::test]
async fn a_secure_3papisid_alone_is_a_login_and_http_only_lines_count() {
    let rig = Rig::new(Env::Empty);
    let line = format!(
        "#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1900000000\t__Secure-3PAPISID\t{SENTINEL_SAPISID}"
    );
    let file = rig.file("c.txt", &cookies_txt(&line));
    rig.login_cookies(&file).await.unwrap();
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);
}

#[tokio::test]
async fn only_youtube_cookies_are_stored_and_the_users_file_is_untouched() {
    let rig = Rig::new(Env::Empty);
    let text = good_cookies();
    let file = rig.file("cookies.txt", &text);
    // Read-only on disk: a write or a delete of the user's file would fail or show.
    let mut perms = std::fs::metadata(&file).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&file, perms).unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();

    rig.login_cookies(&file).await.unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    let stored = stored_cookies(&rig.store);
    let stored = stored.as_str();
    // Only ytmusic- accounts: the count and one part.
    assert_eq!(
        cookie_accounts(&rig.store),
        [COOKIES_COUNT_ACCOUNT, "ytmusic-cookies-0"]
    );
    assert!(COOKIES_COUNT_ACCOUNT.starts_with(ACCOUNT_PREFIX));
    // Kept: the YouTube cookies (including the HttpOnly one).
    for kept in [
        "SAPISID",
        "__Secure-3PSID",
        "VISITOR",
        "HOSTONLYELSEWHERE",
        SENTINEL_COOKIE,
    ] {
        assert!(stored.contains(kept), "{kept} is kept");
    }
    // Dropped: other domains, lookalikes, junk, a value with a `;`, a line with the wrong fields.
    for dropped in [
        "OTHER-DOMAIN-SECRET",
        "evil-lookalike",
        "evil-suffix",
        "BADVALUE",
        "only",
        "junk",
    ] {
        assert!(!stored.contains(dropped), "{dropped} is dropped");
    }
    // What is stored reads back to a login.
    assert!(
        Jar::from_secret(&Secret::new(stored))
            .unwrap()
            .sapisid()
            .is_some()
    );
    // Quoted path ("Copy as path") works too.
    let quoted = format!("\"{}\"", file.display());
    let mut input = LoginInput::default();
    input.0.insert(INPUT_PATH.into(), quoted);
    rig.provider
        .auth()
        .login(STEP_COOKIES, &input)
        .await
        .unwrap();
}

// ---- the cookies in parts (the Windows blob limit) ---------------------------------------------

/// A cookies.txt of at least `extra` bytes: the good cookies and YouTube cookies of 300
/// characters.
fn big_cookies(extra: usize) -> String {
    let mut text = good_cookies();
    let mut n = 0;
    while text.len() < extra {
        text.push_str(&format!(
            ".youtube.com\tTRUE\t/\tTRUE\t1900000000\tBIG{n}\t{}\n",
            "v".repeat(300)
        ));
        n += 1;
    }
    text
}

#[tokio::test]
async fn six_kilobytes_of_cookies_are_stored_in_parts_and_read_back_whole() {
    let rig = Rig::new(Env::Empty);
    let file = rig.file("big.txt", &big_cookies(6000));
    let before = rig.store.get(COOKIES_COUNT_ACCOUNT).unwrap();
    assert!(before.is_none());
    let status = rig.login_cookies(&file).await.unwrap();
    assert_eq!(status.steps[1].state, LoginState::LoggedIn);

    let count: usize = rig
        .store
        .get(COOKIES_COUNT_ACCOUNT)
        .unwrap()
        .unwrap()
        .expose()
        .parse()
        .unwrap();
    assert!(count >= 6, "{count} parts");
    // Every part is within the character limit, and the accounts are the count and the parts.
    for i in 0..count {
        let part = rig.store.get(&cookies_part_account(i)).unwrap().unwrap();
        assert!(part.expose().chars().count() <= COOKIES_PART_CHARS);
    }
    assert_eq!(cookie_accounts(&rig.store).len(), count + 1);

    // The parts joined are the jar the file makes, and the provider reads it back.
    let expected = Jar::parse(big_cookies(6000).as_bytes())
        .unwrap()
        .to_secret();
    assert_eq!(stored_cookies(&rig.store), expected.expose());
    let got = rig.provider.auth_state().cookies_secret().unwrap().unwrap();
    assert_eq!(got.expose(), expected.expose());
    assert!(rig.provider.auth_state().cookies().unwrap().is_some());
    assert!(rig.provider.auth_state().cookies_usable());
}

#[tokio::test]
async fn a_store_failure_on_a_part_leaves_nothing_behind() {
    let rig = Rig::new(Env::Empty);
    let file = rig.file("big.txt", &big_cookies(6000));
    rig.store.fail_set_of(&cookies_part_account(3));
    assert_eq!(
        rig.login_cookies(&file).await.unwrap_err(),
        "Could not use the credential store"
    );
    assert_no_cookie_accounts(&rig.store);
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedOut);

    // The same on the count, written last.
    rig.store.fail_set_of(COOKIES_COUNT_ACCOUNT);
    assert!(rig.login_cookies(&file).await.is_err());
    assert_no_cookie_accounts(&rig.store);

    // A failure while replacing a set leaves neither the old set nor the new one.
    rig.store.stop_failing();
    rig.login_cookies(&file).await.unwrap();
    rig.store.fail_set_of(&cookies_part_account(2));
    assert!(rig.login_cookies(&file).await.is_err());
    assert_no_cookie_accounts(&rig.store);
}

#[tokio::test]
async fn logout_leaves_no_cookie_account_not_even_the_first_builds() {
    let rig = Rig::new(Env::Empty);
    rig.login_cookies(&rig.file("big.txt", &big_cookies(6000)))
        .await
        .unwrap();
    rig.store
        .set(COOKIES_ACCOUNT, &Secret::new("one big value"))
        .unwrap();
    // Parts past the count, as a crash could leave them.
    rig.store
        .set(&cookies_part_account(40), &Secret::new("stray"))
        .unwrap();
    let after = rig.provider.auth().logout(STEP_COOKIES).await.unwrap();
    assert_eq!(after.steps[1].state, LoginState::LoggedOut);
    assert_no_cookie_accounts(&rig.store);
    assert!(rig.provider.auth_state().cookies().unwrap().is_none());
}

#[tokio::test]
async fn a_shorter_import_leaves_no_stale_parts() {
    let rig = Rig::new(Env::Empty);
    rig.login_cookies(&rig.file("big.txt", &big_cookies(9000)))
        .await
        .unwrap();
    let big = cookie_accounts(&rig.store).len();
    assert!(big > 8);
    rig.login_cookies(&rig.file("small.txt", &good_cookies()))
        .await
        .unwrap();
    assert_eq!(
        cookie_accounts(&rig.store),
        [COOKIES_COUNT_ACCOUNT, "ytmusic-cookies-0"]
    );
    assert_eq!(
        stored_cookies(&rig.store),
        Jar::parse(good_cookies().as_bytes())
            .unwrap()
            .to_secret()
            .expose()
    );

    // With the count unreadable the old parts are still found and deleted.
    rig.login_cookies(&rig.file("big.txt", &big_cookies(9000)))
        .await
        .unwrap();
    rig.store
        .set(COOKIES_COUNT_ACCOUNT, &Secret::new("garbage"))
        .unwrap();
    rig.login_cookies(&rig.file("small.txt", &good_cookies()))
        .await
        .unwrap();
    assert_eq!(
        cookie_accounts(&rig.store),
        [COOKIES_COUNT_ACCOUNT, "ytmusic-cookies-0"]
    );
}

#[tokio::test]
async fn a_missing_part_or_a_bad_count_reads_as_logged_out() {
    let rig = Rig::new(Env::Empty);
    let file = rig.file("big.txt", &big_cookies(6000));
    rig.login_cookies(&file).await.unwrap();
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);

    // One part gone: nothing is read, not the parts before it.
    rig.store.delete(&cookies_part_account(1)).unwrap();
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedOut);
    assert!(
        rig.provider
            .auth_state()
            .cookies_secret()
            .unwrap()
            .is_none()
    );
    assert!(rig.provider.auth_state().cookies().unwrap().is_none());
    assert!(!rig.provider.auth_state().cookies_usable());

    for bad in [
        "",
        "0",
        "65",
        "-1",
        "1e1",
        "x",
        " 2",
        "99999999999999999999",
    ] {
        rig.login_cookies(&file).await.unwrap();
        // A count that cannot be written as a value is deleted instead.
        if bad.is_empty() {
            rig.store.delete(COOKIES_COUNT_ACCOUNT).unwrap();
        } else {
            rig.store
                .set(COOKIES_COUNT_ACCOUNT, &Secret::new(bad))
                .unwrap();
        }
        assert_eq!(
            rig.step(STEP_COOKIES).await.state,
            LoginState::LoggedOut,
            "count {bad:?}"
        );
        assert!(
            rig.provider
                .auth_state()
                .cookies_secret()
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn more_than_sixty_four_parts_are_refused_and_nothing_is_stored() {
    let rig = Rig::new(Env::Empty);
    // 20 cookies of 4000 characters: over 64,000.
    let mut text = good_cookies();
    for n in 0..20 {
        text.push_str(&format!(
            ".youtube.com\tTRUE\t/\tTRUE\t1900000000\tHUGE{n}\t{}\n",
            "v".repeat(4000)
        ));
    }
    let file = rig.file("huge.txt", &text);
    assert_eq!(
        rig.login_cookies(&file).await.unwrap_err(),
        "That cookies file is too large"
    );
    assert_no_cookie_accounts(&rig.store);

    // A set that is already stored stays when a new one is refused for its size.
    rig.login_cookies(&rig.file("ok.txt", &good_cookies()))
        .await
        .unwrap();
    assert!(rig.login_cookies(&file).await.is_err());
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);
}

// ---- a crashed run's cookies file ------------------------------------------------------------

#[tokio::test]
async fn building_the_provider_removes_what_a_crashed_run_left_in_the_run_folder() {
    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("tools");
    let run = tools.join("run");
    std::fs::create_dir_all(run.join("a-folder")).unwrap();
    std::fs::write(run.join("0123456789abcdef.txt"), SENTINEL_COOKIE).unwrap();
    std::fs::write(run.join("other"), b"x").unwrap();
    std::fs::write(run.join("a-folder").join("inner.txt"), b"x").unwrap();
    // Beside the folder, not in it: untouched.
    std::fs::write(
        tools.join("helper-off"),
        b"off
",
    )
    .unwrap();
    std::fs::write(dir.path().join("neighbour.txt"), b"x").unwrap();

    let _provider = YtMusicProvider::new(YtDeps {
        http: Arc::new(FakeHttp::default()),
        runner: Arc::new(FakeRunner {
            calls: Mutex::new(Vec::new()),
        }),
        env: ToolEnv::new(tools.clone()),
        store: Arc::new(MemoryStore::default()),
        source: Engine::new(EngineConfig::default(), None).source,
        events: tokio::sync::broadcast::channel(16).0,
    });

    assert!(!run.join("0123456789abcdef.txt").exists());
    assert!(!run.join("other").exists());
    // Only regular files: a folder in it, and what is in that, stay.
    assert!(run.join("a-folder").join("inner.txt").is_file());
    assert!(tools.join("helper-off").is_file());
    assert!(dir.path().join("neighbour.txt").is_file());

    // No run folder at all is fine.
    let empty = tempfile::tempdir().unwrap();
    let _provider = YtMusicProvider::new(YtDeps {
        http: Arc::new(FakeHttp::default()),
        runner: Arc::new(FakeRunner {
            calls: Mutex::new(Vec::new()),
        }),
        env: ToolEnv::new(empty.path().join("tools")),
        store: Arc::new(MemoryStore::default()),
        source: Engine::new(EngineConfig::default(), None).source,
        events: tokio::sync::broadcast::channel(16).0,
    });
    assert!(!empty.path().join("tools").exists(), "nothing is created");
}

#[tokio::test]
async fn a_cookies_file_over_one_mib_is_refused() {
    let rig = Rig::new(Env::Empty);
    let mut text = good_cookies();
    text.push_str(&"#".repeat(MAX_FILE_BYTES));
    let file = rig.file("big.txt", &text);
    assert_eq!(
        rig.login_cookies(&file).await.unwrap_err(),
        "That file is too large"
    );
    // Exactly at the limit is read.
    let mut text = good_cookies();
    let pad = MAX_FILE_BYTES - text.len();
    text.push_str(&"#".repeat(pad));
    assert_eq!(text.len(), MAX_FILE_BYTES);
    let file = rig.file("limit.txt", &text);
    rig.login_cookies(&file).await.unwrap();
}

#[test]
fn the_cookie_header_has_what_matches_the_request_and_has_not_expired() {
    let jar = Jar::parse(good_cookies().as_bytes()).unwrap();
    let header = jar.header("music.youtube.com", "/youtubei/v1/browse", T0_SECS);
    let header = header.expose();
    for want in [
        format!("SAPISID={SENTINEL_SAPISID}"),
        format!("__Secure-3PSID={SENTINEL_COOKIE}"),
        "VISITOR=v1".to_owned(),
    ] {
        assert!(header.contains(&want), "{want} in the header");
    }
    // Another host's cookie, and an expired one, are not sent.
    assert!(!header.contains("HOSTONLYELSEWHERE") && !header.contains("EXPIRED"));
    // After the cookies' expiry nothing is sent.
    assert!(
        jar.header("music.youtube.com", "/", 1_950_000_000)
            .expose()
            .contains("VISITOR")
    );
    assert!(
        !jar.header("music.youtube.com", "/", 1_950_000_000)
            .expose()
            .contains("SAPISID")
    );
    // The Debug print of a jar holds nothing.
    assert_eq!(format!("{jar:?}"), "Jar(<redacted>)");
}

// ---- contract 4: SAPISIDHASH ----------------------------------------------------------------

#[test]
fn sapisid_hash_matches_the_ytmusicapi_formula() {
    // ytmusicapi: sha1(f"{unix_timestamp} {auth} https://music.youtube.com"), "SAPISIDHASH ts_hex".
    // The vector below was computed with Python's hashlib.sha1 for ts 1700000000.
    assert_eq!(
        sapisid_hash("abc123_SAPISID", 1_700_000_000),
        "SAPISIDHASH 1700000000_9da0009a88b9d602604dcd6de3a74bb667d7a647"
    );
    // And the formula spelled out again here.
    let digest = Sha1::digest(b"1700000000 abc123_SAPISID https://music.youtube.com");
    assert_eq!(
        sapisid_hash("abc123_SAPISID", 1_700_000_000),
        format!("SAPISIDHASH 1700000000_{}", hex(&digest))
    );
    // A different timestamp or key gives a different hash.
    assert_ne!(
        sapisid_hash("abc123_SAPISID", 1_700_000_001),
        sapisid_hash("abc123_SAPISID", 1_700_000_000)
    );
    assert_ne!(
        sapisid_hash("other", 1_700_000_000),
        sapisid_hash("abc123_SAPISID", 1_700_000_000)
    );
}

#[test]
fn the_client_version_is_the_utc_date_of_the_request() {
    assert_eq!(client_version(0), "1.19700101.01.00");
    assert_eq!(client_version(951_782_400), "1.20000229.01.00");
    assert_eq!(client_version(1_709_164_800), "1.20240229.01.00");
    assert_eq!(client_version(T0_SECS), "1.20270115.01.00");
    // The last second of a day and the first of the next.
    assert_eq!(client_version(1_800_057_599), "1.20270115.01.00");
    assert_eq!(client_version(1_800_057_600), "1.20270116.01.00");
}

// ---- the InnerTube client -------------------------------------------------------------------

fn tube_rig(statuses: &[u16]) -> (Rig, InnerTube) {
    let rig = Rig::new(Env::Empty);
    rig.http.answer_posts_with(statuses);
    let tube = InnerTube::with_clock(
        rig.http.clone(),
        rig.provider.auth_state().clone(),
        Box::new(|| UNIX_EPOCH + Duration::from_secs(T0_SECS)),
    );
    (rig, tube)
}

#[tokio::test]
async fn a_request_carries_the_context_the_cookies_and_the_hash() {
    let (rig, tube) = tube_rig(&[200]);
    rig.login_cookies(&rig.file("c.txt", &good_cookies()))
        .await
        .unwrap();
    let answer = tube
        .post("browse", json!({"browseId": "FEmusic_liked_videos"}))
        .unwrap();
    assert_eq!(answer, json!({"ok": true}));

    let post = &rig.http.posts()[0];
    assert_eq!(
        post.url,
        "https://music.youtube.com/youtubei/v1/browse?alt=json"
    );
    assert_eq!(
        post.header("Authorization"),
        Some(sapisid_hash(SENTINEL_SAPISID, T0_SECS).as_str())
    );
    assert!(
        post.header("Cookie")
            .unwrap()
            .contains(&format!("SAPISID={SENTINEL_SAPISID}"))
    );
    assert_eq!(post.header("X-Origin"), Some("https://music.youtube.com"));
    assert_eq!(post.header("Origin"), Some("https://music.youtube.com"));
    assert_eq!(post.header("X-Goog-AuthUser"), Some("0"));
    assert_eq!(post.body["context"]["client"]["clientName"], "WEB_REMIX");
    assert_eq!(
        post.body["context"]["client"]["clientVersion"],
        "1.20270115.01.00"
    );
    assert_eq!(post.body["browseId"], "FEmusic_liked_videos");

    // A caller cannot replace the context.
    tube.post(
        "search",
        json!({"context": {"client": {"clientName": "EVIL"}}, "query": "q"}),
    )
    .unwrap();
    let post = &rig.http.posts()[1];
    assert_eq!(post.body["context"]["client"]["clientName"], "WEB_REMIX");
    assert_eq!(
        post.url,
        "https://music.youtube.com/youtubei/v1/search?alt=json"
    );
    tube.post("like/like", json!({"target": {"videoId": "x"}}))
        .unwrap();
    assert!(rig.http.posts()[2].url.contains("/youtubei/v1/like/like?"));
}

#[tokio::test]
async fn without_cookies_or_with_a_bad_endpoint_nothing_is_sent() {
    let (rig, tube) = tube_rig(&[200]);
    assert_eq!(tube.post("browse", json!({})), Err(TubeError::NoCookies));
    rig.login_cookies(&rig.file("c.txt", &good_cookies()))
        .await
        .unwrap();
    for bad in [
        "",
        "Browse",
        "../x",
        "a b",
        "browse?x=1",
        "/browse",
        "browse/",
        "a//b",
        "browse\n",
    ] {
        assert_eq!(
            tube.post(bad, json!({})),
            Err(TubeError::BadEndpoint),
            "{bad:?}"
        );
    }
    assert_eq!(tube.post("browse", json!([1])), Err(TubeError::BadEndpoint));
    assert!(rig.http.posts().is_empty());
}

#[tokio::test]
async fn a_401_or_403_marks_the_cookies_expired_until_they_are_imported_again() {
    for status in [401u16, 403] {
        let (rig, tube) = tube_rig(&[status, 200]);
        let file = rig.file("c.txt", &good_cookies());
        rig.login_cookies(&file).await.unwrap();
        assert_eq!(
            tube.post("browse", json!({})),
            Err(TubeError::CookiesExpired)
        );
        let step = rig.step(STEP_COOKIES).await;
        assert_eq!(
            step.state,
            LoginState::Error {
                message: MSG_COOKIES_EXPIRED.into()
            }
        );
        assert_eq!(MSG_COOKIES_EXPIRED, "Cookies expired: import them again");
        assert_eq!(
            TubeError::CookiesExpired.to_string(),
            "Cookies expired: import them again"
        );
        // Importing the file again clears it.
        rig.login_cookies(&file).await.unwrap();
        assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);
    }
    // A later request that goes through clears it too.
    let (rig, tube) = tube_rig(&[403, 200]);
    rig.login_cookies(&rig.file("c.txt", &good_cookies()))
        .await
        .unwrap();
    let _ = tube.post("browse", json!({}));
    tube.post("browse", json!({})).unwrap();
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);
    // Logging out clears it as well.
    rig.http.answer_posts_with(&[401]);
    let _ = tube.post("browse", json!({}));
    rig.provider.auth().logout(STEP_COOKIES).await.unwrap();
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedOut);
}

#[tokio::test]
async fn another_failure_status_is_not_an_expiry() {
    let (rig, tube) = tube_rig(&[500]);
    rig.login_cookies(&rig.file("c.txt", &good_cookies()))
        .await
        .unwrap();
    assert_eq!(tube.post("browse", json!({})), Err(TubeError::Status));
    assert_eq!(rig.step(STEP_COOKIES).await.state, LoginState::LoggedIn);
}

// ---- contract 1: no secret in any text -------------------------------------------------------

#[tokio::test]
async fn no_cookie_sapisid_or_authorization_reaches_a_text() {
    let (rig, tube) = tube_rig(&[401, 500, 200]);
    let mut texts: Vec<String> = Vec::new();
    let mut collect = |what: &str, text: String| texts.push(format!("{what}: {text}"));

    // Import failures whose files hold the sentinels.
    let no_login = rig.file(
        "nologin.txt",
        &format!(".youtube.com\tTRUE\t/\tTRUE\t0\tSID\t{SENTINEL_COOKIE}\n"),
    );
    collect("no login", rig.login_cookies(&no_login).await.unwrap_err());
    let junk = rig.file("junk.txt", &format!("{SENTINEL_COOKIE} {SENTINEL_SAPISID}"));
    collect("junk", rig.login_cookies(&junk).await.unwrap_err());
    let binary = rig.dir.path().join("b.bin");
    std::fs::write(&binary, [0xff, 0xfe]).unwrap();
    collect("binary", rig.login_cookies(&binary).await.unwrap_err());
    // A path that holds a sentinel is not echoed either.
    let missing = rig.dir.path().join(SENTINEL_COOKIE);
    collect("missing", rig.login_cookies(&missing).await.unwrap_err());

    // A good login, the statuses, and the errors of the client.
    let good = rig.file("good.txt", &good_cookies());
    let status = rig.login_cookies(&good).await.unwrap();
    collect(
        "status",
        format!(
            "{status:?} {}",
            serde_json::to_string(&status.steps[1].state).unwrap()
        ),
    );
    for _ in 0..3 {
        match tube.post("browse", json!({})) {
            Err(e) => collect("tube", format!("{e} {e:?}")),
            Ok(v) => collect("tube ok", v.to_string()),
        }
    }
    collect("step", format!("{:?}", rig.step(STEP_COOKIES).await));
    collect(
        "jar",
        format!("{:?}", Jar::parse(good_cookies().as_bytes()).unwrap()),
    );
    collect(
        "secret",
        format!(
            "{:?} {}",
            Secret::new(SENTINEL_COOKIE),
            Secret::new(SENTINEL_COOKIE)
        ),
    );
    collect(
        "logout",
        format!("{:?}", rig.provider.auth().logout(STEP_COOKIES).await),
    );

    for text in &texts {
        for secret in [
            SENTINEL_COOKIE,
            SENTINEL_SAPISID,
            "SAPISIDHASH",
            "Authorization",
        ] {
            assert!(!text.contains(secret), "{secret} in: {text}");
        }
    }
    assert!(texts.len() > 10);
}

// ---- the account name -------------------------------------------------------------------------

#[test]
fn the_account_names_are_frozen() {
    assert_eq!(COOKIES_ACCOUNT, "ytmusic-cookies");
    assert_eq!(COOKIES_COUNT_ACCOUNT, "ytmusic-cookies-n");
    assert_eq!(cookies_part_account(0), "ytmusic-cookies-0");
    assert_eq!(cookies_part_account(63), "ytmusic-cookies-63");
    assert_eq!(ACCOUNT_PREFIX, "ytmusic-");
    assert!(QJS_RELEASE_BASE.ends_with(&format!("/v{QJS_VERSION}")));
}
