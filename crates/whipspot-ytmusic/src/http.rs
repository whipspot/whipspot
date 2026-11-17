//! The adapter-internal synchronous HTTP trait and its host allowlist (D2).
//!
//! [`Http`] is what the rest of the adapter talks to and what its tests fake. The real
//! implementation, [`ReqwestHttp`], is a [`Client`] over a [`Wire`] (the reqwest blocking client).
//! Everything that makes a request safe lives in `Client`, above the wire, so tests run it over a
//! fake wire with no network:
//!
//! - one allowlist, checked on the one parsed `Url` the request is built from (the idea of
//!   `whipspot-net`'s `HostList`, copied rather than depended on: boundary rule R5);
//! - redirects are off at the wire and followed here by hand, only for a GitHub release download
//!   (`github.com` to `release-assets.githubusercontent.com`, at most three hops), with the
//!   caller's headers dropped;
//! - every body is read through a byte cap.
//!
//! Errors are fixed strings. A reqwest error carries its URL (and so a query); it is dropped at
//! the wire and never reaches a message (hard rule 1).
//!
//! Build and drop a [`ReqwestHttp`] on a plain thread, not on a tokio worker: reqwest's blocking
//! client owns a runtime of its own. Async callers call the methods through `spawn_blocking`.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::Url;

/// The longest URL text a caller may pass, checked before parsing. A signed stream URL runs to
/// a few KiB.
pub const MAX_URL_BYTES: usize = 8192;
/// The longest redirect target followed (a signed GitHub asset URL is long).
const MAX_REDIRECT_URL_BYTES: usize = 4096;
/// The most redirect hops followed for one request.
pub const MAX_REDIRECTS: usize = 3;
/// How long the connect step may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest body a call accepts. A body over it fails with [`HttpError::TooLarge`] and is
/// dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap(usize);

impl Cap {
    /// An InnerTube answer: 8 MiB.
    pub const INNERTUBE: Cap = Cap(8 << 20);
    /// A checksum list or a signature: 64 KiB.
    pub const SMALL: Cap = Cap(64 << 10);
    /// One range of audio: 16 MiB (the plan's three caps do not cover a media block).
    pub const MEDIA: Cap = Cap(16 << 20);
    /// A downloaded tool binary: 64 MiB.
    pub const BINARY: Cap = Cap(64 << 20);

    /// The cap in bytes.
    pub fn bytes(self) -> usize {
        self.0
    }

    /// The whole-request deadline: reqwest's blocking client has no separate read timeout, so a
    /// request has one limit from connect to the last body byte.
    fn timeout(self) -> Duration {
        if self == Cap::BINARY {
            Duration::from_secs(300)
        } else {
            Duration::from_secs(30)
        }
    }
}

/// Why a request did not complete. Every variant is a fixed message with nothing from the URL, a
/// header or the library's own error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// The URL, the method for that host or a header was not allowed. Nothing was sent.
    Refused,
    /// A redirect that is not a permitted GitHub release redirect.
    Redirect,
    /// A body over its cap.
    TooLarge,
    /// The request ran past its deadline.
    Timeout,
    /// Any other failure of the connection.
    Network,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HttpError::Refused => "Request refused: address not allowed",
            HttpError::Redirect => "Request refused: unexpected redirect",
            HttpError::TooLarge => "Response too large",
            HttpError::Timeout => "The request timed out",
            HttpError::Network => "Network error",
        })
    }
}

impl std::error::Error for HttpError {}

/// An answer: the status and the (capped) body. Headers are not kept (a `Set-Cookie` must not
/// travel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    /// A 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The `Content-Range` of a `206` answer, `bytes <start>-<end>/<total>`; `total` is `None` for
/// `*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

impl ContentRange {
    /// Reads a `Content-Range` header value; `None` for anything else (an unsatisfied range
    /// `bytes */n` included). Never panics.
    pub fn parse(text: &str) -> Option<ContentRange> {
        if text.len() > 64 {
            return None;
        }
        let rest = text.trim().strip_prefix("bytes ")?;
        let (span, total) = rest.split_once('/')?;
        let (start, end) = span.split_once('-')?;
        let start: u64 = start.parse().ok()?;
        let end: u64 = end.parse().ok()?;
        let total = match total {
            "*" => None,
            n => Some(n.parse::<u64>().ok()?),
        };
        (start <= end && total.is_none_or(|t| end < t)).then_some(ContentRange {
            start,
            end,
            total,
        })
    }
}

