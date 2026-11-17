//! The engine's path on fake outputs (MD37): resampler smoke, gapless through the resampler,
//! verdict versus null, diagnostics. No device is opened; every signal is built here.

use std::f64::consts::PI;

use whipspot_core::diagnostics::{Resampler, Verdict};
use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, EqSettings, OutputMode, OutputRequest, SampleFormat,
    SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;

const SRC: u32 = 44_100;
const DEV: u32 = 48_000;
/// Input frames per write.
const CHUNK: usize = 1_000;

fn device(rate: u32, sample: SampleFormat, valid_bits: u8) -> DeviceFormat {
    DeviceFormat {
        rate,
        channels: 2,
        sample,
        valid_bits,
    }
}

fn format(rate: u32, bits: Option<u8>) -> StreamFormat {
    StreamFormat {
        rate,
        channels: 2,
        source: SourceInfo {
            codec: if bits.is_some() { "pcm" } else { "vorbis" },
            bits,
            bitrate_kbps: if bits.is_some() { None } else { Some(320) },
            lossless: bits.is_some(),
            replay_gain: None,
        },
    }
}

fn rig(dev: DeviceFormat, mode: OutputMode) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(dev, mode);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    (engine, parts.source, fake)
}

fn sine(freq: f64, amp: f64, rate: u32, frames: usize) -> Vec<f64> {
    (0..frames)
        .flat_map(|i| {
            let v = amp * (2.0 * PI * freq * i as f64 / f64::from(rate)).sin();
            [v, v]
        })
        .collect()
}

/// Write `input` (interleaved stereo at `SRC`) in chunks into a 48 kHz F32 fake, starting a
/// second track at frame `split` (`end()` + same-rate `start_track`) when given, then
/// `finish()`; render while writing so the ring never runs short, then the rest. Returns the
/// rendered output (interleaved) and the frame count the engine produced.
fn resample_run(
    engine: &Engine,
    src: &SourceWriter,
    fake: &FakeOutput,
    input: &[f64],
    split: Option<usize>,
) -> (Vec<f32>, usize) {
    src.start_track(format(SRC, None)).unwrap();
    let mut out = Vec::new();
    let mut rendered = 0usize;
    let mut written = 0usize;
    let frames = input.len() / 2;
    let write = |from: usize, to: usize| src.write(&input[2 * from..2 * to]).unwrap();
    while written < frames {
        let mut next = (written + CHUNK).min(frames);
        if let Some(s) = split.filter(|&s| s > written && s <= next) {
            write(written, s);
            src.end();
            src.start_track(format(SRC, None)).unwrap();
            written = s;
            next = (written + CHUNK).min(frames).max(s);
        }
        write(written, next);
        written = next;
        // Stay about 2000 frames behind what the producer has pushed (delay plus staging).
        let target = (written as u64 * u64::from(DEV) / u64::from(SRC)) as usize;
        let target = target.saturating_sub(2_000);
        if target > rendered {
            out.extend(fake.render_f32(target - rendered));
            rendered = target;
        }
    }
    src.finish();
    fake.render_f32(0); // publish the ring fill
    let queued = engine.status().ring_fill as usize;
    out.extend(fake.render_f32(queued));
    (out, rendered + queued)
}

#[test]
fn p1_resampler_smoke_and_p4_diagnostics() {
    let (engine, src, fake) = rig(device(DEV, SampleFormat::F32, 32), OutputMode::Shared);
    let input = sine(1000.0, 0.5, SRC, 2 * SRC as usize);
    let (out, produced) = resample_run(&engine, &src, &fake, &input, None);
    assert_eq!(produced, 96_000);
    assert!(out.iter().all(|x| x.is_finite()));
    let left: Vec<f64> = out.chunks_exact(2).map(|f| f64::from(f[0])).collect();
    let span = &left[DEV as usize / 10..(DEV as usize * 19) / 10];
    let rms = (span.iter().map(|x| x * x).sum::<f64>() / span.len() as f64).sqrt();
    let db = 20.0 * (rms / (0.5 / 2f64.sqrt())).log10();
    // measured 1.9e-8 dB (2026-11-14, 1.92 x86_64; f32 output rounding); bound 1e-6 dB.
    assert!(db.abs() < 1e-6, "rms {rms} ({db} dB)");
    let rising = span
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let freq = rising as f64 / 1.8;
    // Counting rising zero crossings resolves 1/1.8 = 0.556 Hz: measured 999.44 Hz (one
    // crossing short, the edge); bound one crossing, 0.6 Hz.
    assert!((freq - 1000.0).abs() <= 0.6, "{freq} Hz");
    let peak = left.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    // measured 0.49958 (2026-11-14, 1.92 x86_64): the sampled crest sits just under 0.5, no
    // overshoot; bound 0.5005 (0.1 % over the sine's amplitude).
    assert!(peak <= 0.5005, "peak {peak}");

    // P4: diagnostics after P1.
    let d = engine.monitor().diagnostics();
    assert_eq!(d.source.sample_rate_hz, Some(SRC));
    assert_eq!(d.output.sample_rate_hz, Some(DEV));
    match d.dsp.resampler {
        Some(Resampler::Active { ratio, .. }) => {
            // measured 0 (the same f64 division); bound 1e-12.
            assert!(
                (ratio - f64::from(DEV) / f64::from(SRC)).abs() < 1e-12,
                "{ratio}"
            );
        }
        other => panic!("resampler {other:?}"),
    }
    assert_eq!(d.output.underruns, Some(0));
    assert!(
        matches!(d.verdict, Some(Verdict::Resampled { .. })),
        "{:?}",
        d.verdict
    );
    assert_eq!(d.lossy_source, Some(true));
    assert_eq!(d.decode.frames_decoded, Some(2 * u64::from(SRC)));
}

