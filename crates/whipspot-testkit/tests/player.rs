//! The player service (C1) on the testkit fakes. Every wait polls with a timeout; no fixed sleep
//! is an assertion.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use whipspot_core::diagnostics::Stage;
use whipspot_core::engine::{
    DeviceFormat, OutputDevice, OutputError, OutputFault, OutputMode, OutputRequest, SampleFormat,
    SourceError, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_core::events::UiEvent;
use whipspot_core::player::{
    self, EngineSetup, LibraryQuery, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle,
    PlayerNotice, PlayerState, RepeatMode, RequestId, SPECTRUM_BANDS, SpectrumFrame, WAVEFORM_LEN,
    texts,
};
use whipspot_core::provider::{ProviderContext, ProviderRegistry, Track};
use whipspot_testkit::{ClockedOutput, FakeOutput, FakeProvider};

const TIMEOUT: Duration = Duration::from_secs(5);
const RATE: u32 = 44_100;

struct Rig {
    h: PlayerHandle,
    ctx: ProviderContext,
    a: Arc<FakeProvider>,
    b: Arc<FakeProvider>,
    fake: FakeOutput,
}

fn fake_output() -> FakeOutput {
    FakeOutput::new(
        DeviceFormat {
            rate: RATE,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        },
        OutputMode::Shared,
    )
}

fn setup(fake: &FakeOutput) -> EngineSetup {
    EngineSetup::Output {
        output: Box::new(fake.clone()),
        request: OutputRequest::default(),
    }
}

fn rig() -> Rig {
    rig_on(fake_output())
}

fn rig_on(fake: FakeOutput) -> Rig {
    rig_choice(fake, OutputRequest::default())
}

/// A rig whose initial output choice is `request`.
fn rig_choice(fake: FakeOutput, request: OutputRequest) -> Rig {
    let engine = EngineSetup::Output {
        output: Box::new(fake.clone()),
        request,
    };
    let (ctx, wiring) = player::wiring(engine, 64);
    let a = Arc::new(FakeProvider::new("fakea", true).with_context(ctx.clone()));
    let b = Arc::new(FakeProvider::new("fakeb", true).with_context(ctx.clone()));
    let mut reg = ProviderRegistry::new();
    reg.register(a.clone()).unwrap();
    reg.register(b.clone()).unwrap();
    let h = Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    Rig { h, ctx, a, b, fake }
}

fn track(uri: &str) -> Track {
    Track {
        uri: uri.into(),
        name: uri.into(),
        artist: "artist".into(),
        album: "album".into(),
        duration_ms: 180_000,
        images: vec![],
        year: None,
        album_ref: None,
        artists: vec![],
    }
}

fn tracks(uris: &[&str]) -> Vec<Track> {
    uris.iter().map(|u| track(u)).collect()
}

async fn wait_for(h: &PlayerHandle, what: &str, f: impl Fn(&PlayerState) -> bool) {
    let start = Instant::now();
    while !f(&h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}: {:?}",
            h.state()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_calls(p: &FakeProvider, what: &str, f: impl Fn(&[String]) -> bool) {
    let start = Instant::now();
    while !f(&p.calls()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out waiting for {what}: {:?}",
            p.calls()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Wait for a notice matching `f`; returns it (other notices are dropped).
async fn wait_notice(h: &PlayerHandle, f: impl Fn(&PlayerNotice) -> bool) -> PlayerNotice {
    let start = Instant::now();
    loop {
        if let Some(n) = h.drain_notices().into_iter().find(|n| f(n)) {
            return n;
        }
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for a notice");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

fn current_uri(s: &PlayerState) -> Option<&str> {
    s.current.as_ref().map(|c| c.track.uri.as_str())
}

async fn play_item(r: &Rig, uris: &[&str], index: usize) {
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(uris),
        index,
    });
    let want = uris[index].to_owned();
    wait_for(&r.h, "the item to play", |s| {
        current_uri(s) == Some(want.as_str()) && s.play_state == PlayState::Playing
    })
    .await;
}

// (a)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transport_before_any_load_calls_no_provider() {
    let r = rig();
    for a in [
        PlayerAction::Play,
        PlayerAction::Pause,
        PlayerAction::Toggle,
        PlayerAction::Stop,
        PlayerAction::Seek { position_ms: 1000 },
        PlayerAction::Next,
        PlayerAction::Previous,
    ] {
        r.h.dispatch(a);
    }
    // A load queued after them runs after them on the transport task.
    play_item(&r, &["fakeb:track:1"], 0).await;
    assert!(r.a.calls().is_empty(), "{:?}", r.a.calls());
    assert_eq!(r.b.calls(), ["load fakeb:track:1 true 0"]);
}

// (b)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_scheme_posts_a_notice_and_calls_no_provider() {
    let r = rig();
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["nope:track:x"]),
        index: 0,
    });
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::NOT_A_TRACK));
    let s = r.h.state();
    assert!(s.current.is_none());
    assert!(s.queue.items.is_empty());
    assert!(r.a.calls().is_empty() && r.b.calls().is_empty());
}

// (c)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_on_another_provider_stops_the_previous_and_redirects_transport() {
    let r = rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    r.h.dispatch(PlayerAction::Pause);
    wait_for(&r.h, "paused", |s| s.play_state == PlayState::Paused).await;
    play_item(&r, &["fakeb:track:9"], 0).await;
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0", "pause", "stop"]);
    assert_eq!(r.h.state().current.as_ref().unwrap().provider, "fakeb");
    r.h.dispatch(PlayerAction::Pause);
    r.h.dispatch(PlayerAction::Seek { position_ms: 42 });
    wait_calls(&r.b, "pause and seek on B", |c| c.len() == 3).await;
    assert_eq!(
        r.b.calls(),
        ["load fakeb:track:9 true 0", "pause", "seek 42"]
    );
    assert_eq!(r.a.calls().len(), 3);
}

