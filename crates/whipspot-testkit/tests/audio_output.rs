//! The engine's open-request protocol on a fake output (MD38, MD39, I12–I17): rate follow,
//! declined requests, release and reopen on demand, reopen keeping blocked writers alive,
//! no request without data, the player giving an exclusive device back after a long pause
//! (H28), and the actions that reach the heard track during a held gapless switch (H25). No
//! device is opened; every signal is built here.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputError, OutputMode, OutputRequest, SampleFormat,
    SourceError, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_core::events::UiEvent;
use whipspot_core::player::{
    self, EngineSetup, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle, PlayerState,
};
use whipspot_core::provider::{ProviderRegistry, Track};
use whipspot_testkit::{FakeOutput, FakeProvider};

const RATE: u32 = 44_100;
const HIGH: u32 = 96_000;
/// 250 ms at 44.1 kHz: the ring's soft depth.
const DEPTH: usize = 11_025;
/// How long a test waits for something that should happen at once.
const DEADLINE: Duration = Duration::from_secs(5);

fn device() -> DeviceFormat {
    DeviceFormat {
        rate: RATE,
        channels: 2,
        sample: SampleFormat::I16,
        valid_bits: 16,
    }
}

fn track(rate: u32) -> StreamFormat {
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

fn exclusive(rate: Option<u32>) -> OutputRequest {
    OutputRequest {
        mode: OutputMode::Exclusive,
        rate,
        ..OutputRequest::default()
    }
}

/// An engine on an exclusive I16 fake at 44.1 kHz that can also open at 96 kHz, opened at
/// 44.1 kHz.
fn rig(config: EngineConfig) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(device(), OutputMode::Exclusive);
    fake.set_rates(&[RATE, HIGH]);
    let parts = Engine::new(config, Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&exclusive(None)).unwrap();
    assert_eq!(engine.output().unwrap().format.rate, RATE);
    (engine, parts.source, fake)
}

/// Interleave 16-bit frames as samples.
fn samples(frames: &[[i32; 2]]) -> Vec<f64> {
    frames
        .iter()
        .flat_map(|f| [f64::from(f[0]) / 32_768.0, f64::from(f[1]) / 32_768.0])
        .collect()
}

/// `n` distinct, nonzero 16-bit frames starting at `base`.
fn ramp(base: i32, n: usize) -> Vec<[i32; 2]> {
    (0..n as i32)
        .map(|i| [base + i % 20_000, -(base + i % 20_000)])
        .collect()
}

fn frames16(out: &[i16]) -> Vec<[i32; 2]> {
    out.chunks_exact(2)
        .map(|f| [i32::from(f[0]), i32::from(f[1])])
        .collect()
}

/// A hook that counts its calls.
fn counting_hook(engine: &mut Engine) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    engine.set_request_hook(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    }));
    calls
}

/// Wait until the engine sees an open request; its wanted rate.
fn wait_request(engine: &Engine) -> u32 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(rate) = engine.open_request() {
            return rate;
        }
        assert!(Instant::now() < deadline, "no open request");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The R1/R2 producer: track A at 44.1 kHz (30 ms), its end, then track B at 96 kHz (30 ms).
fn two_rate_producer(
    src: &SourceWriter,
    a: Vec<[i32; 2]>,
    b: Vec<[i32; 2]>,
) -> JoinHandle<Result<(), SourceError>> {
    let src = src.clone();
    std::thread::spawn(move || {
        src.start_track(track(RATE))?;
        src.write(&samples(&a))?;
        src.end();
        src.start_track(track(HIGH))?;
        src.write(&samples(&b))?;
        src.end();
        Ok(())
    })
}

