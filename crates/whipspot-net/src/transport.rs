//! The transport seam: one capped GET on an already-parsed `Url`. Tests inject canned responses
//! through [`Transport`]; production uses [`ReqwestTransport`].

use std::time::Duration;

use whipspot_core::provider::BoxFuture;

use crate::client;

/// Total time for one request, body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What a GET answered. `body` is read only for status 200.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    /// The `Retry-After` value, kept only when at most 64 bytes of visible ASCII.
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

/// Why a GET produced no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// Connection, TLS, protocol, timeout or scheme refusal.
    Network,
    /// The body is over the cap.
    TooLarge,
}

/// One GET with a body cap. Never panics; the `Url` is the one the request is built from.
pub trait Transport: Send + Sync {
    fn get<'a>(
        &'a self,
        url: &'a reqwest::Url,
        cap: usize,
    ) -> BoxFuture<'a, Result<Fetched, FetchError>>;
}

/// The production transport.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// `None` when the client cannot be built.
    pub fn new() -> Option<ReqwestTransport> {
        client::client(REQUEST_TIMEOUT, true).map(|client| ReqwestTransport { client })
    }
}

/// `Some` only for at most 64 bytes of visible ASCII (space to tilde).
fn clean_retry_after(value: &reqwest::header::HeaderValue) -> Option<String> {
    let b = value.as_bytes();
    if b.len() > 64 || !b.iter().all(|c| (0x20..=0x7e).contains(c)) {
        return None;
    }
    std::str::from_utf8(b).ok().map(str::to_owned)
}

impl Transport for ReqwestTransport {
    fn get<'a>(
        &'a self,
        url: &'a reqwest::Url,
        cap: usize,
    ) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        Box::pin(async move {
            // The same parsed `Url` the host check saw, never text; no per-request header.
            let mut resp = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(|_| FetchError::Network)?;
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(clean_retry_after);
            if status != 200 {
                return Ok(Fetched {
                    status,
                    retry_after,
                    body: Vec::new(),
                });
            }
            if resp.content_length().is_some_and(|n| n > cap as u64) {
                return Err(FetchError::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| FetchError::Network)? {
                if body.len().saturating_add(chunk.len()) > cap {
                    return Err(FetchError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Fetched {
                status,
                retry_after,
                body,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::*;

    const CAP: usize = 100;
    const TEST_TIMEOUT: Duration = Duration::from_millis(500);

    /// A loopback server: one thread, `respond` writes each answer; request heads are recorded.
    struct Server {
        port: u16,
        heads: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        fn start(respond: impl Fn(&mut TcpStream, usize) + Send + 'static) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let heads = Arc::new(Mutex::new(Vec::new()));
            let h = heads.clone();
            std::thread::spawn(move || {
                for (n, conn) in listener.incoming().enumerate() {
                    let Ok(mut conn) = conn else { continue };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match conn.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => break,
                        }
                    }
                    h.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    respond(&mut conn, n);
                }
            });
            Server { port, heads }
        }

        fn url(&self, path: &str) -> reqwest::Url {
            reqwest::Url::parse(&format!("http://127.0.0.1:{}{path}", self.port)).unwrap()
        }

        fn requests(&self) -> Vec<String> {
            self.heads.lock().unwrap().clone()
        }
    }

    /// A test-only transport: https not required, no system proxy.
    fn test_transport() -> ReqwestTransport {
        let client = client::builder(TEST_TIMEOUT, false)
            .no_proxy()
            .build()
            .unwrap();
        ReqwestTransport { client }
    }

    #[tokio::test]
    async fn r1_a_200_body_is_read() {
        let s = Server::start(|c, _| {
            let _ = c.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        });
        let got = test_transport().get(&s.url("/a"), CAP).await.unwrap();
        assert_eq!(got.status, 200);
        assert_eq!(got.body, b"hello");
        assert_eq!(got.retry_after, None);
    }

    #[tokio::test]
    async fn r1_content_length_over_the_cap_is_refused_unread() {
        // Headers only, then the connection stays open: reading the body would stall to the
        // timeout and answer Network, not TooLarge.
        let s = Server::start(|c, _| {
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nxx");
            std::thread::sleep(Duration::from_millis(1500));
        });
        let t = Instant::now();
        let got = test_transport().get(&s.url("/big"), CAP).await;
        assert_eq!(got, Err(FetchError::TooLarge));
        assert!(t.elapsed() < Duration::from_millis(400));
    }

    #[tokio::test]
    async fn r1_a_chunked_body_over_the_cap_is_too_large() {
        let s = Server::start(|c, _| {
            let mut out =
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                    .to_vec();
            for _ in 0..4 {
                out.extend_from_slice(b"32\r\n"); // 0x32 = 50 bytes
                out.extend_from_slice(&[b'x'; 50]);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
            let _ = c.write_all(&out);
        });
        let got = test_transport().get(&s.url("/chunked"), CAP).await;
        assert_eq!(got, Err(FetchError::TooLarge));
    }

    #[tokio::test]
    async fn r1_a_redirect_is_returned_and_never_followed() {
        let s = Server::start(|c, _| {
            let _ = c.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: /second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let got = test_transport().get(&s.url("/first"), CAP).await.unwrap();
        assert_eq!(got.status, 302);
        assert!(got.body.is_empty());
        std::thread::sleep(Duration::from_millis(100));
        let reqs = s.requests();
        assert_eq!(reqs.len(), 1, "the Location was requested: {reqs:?}");
        assert!(reqs[0].starts_with("GET /first "));
    }

    #[tokio::test]
    async fn r1_a_stalled_server_is_network_within_the_timeout() {
        let s = Server::start(|_, _| std::thread::sleep(Duration::from_millis(2500)));
        let t = Instant::now();
        let got = test_transport().get(&s.url("/stall"), CAP).await;
        assert_eq!(got, Err(FetchError::Network));
        assert!(t.elapsed() < Duration::from_millis(2000));
    }

    #[tokio::test]
    async fn r1_only_the_user_agent_identifies_us() {
        let s = Server::start(|c, _| {
            let _ = c.write_all(
                b"HTTP/1.1 429 Slow\r\nRetry-After: 17\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let got = test_transport().get(&s.url("/h"), CAP).await.unwrap();
        assert_eq!(got.status, 429);
        assert_eq!(got.retry_after.as_deref(), Some("17"));
        let head = s.requests().remove(0).to_ascii_lowercase();
        let ua = format!("user-agent: whipspot/{}\r\n", env!("CARGO_PKG_VERSION"));
        assert!(head.contains(&ua), "{head}");
        for banned in ["cookie:", "authorization:", "referer:"] {
            assert!(!head.contains(banned), "{banned} in {head}");
        }
    }

    #[test]
    fn retry_after_is_kept_only_when_short_visible_ascii() {
        use reqwest::header::HeaderValue;
        let v = |s: &str| clean_retry_after(&HeaderValue::from_str(s).unwrap());
        assert_eq!(v("120").as_deref(), Some("120"));
        assert_eq!(v(&"9".repeat(65)), None);
        assert!(v(&"9".repeat(64)).is_some());
        assert_eq!(
            clean_retry_after(&HeaderValue::from_bytes(b"1\x80").unwrap()),
            None
        );
    }

    #[tokio::test]
    async fn r2_the_production_client_refuses_http_before_connecting() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = ReqwestTransport::new().expect("client");
        let url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        assert_eq!(t.get(&url, CAP).await, Err(FetchError::Network));
        let accepted = listener.accept();
        assert!(
            matches!(&accepted, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a connection was opened: {accepted:?}"
        );
    }
}
