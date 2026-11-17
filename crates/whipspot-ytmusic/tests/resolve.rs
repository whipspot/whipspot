//! Stream resolve: the yt-dlp command line, the checks on its answer, the cookies file of a run,
//! the retry after an update, and the cache. A fake `Runner` stands in for yt-dlp and a fake
//! `Http` for the network: nothing is spawned and nothing is fetched.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pgp::composed::{
    ArmorOptions, DetachedSignature, KeyType, SecretKeyParamsBuilder, SignedPublicKey,
    SignedSecretKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::ser::Serialize;
use pgp::types::Password;
use rand::thread_rng;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use whipspot_core::secret::Secret;
use whipspot_ytmusic::http::{Cap, Http, HttpError, Response};
use whipspot_ytmusic::resolve::{
    CACHE_ENTRIES, RESOLVE_TIMEOUT, RangeForm, ResolveError, Resolver, VERSION_TIMEOUT,
    valid_video_id,
};
use whipspot_ytmusic::run::{Output, RunError, Runner, STDOUT_CAP};
use whipspot_ytmusic::tools::{
    Managed, QJS_RELEASE_BASE, ToolEnv, Tools, Trust, YTDLP_RELEASE_BASE,
};

const ID: &str = "dQw4w9WgXcQ";
const YTDLP_FILE: &str = if cfg!(windows) {
    "yt-dlp.exe"
} else {
    "yt-dlp"
};
const QJS_FILE: &str = if cfg!(windows) { "qjs.exe" } else { "qjs" };
const T0_SECS: u64 = 1_800_000_000;
/// The `expire` of the test URLs: three hours after T0.
const EXPIRE: u64 = T0_SECS + 3 * 3600;

fn url_with(host: &str, itag: u32) -> String {
    format!(
        "https://{host}/videoplayback?expire={EXPIRE}&mime=audio%2Fmp4&itag={itag}&sig=SECRETSIG"
    )
}

fn good_url() -> String {
    url_with("rr4---sn-uphxqvujvh-30az.googlevideo.com", 140)
}

/// The shape of `yt-dlp -J -f 141/140` for one track: the chosen format's fields at the top and
/// the whole format list beside them.
fn info(format_id: &str, url: &str, abr: Value) -> Value {
    json!({
        "id": ID,
        "title": "A title",
        "formats": [
            {"format_id": "139", "url": "https://rr1.googlevideo.com/x", "acodec": "mp4a.40.5"},
            {"format_id": "251", "url": "https://rr1.googlevideo.com/y", "acodec": "opus"},
        ],
        "format_id": format_id,
        "url": url,
        "ext": "m4a",
        "acodec": "mp4a.40.2",
        "vcodec": "none",
        "abr": abr,
        "filesize": 3_449_447,
        "http_headers": {
            "User-Agent": "Mozilla/5.0 (test)",
            "Accept": "*/*",
            "Cookie": "SECRET-COOKIE",
            "Authorization": "Bearer SECRET",
            "Referer": "https://evil.example/"
        },
        "downloader_options": {"http_chunk_size": 10_485_760},
    })
}

fn good_info() -> Value {
    info("140", &good_url(), json!(129.502))
}

fn out(code: i32, stdout: &[u8], stderr: &[u8]) -> Result<Output, RunError> {
    Ok(Output {
        code: Some(code),
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    })
}

fn ok(v: &Value) -> Result<Output, RunError> {
    out(0, &serde_json::to_vec(v).unwrap(), b"")
}

// ---- fakes ------------------------------------------------------------------------------

/// What the fake yt-dlp was asked.
#[derive(Debug, Clone)]
struct Call {
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    /// The cookies file's text at the time of the run, when `--cookies` named one.
    cookies_during_run: Option<String>,
    cookies_path: Option<PathBuf>,
}

struct FakeRunner {
    answers: Mutex<VecDeque<Result<Output, RunError>>>,
    calls: Mutex<Vec<Call>>,
}

