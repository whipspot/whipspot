//! `Lrclib`: the `LyricsSource` of the network adapter (C7, MD67). One fixed base URL, query
//! values only from the track, one request at a time, paced, Retry-After honoured, body capped,
//! JSON read leniently.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Url;
use whipspot_core::lyrics::{Lyrics, LyricsError, LyricsQuery, LyricsSource};
use whipspot_core::provider::BoxFuture;

use crate::allow::HostList;
use crate::lrc::{parse_plain, parse_synced};
use crate::transport::{FetchError, ReqwestTransport, Transport};

/// The one URL lookups are built from; never derived from input.
const BASE: &str = "https://lrclib.net/api/get";
/// The host `BASE` names, declared to the same check art uses.
const HOST: &str = "lrclib.net";
/// The most bytes a lyrics body may have.
const LYRICS_MAX_BYTES: usize = 512 * 1024;
/// The least time between two request starts: at most 43 a minute, under LRCLIB's about 45 for
/// a client without a registered identity.
const SPACING: Duration = Duration::from_millis(1_400);
/// Back-off when `Retry-After` is absent or unusable.
const DEFAULT_RETRY_SECS: u64 = 60;
/// The longest `Retry-After` honoured.
const MAX_RETRY_SECS: u64 = 3_600;
/// The most jitter added to a back-off, as a fraction.
const MAX_JITTER: f64 = 0.3;
/// Each query value is cut to this many characters.
const MAX_VALUE_CHARS: usize = 200;

/// Request timing, pure: times are offsets from one epoch, the jitter is injected.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    last_start: Option<Duration>,
    blocked_until: Option<Duration>,
}

impl Pacer {
    /// When a request wanted at `now` may start (at least [`SPACING`] after the previous start);
    /// records it as the latest start.
    pub(crate) fn next_start(&mut self, now: Duration) -> Duration {
        let start = match self.last_start {
            Some(last) => now.max(last + SPACING),
            None => now,
        };
        self.last_start = Some(start);
        start
    }

    /// Blocks requests for `retry_secs` (cut to the cap) plus `jitter` of it (clamped to
    /// 0..=0.3). Returns the length of the block.
    pub(crate) fn back_off(&mut self, now: Duration, retry_secs: u64, jitter: f64) -> Duration {
        let base = Duration::from_secs(retry_secs.min(MAX_RETRY_SECS));
        let jitter = if jitter.is_finite() {
            jitter.clamp(0.0, MAX_JITTER)
        } else {
            0.0
        };
        let block = base + base.mul_f64(jitter);
        self.blocked_until = Some(now + block);
        block
    }

    /// Whether a back-off is still running at `now`.
    pub(crate) fn blocked(&self, now: Duration) -> bool {
        self.blocked_until.is_some_and(|until| now < until)
    }
}

/// Seconds to back off from a `Retry-After` value: an integer is used (cut to the cap), absent or
/// anything else (a date, a negative, text) is [`DEFAULT_RETRY_SECS`].
pub(crate) fn retry_secs(header: Option<&str>) -> u64 {
    let Some(v) = header.map(str::trim) else {
        return DEFAULT_RETRY_SECS;
    };
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return DEFAULT_RETRY_SECS;
    }
    // All digits: too long for a u64 is certainly over the cap.
    v.parse::<u64>().unwrap_or(u64::MAX).min(MAX_RETRY_SECS)
}

/// A jitter fraction in 0..=0.3 from the std hasher's per-instance random keys.
fn random_jitter() -> f64 {
    let h = RandomState::new().build_hasher().finish();
    (h % 1_001) as f64 / 1_000.0 * MAX_JITTER
}

/// `s` trimmed and cut to [`MAX_VALUE_CHARS`] characters.
fn value(s: &str) -> String {
    s.trim().chars().take(MAX_VALUE_CHARS).collect()
}

