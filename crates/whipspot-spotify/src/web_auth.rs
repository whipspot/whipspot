//! PKCE login against the user's OWN Spotify app, used only for Web API calls, plus the token
//! endpoint client (code exchange and refresh) shared with the playback login.
//!
//! Playback keeps the librespot client id and its login (`auth.rs`); this module is separate.
//! `librespot-oauth` does not check the CSRF `state`, so the flow is implemented here
//! (Spotify PKCE docs: <https://developer.spotify.com/documentation/web-api/tutorials/code-pkce-flow>).
//!
//! Storage is not done here (see `credential_store` / `auth_manager`). Tokens live in [`Secret`]
//! and are never printed, logged or Debug-formatted; error text built here never includes a token
//! or a response body.

use std::{
    fmt,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::auth::{LoginError, Secret, Tokens};

pub mod consts {
    /// Must be registered verbatim as a Redirect URI in the user's Spotify app.
    pub const REDIRECT_URI: &str = "http://127.0.0.1:8899/callback";
    pub const LISTEN_PORT: u16 = 8899;
    pub const CALLBACK_PATH: &str = "/callback";
    pub const SCOPES: &[&str] = &[
        "playlist-read-private",
        "playlist-read-collaborative",
        "user-library-read",
        "user-library-modify",
    ];
    /// The scope saving and removing tracks needs (D50); `has_scope` is asked about it.
    pub const SCOPE_LIBRARY_MODIFY: &str = "user-library-modify";
    pub const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
    pub const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
    pub const STORE_REFRESH_ACCOUNT: &str = crate::accounts::WEB_REFRESH;
    pub const STORE_CLIENT_ID_ACCOUNT: &str = crate::accounts::WEB_CLIENT_ID;
    pub const LOGIN_TIMEOUT_SECS: u64 = 120;
}

// ---------------------------------------------------------------- pure helpers

/// A Spotify client id is 32 hex characters.
pub fn is_client_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn random_b64url(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("OS random source failed: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 7636 code_verifier: 32 bytes from the OS CSPRNG, base64url without padding = 43 chars
/// from the unreserved set (the minimum allowed length, 256 bits of entropy).
pub fn generate_verifier() -> Result<Secret> {
    random_b64url(32).map(Secret::new)
}

/// CSRF `state`: 128 bits from the OS CSPRNG.
pub fn generate_state() -> Result<String> {
    random_b64url(16)
}

/// RFC 7636 S256: BASE64URL-NOPAD(SHA256(ASCII(verifier))).
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The authorize URL (contains the challenge and state only; no secret).
pub fn authorize_url(client_id: &str, challenge: &str, state: &str) -> String {
    let mut u = url::Url::parse(consts::AUTHORIZE_URL).expect("constant URL");
    u.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", consts::REDIRECT_URI)
        .append_pair("scope", &consts::SCOPES.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge);
    u.into()
}

/// What one HTTP request to the loopback listener means.
#[derive(Debug)]
pub enum Callback {
    /// Not `GET /callback` (e.g. `/favicon.ico`): answer 404, keep listening.
    NotCallback,
    /// `/callback` with a missing/duplicated/wrong `state` or no `code`: answer 400, keep
    /// listening (a stray request must not be able to abort or complete the login).
    Invalid(&'static str),
    /// Valid `state` and Spotify reported an error (e.g. `access_denied`): stop.
    Denied(String),
    /// Valid `state` and an authorization code.
    Code(Secret),
}

/// Classify the HTTP request line (`GET /callback?code=..&state=.. HTTP/1.1`). The path must be
/// exactly `/callback`; `state` is compared in constant time.
pub fn parse_callback(request_line: &str, expected_state: &str) -> Callback {
    let mut parts = request_line.trim_end().split(' ');
    let (Some("GET"), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Callback::NotCallback;
    };
    if !version.starts_with("HTTP/") || !target.starts_with('/') {
        return Callback::NotCallback;
    }
    let Ok(u) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::NotCallback;
    };
    if u.path() != consts::CALLBACK_PATH {
        return Callback::NotCallback;
    }
    let mut state = Vec::new();
    let mut code = Vec::new();
    let mut error = Vec::new();
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "state" => state.push(v.into_owned()),
            "code" => code.push(v.into_owned()),
            "error" => error.push(v.into_owned()),
            _ => {}
        }
    }
    let [state] = state.as_slice() else {
        return Callback::Invalid("missing or repeated state");
    };
    if !bool::from(state.as_bytes().ct_eq(expected_state.as_bytes())) {
        return Callback::Invalid("state mismatch");
    }
    if let [e] = error.as_slice() {
        let e: String = e
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' '))
            .take(80)
            .collect();
        return Callback::Denied(e);
    }
    match code.as_slice() {
        [c] if !c.is_empty() => Callback::Code(Secret::new(c.clone())),
        _ => Callback::Invalid("missing or repeated code"),
    }
}

