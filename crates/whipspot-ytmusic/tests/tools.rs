//! The managed tools: how they are found, and that a download is installed only after it
//! verifies. No network (a fake `Http` serves canned bytes) and no real yt-dlp (nothing is run).
//!
//! The release is signed in the test with a throwaway key that the code is told to trust through
//! `Trust::Custom`; one test also checks the real yt-dlp signature file against the compiled-in
//! key.

use std::collections::HashMap;
use std::io;
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
use sha2::{Digest, Sha256};
use whipspot_ytmusic::http::{Cap, Http, HttpError, ReqwestHttp, Response};
use whipspot_ytmusic::tools::{
    Change, Installed, Managed, QJS_RELEASE_BASE, Tool, ToolEnv, ToolError, ToolSet, Tools, Trust,
    YTDLP_RELEASE_BASE, sums_hash, verify_signature,
};

const YTDLP_ASSET: &str = "yt-dlp-test";
const QJS_ASSET: &str = "qjs-test";
const YTDLP_FILE: &str = if cfg!(windows) {
    "yt-dlp.exe"
} else {
    "yt-dlp"
};
const QJS_FILE: &str = if cfg!(windows) { "qjs.exe" } else { "qjs" };
const SUMS_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";
const SIG_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS.sig";

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ytdlp_url() -> String {
    format!("{YTDLP_RELEASE_BASE}/{YTDLP_ASSET}")
}

fn qjs_url() -> String {
    format!("{QJS_RELEASE_BASE}/{QJS_ASSET}")
}

// ---- a throwaway signing key -------------------------------------------------------------

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
    let secret = params
        .build()
        .expect("key params")
        .generate(thread_rng())
        .expect("generate");
    let armored_public = SignedPublicKey::from(secret.clone())
        .to_armored_string(ArmorOptions::default())
        .expect("armor");
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
    .expect("sign")
    .to_bytes()
    .expect("serialize")
}

// ---- a fake Http -------------------------------------------------------------------------

