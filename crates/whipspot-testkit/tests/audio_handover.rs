//! The engine's per-track ReplayGain stage (D8) and source ownership between providers (D13),
//! on fake outputs. No device is opened; every signal is built here.

use std::time::{Duration, Instant};

use whipspot_core::diagnostics::{Stage, Verdict};
use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputMode, OutputRequest, PACE_CAP, ReplayGain,
    ReplayGainMode, SampleFormat, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;

const RATE: u32 = 44_100;
/// The pause fade and the declick fade-in after a flush: 5 ms.
const FADE: usize = 221;

fn device(sample: SampleFormat, valid_bits: u8) -> DeviceFormat {
    DeviceFormat {
        rate: RATE,
        channels: 2,
        sample,
        valid_bits,
    }
}

fn track(bits: u8, replay_gain: Option<ReplayGain>) -> StreamFormat {
    StreamFormat {
        rate: RATE,
        channels: 2,
        source: SourceInfo {
            codec: "flac",
            bits: Some(bits),
            bitrate_kbps: None,
            lossless: true,
            replay_gain,
        },
    }
}

fn gain(track_db: f64) -> Option<ReplayGain> {
    Some(ReplayGain {
        track_db: Some(track_db),
        ..ReplayGain::default()
    })
}

fn rig(dev: DeviceFormat) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(dev, OutputMode::Exclusive);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    (engine, parts.source, fake)
}

fn f32_rig() -> (Engine, SourceWriter, FakeOutput) {
    rig(device(SampleFormat::F32, 32))
}

/// 16-bit-valued stereo samples (distinct, nonzero frames), well under full scale.
fn pcm16(n: usize) -> Vec<f64> {
    (0..n as i32)
        .flat_map(|i| {
            let k = 1_000 + (i * 7) % 20_000;
            [f64::from(k) / 32_768.0, -f64::from(k) / 32_768.0]
        })
        .collect()
}

/// Interleaved stereo with a left channel in `[base, base + 0.1)` and the right its negative.
fn level(base: f64, n: usize) -> Vec<f64> {
    (0..n)
        .flat_map(|i| {
            let v = base + 0.1 * ((i % 997) as f64 / 997.0);
            [v, -v]
        })
        .collect()
}

fn to_samples(out: &[i16]) -> Vec<f64> {
    out.iter().map(|&x| f64::from(x) / 32_768.0).collect()
}

fn f32s(x: &[f64]) -> Vec<f32> {
    x.iter().map(|&v| v as f32).collect()
}

fn scaled(x: &[f64], g: f64) -> Vec<f32> {
    x.iter().map(|&v| (v * g) as f32).collect()
}

fn reasons(v: &Option<Verdict>) -> Vec<String> {
    match v {
        Some(Verdict::LosslessProcessed { reasons }) | Some(Verdict::Resampled { reasons }) => {
            reasons.clone()
        }
        _ => Vec::new(),
    }
}

/// Write `input` and render the same number of frames, 1 000 at a time, as I16.
fn play_i16(src: &SourceWriter, fake: &FakeOutput, input: &[f64]) -> Vec<i16> {
    let mut out = Vec::new();
    for chunk in input.chunks(2_000) {
        src.write(chunk).unwrap();
        out.extend(fake.render_i16(chunk.len() / 2));
    }
    out
}

#[test]
fn r1_off_or_untagged_is_bit_perfect_and_active_is_processed() {
    let input = pcm16(10_000);
    let i16_dev = device(SampleFormat::I16, 16);

    // (a) Tags present, mode off: the output is the input and the path stays bit-perfect.
    let (engine, src, fake) = rig(i16_dev);
    src.start_track(track(16, gain(-6.0))).unwrap();
    assert_eq!(to_samples(&play_i16(&src, &fake, &input)), input);
    let d = engine.monitor().diagnostics();
    assert_eq!(d.verdict, Some(Verdict::BitPerfect));
    assert_eq!(d.dsp.replay_gain, Some(Stage::Bypassed));
    assert_eq!(d.dsp.replay_gain_db, None);

    // (b) Track and album mode, no tags: bypassed, bit-perfect.
    for mode in [ReplayGainMode::Track, ReplayGainMode::Album] {
        let (mut engine, src, fake) = rig(i16_dev);
        engine.set_replay_gain(mode);
        src.start_track(track(16, None)).unwrap();
        assert_eq!(to_samples(&play_i16(&src, &fake, &input)), input);
        let d = engine.monitor().diagnostics();
        assert_eq!(d.verdict, Some(Verdict::BitPerfect), "{mode:?}");
        assert_eq!(d.dsp.replay_gain, Some(Stage::Bypassed));
    }

    // (c) Track mode, -6 dB: the stage is active, the output differs and the verdict says so.
    let (mut engine, src, fake) = rig(i16_dev);
    engine.set_replay_gain(ReplayGainMode::Track);
    src.start_track(track(16, gain(-6.0))).unwrap();
    assert_ne!(to_samples(&play_i16(&src, &fake, &input)), input);
    let d = engine.monitor().diagnostics();
    assert!(
        matches!(d.verdict, Some(Verdict::LosslessProcessed { .. })),
        "{:?}",
        d.verdict
    );
    let r = reasons(&d.verdict);
    assert!(r.iter().any(|t| t == "ReplayGain"), "{r:?}");
    assert!(r.iter().any(|t| t == "TPDF dither"), "{r:?}");
    assert_eq!(d.dsp.replay_gain, Some(Stage::Active));
    let db = d.dsp.replay_gain_db.unwrap();
    // measured 0 (the f32 of -6.0); bound 1e-4 (f32 rounding).
    assert!((db + 6.0).abs() < 1e-4, "{db}");
}