/// Parse a 2xx token endpoint body. Errors never include the body.
pub fn parse_token_response(json: &serde_json::Value) -> Result<Tokens> {
    let access = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("token response has no access_token")?;
    let refresh = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let expires_in = json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    let scopes = json
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    Ok(Tokens {
        access: Secret::new(access),
        refresh: Secret::new(refresh),
        expires_at: Instant::now() + Duration::from_secs(expires_in),
        scopes,
    })
}

// ---------------------------------------------------------------- loopback listener

fn respond(mut stream: TcpStream, status: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(msg.as_bytes());
    let _ = stream.flush();
}

/// Read the request line (up to 8 KiB, 2 s) from a connection.
fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut buf = [0u8; 8192];
    let mut n = 0;
    while n < buf.len() && !buf[..n].windows(2).any(|w| w == b"\r\n") {
        match stream.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let head = &buf[..n];
    let end = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(head.len());
    String::from_utf8(head[..end].to_vec()).ok()
}

/// Browser step of the PKCE login. BLOCKING: run it on a plain thread / `spawn_blocking`.
/// Binds 127.0.0.1:8899 first (clear error when the port is taken), calls `on_ready` with the
/// authorize URL, then accepts connections until a valid callback or `timeout`.
/// Returns the authorization code and the verifier to redeem it with.
pub fn authorize_blocking(
    client_id: &str,
    timeout: Duration,
    on_ready: impl FnOnce(&str),
) -> Result<(Secret, Secret)> {
    if !is_client_id(client_id) {
        bail!("client id must be 32 hex characters");
    }
    let verifier = generate_verifier()?;
    let state = generate_state()?;
    let url = authorize_url(client_id, &code_challenge(verifier.expose()), &state);

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, consts::LISTEN_PORT));
    let listener = TcpListener::bind(addr).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::Error::new(LoginError::PortInUse).context(format!(
                "port {} is already in use (another login running?); free it and retry",
                consts::LISTEN_PORT
            ))
        } else {
            anyhow!("cannot listen on {addr}: {e}")
        }
    })?;
    listener.set_nonblocking(true)?;
    on_ready(&url);

    let deadline = Instant::now() + timeout;
    let mut last_invalid: Option<&'static str> = None;
    loop {
        if Instant::now() >= deadline {
            let err = anyhow::Error::new(LoginError::Timeout);
            return Err(match last_invalid {
                Some(why) => err.context(format!(
                    "no valid callback within {}s (last rejected callback: {why})",
                    timeout.as_secs()
                )),
                None => err.context(format!("no callback within {}s", timeout.as_secs())),
            });
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => bail!("accept failed: {e}"),
        };
        let Some(line) = read_request_line(&mut stream) else {
            continue;
        };
        match parse_callback(&line, &state) {
            Callback::NotCallback => respond(stream, "404 Not Found", "not found"),
            Callback::Invalid(why) => {
                last_invalid = Some(why);
                respond(stream, "400 Bad Request", "invalid callback");
            }
            Callback::Denied(e) => {
                respond(stream, "200 OK", "whipspot: login was not completed.");
                return Err(anyhow::Error::new(LoginError::Denied)
                    .context(format!("Spotify reported an error: {e}")));
            }
            Callback::Code(code) => {
                respond(
                    stream,
                    "200 OK",
                    "whipspot: login complete, you can close this tab.",
                );
                return Ok((code, verifier));
            }
        }
    }
}

// ---------------------------------------------------------------- token endpoint

/// Why a token endpoint call failed. Display is fixed text; response bodies are never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// `invalid_grant`: the refresh token / code is no longer valid. Credentials must be dropped.
    InvalidGrant,
    /// `invalid_client`: wrong client id, or the redirect URI is not registered for it.
    InvalidClient,
    /// HTTP 429, with `Retry-After` seconds when it was plain digits.
    RateLimited(Option<u64>),
    /// Any other non-2xx status.
    Rejected(u16),
    /// Transport failure or timeout.
    Network,
    /// 2xx without a usable `access_token`.
    BadResponse,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenError::InvalidGrant => f.write_str("Spotify no longer accepts this login"),
            TokenError::InvalidClient => {
                f.write_str("Spotify rejected the client id or redirect URI")
            }
            TokenError::RateLimited(_) => f.write_str("Spotify is rate-limiting token requests"),
            TokenError::Rejected(code) => write!(f, "token endpoint returned HTTP {code}"),
            TokenError::Network => f.write_str("network error reaching Spotify"),
            TokenError::BadResponse => f.write_str("unexpected response from Spotify"),
        }
    }
}

