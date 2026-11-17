//! The engine's real-time protocol on a fake output (MD37): null, gapless, flush, interrupt,
//! pause, end versus starve, reopen, faults, blocking and stall, and the declick at a flush
//! and an interrupt (H26). No device is opened; every
//! signal is built here.

use std::time::{Duration, Instant};

use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputFault, OutputMode, OutputRequest, SampleFormat,
    SourceError, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;

const RATE: u32 = 44_100;
/// 250 ms at 44.1 kHz: the ring's soft depth.
const DEPTH: usize = 11_025;

fn device(sample: SampleFormat, valid_bits: u8) -> DeviceFormat {
    DeviceFormat {
        rate: RATE,
        channels: 2,
        sample,
        valid_bits,
    }
}

fn track(bits: u8) -> StreamFormat {
    StreamFormat {
        rate: RATE,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(bits),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// An engine on an opened fake output.
fn rig(dev: DeviceFormat, config: EngineConfig) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(dev, OutputMode::Exclusive);
    let parts = Engine::new(config, Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    (engine, parts.source, fake)
}

fn i16_rig() -> (Engine, SourceWriter, FakeOutput) {
    rig(device(SampleFormat::I16, 16), EngineConfig::default())
}

/// Interleave integer frames as samples scaled by `1 / 2^(bits-1)`.
fn samples(frames: &[[i32; 2]], bits: u32) -> Vec<f64> {
    let scale = f64::from(1u32 << (bits - 1));
    frames
        .iter()
        .flat_map(|f| [f64::from(f[0]) / scale, f64::from(f[1]) / scale])
        .collect()
}

/// A signal of `n` distinct, nonzero 16-bit frames starting at `base`.
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

/// Write `frames` and render as much, alternating in chunks, so the ring never runs short.
fn stream<T>(
    src: &SourceWriter,
    frames: &[[i32; 2]],
    bits: u32,
    render: impl Fn(usize) -> Vec<T>,
) -> Vec<T> {
    let mut out = Vec::new();
    for chunk in frames.chunks(2_000) {
        src.write(&samples(chunk, bits)).unwrap();
        out.extend(render(chunk.len()));
    }
    out
}

/// Frames covering every extreme: L starts at the minimum, R at the maximum.
fn extremes(bits: u32, n: usize, step: i64) -> Vec<[i32; 2]> {
    let span = 1i64 << bits;
    let min = -(1i64 << (bits - 1));
    let max = (1i64 << (bits - 1)) - 1;
    (0..n as i64)
        .map(|i| {
            let o = (i * step) % span;
            [(min + o) as i32, (max - o) as i32]
        })
        .collect()
}

/// Frames in a declick tail or a fade from full gain: 5 ms at 44.1 kHz, rounded up.
const TAIL: usize = 221;

/// The declick tail's gain on its frame `j` (from full gain to zero in `TAIL + 1` steps).
fn tail_gain(j: usize) -> f64 {
    1.0 - (j + 1) as f64 / (TAIL + 1) as f64
}

/// The fade-in's gain on its frame `j` (the pause fade's 5 ms ramp).
fn fade_in_gain(j: usize) -> f64 {
    ((j + 1) as f64 / 220.5).min(1.0)
}

/// `out[j]` is `src[j]` times `gain(j)`, rounded to the nearest code (within half an LSB), for
/// every frame of `out`.
fn assert_gained(out: &[[i32; 2]], src: &[[i32; 2]], gain: impl Fn(usize) -> f64, what: &str) {
    assert!(out.len() <= src.len());
    for (j, (o, s)) in out.iter().zip(src).enumerate() {
        for c in 0..2 {
            let want = f64::from(s[c]) * gain(j);
            assert!(
                // Exact rule: round to nearest is within 0.5 LSB (measured 0.5 at most on
                // 2026-11-14, 1.92 x86_64); 1e-6 for the f64 product.
                (f64::from(o[c]) - want).abs() <= 0.5 + 1e-6,
                "{what} frame {j} ch {c}: {} vs {want}",
                o[c]
            );
        }
    }
}

/// A full-scale 1 kHz sine's frame `i`, 1 rad off a zero crossing at every 441-frame
/// boundary so that a hard seam there shows its step; `sign` -1 is the opposite phase.
fn sine_1k(i: usize, sign: f64) -> f64 {
    sign * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / f64::from(RATE) + 1.0).sin()
}

/// Interleaved stereo frames `range` of `sine_1k`.
fn sine_frames(range: std::ops::Range<usize>, sign: f64) -> Vec<f64> {
    range
        .flat_map(|i| {
            let v = sine_1k(i, sign);
            [v, v]
        })
        .collect()
}

/// The largest step between consecutive samples.
fn max_step(x: &[f64]) -> f64 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f64::max)
}