#[test]
fn r2_track_gain_scales_the_samples() {
    let (mut engine, src, fake) = f32_rig();
    engine.set_replay_gain(ReplayGainMode::Track);
    src.start_track(track(24, gain(-6.02))).unwrap();
    let input = level(0.3, 4_000);
    src.write(&input).unwrap();
    let out = fake.render_f32(4_000);
    // The stage multiplies in f64 (the core's unit test checks 1e-12 there); the F32 output is
    // exactly its rounding.
    assert_eq!(out, scaled(&input, 10f64.powf(-6.02 / 20.0)));
    let s = engine.status();
    assert!(s.applied.replay_gain && !s.applied.exact);
}

#[test]
fn r3_gapless_change_switches_gain_at_the_first_sample() {
    let (mut engine, src, fake) = f32_rig();
    engine.set_replay_gain(ReplayGainMode::Album);
    let a = level(0.3, 3_000);
    let b = level(0.5, 3_000);
    // A at -3 dB (album mode falls back to the track gain), then B untagged, gaplessly.
    src.start_track(track(24, gain(-3.0))).unwrap();
    src.write(&a).unwrap();
    src.end();
    src.start_track(track(24, None)).unwrap();
    src.write(&b).unwrap();
    // The change of track lands inside the second callback.
    let mut out = fake.render_f32(2_000);
    out.extend(fake.render_f32(2_500));
    out.extend(fake.render_f32(1_500));
    let (oa, ob) = out.split_at(a.len());
    assert_eq!(oa, scaled(&a, 10f64.powf(-3.0 / 20.0)).as_slice());
    assert_eq!(ob, f32s(&b).as_slice());
    let s = engine.status();
    assert!(!s.applied.replay_gain && s.applied.exact, "{:?}", s.applied);
    assert_eq!(s.underruns, 0);

    // And the other way round: untagged, then tagged at -6 dB from its very first sample.
    let (mut engine, src, fake) = f32_rig();
    engine.set_replay_gain(ReplayGainMode::Track);
    src.start_track(track(24, None)).unwrap();
    src.write(&a).unwrap();
    src.end();
    src.start_track(track(24, gain(-6.0))).unwrap();
    src.write(&b).unwrap();
    let mut out = fake.render_f32(2_999);
    out.extend(fake.render_f32(3_001));
    let (oa, ob) = out.split_at(a.len());
    assert_eq!(oa, f32s(&a).as_slice());
    assert_eq!(ob, scaled(&b, 10f64.powf(-6.0 / 20.0)).as_slice());
}

#[test]
fn r4_mode_change_ramps_then_holds() {
    let (mut engine, src, fake) = f32_rig();
    src.start_track(track(24, gain(-6.0))).unwrap();
    let input = level(0.4, 3_000);
    src.write(&input).unwrap();
    // Off: untouched.
    assert_eq!(fake.render_f32(1_000), f32s(&input[..2_000]));
    engine.set_replay_gain(ReplayGainMode::Track);
    let out = fake.render_f32(2_000);
    let g = 10f64.powf(-6.0 / 20.0);
    // A 10 ms ramp (441 frames) from unity down to the gain, then the gain exactly.
    let ramp = 441;
    for i in 0..ramp {
        let x = input[2_000 + 2 * i];
        let y = f64::from(out[2 * i]);
        assert!(y < x && y > x * g - 1e-6, "frame {i}: {y} vs {x}");
    }
    assert_eq!(out[2 * ramp..], scaled(&input[2_000 + 2 * ramp..], g)[..]);
}