impl std::error::Error for TokenError {}

/// `Retry-After` as plain ASCII digits (no sign, no decimals, at most 10 digits); anything else
/// is "not sent".
pub fn parse_retry_after(value: &str) -> Option<u64> {
    (!value.is_empty() && value.len() <= 10 && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// Classify a non-2xx token endpoint answer from its status, `Retry-After` header and the
/// `error` field of its JSON body (nothing else of the body is used).
pub fn classify_token_failure(
    status: u16,
    retry_after: Option<&str>,
    error: Option<&str>,
) -> TokenError {
    if status == 429 {
        return TokenError::RateLimited(retry_after.and_then(parse_retry_after));
    }
    match error {
        Some("invalid_grant") => TokenError::InvalidGrant,
        Some("invalid_client") => TokenError::InvalidClient,
        _ => TokenError::Rejected(status),
    }
}

async fn token_request(form: &[(&str, &str)]) -> Result<Tokens, TokenError> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| TokenError::Network)?;
    let resp = http
        .post(consts::TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|_| TokenError::Network)?;
    let status = resp.status();
    let retry = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.text().await.map_err(|_| TokenError::Network)?;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let error = json.get("error").and_then(|v| v.as_str());
        return Err(classify_token_failure(
            status.as_u16(),
            retry.as_deref(),
            error,
        ));
    }
    parse_token_response(&json).map_err(|_| TokenError::BadResponse)
}