/// The left channel of interleaved F32 output.
fn left(out: &[f32]) -> Vec<f64> {
    out.iter().step_by(2).map(|&v| f64::from(v)).collect()
}

/// Render `total` frames in callbacks of at most `chunk` frames.
fn render_in(fake: &FakeOutput, total: usize, chunk: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut done = 0;
    while done < total {
        let n = chunk.min(total - done);
        out.extend(fake.render_f32(n));
        done += n;
    }
    out
}

/// The step bound across a declicked seam: the sine's own largest step times 1.5.
fn seam_bound() -> f64 {
    let a: Vec<f64> = (0..4_410).map(|i| sine_1k(i, 1.0)).collect();
    1.5 * max_step(&a)
}
#[test]
fn t1_null() {
    let src16 = extremes(16, 10_000, 13);
    assert!(src16.iter().any(|f| f[0] == -32_768) && src16.iter().any(|f| f[1] == 32_767));

    // 16-bit source into I16.
    let (engine, src, fake) = i16_rig();
    src.start_track(track(16)).unwrap();
    let out = stream(&src, &src16, 16, |n| fake.render_i16(n));
    assert_eq!(frames16(&out), src16);
    assert!(engine.status().applied.exact);
    assert_eq!(engine.status().clips, 0);

    // 16-bit source into I32/32.
    let (engine, src, fake) = rig(device(SampleFormat::I32, 32), EngineConfig::default());
    src.start_track(track(16)).unwrap();
    let out = stream(&src, &src16, 16, |n| fake.render_i32(n));
    let want: Vec<i32> = src16
        .iter()
        .flat_map(|f| [f[0] << 16, f[1] << 16])
        .collect();
    assert_eq!(out, want);
    assert!(engine.status().applied.exact);

    // 24-bit source into I32/24.
    let src24 = extremes(24, 10_000, 839);
    let (engine, src, fake) = rig(device(SampleFormat::I32, 24), EngineConfig::default());
    src.start_track(track(24)).unwrap();
    let out = stream(&src, &src24, 24, |n| fake.render_i32(n));
    let want: Vec<i32> = src24.iter().flat_map(|f| [f[0] << 8, f[1] << 8]).collect();
    assert_eq!(out, want);
    assert!(engine.status().applied.exact);

    // 16-bit source into F32.
    let (engine, src, fake) = rig(device(SampleFormat::F32, 32), EngineConfig::default());
    src.start_track(track(16)).unwrap();
    let out = stream(&src, &src16, 16, |n| fake.render_f32(n));
    let want: Vec<f32> = src16
        .iter()
        .flat_map(|f| {
            [
                (f64::from(f[0]) / 32_768.0) as f32,
                (f64::from(f[1]) / 32_768.0) as f32,
            ]
        })
        .collect();
    assert_eq!(out, want);
    assert!(engine.status().applied.exact);
}

#[test]
fn t2_gapless_split_equals_unsplit() {
    let sine: Vec<[i32; 2]> = (0..30_000)
        .map(|i| {
            let s = (0.5
                * 32_767.0
                * (2.0 * std::f64::consts::PI * 441.0 * f64::from(i) / f64::from(RATE)).sin())
            .round() as i32;
            [s, -s]
        })
        .collect();
    let (a, b) = sine.split_at(12_345);
    let (engine, src, fake) = i16_rig();
    let mut out = Vec::new();
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a[..5_000], 16)).unwrap();
    out.extend(fake.render_i16(3_000));
    src.write(&samples(&a[5_000..10_000], 16)).unwrap();
    out.extend(fake.render_i16(3_000));
    src.write(&samples(&a[10_000..], 16)).unwrap();
    src.end();
    out.extend(fake.render_i16(3_000));
    // Second track issued after rendering part of the first.
    src.start_track(track(16)).unwrap();
    let mut backlog = a.len() - 9_000;
    for chunk in b.chunks(3_000) {
        src.write(&samples(chunk, 16)).unwrap();
        backlog += chunk.len();
        out.extend(fake.render_i16(3_000.min(backlog)));
        backlog -= 3_000.min(backlog);
    }
    out.extend(fake.render_i16(backlog));
    assert_eq!(frames16(&out), sine);
    assert_eq!(engine.status().underruns, 0);
}