/// Render in small callbacks until the engine sees an open request; what came out before it.
fn render_until_request(engine: &Engine, fake: &FakeOutput) -> (Vec<[i32; 2]>, u32) {
    let deadline = Instant::now() + DEADLINE;
    let mut out = Vec::new();
    loop {
        if let Some(rate) = engine.open_request() {
            return (out, rate);
        }
        assert!(Instant::now() < deadline, "no open request");
        out.extend(frames16(&fake.render_i16(256)));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Every frame of `a` came out exactly once, in order, and nothing else but silence.
fn assert_played_once(out: &[[i32; 2]], a: &[[i32; 2]]) {
    let first = out
        .iter()
        .position(|f| *f != [0, 0])
        .expect("track A never played");
    assert_eq!(out[first..first + a.len()], a[..]);
    assert!(out[first + a.len()..].iter().all(|f| *f == [0, 0]));
}

#[test]
fn r1_rate_follow_reopens_at_the_track_rate() {
    let (mut engine, src, fake) = rig(EngineConfig::default());
    engine.set_follow_rate(true);
    let calls = counting_hook(&mut engine);
    let a = ramp(1_000, 1_323);
    let b = ramp(-25_000, 2_880);
    let producer = two_rate_producer(&src, a.clone(), b.clone());

    let (out, rate) = render_until_request(&engine, &fake);
    assert_eq!(rate, HIGH);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_played_once(&out, &a);
    let reopened = engine.reopen(&[exclusive(Some(HIGH))]);
    assert!(reopened.errors.is_empty());
    let (index, info) = reopened.opened.expect("reopened");
    assert_eq!(index, 0);
    assert_eq!(info.format.rate, HIGH);
    assert_eq!(engine.open_request(), None);
    assert_eq!(producer.join().unwrap(), Ok(()));

    assert_eq!(frames16(&fake.render_i16(b.len())), b);
    let status = engine.status();
    let t = status.track.expect("track B");
    assert_eq!(t.format.rate, HIGH);
    assert!(!t.resampled);
    assert_eq!(status.device.unwrap().rate, HIGH);
    assert_eq!(fake.requests().last().unwrap().rate, Some(HIGH));
}

#[test]
fn r2_declined_request_resamples() {
    let (mut engine, src, fake) = rig(EngineConfig::default());
    engine.set_follow_rate(true);
    let a = ramp(1_000, 1_323);
    let b = ramp(-25_000, 2_880);
    let producer = two_rate_producer(&src, a.clone(), b);

    let (out, rate) = render_until_request(&engine, &fake);
    assert_eq!(rate, HIGH);
    assert_played_once(&out, &a);
    engine.answer_request();
    assert_eq!(engine.open_request(), None);
    assert_eq!(producer.join().unwrap(), Ok(()));

    let out = frames16(&fake.render_i16(1_323));
    assert!(out.iter().any(|f| *f != [0, 0]), "track B never played");
    let t = engine.status().track.expect("track B");
    assert_eq!(t.format.rate, HIGH);
    assert!(t.resampled);
    assert_eq!(engine.output().unwrap().format.rate, RATE);
    // Nothing was reopened: only the rig's open reached the adapter.
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn r3_release_and_open_on_demand() {
    let (mut engine, src, fake) = rig(EngineConfig::default());
    engine.release();
    assert!(engine.is_released());
    assert!(!fake.is_open());
    assert!(engine.output().is_none());

    // Answered with an open: the producer goes on.
    let producer = {
        let src = src.clone();
        std::thread::spawn(move || src.start_track(track(RATE)))
    };
    assert_eq!(wait_request(&engine), RATE);
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        !producer.is_finished(),
        "start_track should wait for an answer"
    );
    let reopened = engine.reopen(&[exclusive(Some(RATE))]);
    assert_eq!(reopened.opened.map(|(i, _)| i), Some(0));
    assert!(!engine.is_released());
    assert_eq!(producer.join().unwrap(), Ok(()));
    let b = ramp(-25_000, 2_000);
    src.write(&samples(&b)).unwrap();
    assert_eq!(frames16(&fake.render_i16(2_000)), b);

    // Answered without an open: `NoOutput` at once.
    engine.release();
    let producer = {
        let src = src.clone();
        std::thread::spawn(move || src.start_track(track(RATE)))
    };
    wait_request(&engine);
    let t = Instant::now();
    engine.answer_request();
    assert_eq!(producer.join().unwrap(), Err(SourceError::NoOutput));
    // measured 1.1 to 1.3 ms (2026-11-14, 1.92 x86_64); a liveness limit, kept loose on
    // purpose (scheduler latency on a loaded host).
    assert!(t.elapsed() < Duration::from_millis(200));
    assert!(engine.is_released());

    // Not answered: `NoOutput` after the request timeout.
    let config = EngineConfig {
        request_timeout: Duration::from_millis(100),
        ..EngineConfig::default()
    };
    let (mut engine, src, _fake) = rig(config);
    engine.release();
    let t = Instant::now();
    assert_eq!(src.start_track(track(RATE)), Err(SourceError::NoOutput));
    let elapsed = t.elapsed();
    // Exact rule: at least the request timeout; measured 100.6 to 101.0 ms (2026-11-14,
    // 1.92 x86_64). The upper bound is a liveness limit, loose for loaded hosts.
    assert!(elapsed >= Duration::from_millis(100), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    assert_eq!(engine.open_request(), Some(RATE));
}

#[test]
fn r4_reopen_keeps_blocked_writers_alive() {
    let (mut engine, src, fake) = rig(EngineConfig::default());
    fake.fail_mode(OutputMode::Exclusive, Some(OutputError::Busy));
    src.start_track(track(RATE)).unwrap();
    let writer = {
        let src = src.clone();
        let data = samples(&ramp(1_000, 3 * DEPTH));
        std::thread::spawn(move || src.write(&data))
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(!writer.is_finished(), "writer should block on a full ring");

    let shared = OutputRequest::default();
    let reopened = engine.reopen(&[exclusive(None), shared.clone()]);
    assert_eq!(reopened.errors, vec![OutputError::Busy]);
    assert_eq!(reopened.opened.as_ref().map(|(i, _)| *i), Some(1));
    assert_eq!(writer.join().unwrap(), Ok(()));
    let requests = fake.requests();
    assert_eq!(requests[requests.len() - 2..], [exclusive(None), shared]);

    let c = ramp(-25_000, 2_000);
    src.write(&samples(&c)).unwrap();
    assert_eq!(frames16(&fake.render_i16(2_000)), c);
}

#[test]
fn r5_no_request_without_data() {
    let (mut engine, src, fake) = rig(EngineConfig::default());
    let calls = counting_hook(&mut engine);
    src.start_track(track(RATE)).unwrap();
    src.write(&samples(&ramp(1_000, 441))).unwrap();
    fake.render_i16(441);
    engine.release();
    src.finish();
    src.flush();
    src.end();
    src.pause();
    src.resume();
    assert_eq!(engine.open_request(), None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let writer = {
        let src = src.clone();
        std::thread::spawn(move || src.write(&samples(&ramp(1_000, 441))))
    };
    assert_eq!(wait_request(&engine), RATE);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    engine.answer_request();
    assert_eq!(writer.join().unwrap(), Err(SourceError::NoOutput));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(engine.open_request(), None);
}

// ---------------------------------------------------------------- pause release (H28, D14)

/// The pause timeout the tests use.
const PAUSE_RELEASE: Duration = Duration::from_millis(50);

struct PlayerRig {
    h: PlayerHandle,
    source: SourceWriter,
    provider: Arc<FakeProvider>,
    fake: FakeOutput,
}

/// A player on an exclusive-capable I16 fake, the exclusive mode chosen, and a pause release
/// of [`PAUSE_RELEASE`]; playing one track whose audio the test writes itself.
async fn pause_rig(mode: OutputMode) -> PlayerRig {
    pause_rig_with(mode, &["fakea:track:1"]).await
}

/// [`pause_rig`] with the queue `uris`, the first playing.
async fn pause_rig_with(mode: OutputMode, uris: &[&str]) -> PlayerRig {
    let fake = FakeOutput::new(device(), OutputMode::Shared);
    fake.report_request_mode(true);
    fake.set_modes(vec![OutputMode::Shared, OutputMode::Exclusive]);
    let setup = EngineSetup::Output {
        output: Box::new(fake.clone()),
        request: OutputRequest {
            mode,
            ..OutputRequest::default()
        },
    };
    let (ctx, wiring) = player::wiring(setup, 64);
    let provider = Arc::new(FakeProvider::new("fakea", true).with_context(ctx.clone()));
    let mut registry = ProviderRegistry::new();
    registry.register(provider.clone()).unwrap();
    let h = Player::start(
        registry,
        wiring,
        PlayerConfig::default().with_exclusive_pause_release(PAUSE_RELEASE),
        &tokio::runtime::Handle::current(),
    );
    let rig = PlayerRig {
        h,
        source: ctx.source,
        provider,
        fake,
    };
    rig.h.dispatch(PlayerAction::PlayItem {
        context: uris
            .iter()
            .map(|&uri| Track {
                uri: uri.into(),
                name: "one".into(),
                artist: "artist".into(),
                album: "album".into(),
                duration_ms: 180_000,
                images: vec![],
                year: None,
                album_ref: None,
                artists: vec![],
            })
            .collect(),
        index: 0,
    });
    wait_state(&rig.h, "playing", |s| s.play_state == PlayState::Playing).await;
    rig.write(true).await;
    rig
}

async fn wait_state(h: &PlayerHandle, what: &str, f: impl Fn(&PlayerState) -> bool) {
    let start = Instant::now();
    while !f(&h.state()) {
        assert!(
            start.elapsed() < DEADLINE,
            "timed out waiting for {what}: {:?}",
            h.state()
        );
        // A blocking nap: tokio's timer wheel rounds up to the Windows tick (about 15 ms), which
        // is as long as the pauses under test.
        std::thread::sleep(Duration::from_micros(200));
    }
}

impl PlayerRig {
    /// Write a little audio from a thread of its own (a released output makes the writer ask
    /// for an open and wait); `start` starts the track first.
    async fn write(&self, start: bool) {
        let source = self.source.clone();
        let result = tokio::task::spawn_blocking(move || {
            if start {
                source.start_track(track(RATE))?;
            }
            source.write(&[0.0; 882])
        })
        .await
        .unwrap();
        assert_eq!(result, Ok(()));
    }

    /// Pauses and waits for the state to show it; returns the time just before the pause was
    /// asked for. The player arms its release timer after that, so the time since is at least
    /// how long the player has been paused, however late this thread saw the state.
    async fn pause(&self) -> Instant {
        let asked = Instant::now();
        self.h.dispatch(PlayerAction::Pause);
        wait_state(&self.h, "paused", |s| s.play_state == PlayState::Paused).await;
        asked
    }

    async fn resume(&self) {
        self.h.dispatch(PlayerAction::Play);
        wait_state(&self.h, "playing", |s| s.play_state == PlayState::Playing).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_pause_releases_exclusive_and_resume_reopens() {
    let r = pause_rig(OutputMode::Exclusive).await;
    assert!(r.fake.is_open());
    let paused = r.pause().await;
    wait_state(&r.h, "the device released", |s| s.output.idle).await;
    let released = paused.elapsed();
    // measured 51.7 to 66.2 ms for the 50 ms release (2026-11-15, 1.92 x86_64); bound: the exact
    // rule, as the timer is armed after the pause was asked for and never fires early (so no
    // margin for how late this thread saw the state).
    assert!(released >= PAUSE_RELEASE, "{released:?}");
    assert!(!r.fake.is_open());
    assert_eq!(r.h.state().play_state, PlayState::Paused);
    // The provider is put back at the paused position: the queued audio went with the device.
    // The state flips to idle before the player's task makes the call, so wait for it
    // (a race seen in about 1 run in 12 when asserted at once).
    let start = Instant::now();
    while !r.provider.calls().iter().any(|c| c.starts_with("seek ")) {
        assert!(
            start.elapsed() < DEADLINE,
            "no seek: {:?}",
            r.provider.calls()
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    let reopens = r.fake.requests().len();
    // Resume: the writer's next audio asks for the device and gets it.
    r.resume().await;
    r.write(false).await;
    assert!(r.fake.is_open());
    assert_eq!(r.fake.requests().len(), reopens + 1);
    let s = r.h.state();
    assert_eq!(s.play_state, PlayState::Playing);
    assert!(!s.output.idle);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_pause_keeps_the_device() {
    let r = pause_rig(OutputMode::Exclusive).await;
    // A 20 ms pause against the 50 ms timeout. The time from just before the pause was asked for
    // to after the resume shows is at least how long the player was paused: when it is under
    // the timeout, the device must still be open (exact: the timer never fires early). A host
    // too loaded to resume in time leaves the attempt open, and it is tried again (the device
    // reopened) until the deadline.
    let start = Instant::now();
    loop {
        let paused = r.pause().await;
        std::thread::sleep(Duration::from_millis(20));
        r.resume().await;
        // measured 21.1 to 21.5 ms (2026-11-15, 1.92 x86_64); bound: the exact rule above.
        if paused.elapsed() < PAUSE_RELEASE {
            assert!(r.fake.is_open());
            assert!(!r.h.state().output.idle);
            return;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "the host never resumed a 20 ms pause within {PAUSE_RELEASE:?}"
        );
        r.write(false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_output_is_never_released_by_a_pause() {
    let r = pause_rig(OutputMode::Shared).await;
    let paused = r.pause().await;
    tokio::time::sleep(PAUSE_RELEASE * 3).await;
    assert!(paused.elapsed() > PAUSE_RELEASE * 2);
    assert!(r.fake.is_open());
    assert!(!r.h.state().output.idle);
}

// ------------------------------------------------- actions during a held gapless switch (H25)

const ONE: &str = "fakea:track:1";
const TWO: &str = "fakea:track:2";

fn current(s: &PlayerState) -> Option<&str> {
    s.current.as_ref().map(|c| c.track.uri.as_str())
}

/// Waits until the provider was called `want`, and returns every call so far.
fn wait_call(r: &PlayerRig, what: &str, want: impl Fn(&[String]) -> bool) -> Vec<String> {
    let start = Instant::now();
    loop {
        let calls = r.provider.calls();
        if want(&calls) {
            return calls;
        }
        assert!(start.elapsed() < DEADLINE, "no {what}: {calls:?}");
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Plays track one of [`ONE`, `TWO`] at 179 s with its last audio queued (the fake output is
/// never pulled, so it stays queued), then ends it: the switch to two is held, its load done.
async fn held_switch(mode: OutputMode) -> PlayerRig {
    let r = pause_rig_with(mode, &[ONE, TWO]).await;
    assert!(r.fake.is_open());
    r.provider.emit(UiEvent::Position {
        uri: ONE.into(),
        position_ms: 179_000,
    });
    wait_state(&r.h, "the position", |s| s.position_ms > 100_000).await;
    r.provider.emit(UiEvent::EndOfTrack { uri: ONE.into() });
    let next = format!("load {TWO} true 0");
    wait_call(&r, "next load", |c| c.contains(&next));
    // The load's report follows the call at once; give the player its turn.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let s = r.h.state();
    assert_eq!(current(&s), Some(ONE), "the switch is held");
    assert_eq!(s.play_state, PlayState::Playing);
    r
}

// F1 (M10 review): a long pause during a held switch gives the device back with the old track's
// tail; the next track, never heard, becomes now-playing paused at 0 and the provider is sought
// to 0, not to the old track's position.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_pause_during_a_held_switch_releases_onto_the_next_track_at_0() {
    let r = held_switch(OutputMode::Exclusive).await;
    r.pause().await;
    wait_state(&r.h, "the device released", |s| s.output.idle).await;
    let calls = wait_call(&r, "seek", |c| c.iter().any(|l| l.starts_with("seek ")));
    let seeks: Vec<&String> = calls.iter().filter(|l| l.starts_with("seek ")).collect();
    assert_eq!(seeks, ["seek 0"], "{calls:?}");
    let s = r.h.state();
    assert_eq!(current(&s), Some(TWO));
    assert_eq!(s.play_state, PlayState::Paused);
    assert_eq!(s.position_now(), 0);
    r.resume().await;
    r.write(false).await;
    assert_eq!(current(&r.h.state()), Some(TWO));
}

// F1: Previous past 3 s during a held switch restarts the heard track (a seek would restart the
// next one, which the provider already plays).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn previous_during_a_held_switch_restarts_the_heard_track() {
    let r = held_switch(OutputMode::Shared).await;
    r.h.dispatch(PlayerAction::Previous);
    let reload = format!("load {ONE} true 0");
    let calls = wait_call(&r, "reload", |c| {
        c.iter().filter(|l| **l == reload).count() == 2
    });
    assert!(!calls.iter().any(|l| l.starts_with("seek ")), "{calls:?}");
    wait_state(&r.h, "the heard track restarted", |s| {
        current(s) == Some(ONE) && s.play_state == PlayState::Playing && s.position_now() < 3_000
    })
    .await;
    // The cancelled switch does not come back.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(current(&r.h.state()), Some(ONE));
}

// F1: a seek during a held switch reloads the heard track at the target, playing or paused as
// it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seek_during_a_held_switch_reloads_the_heard_track_at_the_target() {
    let r = held_switch(OutputMode::Shared).await;
    r.h.dispatch(PlayerAction::Seek {
        position_ms: 60_000,
    });
    let calls = wait_call(&r, "reload", |c| {
        c.contains(&format!("load {ONE} true 60000"))
    });
    assert!(!calls.iter().any(|l| l.starts_with("seek ")), "{calls:?}");
    wait_state(&r.h, "the heard track at the target", |s| {
        current(s) == Some(ONE)
            && s.play_state == PlayState::Playing
            && (60_000..63_000).contains(&s.position_now())
    })
    .await;

    // Paused: the reload waits paused at the target.
    let r = held_switch(OutputMode::Shared).await;
    r.pause().await;
    r.h.dispatch(PlayerAction::Seek {
        position_ms: 30_000,
    });
    wait_call(&r, "paused reload", |c| {
        c.contains(&format!("load {ONE} false 30000"))
    });
    wait_state(&r.h, "paused at the target", |s| {
        current(s) == Some(ONE) && s.play_state == PlayState::Paused && s.position_now() == 30_000
    })
    .await;
}