impl FakeRunner {
    /// Answers in order; the last answer repeats.
    fn new(answers: Vec<Result<Output, RunError>>) -> Arc<FakeRunner> {
        Arc::new(FakeRunner {
            answers: Mutex::new(answers.into()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for FakeRunner {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<Output, RunError> {
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let cookies_path = args
            .iter()
            .position(|a| a == "--cookies")
            .map(|i| PathBuf::from(&args[i + 1]));
        let cookies_during_run = cookies_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok());
        self.calls.lock().unwrap().push(Call {
            program: program.to_path_buf(),
            args,
            timeout,
            cookies_during_run,
            cookies_path,
        });
        let mut answers = self.answers.lock().unwrap();
        if answers.len() > 1 {
            answers.pop_front().unwrap()
        } else {
            answers.front().cloned().unwrap_or(Err(RunError::Spawn))
        }
    }
}

#[derive(Default)]
struct FakeHttp {
    routes: Mutex<std::collections::HashMap<String, Response>>,
    calls: Mutex<Vec<String>>,
}

impl FakeHttp {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
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
        self.calls.lock().unwrap().push(url.to_owned());
        self.routes
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or(HttpError::Network)
    }

    fn post_json(
        &self,
        _: &str,
        _: &[(&str, &str)],
        _: &[u8],
        _: Cap,
    ) -> Result<Response, HttpError> {
        Err(HttpError::Refused)
    }
}

// ---- the rig ----------------------------------------------------------------------------

/// A throwaway key that the rig's tools trust, so a test can publish a signed checksum list.
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

struct Rig {
    key: TestKey,
    dir: tempfile::TempDir,
    http: Arc<FakeHttp>,
    runner: Arc<FakeRunner>,
    clock: Arc<Mutex<SystemTime>>,
    resolver: Resolver,
}

#[derive(Clone, Copy, PartialEq)]
enum Yt {
    /// Our own copy and our own script runtime, in the tools folder.
    Managed,
    /// Our own copy, no script runtime.
    ManagedNoRuntime,
    /// The user's yt-dlp (found on PATH); the managed script runtime is installed.
    External,
    /// Nothing installed.
    None,
}

impl Rig {
    fn new(yt: Yt, answers: Vec<Result<Output, RunError>>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let mut env = ToolEnv::new(tools_dir.clone());
        let key = test_key();
        env.trust = Trust::Custom(key.armored_public.clone());
        let qjs_bytes = b"qjs test bytes".to_vec();
        env.managed = Some(Managed {
            ytdlp_asset: "yt-dlp-test".into(),
            qjs_asset: "qjs-test".into(),
            qjs_sha256: Sha256::digest(&qjs_bytes).into(),
        });
        let http = Arc::new(FakeHttp::default());
        http.routes.lock().unwrap().insert(
            format!("{QJS_RELEASE_BASE}/qjs-test"),
            Response {
                status: 200,
                body: qjs_bytes.clone(),
            },
        );
        match yt {
            Yt::Managed | Yt::ManagedNoRuntime => {
                std::fs::write(tools_dir.join(YTDLP_FILE), b"yt-dlp").unwrap();
                if yt == Yt::Managed {
                    std::fs::write(tools_dir.join(QJS_FILE), &qjs_bytes).unwrap();
                }
            }
            Yt::External => {
                let external = dir.path().join("their-yt-dlp");
                std::fs::write(&external, b"x").unwrap();
                std::fs::write(tools_dir.join(QJS_FILE), &qjs_bytes).unwrap();
                env.path_lookup = Box::new(move |_| Some(external.clone()));
            }
            Yt::None => {}
        }
        let clock = Arc::new(Mutex::new(UNIX_EPOCH + Duration::from_secs(T0_SECS)));
        let c = Arc::clone(&clock);
        env.now = Box::new(move || *c.lock().unwrap());
        let tools = Arc::new(Tools::new(env, Arc::clone(&http) as Arc<dyn Http>));
        let runner = FakeRunner::new(answers);
        let c = Arc::clone(&clock);
        let resolver = Resolver::with_clock(
            tools,
            Arc::clone(&runner) as Arc<dyn Runner>,
            Box::new(move || *c.lock().unwrap()),
        );
        Rig {
            key,
            dir,
            http,
            runner,
            clock,
            resolver,
        }
    }

    fn tools_dir(&self) -> PathBuf {
        self.dir.path().join("tools")
    }

    /// Publishes a signed checksum list in which the installed yt-dlp is the current one, so an
    /// update of our own copy succeeds and changes nothing.
    fn publish_current_sums(&self) {
        let hash: String = Sha256::digest(b"yt-dlp")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sums = format!("{hash}  yt-dlp-test\n");
        let route = |body: Vec<u8>| Response { status: 200, body };
        let mut routes = self.http.routes.lock().unwrap();
        routes.insert(
            format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS"),
            route(sums.clone().into_bytes()),
        );
        routes.insert(
            format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS.sig"),
            route(sign(&self.key, sums.as_bytes())),
        );
    }

    fn advance(&self, d: Duration) {
        *self.clock.lock().unwrap() += d;
    }

    /// The files in `<tools>/run/`.
    fn run_files(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.tools_dir().join("run")) {
            Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

// ---- the command line and a good answer -----------------------------------------------------

#[test]
fn a_good_answer_gives_the_stream_and_the_argv_is_the_fixed_one() {
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    let s = rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(s.url, good_url());
    assert_eq!(
        s.headers,
        vec![("User-Agent".to_owned(), "Mozilla/5.0 (test)".to_owned())]
    );
    assert_eq!(
        (s.itag, s.abr_kbps, s.filesize),
        (140, Some(128), Some(3_449_447))
    );
    assert_eq!(s.expires_at, Some(UNIX_EPOCH + Duration::from_secs(EXPIRE)));
    assert_eq!(s.range_form, RangeForm::Header);
    assert_eq!(s.chunk_size, Some(10_485_760));

    let calls = rig.runner.calls();
    assert_eq!(calls.len(), 1);
    let tools = rig.tools_dir();
    assert_eq!(calls[0].program, tools.join(YTDLP_FILE));
    assert_eq!(calls[0].timeout, RESOLVE_TIMEOUT);
    assert_eq!(RESOLVE_TIMEOUT, Duration::from_secs(45));
    let expected: Vec<String> = [
        "--ignore-config",
        "--no-playlist",
        "--no-warnings",
        "--no-progress",
        "-J",
        "-f",
        "141/140",
        "--cache-dir",
        &tools.join("cache").to_string_lossy(),
        "--js-runtimes",
        &format!("quickjs:{}", tools.join(QJS_FILE).to_string_lossy()),
        "--",
        &format!("https://music.youtube.com/watch?v={ID}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(calls[0].args, expected);
}

#[test]
fn the_script_runtime_is_named_only_for_our_own_yt_dlp_that_has_one() {
    for yt in [Yt::ManagedNoRuntime, Yt::External] {
        let rig = Rig::new(yt, vec![ok(&good_info())]);
        rig.resolver.resolve(ID, None).unwrap();
        let args = &rig.runner.calls()[0].args;
        assert!(!args.iter().any(|a| a == "--js-runtimes"), "{args:?}");
        assert!(args.contains(&"--cache-dir".to_owned()));
    }
}

#[test]
fn no_yt_dlp_is_a_clear_error_and_runs_nothing() {
    let rig = Rig::new(Yt::None, vec![ok(&good_info())]);
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::NoTools));
    assert!(rig.runner.calls().is_empty());
    // Not a failure an update cures: nothing is fetched either.
    assert!(rig.http.calls().is_empty());
}

#[test]
fn requested_formats_is_read_when_yt_dlp_gives_it() {
    let mut v = good_info();
    let chosen = v.clone();
    v["requested_formats"] = json!([chosen]);
    v.as_object_mut().unwrap().remove("url"); // the top-level fields are not needed then
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert_eq!(rig.resolver.resolve(ID, None).unwrap().url, good_url());

    // More than one requested format (an audio and a video) is not a stream we asked for.
    let mut v = good_info();
    v["requested_formats"] = json!([good_info(), good_info()]);
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::NoStream));
}

// ---- contract 5: an id that fails never reaches the runner ----------------------------------

#[test]
fn an_invalid_id_never_reaches_the_runner() {
    let bad = [
        "",
        "short",
        "dQw4w9WgXcQ1",
        "dQw4w9WgXc",
        "dQw4w9WgXc ",
        "dQw4w9WgX/Q",
        "dQw4w9WgX;Q",
        "dQw4w9WgX&Q",
        "dQw4w9WgX?Q",
        "--exec calc",
        "-J --exec x",
        "../../etc/p",
        "dQw4w9WgXc\n",
        "dQw4w9WgXc\0",
        "dQw4w9WgXcé",
        "ｄQw4w9WgXcQ",
    ];
    for id in bad {
        let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
        let cookies = Secret::new("# Netscape HTTP Cookie File\n");
        assert_eq!(
            rig.resolver.resolve(id, Some(&cookies)),
            Err(ResolveError::BadId),
            "{id:?}"
        );
        assert_eq!(
            rig.resolver.refresh(id, Some(&cookies)),
            Err(ResolveError::BadId),
            "{id:?}"
        );
        assert!(rig.runner.calls().is_empty(), "{id:?}");
        assert!(
            rig.run_files().is_empty(),
            "no cookies file was written for {id:?}"
        );
        assert!(rig.http.calls().is_empty());
        assert!(!valid_video_id(id), "{id:?}");
    }
    assert!(
        valid_video_id("dQw4w9WgXcQ")
            && valid_video_id("-_-_-_-_-_-")
            && valid_video_id("0123456789A")
    );
}

// ---- contract 1: the URL and the headers ----------------------------------------------------

#[test]
fn only_https_on_a_googlevideo_subdomain_with_aac_is_a_stream() {
    let good_host = "rr4---sn-x.googlevideo.com";
    let refused: Vec<(&str, Value)> = vec![
        (
            "http",
            info(
                "140",
                &format!("http://{good_host}/videoplayback?expire={EXPIRE}"),
                json!(128.0),
            ),
        ),
        (
            "evil host",
            info("140", &url_with("evil.example", 140), json!(128.0)),
        ),
        (
            "apex",
            info("140", &url_with("googlevideo.com", 140), json!(128.0)),
        ),
        (
            "suffix lookalike",
            info("140", &url_with("evilgooglevideo.com", 140), json!(128.0)),
        ),
        (
            "prefix lookalike",
            info(
                "140",
                &url_with("googlevideo.com.evil.example", 140),
                json!(128.0),
            ),
        ),
        (
            "userinfo",
            info(
                "140",
                &format!("https://u:p@{good_host}/v?expire={EXPIRE}"),
                json!(128.0),
            ),
        ),
        (
            "at-host trick",
            info(
                "140",
                "https://rr1.googlevideo.com@evil.example/v",
                json!(128.0),
            ),
        ),
        (
            "port",
            info("140", &format!("https://{good_host}:8443/v"), json!(128.0)),
        ),
        ("ip", info("140", "https://203.0.113.9/v", json!(128.0))),
        (
            "localhost",
            info("140", "https://localhost/v", json!(128.0)),
        ),
        (
            "a video mime",
            info(
                "140",
                &format!("https://{good_host}/v?mime=video%2Fmp4"),
                json!(128.0),
            ),
        ),
        ("not a url", info("140", "garbage", json!(128.0))),
        ("a number", info("140", "1", json!(128.0))),
        (
            "too long",
            info(
                "140",
                &format!("https://{good_host}/v?{}", "a".repeat(9000)),
                json!(128.0),
            ),
        ),
    ];
    for (what, v) in refused {
        let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
        assert_eq!(
            rig.resolver.resolve(ID, None),
            Err(ResolveError::NoStream),
            "{what}"
        );
    }

    // Not AAC, or not audio only, or no itag, or no url.
    let mut cases = Vec::new();
    for (key, val) in [
        ("acodec", json!("opus")),
        ("acodec", json!("none")),
        ("acodec", Value::Null),
        ("acodec", json!("MP4A.40.2")),
        ("vcodec", json!("avc1.4d401f")),
        ("format_id", json!("abc")),
        ("format_id", json!("")),
        ("format_id", json!("-1")),
        ("url", Value::Null),
    ] {
        let mut v = good_info();
        v[key] = val.clone();
        cases.push((format!("{key}={val}"), v));
    }
    let mut v = good_info();
    v.as_object_mut().unwrap().remove("acodec");
    cases.push(("no acodec".to_owned(), v));
    for (what, v) in cases {
        let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
        assert_eq!(
            rig.resolver.resolve(ID, None),
            Err(ResolveError::NoStream),
            "{what}"
        );
    }

    // A URL with no `mime` and no `expire` is fine; an `audio/mp4` mime is fine.
    let v = info(
        "140",
        &format!("https://{good_host}/videoplayback?itag=140"),
        json!(128.0),
    );
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert_eq!(rig.resolver.resolve(ID, None).unwrap().expires_at, None);
    // A dash suffix on the format id (a second audio track) keeps the itag.
    let rig = Rig::new(
        Yt::Managed,
        vec![ok(&info("140-1", &good_url(), json!(129.5)))],
    );
    assert_eq!(rig.resolver.resolve(ID, None).unwrap().itag, 140);
}

#[test]
fn only_a_short_clean_user_agent_is_kept_and_no_other_header() {
    let with_ua = |ua: Value| {
        let mut v = good_info();
        v["http_headers"] = json!({"User-Agent": ua, "Cookie": "SECRET", "Referer": "x"});
        let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
        rig.resolver.resolve(ID, None).unwrap().headers
    };
    let ua512 = "a".repeat(512);
    assert_eq!(
        with_ua(json!(ua512.clone())),
        vec![("User-Agent".to_owned(), ua512)]
    );
    assert!(with_ua(json!("a".repeat(513))).is_empty());
    assert!(with_ua(json!("agent\r\nX-Evil: 1")).is_empty());
    assert!(with_ua(json!("agent\nX")).is_empty());
    assert!(with_ua(json!("agent\rX")).is_empty());
    assert!(with_ua(json!("agent\u{0}X")).is_empty());
    assert!(with_ua(json!("")).is_empty());
    // A User-Agent that is not a string makes the answer unreadable; it does not panic.
    let mut v = good_info();
    v["http_headers"] = json!({"User-Agent": 5});
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert_eq!(
        rig.resolver.resolve(ID, None),
        Err(ResolveError::Unreadable)
    );
    // No headers object at all.
    let mut v = good_info();
    v.as_object_mut().unwrap().remove("http_headers");
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert!(rig.resolver.resolve(ID, None).unwrap().headers.is_empty());
}

// ---- contract 4: the bitrate never overstates -------------------------------------------------

#[test]
fn the_bitrate_is_known_only_when_it_agrees_with_the_itag() {
    let cases: Vec<(&str, Value, Option<u32>)> = vec![
        ("140", json!(129.502), Some(128)),
        ("140", json!(128), Some(128)),
        ("141", json!(256.0), Some(256)),
        ("141", json!(255.9), Some(256)),
        // disagree: the number of the other format, or something else entirely
        ("140", json!(256.0), None),
        ("141", json!(128.0), None),
        ("140", json!(48.0), None),
        ("140", json!(320.0), None),
        ("141", json!(0.0), None),
        ("140", json!(-128.0), None),
        ("140", Value::Null, None),
    ];
    for (itag, abr, expected) in cases {
        let rig = Rig::new(Yt::Managed, vec![ok(&info(itag, &good_url(), abr.clone()))]);
        let s = rig.resolver.resolve(ID, None).unwrap();
        assert_eq!(s.abr_kbps, expected, "itag {itag} abr {abr}");
    }
    // An itag we did not ask for gets no bitrate at all, even at 128.
    let rig = Rig::new(
        Yt::Managed,
        vec![ok(&info("139", &good_url(), json!(128.0)))],
    );
    assert_eq!(rig.resolver.resolve(ID, None).unwrap().abr_kbps, None);
    // A missing abr key.
    let mut v = good_info();
    v.as_object_mut().unwrap().remove("abr");
    let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
    assert_eq!(rig.resolver.resolve(ID, None).unwrap().abr_kbps, None);
}

#[test]
fn an_odd_filesize_or_chunk_size_is_unknown_not_a_crash() {
    for (filesize, chunk) in [
        (json!(-5), json!(-1)),
        (json!(0), json!(0)),
        (json!(1e300), json!(1e300)),
        (json!("big"), json!("big")),
    ] {
        let mut v = good_info();
        v["filesize"] = filesize;
        v["downloader_options"] = json!({ "http_chunk_size": chunk });
        let rig = Rig::new(Yt::Managed, vec![ok(&v)]);
        // A wrong type is unreadable; a silly number is unknown.
        match rig.resolver.resolve(ID, None) {
            Ok(s) => assert_eq!((s.filesize, s.chunk_size), (None, None)),
            Err(e) => assert_eq!(e, ResolveError::Unreadable),
        }
    }
}

#[test]
fn an_odd_expire_is_no_expiry_not_a_crash() {
    // 18446744073709551615 is u64::MAX, past any platform's `SystemTime`; the others are not
    // numbers or not whole.
    for expire in [
        "18446744073709551615",
        "99999999999999",
        "-1",
        "1e9",
        "soon",
        "",
    ] {
        let url = format!(
            "https://rr4---sn-x.googlevideo.com/videoplayback?expire={expire}&mime=audio%2Fmp4&itag=140"
        );
        let rig = Rig::new(Yt::Managed, vec![ok(&info("140", &url, json!(129.5)))]);
        let s = rig
            .resolver
            .resolve(ID, None)
            .unwrap_or_else(|e| panic!("expire={expire}: {e:?}"));
        assert!(
            s.expires_at.is_none() || expire == "99999999999999",
            "expire={expire}"
        );
    }
}

// ---- contract 3: bad, huge and nested JSON ----------------------------------------------------

#[test]
fn malformed_output_is_an_error_never_a_panic() {
    let nest = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n)).into_bytes();
    let deep_object = format!("{}1{}", "{\"a\":".repeat(5000), "}".repeat(5000)).into_bytes();
    let mut truncated = serde_json::to_vec(&good_info()).unwrap();
    truncated.truncate(truncated.len() / 2);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("spaces", b"  \n ".to_vec()),
        ("null", b"null".to_vec()),
        ("array", b"[]".to_vec()),
        ("string", b"\"x\"".to_vec()),
        ("number", b"1".to_vec()),
        ("object without a stream", b"{}".to_vec()),
        ("garbage", b"ERROR: not json".to_vec()),
        ("truncated", truncated),
        ("binary", vec![0xff, 0xfe, 0x00, 0x01]),
        ("deep array", nest(200_000)),
        ("deep array just over the limit", nest(129)),
        ("deep object", deep_object),
        (
            "url is a number",
            br#"{"format_id":"140","url":5,"acodec":"mp4a.40.2"}"#.to_vec(),
        ),
        (
            "url is an object",
            br#"{"format_id":"140","url":{},"acodec":"mp4a.40.2"}"#.to_vec(),
        ),
        (
            "formats is a string",
            br#"{"requested_formats":"x"}"#.to_vec(),
        ),
    ];
    for (what, stdout) in cases {
        let rig = Rig::new(Yt::Managed, vec![out(0, &stdout, b"")]);
        let r = rig.resolver.resolve(ID, None);
        assert!(
            matches!(r, Err(ResolveError::Unreadable | ResolveError::NoStream)),
            "{what}: {r:?}"
        );
    }
}

