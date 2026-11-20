//! Client tests against a local mock HTTP server (std threads, plain HTTP on 127.0.0.1) and a
//! fake token backend: no Spotify, no credential store, no real network. Response bodies are synthetic,
//! modelled on Spotify's docs and on the key names the live probe reported; no real body was ever
//! captured. The injected clock stands in for time passing (nothing here sleeps).

use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde_json::json;

use super::*;
use crate::{
    accounts,
    auth::{Secret, Tokens},
    auth_manager::{AuthManager, BoxFuture, SlotBackend},
    web_auth::TokenError,
};
use whipspot_core::credential_store::CredentialStore;
use whipspot_core::provider::AlbumKind;
use whipspot_testkit::MemoryStore;

const A: &str = "4uLU6hMCjMI75M1A2tKUQC";
const B: &str = "0VjIjW4GlUZAMYd2vXMi3b";
const PL: &str = "1QkGCYuU2hrwb32Ku0nXYg";
const CLIENT_ID: &str = "0123456789abcdef0123456789abcdef";

// ---------------------------------------------------------------- mock server

struct Canned {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn ok(body: serde_json::Value) -> Canned {
    Canned {
        status: 200,
        headers: vec![],
        body: body.to_string(),
    }
}

fn status(code: u16) -> Canned {
    Canned {
        status: code,
        headers: vec![],
        body: "{}".into(),
    }
}

fn too_many(retry_after: Option<&str>) -> Canned {
    Canned {
        status: 429,
        headers: retry_after
            .map(|v| ("Retry-After", v.to_owned()))
            .into_iter()
            .collect(),
        // A 429 body is never used for decisions; make it tempting.
        body: json!({"error": {"status": 429, "message": "x", "reason": "QUOTA_EXCEEDED"}})
            .to_string(),
    }
}

#[derive(Debug, Clone)]
struct Recorded {
    request_line: String,
    authorization: Option<String>,
    content_length: Option<String>,
}

struct Mock {
    base: String,
    port: u16,
    hits: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    fn start(script: Vec<Canned>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s) = (hits.clone(), stop.clone());
        let mut script: VecDeque<Canned> = script.into();
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s.load(SeqCst) {
                    return;
                }
                let Ok(mut conn) = conn else { continue };
                let Some(rec) = read_request(&mut conn) else {
                    continue;
                };
                h.lock().unwrap().push(rec);
                let c = script.pop_front().unwrap_or_else(|| status(599));
                let mut out = format!("HTTP/1.1 {} X\r\n", c.status);
                for (k, v) in &c.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str(&format!(
                    "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    c.body.len(),
                    c.body
                ));
                let _ = conn.write_all(out.as_bytes());
            }
        });
        Mock {
            base: format!("http://127.0.0.1:{port}"),
            port,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn lines(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.request_line.clone())
            .collect()
    }

    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake accept()
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_request(conn: &mut TcpStream) -> Option<Recorded> {
    conn.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = conn.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.lines();
    let request_line = lines.next()?.to_owned();
    let headers: Vec<(&str, &str)> = lines.filter_map(|l| l.split_once(':')).collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_owned())
    };
    Some(Recorded {
        request_line,
        authorization: header("authorization"),
        content_length: header("content-length"),
    })
}

// ---------------------------------------------------------------- token fake and rig

/// Web slot backend: tokens are `web-access-<n>`; refresh failures can be scripted.
struct FakeWeb {
    refreshes: AtomicUsize,
    errors: Mutex<VecDeque<TokenError>>,
    /// Scopes named in each token response (empty = no `scope` field).
    scopes: Mutex<Vec<String>>,
}

impl SlotBackend for FakeWeb {
    fn refresh<'a>(
        &'a self,
        _: Option<&'a str>,
        _: &'a Secret,
    ) -> BoxFuture<'a, Result<Tokens, TokenError>> {
        Box::pin(async move {
            let n = self.refreshes.fetch_add(1, SeqCst) + 1;
            if let Some(e) = self.errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            Ok(Tokens {
                access: Secret::new(format!("web-access-{n}")),
                refresh: Secret::new(""),
                expires_at: Instant::now() + Duration::from_secs(3600),
                scopes: self.scopes.lock().unwrap().clone(),
            })
        })
    }

    fn login<'a>(
        &'a self,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Tokens, crate::auth_manager::AuthError>> {
        Box::pin(async { Err(crate::auth_manager::AuthError::LoginFailed) })
    }
}

struct Clock {
    base: Instant,
    offset_ms: AtomicUsize,
}

impl Clock {
    fn advance(&self, secs: u64) {
        self.offset_ms.fetch_add((secs * 1000) as usize, SeqCst);
    }
}

struct Rig {
    api: WebApi,
    mock: Mock,
    web: Arc<FakeWeb>,
    clock: Arc<Clock>,
}

fn rig_with_login(script: Vec<Canned>, logged_in: bool) -> Rig {
    let store = Arc::new(MemoryStore::default());
    if logged_in {
        store
            .set(accounts::WEB_REFRESH, &Secret::new("web-refresh"))
            .unwrap();
        store
            .set(accounts::WEB_CLIENT_ID, &Secret::new(CLIENT_ID))
            .unwrap();
    }
    let web = Arc::new(FakeWeb {
        refreshes: AtomicUsize::new(0),
        errors: Mutex::default(),
        scopes: Mutex::default(),
    });
    let playback = Arc::new(FakeWeb {
        refreshes: AtomicUsize::new(0),
        errors: Mutex::default(),
        scopes: Mutex::default(),
    });
    let auth = AuthManager::with_backends(store, playback, web.clone());
    let mock = Mock::start(script);
    let clock = Arc::new(Clock {
        base: Instant::now(),
        offset_ms: AtomicUsize::new(0),
    });
    let c = clock.clone();
    let api = WebApi::for_test(
        auth.web(),
        mock.base.clone(),
        Arc::new(move || c.base + Duration::from_millis(c.offset_ms.load(SeqCst) as u64)),
    );
    Rig {
        api,
        mock,
        web,
        clock,
    }
}

fn rig(script: Vec<Canned>) -> Rig {
    rig_with_login(script, true)
}

