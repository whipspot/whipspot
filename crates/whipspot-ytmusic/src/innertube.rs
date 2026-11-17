//! The InnerTube client: the one place that talks to `music.youtube.com`'s API (D1). Each request
//! carries the user's cookies and a `SAPISIDHASH` authorization, in the shape the web client
//! uses (the context and the hash follow the MIT-licensed ytmusicapi).
//!
//! The cookie value, the SAPISID and the `Authorization` header are built per request, passed to
//! [`Http`] and dropped; they never reach a log, an error or a `Debug` print (hard rule 1).
//!
//! The call is synchronous (it blocks on [`Http`]): async callers use `spawn_blocking`.

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::auth::{MSG_COOKIES_EXPIRED, YtAuth};
use crate::http::{Cap, Http, HttpError};

const HOST: &str = "music.youtube.com";
const ORIGIN: &str = "https://music.youtube.com";
const API_PATH: &str = "/youtubei/v1/";
/// The web client's own user agent, as ytmusicapi sends it.
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0";

/// The `Authorization` value of a request made at `unix_secs` with the cookie `sapisid`:
/// `SAPISIDHASH <ts>_<sha1 of "<ts> <sapisid> <origin>" in hex>`.
pub fn sapisid_hash(sapisid: &str, unix_secs: u64) -> String {
    let mut sha = Sha1::new();
    sha.update(format!("{unix_secs} {sapisid} {ORIGIN}").as_bytes());
    let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {unix_secs}_{hex}")
}

/// The calendar date (UTC) of a Unix time.
fn civil_date(unix_secs: u64) -> (i64, i64, i64) {
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `1.YYYYMMDD.01.00` for the UTC date of `unix_secs`.
pub fn client_version(unix_secs: u64) -> String {
    let (y, m, d) = civil_date(unix_secs);
    format!("1.{y:04}{m:02}{d:02}.01.00")
}

/// The request context of the `WEB_REMIX` client.
pub fn context(unix_secs: u64) -> Value {
    json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": client_version(unix_secs),
                "hl": "en",
            },
            "user": {},
        }
    })
}

/// Why a request gave no answer. Fixed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TubeError {
    /// The endpoint name is not one of the plain `browse`, `like/like` shape.
    BadEndpoint,
    /// No usable cookies are stored.
    NoCookies,
    /// YouTube answered 401 or 403: the cookies are no longer accepted.
    CookiesExpired,
    /// YouTube answered 429: too many requests.
    RateLimited,
    /// Any other status that is not a success.
    Status,
    Http(HttpError),
    /// The answer was not JSON.
    Unreadable,
}

impl fmt::Display for TubeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TubeError::BadEndpoint => f.write_str("Invalid request"),
            TubeError::NoCookies => f.write_str("Not signed in: import your cookies"),
            TubeError::CookiesExpired => f.write_str(MSG_COOKIES_EXPIRED),
            TubeError::RateLimited => f.write_str("YouTube Music asked to slow down"),
            TubeError::Status => f.write_str("YouTube Music answered with an error"),
            TubeError::Http(e) => write!(f, "{e}"),
            TubeError::Unreadable => f.write_str("YouTube Music's answer was not readable"),
        }
    }
}

impl std::error::Error for TubeError {}

impl From<HttpError> for TubeError {
    fn from(e: HttpError) -> Self {
        TubeError::Http(e)
    }
}

fn endpoint_ok(e: &str) -> bool {
    (1..=64).contains(&e.len())
        && e.bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'/')
        && !e.starts_with('/')
        && !e.ends_with('/')
        && !e.contains("//")
}

/// A continuation token as it may sit in a URL: nothing that needs more escaping than `%`.
pub fn token_ok(t: &str) -> bool {
    (1..=4096).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'%' | b'='))
}

/// Posts to InnerTube with the user's login.
pub struct InnerTube {
    http: Arc<dyn Http>,
    auth: YtAuth,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
}

impl InnerTube {
    pub fn new(http: Arc<dyn Http>, auth: YtAuth) -> InnerTube {
        InnerTube::with_clock(http, auth, Box::new(SystemTime::now))
    }

    pub fn with_clock(
        http: Arc<dyn Http>,
        auth: YtAuth,
        now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    ) -> InnerTube {
        InnerTube { http, auth, now }
    }

    /// POST `body` (a JSON object of the endpoint's own fields) to `endpoint` (`browse`, `search`,
    /// `like/like`, ...) and return the parsed answer. A 401 or 403 marks the cookies expired.
    pub fn post(&self, endpoint: &str, body: Value) -> Result<Value, TubeError> {
        self.post_inner(endpoint, body, None)
    }

    /// Like [`post`](Self::post) for the older kind of continuation, which names its token in the
    /// URL (`&ctoken=..&continuation=..`) next to the request's own body. A token with a
    /// character outside `A-Z a-z 0-9 - _ . % =` is refused.
    pub fn post_legacy_continuation(
        &self,
        endpoint: &str,
        body: Value,
        token: &str,
    ) -> Result<Value, TubeError> {
        if !token_ok(token) {
            return Err(TubeError::BadEndpoint);
        }
        self.post_inner(endpoint, body, Some(token))
    }

    fn post_inner(
        &self,
        endpoint: &str,
        body: Value,
        legacy_token: Option<&str>,
    ) -> Result<Value, TubeError> {
        if !endpoint_ok(endpoint) {
            return Err(TubeError::BadEndpoint);
        }
        let Value::Object(fields) = body else {
            return Err(TubeError::BadEndpoint);
        };
        let jar = match self.auth.cookies() {
            Ok(Some(jar)) => jar,
            _ => return Err(TubeError::NoCookies),
        };
        let sapisid = jar.sapisid().ok_or(TubeError::NoCookies)?;
        let unix = (self.now)()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        let mut request = context(unix);
        if let Value::Object(map) = &mut request {
            for (k, v) in fields {
                // The context is ours.
                if k != "context" {
                    map.insert(k, v);
                }
            }
        }
        let body = serde_json::to_vec(&request).map_err(|_| TubeError::BadEndpoint)?;

        let mut url = format!("https://{HOST}{API_PATH}{endpoint}?alt=json");
        if let Some(token) = legacy_token {
            url.push_str(&format!("&ctoken={token}&continuation={token}"));
        }
        let cookie = jar.header(HOST, &format!("{API_PATH}{endpoint}"), unix);
        let authorization = sapisid_hash(sapisid, unix);
        let headers = [
            ("Cookie", cookie.expose()),
            ("Authorization", authorization.as_str()),
            ("X-Origin", ORIGIN),
            ("Origin", ORIGIN),
            ("X-Goog-AuthUser", "0"),
            ("User-Agent", USER_AGENT),
        ];
        let answer = self.http.post_json(&url, &headers, &body, Cap::INNERTUBE)?;
        match answer.status {
            401 | 403 => {
                self.auth.mark_cookies_expired();
                return Err(TubeError::CookiesExpired);
            }
            429 => return Err(TubeError::RateLimited),
            s if !(200..300).contains(&s) => return Err(TubeError::Status),
            _ => {}
        }
        let value: Value =
            serde_json::from_slice(&answer.body).map_err(|_| TubeError::Unreadable)?;
        self.auth.clear_cookies_expired();
        Ok(value)
    }
}