#[derive(Default)]
struct FakeHttp {
    routes: Mutex<HashMap<String, Result<Response, HttpError>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeHttp {
    fn serve(&self, url: &str, body: &[u8]) {
        self.routes.lock().unwrap().insert(
            url.to_owned(),
            Ok(Response {
                status: 200,
                body: body.to_vec(),
            }),
        );
    }

    fn serve_status(&self, url: &str, status: u16) {
        self.routes.lock().unwrap().insert(
            url.to_owned(),
            Ok(Response {
                status,
                body: Vec::new(),
            }),
        );
    }

    fn fail(&self, url: &str, e: HttpError) {
        self.routes.lock().unwrap().insert(url.to_owned(), Err(e));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Http for FakeHttp {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        assert!(
            headers.is_empty() && range.is_none(),
            "a download carries no caller headers"
        );
        self.calls.lock().unwrap().push(url.to_owned());
        self.routes
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(Err(HttpError::Network))
    }

    fn post_json(
        &self,
        _url: &str,
        _headers: &[(&str, &str)],
        _body: &[u8],
        _cap: Cap,
    ) -> Result<Response, HttpError> {
        Err(HttpError::Refused)
    }
}

// ---- the rig -----------------------------------------------------------------------------

struct Rig {
    dir: tempfile::TempDir,
    http: Arc<FakeHttp>,
    key: TestKey,
    qjs_bytes: Vec<u8>,
}

impl Rig {
    fn new() -> Rig {
        let rig = Rig {
            dir: tempfile::tempdir().unwrap(),
            http: Arc::new(FakeHttp::default()),
            key: test_key(),
            qjs_bytes: b"qjs v0.18.0 test bytes".to_vec(),
        };
        rig.http.serve(&qjs_url(), &rig.qjs_bytes);
        rig
    }

    fn tools_dir(&self) -> PathBuf {
        self.dir.path().join("tools")
    }

    fn env(&self) -> ToolEnv {
        let mut env = ToolEnv::new(self.tools_dir());
        env.trust = Trust::Custom(self.key.armored_public.clone());
        env.managed = Some(Managed {
            ytdlp_asset: YTDLP_ASSET.into(),
            qjs_asset: QJS_ASSET.into(),
            qjs_sha256: sha(&self.qjs_bytes),
        });
        env
    }

    fn tools(&self) -> Tools {
        self.tools_with(self.env())
    }

    fn tools_with(&self, env: ToolEnv) -> Tools {
        Tools::new(env, Arc::clone(&self.http) as Arc<dyn Http>)
    }

    /// Publishes a checksum list signed by `signer`.
    fn publish(&self, sums_text: &str, signer: &TestKey) {
        self.http.serve(SUMS_URL, sums_text.as_bytes());
        self.http
            .serve(SIG_URL, &sign(signer, sums_text.as_bytes()));
    }

    /// A correct release of `binary`.
    fn release(&self, binary: &[u8]) {
        self.http.serve(&ytdlp_url(), binary);
        self.publish(&sums_for(binary), &self.key);
    }

    fn ytdlp_path(&self) -> PathBuf {
        self.tools_dir().join(YTDLP_FILE)
    }

    fn files(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.tools_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn sums_for(binary: &[u8]) -> String {
    // An unrelated line first and last: the asset's line is found among others.
    format!(
        "{}  other-asset\n{}  {YTDLP_ASSET}\n{}  zzz-asset\n",
        hex(&sha(b"other")),
        hex(&sha(binary)),
        hex(&sha(b"zzz"))
    )
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

// ---- installing --------------------------------------------------------------------------

#[test]
fn a_verified_release_is_installed_and_found() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let tools = rig.tools();
    assert_eq!(tools.resolve(), None, "nothing installed yet");

    let done = tools.install_or_update().unwrap();
    assert_eq!(
        done,
        Installed {
            ytdlp: Change::Installed,
            qjs: Change::Installed
        }
    );
    assert_eq!(read(&rig.ytdlp_path()), b"yt-dlp v1");
    assert_eq!(
        tools.resolve(),
        Some(ToolSet {
            ytdlp: rig.ytdlp_path(),
            qjs: Some(rig.tools_dir().join(QJS_FILE)),
            own: true
        })
    );
    // No temporary file is left, only the two tools and the update stamp.
    assert_eq!(
        rig.files(),
        vec![
            QJS_FILE.to_owned(),
            "update-stamp".into(),
            YTDLP_FILE.into()
        ]
    );
}

#[test]
fn an_installed_copy_that_matches_is_not_downloaded_again() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let tools = rig.tools();
    tools.install_or_update().unwrap();
    let before = rig.http.calls().len();
    let done = tools.install_or_update().unwrap();
    assert_eq!(
        done,
        Installed {
            ytdlp: Change::Unchanged,
            qjs: Change::Unchanged
        }
    );
    // Only the checksum list and its signature were fetched.
    assert_eq!(rig.http.calls()[before..], [SUMS_URL, SIG_URL]);
}

#[test]
fn a_newer_release_replaces_the_installed_copy() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    rig.tools().install_or_update().unwrap();
    rig.release(b"yt-dlp v2");
    let done = rig.tools().install_or_update().unwrap();
    assert_eq!(done.ytdlp, Change::Updated);
    assert_eq!(read(&rig.ytdlp_path()), b"yt-dlp v2");
}

// ---- contract 2: every failed check keeps the old copy and writes nothing ---------------

/// Installs v1, then makes `break_it` publish something wrong for v2 and checks that the
/// install fails with `expected`, the installed copy is still v1, and no file was added.
fn assert_rejected(expected: ToolError, break_it: impl FnOnce(&Rig)) {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    rig.tools().install_or_update().unwrap();
    let files_before = rig.files();

    rig.release(b"yt-dlp v2");
    break_it(&rig);
    let err = rig.tools().install_or_update().unwrap_err();
    assert_eq!(err, expected);
    assert_eq!(
        read(&rig.ytdlp_path()),
        b"yt-dlp v1",
        "the installed copy is untouched"
    );
    assert_eq!(rig.files(), files_before, "no temporary file is left");
}

#[test]
fn a_signature_by_another_key_is_rejected_before_the_binary_is_fetched() {
    let other = test_key();
    let rig_checked = Rig::new();
    rig_checked.release(b"yt-dlp v1");
    rig_checked.tools().install_or_update().unwrap();
    let before = rig_checked.http.calls().len();
    rig_checked.publish(&sums_for(b"yt-dlp v2"), &other);
    rig_checked.http.serve(&ytdlp_url(), b"yt-dlp v2");
    let err = rig_checked.tools().install_or_update().unwrap_err();
    assert_eq!(err, ToolError::BadSignature);
    // The binary was never requested: a failed signature stops everything before it.
    assert!(!rig_checked.http.calls()[before..].contains(&ytdlp_url()));
    assert_eq!(read(&rig_checked.ytdlp_path()), b"yt-dlp v1");

    assert_rejected(ToolError::BadSignature, |rig| {
        rig.publish(&sums_for(b"yt-dlp v2"), &other);
    });
}

#[test]
fn sums_changed_after_signing_are_rejected() {
    assert_rejected(ToolError::BadSignature, |rig| {
        // The signature is of the real list; the list served is another one.
        let real = sums_for(b"yt-dlp v2");
        rig.http.serve(SIG_URL, &sign(&rig.key, real.as_bytes()));
        let tampered = real.replace(&hex(&sha(b"yt-dlp v2")), &hex(&sha(b"evil")));
        rig.http.serve(SUMS_URL, tampered.as_bytes());
    });
}

#[test]
fn a_garbage_or_missing_signature_is_rejected() {
    assert_rejected(ToolError::BadSignature, |rig| {
        rig.http.serve(SIG_URL, b"not a signature")
    });
    assert_rejected(ToolError::BadSignature, |rig| rig.http.serve(SIG_URL, b""));
}

/// A binary signature whose unhashed area holds an Embedded Signature subpacket that holds another,
/// `levels` deep (GHSA-58v8-hh27-23g2: pgp up to 0.20 recursed once per level without a limit and
/// overflowed the stack).
fn nested_signature(levels: usize) -> Vec<u8> {
    // The innermost body is cut short on purpose: it is not a valid signature.
    let mut body = vec![4u8, 0, 22, 8, 0, 0, 0, 0];
    for _ in 0..levels {
        // Subpacket: 5-byte length, type 32 (Embedded Signature), the inner body.
        let mut sub = vec![0xFF];
        sub.extend_from_slice(&((body.len() + 1) as u32).to_be_bytes());
        sub.push(32);
        sub.extend_from_slice(&body);
        let mut outer = vec![4u8, 0, 22, 8, 0, 0];
        outer.extend_from_slice(&(sub.len() as u16).to_be_bytes());
        outer.extend_from_slice(&sub);
        body = outer;
    }
    // A new-format packet header, tag 2 (Signature), five-byte length.
    let mut packet = vec![0xC2, 0xFF];
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
    packet.extend_from_slice(&body);
    packet
}

#[test]
fn a_deeply_nested_signature_is_rejected_without_overflowing_the_stack() {
    // About 16 KiB, well under the 64 KiB cap on the download; the test thread has the default
    // 2 MiB stack, like the update thread.
    let sig = nested_signature(1000);
    assert!(sig.len() < 64 * 1024, "{}", sig.len());
    assert!(!verify_signature(&Trust::Compiled, REAL_SUMS, &sig));
    let deep = nested_signature(1400);
    assert!(deep.len() < 64 * 1024, "{}", deep.len());
    assert!(!verify_signature(&Trust::Compiled, REAL_SUMS, &deep));
}

#[test]
fn a_signed_list_without_the_assets_line_is_rejected() {
    assert_rejected(ToolError::BadSums, |rig| {
        let text = format!("{}  some-other-asset\n", hex(&sha(b"yt-dlp v2")));
        rig.publish(&text, &rig.key);
    });
}

#[test]
fn a_duplicate_line_for_the_asset_is_rejected_even_if_both_agree() {
    assert_rejected(ToolError::BadSums, |rig| {
        let h = hex(&sha(b"yt-dlp v2"));
        let text = format!("{h}  {YTDLP_ASSET}\n{h}  {YTDLP_ASSET}\n");
        rig.publish(&text, &rig.key);
    });
}

#[test]
fn a_binary_that_does_not_match_its_line_is_rejected() {
    assert_rejected(ToolError::HashMismatch, |rig| {
        rig.http
            .serve(&ytdlp_url(), b"yt-dlp v2 but altered in transit");
    });
}

#[test]
fn a_download_that_is_not_a_success_is_rejected() {
    assert_rejected(ToolError::Download, |rig| {
        rig.http.serve_status(&ytdlp_url(), 404)
    });
}

#[test]
fn the_checksum_list_table() {
    let h1 = hex(&sha(b"1"));
    let h2 = hex(&sha(b"2"));
    let ok = |s: &str, name: &str| sums_hash(s.as_bytes(), name);
    assert_eq!(ok(&format!("{h1}  a\n"), "a"), Ok(sha(b"1")));
    assert_eq!(ok(&format!("{h1}  a\n{h2}  b\n"), "b"), Ok(sha(b"2")));
    // Upper-case hex digits are digits; the name is not folded.
    assert_eq!(
        ok(&format!("{}  a\n", h1.to_uppercase()), "a"),
        Ok(sha(b"1"))
    );
    assert_eq!(ok(&format!("{h1}  A\n"), "a"), Err(ToolError::BadSums));
    assert_eq!(ok(&format!("{h1}  a\n"), "A"), Err(ToolError::BadSums));

    let bad = [
        // a second line for one name, even if identical
        format!("{h1}  a\n{h2}  a\n"),
        format!("{h1}  a\n{h1}  a\n"),
        // one space, a binary-mode star, a tab, three spaces
        format!("{h1} a\n"),
        format!("{h1} *a\n"),
        format!("{h1}\ta\n"),
        // the name gets the third space, so it is another name
        format!("{h1}   a\n"),
        // short or long or non-hex digest
        format!("{}  a\n", &h1[..63]),
        format!("{h1}0  a\n"),
        format!("{}g  a\n", &h1[..63]),
        // an empty name, an empty line, no final newline, a CR, a stray line
        format!("{h1}  \n"),
        format!("{h1}  a\n\n"),
        format!("\n{h1}  a\n"),
        format!("{h1}  a"),
        format!("{h1}  a\r\n"),
        format!("{h1}  a\nnot a checksum line\n"),
        String::new(),
        "\n".to_owned(),
    ];
    for text in &bad {
        assert_eq!(
            sums_hash(text.as_bytes(), "a"),
            Err(ToolError::BadSums),
            "{text:?}"
        );
    }
    assert_eq!(sums_hash(b"\xff\xfe", "a"), Err(ToolError::BadSums));
}

// ---- the script runtime's pin ------------------------------------------------------------

#[test]
fn a_script_runtime_that_does_not_match_its_pin_is_rejected_and_nothing_is_written() {
    let rig = Rig::new();
    let mut env = rig.env();
    // yt-dlp is supplied from outside, so the runtime is the only download.
    let external = rig.dir.path().join("external-yt-dlp");
    std::fs::write(&external, b"x").unwrap();
    env.path_lookup = Box::new(move |_| Some(external.clone()));
    env.managed.as_mut().unwrap().qjs_sha256 = sha(b"a different build");
    let err = rig.tools_with(env).install_or_update().unwrap_err();
    assert_eq!(err, ToolError::HashMismatch);
    assert!(
        !rig.tools_dir().exists(),
        "nothing was written, not even the folder"
    );
}

#[test]
fn a_pinned_runtime_that_changed_is_replaced() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    rig.tools().install_or_update().unwrap();
    let mut newer = rig.env();
    let bytes = b"qjs v0.19 test bytes";
    rig.http.serve(&qjs_url(), bytes);
    newer.managed.as_mut().unwrap().qjs_sha256 = sha(bytes);
    let done = rig.tools_with(newer).install_or_update().unwrap();
    assert_eq!(done.qjs, Change::Updated);
    assert_eq!(read(&rig.tools_dir().join(QJS_FILE)), bytes);
}

// ---- contract 1: the transport's refusals write nothing ----------------------------------

#[test]
fn a_transport_refusal_fails_with_its_fixed_error_and_writes_nothing() {
    for e in [
        HttpError::Refused,
        HttpError::Redirect,
        HttpError::TooLarge,
        HttpError::Timeout,
    ] {
        let rig = Rig::new();
        rig.release(b"yt-dlp v1");
        rig.http.fail(SUMS_URL, e);
        assert_eq!(rig.tools().install_or_update(), Err(ToolError::Http(e)));
        assert!(!rig.tools_dir().exists());

        let rig = Rig::new();
        rig.release(b"yt-dlp v1");
        rig.http.fail(&ytdlp_url(), e);
        assert_eq!(rig.tools().install_or_update(), Err(ToolError::Http(e)));
        assert!(!rig.tools_dir().exists());
    }
}

#[test]
fn a_body_over_its_cap_from_any_transport_is_refused() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    // A transport that did not enforce the cap: the tools check it again.
    rig.http
        .serve(SUMS_URL, &vec![b'a'; Cap::SMALL.bytes() + 1]);
    assert_eq!(
        rig.tools().install_or_update(),
        Err(ToolError::Http(HttpError::TooLarge))
    );
    assert!(!rig.tools_dir().exists());
}