/// The lookup URL for `q`, or `None` when title or artist is empty (no request is made).
fn url(q: &LyricsQuery) -> Option<Url> {
    let title = value(&q.title);
    let artist = value(&q.artist);
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    let album = value(&q.album);
    let seconds = q.duration_ms.saturating_add(500) / 1_000;
    let mut params: Vec<(&str, String)> = vec![("track_name", title), ("artist_name", artist)];
    if !album.is_empty() {
        params.push(("album_name", album));
    }
    if (1..=3_600).contains(&seconds) {
        params.push(("duration", seconds.to_string()));
    }
    Url::parse_with_params(BASE, params).ok()
}

/// Reads LRCLIB's JSON object. A field of the wrong type counts as absent.
pub(crate) fn parse(body: &[u8]) -> Result<Lyrics, LyricsError> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| LyricsError::BadResponse)?;
    let obj = json.as_object().ok_or(LyricsError::BadResponse)?;
    if obj.get("instrumental").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(Lyrics::Instrumental);
    }
    let text = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
    };
    if let Some(synced) = text("syncedLyrics") {
        let lines = parse_synced(synced);
        if !lines.is_empty() {
            return Ok(Lyrics::Synced(lines));
        }
    }
    if let Some(plain) = text("plainLyrics") {
        let lines = parse_plain(plain);
        if !lines.is_empty() {
            return Ok(Lyrics::Plain(lines));
        }
    }
    Ok(Lyrics::None)
}

/// Looks lyrics up on LRCLIB. Construct only in the composition root.
pub struct Lrclib {
    transport: Option<Arc<dyn Transport>>,
    hosts: HostList,
    /// Held across a whole request: one at a time.
    pacer: tokio::sync::Mutex<Pacer>,
    epoch: Instant,
}

impl Lrclib {
    /// When the HTTP client cannot be built every lookup answers `Unavailable`.
    pub fn new() -> Lrclib {
        let transport = ReqwestTransport::new().map(|t| Arc::new(t) as Arc<dyn Transport>);
        Lrclib::build(transport)
    }

    /// Test constructor: any transport.
    #[doc(hidden)]
    pub fn with_transport(transport: Arc<dyn Transport>) -> Lrclib {
        Lrclib::build(Some(transport))
    }

    fn build(transport: Option<Arc<dyn Transport>>) -> Lrclib {
        Lrclib {
            transport,
            hosts: HostList::new(&[HOST]),
            pacer: tokio::sync::Mutex::new(Pacer::default()),
            epoch: Instant::now(),
        }
    }

    async fn get(&self, q: &LyricsQuery) -> Result<Lyrics, LyricsError> {
        let Some(url) = url(q) else {
            return Ok(Lyrics::None);
        };
        if self.hosts.check(&url).is_err() {
            return Err(LyricsError::Unavailable);
        }
        let transport = self.transport.as_ref().ok_or(LyricsError::Unavailable)?;
        let mut pacer = self.pacer.lock().await;
        let now = self.epoch.elapsed();
        if pacer.blocked(now) {
            return Err(LyricsError::RateLimited);
        }
        let start = pacer.next_start(now);
        if start > now {
            tokio::time::sleep(start - now).await;
        }
        let fetched = transport
            .get(&url, LYRICS_MAX_BYTES)
            .await
            .map_err(|e| match e {
                FetchError::TooLarge | FetchError::Network => LyricsError::Unavailable,
            })?;
        match fetched.status {
            200 => parse(&fetched.body),
            404 => Ok(Lyrics::None),
            429 => {
                let secs = retry_secs(fetched.retry_after.as_deref());
                pacer.back_off(self.epoch.elapsed(), secs, random_jitter());
                Err(LyricsError::RateLimited)
            }
            _ => Err(LyricsError::Unavailable),
        }
    }
}

impl Default for Lrclib {
    fn default() -> Lrclib {
        Lrclib::new()
    }
}

