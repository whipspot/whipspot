//! Device-format conversion (MD27): round to the nearest device code, clamp and count clips
//! (I9). Off the exact path, integer formats add TPDF dither before rounding; float formats
//! never dither.
//!
//! Channel map: device channel 0 = L, 1 = R, others 0; a mono device gets `(L+R)/2`.

use super::format::{DeviceFormat, SampleFormat, StreamFormat};
use super::output::OutBuf;

/// Whether the device format holds the source's samples without loss of resolution (MD27):
/// lossy decoder output always; an integer source of b bits when b is at most the device's
/// valid bits, or at most 24 for a float device.
pub(crate) fn holds_source(source: &StreamFormat, device: &DeviceFormat) -> bool {
    match source.source.bits {
        None => true,
        Some(b) => match device.sample {
            SampleFormat::F32 => b <= 24,
            SampleFormat::I16 | SampleFormat::I32 => b <= device.valid_bits,
        },
    }
}

#[inline]
fn frame_sample(frame: [f64; 2], ch: usize, channels: usize) -> f64 {
    if channels == 1 {
        (frame[0] + frame[1]) * 0.5
    } else {
        match ch {
            0 => frame[0],
            1 => frame[1],
            _ => 0.0,
        }
    }
}

#[inline]
fn to_f32(x: f64, clips: &mut u64) -> f32 {
    if !(-1.0..=1.0).contains(&x) {
        *clips += 1;
    }
    x.clamp(-1.0, 1.0) as f32
}

/// TPDF dither source: the difference of two uniforms in [−0.5, 0.5) LSB, from one xorshift64*
/// generator per renderer with a fixed seed.
pub(crate) struct Tpdf {
    state: u64,
}