#[test]
fn a_sixteen_mib_run_is_cut_by_the_cap_or_refused() {
    // What the real runner's cap leaves of a longer answer: exactly STDOUT_CAP bytes, ending in
    // the middle of a string.
    let mut cut = br#"{"format_id":"140","title":""#.to_vec();
    cut.resize(STDOUT_CAP, b'x');
    assert_eq!(cut.len(), 16 * 1024 * 1024);
    let rig = Rig::new(Yt::Managed, vec![out(0, &cut, b"")]);
    assert_eq!(
        rig.resolver.resolve(ID, None),
        Err(ResolveError::Unreadable)
    );

    // A runner that did not cut it: more than the cap is refused before it is parsed.
    let mut over = serde_json::to_vec(&good_info()).unwrap();
    // (a complete, valid answer that is only too long: padding after the JSON)
    over.resize(STDOUT_CAP + 1, b' ');
    let rig = Rig::new(Yt::Managed, vec![out(0, &over, b"")]);
    assert_eq!(
        rig.resolver.resolve(ID, None),
        Err(ResolveError::Unreadable)
    );

    // And the real runner's own verdict passes through as a fixed error.
    let rig = Rig::new(Yt::Managed, vec![Err(RunError::OutputTooLarge)]);
    assert_eq!(
        rig.resolver.resolve(ID, None),
        Err(ResolveError::Run(RunError::OutputTooLarge))
    );
}