#[test]
fn p2_gapless_through_the_resampler() {
    let input = sine(1000.0, 0.5, SRC, SRC as usize);
    let run = |split| {
        let (engine, src, fake) = rig(device(DEV, SampleFormat::F32, 32), OutputMode::Shared);
        let r = resample_run(&engine, &src, &fake, &input, split);
        assert_eq!(engine.status().underruns, 0);
        r
    };
    let (whole, n_whole) = run(None);
    let (split, n_split) = run(Some(20_123));
    assert_eq!(n_whole, n_split);
    assert_eq!(whole.len(), split.len());
    // Gapless is bit-exact: measured largest difference 0 (2026-11-14, 1.92 x86_64), so the
    // bound is equality.
    for (i, (a, b)) in whole.iter().zip(&split).enumerate() {
        assert_eq!(a, b, "sample {i}");
    }
}

/// 16-bit-valued stereo samples (distinct, nonzero frames).
fn pcm16(n: usize) -> Vec<f64> {
    (0..n as i32)
        .flat_map(|i| {
            let k = 1_000 + (i * 7) % 20_000;
            [f64::from(k) / 32_768.0, -f64::from(k) / 32_768.0]
        })
        .collect()
}

fn to_samples(out: &[i16]) -> Vec<f64> {
    out.iter().map(|&x| f64::from(x) / 32_768.0).collect()
}

/// Play `input` at the device rate (write then render the same count).
fn play_i16(src: &SourceWriter, fake: &FakeOutput, input: &[f64]) -> Vec<i16> {
    let mut out = Vec::new();
    for chunk in input.chunks(2 * CHUNK) {
        src.write(chunk).unwrap();
        out.extend(fake.render_i16(chunk.len() / 2));
    }
    out
}

fn reasons(v: &Option<Verdict>) -> Vec<String> {
    match v {
        Some(Verdict::LosslessProcessed { reasons }) | Some(Verdict::Resampled { reasons }) => {
            reasons.clone()
        }
        _ => Vec::new(),
    }
}

#[test]
fn p3_verdict_versus_null() {
    let input = pcm16(10_000);
    let i16_44 = device(SRC, SampleFormat::I16, 16);

    // (a) Neutral, exclusive, at the device rate: bit-perfect and the output is the input.
    let (engine, src, fake) = rig(i16_44, OutputMode::Exclusive);
    assert_eq!(engine.monitor().diagnostics().verdict, None);
    src.start_track(format(SRC, Some(16))).unwrap();
    let out = play_i16(&src, &fake, &input);
    assert_eq!(to_samples(&out), input);
    let d = engine.monitor().diagnostics();
    assert_eq!(d.verdict, Some(Verdict::BitPerfect));
    assert_eq!(d.lossy_source, Some(false));
    assert_eq!(d.output.format.as_deref(), Some("16-bit"));

    // (b) Band 4 at +3 dB: processed, with EQ among the reasons, and the output differs.
    let (mut engine, src, fake) = rig(i16_44, OutputMode::Exclusive);
    let mut eq = EqSettings::default();
    eq.bands_db[4] = 3.0;
    engine.set_eq(eq);
    src.start_track(format(SRC, Some(16))).unwrap();
    let out = play_i16(&src, &fake, &input);
    assert_ne!(to_samples(&out), input);
    let v = engine.monitor().diagnostics().verdict;
    assert!(
        matches!(v, Some(Verdict::LosslessProcessed { .. })),
        "{v:?}"
    );
    assert!(reasons(&v).iter().any(|r| r == "EQ"), "{v:?}");

    // (c) 44.1 kHz into a 48 kHz exclusive device: resampled.
    let (engine, src, fake) = rig(device(DEV, SampleFormat::I16, 16), OutputMode::Exclusive);
    src.start_track(format(SRC, None)).unwrap();
    for chunk in sine(1000.0, 0.5, SRC, 10_000).chunks(2 * CHUNK) {
        src.write(chunk).unwrap();
        fake.render_i16(500);
    }
    let d = engine.monitor().diagnostics();
    assert!(
        matches!(d.verdict, Some(Verdict::Resampled { .. })),
        "{:?}",
        d.verdict
    );
    assert!(
        reasons(&d.verdict).contains(&"resampled 44100 Hz to 48000 Hz (sinc)".to_owned()),
        "{:?}",
        d.verdict
    );
    assert_eq!(d.lossy_source, Some(true));

    // (d) Neutral into a shared device: the engine's output is exact, but the system mixer
    // follows, so not bit-perfect.
    let (engine, src, fake) = rig(i16_44, OutputMode::Shared);
    src.start_track(format(SRC, Some(16))).unwrap();
    let out = play_i16(&src, &fake, &input);
    assert_eq!(to_samples(&out), input);
    let v = engine.monitor().diagnostics().verdict;
    assert!(
        matches!(v, Some(Verdict::LosslessProcessed { .. })),
        "{v:?}"
    );
    assert!(
        reasons(&v).contains(&"shared mode: the system mixer processes the output".to_owned()),
        "{v:?}"
    );
}