/// Owner A plays 1 000 frames; B takes over and writes `b` after its track start. `late` runs
/// right after the switch and again after B's writes (a late stop from A's own thread).
fn hand_over(b: &[f64], late: impl Fn(&SourceWriter)) -> (Engine, Vec<f32>) {
    let (engine, src, fake) = f32_rig();
    let switch = src.owner_switch();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    switch.set_owner("a");
    pa.start_track(track(24, None)).unwrap();
    let a = level(0.3, 1_000);
    pa.write(&a).unwrap();
    // The first owner change flushes too: A fades in.
    assert_eq!(fake.render_f32(1_000)[2 * FADE..], f32s(&a[2 * FADE..])[..]);
    // The transport: B owns the engine, then B loads and writes.
    switch.set_owner("b");
    late(&pa);
    pb.interrupt();
    pb.start_track(track(24, None)).unwrap();
    pb.write(b).unwrap();
    late(&pa);
    let out = fake.render_f32(b.len() / 2);
    (engine, out)
}

#[test]
fn o1_late_calls_from_the_previous_owner_change_nothing() {
    let b = level(-0.6, 3_000);
    let (_, reference) = hand_over(&b, |_| {});
    let (engine, out) = hand_over(&b, |a| {
        a.pause();
        a.finish();
        a.interrupt();
        a.flush();
        a.end();
        assert!(a.write(&level(0.9, 500)).is_ok());
        assert!(a.start_track(track(16, None)).is_ok());
    });
    assert_eq!(out, reference);
    assert!(!engine.status().paused);
    // B fades in after the switch's flush (the declick), then plays as written.
    assert_eq!(out[2 * FADE..], f32s(&b[2 * FADE..])[..]);
}

#[test]
fn o2_a_failed_load_gives_the_engine_back() {
    let (_engine, src, fake) = f32_rig();
    let switch = src.owner_switch();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    switch.set_owner("a");
    pa.start_track(track(24, None)).unwrap();
    pa.write(&level(0.3, 1_000)).unwrap();
    fake.render_f32(1_000);

    // B's load fails before it writes; the transport restores A, whose writes play on.
    switch.set_owner("b");
    switch.set_owner("a");
    assert_eq!(switch.owner().as_deref(), Some("a"));
    let a2 = level(0.2, 2_000);
    pa.write(&a2).unwrap();
    let out = fake.render_f32(2_000);
    assert_eq!(out[2 * FADE..], f32s(&a2[2 * FADE..])[..]);

    // B's load fails after its track start: A's writes in B's format are dropped until A
    // starts its next track (never A's data in another owner's format).
    switch.set_owner("b");
    pb.start_track(track(16, None)).unwrap();
    switch.set_owner("a");
    pa.write(&level(0.2, 500)).unwrap();
    assert!(fake.render_f32(500).iter().all(|&v| v == 0.0));
    pa.start_track(track(24, None)).unwrap();
    let a3 = level(0.1, 1_000);
    pa.write(&a3).unwrap();
    let out = fake.render_f32(1_000);
    assert_eq!(out[2 * FADE..], f32s(&a3[2 * FADE..])[..]);
}

#[test]
fn o3_the_previous_owners_queue_never_plays_after_the_new_audio() {
    let (_engine, src, fake) = f32_rig();
    let switch = src.owner_switch();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    switch.set_owner("a");
    pa.start_track(track(24, None)).unwrap();
    // A's left channel is positive, B's negative.
    pa.write(&level(0.3, 6_000)).unwrap();
    fake.render_f32(1_000);
    // 5 000 frames of A are still queued when B takes over without an interrupt of its own
    // (a gapless-looking load after A's end of track).
    switch.set_owner("b");
    pb.start_track(track(24, None)).unwrap();
    let b = level(-0.6, 2_000);
    pb.write(&b).unwrap();
    let out = fake.render_f32(8_000);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let first_b = left.iter().position(|&v| v < 0.0).expect("B plays");
    // At most the 5 ms declick of A before B, and no A after B's first sample.
    assert!(first_b <= FADE, "{first_b}");
    assert!(left[first_b..].iter().all(|&v| v <= 0.0));
    assert_eq!(
        out[2 * (first_b + FADE)..2 * (first_b + 2_000)],
        {
            let o = 2 * (first_b + FADE) - 2 * first_b;
            f32s(&b[o..])
        }[..]
    );
}