#[test]
fn errors_never_carry_the_tools_stderr_or_the_cookies() {
    let rig = Rig::new(
        Yt::Managed,
        vec![out(
            1,
            b"",
            b"ERROR: Sign in: SECRET-COOKIE https://x/?sig=SECRETSIG",
        )],
    );
    let cookies = Secret::new("SECRET-COOKIE");
    let e = rig.resolver.resolve(ID, Some(&cookies)).unwrap_err();
    assert_eq!(e, ResolveError::Failed);
    for e in [
        e,
        ResolveError::BadId,
        ResolveError::NoTools,
        ResolveError::Run(RunError::Timeout),
        ResolveError::Unreadable,
        ResolveError::NoStream,
        ResolveError::Disk,
    ] {
        let text = format!("{e} | {e:?}");
        for bad in ["SECRET", "sig=", "http", "cookie", "--"] {
            assert!(!text.contains(bad), "{text} has {bad}");
        }
    }
}

// ---- contract 2: the cookies file of a run ---------------------------------------------------

const COOKIES: &str =
    "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tSECRETVALUE\n";

#[test]
fn the_cookies_file_exists_for_the_run_and_is_gone_after_on_every_path() {
    let cookies = Secret::new(COOKIES);
    let answers: Vec<(&str, Result<Output, RunError>)> = vec![
        ("success", ok(&good_info())),
        ("non-zero exit", out(1, b"", b"x")),
        ("unreadable", out(0, b"{", b"")),
        ("timeout", Err(RunError::Timeout)),
        ("spawn failure", Err(RunError::Spawn)),
        ("too much output", Err(RunError::OutputTooLarge)),
        ("no stream", ok(&json!({}))),
    ];
    for (what, answer) in answers {
        let rig = Rig::new(Yt::Managed, vec![answer]);
        let _ = rig.resolver.resolve(ID, Some(&cookies));
        let calls = rig.runner.calls();
        assert!(!calls.is_empty(), "{what}");
        for call in &calls {
            assert_eq!(call.cookies_during_run.as_deref(), Some(COOKIES), "{what}");
            let path = call.cookies_path.as_ref().unwrap();
            assert_eq!(
                path.parent().unwrap(),
                rig.tools_dir().join("run"),
                "{what}"
            );
            assert!(!path.exists(), "{what}: the cookies file is removed");
            // Passed right before the `--`.
            let i = call.args.iter().position(|a| a == "--cookies").unwrap();
            assert_eq!(call.args[i + 2], "--");
        }
        assert!(
            rig.run_files().is_empty(),
            "{what}: nothing is left in run/"
        );
    }
}