// (d)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_load_on_another_provider_leaves_the_previous_current_and_the_queue() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    let before = r.h.state();
    r.b.fail_load("fakeb:track:9", true);
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakeb:track:9"]),
        index: 0,
    });
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::LOAD_FAILED));
    wait_for(&r.h, "play state restored", |s| {
        s.play_state == PlayState::Playing
    })
    .await;
    let s = r.h.state();
    assert_eq!(s.current, before.current);
    assert_eq!(s.queue, before.queue);
    assert_eq!(r.a.calls(), ["load fakea:track:1 true 0"]);
    // Transport still goes to A.
    r.h.dispatch(PlayerAction::Pause);
    wait_calls(&r.a, "pause on A", |c| {
        c.last().is_some_and(|l| l == "pause")
    })
    .await;
    assert_eq!(r.b.calls(), ["load fakeb:track:9 true 0"]);
}

// (e)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_quick_play_items_run_in_order_and_the_second_wins() {
    let r = rig();
    r.a.set_load_delay(Some(Duration::from_millis(150)));
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:1", "fakea:track:x"]),
        index: 0,
    });
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:2"]),
        index: 0,
    });
    // The second load started: the first answered and must not have committed.
    wait_calls(&r.a, "second load", |c| c.len() == 2).await;
    assert!(r.h.state().current.is_none());
    wait_for(&r.h, "second item", |s| {
        current_uri(s) == Some("fakea:track:2") && s.play_state == PlayState::Playing
    })
    .await;
    assert_eq!(
        r.a.calls(),
        ["load fakea:track:1 true 0", "load fakea:track:2 true 0"]
    );
    let s = r.h.state();
    assert_eq!(*s.queue.items, tracks(&["fakea:track:2"]));
    assert_eq!(s.queue.current, Some(0));
}

// (e) continued: the superseded first load succeeds, the second fails. The state must describe
// what really plays: the first track, committed in place of the old state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn when_the_newest_load_fails_the_superseded_load_that_plays_is_committed() {
    let r = rig();
    r.a.set_load_delay(Some(Duration::from_millis(50)));
    r.a.fail_load("fakea:track:2", true);
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:1"]),
        index: 0,
    });
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:2"]),
        index: 0,
    });
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::LOAD_FAILED));
    let s = r.h.state();
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
    assert_eq!(*s.queue.items, tracks(&["fakea:track:1"]));
    assert_eq!(s.queue.current, Some(0));
    assert_eq!(s.play_state, PlayState::Playing);
    // Events of the track that plays apply again.
    r.a.emit(UiEvent::Paused {
        uri: "fakea:track:1".into(),
        position_ms: 7,
    });
    wait_for(&r.h, "paused by the provider", |s| {
        s.play_state == PlayState::Paused && s.position_ms == 7
    })
    .await;
}

// (e) continued: a queue edit while the loads run keeps the queue; the superseded track that
// plays is marked current by uri.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_committed_superseded_load_keeps_a_queue_edited_meanwhile() {
    let r = rig();
    play_item(&r, &["fakea:track:0", "fakea:track:1"], 0).await;
    r.a.set_load_delay(Some(Duration::from_millis(50)));
    r.a.fail_load("fakea:track:2", true);
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:1"]),
        index: 0,
    });
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:2"]),
        index: 0,
    });
    r.h.dispatch(PlayerAction::Enqueue(tracks(&["fakea:track:3"])));
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::LOAD_FAILED));
    let s = r.h.state();
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
    assert_eq!(
        *s.queue.items,
        tracks(&["fakea:track:0", "fakea:track:1", "fakea:track:3"])
    );
    assert_eq!(s.queue.current, Some(1));
    assert_eq!(s.play_state, PlayState::Playing);
}

// D11: a queue holds one provider's tracks; enqueueing another provider's track replaces it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enqueueing_another_providers_track_replaces_the_queue_and_the_same_one_appends() {
    let r = rig();
    play_item(&r, &["fakea:track:0", "fakea:track:1"], 0).await;
    r.h.dispatch(PlayerAction::Enqueue(tracks(&["fakea:track:2"])));
    wait_for(&r.h, "an appended item", |s| s.queue.items.len() == 3).await;
    r.h.dispatch(PlayerAction::Enqueue(tracks(&["fakeb:track:7"])));
    wait_for(&r.h, "the queue replaced", |s| s.queue.items.len() == 1).await;
    let s = r.h.state();
    assert_eq!(*s.queue.items, tracks(&["fakeb:track:7"]));
    // The playing track keeps playing; the queued track follows it.
    assert_eq!(current_uri(&s), Some("fakea:track:0"));
    assert_eq!(s.play_state, PlayState::Playing);
    assert_eq!(s.queue.current, None);
    assert_eq!(s.queue.next, Some(0));
}