impl LyricsSource for Lrclib {
    fn lookup<'a>(&'a self, query: &'a LyricsQuery) -> BoxFuture<'a, Result<Lyrics, LyricsError>> {
        Box::pin(self.get(query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::lyrics::LyricLine;

    fn q(title: &str, artist: &str, album: &str, ms: u64) -> LyricsQuery {
        LyricsQuery {
            artist: artist.into(),
            title: title.into(),
            album: album.into(),
            duration_ms: ms,
        }
    }

    fn pairs(u: &Url) -> Vec<(String, String)> {
        u.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn url_encodes_values_and_keeps_the_base() {
        let u = url(&q("a b&c#d é", "Ä/B?", "Al", 200_000)).unwrap();
        assert!(u.as_str().starts_with("https://lrclib.net/api/get?"));
        assert_eq!(u.host_str(), Some("lrclib.net"));
        assert_eq!(u.fragment(), None);
        assert_eq!(
            pairs(&u),
            vec![
                ("track_name".to_owned(), "a b&c#d é".to_owned()),
                ("artist_name".to_owned(), "Ä/B?".to_owned()),
                ("album_name".to_owned(), "Al".to_owned()),
                ("duration".to_owned(), "200".to_owned()),
            ]
        );
        // Only `&`-separated pairs: nothing from a value leaked into the structure.
        assert_eq!(u.query_pairs().count(), 4);
        assert!(HostList::new(&[HOST]).check(&u).is_ok());
    }

    #[test]
    fn url_leaves_out_what_is_empty_or_out_of_range() {
        let none = |u: Url| {
            let p = pairs(&u);
            (
                p.iter().any(|(k, _)| k == "album_name"),
                p.iter().any(|(k, _)| k == "duration"),
            )
        };
        assert_eq!(none(url(&q("t", "a", "", 100_000)).unwrap()), (false, true));
        assert_eq!(
            none(url(&q("t", "a", "  ", 100_000)).unwrap()),
            (false, true)
        );
        let dur = |ms: u64| {
            pairs(&url(&q("t", "a", "x", ms)).unwrap())
                .into_iter()
                .find(|(k, _)| k == "duration")
                .map(|(_, v)| v)
        };
        assert_eq!(dur(1_499).as_deref(), Some("1"));
        assert_eq!(dur(1_500).as_deref(), Some("2"));
        assert_eq!(dur(0), None);
        assert_eq!(dur(499), None);
        assert_eq!(dur(3_600_000).as_deref(), Some("3600"));
        assert_eq!(dur(3_601_000), None);
        assert_eq!(dur(u64::MAX), None);
    }

    #[test]
    fn url_needs_a_title_and_an_artist_and_cuts_long_values() {
        assert!(url(&q("", "a", "x", 1000)).is_none());
        assert!(url(&q("t", "  ", "x", 1000)).is_none());
        let u = url(&q(&"é".repeat(500), "a", "", 1000)).unwrap();
        assert_eq!(pairs(&u)[0].1.chars().count(), 200);
    }

    #[test]
    fn json_is_read_leniently() {
        use serde_json::json;
        let b = |v: serde_json::Value| v.to_string().into_bytes();
        let synced = "[00:02.00]b\n[00:01.00]a";
        let Ok(Lyrics::Synced(l)) =
            parse(&b(json!({"syncedLyrics": synced, "plainLyrics": "a\nb"})))
        else {
            panic!("synced expected")
        };
        assert_eq!(
            l,
            vec![
                LyricLine {
                    at_ms: 1_000,
                    text: "a".into()
                },
                LyricLine {
                    at_ms: 2_000,
                    text: "b".into()
                },
            ]
        );
        assert_eq!(
            parse(&b(json!({"syncedLyrics": null, "plainLyrics": "a\nb"}))),
            Ok(Lyrics::Plain(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            parse(&b(json!({"syncedLyrics": null, "plainLyrics": null}))),
            Ok(Lyrics::None)
        );
        assert_eq!(
            parse(&b(json!({"instrumental": true, "syncedLyrics": synced}))),
            Ok(Lyrics::Instrumental)
        );
        assert_eq!(
            parse(&b(json!({"instrumental": "yes", "plainLyrics": "x"}))),
            Ok(Lyrics::Plain(vec!["x".into()]))
        );
        // Wrong types count as absent.
        assert_eq!(
            parse(&b(json!({"syncedLyrics": 5, "plainLyrics": "p"}))),
            Ok(Lyrics::Plain(vec!["p".into()]))
        );
        assert_eq!(
            parse(&b(json!({"syncedLyrics": ["a"], "plainLyrics": ["b"]}))),
            Ok(Lyrics::None)
        );
        // Synced text with no usable line falls back to plain.
        assert_eq!(
            parse(&b(json!({"syncedLyrics": "[ar:x]", "plainLyrics": "p"}))),
            Ok(Lyrics::Plain(vec!["p".into()]))
        );
        assert_eq!(parse(&b(json!({}))), Ok(Lyrics::None));
        assert_eq!(parse(b"[1]"), Err(LyricsError::BadResponse));
        assert_eq!(parse(b"\"x\""), Err(LyricsError::BadResponse));
        assert_eq!(parse(b"{"), Err(LyricsError::BadResponse));
        assert_eq!(parse(b""), Err(LyricsError::BadResponse));
        assert_eq!(
            parse(b"{\"plainLyrics\":\"\xff\xfe\"}"),
            Err(LyricsError::BadResponse)
        );
        assert_eq!(parse(&[0xff, 0xfe]), Err(LyricsError::BadResponse));
        // Deep nesting is refused by the parser, not by a stack overflow.
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert_eq!(parse(deep.as_bytes()), Err(LyricsError::BadResponse));
    }

    #[test]
    fn retry_after_values() {
        assert_eq!(retry_secs(Some("7")), 7);
        assert_eq!(retry_secs(Some(" 12 ")), 12);
        assert_eq!(retry_secs(Some("0")), 0);
        assert_eq!(retry_secs(None), 60);
        assert_eq!(retry_secs(Some("")), 60);
        assert_eq!(retry_secs(Some("soon")), 60);
        assert_eq!(retry_secs(Some("-5")), 60);
        assert_eq!(retry_secs(Some("1.5")), 60);
        assert_eq!(retry_secs(Some("Wed, 21 Oct 2026 07:28:00 GMT")), 60);
        assert_eq!(retry_secs(Some("3600")), 3_600);
        assert_eq!(retry_secs(Some("99999")), 3_600);
        assert_eq!(retry_secs(Some(&"9".repeat(40))), 3_600);
    }

    #[test]
    fn pacer_spaces_starts() {
        let mut p = Pacer::default();
        let t = ms(1_000);
        assert_eq!(p.next_start(t), t);
        assert_eq!(p.next_start(t + ms(100)), t + ms(1_400));
        // A late request starts at once.
        assert_eq!(p.next_start(t + ms(10_000)), t + ms(10_000));
    }

    #[test]
    fn fifty_starts_never_exceed_43_in_a_minute() {
        let mut p = Pacer::default();
        let starts: Vec<Duration> = (0..50).map(|_| p.next_start(Duration::ZERO)).collect();
        for (i, s) in starts.iter().enumerate() {
            let in_window = starts[i..]
                .iter()
                .take_while(|t| **t <= *s + Duration::from_secs(60))
                .count();
            assert!(in_window <= 43, "window at {i}: {in_window}");
        }
    }

    #[test]
    fn back_off_blocks_for_the_time_plus_bounded_jitter() {
        let mut p = Pacer::default();
        let now = ms(5_000);
        assert!(!p.blocked(now));
        let block = p.back_off(now, 7, 0.0);
        assert_eq!(block, Duration::from_secs(7));
        assert!(p.blocked(now + ms(6_999)));
        assert!(!p.blocked(now + ms(7_000)));
        assert_eq!(p.back_off(now, 99_999, 0.0), Duration::from_secs(3_600));
        assert_eq!(p.back_off(now, 100, 0.3), Duration::from_secs(130));
        // Jitter is clamped to 0..=0.3 and a NaN counts as none.
        assert_eq!(p.back_off(now, 100, 5.0), Duration::from_secs(130));
        assert_eq!(p.back_off(now, 100, -1.0), Duration::from_secs(100));
        assert_eq!(p.back_off(now, 100, f64::NAN), Duration::from_secs(100));
        for _ in 0..100 {
            let j = random_jitter();
            assert!((0.0..=MAX_JITTER).contains(&j));
        }
    }
}
