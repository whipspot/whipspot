//! The engine's DSP chain on a fake output (MD37): EQ response against an analytic RBJ
//! magnitude, headroom, limiter, bypass, volume ramp and dither. No device is opened; every
//! signal is built here.

mod common;

use std::f64::consts::PI;

use whipspot_core::engine::{
    BAND_COUNT, BAND_HZ, DeviceFormat, Engine, EngineConfig, EqSettings, OutputMode, OutputRequest,
    SampleFormat, SourceInfo, SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;

/// Frames written and rendered per round.
const CHUNK: usize = 2_000;

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
            bitrate_kbps: None,
            lossless: bits.is_some(),
            replay_gain: None,
        },
    }
}

/// An engine on an opened fake output, with a track started.
fn rig(
    dev: DeviceFormat,
    mode: OutputMode,
    bits: Option<u8>,
) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(dev, mode);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    parts.source.start_track(format(dev.rate, bits)).unwrap();
    (engine, parts.source, fake)
}

/// A shared F32 fake at `rate` playing a lossy source.
fn f32_rig(rate: u32) -> (Engine, SourceWriter, FakeOutput) {
    rig(
        device(rate, SampleFormat::F32, 32),
        OutputMode::Shared,
        None,
    )
}

/// Interleaved stereo sine (both channels equal).
fn sine(freq: f64, amp: f64, rate: u32, frames: usize) -> Vec<f64> {
    (0..frames)
        .flat_map(|i| {
            let v = amp * (2.0 * PI * freq * i as f64 / f64::from(rate)).sin();
            [v, v]
        })
        .collect()
}

/// Write `samples` (interleaved stereo) and render as much, alternating in chunks, so the
/// output is aligned with the input.
fn play<T>(src: &SourceWriter, samples: &[f64], render: impl Fn(usize) -> Vec<T>) -> Vec<T> {
    let mut out = Vec::new();
    for chunk in samples.chunks(2 * CHUNK) {
        src.write(chunk).unwrap();
        out.extend(render(chunk.len() / 2));
    }
    out
}

fn rms(samples: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = samples.fold((0.0, 0usize), |(s, n), x| (s + x * x, n + 1));
    (sum / n as f64).sqrt()
}

fn eq_band(band: usize, db: f32) -> EqSettings {
    let mut eq = EqSettings::default();
    eq.bands_db[band] = db;
    eq
}

// --- Analytic RBJ magnitude (Audio EQ Cookbook; independent of the engine's code) -------------

#[derive(Clone, Copy)]
enum Kind {
    LowShelf,
    Peaking,
    HighShelf,
}