/// An answer to a ranged GET: like [`Response`], with the `Content-Range` header when the server
/// sent a readable one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_range: Option<ContentRange>,
}

/// The HTTP surface of the adapter. Synchronous: async callers use `spawn_blocking`.
pub trait Http: Send + Sync {
    /// GET `url`. `headers` are the caller's own (never `Host`, `Range`, `Content-Length`,
    /// `Connection` or `Transfer-Encoding`); `range` is an inclusive byte range, sent as a
    /// `Range` header.
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        cap: Cap,
    ) -> Result<Response, HttpError>;

    /// POST `body` as `application/json`. Only `music.youtube.com` takes a POST.
    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        cap: Cap,
    ) -> Result<Response, HttpError>;

    /// GET the inclusive byte range `range` of `url`, and report the `Content-Range` the server
    /// answered with. The default asks [`get`](Self::get) and reports none (a fake that cannot
    /// say).
    fn get_range(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: (u64, u64),
        cap: Cap,
    ) -> Result<RangeResponse, HttpError> {
        let r = self.get(url, headers, Some(range), cap)?;
        Ok(RangeResponse {
            status: r.status,
            body: r.body,
            content_range: None,
        })
    }
}

// ---- the allowlist ----------------------------------------------------------------------

/// One allowed host: exactly this name, or this name and its subdomains.
#[derive(Debug, Clone, Copy)]
enum Rule {
    Exact(&'static str),
    WithSubdomains(&'static str),
}

const ALLOWED: &[Rule] = &[
    Rule::Exact("music.youtube.com"),
    Rule::WithSubdomains("googlevideo.com"),
    Rule::Exact("github.com"),
    Rule::Exact("release-assets.githubusercontent.com"),
];

const MUSIC_HOST: &str = "music.youtube.com";
const GITHUB_HOST: &str = "github.com";
/// The hosts a GitHub release download may be redirected to.
const GITHUB_REDIRECT_TARGETS: &[&str] = &["github.com", "release-assets.githubusercontent.com"];

/// Passes only an `https` URL with no userinfo and no explicit port, whose host is a domain (an
/// IP literal, `0x7f.1` and `2130706433` have none), not `localhost` or a trailing-dot name, and
/// that an [`ALLOWED`] rule names.
fn check_url(url: &Url) -> Result<&str, HttpError> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(HttpError::Refused);
    }
    let domain = url.domain().ok_or(HttpError::Refused)?;
    if domain == "localhost" || domain.ends_with(".localhost") || domain.ends_with('.') {
        return Err(HttpError::Refused);
    }
    let ok = ALLOWED.iter().any(|rule| match *rule {
        Rule::Exact(h) => domain == h,
        Rule::WithSubdomains(h) => {
            domain == h
                || domain
                    .strip_suffix(h)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        }
    });
    if ok {
        Ok(domain)
    } else {
        Err(HttpError::Refused)
    }
}

/// The text length bound, then the parse, then [`check_url`]; returns the one `Url` the request
/// must be built from.
fn parse_checked(text: &str, max_len: usize) -> Result<Url, HttpError> {
    if text.len() > max_len {
        return Err(HttpError::Refused);
    }
    let url = Url::parse(text).map_err(|_| HttpError::Refused)?;
    check_url(&url)?;
    Ok(url)
}

/// Header names a caller may not set: the client owns them.
fn header_allowed(name: &str) -> bool {
    ![
        "host",
        "range",
        "content-length",
        "connection",
        "transfer-encoding",
    ]
    .iter()
    .any(|n| name.eq_ignore_ascii_case(n))
}

// ---- the wire ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    PostJson,
}

/// One request as the wire sends it: already checked, headers already screened.
pub struct WireRequest<'a> {
    pub method: Method,
    pub url: &'a Url,
    pub headers: &'a [(&'a str, &'a str)],
    pub range: Option<(u64, u64)>,
    pub body: &'a [u8],
    pub timeout: Duration,
}

