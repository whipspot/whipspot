//! Lyrics lookups of C1 (MD67, MD68) on FakeLyrics. Every wait polls with a timeout; no fixed
//! sleep is an assertion.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
use whipspot_core::lyrics::{LyricLine, Lyrics, LyricsError, MAX_LYRIC_CHARS, MAX_LYRIC_LINES};
use whipspot_core::player::{
    self, EngineSetup, LyricsState, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle,
    PlayerState,
};
use whipspot_core::provider::{ProviderRegistry, Track};
use whipspot_testkit::{FakeLyrics, FakeOutput, FakeProvider, demo_lyrics};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Rig {
    h: PlayerHandle,
    lyrics: Arc<FakeLyrics>,
    /// Every lyrics state seen by the wake callback, in order.
    seen: Arc<Mutex<Vec<LyricsState>>>,
}

fn rig_with(lyrics: Arc<FakeLyrics>, enabled: bool) -> Rig {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: 44_100,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    );
    let engine = EngineSetup::Output {
        output: Box::new(fake),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(engine, 64);
    let mut reg = ProviderRegistry::new();
    reg.register(Arc::new(FakeProvider::new("fakea", true).with_context(ctx)))
        .unwrap();
    let config = PlayerConfig::default().with_lyrics(lyrics.clone(), enabled);
    let h = Player::start(reg, wiring, config, &tokio::runtime::Handle::current());
    let seen = Arc::new(Mutex::new(vec![]));
    let (s, hh) = (seen.clone(), h.clone());
    h.set_wake(Arc::new(move || {
        let l = hh.state().lyrics.clone();
        let mut s = s.lock().unwrap();
        if s.last() != Some(&l) {
            s.push(l);
        }
    }));
    Rig { h, lyrics, seen }
}

fn rig() -> Rig {
    rig_with(Arc::new(FakeLyrics::new()), true)
}

/// A track titled `title`, uri `fakea:track:<title>`.
fn track(title: &str) -> Track {
    Track {
        uri: uri(title),
        name: title.into(),
        artist: "artist".into(),
        album: "album".into(),
        duration_ms: 180_000,
        images: vec![],
        year: None,
    }
}

fn uri(title: &str) -> String {
    format!("fakea:track:{title}")
}