#[test]
fn the_cookies_file_has_a_random_name_each_run() {
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    let cookies = Secret::new(COOKIES);
    for _ in 0..8 {
        // The cache would answer a repeat: ask for a fresh one each time.
        rig.resolver.refresh(ID, Some(&cookies)).unwrap();
    }
    let names: std::collections::HashSet<String> = rig
        .runner
        .calls()
        .iter()
        .map(|c| {
            c.cookies_path
                .as_ref()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names.len(), 8, "{names:?}");
    for n in &names {
        assert!(
            !n.contains(ID) && !n.contains("cookie") && n.len() >= 32 + 4,
            "{n}"
        );
    }
}

#[test]
fn no_cookies_means_no_cookies_flag_and_no_file() {
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    rig.resolver.resolve(ID, None).unwrap();
    assert!(!rig.runner.calls()[0].args.iter().any(|a| a == "--cookies"));
    assert!(rig.run_files().is_empty());
    assert!(!rig.tools_dir().join("run").exists());
    // An empty secret is no cookies.
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    rig.resolver.resolve(ID, Some(&Secret::new(""))).unwrap();
    assert!(!rig.runner.calls()[0].args.iter().any(|a| a == "--cookies"));
}

// ---- the retry after an update -----------------------------------------------------------------

#[test]
fn a_failed_run_of_our_own_yt_dlp_triggers_one_update_and_one_retry() {
    let rig = Rig::new(Yt::Managed, vec![out(1, b"", b""), ok(&good_info())]);
    rig.publish_current_sums();
    let s = rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(s.itag, 140);
    assert_eq!(
        rig.runner.calls().len(),
        2,
        "one run, one update, one retry"
    );
    assert_eq!(
        rig.http.calls(),
        vec![
            format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS"),
            format!("{YTDLP_RELEASE_BASE}/SHA2-256SUMS.sig"),
        ],
        "the checksum list only: everything was current"
    );
}

#[test]
fn a_failed_run_of_the_users_yt_dlp_fetches_nothing_and_runs_once() {
    // Installing is the user's button: an update of the runtime would not change what their
    // yt-dlp does, so neither the download nor a second run happens.
    for answer in [out(1, b"", b""), out(0, b"", b"")] {
        let rig = Rig::new(Yt::External, vec![answer]);
        std::fs::remove_file(rig.tools_dir().join(QJS_FILE)).unwrap();
        assert!(rig.resolver.resolve(ID, None).is_err());
        assert_eq!(rig.runner.calls().len(), 1, "no retry");
        assert!(rig.http.calls().is_empty(), "nothing was fetched");
        assert!(!rig.tools_dir().join(QJS_FILE).exists());
    }
}

#[test]
fn an_empty_result_is_retried_too_and_there_is_no_third_run() {
    let rig = Rig::new(Yt::Managed, vec![out(0, b"", b""), out(0, b"", b"")]);
    rig.publish_current_sums();
    assert_eq!(
        rig.resolver.resolve(ID, None),
        Err(ResolveError::Unreadable)
    );
    assert_eq!(rig.runner.calls().len(), 2);
}

#[test]
fn a_failed_update_means_no_retry_and_updates_are_rate_limited() {
    // Our own yt-dlp: the update starts with the checksum list, which the fake cannot serve.
    let rig = Rig::new(Yt::Managed, vec![out(1, b"", b"")]);
    let sums = |rig: &Rig| {
        rig.http
            .calls()
            .iter()
            .filter(|c| c.ends_with("SHA2-256SUMS"))
            .count()
    };
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::Failed));
    assert_eq!(rig.runner.calls().len(), 1, "the update failed: no retry");
    assert_eq!(sums(&rig), 1);

    // A minute later: no new update, and so no retry.
    rig.advance(Duration::from_secs(60));
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::Failed));
    assert_eq!(rig.runner.calls().len(), 2);
    assert_eq!(sums(&rig), 1);

    // After the limit another one is tried.
    rig.advance(Duration::from_secs(11 * 60));
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::Failed));
    assert_eq!(sums(&rig), 2);
}