/// What the wire got back: the head, and the body as a reader (not read yet).
pub struct WireResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    /// The `Content-Range` header's text, if there was one.
    pub content_range: Option<String>,
    pub body: Box<dyn Read + Send>,
}

/// The one thing a real transport does: send a request and give the head back. No redirects.
pub trait Wire: Send + Sync {
    fn send(&self, req: &WireRequest<'_>) -> Result<WireResponse, HttpError>;
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Reads `reader` to its end, at most `cap` bytes.
pub fn read_capped(reader: impl Read, cap: Cap) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    // One byte more than the cap: it tells "exactly the cap" from "over".
    reader
        .take(cap.bytes() as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                HttpError::Timeout
            } else {
                HttpError::Network
            }
        })?;
    if body.len() > cap.bytes() {
        return Err(HttpError::TooLarge);
    }
    Ok(body)
}

/// The checks around a [`Wire`]: allowlist, hand-followed redirects, body caps.
pub struct Client<W> {
    wire: W,
}

impl<W: Wire> Client<W> {
    pub fn with_wire(wire: W) -> Client<W> {
        Client { wire }
    }

    fn execute(
        &self,
        method: Method,
        url_text: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        body: &[u8],
        cap: Cap,
    ) -> Result<Response, HttpError> {
        self.execute_full(method, url_text, headers, range, body, cap)
            .map(|(r, _)| r)
    }

    /// [`execute`](Self::execute), also giving the `Content-Range` text of the final answer.
    fn execute_full(
        &self,
        method: Method,
        url_text: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        body: &[u8],
        cap: Cap,
    ) -> Result<(Response, Option<String>), HttpError> {
        let mut url = parse_checked(url_text, MAX_URL_BYTES)?;
        let first_host = check_url(&url)?;
        if method == Method::PostJson && first_host != MUSIC_HOST {
            return Err(HttpError::Refused);
        }
        if headers.iter().any(|(n, _)| !header_allowed(n)) {
            return Err(HttpError::Refused);
        }
        if range.is_some_and(|(a, b)| a > b) || (range.is_some() && method != Method::Get) {
            return Err(HttpError::Refused);
        }
        let timeout = cap.timeout();

        let mut hops = 0usize;
        let mut first = true;
        loop {
            // After a redirect: a plain GET with no caller headers and no range.
            let req = if first {
                WireRequest {
                    method,
                    url: &url,
                    headers,
                    range,
                    body,
                    timeout,
                }
            } else {
                WireRequest {
                    method: Method::Get,
                    url: &url,
                    headers: &[],
                    range: None,
                    body: &[],
                    timeout,
                }
            };
            let resp = self.wire.send(&req)?;

            if is_redirect(resp.status) {
                // Only a GET of a GitHub release asset is ever redirected.
                let from_github = check_url(&url).is_ok_and(|h| h == GITHUB_HOST);
                if req.method != Method::Get || !from_github {
                    return Err(HttpError::Redirect);
                }
                hops += 1;
                if hops > MAX_REDIRECTS {
                    return Err(HttpError::Redirect);
                }
                let location = resp.location.ok_or(HttpError::Redirect)?;
                let next = url.join(&location).map_err(|_| HttpError::Redirect)?;
                if next.as_str().len() > MAX_REDIRECT_URL_BYTES {
                    return Err(HttpError::Redirect);
                }
                let host = check_url(&next).map_err(|_| HttpError::Redirect)?;
                if !GITHUB_REDIRECT_TARGETS.contains(&host) {
                    return Err(HttpError::Redirect);
                }
                url = next;
                first = false;
                continue;
            }

            if resp.content_length.is_some_and(|n| n > cap.bytes() as u64) {
                return Err(HttpError::TooLarge);
            }
            let body = read_capped(resp.body, cap)?;
            return Ok((
                Response {
                    status: resp.status,
                    body,
                },
                resp.content_range,
            ));
        }
    }
}