#[test]
fn t3_flush_drops_everything_before_it() {
    let (_engine, src, fake) = i16_rig();
    let a = ramp(1_000, 4_410);
    let b = ramp(-25_000, 2_000);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(441)), a[..441]);
    src.flush();
    src.write(&samples(&b, 16)).unwrap();
    let out = frames16(&fake.render_i16(1_500));
    // Only the declick tail of what came next (H26): faded to zero, then the new content
    // fading in; no pre-flush sample after the tail (a's left channel is positive, b's
    // negative).
    assert_gained(&out[..TAIL], &a[441..], tail_gain, "tail");
    assert!(out[TAIL..].iter().all(|f| f[0] <= 0));
    assert_gained(&out[TAIL..], &b, fade_in_gain, "fade-in");
    assert_eq!(out[TAIL + TAIL..], b[TAIL..1_500 - TAIL]);
}

#[test]
fn t4_interrupt_silences_and_drops_until_acknowledged() {
    let (_engine, src, fake) = i16_rig();
    let a = ramp(1_000, 4_410);
    let x = ramp(21_000, 1_000);
    let b = ramp(-25_000, 2_000);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(441)), a[..441]);
    src.interrupt();
    // The declick tail (H26), then silence.
    let out = frames16(&fake.render_i16(441));
    assert_gained(&out[..TAIL], &a[441..], tail_gain, "tail");
    assert!(out[TAIL..].iter().all(|f| *f == [0, 0]));
    assert_eq!(src.write(&samples(&x, 16)), Ok(()));
    assert!(fake.render_i16(441).iter().all(|&s| s == 0));
    src.start_track(track(16)).unwrap();
    src.write(&samples(&b, 16)).unwrap();
    let out = frames16(&fake.render_i16(2_000));
    assert_gained(&out, &b, fade_in_gain, "fade-in");
    assert_eq!(out[TAIL..], b[TAIL..]);
}
#[test]
fn t5_pause_fades_keeps_the_ring_and_resumes_at_the_next_frame() {
    let (engine, src, fake) = i16_rig();
    let a = ramp(1_000, 4_410);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(441)), a[..441]);
    let fill = engine.status().ring_fill as usize;
    assert_eq!(fill, 4_410 - 441);

    src.pause();
    let out = frames16(&fake.render_i16(441));
    // 5 ms at 44.1 kHz is 220.5 frames.
    let consumed = fill - engine.status().ring_fill as usize;
    assert!(consumed <= 221, "fade-out consumed {consumed} frames");
    assert!(out[220..].iter().all(|f| *f == [0, 0]));
    for (o, s) in out.iter().zip(&a[441..]) {
        assert!(o[0].abs() <= s[0].abs() && o[1].abs() <= s[1].abs());
    }
    // Paused: silent, nothing consumed, no underrun.
    assert!(fake.render_i16(4_410).iter().all(|&s| s == 0));
    assert_eq!(engine.status().ring_fill as usize, fill - consumed);

    src.resume();
    let out = frames16(&fake.render_i16(1_000));
    let next = 441 + consumed;
    assert_eq!(out[221..], a[next + 221..next + 1_000]);
    for (o, s) in out.iter().zip(&a[next..]) {
        assert!(o[0].abs() <= s[0].abs() && o[1].abs() <= s[1].abs());
    }
    assert_eq!(engine.status().underruns, 0);
}

#[test]
fn t6_end_is_not_an_underrun_starving_is() {
    let a = ramp(1_000, 882);
    let (engine, src, fake) = i16_rig();
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    src.end();
    let out = frames16(&fake.render_i16(2_205));
    assert_eq!(out[..882], a[..]);
    assert!(out[882..].iter().all(|f| *f == [0, 0]));
    assert_eq!(engine.status().underruns, 0);
    assert!(engine.status().ending);

    let (engine, src, fake) = i16_rig();
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    fake.render_i16(2_205);
    assert!(engine.status().underruns >= 1);
}

#[test]
fn t7_reopen_discards_what_was_queued() {
    let (mut engine, src, fake) = i16_rig();
    let a = ramp(1_000, 4_410);
    let b = ramp(-25_000, 2_000);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(441)), a[..441]);
    engine.close();
    assert!(engine.output().is_none());
    engine.open(&OutputRequest::default()).unwrap();
    assert!(fake.render_i16(441).iter().all(|&s| s == 0));
    src.write(&samples(&b, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(2_000)), b);
}