#[test]
fn the_real_transport_refuses_a_foreign_host_and_a_plain_http_address() {
    // Refused in the allowlist, before any connection: no network is touched.
    let http = ReqwestHttp::new().unwrap();
    for url in [
        "http://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
        "https://evil.example/yt-dlp.exe",
        "https://github.com.evil.example/yt-dlp.exe",
        "https://github.com:8443/yt-dlp.exe",
    ] {
        assert_eq!(
            http.get(url, &[], None, Cap::SMALL),
            Err(HttpError::Refused),
            "{url}"
        );
    }
}

// ---- contract 3: the swap waits for runs, and a failed swap keeps the old copy ----------

#[test]
fn an_update_waits_while_a_run_holds_the_tools() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let tools = Arc::new(rig.tools());
    tools.install_or_update().unwrap();
    rig.release(b"yt-dlp v2");

    let held = tools.acquire().expect("installed");
    let updater = {
        let tools = Arc::clone(&tools);
        std::thread::spawn(move || tools.install_or_update())
    };
    // Long enough for the download and its checks, which do not need the lock.
    std::thread::sleep(Duration::from_millis(500));
    assert!(!updater.is_finished(), "the update must wait for the run");
    assert_eq!(read(&rig.ytdlp_path()), b"yt-dlp v1");

    drop(held);
    let done = updater.join().unwrap().unwrap();
    assert_eq!(done.ytdlp, Change::Updated);
    assert_eq!(read(&rig.ytdlp_path()), b"yt-dlp v2");
}