impl<W: Wire> Http for Client<W> {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: Option<(u64, u64)>,
        cap: Cap,
    ) -> Result<Response, HttpError> {
        self.execute(Method::Get, url, headers, range, &[], cap)
    }

    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        cap: Cap,
    ) -> Result<Response, HttpError> {
        self.execute(Method::PostJson, url, headers, None, body, cap)
    }

    fn get_range(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        range: (u64, u64),
        cap: Cap,
    ) -> Result<RangeResponse, HttpError> {
        let (r, text) = self.execute_full(Method::Get, url, headers, Some(range), &[], cap)?;
        Ok(RangeResponse {
            status: r.status,
            body: r.body,
            content_range: text.as_deref().and_then(ContentRange::parse),
        })
    }
}

// ---- the real transport ------------------------------------------------------------------

/// The reqwest blocking client: native TLS, https only, no redirects, no cookie store.
pub struct ReqwestWire {
    client: reqwest::blocking::Client,
}

/// The real [`Http`].
pub type ReqwestHttp = Client<ReqwestWire>;

impl ReqwestHttp {
    /// Builds the client. See the module note on which thread to call this from.
    pub fn new() -> Result<ReqwestHttp, HttpError> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent("Whipspot")
            .build()
            .map_err(|_| HttpError::Network)?;
        Ok(Client::with_wire(ReqwestWire { client }))
    }
}