// (f)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_of_track_advances_and_repeat_modes_behave() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    let eot = |uri: &str| UiEvent::EndOfTrack { uri: uri.into() };
    // Off: advance, then stop at the end with the current item kept.
    r.a.emit(eot("fakea:track:1"));
    wait_for(&r.h, "second item", |s| {
        current_uri(s) == Some("fakea:track:2") && s.queue.current == Some(1)
    })
    .await;
    r.a.emit(eot("fakea:track:2"));
    wait_for(&r.h, "stopped at the end", |s| {
        s.play_state == PlayState::Stopped
    })
    .await;
    assert_eq!(current_uri(&r.h.state()), Some("fakea:track:2"));
    assert_eq!(r.a.calls().last().unwrap(), "stop");
    // All: wrap to the first.
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::All));
    wait_for(&r.h, "repeat all", |s| s.repeat == RepeatMode::All).await;
    r.a.emit(eot("fakea:track:2"));
    wait_for(&r.h, "wrapped", |s| {
        current_uri(s) == Some("fakea:track:1") && s.play_state == PlayState::Playing
    })
    .await;
    // One: replay the current item.
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::One));
    wait_for(&r.h, "repeat one", |s| s.repeat == RepeatMode::One).await;
    let loads = |c: &[String]| {
        c.iter()
            .filter(|l| *l == "load fakea:track:1 true 0")
            .count()
    };
    assert_eq!(loads(&r.a.calls()), 2);
    r.a.emit(eot("fakea:track:1"));
    wait_calls(&r.a, "replay", |c| loads(c) == 3).await;
    wait_for(&r.h, "replayed", |s| {
        current_uri(s) == Some("fakea:track:1")
            && s.queue.current == Some(0)
            && s.play_state == PlayState::Playing
    })
    .await;
}

// (g)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_end_of_track_for_another_uri_changes_nothing() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    let calls = r.a.calls();
    r.a.emit(UiEvent::EndOfTrack {
        uri: "fakea:track:2".into(),
    });
    // Same channel, so applied after the stale event.
    r.a.emit(UiEvent::Position {
        uri: "fakea:track:1".into(),
        position_ms: 1234,
    });
    wait_for(&r.h, "position", |s| s.position_ms == 1234).await;
    let s = r.h.state();
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
    assert_eq!(s.queue.current, Some(0));
    assert_eq!(s.play_state, PlayState::Playing);
    assert_eq!(r.a.calls(), calls);
}

// (h)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wake_runs_after_a_dispatch_and_may_read_state() {
    let r = rig();
    let (tx, rx) = mpsc::channel();
    let h2 = r.h.clone();
    let tx = std::sync::Mutex::new(tx);
    r.h.set_wake(Arc::new(move || {
        let s = h2.state();
        let _ = tx.lock().unwrap().send((s.revision, s.volume));
    }));
    let rev0 = r.h.state().revision;
    r.h.dispatch(PlayerAction::SetVolume(30));
    let (rev, vol) = tokio::task::spawn_blocking(move || rx.recv_timeout(TIMEOUT).unwrap())
        .await
        .unwrap();
    assert!(rev > rev0);
    assert_eq!(vol, 30);
}

// (i)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn volume_and_eq_reach_the_engine_while_a_slow_load_is_pending() {
    let r = rig();
    r.a.set_load_delay(Some(Duration::from_secs(2)));
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:1"]),
        index: 0,
    });
    wait_for(&r.h, "loading", |s| s.play_state == PlayState::Loading).await;
    r.h.dispatch(PlayerAction::SetVolume(50));
    r.h.dispatch(PlayerAction::SetEqBand {
        index: 0,
        gain_db: 6.0,
    });
    // Invalid values change nothing.
    r.h.dispatch(PlayerAction::SetVolume(101));
    r.h.dispatch(PlayerAction::SetEqBand {
        index: 1,
        gain_db: f32::NAN,
    });
    r.h.dispatch(PlayerAction::SetEqBand {
        index: 10,
        gain_db: 1.0,
    });
    wait_for(&r.h, "controls in state", |s| {
        s.volume == 50 && s.eq.bands_db[0] == 6.0
    })
    .await;
    let s = r.h.state();
    assert_eq!(
        s.play_state,
        PlayState::Loading,
        "the load is still pending"
    );
    assert_eq!(s.eq.bands_db[1], 0.0);
    // The engine changed before the state did; it applies the values at its next callback.
    r.fake.render_f32(441);
    let d = r.h.diagnostics();
    assert_eq!(d.dsp.volume, Some(Stage::Active), "{d:?}");
    assert_eq!(d.dsp.eq, Some(Stage::Active), "{d:?}");
    assert_eq!(d.dsp.balance, Some(Stage::Bypassed));
}

// (j)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spectrum_is_silent_without_tap_data_and_shows_a_tone_through_the_engine() {
    let r = rig();
    let mut f = SpectrumFrame::default();
    r.h.spectrum(&mut f);
    assert_eq!(f.bands.len(), SPECTRUM_BANDS);
    assert_eq!(f.waveform.len(), WAVEFORM_LEN);
    assert!(!f.active);
    assert!(f.waveform.iter().chain(&f.bands).all(|&v| v == 0.0));
    play_item(&r, &["fakea:track:1"], 0).await;
    r.h.spectrum(&mut f);
    assert!(!f.active, "playing but no tap data");
    // 100 ms of a 1 kHz tone through the source, rendered by the fake output.
    let frames = (RATE / 10) as usize;
    let tone: Vec<f64> = (0..frames)
        .flat_map(|i| {
            let v = 0.5 * (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(RATE)).sin();
            [v, v]
        })
        .collect();
    let format = StreamFormat {
        rate: RATE,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(16),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    };
    r.ctx.source.start_track(format).unwrap();
    r.ctx.source.write(&tone).unwrap();
    r.fake.render_f32(frames);
    r.h.spectrum(&mut f);
    assert!(f.active);
    assert!(f.waveform.iter().any(|&v| v != 0.0));
    assert!(f.waveform.iter().all(|&v| (-1.0..=1.0).contains(&v)));
    assert_eq!(f.bands.len(), SPECTRUM_BANDS);
    assert!(f.bands.iter().all(|&v| (0.0..=1.0).contains(&v)));
    let loudest = f
        .bands
        .iter()
        .enumerate()
        .fold((0, f32::MIN), |b, (i, &v)| if v > b.1 { (i, v) } else { b })
        .0;
    assert_eq!(loudest, 17, "{:?}", f.bands);
}