fn track(id: &str) -> serde_json::Value {
    json!({"type": "track", "uri": format!("spotify:track:{id}"), "name": "T", "duration_ms": 1000,
           "artists": [{"name": "Ar"}], "album": {"name": "Al"}})
}

fn items_page(entries: Vec<serde_json::Value>, next: serde_json::Value) -> serde_json::Value {
    json!({"items": entries, "total": 2, "next": next})
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn items_request_shape_and_bearer_only_in_the_header() {
    let r = rig(vec![ok(items_page(
        vec![json!({"item": track(A)}), json!({"track": track(B)})],
        json!(null),
    ))]);
    let page = r.api.playlist_items(PL, 0).await.unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(
        r.mock.lines(),
        [format!(
            "GET /playlists/{PL}/items?limit=50&offset=0 HTTP/1.1"
        )]
    );
    let rec = r.mock.hits.lock().unwrap()[0].clone();
    assert_eq!(rec.authorization.as_deref(), Some("Bearer web-access-1"));
    assert!(
        !rec.request_line.contains("web-access"),
        "token must not appear in the URL"
    );
}

#[tokio::test]
async fn first_429_closes_the_gate_and_later_calls_send_nothing() {
    let r = rig(vec![
        too_many(Some("26")),
        ok(items_page(vec![], json!(null))),
    ]);
    assert_eq!(
        r.api.playlist_items(PL, 0).await,
        Err(Error::RateLimited {
            retry_after_secs: 26
        })
    );
    assert_eq!(r.mock.count(), 1);
    assert_eq!(r.web.refreshes.load(SeqCst), 1);

    // Inside the window: every method fails fast, zero requests and no token refresh.
    r.clock.advance(10);
    for _ in 0..3 {
        assert_eq!(
            r.api.playlist_items(PL, 0).await,
            Err(Error::RateLimited {
                retry_after_secs: 16
            })
        );
        assert_eq!(
            r.api.liked_tracks(0).await,
            Err(Error::RateLimited {
                retry_after_secs: 16
            })
        );
        assert_eq!(
            r.api.playlists(0).await,
            Err(Error::RateLimited {
                retry_after_secs: 16
            })
        );
        assert!(matches!(r.api.me().await, Err(Error::RateLimited { .. })));
    }
    assert_eq!(
        r.mock.count(),
        1,
        "mock hit count must stay 1 while the gate is closed"
    );

    // After the window the next call is allowed again.
    r.clock.advance(16);
    assert!(r.api.playlist_items(PL, 0).await.is_ok());
    assert_eq!(r.mock.count(), 2);
}

#[tokio::test]
async fn a_429_without_a_usable_header_assumes_30_seconds() {
    for header in [
        None,
        Some("abc"),
        Some("-5"),
        Some("1.5"),
        Some("0"),
        Some("7 x"), // (a leading space is stripped by HTTP parsing; covered in gate.rs)
        Some("99999999999"),
    ] {
        let r = rig(vec![too_many(header)]);
        assert_eq!(
            r.api.liked_tracks(0).await,
            Err(Error::RateLimited {
                retry_after_secs: 30
            }),
            "{header:?}"
        );
        assert_eq!(r.mock.count(), 1);
    }
    let r = rig(vec![too_many(Some("9999999999"))]);
    assert_eq!(
        r.api.liked_tracks(0).await,
        Err(Error::RateLimited {
            retry_after_secs: 86_400
        })
    );
}

#[tokio::test]
async fn one_method_call_never_sends_more_than_one_request_after_a_429() {
    // playlists() = /me then /me/playlists: a 429 on /me stops there.
    let r = rig(vec![too_many(Some("5")), ok(json!({})), ok(json!({}))]);
    assert!(matches!(
        r.api.playlists(0).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(r.mock.count(), 1);
    // ... and a 429 on the second request stops there.
    let r = rig(vec![
        ok(json!({"id": "me"})),
        too_many(Some("5")),
        ok(json!({})),
    ]);
    assert!(matches!(
        r.api.playlists(0).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(r.mock.count(), 2);
}

#[tokio::test]
async fn a_token_endpoint_429_also_closes_the_gate() {
    let r = rig(vec![ok(items_page(vec![], json!(null)))]);
    r.web
        .errors
        .lock()
        .unwrap()
        .push_back(TokenError::RateLimited(Some(40)));
    assert_eq!(
        r.api.liked_tracks(0).await,
        Err(Error::RateLimited {
            retry_after_secs: 40
        })
    );
    assert_eq!(r.mock.count(), 0);
    assert_eq!(r.web.refreshes.load(SeqCst), 1);
    assert!(matches!(
        r.api.liked_tracks(0).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(
        r.web.refreshes.load(SeqCst),
        1,
        "no second token request while gated"
    );
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let r = rig(vec![
        status(401),
        ok(items_page(vec![json!({"track": track(A)})], json!(null))),
    ]);
    let page = r.api.liked_tracks(0).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(r.mock.count(), 2);
    assert_eq!(
        r.web.refreshes.load(SeqCst),
        2,
        "initial token + exactly one forced refresh"
    );
    let auth: Vec<_> = r
        .mock
        .hits
        .lock()
        .unwrap()
        .iter()
        .map(|h| h.authorization.clone().unwrap())
        .collect();
    assert_eq!(auth, ["Bearer web-access-1", "Bearer web-access-2"]);
}

#[tokio::test]
async fn two_401s_mean_not_logged_in_after_two_requests() {
    let r = rig(vec![status(401), status(401), ok(json!({}))]);
    assert_eq!(r.api.liked_tracks(0).await, Err(Error::WebNotLoggedIn));
    assert_eq!(r.mock.count(), 2);
    assert_eq!(r.web.refreshes.load(SeqCst), 2);
}

#[tokio::test]
async fn no_web_login_fails_without_any_request() {
    let r = rig_with_login(vec![], false);
    assert_eq!(r.api.liked_tracks(0).await, Err(Error::WebNotLoggedIn));
    assert_eq!(r.api.me().await, Err(Error::WebNotLoggedIn));
    assert_eq!(r.mock.count(), 0);
    assert_eq!(r.web.refreshes.load(SeqCst), 0);
}

#[tokio::test]
async fn forbidden_items_is_not_readable_and_is_not_retried() {
    let r = rig(vec![status(403), ok(items_page(vec![], json!(null)))]);
    assert_eq!(r.api.playlist_items(PL, 0).await, Err(Error::NotReadable));
    assert_eq!(r.mock.count(), 1);
    assert_eq!(r.web.refreshes.load(SeqCst), 1);
}

#[tokio::test]
async fn forbidden_elsewhere_means_the_app_has_no_access() {
    let r = rig(vec![status(403)]);
    assert_eq!(r.api.liked_tracks(0).await, Err(Error::AccessDenied));
    let r = rig(vec![status(403)]);
    assert_eq!(r.api.me().await, Err(Error::AccessDenied));
    let r = rig(vec![ok(json!({"id": "me"})), status(403)]);
    assert_eq!(r.api.playlists(0).await, Err(Error::AccessDenied));
}

#[tokio::test]
async fn other_statuses_are_errors_and_not_retried() {
    for code in [400u16, 404, 500, 502, 503] {
        let r = rig(vec![status(code), ok(json!({}))]);
        assert_eq!(r.api.liked_tracks(0).await, Err(Error::Http(code)));
        assert_eq!(r.mock.count(), 1, "{code}");
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let elsewhere = Mock::start(vec![ok(items_page(vec![], json!(null)))]);
    let redirect = Canned {
        status: 302,
        headers: vec![("Location", format!("{}/steal", elsewhere.base))],
        body: String::new(),
    };
    let r = rig(vec![redirect]);
    assert_eq!(r.api.liked_tracks(0).await, Err(Error::Http(302)));
    assert_eq!(r.mock.count(), 1);
    assert_eq!(
        elsewhere.count(),
        0,
        "the bearer must never be sent to the Location"
    );
}

#[tokio::test]
async fn a_hostile_next_url_is_never_requested() {
    let evil = Mock::start(vec![ok(items_page(vec![], json!(null)))]);
    let next = format!("{}/v1/me/tracks?offset=2", evil.base);
    let r = rig(vec![ok(items_page(
        vec![json!({"track": track(A)}), json!({"track": track(B)})],
        json!(next),
    ))]);
    let page = r.api.liked_tracks(0).await.unwrap();
    assert_eq!(page.next_offset, Some(2));
    assert_eq!(r.mock.count(), 1);
    assert_eq!(evil.count(), 0);
    // The client itself pages by offset against its own base.
    let r2 = rig(vec![ok(items_page(vec![], json!(null)))]);
    r2.api.liked_tracks(2).await.unwrap();
    assert_eq!(
        r2.mock.lines(),
        ["GET /me/tracks?limit=50&offset=2 HTTP/1.1"]
    );
}

#[tokio::test]
async fn playlist_ids_are_validated_before_any_request() {
    let r = rig(vec![]);
    let bad = [
        "",
        "short",
        &PL[..21],
        &format!("{PL}x"),
        "../x",
        "a/b",
        "%2e%2e%2f%2e%2e%2fxxxxxxxxx",
        &format!("{}%2e", &PL[..20]),
        "1QkGCYuU2hrwb32Ku0nXY?",
        "1QkGCYuU2hrwb32Ku0nXY#",
        "１QkGCYuU2hrwb32Ku0nXYg",
        &format!("{PL}\n"),
        &format!(" {PL}"),
    ];
    for id in bad {
        assert_eq!(
            r.api.playlist_items(id, 0).await,
            Err(Error::InvalidId),
            "{id:?}"
        );
    }
    assert_eq!(r.mock.count(), 0);
    assert_eq!(r.web.refreshes.load(SeqCst), 0);
}

#[tokio::test]
async fn offsets_beyond_the_row_cap_are_rejected_locally() {
    let r = rig(vec![]);
    assert_eq!(
        r.api.liked_tracks(MAX_ROWS).await,
        Err(Error::InvalidOffset)
    );
    assert_eq!(
        r.api.playlist_items(PL, u32::MAX).await,
        Err(Error::InvalidOffset)
    );
    assert_eq!(r.api.playlists(MAX_ROWS).await, Err(Error::InvalidOffset));
    assert_eq!(r.mock.count(), 0);
}

#[tokio::test]
async fn playlists_asks_for_me_once_and_predicts_readability() {
    let me = json!({"id": "me123"});
    let page = |n: &str| {
        json!({"items": [
            {"id": A, "name": "mine", "owner": {"id": "me123"}, "collaborative": false, "tracks": {"total": 3}},
            {"id": B, "name": "theirs", "owner": {"id": "other"}, "collaborative": false},
        ], "total": 2, "next": null, "x": n})
    };
    let r = rig(vec![ok(me), ok(page("1")), ok(page("2"))]);
    let p = r.api.playlists(0).await.unwrap();
    assert_eq!(p.items[0].readable, Readable::Readable);
    assert_eq!(p.items[1].readable, Readable::Unreadable);
    r.api.playlists(0).await.unwrap();
    assert_eq!(
        r.mock.lines(),
        [
            "GET /me HTTP/1.1".to_owned(),
            "GET /me/playlists?limit=50&offset=0 HTTP/1.1".to_owned(),
            "GET /me/playlists?limit=50&offset=0 HTTP/1.1".to_owned(),
        ]
    );
    assert_eq!(r.api.me().await.unwrap(), "me123");
    assert_eq!(r.mock.count(), 3, "me() is cached");
}

// S1 (M10 security review): the cached user id ends with the web login; the next login may be
// another account.
#[tokio::test]
async fn forget_user_asks_for_me_again() {
    let r = rig(vec![
        ok(json!({"id": "me123"})),
        ok(json!({"id": "other456"})),
    ]);
    assert_eq!(r.api.me().await.unwrap(), "me123");
    r.api.forget_user();
    assert_eq!(r.api.me().await.unwrap(), "other456");
    assert_eq!(r.api.me().await.unwrap(), "other456");
    assert_eq!(r.mock.count(), 2);
}

#[tokio::test]
async fn bad_bodies_are_unexpected_shape_not_empty_lists() {
    for body in ["not json", "{}", "[]", r#"{"items": 5}"#] {
        let c = Canned {
            status: 200,
            headers: vec![],
            body: body.into(),
        };
        let r = rig(vec![c]);
        assert_eq!(
            r.api.liked_tracks(0).await,
            Err(Error::UnexpectedShape),
            "{body}"
        );
    }
    let r = rig(vec![ok(json!({"display_name": "x"}))]);
    assert_eq!(r.api.me().await, Err(Error::UnexpectedShape));
}

#[tokio::test]
async fn an_unreachable_server_is_a_network_error() {
    let r = rig(vec![]);
    let port = r.mock.port;
    drop(r.mock); // the port is closed now
    let api = WebApi::for_test(
        AuthManager::with_backends(
            {
                let s = Arc::new(MemoryStore::default());
                s.set(accounts::WEB_REFRESH, &Secret::new("r")).unwrap();
                s.set(accounts::WEB_CLIENT_ID, &Secret::new(CLIENT_ID))
                    .unwrap();
                s
            },
            r.web.clone(),
            r.web.clone(),
        )
        .web(),
        format!("http://127.0.0.1:{port}"),
        Arc::new(Instant::now),
    );
    assert_eq!(api.liked_tracks(0).await, Err(Error::Network));
}

#[test]
fn errors_serialise_without_secrets_and_with_stable_kinds() {
    let e = Error::RateLimited {
        retry_after_secs: 26,
    };
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"rateLimited","message":"Spotify is rate-limiting this app. Try again in about 26 s.","retryAfterSecs":26}"#
    );
    let json = serde_json::to_string(&Error::NotReadable).unwrap();
    assert!(
        json.starts_with(r#"{"kind":"notReadable","message":"Spotify doesn't let this app read"#),
        "{json}"
    );
    assert!(json.ends_with(r#","retryAfterSecs":null}"#));
    for e in [
        Error::Http(500),
        Error::Network,
        Error::WebNotLoggedIn,
        Error::AccessDenied,
    ] {
        let s = serde_json::to_string(&e).unwrap();
        for leak in ["Bearer", "web-access", "http://", "https://"] {
            assert!(!s.contains(leak), "{leak} in {s}");
        }
    }
}

#[tokio::test]
async fn albums_album_tracks_and_search_request_shapes() {
    let album = json!({"items": [{"added_at": "x", "album": {"id": A, "name": "Al", "total_tracks": 2,
        "artists": [{"name": "Ar"}]}}], "total": 1, "next": null});
    let simple = json!({"type": "track", "uri": format!("spotify:track:{B}"), "name": "T",
        "duration_ms": 1000, "artists": [{"name": "Ar"}]});
    let tracks = json!({"items": [simple], "total": 1, "next": null});
    let found = json!({"tracks": {"items": [track(A)], "total": 1, "next": null}});
    let r = rig(vec![ok(album), ok(tracks), ok(found)]);
    let a = r.api.saved_albums(0).await.unwrap();
    assert_eq!((a.items.len(), a.items[0].id.as_str()), (1, A));
    let t = r.api.album_tracks(A, 0).await.unwrap();
    assert_eq!((t.items.len(), t.items[0].album.as_str()), (1, ""));
    // Trimmed, percent-encoded (space, ampersand, non-ASCII), 10 per page.
    let s = r.api.search_tracks("  a b&c é  ", 20).await.unwrap();
    assert_eq!(s.items.len(), 1);
    assert_eq!(
        r.mock.lines(),
        [
            "GET /me/albums?limit=50&offset=0 HTTP/1.1".to_owned(),
            format!("GET /albums/{A}/tracks?limit=50&offset=0 HTTP/1.1"),
            "GET /search?type=track&q=a+b%26c+%C3%A9&limit=10&offset=20 HTTP/1.1".to_owned(),
        ]
    );
}

#[tokio::test]
async fn new_endpoints_validate_locally_and_go_through_the_gate() {
    let r = rig(vec![too_many(Some("12")), ok(json!({}))]);
    for q in ["", "  ", "a\nb", &"x".repeat(MAX_TEXT_CHARS + 1)] {
        assert_eq!(
            r.api.search_tracks(q, 0).await,
            Err(Error::InvalidQuery),
            "{q:?}"
        );
    }
    assert_eq!(
        r.api.search_tracks("ok", SEARCH_MAX_ROWS).await,
        Err(Error::InvalidOffset)
    );
    assert_eq!(r.api.album_tracks("../x", 0).await, Err(Error::InvalidId));
    assert_eq!(
        r.api.saved_albums(MAX_ROWS).await,
        Err(Error::InvalidOffset)
    );
    assert_eq!(r.mock.count(), 0);
    // A 429 on one of them closes the gate for all of them.
    assert!(matches!(
        r.api.saved_albums(0).await,
        Err(Error::RateLimited {
            retry_after_secs: 12
        })
    ));
    assert!(matches!(
        r.api.search_tracks("ok", 0).await,
        Err(Error::RateLimited { .. })
    ));
    assert!(matches!(
        r.api.album_tracks(A, 0).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(r.mock.count(), 1);
}

#[tokio::test]
async fn new_endpoints_403_is_access_denied_and_a_non_page_is_unexpected() {
    let r = rig(vec![status(403), status(403), ok(json!({"albums": {}}))]);
    assert_eq!(r.api.saved_albums(0).await, Err(Error::AccessDenied));
    assert_eq!(r.api.album_tracks(A, 0).await, Err(Error::AccessDenied));
    assert_eq!(
        r.api.search_tracks("ok", 0).await,
        Err(Error::UnexpectedShape)
    );
}

// ---------------------------------------------------------------- likes (D50, MD71)

fn uri(id: &str) -> String {
    format!("spotify:track:{id}")
}

fn enc(id: &str) -> String {
    format!("spotify%3Atrack%3A{id}")
}

/// 45 distinct valid ids (22 base62 characters each).
fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{i:0>22}")).collect()
}

/// L1
#[tokio::test]
async fn liked_asks_contains_with_the_encoded_uris() {
    let r = rig(vec![ok(json!([true, false, true]))]);
    let uris = [uri(A), uri(B), uri(PL)];
    assert_eq!(r.api.liked(&uris).await.unwrap(), [true, false, true]);
    assert_eq!(
        r.mock.lines(),
        [format!(
            "GET /me/library/contains?uris={},{},{} HTTP/1.1",
            enc(A),
            enc(B),
            enc(PL)
        )]
    );
}

/// L2
#[tokio::test]
async fn forty_five_uris_go_in_batches_of_forty_and_five() {
    let first: Vec<bool> = (0..40).map(|i| i % 2 == 0).collect();
    let r = rig(vec![
        ok(json!(first)),
        ok(json!([true, true, true, true, true])),
    ]);
    let uris: Vec<String> = ids(45).iter().map(|i| uri(i)).collect();
    let out = r.api.liked(&uris).await.unwrap();
    assert_eq!(out.len(), 45);
    assert_eq!(out[..40], first[..]);
    assert_eq!(out[40..], [true; 5]);
    let lines = r.mock.lines();
    assert_eq!(lines.len(), 2);
    let count = |l: &str| l.matches("spotify%3Atrack%3A").count();
    assert_eq!((count(&lines[0]), count(&lines[1])), (40, 5));
}

/// L3
#[tokio::test]
async fn invalid_uris_are_false_and_never_sent() {
    let r = rig(vec![ok(json!([true]))]);
    let bad = "spotify:track:bad".to_owned();
    let uris = [
        bad.clone(),
        uri(A),
        format!("spotify:artist:{B}"),
        A.to_owned(),
        format!("spotify:album:{B}\n"),
    ];
    assert_eq!(
        r.api.liked(&uris).await.unwrap(),
        [false, true, false, false, false]
    );
    let lines = r.mock.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(&enc(A)));
    assert!(!lines[0].contains("bad") && !lines[0].contains("artist"));
    // Nothing valid: no request at all.
    assert_eq!(r.api.liked(&[bad]).await.unwrap(), [false]);
    assert_eq!(r.api.liked(&[]).await.unwrap(), Vec::<bool>::new());
    assert_eq!(r.mock.count(), 1);
}

/// L4
#[tokio::test]
async fn a_wrong_length_or_a_non_bool_answer_is_unexpected_shape() {
    let r = rig(vec![
        ok(json!([true])),
        ok(json!([true, "yes"])),
        ok(json!({"0": true, "1": false})),
        ok(json!([true, false, true])),
    ]);
    let uris = [uri(A), uri(B)];
    for _ in 0..4 {
        assert_eq!(r.api.liked(&uris).await, Err(Error::UnexpectedShape));
    }
}

/// L5
#[tokio::test]
async fn set_liked_puts_to_like_and_deletes_to_unlike() {
    let r = rig(vec![ok(json!({})), status(200)]);
    r.api.set_liked(&uri(A), true).await.unwrap();
    r.api.set_liked(&uri(B), false).await.unwrap();
    assert_eq!(
        r.mock.lines(),
        [
            format!("PUT /me/library?uris={} HTTP/1.1", enc(A)),
            format!("DELETE /me/library?uris={} HTTP/1.1", enc(B)),
        ]
    );
    // An empty body, sent as such.
    for h in r.mock.hits.lock().unwrap().iter() {
        assert_eq!(h.content_length.as_deref(), Some("0"));
    }
}

/// L5b: a non-JSON or empty write response is fine (the body is ignored); other statuses fail.
#[tokio::test]
async fn set_liked_ignores_the_response_body_and_maps_statuses() {
    let empty = Canned {
        status: 200,
        headers: vec![],
        body: String::new(),
    };
    let r = rig(vec![empty, status(500), status(404)]);
    r.api.set_liked(&uri(A), true).await.unwrap();
    assert_eq!(r.api.set_liked(&uri(A), true).await, Err(Error::Http(500)));
    assert_eq!(r.api.set_liked(&uri(A), false).await, Err(Error::Http(404)));
    assert!(r.api.can_change_likes());
    assert_eq!(r.api.set_liked("nope", true).await, Err(Error::InvalidId));
    assert_eq!(r.mock.count(), 3);
}

/// L6a: scopes unknown, a 403 on the write forgets the scope.
#[tokio::test]
async fn a_403_on_a_write_with_unknown_scopes_makes_the_heart_read_only() {
    let r = rig(vec![status(403)]);
    assert!(r.api.can_change_likes());
    assert_eq!(
        r.api.set_liked(&uri(A), true).await,
        Err(Error::AccessDenied)
    );
    assert!(!r.api.can_change_likes());
    // The next write sends nothing.
    assert_eq!(
        r.api.set_liked(&uri(A), true).await,
        Err(Error::AccessDenied)
    );
    assert_eq!(r.mock.count(), 1);
}

/// L6b: scopes known with modify, a 403 does not forget it.
#[tokio::test]
async fn a_403_with_the_scope_known_to_be_granted_keeps_the_heart_writable() {
    let r = rig(vec![status(403), ok(json!({}))]);
    *r.web.scopes.lock().unwrap() = vec!["user-library-read".into(), "user-library-modify".into()];
    assert_eq!(
        r.api.set_liked(&uri(A), true).await,
        Err(Error::AccessDenied)
    );
    assert!(r.api.can_change_likes());
    r.api.set_liked(&uri(A), true).await.unwrap();
    assert_eq!(r.mock.count(), 2);
}

/// L7
#[tokio::test]
async fn scopes_known_without_modify_send_nothing() {
    let r = rig(vec![ok(json!([true]))]);
    *r.web.scopes.lock().unwrap() = vec!["user-library-read".into()];
    // The scopes become known when the first token arrives (a read).
    assert!(r.api.can_change_likes());
    assert_eq!(r.api.liked(&[uri(A)]).await.unwrap(), [true]);
    assert!(!r.api.can_change_likes());
    assert_eq!(
        r.api.set_liked(&uri(A), true).await,
        Err(Error::AccessDenied)
    );
    assert_eq!(
        r.api.set_liked(&uri(A), false).await,
        Err(Error::AccessDenied)
    );
    assert_eq!(r.mock.count(), 1);
    // Reads still work; an empty `scope` field means unknown, so writable.
    let r = rig(vec![ok(json!([true]))]);
    r.api.liked(&[uri(A)]).await.unwrap();
    assert!(r.api.can_change_likes());
}

/// L8
#[tokio::test]
async fn a_429_on_contains_closes_the_gate_for_writes_too() {
    let r = rig(vec![too_many(Some("20")), ok(json!({}))]);
    assert_eq!(
        r.api.liked(&[uri(A)]).await,
        Err(Error::RateLimited {
            retry_after_secs: 20
        })
    );
    assert!(matches!(
        r.api.set_liked(&uri(A), true).await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(r.mock.count(), 1);
    r.clock.advance(21);
    r.api.set_liked(&uri(A), true).await.unwrap();
    assert_eq!(r.mock.count(), 2);
}

/// L9: every like request carries the bearer to the mock base, and a 401 on a write refreshes
/// once and retries once.
#[tokio::test]
async fn like_requests_carry_the_bearer_only_to_the_api_base() {
    let r = rig(vec![
        ok(json!([false])),
        status(401),
        ok(json!({})),
        ok(json!({})),
    ]);
    r.api.liked(&[uri(A)]).await.unwrap();
    r.api.set_liked(&uri(A), true).await.unwrap();
    r.api.set_liked(&uri(A), false).await.unwrap();
    let hits = r.mock.hits.lock().unwrap().clone();
    let auth: Vec<_> = hits
        .iter()
        .map(|h| h.authorization.clone().unwrap())
        .collect();
    assert_eq!(
        auth,
        [
            "Bearer web-access-1",
            "Bearer web-access-1",
            "Bearer web-access-2",
            "Bearer web-access-2"
        ]
    );
    for h in &hits {
        // Origin-form request lines: the path only, on the one connection the mock serves.
        assert!(
            h.request_line.contains(" /me/library"),
            "{}",
            h.request_line
        );
    }
    assert_eq!(r.web.refreshes.load(SeqCst), 2);
}

// ---------------------------------------------------------------- fingerprint (D12)

fn playlist_stamps(entries: &[(&str, &str)], next: serde_json::Value) -> serde_json::Value {
    let items: Vec<_> = entries
        .iter()
        .map(
            |(id, snap)| json!({"id": id, "name": "P", "snapshot_id": snap, "owner": {"id": "me"}}),
        )
        .collect();
    json!({"items": items, "total": 3, "next": next})
}

fn liked_stamp(total: u32, newest: Option<&str>) -> serde_json::Value {
    let items: Vec<_> = newest
        .map(|t| json!({"added_at": t, "track": track(A)}))
        .into_iter()
        .collect();
    json!({"items": items, "total": total, "next": null})
}

#[tokio::test]
async fn fingerprint_reads_playlist_stamps_in_pages_and_the_newest_liked_song() {
    let r = rig(vec![
        ok(playlist_stamps(&[(B, "snap-b")], json!("https://x/next"))),
        ok(playlist_stamps(&[(A, "snap-a")], json!(null))),
        ok(liked_stamp(7, Some("2026-03-04T05:06:07Z"))),
    ]);
    let fp = r.api.fingerprint().await.unwrap();
    // Sorted by id, whatever the order Spotify answered in.
    let mut want = vec![
        (B.to_owned(), "snap-b".to_owned()),
        (A.to_owned(), "snap-a".to_owned()),
    ];
    want.sort();
    assert_eq!(fp.playlists, want);
    assert!(fp.playlists[0].0 < fp.playlists[1].0);
    assert_eq!(fp.liked_total, 7);
    assert_eq!(fp.liked_newest.as_deref(), Some("2026-03-04T05:06:07Z"));
    assert_eq!(
        r.mock.lines(),
        [
            "GET /me/playlists?limit=50&offset=0 HTTP/1.1",
            "GET /me/playlists?limit=50&offset=1 HTTP/1.1",
            "GET /me/tracks?limit=1 HTTP/1.1",
        ]
    );
    // The bearer goes in the header only, as everywhere.
    for h in r.mock.hits.lock().unwrap().iter() {
        assert_eq!(h.authorization.as_deref(), Some("Bearer web-access-1"));
    }
}

#[tokio::test]
async fn fingerprint_with_no_liked_song_has_no_newest() {
    let r = rig(vec![
        ok(playlist_stamps(&[], json!(null))),
        ok(liked_stamp(0, None)),
    ]);
    let fp = r.api.fingerprint().await.unwrap();
    assert_eq!(fp.liked_total, 0);
    assert_eq!(fp.liked_newest, None);
}

#[tokio::test]
async fn fingerprint_reads_at_most_twenty_playlist_pages() {
    let mut script: Vec<Canned> = (0..25)
        .map(|_| ok(playlist_stamps(&[(A, "s")], json!("https://x/next"))))
        .collect();
    script.push(ok(liked_stamp(1, Some("t"))));
    let r = rig(script);
    // Twenty playlist pages, then the liked request, though the 20th still had a next page.
    let fp = r.api.fingerprint().await.unwrap();
    assert_eq!(fp.playlists.len(), 20);
    assert_eq!(fp.liked_newest, None);
    assert_eq!(
        r.mock.lines().last().unwrap(),
        "GET /me/tracks?limit=1 HTTP/1.1"
    );
    assert_eq!(r.mock.count(), 21);
}

#[tokio::test]
async fn fingerprint_failures_are_the_usual_errors_and_stop_at_once() {
    // Not a page.
    let r = rig(vec![ok(json!({"nope": 1}))]);
    assert_eq!(r.api.fingerprint().await, Err(Error::UnexpectedShape));
    assert_eq!(r.mock.count(), 1);
    // The liked total is required.
    let r = rig(vec![
        ok(playlist_stamps(&[], json!(null))),
        ok(json!({"items": []})),
    ]);
    assert_eq!(r.api.fingerprint().await, Err(Error::UnexpectedShape));
    // A 429 closes the gate for the later calls, as for every request.
    let r = rig(vec![too_many(Some("30"))]);
    assert_eq!(
        r.api.fingerprint().await,
        Err(Error::RateLimited {
            retry_after_secs: 30
        })
    );
    assert_eq!(
        r.api.fingerprint().await,
        Err(Error::RateLimited {
            retry_after_secs: 30
        })
    );
    assert_eq!(r.mock.count(), 1);
}

#[tokio::test]
async fn fingerprint_without_a_web_login_sends_no_request() {
    let r = rig_with_login(vec![], false);
    assert_eq!(r.api.fingerprint().await, Err(Error::WebNotLoggedIn));
    assert_eq!(r.mock.count(), 0);
    assert_eq!(r.web.refreshes.load(SeqCst), 0);
}

// ---------------------------------------------------------------- explore (artists, albums)

const ART: &str = "0OdUWJ0sBjDrqHygGUXeCF";

fn art_uri() -> String {
    format!("spotify:artist:{ART}")
}

fn alb_uri(id: &str) -> String {
    format!("spotify:album:{id}")
}

fn artist_body(name: &str) -> serde_json::Value {
    json!({"id": ART, "name": name, "images": [{"url": "https://i.scdn.co/p", "width": 640}]})
}

fn release_entry(id: &str, group: &str) -> serde_json::Value {
    json!({"id": id, "name": "R", "album_type": "album", "album_group": group,
           "artists": [{"uri": art_uri(), "name": "Maker"}]})
}

fn popular_body(n: usize) -> serde_json::Value {
    let tracks: Vec<_> = (0..n)
        .map(|i| {
            json!({"type": "track", "uri": format!("spotify:track:{i:0>22}"), "name": "P",
                   "duration_ms": 1000, "artists": [{"uri": art_uri(), "name": "Maker"}]})
        })
        .collect();
    json!({"tracks": {"items": tracks, "total": 99, "next": null}})
}

/// X1: an artist page is three requests, in this order, on the documented paths only.
#[tokio::test]
async fn an_artist_page_asks_the_artist_its_releases_and_a_filtered_search() {
    let releases = items_page(
        vec![release_entry(A, "album"), release_entry(B, "single")],
        json!("n"),
    );
    let r = rig(vec![
        ok(artist_body("Maker")),
        ok(releases),
        ok(popular_body(12)),
    ]);
    let page = r.api.artist_page(&art_uri()).await.unwrap();
    assert_eq!(
        r.mock.lines(),
        [
            format!("GET /artists/{ART} HTTP/1.1"),
            format!(
                "GET /artists/{ART}/albums?include_groups=album,single,compilation,appears_on&limit=10&offset=0 HTTP/1.1"
            ),
            "GET /search?type=track&limit=10&q=artist%3A%22Maker%22 HTTP/1.1".to_owned(),
        ]
    );
    assert_eq!(page.artist.name, "Maker");
    assert_eq!(page.releases.items.len(), 2);
    assert_eq!(page.releases.next_offset, Some(2));
    assert_eq!(page.popular.len(), 10);
    // The bearer travels in the header only.
    for h in r.mock.hits.lock().unwrap().iter() {
        assert_eq!(h.authorization.as_deref(), Some("Bearer web-access-1"));
        assert!(!h.request_line.contains("web-access"));
    }
}

/// X2: quotes in the name never reach the query, and an odd name is percent-encoded.
#[tokio::test]
async fn the_popular_query_strips_quotes_and_encodes_the_name() {
    let r = rig(vec![
        ok(artist_body("\"Weird\" & Co")),
        ok(items_page(vec![], json!(null))),
        ok(popular_body(0)),
    ]);
    r.api.artist_page(&art_uri()).await.unwrap();
    assert_eq!(
        r.mock.lines()[2],
        "GET /search?type=track&limit=10&q=artist%3A%22Weird+%26+Co%22 HTTP/1.1"
    );
}

/// X3: the Popular list is the one part that may fail; a rate limit still fails the page.
#[tokio::test]
async fn a_failed_popular_search_leaves_the_list_empty_but_a_429_fails_the_page() {
    let none = || ok(items_page(vec![], json!(null)));
    let r = rig(vec![
        ok(artist_body("Maker")),
        none(),
        status(500),
        ok(artist_body("Maker")),
        none(),
        ok(json!({"unexpected": true})),
        ok(artist_body("Maker")),
        none(),
        too_many(Some("7")),
    ]);
    assert!(
        r.api
            .artist_page(&art_uri())
            .await
            .unwrap()
            .popular
            .is_empty()
    );
    assert!(
        r.api
            .artist_page(&art_uri())
            .await
            .unwrap()
            .popular
            .is_empty()
    );
    assert!(matches!(
        r.api.artist_page(&art_uri()).await,
        Err(Error::RateLimited {
            retry_after_secs: 7
        })
    ));
}

/// X4: a bad uri or offset sends nothing; a foreign artist body is not the artist.
#[tokio::test]
async fn explore_arguments_are_validated_before_any_request() {
    let r = rig(vec![
        ok(artist_body("Maker")),
        ok(json!({"id": B, "name": "Other"})),
    ]);
    for bad in [
        "",
        ART,
        &format!("spotify:album:{ART}"),
        &format!("spotify:artist:{ART}\n"),
        &format!("spotify:artist:{}", &ART[..21]),
        "spotify:artist:../../me",
        &format!("spotify:artist:{}", "9".repeat(5000)),
    ] {
        assert_eq!(
            r.api.artist_page(bad).await,
            Err(Error::InvalidId),
            "{bad:?}"
        );
        assert_eq!(
            r.api.artist_releases(bad, ReleaseGroup::All, 0).await,
            Err(Error::InvalidId)
        );
    }
    for bad in [
        "",
        A,
        &art_uri(),
        &format!("spotify:album:{A}/x"),
        "spotify:album:%2e%2e",
    ] {
        assert_eq!(r.api.album(bad).await, Err(Error::InvalidId), "{bad:?}");
    }
    assert_eq!(
        r.api
            .artist_releases(&art_uri(), ReleaseGroup::All, MAX_ROWS)
            .await,
        Err(Error::InvalidOffset)
    );
    assert_eq!(r.mock.count(), 0);
    // The answer for another artist than the one asked for is refused.
    assert_eq!(
        r.api.artist_page(&format!("spotify:artist:{B}")).await,
        Err(Error::UnexpectedShape)
    );
    assert_eq!(r.mock.count(), 1);
}

/// X5: the release groups map onto the documented `include_groups`, paged by the offset given.
#[tokio::test]
async fn release_groups_and_offsets_reach_the_request() {
    let r = rig(vec![
        ok(items_page(vec![release_entry(A, "album")], json!(null))),
        ok(items_page(vec![release_entry(A, "single")], json!(null))),
        ok(items_page(vec![release_entry(A, "appears_on")], json!("n"))),
    ]);
    let albums = r
        .api
        .artist_releases(&art_uri(), ReleaseGroup::Albums, 50)
        .await
        .unwrap();
    let singles = r
        .api
        .artist_releases(&art_uri(), ReleaseGroup::Singles, 0)
        .await
        .unwrap();
    let guest = r
        .api
        .artist_releases(&art_uri(), ReleaseGroup::AppearsOn, 100)
        .await
        .unwrap();
    assert_eq!(
        r.mock.lines(),
        [
            format!(
                "GET /artists/{ART}/albums?include_groups=album,compilation&limit=10&offset=50 HTTP/1.1"
            ),
            format!("GET /artists/{ART}/albums?include_groups=single&limit=10&offset=0 HTTP/1.1"),
            format!(
                "GET /artists/{ART}/albums?include_groups=appears_on&limit=10&offset=100 HTTP/1.1"
            ),
        ]
    );
    assert_eq!(albums.items[0].kind, AlbumKind::Album);
    assert_eq!(singles.items[0].kind, AlbumKind::Single);
    assert_eq!(
        (guest.items[0].kind, guest.next_offset),
        (AlbumKind::AppearsOn, Some(101))
    );
}

/// A full page of 10 releases with a `next` URL pages on by exactly 10.
#[tokio::test]
async fn a_full_page_of_ten_releases_pages_on_by_ten() {
    let entries = (0..10)
        .map(|i| release_entry(&format!("{i:0>22}"), "album"))
        .collect();
    let r = rig(vec![ok(items_page(entries, json!("n")))]);
    let page = r
        .api
        .artist_releases(&art_uri(), ReleaseGroup::All, 20)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 10);
    assert_eq!(page.next_offset, Some(30));
}

/// X6: an album is one request; its tracks carry the album's name and link.
#[tokio::test]
async fn an_album_is_one_request_and_names_itself_on_its_tracks() {
    let simple = json!({"type": "track", "uri": format!("spotify:track:{B}"), "name": "One",
        "duration_ms": 1000, "artists": [{"uri": art_uri(), "name": "Maker"}]});
    let body = json!({"id": A, "name": "The Album", "album_type": "album", "total_tracks": 1,
        "artists": [{"uri": art_uri(), "name": "Maker"}], "release_date": "2001",
        "tracks": {"items": [simple], "total": 1, "next": null}});
    let r = rig(vec![ok(body), status(404), status(403)]);
    let d = r.api.album(&alb_uri(A)).await.unwrap();
    assert_eq!(r.mock.lines(), [format!("GET /albums/{A} HTTP/1.1")]);
    assert_eq!(d.album.uri, alb_uri(A));
    assert_eq!(d.tracks.items[0].album, "The Album");
    assert_eq!(
        d.tracks.items[0].album_ref.as_ref().map(|x| x.uri.clone()),
        Some(alb_uri(A))
    );
    assert_eq!(r.api.album(&alb_uri(A)).await, Err(Error::Http(404)));
    assert_eq!(r.api.album(&alb_uri(A)).await, Err(Error::AccessDenied));
}

/// X7: a 429 on any explore call closes the gate for the others.
#[tokio::test]
async fn a_429_on_an_explore_call_closes_the_gate() {
    let r = rig(vec![too_many(Some("9"))]);
    assert!(matches!(
        r.api.album(&alb_uri(A)).await,
        Err(Error::RateLimited { .. })
    ));
    assert!(matches!(
        r.api.artist_page(&art_uri()).await,
        Err(Error::RateLimited { .. })
    ));
    assert!(matches!(
        r.api
            .artist_releases(&art_uri(), ReleaseGroup::All, 0)
            .await,
        Err(Error::RateLimited { .. })
    ));
    assert_eq!(r.mock.count(), 1);
}

/// X8: albums are saved and read back through the same two calls as tracks, strict uri only.
#[tokio::test]
async fn albums_are_saved_and_checked_like_tracks() {
    let r = rig(vec![ok(json!([true, false])), ok(json!({})), ok(json!({}))]);
    let album = alb_uri(A);
    assert_eq!(
        r.api.liked(&[album.clone(), uri(B)]).await.unwrap(),
        [true, false]
    );
    r.api.set_liked(&album, true).await.unwrap();
    r.api.set_liked(&album, false).await.unwrap();
    let e = |u: &str| url::form_urlencoded::byte_serialize(u.as_bytes()).collect::<String>();
    assert_eq!(
        r.mock.lines(),
        [
            format!(
                "GET /me/library/contains?uris={},{} HTTP/1.1",
                e(&album),
                enc(B)
            ),
            format!("PUT /me/library?uris={} HTTP/1.1", e(&album)),
            format!("DELETE /me/library?uris={} HTTP/1.1", e(&album)),
        ]
    );
    // An artist, a bare id and an album uri with junk are not saveable.
    for bad in [
        art_uri(),
        A.to_owned(),
        format!("{album}x"),
        format!("{album}\n"),
    ] {
        assert_eq!(
            r.api.set_liked(&bad, true).await,
            Err(Error::InvalidId),
            "{bad:?}"
        );
    }
    assert_eq!(r.mock.count(), 3);
}

#[test]
fn waits_read_as_seconds_minutes_or_hours() {
    assert_eq!(super::wait_text(26), "26 s");
    assert_eq!(super::wait_text(89), "89 s");
    assert_eq!(super::wait_text(90), "2 min");
    assert_eq!(super::wait_text(3600), "1 h");
    assert_eq!(super::wait_text(44_515), "12 h 22 min");
    assert_eq!(
        Error::RateLimited {
            retry_after_secs: 44_515
        }
        .to_string(),
        "Spotify is rate-limiting this app. Try again in about 12 h 22 min."
    );
}