#[test]
fn two_runs_may_hold_the_tools_together() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let tools = rig.tools();
    tools.install_or_update().unwrap();
    let a = tools.acquire().unwrap();
    let b = tools.acquire().unwrap();
    assert_eq!(a.ytdlp, b.ytdlp);
}

#[test]
fn a_rename_that_fails_keeps_the_old_copy_and_gives_the_fixed_error() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    rig.tools().install_or_update().unwrap();
    let files_before = rig.files();

    rig.release(b"yt-dlp v2");
    let mut env = rig.env();
    env.rename = Box::new(|_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let err = rig.tools_with(env).install_or_update().unwrap_err();
    assert_eq!(err, ToolError::InUse(Tool::Ytdlp));
    assert_eq!(
        err.to_string(),
        "Update failed: yt-dlp is in use; try again"
    );
    assert_eq!(read(&rig.ytdlp_path()), b"yt-dlp v1");
    assert_eq!(rig.files(), files_before, "the temporary file is deleted");
}

#[test]
fn a_first_install_whose_rename_fails_leaves_nothing_but_the_folder() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let mut env = rig.env();
    env.rename = Box::new(|_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let err = rig.tools_with(env).install_or_update().unwrap_err();
    assert_eq!(err, ToolError::InUse(Tool::Ytdlp));
    assert_eq!(rig.files(), Vec::<String>::new());
}