async fn wait_for(h: &PlayerHandle, what: &str, f: impl Fn(&PlayerState) -> bool) {
    let start = Instant::now();
    while !f(&h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}: {:?}",
            h.state().lyrics
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_calls(r: &Rig, n: usize) {
    let start = Instant::now();
    while r.lyrics.calls().len() < n {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {n} calls");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Play `title` and wait until it is current and playing.
async fn play(r: &Rig, title: &str) {
    r.h.dispatch(PlayerAction::PlayItem {
        context: vec![track(title)],
        index: 0,
    });
    let want = uri(title);
    wait_for(&r.h, "the track to play", |s| {
        s.current.as_ref().is_some_and(|c| c.track.uri == want)
            && s.play_state == PlayState::Playing
    })
    .await;
}

fn found(s: &PlayerState, title: &str) -> Option<Lyrics> {
    match &s.lyrics {
        LyricsState::Found { uri: u, lyrics } if *u == uri(title) => Some(Lyrics::clone(lyrics)),
        _ => None,
    }
}

fn titles(r: &Rig) -> Vec<String> {
    r.lyrics.calls().into_iter().map(|q| q.title).collect()
}

// Y1
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y1_playing_a_track_searches_then_finds_synced_lyrics() {
    let r = rig();
    assert_eq!(r.h.state().lyrics, LyricsState::Idle);
    assert!(r.h.state().lyrics_enabled);
    r.lyrics.with("A", demo_lyrics());
    let gate = r.lyrics.gate();
    play(&r, "A").await;
    let a = uri("A");
    wait_for(&r.h, "searching", |s| {
        s.lyrics == LyricsState::Searching { uri: a.clone() }
    })
    .await;
    gate.open();
    wait_for(&r.h, "found", |s| found(s, "A").is_some()).await;
    assert_eq!(found(&r.h.state(), "A"), Some(demo_lyrics()));
    assert_eq!(titles(&r), ["A"]);
}

// Y2
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y2_disabled_in_the_config_makes_no_call_until_switched_on() {
    let lyrics = Arc::new(FakeLyrics::new());
    lyrics.with("A", demo_lyrics());
    let r = rig_with(lyrics, false);
    assert_eq!(r.h.state().lyrics, LyricsState::Off);
    assert!(!r.h.state().lyrics_enabled);
    play(&r, "A").await;
    assert_eq!(r.h.state().lyrics, LyricsState::Off);
    assert!(r.lyrics.calls().is_empty());
    r.h.dispatch(PlayerAction::SetLyrics(true));
    wait_for(&r.h, "found", |s| found(s, "A").is_some()).await;
    assert!(r.h.state().lyrics_enabled);
    assert_eq!(titles(&r), ["A"]);
}

// Y3
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y3_switched_off_shows_off_and_the_next_track_makes_no_call() {
    let r = rig();
    r.lyrics.with("A", demo_lyrics());
    play(&r, "A").await;
    wait_for(&r.h, "found", |s| found(s, "A").is_some()).await;
    r.h.dispatch(PlayerAction::SetLyrics(false));
    wait_for(&r.h, "off", |s| {
        s.lyrics == LyricsState::Off && !s.lyrics_enabled
    })
    .await;
    play(&r, "B").await;
    assert_eq!(r.h.state().lyrics, LyricsState::Off);
    assert_eq!(titles(&r), ["A"]);
}

// Y4
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y4_a_track_played_again_is_answered_from_the_cache() {
    let r = rig();
    r.lyrics.with("A", demo_lyrics());
    play(&r, "A").await;
    wait_for(&r.h, "found A", |s| found(s, "A").is_some()).await;
    play(&r, "B").await;
    wait_for(&r.h, "found B", |s| found(s, "B").is_some()).await;
    play(&r, "A").await;
    wait_for(&r.h, "found A again", |s| found(s, "A").is_some()).await;
    assert_eq!(found(&r.h.state(), "A"), Some(demo_lyrics()));
    assert_eq!(titles(&r), ["A", "B"]);
}

/// A, B and C played while A's lookup is held: returns the rig after the gate opened and C was
/// found.
async fn a_b_c_behind_a_gate() -> Rig {
    let r = rig();
    r.lyrics.with("A", demo_lyrics());
    r.lyrics.with("C", Lyrics::Plain(vec!["c".into()]));
    let gate = r.lyrics.gate();
    play(&r, "A").await;
    wait_calls(&r, 1).await;
    play(&r, "B").await;
    play(&r, "C").await;
    let c = uri("C");
    wait_for(&r.h, "searching C", |s| {
        s.lyrics == LyricsState::Searching { uri: c.clone() }
    })
    .await;
    assert_eq!(titles(&r), ["A"]);
    gate.open();
    wait_for(&r.h, "found C", |s| found(s, "C").is_some()).await;
    r
}

// Y5
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y5_only_the_newest_waiting_lookup_runs() {
    let r = a_b_c_behind_a_gate().await;
    assert_eq!(titles(&r), ["A", "C"]);
    assert_eq!(
        found(&r.h.state(), "C"),
        Some(Lyrics::Plain(vec!["c".into()]))
    );
}

// Y6
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y6_a_late_answer_never_replaces_the_current_items_state() {
    let r = a_b_c_behind_a_gate().await;
    let a = uri("A");
    let seen = r.seen.lock().unwrap().clone();
    assert!(
        !seen
            .iter()
            .any(|l| matches!(l, LyricsState::Found { uri, .. } if *uri == a)),
        "{seen:?}"
    );
    // A's answer was cached: playing A again asks nothing.
    play(&r, "A").await;
    wait_for(&r.h, "found A", |s| found(s, "A").is_some()).await;
    assert_eq!(titles(&r), ["A", "C"]);
}

// Y7
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y7_an_unsorted_oversize_answer_is_published_sanitised() {
    let r = rig();
    let mut lines: Vec<LyricLine> = (0..1500u64)
        .rev()
        .map(|n| LyricLine {
            at_ms: n * 10,
            text: format!("line\t{n}"),
        })
        .collect();
    lines[0].text = "x".repeat(1000);
    let raw = Lyrics::Synced(lines);
    r.lyrics.with("A", raw.clone());
    play(&r, "A").await;
    wait_for(&r.h, "found", |s| found(s, "A").is_some()).await;
    let got = found(&r.h.state(), "A").unwrap();
    assert_eq!(got, raw.sanitized());
    let Lyrics::Synced(lines) = got else {
        panic!("synced expected")
    };
    assert_eq!(lines.len(), MAX_LYRIC_LINES);
    assert!(lines.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
    assert!(
        lines
            .iter()
            .all(|l| l.text.chars().count() <= MAX_LYRIC_CHARS)
    );
    assert!(lines.iter().all(|l| !l.text.chars().any(char::is_control)));
}

// Y8
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y8_a_failed_lookup_is_not_cached() {
    let r = rig();
    r.lyrics.fail("A", LyricsError::Unavailable);
    play(&r, "A").await;
    let a = uri("A");
    wait_for(&r.h, "failed", |s| {
        s.lyrics == LyricsState::Failed { uri: a.clone() }
    })
    .await;
    play(&r, "B").await;
    wait_for(&r.h, "found B", |s| found(s, "B").is_some()).await;
    play(&r, "A").await;
    wait_for(&r.h, "failed again", |s| {
        s.lyrics == LyricsState::Failed { uri: a.clone() }
    })
    .await;
    assert_eq!(titles(&r), ["A", "B", "A"]);
}