impl Default for Tpdf {
    fn default() -> Self {
        Tpdf {
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

impl Tpdf {
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [−0.5, 0.5).
    #[inline]
    fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64) - 0.5
    }

    /// Triangular in (−1, 1) LSB.
    #[inline]
    fn sample(&mut self) -> f64 {
        self.uniform() - self.uniform()
    }
}

/// Round `x * 2^(bits-1)` (plus dither) and clamp it to a signed `bits`-bit integer.
#[inline]
fn to_int(x: f64, bits: u8, clips: &mut u64, dither: Option<&mut Tpdf>) -> i64 {
    let scale = (1i64 << (bits - 1)) as f64;
    let max = (1i64 << (bits - 1)) - 1;
    let min = -(1i64 << (bits - 1));
    let mut v = x * scale;
    if let Some(d) = dither {
        v += d.sample();
    }
    let r = v.round();
    if r > max as f64 {
        *clips += 1;
        max
    } else if r < min as f64 {
        *clips += 1;
        min
    } else {
        r as i64
    }
}

/// Write `frames` (stereo) into `out` starting at frame `offset`, in the device format, with
/// TPDF dither from `dither` for integer formats (`None`: the exact path, rounding only).
/// `out` must hold at least `(offset + frames.len()) * channels` samples.
pub(crate) fn quantise(
    frames: &[[f64; 2]],
    out: &mut OutBuf<'_>,
    offset: usize,
    device: &DeviceFormat,
    clips: &mut u64,
    mut dither: Option<&mut Tpdf>,
) {
    let channels = device.channels as usize;
    let start = offset * channels;
    let end = start + frames.len() * channels;
    match out {
        OutBuf::F32(buf) => {
            for (frame, dst) in frames
                .iter()
                .zip(buf[start..end].chunks_exact_mut(channels))
            {
                for (ch, d) in dst.iter_mut().enumerate() {
                    *d = to_f32(frame_sample(*frame, ch, channels), clips);
                }
            }
        }
        OutBuf::I16(buf) => {
            for (frame, dst) in frames
                .iter()
                .zip(buf[start..end].chunks_exact_mut(channels))
            {
                for (ch, d) in dst.iter_mut().enumerate() {
                    *d = to_int(
                        frame_sample(*frame, ch, channels),
                        16,
                        clips,
                        dither.as_deref_mut(),
                    ) as i16;
                }
            }
        }
        OutBuf::I32(buf) => {
            let bits = device.valid_bits;
            let shift = 32 - u32::from(bits);
            for (frame, dst) in frames
                .iter()
                .zip(buf[start..end].chunks_exact_mut(channels))
            {
                for (ch, d) in dst.iter_mut().enumerate() {
                    *d = (to_int(
                        frame_sample(*frame, ch, channels),
                        bits,
                        clips,
                        dither.as_deref_mut(),
                    ) as i32)
                        << shift;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(sample: SampleFormat, valid_bits: u8, channels: u16) -> DeviceFormat {
        DeviceFormat {
            rate: 48_000,
            channels,
            sample,
            valid_bits,
        }
    }

    #[test]
    fn full_scale_clamps_and_counts() {
        let mut clips = 0;
        let mut out = [0i16; 4];
        quantise(
            &[[1.0, -1.0], [-1.5, 0.5]],
            &mut OutBuf::I16(&mut out),
            0,
            &dev(SampleFormat::I16, 16, 2),
            &mut clips,
            None,
        );
        assert_eq!(out, [32767, -32768, -32768, 16384]);
        assert_eq!(clips, 2);
    }

    #[test]
    fn i32_24_is_left_justified_and_extra_channels_are_zero() {
        let mut clips = 0;
        let mut out = [7i32; 3];
        quantise(
            &[[0.5, -1.0 / 8_388_608.0]],
            &mut OutBuf::I32(&mut out),
            0,
            &dev(SampleFormat::I32, 24, 3),
            &mut clips,
            None,
        );
        assert_eq!(out, [4_194_304 << 8, -1 << 8, 0]);
        assert_eq!(clips, 0);
    }

    #[test]
    fn tpdf_is_triangular_and_deterministic() {
        let (mut a, mut b) = (Tpdf::default(), Tpdf::default());
        let mut sum = 0.0;
        let n = 100_000;
        for _ in 0..n {
            let x = a.sample();
            assert_eq!(x, b.sample());
            assert!(x > -1.0 && x < 1.0);
            sum += x;
        }
        eprintln!("H6 tpdf mean: {}", (sum / f64::from(n)).abs());
        // measured 1.7e-3 (2026-11-14, 1.92 x86_64); bound 4 sigma of the mean: sigma = sqrt(1/6 / 1e5) = 1.3e-3,
        // so 0.005.
        assert!((sum / f64::from(n)).abs() < 0.005);
    }

    /// H4 (M10 step 7): the dither's distribution against the triangular pdf on (-1, 1) LSB.
    /// 10^6 draws of the fixed-seed generator, 20 bins of width 0.1; every bin within 4 sigma
    /// of its binomial expectation (the largest deviation measured is printed).
    #[test]
    fn tpdf_histogram_mean_and_variance_match_the_triangular_pdf() {
        let n = 1_000_000usize;
        let mut d = Tpdf::default();
        let mut counts = [0usize; 20];
        let (mut sum, mut sum2) = (0.0, 0.0);
        for _ in 0..n {
            let x = d.sample();
            counts[((x + 1.0) * 10.0) as usize] += 1;
            sum += x;
            sum2 += x * x;
        }
        // P(bin): the integral of 1 - |x| over [a, b], a and b on the same side of zero.
        let prob = |k: usize| {
            let (a, b) = (k as f64 * 0.1 - 1.0, (k + 1) as f64 * 0.1 - 1.0);
            let (a, b) = if b <= 0.0 { (-b, -a) } else { (a, b) };
            (b - a) - (b * b - a * a) / 2.0
        };
        let mut worst = 0.0f64;
        for (k, &c) in counts.iter().enumerate() {
            let (p, nf) = (prob(k), n as f64);
            let z = (c as f64 - nf * p) / (nf * p * (1.0 - p)).sqrt();
            worst = worst.max(z.abs());
            assert!(z.abs() < 4.0, "bin {k}: {c} vs {:.0} (z {z:.2})", nf * p);
        }
        let mean = sum / n as f64;
        let var = sum2 / n as f64 - mean * mean;
        // The mean's sigma is sqrt((1/6) / n); the variance's relative sigma is
        // sqrt((kurtosis - 1) / n) with the triangular kurtosis 2.4.
        let mean_sigma = (1.0 / 6.0 / n as f64).sqrt();
        let var_rel = (var * 6.0 - 1.0).abs();
        eprintln!(
            "tpdf: worst bin z {worst:.2}, mean {mean:.2e} ({:.2} sigma), variance {var:.6} (rel err {var_rel:.2e})",
            mean / mean_sigma
        );
        assert!(mean.abs() < 4.0 * mean_sigma, "mean {mean}");
        assert!(var_rel < 4.0 * (1.4 / n as f64).sqrt(), "variance {var}");
    }

    /// The error of the whole quantiser (rounding plus dither) in LSB: variance 1/12 + 1/6 =
    /// 1/4 and no correlation with a -90 dBFS sine (about 1 LSB peak) however it sits on the
    /// grid. Without dither the same sine correlates with its rounding error (the instrument
    /// sees a correlation when there is one).
    #[test]
    fn quantiser_error_is_a_quarter_lsb_squared_and_uncorrelated_with_the_signal() {
        let n = 1_000_000usize;
        let amp = 10f64.powf(-90.0 / 20.0);
        let scale = 32_768.0;
        let run = |dither: bool| {
            let mut d = Tpdf::default();
            let mut clips = 0;
            let (mut se, mut se2, mut ss2, mut sse) = (0.0, 0.0, 0.0, 0.0);
            for i in 0..n {
                // An offset off the grid and a slow sine: 1 kHz at 44.1 kHz.
                let s = amp * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 44_100.0).sin();
                let x = 0.123_456_7 + s;
                let q = to_int(x, 16, &mut clips, dither.then_some(&mut d)) as f64;
                let e = q - x * scale;
                let s = s * scale;
                se += e;
                se2 += e * e;
                ss2 += s * s;
                sse += e * s;
            }
            let nf = n as f64;
            let var = se2 / nf - (se / nf) * (se / nf);
            (var, sse / nf / ((se2 / nf).sqrt() * (ss2 / nf).sqrt()))
        };
        let (var, corr) = run(true);
        let (plain_var, plain_corr) = run(false);
        eprintln!(
            "quantiser: dithered variance {var:.5} (rel err {:.2e}), corr {corr:.2e}; undithered variance {plain_var:.5}, corr {plain_corr:.3}",
            (var * 4.0 - 1.0).abs()
        );
        // Measured 7.5e-4 and 7.6e-4; the sampling sigma at 10^6 draws is 1.2e-3 and 1.0e-3
        // (4 sigma: 4.7e-3 and 4e-3).
        assert!((var * 4.0 - 1.0).abs() < 0.005, "variance {var}");
        assert!(corr.abs() < 0.004, "correlation {corr}");
        assert!(plain_corr.abs() > 0.05, "instrument: {plain_corr}");
    }

    #[test]
    fn dithered_rounding_stays_in_range() {
        let mut clips = 0;
        let mut out = [0i16; 2000];
        let x = 1.0 - 1.5 / 32_768.0;
        let frames = vec![[x, -x]; 1000];
        let mut d = Tpdf::default();
        quantise(
            &frames,
            &mut OutBuf::I16(&mut out),
            0,
            &dev(SampleFormat::I16, 16, 2),
            &mut clips,
            Some(&mut d),
        );
        assert_eq!(clips, 0);
        assert!(
            out.chunks_exact(2)
                .all(|f| f[0] >= 32_765 && f[1] <= -32_765)
        );
    }

    #[test]
    fn mono_device_averages() {
        let mut clips = 0;
        let mut out = [0f32; 1];
        quantise(
            &[[0.5, 0.25]],
            &mut OutBuf::F32(&mut out),
            0,
            &dev(SampleFormat::F32, 32, 1),
            &mut clips,
            None,
        );
        assert_eq!(out, [0.375]);
    }
}