// ---- how tools are found -----------------------------------------------------------------

#[test]
fn the_tool_search_order() {
    let rig = Rig::new();
    let extra = rig.dir.path().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    let set_path = extra.join("set-yt-dlp");
    let on_path = extra.join("path-yt-dlp");
    std::fs::write(&set_path, b"s").unwrap();
    std::fs::write(&on_path, b"p").unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let env_with = |env_path: Option<PathBuf>, on_path: Option<PathBuf>| {
        let mut env = rig.env();
        env.env_path = env_path;
        let asked = Arc::clone(&asked);
        env.path_lookup = Box::new(move |name| {
            asked.lock().unwrap().push(name.to_owned());
            on_path.clone()
        });
        rig.tools_with(env)
    };

    // Nothing anywhere: none.
    assert_eq!(env_with(None, None).resolve(), None);
    assert_eq!(asked.lock().unwrap().as_slice(), [YTDLP_FILE]);

    // Only the managed copy: it, and it is ours.
    std::fs::create_dir_all(rig.tools_dir()).unwrap();
    std::fs::write(rig.ytdlp_path(), b"m").unwrap();
    let managed = env_with(None, None).resolve().unwrap();
    assert_eq!(
        (managed.ytdlp.clone(), managed.own, managed.qjs),
        (rig.ytdlp_path(), true, None)
    );

    // PATH beats the managed copy; the runtime is still the managed one.
    std::fs::write(rig.tools_dir().join(QJS_FILE), b"q").unwrap();
    let found = env_with(None, Some(on_path.clone())).resolve().unwrap();
    assert_eq!(found.ytdlp, on_path);
    assert!(!found.own);
    assert_eq!(found.qjs, Some(rig.tools_dir().join(QJS_FILE)));

    // The set path beats PATH.
    let found = env_with(Some(set_path.clone()), Some(on_path.clone()))
        .resolve()
        .unwrap();
    assert_eq!((found.ytdlp, found.own), (set_path.clone(), false));

    // A set path that is no file means no yt-dlp: it does not fall back to anything else.
    let missing = extra.join("missing");
    assert_eq!(
        env_with(Some(missing), Some(on_path.clone())).resolve(),
        None
    );
    assert_eq!(
        env_with(Some(PathBuf::from("yt-dlp")), Some(on_path)).resolve(),
        None
    );
}