// (c)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preload_asks_the_active_provider_for_the_next_track() {
    let r = rig();
    let uris = ["fakea:track:0", "fakea:track:1", "fakea:track:2"];
    play_item(&r, &uris, 0).await;
    r.a.emit(UiEvent::PreloadNext {
        uri: uris[0].into(),
    });
    wait_calls(&r.a, "preload of item 1", |c| {
        c.contains(&"preload:fakea:track:1".to_owned())
    })
    .await;
    // Repeat One preloads the same track.
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::One));
    wait_for(&r.h, "repeat one", |s| s.repeat == RepeatMode::One).await;
    r.a.emit(UiEvent::PreloadNext {
        uri: uris[0].into(),
    });
    wait_calls(&r.a, "preload of item 0", |c| {
        c.contains(&"preload:fakea:track:0".to_owned())
    })
    .await;
    // Repeat All wraps from the last item.
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::All));
    wait_for(&r.h, "repeat all", |s| s.repeat == RepeatMode::All).await;
    play_item(&r, &uris, 2).await;
    r.a.emit(UiEvent::PreloadNext {
        uri: uris[2].into(),
    });
    wait_calls(&r.a, "wrapped preload", |c| {
        c.iter().filter(|l| *l == "preload:fakea:track:0").count() == 2
    })
    .await;
}

// (c), the negative cases
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preload_is_not_sent_without_a_next_track_or_for_another_track() {
    let r = rig();
    let uris = ["fakea:track:0", "fakea:track:1"];
    play_item(&r, &uris, 1).await;
    // The last item with repeat Off, then an event for a track that is not current.
    r.a.emit(UiEvent::PreloadNext {
        uri: uris[1].into(),
    });
    r.a.emit(UiEvent::PreloadNext {
        uri: uris[0].into(),
    });
    // Same channel, so applied after the two events; a pause after it runs after any preload
    // on the transport task.
    r.a.emit(UiEvent::Position {
        uri: uris[1].into(),
        position_ms: 777,
    });
    wait_for(&r.h, "position", |s| s.position_ms == 777).await;
    r.h.dispatch(PlayerAction::Pause);
    wait_calls(&r.a, "pause", |c| c.contains(&"pause".to_owned())).await;
    assert!(
        r.a.calls().iter().all(|c| !c.starts_with("preload:")),
        "{:?}",
        r.a.calls()
    );
}

// (d)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_output_open_is_reported() {
    let fake = fake_output();
    fake.fail_open(true);
    let r = rig_on(fake);
    assert_eq!(r.h.state().output_summary, "No audio output");
    assert!(
        r.h.drain_notices()
            .contains(&PlayerNotice::Error(texts::OUTPUT_OPEN_FAILED))
    );
}

// (e)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_output_posts_one_notice_at_the_next_dispatch() {
    let r = rig();
    assert_eq!(
        r.h.state().output_summary,
        "Shared mode, fake, 44.1 kHz, 32-bit float"
    );
    r.fake.fault(OutputFault::DeviceLost);
    r.h.dispatch(PlayerAction::SetVolume(70));
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::OUTPUT_LOST));
    assert_eq!(r.h.state().output_summary, "Audio output stopped");
    // Reported once.
    r.h.dispatch(PlayerAction::SetVolume(71));
    wait_for(&r.h, "volume", |s| s.volume == 71).await;
    assert!(r.h.drain_notices().is_empty());
}

// (k)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_library_request_answers_with_its_request_id() {
    let r = rig();
    r.h.dispatch(PlayerAction::Library {
        request: RequestId(7),
        provider: "fakea".into(),
        query: LibraryQuery::Playlists { offset: 0 },
    });
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::LibraryPage { .. })).await;
    match n {
        PlayerNotice::LibraryPage { request, result } => {
            assert_eq!(request, RequestId(7));
            // The fake is logged out.
            assert!(result.is_err());
        }
        other => panic!("{other:?}"),
    }
}

// The explore queries are dispatched like the others and answer with their request id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explore_requests_answer_with_their_request_id() {
    let r = rig();
    let queries = [
        LibraryQuery::Artist {
            uri: "fakea:artist:1".into(),
        },
        LibraryQuery::Album {
            uri: "fakea:album:1".into(),
        },
        LibraryQuery::ArtistReleases {
            uri: "fakea:artist:1".into(),
            group: whipspot_core::provider::ReleaseGroup::All,
            offset: 0,
        },
    ];
    for (i, query) in queries.into_iter().enumerate() {
        let id = RequestId(20 + i as u64);
        r.h.dispatch(PlayerAction::Library {
            request: id,
            provider: "fakea".into(),
            query,
        });
        let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::LibraryPage { .. })).await;
        match n {
            PlayerNotice::LibraryPage { request, result } => {
                assert_eq!(request, id);
                // The fake is logged out.
                assert!(result.is_err());
            }
            other => panic!("{other:?}"),
        }
    }
}