/// |H(e^{jw})| in dB of one cookbook biquad (shelf slope S = 1, peaking Q = 1).
fn rbj_db(kind: Kind, f0: f64, gain_db: f64, rate: u32, freq: f64) -> f64 {
    let fs = f64::from(rate);
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * f0 / fs;
    let (s, c) = (w0.sin(), w0.cos());
    let (b, d): ([f64; 3], [f64; 3]) = match kind {
        Kind::Peaking => {
            let alpha = s / (2.0 * 1.0);
            (
                [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a],
                [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a],
            )
        }
        Kind::LowShelf | Kind::HighShelf => {
            let slope = 1.0;
            let alpha = s / 2.0 * ((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt();
            let k = 2.0 * a.sqrt() * alpha;
            let sign = if matches!(kind, Kind::LowShelf) {
                1.0
            } else {
                -1.0
            };
            // Low shelf as printed; the high shelf is the same with cos(w0) negated in the
            // (A ± 1) terms and the b1/a1 signs flipped.
            let cc = sign * c;
            (
                [
                    a * ((a + 1.0) - (a - 1.0) * cc + k),
                    sign * 2.0 * a * ((a - 1.0) - (a + 1.0) * cc),
                    a * ((a + 1.0) - (a - 1.0) * cc - k),
                ],
                [
                    (a + 1.0) + (a - 1.0) * cc + k,
                    -sign * 2.0 * ((a - 1.0) + (a + 1.0) * cc),
                    (a + 1.0) + (a - 1.0) * cc - k,
                ],
            )
        }
    };
    let w = 2.0 * PI * freq / fs;
    let eval = |p: [f64; 3]| {
        let re = p[0] + p[1] * w.cos() + p[2] * (2.0 * w).cos();
        let im = -(p[1] * w.sin() + p[2] * (2.0 * w).sin());
        (re * re + im * im).sqrt()
    };
    20.0 * (eval(b) / eval(d)).log10()
}

fn kind(band: usize) -> Kind {
    match band {
        0 => Kind::LowShelf,
        b if b == BAND_COUNT - 1 => Kind::HighShelf,
        _ => Kind::Peaking,
    }
}

/// Rates the EQ is checked at (H7): the standard 44.1 and 48 kHz families up to 384 kHz.
const RATES: [u32; 8] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// Agreement of the engine's EQ with the analytic RBJ magnitude, in dB: measured 4.9e-7 dB
/// at worst over all bands and rates (F32 output rounding limits the fit); bound about 8x.
const EQ_TOL_DB: f64 = 4e-6;

/// Measured gain (dB) of a 0.25-amplitude sine at `freq` through band `band` at `db`: the
/// least-squares amplitude of the output over the input's, after 0.1 s of settling and over
/// the next 0.2 s (a fit, so the window need not hold whole cycles). Also returns the
/// engine's headroom.
fn measure(rate: u32, band: usize, db: f32, freq: f64) -> (f64, f64) {
    let (mut engine, src, fake) = f32_rig(rate);
    engine.set_eq(eq_band(band, db));
    let settle = rate as usize / 10;
    let window = rate as usize / 5;
    let input = sine(freq, 0.25, rate, settle + window);
    let out = play(&src, &input, |n| fake.render_f32(n));
    let left: Vec<f64> = out[2 * settle..]
        .iter()
        .step_by(2)
        .map(|&x| f64::from(x))
        .collect();
    let (amp, _) = common::measure::fit_tones(&left, &[freq], f64::from(rate));
    (
        20.0 * (amp[0] / 0.25).log10(),
        f64::from(engine.status().headroom_db),
    )
}

/// Every band, cut and boost, at every rate in `RATES`, at the band's frequency and an octave
/// either side (kept below 0.4 of the rate): the engine's gain against the analytic RBJ
/// magnitude. A cut takes no headroom, so its level matches absolutely; a boost's level is
/// lowered by the headroom the engine reports, and its shape matches relative to a probe at
/// 1 kHz. The largest error seen is printed (`--nocapture`).
#[test]
fn d1_eq_response_matches_analytic_rbj() {
    let mut worst = (0.0f64, String::new());
    for rate in RATES {
        let analytic = |band: usize, db: f32, f: f64| {
            rbj_db(kind(band), BAND_HZ[band], f64::from(db), rate, f)
        };
        for (band, &f0) in BAND_HZ.iter().enumerate() {
            let mut probes = vec![f0 / 2.0, f0, (2.0 * f0).min(0.4 * f64::from(rate))];
            probes.dedup();
            // Cut: no headroom is taken.
            for &f in &probes {
                let (m, headroom) = measure(rate, band, -6.0, f);
                // The response maximum is evaluated in f64: a cut's "0 dB" is rounding-level
                // (measured up to 1.9e-15 dB at 176.4 kHz); bound 1e-12.
                assert!(
                    headroom.abs() < 1e-12,
                    "{rate} Hz: band {band} cut takes no headroom: {headroom}"
                );
                let err = (m - analytic(band, -6.0, f)).abs();
                if err > worst.0 {
                    worst = (err, format!("{rate} Hz band {band} cut at {f} Hz"));
                }
                assert!(
                    err < EQ_TOL_DB,
                    "{rate} Hz, band {band} cut at {f} Hz: {m} vs {}",
                    analytic(band, -6.0, f)
                );
            }
            // Boost: shape relative to a far probe, level lowered by the headroom.
            let (m_far, _) = measure(rate, band, 6.0, 1000.0);
            for &f in &probes {
                let (m, headroom) = measure(rate, band, 6.0, f);
                let rel = (m - m_far) - (analytic(band, 6.0, f) - analytic(band, 6.0, 1000.0));
                assert!(
                    headroom > 5.0,
                    "{rate} Hz, band {band}: headroom {headroom}"
                );
                let abs = m - (analytic(band, 6.0, f) - headroom);
                let err = rel.abs().max(abs.abs());
                if err > worst.0 {
                    worst = (err, format!("{rate} Hz band {band} boost at {f} Hz"));
                }
                assert!(
                    rel.abs() < EQ_TOL_DB,
                    "{rate} Hz, band {band} at {f} Hz: rel {rel}"
                );
                assert!(
                    abs.abs() < EQ_TOL_DB,
                    "{rate} Hz, band {band} at {f} Hz: abs {abs}"
                );
            }
        }
    }
    eprintln!("d1: largest error {:.3e} dB ({})", worst.0, worst.1);
}

#[test]
fn d2_headroom_cancels_a_boost_at_its_centre() {
    let rate = 44_100;
    let (mut engine, src, fake) = f32_rig(rate);
    engine.set_eq(eq_band(4, 12.0));
    let frames = rate as usize / 2;
    let input = sine(1000.0, 1.0, rate, frames);
    let out = play(&src, &input, |n| fake.render_f32(n));
    assert!(out.iter().all(|x| x.abs() <= 1.0));
    let range = 2 * (rate as usize / 10)..2 * frames;
    let rin = rms(input[range.clone()].iter().copied());
    let rout = rms(out[range].iter().map(|&x| f64::from(x)));
    let db = 20.0 * (rout / rin).log10();
    let headroom = engine.status().headroom_db;
    // measured 1.3e-8 dB (2026-11-14, 1.92 x86_64; f32 output rounding); bound 1e-7 dB.
    assert!(db.abs() < 1e-7, "{db}");
    // measured 0 (a +12 dB peak at its centre frequency is exact in f32); bound 1e-4 dB.
    assert!((headroom - 12.0).abs() < 1e-4, "{headroom}");
    assert_eq!(engine.status().clips, 0);
}

#[test]
fn d3_limiter_holds_the_ceiling() {
    let rate = 44_100;
    let (mut engine, src, fake) = f32_rig(rate);
    engine.set_eq(eq_band(4, 3.0));
    let input = sine(1000.0, 1.25, rate, rate as usize / 2);
    let out = play(&src, &input, |n| fake.render_f32(n));
    assert!(out.iter().all(|x| x.abs() <= 1.0));
    let s = engine.status();
    assert!(s.limiter_frames > 0);
    assert!(s.applied.limiter);
    assert_eq!(s.clips, 0);
}

/// 16-bit-valued stereo frames covering both extremes (as T1).
fn frames16(n: usize) -> Vec<[i32; 2]> {
    (0..n as i64)
        .map(|i| {
            let o = (i * 13) % 65_536;
            [(-32_768 + o) as i32, (32_767 - o) as i32]
        })
        .collect()
}

fn to_samples(frames: &[[i32; 2]]) -> Vec<f64> {
    frames
        .iter()
        .flat_map(|f| [f64::from(f[0]) / 32_768.0, f64::from(f[1]) / 32_768.0])
        .collect()
}

fn to_frames(out: &[i16]) -> Vec<[i32; 2]> {
    out.chunks_exact(2)
        .map(|f| [i32::from(f[0]), i32::from(f[1])])
        .collect()
}

#[test]
fn d4_bypassed_stages_stay_bit_exact() {
    let (mut engine, src, fake) = rig(
        device(44_100, SampleFormat::I16, 16),
        OutputMode::Exclusive,
        Some(16),
    );
    engine.set_eq(EqSettings {
        preamp_db: 3.0,
        ..EqSettings::default()
    });
    let render = |n| fake.render_i16(n);

    // Flat EQ with a positive preamp: nothing to compensate, so bypassed.
    let a = frames16(10_000);
    let out = play(&src, &to_samples(&a), render);
    assert_eq!(to_frames(&out), a);
    assert!(engine.status().applied.exact);

    // Volume 99 changes the output.
    engine.set_volume(99);
    let b = frames16(6_000);
    let out = play(&src, &to_samples(&b), render);
    assert_ne!(to_frames(&out), b);
    assert!(!engine.status().applied.exact);

    // Back to 100: the callback that lands the ramp is processed, the next ones are exact.
    engine.set_volume(100);
    let c = frames16(10_000);
    let out = play(&src, &to_samples(&c), render);
    assert_eq!(to_frames(&out)[CHUNK..], c[CHUNK..]);
    let s = engine.status();
    assert!(s.applied.exact && !s.applied.volume && !s.applied.limiter && !s.applied.dither);
    assert_eq!(s.clips, 0);
}

#[test]
fn d5_volume_ramp_is_linear_and_lands() {
    let rate = 44_100;
    let (mut engine, src, fake) = f32_rig(rate);
    let c = 0.5;
    let render = |n| fake.render_f32(n);
    let before = play(&src, &vec![c; 2 * 1_000], render);
    assert!(before.iter().all(|&x| x == c as f32));

    engine.set_volume(50);
    let out = play(&src, &vec![c; 2 * 1_000], render);
    let gains: Vec<f64> = out.chunks_exact(2).map(|f| f64::from(f[0]) / c).collect();
    let ramp = 441;
    // The ramp is linear from 1.0 to 0.25 over 441 frames: 0.75/441 per frame. The F32 fake
    // rounds every output to f32: allow its quantum on top (measured largest step 0.00170070,
    // 1.9e-8 over the rule on 2026-11-14, 1.92 x86_64); bound rule + 2^-24 + 1e-12.
    let tol = 0.75 / 441.0 + 1e-12 + 2f64.powi(-24);
    let mut prev = 1.0;
    for (i, &g) in gains.iter().enumerate().take(ramp) {
        assert!(g <= prev, "frame {i}: {g} after {prev}");
        assert!(prev - g <= tol, "frame {i}: step {}", prev - g);
        prev = g;
    }
    assert!(gains[0] < 1.0);
    assert!(gains[ramp - 2] > 0.25);
    assert!(
        out[2 * (ramp - 1)..]
            .iter()
            .all(|&x| x == (c * 0.25) as f32)
    );
    assert!(engine.status().applied.volume);
}

#[test]
fn d6_dither_on_integers_only() {
    let rate = 48_000;
    let frames = 48_000;
    let skip = rate as usize / 50; // 20 ms: the ramp from the initial unity gain
    let input = vec![0.3; 2 * frames];
    let want = 0.3 * 0.25 * 32_768.0;

    let (mut engine, src, fake) = rig(
        device(rate, SampleFormat::I16, 16),
        OutputMode::Exclusive,
        None,
    );
    engine.set_volume(50);
    let out = play(&src, &input, |n| fake.render_i16(n));
    let tail: Vec<f64> = out[2 * skip..].iter().map(|&x| f64::from(x)).collect();
    // Exact rule: rounding (+-0.5 LSB) plus TPDF dither (|d| < 1 LSB) stays within 1.5 LSB.
    assert!(
        tail.iter().all(|x| (x - want).abs() <= 1.5),
        "out of ±1.5 LSB"
    );
    let mut distinct: Vec<i16> = out[2 * skip..].to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(distinct.len() >= 2, "{distinct:?}");
    let mean = tail.iter().sum::<f64>() / tail.len() as f64;
    // measured 5.1e-4 LSB (2026-11-14, 1.92 x86_64; fixed-seed dither); bound 4 sigma of the
    // mean: the error std is 0.5 LSB over 9.4e4 samples, sigma 1.6e-3, so 0.007.
    assert!((mean - want).abs() < 0.007, "mean {mean} vs {want}");
    assert!(engine.status().applied.dither);

    let (mut engine, src, fake) = rig(
        device(rate, SampleFormat::F32, 32),
        OutputMode::Exclusive,
        None,
    );
    engine.set_volume(50);
    let out = play(&src, &input, |n| fake.render_f32(n));
    assert!(out[2 * skip..].iter().all(|&x| x == (0.3f64 * 0.25) as f32));
    assert!(!engine.status().applied.dither);
}