/// Redeem an authorization code (PKCE: `code_verifier`, no client secret).
pub async fn exchange_code(
    client_id: &str,
    code: &Secret,
    verifier: &Secret,
) -> Result<Tokens, TokenError> {
    token_request(&[
        ("grant_type", "authorization_code"),
        ("code", code.expose()),
        ("redirect_uri", consts::REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", verifier.expose()),
    ])
    .await
}

/// Redeem a refresh token (PKCE client: `client_id`, no secret). Works for any PKCE client id,
/// the user's own and librespot's. Storing a rotated refresh token is the caller's job.
pub async fn refresh(client_id: &str, refresh_token: &Secret) -> Result<Tokens, TokenError> {
    token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose()),
        ("client_id", client_id),
    ])
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "st4te_-AZaz09";

    #[test]
    fn rfc7636_appendix_b_vector() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_valid_and_random() {
        let a = generate_verifier().unwrap();
        let b = generate_verifier().unwrap();
        assert_ne!(a.expose(), b.expose());
        let v = a.expose();
        assert!((43..=128).contains(&v.len()));
        assert!(
            v.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._~".contains(&c))
        );
        assert!(generate_state().unwrap().len() >= 22);
    }

    #[test]
    fn client_id_shape() {
        assert!(is_client_id("65b708073fc0480ea92a077233ca87bd"));
        assert!(!is_client_id(""));
        assert!(!is_client_id("65b708073fc0480ea92a077233ca87b"));
        assert!(!is_client_id("65b708073fc0480ea92a077233ca87bg"));
        assert!(!is_client_id("65b708073fc0480ea92a077233ca87bd0"));
    }

    #[test]
    fn authorize_url_has_required_params() {
        let u = url::Url::parse(&authorize_url(
            "65b708073fc0480ea92a077233ca87bd",
            "CHAL",
            "STATE",
        ))
        .unwrap();
        assert_eq!(
            u.origin().ascii_serialization(),
            "https://accounts.spotify.com"
        );
        assert_eq!(u.path(), "/authorize");
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:8899/callback");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CHAL");
        assert_eq!(q["state"], "STATE");
        assert_eq!(
            q["scope"],
            "playlist-read-private playlist-read-collaborative user-library-read user-library-modify"
        );
        assert!(!q["scope"].contains("streaming"));
    }

    fn line(target: &str) -> String {
        format!("GET {target} HTTP/1.1")
    }

    #[test]
    fn callback_accepts_valid_code() {
        match parse_callback(
            &line(&format!("/callback?code=AQ%2Bx&state={STATE}")),
            STATE,
        ) {
            Callback::Code(c) => assert_eq!(c.expose(), "AQ+x"),
            other => panic!("unexpected {other:?}"),
        }
        // Parameter order does not matter; unknown params are ignored.
        assert!(matches!(
            parse_callback(&line(&format!("/callback?state={STATE}&x=1&code=c")), STATE),
            Callback::Code(_)
        ));
    }

    #[test]
    fn callback_path_is_anchored() {
        for p in [
            "/callbackx",
            "/callback/",
            "/x/callback",
            "//callback",
            "/",
            "/favicon.ico",
            "/callback%2Fx",
        ] {
            assert!(
                matches!(
                    parse_callback(&line(&format!("{p}?code=c&state={STATE}")), STATE),
                    Callback::NotCallback
                ),
                "{p}"
            );
        }
        assert!(matches!(
            parse_callback("POST /callback?code=c HTTP/1.1", STATE),
            Callback::NotCallback
        ));
        assert!(matches!(
            parse_callback("garbage", STATE),
            Callback::NotCallback
        ));
        assert!(matches!(parse_callback("", STATE), Callback::NotCallback));
        assert!(matches!(
            parse_callback("GET @evil/callback HTTP/1.1", STATE),
            Callback::NotCallback
        ));
    }

    #[test]
    fn callback_rejects_bad_state() {
        for q in [
            "code=c".to_owned(),
            "code=c&state=".to_owned(),
            "code=c&state=wrong".to_owned(),
            format!("code=c&state={}x", STATE),
            format!("code=c&state={}", &STATE[..STATE.len() - 1]),
            format!("code=c&state={STATE}&state={STATE}"),
            format!("code=c&state=wrong&state={STATE}"),
        ] {
            assert!(
                matches!(
                    parse_callback(&line(&format!("/callback?{q}")), STATE),
                    Callback::Invalid(_)
                ),
                "{q}"
            );
        }
        // Even an error response is ignored without the right state.
        assert!(matches!(
            parse_callback(&line("/callback?error=access_denied&state=wrong"), STATE),
            Callback::Invalid(_)
        ));
    }

    #[test]
    fn callback_code_problems_and_denial() {
        for q in [
            format!("state={STATE}"),
            format!("code=&state={STATE}"),
            format!("code=a&code=b&state={STATE}"),
        ] {
            assert!(
                matches!(
                    parse_callback(&line(&format!("/callback?{q}")), STATE),
                    Callback::Invalid(_)
                ),
                "{q}"
            );
        }
        match parse_callback(
            &line(&format!("/callback?error=access_denied&state={STATE}")),
            STATE,
        ) {
            Callback::Denied(e) => assert_eq!(e, "access_denied"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn token_response_parsing_is_redacted() {
        let j = serde_json::json!({
            "access_token": "AQDsecretaccess", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "AQDsecretrefresh", "scope": "user-library-read playlist-read-private"
        });
        let t = parse_token_response(&j).unwrap();
        assert_eq!(t.access.expose(), "AQDsecretaccess");
        assert_eq!(t.refresh.expose(), "AQDsecretrefresh");
        assert_eq!(t.scopes, ["user-library-read", "playlist-read-private"]);
        assert!(!format!("{t:?}").contains("secret"));
        // No rotation on refresh: empty refresh token, which must not be stored.
        let j = serde_json::json!({"access_token": "a", "expires_in": 3600});
        assert!(!crate::auth::should_store_refresh(
            parse_token_response(&j).unwrap().refresh.expose()
        ));
        assert!(parse_token_response(&serde_json::json!({"error": "x"})).is_err());
        assert!(parse_token_response(&serde_json::json!({"access_token": ""})).is_err());
    }

    #[test]
    fn retry_after_is_plain_digits_only() {
        assert_eq!(parse_retry_after("26"), Some(26));
        assert_eq!(parse_retry_after("0"), Some(0));
        for bad in ["", "abc", "-5", "1.5", " 7", "7 ", "+7", "12345678901", "٣"] {
            assert_eq!(parse_retry_after(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn token_failures_are_classified() {
        use TokenError::*;
        assert_eq!(
            classify_token_failure(429, Some("26"), None),
            RateLimited(Some(26))
        );
        assert_eq!(
            classify_token_failure(429, None, Some("invalid_grant")),
            RateLimited(None)
        );
        assert_eq!(
            classify_token_failure(429, Some("x"), None),
            RateLimited(None)
        );
        assert_eq!(
            classify_token_failure(400, None, Some("invalid_grant")),
            InvalidGrant
        );
        assert_eq!(
            classify_token_failure(400, None, Some("invalid_client")),
            InvalidClient
        );
        assert_eq!(
            classify_token_failure(400, None, Some("invalid_request")),
            Rejected(400)
        );
        assert_eq!(classify_token_failure(503, None, None), Rejected(503));
        // Display is fixed text and never carries a body or header value.
        assert!(!RateLimited(Some(26)).to_string().contains("26"));
    }
}