// F6: the track that plays ends (or turns out unavailable) while a load is pending, then that
// load fails. Nothing plays any more, so the state must say `Stopped`, not `Playing`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_of_track_during_a_load_that_fails_leaves_stopped() {
    for unavailable in [false, true] {
        let r = rig();
        play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
        r.a.set_load_delay(Some(Duration::from_millis(150)));
        r.a.fail_load("fakea:track:2", true);
        r.h.dispatch(PlayerAction::Next);
        wait_for(&r.h, "loading", |s| s.play_state == PlayState::Loading).await;
        let uri = "fakea:track:1".to_owned();
        r.a.emit(if unavailable {
            UiEvent::Unavailable { uri }
        } else {
            UiEvent::EndOfTrack { uri }
        });
        let mut notices = Vec::new();
        let start = Instant::now();
        while !notices.contains(&PlayerNotice::Error(texts::LOAD_FAILED)) {
            assert!(start.elapsed() < TIMEOUT, "no LOAD_FAILED: {notices:?}");
            notices.extend(r.h.drain_notices());
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            notices.contains(&PlayerNotice::Error(texts::TRACK_UNAVAILABLE)),
            unavailable,
            "{notices:?}"
        );
        let s = r.h.state();
        assert_eq!(
            s.play_state,
            PlayState::Stopped,
            "unavailable: {unavailable}"
        );
        assert_eq!(current_uri(&s), Some("fakea:track:1"));
        assert_eq!(s.queue.current, Some(0));
        // The load in flight replaced the ending track: no advance, no stop call.
        assert_eq!(
            r.a.calls(),
            ["load fakea:track:1 true 0", "load fakea:track:2 true 0"]
        );
    }
}

// F6 class: a play state and position reported while a load is pending are what the state shows
// when that load fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_reported_during_a_load_that_fails_is_restored_with_its_position() {
    let r = rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    r.a.set_load_delay(Some(Duration::from_millis(150)));
    r.a.fail_load("fakea:track:2", true);
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:2"]),
        index: 0,
    });
    wait_for(&r.h, "loading", |s| s.play_state == PlayState::Loading).await;
    r.a.emit(UiEvent::Paused {
        uri: "fakea:track:1".into(),
        position_ms: 4321,
    });
    let n = wait_notice(&r.h, |n| matches!(n, PlayerNotice::Error(_))).await;
    assert_eq!(n, PlayerNotice::Error(texts::LOAD_FAILED));
    let s = r.h.state();
    assert_eq!(s.play_state, PlayState::Paused);
    assert_eq!(s.position_ms, 4321);
    assert_eq!(s.position_now(), 4321);
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_format_comes_from_the_committing_provider() {
    let (ctx, wiring) = player::wiring(setup(&fake_output()), 64);
    let a = Arc::new(
        FakeProvider::new("fakea", true)
            .with_context(ctx.clone())
            .with_source_format("Test 1 kbps"),
    );
    let b = Arc::new(FakeProvider::new("fakeb", true).with_context(ctx));
    let mut reg = ProviderRegistry::new();
    reg.register(a).unwrap();
    reg.register(b).unwrap();
    let h = Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    let play = |uri: &str| {
        h.dispatch(PlayerAction::PlayItem {
            context: tracks(&[uri]),
            index: 0,
        })
    };
    let want = |uri: &'static str| {
        let h = &h;
        async move {
            wait_for(h, "the item to play", |s| {
                current_uri(s) == Some(uri) && s.play_state == PlayState::Playing
            })
            .await
        }
    };
    play("fakea:1");
    want("fakea:1").await;
    assert_eq!(
        h.state().current.as_ref().unwrap().source_format.as_deref(),
        Some("Test 1 kbps")
    );
    play("fakeb:1");
    want("fakeb:1").await;
    assert_eq!(h.state().current.as_ref().unwrap().source_format, None);
}

// ------------------------------------------------------------ output control (M4 step 3)

/// An I16 44.1 kHz fake that can open both modes and reports the mode it was asked for.
fn mode_fake() -> FakeOutput {
    let fake = FakeOutput::new(
        DeviceFormat {
            rate: RATE,
            channels: 2,
            sample: SampleFormat::I16,
            valid_bits: 16,
        },
        OutputMode::Shared,
    );
    fake.report_request_mode(true);
    fake.set_modes(vec![OutputMode::Shared, OutputMode::Exclusive]);
    fake
}

fn choice(device: Option<&str>, mode: OutputMode) -> OutputRequest {
    OutputRequest {
        device: device.map(str::to_owned),
        mode,
        ..OutputRequest::default()
    }
}

fn exclusive_rig() -> Rig {
    rig_choice(mode_fake(), choice(None, OutputMode::Exclusive))
}

fn request(device: Option<&str>, mode: OutputMode, rate: Option<u32>) -> OutputRequest {
    OutputRequest {
        device: device.map(str::to_owned),
        mode,
        rate,
        formats: Vec::new(),
    }
}

fn pcm(rate: u32) -> StreamFormat {
    StreamFormat {
        rate,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(16),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// Run `f` on the source from a thread of its own (it may block on an open request) and wait
/// for its result off the runtime.
async fn on_source<T: Send + 'static>(
    source: &SourceWriter,
    f: impl FnOnce(&SourceWriter) -> T + Send + 'static,
) -> T {
    let source = source.clone();
    tokio::task::spawn_blocking(move || f(&source))
        .await
        .unwrap()
}

/// Drain notices until a volume change (dispatched after everything before it) is in the
/// state; returns every notice seen.
async fn notices_settled(h: &PlayerHandle, volume: u8) -> Vec<PlayerNotice> {
    h.dispatch(PlayerAction::SetVolume(volume));
    wait_for(h, "volume", |s| s.volume == volume).await;
    h.drain_notices()
}

const IDLE: &str = "Exclusive mode, default device, idle";

// (a)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exclusive_choice_starts_idle_and_opens_when_audio_arrives() {
    let r = exclusive_rig();
    let s = r.h.state();
    assert_eq!(s.output_summary, IDLE);
    assert!(s.output.idle);
    assert_eq!(s.output.mode, OutputMode::Exclusive);
    assert_eq!(s.output.device, None);
    assert_eq!(s.output.modes, [OutputMode::Shared, OutputMode::Exclusive]);
    assert!(r.fake.requests().is_empty());
    assert!(!r.fake.is_open());
    play_item(&r, &["fakea:track:1"], 0).await;
    let result = on_source(&r.ctx.source, |s| s.start_track(pcm(RATE))).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        r.fake.requests(),
        [request(None, OutputMode::Exclusive, Some(44_100))]
    );
    assert!(r.fake.is_open());
    let s = r.h.state();
    assert!(
        s.output_summary
            .starts_with("Exclusive mode, fake, 44.1 kHz"),
        "{}",
        s.output_summary
    );
    assert!(!s.output.idle);
    assert_eq!(s.output.fallback, None);
}