#[test]
fn a_bad_id_or_missing_tools_never_trigger_an_update() {
    let rig = Rig::new(Yt::None, vec![ok(&good_info())]);
    assert_eq!(rig.resolver.resolve(ID, None), Err(ResolveError::NoTools));
    assert_eq!(rig.resolver.resolve("bad", None), Err(ResolveError::BadId));
    assert!(rig.http.calls().is_empty());
}

// ---- the cache ---------------------------------------------------------------------------------

fn id_n(n: usize) -> String {
    format!("track{n:06}")
}

#[test]
fn a_second_resolve_of_a_track_is_answered_from_the_cache() {
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    let a = rig.resolver.resolve(ID, None).unwrap();
    let b = rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(a, b);
    assert_eq!(rig.runner.calls().len(), 1);
    // `refresh` goes to the tool again.
    rig.resolver.refresh(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 2);
    // A failed run is not cached.
    let rig = Rig::new(Yt::Managed, vec![out(1, b"", b"")]);
    let _ = rig.resolver.resolve(ID, None);
    let _ = rig.resolver.resolve(ID, None);
    assert!(rig.runner.calls().len() >= 2);
}

#[test]
fn the_cache_holds_twelve_tracks_and_drops_the_least_recently_used() {
    assert_eq!(CACHE_ENTRIES, 12);
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    for n in 0..12 {
        rig.resolver.resolve(&id_n(n), None).unwrap();
    }
    assert_eq!(rig.runner.calls().len(), 12);
    // Touch the oldest, then add a 13th: the second oldest goes.
    rig.resolver.resolve(&id_n(0), None).unwrap();
    rig.resolver.resolve(&id_n(12), None).unwrap();
    assert_eq!(rig.runner.calls().len(), 13);
    rig.resolver.resolve(&id_n(0), None).unwrap();
    assert_eq!(rig.runner.calls().len(), 13, "the touched entry stayed");
    rig.resolver.resolve(&id_n(1), None).unwrap();
    assert_eq!(
        rig.runner.calls().len(),
        14,
        "the least recently used entry was dropped"
    );
}