impl Wire for ReqwestWire {
    fn send(&self, req: &WireRequest<'_>) -> Result<WireResponse, HttpError> {
        let mut builder = match req.method {
            Method::Get => self.client.get(req.url.clone()),
            Method::PostJson => self
                .client
                .post(req.url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(req.body.to_vec()),
        };
        builder = builder.timeout(req.timeout);
        for (name, value) in req.headers {
            builder = builder.header(*name, *value);
        }
        if let Some((a, b)) = req.range {
            builder = builder.header(reqwest::header::RANGE, format!("bytes={a}-{b}"));
        }
        // The reqwest error carries the URL: it is classified and dropped here.
        let resp = builder.send().map_err(|e| {
            if e.is_timeout() {
                HttpError::Timeout
            } else if e.is_builder() {
                HttpError::Refused
            } else {
                HttpError::Network
            }
        })?;
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_length = resp.content_length();
        let content_range = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(WireResponse {
            status,
            location,
            content_length,
            content_range,
            body: Box::new(resp),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    /// One scripted answer of the fake wire.
    struct Answer {
        status: u16,
        location: Option<&'static str>,
        content_length: Option<u64>,
        body: Vec<u8>,
    }

    fn ok(body: &[u8]) -> Answer {
        Answer {
            status: 200,
            location: None,
            content_length: None,
            body: body.to_vec(),
        }
    }

    fn redirect(to: &'static str) -> Answer {
        Answer {
            status: 302,
            location: Some(to),
            content_length: None,
            body: Vec::new(),
        }
    }

    /// What the fake wire saw of one request.
    #[derive(Debug, PartialEq)]
    struct Seen {
        method: Method,
        url: String,
        headers: usize,
        range: Option<(u64, u64)>,
    }

    struct FakeWire {
        answers: Mutex<Vec<Answer>>,
        seen: Mutex<Vec<Seen>>,
    }

    impl FakeWire {
        fn new(mut answers: Vec<Answer>) -> FakeWire {
            answers.reverse();
            FakeWire {
                answers: Mutex::new(answers),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl Wire for &FakeWire {
        fn send(&self, req: &WireRequest<'_>) -> Result<WireResponse, HttpError> {
            self.seen.lock().unwrap().push(Seen {
                method: req.method,
                url: req.url.as_str().to_owned(),
                headers: req.headers.len(),
                range: req.range,
            });
            let a = self
                .answers
                .lock()
                .unwrap()
                .pop()
                .ok_or(HttpError::Network)?;
            Ok(WireResponse {
                status: a.status,
                location: a.location.map(str::to_owned),
                content_length: a.content_length,
                content_range: None,
                body: Box::new(Cursor::new(a.body)),
            })
        }
    }

    fn client(wire: &FakeWire) -> Client<&FakeWire> {
        Client::with_wire(wire)
    }

    #[test]
    fn content_range_is_read_strictly() {
        let ok = |text: &str| ContentRange::parse(text);
        assert_eq!(
            ok("bytes 0-524287/3449447"),
            Some(ContentRange {
                start: 0,
                end: 524_287,
                total: Some(3_449_447)
            })
        );
        assert_eq!(ok("bytes 5-9/*").map(|c| c.total), Some(None));
        for bad in [
            "",
            "bytes */100",
            "bytes 9-5/100",
            "bytes 0-100/100",
            "bytes 0-1/x",
            "bytes -1-5/10",
            "items 0-1/10",
            "bytes 0-18446744073709551616/9",
            &format!("bytes 0-1/{}", "9".repeat(80)),
        ] {
            assert_eq!(ContentRange::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_host_table() {
        let allowed = [
            "https://music.youtube.com/youtubei/v1/browse",
            "https://MUSIC.YOUTUBE.COM/x",
            "https://music.youtube.com:443/x",
            "https://rr4---sn-uphxqvujvh-30az.googlevideo.com/videoplayback?a=b",
            "https://googlevideo.com/x",
            "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS",
            "https://release-assets.githubusercontent.com/x?sig=y",
        ];
        for u in allowed {
            assert!(
                parse_checked(u, MAX_URL_BYTES).is_ok(),
                "should be allowed: {u}"
            );
        }
        let long = format!("https://github.com/{}", "a".repeat(MAX_URL_BYTES));
        let refused = [
            "http://music.youtube.com/x",
            "https://music.youtube.com@evil.example/x",
            "https://user:pw@music.youtube.com/x",
            "https://music.youtube.com./x",
            "https://music.youtube.com:8443/x",
            "https://www.youtube.com/x",
            "https://youtube.com/x",
            "https://evil.music.youtube.com/x",
            "https://evilgooglevideo.com/x",
            "https://googlevideo.com.evil.example/x",
            "https://x.github.com/x",
            "https://gist.github.com/x",
            "https://objects.githubusercontent.com/x",
            "https://githubusercontent.com/x",
            "https://127.0.0.1/x",
            "https://0x7f.1/x",
            "https://2130706433/x",
            "https://[::1]/x",
            "https://localhost/x",
            "https://a.localhost/x",
            "ftp://github.com/x",
            "file:///c:/windows/win.ini",
            "not a url",
            "",
            long.as_str(),
        ];
        for u in refused {
            assert!(
                parse_checked(u, MAX_URL_BYTES).is_err(),
                "should be refused: {u}"
            );
        }
    }

    #[test]
    fn a_refused_request_never_reaches_the_wire() {
        let wire = FakeWire::new(vec![ok(b"x")]);
        let c = client(&wire);
        for url in [
            "http://github.com/a",
            "https://evil.example/a",
            "https://github.com:444/a",
        ] {
            assert_eq!(c.get(url, &[], None, Cap::SMALL), Err(HttpError::Refused));
        }
        // A POST to any host but music.youtube.com, a caller-set Host or Range header, a
        // reversed range, and a range on a POST.
        assert_eq!(
            c.post_json("https://github.com/a", &[], b"{}", Cap::SMALL),
            Err(HttpError::Refused)
        );
        assert_eq!(
            c.get(
                "https://github.com/a",
                &[("Host", "evil.example")],
                None,
                Cap::SMALL
            ),
            Err(HttpError::Refused)
        );
        assert_eq!(
            c.get(
                "https://github.com/a",
                &[("rAnGe", "bytes=0-1")],
                None,
                Cap::SMALL
            ),
            Err(HttpError::Refused)
        );
        assert_eq!(
            c.get("https://github.com/a", &[], Some((5, 4)), Cap::SMALL),
            Err(HttpError::Refused)
        );
        assert!(wire.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_get_and_a_post_reach_the_wire_as_asked() {
        let wire = FakeWire::new(vec![ok(b"audio"), ok(b"{}")]);
        let c = client(&wire);
        let r = c
            .get(
                "https://rr1.googlevideo.com/videoplayback?x=1",
                &[("User-Agent", "x")],
                Some((0, 9)),
                Cap::MEDIA,
            )
            .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"audio".as_slice()));
        c.post_json(
            "https://music.youtube.com/youtubei/v1/browse",
            &[],
            b"{}",
            Cap::INNERTUBE,
        )
        .unwrap();
        let seen = wire.seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                Seen {
                    method: Method::Get,
                    url: "https://rr1.googlevideo.com/videoplayback?x=1".into(),
                    headers: 1,
                    range: Some((0, 9)),
                },
                Seen {
                    method: Method::PostJson,
                    url: "https://music.youtube.com/youtubei/v1/browse".into(),
                    headers: 0,
                    range: None,
                },
            ]
        );
    }

    #[test]
    fn a_github_release_redirect_is_followed_without_the_callers_headers() {
        let wire = FakeWire::new(vec![
            redirect("https://github.com/yt-dlp/yt-dlp/releases/download/1/yt-dlp.exe"),
            redirect("https://release-assets.githubusercontent.com/a/b?sig=SECRET"),
            ok(b"bin"),
        ]);
        let r = client(&wire)
            .get(
                "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
                &[("X-Probe", "1")],
                None,
                Cap::BINARY,
            )
            .unwrap();
        assert_eq!(r.body, b"bin");
        let seen = wire.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].headers, 1);
        assert_eq!((seen[1].headers, seen[2].headers), (0, 0));
        assert_eq!(
            seen[2].url,
            "https://release-assets.githubusercontent.com/a/b?sig=SECRET"
        );
    }

    #[test]
    fn a_relative_redirect_stays_on_github() {
        let wire = FakeWire::new(vec![
            redirect("/yt-dlp/yt-dlp/releases/download/1/s"),
            ok(b"s"),
        ]);
        let r = client(&wire)
            .get(
                "https://github.com/yt-dlp/yt-dlp/releases/latest/download/s",
                &[],
                None,
                Cap::SMALL,
            )
            .unwrap();
        assert_eq!(r.body, b"s");
        assert_eq!(
            wire.seen.lock().unwrap()[1].url,
            "https://github.com/yt-dlp/yt-dlp/releases/download/1/s"
        );
    }

    #[test]
    fn a_redirect_anywhere_else_is_refused() {
        let cases: Vec<(&str, &'static str)> = vec![
            ("https://github.com/a", "https://evil.example/a"),
            ("https://github.com/a", "http://github.com/a"),
            ("https://github.com/a", "https://github.com.evil.example/a"),
            ("https://github.com/a", "https://x.githubusercontent.com/a"),
            ("https://github.com/a", "https://127.0.0.1/a"),
            ("https://github.com/a", "https://user@github.com/a"),
            ("https://github.com/a", "https://github.com:8443/a"),
            ("https://github.com/a", "https://rr1.googlevideo.com/a"),
            // A redirect from a host that is not github.com is never followed.
            (
                "https://music.youtube.com/a",
                "https://consent.youtube.com/a",
            ),
            ("https://music.youtube.com/a", "https://github.com/a"),
            ("https://rr1.googlevideo.com/a", "https://github.com/a"),
            (
                "https://release-assets.githubusercontent.com/a",
                "https://github.com/a",
            ),
        ];
        for (from, to) in cases {
            let wire = FakeWire::new(vec![redirect(to), ok(b"never read")]);
            let r = client(&wire).get(from, &[], None, Cap::SMALL);
            assert_eq!(r, Err(HttpError::Redirect), "{from} -> {to}");
            assert_eq!(
                wire.seen.lock().unwrap().len(),
                1,
                "{from} -> {to}: no second request"
            );
        }
    }

    #[test]
    fn a_redirect_without_a_location_or_of_a_post_is_refused() {
        let wire = FakeWire::new(vec![Answer {
            status: 302,
            location: None,
            content_length: None,
            body: Vec::new(),
        }]);
        assert_eq!(
            client(&wire).get("https://github.com/a", &[], None, Cap::SMALL),
            Err(HttpError::Redirect)
        );
        let wire = FakeWire::new(vec![redirect("https://github.com/a")]);
        assert_eq!(
            client(&wire).post_json("https://music.youtube.com/a", &[], b"{}", Cap::SMALL),
            Err(HttpError::Redirect)
        );
    }

    #[test]
    fn the_hop_limit_is_three() {
        let hop = || redirect("https://github.com/n");
        let wire = FakeWire::new(vec![hop(), hop(), hop(), ok(b"three hops are fine")]);
        assert!(
            client(&wire)
                .get("https://github.com/a", &[], None, Cap::SMALL)
                .is_ok()
        );
        let wire = FakeWire::new(vec![hop(), hop(), hop(), hop(), ok(b"too late")]);
        assert_eq!(
            client(&wire).get("https://github.com/a", &[], None, Cap::SMALL),
            Err(HttpError::Redirect)
        );
        assert_eq!(wire.seen.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_body_over_its_cap_fails_and_one_at_the_cap_passes() {
        let at = vec![7u8; Cap::SMALL.bytes()];
        let wire = FakeWire::new(vec![ok(&at)]);
        let r = client(&wire)
            .get("https://github.com/a", &[], None, Cap::SMALL)
            .unwrap();
        assert_eq!(r.body.len(), Cap::SMALL.bytes());

        let over = vec![7u8; Cap::SMALL.bytes() + 1];
        let wire = FakeWire::new(vec![ok(&over)]);
        assert_eq!(
            client(&wire).get("https://github.com/a", &[], None, Cap::SMALL),
            Err(HttpError::TooLarge)
        );
    }

    #[test]
    fn a_declared_length_over_the_cap_is_refused_before_reading() {
        struct Boom;
        impl Read for Boom {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("the body must not be read");
            }
        }
        struct OneShot(Mutex<Option<WireResponse>>);
        impl Wire for OneShot {
            fn send(&self, _: &WireRequest<'_>) -> Result<WireResponse, HttpError> {
                self.0.lock().unwrap().take().ok_or(HttpError::Network)
            }
        }
        let wire = OneShot(Mutex::new(Some(WireResponse {
            status: 200,
            location: None,
            content_length: Some(Cap::SMALL.bytes() as u64 + 1),
            content_range: None,
            body: Box::new(Boom),
        })));
        assert_eq!(
            Client::with_wire(wire).get("https://github.com/a", &[], None, Cap::SMALL),
            Err(HttpError::TooLarge)
        );
    }

    #[test]
    fn the_caps_are_the_plans() {
        assert_eq!(Cap::INNERTUBE.bytes(), 8 * 1024 * 1024);
        assert_eq!(Cap::SMALL.bytes(), 64 * 1024);
        assert_eq!(Cap::BINARY.bytes(), 64 * 1024 * 1024);
    }

    #[test]
    fn a_non_success_status_is_an_answer_not_an_error() {
        let wire = FakeWire::new(vec![Answer {
            status: 404,
            location: None,
            content_length: None,
            body: b"nope".to_vec(),
        }]);
        let r = client(&wire)
            .get("https://github.com/a", &[], None, Cap::SMALL)
            .unwrap();
        assert_eq!(r.status, 404);
        assert!(!r.is_success());
    }

    #[test]
    fn error_texts_are_fixed_and_carry_no_url() {
        for e in [
            HttpError::Refused,
            HttpError::Redirect,
            HttpError::TooLarge,
            HttpError::Timeout,
            HttpError::Network,
        ] {
            let text = format!("{e} / {e:?}");
            assert!(
                !text.contains("http") && !text.contains('?') && !text.contains("sig"),
                "{text}"
            );
        }
    }

    #[test]
    fn the_real_client_refuses_before_it_connects() {
        // These fail in the allowlist, before any socket is opened: no network is touched.
        let http = ReqwestHttp::new().expect("client");
        for url in [
            "http://github.com/a",
            "https://evil.example/a",
            "https://127.0.0.1/a",
            "https://github.com:444/a",
            "https://user@github.com/a",
        ] {
            assert_eq!(
                http.get(url, &[], None, Cap::SMALL),
                Err(HttpError::Refused),
                "{url}"
            );
        }
        assert_eq!(
            http.post_json("https://github.com/a", &[], b"{}", Cap::SMALL),
            Err(HttpError::Refused)
        );
    }
}