// (b)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_while_stopped_with_nothing_pending_is_declined() {
    let r = exclusive_rig();
    let start = Instant::now();
    let result = on_source(&r.ctx.source, |s| s.start_track(pcm(RATE))).await;
    assert_eq!(result, Err(SourceError::NoOutput));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert!(r.fake.requests().is_empty());
    assert_eq!(r.h.state().output_summary, IDLE);
}

// (c)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_exclusive_device_falls_back_to_shared_with_one_notice() {
    let r = exclusive_rig();
    r.fake
        .fail_mode(OutputMode::Exclusive, Some(OutputError::Busy));
    play_item(&r, &["fakea:track:1"], 0).await;
    let result = on_source(&r.ctx.source, |s| s.start_track(pcm(RATE))).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        r.fake.requests(),
        [
            request(None, OutputMode::Exclusive, Some(44_100)),
            request(None, OutputMode::Shared, None),
        ]
    );
    let s = r.h.state();
    assert!(
        s.output_summary.starts_with("Shared mode, fake, 44.1 kHz")
            && s.output_summary
                .ends_with("(exclusive unavailable: device in use)"),
        "{}",
        s.output_summary
    );
    assert_eq!(s.output.fallback, Some("device in use"));
    // The state shows the user's choice, not the fallback.
    assert_eq!(s.output.mode, OutputMode::Exclusive);
    // More audio on the fallback asks for nothing and posts no further notice.
    let more = on_source(&r.ctx.source, |s| s.write(&[0.0; 882])).await;
    assert_eq!(more, Ok(()));
    let notices = notices_settled(&r.h, 90).await;
    let fallbacks = notices
        .iter()
        .filter(|n| **n == PlayerNotice::Error(texts::OUTPUT_EXCLUSIVE_FALLBACK))
        .count();
    assert_eq!(fallbacks, 1, "{notices:?}");
    assert_eq!(r.fake.requests().len(), 2);
}

// (d)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_releases_an_exclusive_output_and_the_next_audio_reopens() {
    let r = exclusive_rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    let result = on_source(&r.ctx.source, |s| s.start_track(pcm(RATE))).await;
    assert_eq!(result, Ok(()));
    assert!(r.fake.is_open());
    r.h.dispatch(PlayerAction::Stop);
    wait_for(&r.h, "stopped and idle", |s| {
        s.play_state == PlayState::Stopped && s.output.idle
    })
    .await;
    assert!(!r.fake.is_open());
    assert_eq!(r.h.state().output_summary, IDLE);
    // The next load plus audio reopens.
    play_item(&r, &["fakea:track:2"], 0).await;
    let result = on_source(&r.ctx.source, |s| {
        s.start_track(pcm(RATE))?;
        s.write(&[0.0; 882])
    })
    .await;
    assert_eq!(result, Ok(()));
    assert!(r.fake.is_open());
    assert_eq!(
        r.fake.requests(),
        [
            request(None, OutputMode::Exclusive, Some(44_100)),
            request(None, OutputMode::Exclusive, Some(44_100)),
        ]
    );
    // Pause keeps the device.
    r.h.dispatch(PlayerAction::Pause);
    wait_for(&r.h, "paused", |s| s.play_state == PlayState::Paused).await;
    assert!(r.fake.is_open());
    assert!(!r.h.state().output.idle);
}

// (e)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_output_to_shared_while_playing_reopens_shared() {
    let r = exclusive_rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    let result = on_source(&r.ctx.source, |s| s.start_track(pcm(RATE))).await;
    assert_eq!(result, Ok(()));
    r.h.dispatch(PlayerAction::SetOutput {
        device: None,
        mode: OutputMode::Shared,
    });
    wait_for(&r.h, "shared choice", |s| {
        s.output.mode == OutputMode::Shared && s.output_summary.starts_with("Shared mode")
    })
    .await;
    assert_eq!(
        r.fake.requests().last(),
        Some(&request(None, OutputMode::Shared, None))
    );
    assert!(r.fake.is_open());
    let s = r.h.state();
    assert_eq!(s.output.fallback, None);
    assert!(!s.output.idle);
}

// (f)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_shared_device_falls_back_to_the_default_device_with_one_notice() {
    let fake = mode_fake();
    fake.fail_device("gone");
    let r = rig_choice(fake, choice(Some("gone"), OutputMode::Shared));
    assert_eq!(
        r.fake.requests(),
        [
            request(Some("gone"), OutputMode::Shared, None),
            request(None, OutputMode::Shared, None),
        ]
    );
    let s = r.h.state();
    assert!(
        s.output_summary.ends_with("(chosen device not found)"),
        "{}",
        s.output_summary
    );
    assert_eq!(s.output.fallback, Some("chosen device not found"));
    assert_eq!(s.output.device.as_deref(), Some("gone"));
    let notices = notices_settled(&r.h, 80).await;
    let missing = notices
        .iter()
        .filter(|n| **n == PlayerNotice::Error(texts::OUTPUT_DEVICE_MISSING))
        .count();
    assert_eq!(missing, 1, "{notices:?}");
}