#[test]
fn an_entry_is_invalid_thirty_minutes_before_its_url_expires() {
    let rig = Rig::new(Yt::Managed, vec![ok(&good_info())]);
    rig.resolver.resolve(ID, None).unwrap();
    // The URL expires at T0 + 3 h. 2 h 29 min after T0 it is still good.
    rig.advance(Duration::from_secs(2 * 3600 + 29 * 60));
    rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 1);
    // 2 h 31 min: inside the 30 minutes before the expiry.
    rig.advance(Duration::from_secs(2 * 60));
    rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 2);
}

#[test]
fn an_entry_is_invalid_five_hours_after_it_was_resolved() {
    let far = format!(
        "https://rr4.googlevideo.com/videoplayback?expire={}&mime=audio%2Fmp4",
        T0_SECS + 24 * 3600
    );
    let rig = Rig::new(Yt::Managed, vec![ok(&info("140", &far, json!(128.0)))]);
    rig.resolver.resolve(ID, None).unwrap();
    rig.advance(Duration::from_secs(5 * 3600 - 60));
    rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 1);
    rig.advance(Duration::from_secs(120));
    rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 2);
}

#[test]
fn a_url_that_expires_within_the_margin_is_never_served_from_the_cache() {
    let soon = format!(
        "https://rr4.googlevideo.com/videoplayback?expire={}&mime=audio%2Fmp4",
        T0_SECS + 10 * 60
    );
    let rig = Rig::new(Yt::Managed, vec![ok(&info("140", &soon, json!(128.0)))]);
    rig.resolver.resolve(ID, None).unwrap();
    rig.resolver.resolve(ID, None).unwrap();
    assert_eq!(rig.runner.calls().len(), 2);
}