#[test]
fn t8_no_output_and_faults() {
    let fake = FakeOutput::new(device(SampleFormat::I16, 16), OutputMode::Exclusive);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let (mut engine, src) = (parts.engine, parts.source);
    assert_eq!(src.write(&[0.0, 0.0]), Err(SourceError::NoOutput));
    assert_eq!(src.start_track(track(16)), Err(SourceError::NoOutput));

    engine.open(&OutputRequest::default()).unwrap();
    src.start_track(track(16)).unwrap();
    src.write(&samples(&ramp(1_000, 100), 16)).unwrap();
    fake.fault(OutputFault::DeviceLost);
    assert_eq!(src.write(&[0.0, 0.0]), Err(SourceError::OutputFailed));
    assert_eq!(engine.poll_fault(), Some(OutputFault::DeviceLost));
    assert_eq!(engine.poll_fault(), None);
    assert!(engine.output().is_none());
    assert_eq!(src.write(&[0.0, 0.0]), Err(SourceError::OutputFailed));

    // Dropping the engine leaves the writer with no output.
    drop(engine);
    assert_eq!(src.write(&[0.0, 0.0]), Err(SourceError::NoOutput));
}

#[test]
fn t9_blocked_writer_wakes_on_render_and_on_interrupt() {
    // Wakes when the output consumes.
    let (engine, src, fake) = i16_rig();
    src.start_track(track(16)).unwrap();
    let data = samples(&ramp(1_000, 3 * DEPTH), 16);
    let writer = {
        let src = src.clone();
        let data = data.clone();
        std::thread::spawn(move || src.write(&data))
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(!writer.is_finished(), "writer should block on a full ring");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !writer.is_finished() {
        assert!(Instant::now() < deadline, "writer never woke");
        fake.render_i16(1_024);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(writer.join().unwrap(), Ok(()));
    assert_eq!(engine.status().source_frames, 3 * DEPTH as u64);

    // Wakes within 100 ms of an interrupt.
    let (_engine, src, _fake) = i16_rig();
    src.start_track(track(16)).unwrap();
    let writer = {
        let src = src.clone();
        std::thread::spawn(move || src.write(&data))
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(!writer.is_finished(), "writer should block on a full ring");
    let t = Instant::now();
    src.interrupt();
    while !writer.is_finished() {
        // measured 1.5 to 2.5 ms (2026-11-14, 1.92 x86_64); a liveness limit, kept loose on
        // purpose: scheduler latency on a loaded host, not a signal property.
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "writer did not wake"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(writer.join().unwrap(), Ok(()));
}

#[test]
fn t10_stall_times_out() {
    let config = EngineConfig {
        stall_timeout: Duration::from_millis(100),
        ..EngineConfig::default()
    };
    let (_engine, src, _fake) = rig(device(SampleFormat::I16, 16), config);
    src.start_track(track(16)).unwrap();
    let t = Instant::now();
    let r = src.write(&samples(&ramp(1_000, 2 * DEPTH), 16));
    assert_eq!(r, Err(SourceError::Stalled));
    // measured 100 to 102 ms against a 100 ms stall timeout (2026-11-14, 1.92 x86_64);
    // bound 1 s: a liveness limit, loose for loaded hosts.
    assert!(t.elapsed() < Duration::from_secs(1));
}
#[test]
fn t11_flush_declick_bounds_the_step() {
    let (engine, src, fake) = rig(device(SampleFormat::F32, 32), EngineConfig::default());
    src.start_track(track(16)).unwrap();
    src.write(&sine_frames(0..4_410, 1.0)).unwrap();
    let mut out = fake.render_f32(441);
    assert!(engine.status().applied.exact);
    src.flush();
    src.write(&sine_frames(441..2_441, -1.0)).unwrap();
    out.extend(fake.render_f32(1_000));
    // Faded frames are not exact; dither follows MD47 (a fade alone rounds without it).
    let st = engine.status().applied;
    assert!(st.fade && !st.exact && !st.dither);
    out.extend(fake.render_f32(1_000));
    let st = engine.status().applied;
    assert!(st.exact && !st.fade);

    let bound = seam_bound();
    let worst = max_step(&left(&out));
    assert!(worst <= bound, "step {worst} over {bound}");
    // The instrument: the same seam as a hard jump steps far past the bound.
    let hard: Vec<f64> = (0..441)
        .map(|i| sine_1k(i, 1.0))
        .chain((441..2_441).map(|i| sine_1k(i, -1.0)))
        .collect();
    assert!(max_step(&hard) > 4.0 * bound);
}

#[test]
fn t12_interrupt_declick_bounds_the_step() {
    let (engine, src, fake) = rig(device(SampleFormat::F32, 32), EngineConfig::default());
    src.start_track(track(16)).unwrap();
    src.write(&sine_frames(0..4_410, 1.0)).unwrap();
    let mut out = fake.render_f32(441);
    src.interrupt();
    out.extend(fake.render_f32(441));
    let st = engine.status().applied;
    assert!(st.fade && !st.exact);
    // The tail, then silence while discarding.
    let l = left(&out);
    assert!(l[441..441 + TAIL].iter().all(|v| *v != 0.0));
    assert!(l[441 + TAIL..].iter().all(|v| *v == 0.0));
    src.start_track(track(16)).unwrap();
    src.write(&sine_frames(882..2_882, -1.0)).unwrap();
    out.extend(fake.render_f32(1_000));
    out.extend(fake.render_f32(1_000));
    assert!(engine.status().applied.exact);

    let bound = seam_bound();
    let worst = max_step(&left(&out));
    assert!(worst <= bound, "step {worst} over {bound}");
    // The instrument: interrupting to silence and starting the new content hard would step
    // far past the bound.
    let hard: Vec<f64> = (0..441)
        .map(|i| sine_1k(i, 1.0))
        .chain([0.0])
        .chain((882..2_882).map(|i| sine_1k(i, -1.0)))
        .collect();
    assert!(max_step(&hard) > 3.0 * bound);
}

#[test]
fn t13_flush_while_paused_stays_silent() {
    let (engine, src, fake) = i16_rig();
    let a = ramp(1_000, 4_410);
    let b = ramp(-25_000, 2_000);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(441)), a[..441]);
    src.pause();
    fake.render_i16(441);
    src.flush();
    src.write(&samples(&b, 16)).unwrap();
    assert!(fake.render_i16(4_410).iter().all(|&s| s == 0));
    assert!(engine.status().paused);
    assert_eq!(engine.status().ring_fill as usize, b.len());
    // Resumed: the new content fades in from its first frame.
    src.resume();
    let out = frames16(&fake.render_i16(1_000));
    assert_gained(&out, &b, fade_in_gain, "fade-in");
    assert_eq!(out[TAIL..], b[TAIL..1_000]);
    assert_eq!(engine.status().underruns, 0);
}

#[test]
fn t14_flush_with_nothing_queued_fades_in_only() {
    let (_engine, src, fake) = i16_rig();
    let a = ramp(1_000, 882);
    let b = ramp(-25_000, 2_000);
    src.start_track(track(16)).unwrap();
    src.write(&samples(&a, 16)).unwrap();
    assert_eq!(frames16(&fake.render_i16(882)), a);
    src.flush();
    src.write(&samples(&b, 16)).unwrap();
    let out = frames16(&fake.render_i16(1_500));
    assert_gained(&out, &b, fade_in_gain, "fade-in");
    assert_eq!(out[TAIL..], b[TAIL..1_500]);
}

#[test]
fn t15_declick_spans_callbacks() {
    // Split callbacks (37 frames, and single frames, so every fade crosses several) equal one
    // callback each.
    let flush = |chunk: usize| {
        let (_engine, src, fake) = rig(device(SampleFormat::F32, 32), EngineConfig::default());
        src.start_track(track(16)).unwrap();
        src.write(&sine_frames(0..4_410, 1.0)).unwrap();
        let mut out = fake.render_f32(441);
        src.flush();
        src.write(&sine_frames(441..2_441, -1.0)).unwrap();
        out.extend(render_in(&fake, 1_500, chunk));
        out
    };
    let whole = flush(1_500);
    assert_eq!(flush(37), whole);
    assert_eq!(flush(1), whole);

    let interrupt = |chunk: usize| {
        let (_engine, src, fake) = rig(device(SampleFormat::F32, 32), EngineConfig::default());
        src.start_track(track(16)).unwrap();
        src.write(&sine_frames(0..4_410, 1.0)).unwrap();
        let mut out = fake.render_f32(441);
        src.interrupt();
        out.extend(render_in(&fake, 500, chunk));
        src.start_track(track(16)).unwrap();
        src.write(&sine_frames(941..2_941, -1.0)).unwrap();
        out.extend(render_in(&fake, 1_500, chunk));
        out
    };
    let whole = interrupt(1_500);
    assert_eq!(interrupt(37), whole);
    assert_eq!(interrupt(1), whole);
}