// (g)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_output_devices_lists_the_adapters_devices() {
    let r = exclusive_rig();
    let devices = vec![
        OutputDevice {
            id: "dac".into(),
            name: "USB DAC".into(),
            is_default: false,
        },
        OutputDevice {
            id: "speakers".into(),
            name: "Speakers".into(),
            is_default: true,
        },
    ];
    r.fake.set_devices(devices.clone());
    assert!(r.h.state().output.devices.is_empty());
    r.h.dispatch(PlayerAction::RefreshOutputDevices);
    wait_for(&r.h, "devices", |s| s.output.devices == devices).await;
}

// (N1) MD56: `next` is the index `advance` chooses for the repeat mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_up_follows_the_repeat_mode() {
    let r = rig();
    let uris = ["fakea:track:1", "fakea:track:2", "fakea:track:3"];
    play_item(&r, &uris, 0).await;
    wait_for(&r.h, "next after item 0", |s| s.queue.next == Some(1)).await;
    play_item(&r, &uris, 2).await;
    wait_for(&r.h, "no next at the end under Off", |s| {
        s.queue.next.is_none()
    })
    .await;
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::All));
    wait_for(&r.h, "wraps to the first under All", |s| {
        s.repeat == RepeatMode::All && s.queue.next == Some(0)
    })
    .await;
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::One));
    wait_for(&r.h, "the current item under One", |s| {
        s.repeat == RepeatMode::One && s.queue.next == Some(2)
    })
    .await;
}

// (N2) MD56: under shuffle, `next` is the track that plays after `Next`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_up_under_shuffle_is_the_track_that_plays_next() {
    let r = rig();
    let uris = [
        "fakea:track:1",
        "fakea:track:2",
        "fakea:track:3",
        "fakea:track:4",
        "fakea:track:5",
    ];
    play_item(&r, &uris, 0).await;
    r.h.dispatch(PlayerAction::SetShuffle(true));
    wait_for(&r.h, "shuffle on", |s| s.shuffle).await;
    let planned = r.h.state().queue.next.expect("a next track under shuffle");
    r.h.dispatch(PlayerAction::Next);
    wait_for(&r.h, "the planned track plays", |s| {
        s.queue.current == Some(planned)
            && current_uri(s) == Some(uris[planned])
            && s.play_state == PlayState::Playing
    })
    .await;
}

// (N3) MD56: removing the next item moves `next` to the one that follows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_next_item_moves_next_up_to_the_following_one() {
    let r = rig();
    let uris = [
        "fakea:track:1",
        "fakea:track:2",
        "fakea:track:3",
        "fakea:track:4",
    ];
    play_item(&r, &uris, 0).await;
    wait_for(&r.h, "next is item 2", |s| s.queue.next == Some(1)).await;
    r.h.dispatch(PlayerAction::RemoveFromQueue { index: 1 });
    wait_for(&r.h, "item 2 removed", |s| s.queue.items.len() == 3).await;
    let s = r.h.state();
    assert_eq!(s.queue.next, Some(1));
    assert_eq!(s.queue.items[1].uri, "fakea:track:3");
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
}

// (N4) MD56: under One, once the playing item left the list nothing in it plays next (advance
// replays the now-playing track).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_up_under_repeat_one_is_none_after_removing_the_playing_item() {
    let r = rig();
    let uris = ["fakea:track:1", "fakea:track:2", "fakea:track:3"];
    play_item(&r, &uris, 0).await;
    r.h.dispatch(PlayerAction::SetRepeat(RepeatMode::One));
    wait_for(&r.h, "the current item under One", |s| {
        s.repeat == RepeatMode::One && s.queue.next == Some(0)
    })
    .await;
    r.h.dispatch(PlayerAction::RemoveFromQueue { index: 0 });
    wait_for(&r.h, "playing item removed", |s| {
        s.queue.items.len() == 2 && s.queue.current.is_none()
    })
    .await;
    let s = r.h.state();
    assert_eq!(s.queue.next, None);
    assert_eq!(current_uri(&s), Some("fakea:track:1"));
}

// ------------------------------------------------------------ render-side timing (M10 step 4, H25)

/// A player on [`ClockedOutput`] (real-time renderer, no device) with one fake provider.
struct ClockedRig {
    h: PlayerHandle,
    ctx: ProviderContext,
    a: Arc<FakeProvider>,
}

fn clocked_rig() -> ClockedRig {
    let engine = EngineSetup::Output {
        output: Box::new(ClockedOutput::new(RATE)),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(engine, 64);
    let a = Arc::new(FakeProvider::new("fakea", true).with_context(ctx.clone()));
    let mut reg = ProviderRegistry::new();
    reg.register(a.clone()).unwrap();
    let h = Player::start(
        reg,
        wiring,
        PlayerConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    ClockedRig { h, ctx, a }
}

/// Play `uris[0]` and write `ms` of its audio into the engine at once (the ring holds 250 ms,
/// so the write does not block): that much is queued, not heard yet, when this returns.
async fn queue_audio(r: &ClockedRig, uris: &[&str], ms: u32) {
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(uris),
        index: 0,
    });
    let want = uris[0].to_owned();
    wait_for(&r.h, "the item to play", |s| {
        current_uri(s) == Some(want.as_str()) && s.play_state == PlayState::Playing
    })
    .await;
    let frames = (RATE * ms / 1000) as usize;
    let result = on_source(&r.ctx.source, move |s| {
        s.start_track(pcm(RATE))?;
        s.write(&vec![0.1; 2 * frames])
    })
    .await;
    assert_eq!(result, Ok(()));
}