// ---- the version -------------------------------------------------------------------------------

#[test]
fn the_version_is_asked_with_a_ten_second_limit_and_read_strictly() {
    let rig = Rig::new(Yt::Managed, vec![out(0, b"2026.10.26\n", b"")]);
    assert_eq!(rig.resolver.ytdlp_version().unwrap(), "2026.10.26");
    let call = &rig.runner.calls()[0];
    assert_eq!(call.args, ["--version"]);
    assert_eq!(call.timeout, VERSION_TIMEOUT);
    assert_eq!(VERSION_TIMEOUT, Duration::from_secs(10));

    for (stdout, expected) in [
        (&b""[..], ResolveError::Unreadable),
        (b"1.0 && calc", ResolveError::Unreadable),
        (b"line1\nline2", ResolveError::Unreadable),
        (&[b'1'; 33][..], ResolveError::Unreadable),
        (b"\xff\xfe", ResolveError::Unreadable),
    ] {
        let rig = Rig::new(Yt::Managed, vec![out(0, stdout, b"")]);
        assert_eq!(rig.resolver.ytdlp_version(), Err(expected));
    }
    let rig = Rig::new(Yt::Managed, vec![out(2, b"2026.10.26", b"")]);
    assert_eq!(rig.resolver.ytdlp_version(), Err(ResolveError::Failed));
    let rig = Rig::new(Yt::None, vec![]);
    assert_eq!(rig.resolver.ytdlp_version(), Err(ResolveError::NoTools));
    let rig = Rig::new(Yt::Managed, vec![Err(RunError::Timeout)]);
    assert_eq!(
        rig.resolver.ytdlp_version(),
        Err(ResolveError::Run(RunError::Timeout))
    );
}
