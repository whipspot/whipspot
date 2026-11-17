//! Audio measurements through the public engine (M10 step 7; H3, H4): the resampler's
//! passband ripple, image/alias rejection, THD+N and intermodulation, and the dither error's
//! spectrum. Every number is measured on the engine's own output (a `FakeOutput`); the
//! instruments are checked on known inputs first (`i1`–`i3`). Run with `-- --nocapture` to
//! see the measured values.

mod common;

use common::measure::*;
use common::{device, resample, tones, trim};
use whipspot_core::engine::{EqSettings, OutputMode, SampleFormat};

/// Rate pairs under test: 44.1 <-> 48 and 44.1 <-> 96 kHz.
const PAIRS: [(u32, u32); 4] = [
    (44_100, 48_000),
    (48_000, 44_100),
    (44_100, 96_000),
    (96_000, 44_100),
];

/// 1/3-octave centres `1000 * 10^(k/10)` Hz.
fn centre(k: i32) -> f64 {
    1000.0 * 10f64.powf(f64::from(k) / 10.0)
}

fn dbfs(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

// --- The instruments, on inputs whose answer is known ----------------------------------------

#[test]
fn i1_thd_n_reads_a_known_third_harmonic() {
    let fs = 48_000.0;
    let f = 997.0;
    let x: Vec<f64> = (0..48_000)
        .map(|n| {
            let t = n as f64 / fs;
            (2.0 * std::f64::consts::PI * f * t).sin()
                + 0.001 * (2.0 * std::f64::consts::PI * 3.0 * f * t).sin()
        })
        .collect();
    let thd = thd_n_db(&x, f, fs);
    eprintln!("i1: sine + -60 dB third harmonic reads {thd:.4} dB");
    assert!((thd + 60.0).abs() < 0.5, "{thd}");
}

#[test]
fn i2_the_spectrum_reads_white_noise_flat() {
    let fs = 48_000.0;
    let mut rng = SplitMix64(7);
    // Uniform on [-0.5, 0.5): variance 1/12.
    let x: Vec<f64> = (0..480_000).map(|_| rng.unit() - 0.5).collect();
    let nfft = 4_096;
    let bands = third_octave_db(&welch(&x, nfft), nfft, fs, 200.0, 20_000.0, 2);
    assert!(bands.len() >= 18, "{} bands", bands.len());
    let want = 10.0 * (1.0f64 / 12.0).log10();
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for &(_, l) in &bands {
        lo = lo.min(l);
        hi = hi.max(l);
    }
    eprintln!("i2: white noise bands {lo:.3} .. {hi:.3} dB (variance 1/12 = {want:.3} dB)");
    assert!(hi - want < 1.0 && want - lo < 1.0, "{lo} .. {hi} vs {want}");
}

#[test]
fn i3_the_windowed_tone_reads_an_off_bin_amplitude() {
    let fs = 48_000.0;
    // 1234.567 Hz is off every bin of a 32768-point window; amplitude 0.3.
    let x: Vec<f64> = (0..32_768)
        .map(|n| 0.3 * (2.0 * std::f64::consts::PI * 1234.567 * n as f64 / fs + 0.4).sin())
        .collect();
    let amp = tone_amp(&x, 1234.567, fs);
    let err = db(amp / 0.3);
    eprintln!("i3: off-bin tone reads {amp:.6} (error {err:.5} dB)");
    assert!(err.abs() < 0.1, "{err}");
    // And a tone far away reads nothing: Hann leakage 2 kHz off is far below -120 dB.
    let far = db(tone_amp(&x, 3_234.567, fs) / 0.3);
    eprintln!("i3: leakage 2 kHz away {far:.1} dB");
    assert!(far < -120.0, "{far}");
}

#[test]
fn i4_the_spur_scan_reads_a_known_line() {
    let fs = 48_000.0;
    // A 1234.5 Hz tone of amplitude 0.5 and a line of -100 dBc at 7777.7 Hz (both off-bin).
    let spur = 0.5 * dbfs(-100.0);
    let x: Vec<f64> = (0..65_536)
        .map(|n| {
            let t = n as f64 / fs;
            0.5 * (2.0 * std::f64::consts::PI * 1234.5 * t).sin()
                + spur * (2.0 * std::f64::consts::PI * 7777.7 * t + 1.0).sin()
        })
        .collect();
    let (d, f) = worst_spur_db(&x, 1234.5, 0.5, 200.0, fs, 32_768);
    eprintln!("i4: -100 dBc line reads {d:.3} dB at {f:.1} Hz");
    // A Hann scalloping loss of up to 1.42 dB applies to a line that is off every bin.
    assert!(
        (d + 100.0).abs() < 1.5 && (f - 7777.7).abs() < 2.0,
        "{d} dB at {f} Hz"
    );
}

// --- H3: the resampler -----------------------------------------------------------------------

/// A tone at `f` folded into 0..=rate/2 as a signal sampled at `rate` shows it.
fn fold(f: f64, rate: u32) -> f64 {
    let r = f64::from(rate);
    let m = f % r;
    if m > r / 2.0 { r - m } else { m }
}

/// Gain (dB) of each tone of a 1/3-octave multitone (equal amplitudes, each -34 dBFS so the
/// sum stays below full scale) through the engine: the Hann-windowed DFT at each frequency.
fn gains(src: u32, dev: u32, freqs: &[f64]) -> Vec<f64> {
    let amp = 0.02;
    let spec: Vec<(f64, f64)> = freqs.iter().map(|&f| (f, amp)).collect();
    let out = resample(src, dev, &tones(&spec, src, 2 * src as usize));
    let x = trim(&out, dev, 100);
    freqs
        .iter()
        .map(|&f| db(tone_amp(x, f, f64::from(dev)) / amp))
        .collect()
}

/// Passband gain, 1/3-octave tones from 20 Hz up. Floor: within +-0.1 dB up to 20 kHz for
/// 44.1 -> 48 (the contract) and, as measured, for every pair up to 16 kHz; 96 -> 44.1 droops
/// at 20 kHz (its sinc has fewer taps per output Hz), bounded by its measured value.
#[test]
fn r1_passband_ripple() {
    for (src, dev) in PAIRS {
        let freqs: Vec<f64> = (-17..=13).map(centre).collect(); // 20 Hz .. 19.95 kHz
        let g = gains(src, dev, &freqs);
        let max_abs = |upto: f64| {
            freqs
                .iter()
                .zip(&g)
                .filter(|(f, _)| **f <= upto)
                .fold(0.0f64, |m, (_, x)| m.max(x.abs()))
        };
        let (m16, m20) = (max_abs(16_000.0), max_abs(20_000.0));
        eprintln!("r1: {src} -> {dev}: max |gain| {m16:.5} dB to 16 kHz, {m20:.5} dB to 19.95 kHz");
        assert!(m16 <= 0.1, "{src} -> {dev}: {m16} dB to 16 kHz");
        // 96 -> 44.1 at 19.95 kHz: measured -0.464 dB; bound about 4x.
        let bound = if (src, dev) == (96_000, 44_100) {
            2.0
        } else {
            0.1
        };
        assert!(m20 <= bound, "{src} -> {dev}: {m20} dB to 19.95 kHz");
    }
}

/// Image (upsampling) and alias (downsampling) rejection of one tone through the engine: the
/// level at the tone's alias line (downsampling) and the worst other line anywhere in the
/// output spectrum (images, aliases, harmonics), both re the tone. Floor 90 dB. Tones sit at
/// least 2 kHz past the band edge where the pair allows it (48 -> 44.1 has 1.95 kHz of room
/// above the output Nyquist). The tones are off round numbers so that quantisation noise does
/// not line up on single lines.
#[test]
fn r2_image_and_alias_rejection() {
    let amp = 0.5;
    for (src, dev) in PAIRS {
        let (s, d) = (f64::from(src), f64::from(dev));
        let tones_hz: Vec<f64> = if dev > src {
            // The image of f lands at src - f: 2 kHz past the input Nyquist for f <= src/2 - 2 kHz.
            vec![4_999.37, 9_999.71, 14_999.13, s / 2.0 - 2_050.3]
        } else {
            let (lo, hi) = (d / 2.0 + 2_000.3, s / 2.0 - 99.7);
            if lo < hi {
                vec![lo, (lo + hi) / 2.0, hi]
            } else {
                // 48 -> 44.1: 1.95 kHz of room.
                vec![s / 2.0 - 99.7, s / 2.0 - 500.3, s / 2.0 - 1_000.7]
            }
        };
        for f in tones_hz {
            let input = tones(&[(f, amp)], src, src as usize + src as usize / 2);
            let out = resample(src, dev, &input);
            let x = trim(&out, dev, 100);
            let kept = fold(f, dev);
            let (spur, at) = worst_spur_db(x, kept, amp, 200.0, d, 32_768);
            // Upsampling: the tone survives at f, every other line is an image. Downsampling:
            // the tone is gone and `fold(f)` is where its alias would be.
            let rejection = if dev < src {
                -db(tone_amp(x, kept, d) / amp)
            } else {
                -spur
            };
            eprintln!(
                "r2: {src} -> {dev}: {f:.0} Hz in: rejection {rejection:.1} dB at line {kept:.0} Hz (worst other line {spur:.1} dB at {at:.0} Hz)"
            );
            assert!(rejection >= 90.0, "{src} -> {dev}, {f} Hz: {rejection} dB");
            assert!(
                -spur >= 90.0,
                "{src} -> {dev}, {f} Hz: spur {spur} dB at {at} Hz"
            );
        }
    }
}

/// THD+N of a 997 Hz tone at -3 dBFS and of a stepped 1/3-octave sweep to 16 kHz, through
/// each pair. Floors -100 dB and -90 dB.
#[test]
fn r3_thd_n_and_sweep() {
    let a = dbfs(-3.0);
    for (src, dev) in PAIRS {
        let input = tones(&[(997.0, a)], src, src as usize);
        let out = resample(src, dev, &input);
        let thd = thd_n_db(trim(&out, dev, 100), 997.0, f64::from(dev));
        let mut worst = (f64::MIN, 0.0);
        for k in -17..=12 {
            let f = centre(k);
            let input = tones(&[(f, a)], src, src as usize / 2 + src as usize / 4);
            let out = resample(src, dev, &input);
            let t = thd_n_db(trim(&out, dev, 100), f, f64::from(dev));
            if t > worst.0 {
                worst = (t, f);
            }
        }
        eprintln!(
            "r3: {src} -> {dev}: 997 Hz THD+N {thd:.1} dB; sweep worst {:.1} dB at {:.0} Hz",
            worst.0, worst.1
        );
        assert!(thd <= -100.0, "{src} -> {dev}: 997 Hz {thd} dB");
        assert!(
            worst.0 <= -90.0,
            "{src} -> {dev}: sweep {} dB at {} Hz",
            worst.0,
            worst.1
        );
    }
}

/// Three tones (-14 dBFS each) at 3.1, 7.7 and 12.9 kHz: everything that is not those tones
/// (all intermodulation products and noise) re one tone's level. Floor -100 dB.
#[test]
fn r4_multitone_intermodulation() {
    let amp = 0.2;
    let freqs = [3_100.0, 7_700.0, 12_900.0];
    let spec: Vec<(f64, f64)> = freqs.iter().map(|&f| (f, amp)).collect();
    for (src, dev) in PAIRS {
        let out = resample(src, dev, &tones(&spec, src, src as usize));
        let (_, resid) = fit_tones(trim(&out, dev, 100), &freqs, f64::from(dev));
        let imd = db(resid / (amp / 2f64.sqrt()));
        eprintln!("r4: {src} -> {dev}: residual re one tone {imd:.1} dB");
        assert!(imd <= -100.0, "{src} -> {dev}: {imd} dB");
    }
}

// --- H4: the dither's error through the engine -----------------------------------------------

/// Active DSP (volume 50 and a mid-band boost, so nothing is on the exact path) into an
/// `I16` fake and into an `F32` fake: the difference is the quantiser's error (the DSP is the
/// same in both). Its Welch spectrum is flat within +-1 dB per 1/3-octave band from 200 Hz to
/// 20 kHz, and its total variance is 1/4 LSB^2 (rounding 1/12 plus TPDF dither 1/6).
#[test]
fn q1_dither_error_spectrum_through_the_engine() {
    let rate = 48_000u32;
    let frames = 10 * rate as usize;
    let lsb = 1.0 / 32_768.0;
    let input = tones(&[(997.0, 0.25)], rate, frames);
    let play = |sample: SampleFormat, bits: u8| -> Vec<f64> {
        let (mut engine, src, fake) =
            common::rig(rate, device(rate, sample, bits), OutputMode::Exclusive);
        engine.set_volume(50);
        let mut eq = EqSettings::default();
        eq.bands_db[4] = 3.0;
        engine.set_eq(eq);
        let mut out = Vec::with_capacity(frames);
        for chunk in input.chunks(2 * 2_000) {
            src.write(chunk).unwrap();
            let n = chunk.len() / 2;
            match sample {
                SampleFormat::I16 => out.extend(
                    fake.render_i16(n)
                        .chunks_exact(2)
                        .map(|f| f64::from(f[0]) * lsb),
                ),
                _ => out.extend(fake.render_f32(n).chunks_exact(2).map(|f| f64::from(f[0]))),
            }
        }
        assert!(engine.status().applied.dither == (sample == SampleFormat::I16));
        out
    };
    let int = play(SampleFormat::I16, 16);
    let float = play(SampleFormat::F32, 32);
    let skip = rate as usize / 10;
    let err: Vec<f64> = int[skip..]
        .iter()
        .zip(&float[skip..])
        .map(|(a, b)| (a - b) / lsb)
        .collect();
    let var = err.iter().map(|e| e * e).sum::<f64>() / err.len() as f64;
    let nfft = 4_096;
    let bands = third_octave_db(
        &welch(&err, nfft),
        nfft,
        f64::from(rate),
        200.0,
        20_000.0,
        2,
    );
    let mean = bands.iter().map(|b| b.1).sum::<f64>() / bands.len() as f64;
    let (lo, hi) = bands
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), b| (l.min(b.1), h.max(b.1)));
    eprintln!(
        "q1: error variance {var:.5} LSB^2 (rel err {:.2e}); {} bands {lo:.3} .. {hi:.3} dB, mean {mean:.3} dB (1/4 = -6.021 dB)",
        (var * 4.0 - 1.0).abs(),
        bands.len()
    );
    // Measured 2.1e-3 (sampling sigma 1.2e-3 at 4.8e5 draws: 4 sigma is 5e-3).
    assert!((var * 4.0 - 1.0).abs() < 0.005, "variance {var}");
    assert!(bands.len() >= 18);
    assert!(
        hi - mean < 1.0 && mean - lo < 1.0,
        "{lo} .. {hi}, mean {mean}"
    );
    // Measured 0.002 dB off; the band-mean sigma is about 0.02 dB.
    assert!((mean + 6.021).abs() < 0.1, "mean {mean} dB");
}