#[test]
fn a_yt_dlp_from_outside_is_not_replaced_but_the_runtime_is_installed() {
    let rig = Rig::new();
    let mut env = rig.env();
    let external = rig.dir.path().join("external");
    std::fs::write(&external, b"x").unwrap();
    env.path_lookup = Box::new(move |_| Some(external.clone()));
    let done = rig.tools_with(env).install_or_update().unwrap();
    assert_eq!(
        done,
        Installed {
            ytdlp: Change::NotManaged,
            qjs: Change::Installed
        }
    );
    assert!(
        rig.http.calls() == [qjs_url()],
        "no checksum list or yt-dlp was fetched"
    );
    assert!(!rig.ytdlp_path().exists());
}

#[test]
fn a_target_without_a_managed_install_says_so() {
    let rig = Rig::new();
    let mut env = rig.env();
    env.managed = None;
    assert_eq!(
        rig.tools_with(env).install_or_update(),
        Err(ToolError::Unsupported)
    );
    assert!(rig.http.calls().is_empty());
    assert!(!rig.tools_dir().exists());
}

// ---- the daily update --------------------------------------------------------------------

#[test]
fn the_update_runs_once_a_day() {
    let rig = Rig::new();
    rig.release(b"yt-dlp v1");
    let clock = Arc::new(Mutex::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let mut env = rig.env();
    let c = Arc::clone(&clock);
    env.now = Box::new(move || *c.lock().unwrap());
    let tools = rig.tools_with(env);
    let advance = |hours: u64| {
        let mut t = clock.lock().unwrap();
        *t += Duration::from_secs(hours * 3600);
    };

    assert!(tools.update_if_due().unwrap().is_some(), "no stamp: due");
    let calls = rig.http.calls().len();
    advance(1);
    assert_eq!(tools.update_if_due(), Ok(None), "an hour later: not due");
    advance(22);
    assert_eq!(tools.update_if_due(), Ok(None), "23 hours later: not due");
    assert_eq!(
        rig.http.calls().len(),
        calls,
        "nothing was fetched while not due"
    );
    advance(2);
    assert!(
        tools.update_if_due().unwrap().is_some(),
        "25 hours later: due"
    );
    // A clock set back puts the stamp in the future: due.
    *clock.lock().unwrap() = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    assert!(tools.update_if_due().unwrap().is_some());
}

#[test]
fn a_failed_update_does_not_start_the_day() {
    let rig = Rig::new();
    let tools = rig.tools();
    assert!(tools.update_if_due().is_err());
    assert!(!rig.tools_dir().join("update-stamp").exists());
    rig.release(b"yt-dlp v1");
    assert!(tools.update_if_due().unwrap().is_some(), "still due");
}

// ---- no secret in a message ---------------------------------------------------------------

#[test]
fn error_texts_are_fixed_and_hold_no_url_or_query() {
    let all = [
        ToolError::Unsupported,
        ToolError::Http(HttpError::Refused),
        ToolError::Http(HttpError::Redirect),
        ToolError::Http(HttpError::TooLarge),
        ToolError::Http(HttpError::Timeout),
        ToolError::Http(HttpError::Network),
        ToolError::Download,
        ToolError::BadSignature,
        ToolError::BadSums,
        ToolError::HashMismatch,
        ToolError::InUse(Tool::Ytdlp),
        ToolError::InUse(Tool::Qjs),
        ToolError::Disk,
    ];
    for e in all {
        let text = format!("{e} | {e:?}");
        for bad in [
            "http:",
            "https:",
            "://",
            "?",
            "sig=",
            "cookie",
            "SAPISID",
            "Authorization",
        ] {
            assert!(!text.contains(bad), "{text} contains {bad}");
        }
    }
}

// ---- the real yt-dlp key and signature -----------------------------------------------------

const REAL_SUMS: &[u8] = include_bytes!("data/ytdlp-SHA2-256SUMS");
const REAL_SIG: &[u8] = include_bytes!("data/ytdlp-SHA2-256SUMS.sig");
const REAL_KEY: &str = include_str!("../src/tools/ytdlp-public.key");

#[test]
fn the_real_release_signature_verifies_with_the_compiled_in_key() {
    assert!(verify_signature(&Trust::Compiled, REAL_SUMS, REAL_SIG));
    // The same key given as a custom anchor verifies too, so the fingerprint pin is not what
    // made the compiled-in case pass.
    assert!(verify_signature(
        &Trust::Custom(REAL_KEY.to_owned()),
        REAL_SUMS,
        REAL_SIG
    ));
    // The list has a line for the Windows asset (yt-dlp 2026.10.26).
    assert_eq!(
        hex(&sums_hash(REAL_SUMS, "yt-dlp.exe").unwrap()),
        "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d"
    );
    assert!(sums_hash(REAL_SUMS, "yt-dlp_macos").is_ok());
}

#[test]
fn the_real_signature_rejects_changed_data_and_a_foreign_key() {
    let mut changed = REAL_SUMS.to_vec();
    changed[0] = if changed[0] == b'0' { b'1' } else { b'0' };
    assert!(!verify_signature(&Trust::Compiled, &changed, REAL_SIG));
    assert!(!verify_signature(
        &Trust::Compiled,
        REAL_SUMS,
        &REAL_SIG[..REAL_SIG.len() - 1]
    ));
    assert!(!verify_signature(&Trust::Compiled, REAL_SUMS, b""));
    // A signature by a test key is not the yt-dlp key's.
    let key = test_key();
    assert!(!verify_signature(
        &Trust::Compiled,
        REAL_SUMS,
        &sign(&key, REAL_SUMS)
    ));
    // And the real signature is not the test key's.
    assert!(!verify_signature(
        &Trust::Custom(key.armored_public.clone()),
        REAL_SUMS,
        REAL_SIG
    ));
}

#[test]
fn the_pinned_fingerprint_is_the_one_yt_dlp_publishes() {
    assert_eq!(
        whipspot_ytmusic::tools::YTDLP_KEY_FINGERPRINT,
        "AC0CBBE6848D6A873464AF4E57CF65933B5A7581"
    );
}