/// `frames` frames out of `fake` in 10 ms callbacks (the last one shorter).
fn render(fake: &FakeOutput, mut frames: usize) {
    let block = RATE as usize / 100;
    while frames > 0 {
        let n = frames.min(block);
        fake.render_f32(n);
        frames -= n;
    }
}

// D12: a provider reports the next track (its end of track) while 200 ms of the old one are
// still queued; now-playing switches when they have been heard, not at the report. The output's
// callbacks are this test's own (`FakeOutput`, nothing renders unless it does; F8, Q26), so the
// switch point is exact on any host: the old track shows until the last queued frame has been
// rendered, and that frame switches. The two pauses before an assertion give a switch that does
// not wait for the audio (the behaviour before H25), or one a frame early, the time to show; they
// cannot fail a right one, which cannot switch before the renderer has passed the queued audio.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gapless_switch_waits_for_the_queued_audio_of_the_old_track() {
    let r = rig();
    play_item(&r, &["fakea:track:1", "fakea:track:2"], 0).await;
    // 200 ms queued (the ring holds 250 ms, so the write does not wait for a callback).
    let queued = RATE as usize / 5;
    let result = on_source(&r.ctx.source, move |s| {
        s.start_track(pcm(RATE))?;
        s.write(&vec![0.1; 2 * queued])
    })
    .await;
    assert_eq!(result, Ok(()));
    r.a.emit(UiEvent::EndOfTrack {
        uri: "fakea:track:1".into(),
    });
    // The next load starts at once (the provider keeps decoding gaplessly); the old track still
    // shows, playing.
    wait_calls(&r.a, "the next load", |c| {
        c.iter().any(|l| l == "load fakea:track:2 true 0")
    })
    .await;
    let old = |s: &PlayerState| {
        current_uri(s) == Some("fakea:track:1") && s.play_state == PlayState::Playing
    };
    assert!(old(&r.h.state()), "{:?}", r.h.state());
    // 150 ms of it heard: still the old track.
    let heard = RATE as usize * 150 / 1000;
    render(&r.fake, heard);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(old(&r.h.state()), "{:?}", r.h.state());
    // All of it but the last frame: still the old track.
    render(&r.fake, queued - heard - 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(old(&r.h.state()), "{:?}", r.h.state());
    // The last frame: the next track shows, playing from 0 (never more than the time since that
    // frame was rendered).
    let last = Instant::now();
    render(&r.fake, 1);
    wait_for(&r.h, "the switch", |s| {
        current_uri(s) == Some("fakea:track:2")
    })
    .await;
    let s = r.h.state();
    assert_eq!(s.play_state, PlayState::Playing);
    let shown = u128::from(s.position_now());
    assert!(shown <= last.elapsed().as_millis(), "{shown} ms");
}

// D12: positions are what is heard (reported less queued), never past the report; a seek shows
// its target at once; pause holds the position.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positions_are_what_is_heard_while_audio_is_queued() {
    let r = clocked_rig();
    queue_audio(&r, &["fakea:track:1"], 200).await;
    r.a.emit(UiEvent::Position {
        uri: "fakea:track:1".into(),
        position_ms: 5_000,
    });
    wait_for(&r.h, "the position", |s| s.position_ms > 1_000).await;
    let shown = r.h.state().position_ms;
    assert!((4_700..=4_900).contains(&shown), "{shown}");
    // A later report: the shown position never goes backwards nor past the report.
    r.a.emit(UiEvent::Position {
        uri: "fakea:track:1".into(),
        position_ms: 5_010,
    });
    wait_for(&r.h, "the second position", |s| s.position_ms != shown).await;
    let s = r.h.state();
    assert!(
        s.position_ms >= shown && s.position_ms <= 5_010,
        "{}",
        s.position_ms
    );
    r.h.dispatch(PlayerAction::Seek {
        position_ms: 60_000,
    });
    wait_for(&r.h, "the seek target", |s| s.position_ms == 60_000).await;
    r.h.dispatch(PlayerAction::Pause);
    wait_for(&r.h, "paused", |s| s.play_state == PlayState::Paused).await;
    let held = r.h.state().position_now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.h.state().position_now(), held);
}

// D12: a manual action that arrives before the held switch cancels it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_action_cancels_a_held_switch() {
    let r = clocked_rig();
    queue_audio(&r, &["fakea:track:1", "fakea:track:2"], 200).await;
    r.a.emit(UiEvent::EndOfTrack {
        uri: "fakea:track:1".into(),
    });
    wait_calls(&r.a, "the next load", |c| {
        c.iter().any(|l| l == "load fakea:track:2 true 0")
    })
    .await;
    r.h.dispatch(PlayerAction::PlayItem {
        context: tracks(&["fakea:track:3"]),
        index: 0,
    });
    wait_for(&r.h, "the chosen item", |s| {
        current_uri(s) == Some("fakea:track:3") && s.play_state == PlayState::Playing
    })
    .await;
    // Past the time the held switch would have come.
    tokio::time::sleep(Duration::from_millis(350)).await;
    let s = r.h.state();
    assert_eq!(current_uri(&s), Some("fakea:track:3"));
    assert_eq!(*s.queue.items, tracks(&["fakea:track:3"]));
}

// D12: with no output open (an exclusive choice, released) positions pass through unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positions_pass_through_with_no_output_open() {
    let r = exclusive_rig();
    play_item(&r, &["fakea:track:1"], 0).await;
    assert!(!r.fake.is_open());
    r.a.emit(UiEvent::Position {
        uri: "fakea:track:1".into(),
        position_ms: 1_234,
    });
    wait_for(&r.h, "the position", |s| s.position_ms == 1_234).await;
}
