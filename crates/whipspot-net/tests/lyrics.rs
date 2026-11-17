//! P4, P6, P7: `Lrclib` over a fake transport. Nothing here touches the network.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use whipspot_core::lyrics::{Lyrics, LyricsError, LyricsQuery, LyricsSource};
use whipspot_core::provider::BoxFuture;
use whipspot_net::Lrclib;
use whipspot_net::transport::{FetchError, Fetched, Transport};
use whipspot_testkit::{LyricsOutcome, run_lyrics_outcome_contract, run_lyrics_source_contract};

type Answer = Box<dyn Fn(&reqwest::Url) -> Result<Fetched, FetchError> + Send + Sync>;

/// Answers by a closure over the request URL; records every call and the in-flight maximum.
struct Fake {
    answer: Answer,
    calls: Mutex<Vec<(Instant, String)>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    delay: Duration,
}

impl Fake {
    fn new(
        answer: impl Fn(&reqwest::Url) -> Result<Fetched, FetchError> + Send + Sync + 'static,
    ) -> Arc<Fake> {
        Arc::new(Fake {
            answer: Box::new(answer),
            calls: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            delay: Duration::from_millis(30),
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Transport for Fake {
    fn get<'a>(
        &'a self,
        url: &'a reqwest::Url,
        cap: usize,
    ) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        Box::pin(async move {
            assert_eq!(cap, 512 * 1024, "the lyrics body cap");
            self.calls
                .lock()
                .unwrap()
                .push((Instant::now(), url.as_str().to_owned()));
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            (self.answer)(url)
        })
    }
}

fn status(status: u16, retry_after: Option<&str>, body: &[u8]) -> Result<Fetched, FetchError> {
    Ok(Fetched {
        status,
        retry_after: retry_after.map(str::to_owned),
        body: body.to_vec(),
    })
}

fn q(title: &str) -> LyricsQuery {
    LyricsQuery {
        artist: "Artist".into(),
        title: title.into(),
        album: "Album".into(),
        duration_ms: 200_000,
    }
}

fn ok_body() -> Vec<u8> {
    json!({"syncedLyrics": "[00:02.00]b\n[00:01.00]a", "plainLyrics": "a\nb"})
        .to_string()
        .into_bytes()
}

#[tokio::test]
async fn a_hit_goes_to_the_fixed_base_with_the_track_in_the_query() {
    let fake = Fake::new(|_| status(200, None, &ok_body()));
    let lrclib = Lrclib::with_transport(fake.clone());
    let Ok(Lyrics::Synced(lines)) = lrclib.lookup(&q("Song & Dance")).await else {
        panic!("synced expected")
    };
    assert_eq!(lines.len(), 2);
    let calls = fake.calls.lock().unwrap();
    let url = reqwest::Url::parse(&calls[0].1).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("lrclib.net"));
    assert_eq!(url.path(), "/api/get");
    let p: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(p[0], ("track_name".into(), "Song & Dance".into()));
    assert!(p.contains(&("duration".into(), "200".into())));
}

#[tokio::test]
async fn an_empty_title_or_artist_makes_no_request() {
    let fake = Fake::new(|_| status(200, None, &ok_body()));
    let lrclib = Lrclib::with_transport(fake.clone());
    assert_eq!(lrclib.lookup(&q("  ")).await, Ok(Lyrics::None));
    let mut no_artist = q("T");
    no_artist.artist = String::new();
    assert_eq!(lrclib.lookup(&no_artist).await, Ok(Lyrics::None));
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn statuses_map_to_fixed_answers() {
    for (st, expect) in [
        (404, Ok(Lyrics::None)),
        (500, Err(LyricsError::Unavailable)),
        (503, Err(LyricsError::Unavailable)),
        (302, Err(LyricsError::Unavailable)),
        (403, Err(LyricsError::Unavailable)),
    ] {
        let fake = Fake::new(move |_| status(st, None, b"{}"));
        let lrclib = Lrclib::with_transport(fake);
        assert_eq!(lrclib.lookup(&q("T")).await, expect, "{st}");
    }
    for e in [FetchError::Network, FetchError::TooLarge] {
        let fake = Fake::new(move |_| Err(e));
        let lrclib = Lrclib::with_transport(fake);
        assert_eq!(
            lrclib.lookup(&q("T")).await,
            Err(LyricsError::Unavailable),
            "{e:?}"
        );
    }
    let fake = Fake::new(|_| status(200, None, b"[1,2]"));
    let lrclib = Lrclib::with_transport(fake);
    assert_eq!(lrclib.lookup(&q("T")).await, Err(LyricsError::BadResponse));
}

#[tokio::test]
async fn a_429_backs_off_and_later_lookups_make_no_request() {
    for header in [Some("7"), None, Some("99999"), Some("soon")] {
        let fake = Fake::new(move |_| status(429, header, b""));
        let lrclib = Lrclib::with_transport(fake.clone());
        assert_eq!(
            lrclib.lookup(&q("T")).await,
            Err(LyricsError::RateLimited),
            "{header:?}"
        );
        assert_eq!(fake.calls(), 1);
        for _ in 0..3 {
            assert_eq!(
                lrclib.lookup(&q("Other")).await,
                Err(LyricsError::RateLimited)
            );
        }
        assert_eq!(
            fake.calls(),
            1,
            "no request during the back-off ({header:?})"
        );
        // A query that never reaches the source is still answered at once.
        assert_eq!(lrclib.lookup(&q("")).await, Ok(Lyrics::None));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_lookups_reach_the_transport_one_at_a_time_and_spaced() {
    let fake = Fake::new(|_| status(200, None, &ok_body()));
    let lrclib = Arc::new(Lrclib::with_transport(fake.clone()));
    let tasks: Vec<_> = (0..2)
        .map(|i| {
            let l = lrclib.clone();
            tokio::spawn(async move { l.lookup(&q(&format!("T{i}"))).await })
        })
        .collect();
    for t in tasks {
        assert!(matches!(t.await.unwrap(), Ok(Lyrics::Synced(_))));
    }
    assert_eq!(fake.calls(), 2);
    assert_eq!(fake.max_in_flight.load(Ordering::SeqCst), 1);
    let calls = fake.calls.lock().unwrap();
    let gap = calls[1].0.duration_since(calls[0].0);
    assert!(gap >= Duration::from_millis(1_300), "gap {gap:?}");
}

#[tokio::test]
async fn the_lyrics_source_contract_holds() {
    let fake = Fake::new(|url| {
        let title = url
            .query_pairs()
            .find(|(k, _)| k == "track_name")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        if title == "Hit" {
            status(200, None, &ok_body())
        } else {
            status(404, None, b"{\"code\":404}")
        }
    });
    let lrclib = Lrclib::with_transport(fake.clone());
    run_lyrics_source_contract(&lrclib, &q("Hit"), &q("Miss")).await;
    // The empty title never reached the source: only the hit and the miss did.
    assert_eq!(fake.calls(), 2);
}

/// 1500 synced lines of 400 characters each, as a service might send.
fn oversize_body() -> Vec<u8> {
    let lines: Vec<String> = (0..1500)
        .map(|i| format!("[{:02}:{:02}.00]{}", i / 60, i % 60, "w".repeat(400)))
        .collect();
    json!({"syncedLyrics": lines.join("
")})
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn the_lyrics_outcome_matrix_holds() {
    type Body = Box<dyn Fn() -> Result<Fetched, FetchError> + Send + Sync>;
    let cases: Vec<(&str, LyricsOutcome, Body)> = vec![
        (
            "synced",
            LyricsOutcome::Synced,
            Box::new(|| status(200, None, &ok_body())),
        ),
        (
            "plain",
            LyricsOutcome::Plain,
            Box::new(|| {
                status(
                    200,
                    None,
                    json!({"plainLyrics": "one
two
three"})
                    .to_string()
                    .as_bytes(),
                )
            }),
        ),
        (
            "oversize",
            LyricsOutcome::Synced,
            Box::new(|| status(200, None, &oversize_body())),
        ),
        (
            "instrumental",
            LyricsOutcome::Instrumental,
            Box::new(|| status(200, None, br#"{"instrumental":true}"#)),
        ),
        (
            "empty",
            LyricsOutcome::NotFound,
            Box::new(|| status(200, None, br#"{"syncedLyrics":"","plainLyrics":null}"#)),
        ),
        (
            "missing",
            LyricsOutcome::NotFound,
            Box::new(|| status(404, None, b"{}")),
        ),
        (
            "down",
            LyricsOutcome::Error(LyricsError::Unavailable),
            Box::new(|| status(503, None, b"")),
        ),
        (
            "network",
            LyricsOutcome::Error(LyricsError::Unavailable),
            Box::new(|| Err(FetchError::Network)),
        ),
        (
            "slow",
            LyricsOutcome::Error(LyricsError::RateLimited),
            Box::new(|| status(429, Some("5"), b"")),
        ),
        (
            "junk",
            LyricsOutcome::Error(LyricsError::BadResponse),
            Box::new(|| status(200, None, b"not json")),
        ),
    ];
    for (title, outcome, body) in cases {
        // A fresh source per case: the pacing and the back-off are per source.
        let lrclib = Lrclib::with_transport(Fake::new(move |_| body()));
        run_lyrics_outcome_contract(&lrclib, &[(q(title), outcome)]).await;
    }
}