#[test]
fn o4_without_an_owner_every_handle_plays() {
    // No owner set: handles made for different owners and the plain one all act.
    let (engine, src, fake) = f32_rig();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    pa.start_track(track(24, None)).unwrap();
    let x = level(0.3, 1_000);
    pb.write(&x).unwrap();
    assert_eq!(fake.render_f32(1_000), f32s(&x));
    src.pause();
    assert!(engine.status().paused);
    pb.resume();
    assert!(!engine.status().paused);

    // An owner set: the plain handle still acts (only owner handles are checked).
    let switch = src.owner_switch();
    switch.set_owner("a");
    src.start_track(track(24, None)).unwrap();
    src.write(&x).unwrap();
    let out = fake.render_f32(1_000);
    assert_eq!(out[2 * FADE..], f32s(&x[2 * FADE..])[..]);
    // Setting the same owner again drops nothing.
    pa.write(&x).unwrap();
    switch.set_owner("a");
    assert_eq!(fake.render_f32(1_000), f32s(&x));
    switch.clear_owner();
    assert_eq!(switch.owner(), None);
}

#[test]
fn o5_a_previous_owner_writes_in_real_time_and_never_blocks_the_new_one() {
    let (_engine, src, fake) = f32_rig();
    let switch = src.owner_switch();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    switch.set_owner("a");
    pa.start_track(track(24, None)).unwrap();
    pa.write(&level(0.3, 1_000)).unwrap();
    fake.render_f32(1_000);
    switch.set_owner("b");

    // A's decoder goes on in 50 ms packets during B's load: 20 packets take 1 s, not decode
    // speed (F1).
    let decoder = {
        let pa = pa.clone();
        std::thread::spawn(move || {
            let packet = level(0.3, 2_205);
            let t = Instant::now();
            for _ in 0..20 {
                pa.write(&packet).unwrap();
            }
            t.elapsed()
        })
    };
    // Meanwhile B starts and plays unhindered (A paces without the producer lock).
    std::thread::sleep(Duration::from_millis(50));
    let t = Instant::now();
    pb.start_track(track(24, None)).unwrap();
    let b = level(-0.6, 3_000);
    pb.write(&b).unwrap();
    // Liveness bound, loose for loaded hosts: B never waits for A's pacing.
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    let out = fake.render_f32(3_000);
    assert_eq!(out[2 * FADE..], f32s(&b[2 * FADE..])[..]);

    let elapsed = decoder.join().unwrap();
    // Exact rule: at least the 1 s of audio dropped; the upper bound is a liveness limit.
    assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");

    // One huge write sleeps at most the cap, so a decoder's Stop is never held up for long.
    let t = Instant::now();
    pa.write(&level(0.3, 441_000)).unwrap();
    let elapsed = t.elapsed();
    assert!(elapsed >= PACE_CAP, "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    assert!(fake.render_f32(1_000).iter().all(|&v| v == 0.0));
}

#[test]
fn o6_a_switch_during_the_previous_owners_open_wait_still_drops_its_chunk() {
    let (mut engine, src, fake) = f32_rig();
    let switch = src.owner_switch();
    let (pa, pb) = (src.for_owner("a"), src.for_owner("b"));
    switch.set_owner("a");
    pa.start_track(track(24, None)).unwrap();
    pa.write(&level(0.3, 1_000)).unwrap();
    fake.render_f32(1_000);

    // The output is released; A's next write asks for an open and waits, holding the
    // producer. The transport switches to B during that wait (F2).
    engine.release();
    let writer = {
        let pa = pa.clone();
        std::thread::spawn(move || pa.write(&level(0.3, 2_000)))
    };
    let t = Instant::now();
    while engine.open_request().is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "A never asked for an open"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    switch.set_owner("b");
    assert!(engine.reopen(&[OutputRequest::default()]).opened.is_some());
    assert_eq!(writer.join().unwrap(), Ok(()));

    pb.start_track(track(24, None)).unwrap();
    let b = level(-0.6, 2_000);
    pb.write(&b).unwrap();
    let out = fake.render_f32(4_000);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    // No sample of A at all (the reopen dropped A's queue; its waiting chunk never lands), and
    // B plays.
    assert!(left.iter().all(|&v| v <= 0.0));
    let first_b = left.iter().position(|&v| v < 0.0).expect("B plays");
    assert_eq!(
        out[2 * (first_b + FADE)..2 * (first_b + 2_000)],
        f32s(&b[2 * FADE..])[..]
    );
}
