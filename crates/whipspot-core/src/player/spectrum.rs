//! Visualiser data from the engine's PCM tap. Read by front-ends at frame rate; never blocks.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::engine::PcmTap;

/// Number of spectrum bands in a [`SpectrumFrame`].
pub const SPECTRUM_BANDS: usize = 32;
/// Number of waveform samples in a [`SpectrumFrame`].
pub const WAVEFORM_LEN: usize = 512;
/// FFT window length in samples (about 46 ms at 44.1 kHz).
pub const FFT_LEN: usize = 2048;
/// The tap rate [`Spectrum::new`] plans for.
const DEFAULT_RATE: u32 = 44_100;
/// Lowest and highest band edge, Hz (the bands are log-spaced between them).
const F_LOW: f64 = 40.0;
const F_HIGH: f64 = 16_000.0;
/// Level floor in dBFS: a magnitude at this dB reads 0, full scale reads 1.
const FLOOR_DB: f32 = -70.0;
/// No tap packet for this long counts as silence.
const STALE: Duration = Duration::from_millis(250);

/// One visualiser frame. `bands` (0..=1) are 0 when inactive; `waveform` holds the latest mono
/// samples in -1..=1, oldest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpectrumFrame {
    pub bands: Vec<f32>,
    pub waveform: Vec<f32>,
    /// False when not playing or no tap data arrived recently; then everything is 0.
    pub active: bool,
}

/// Bin range of one band in the FFT output, plus the bin used when the range is empty.
#[derive(Debug, Clone, Copy)]
struct BandPlan {
    lo: usize,
    hi: usize,
    fallback: usize,
}

/// Hann-windowed real FFT over the last [`FFT_LEN`] samples, mapped to log-spaced bands.
/// Plan, window and buffers are made once in [`Spectrum::new`] (44.1 kHz) or
/// [`Spectrum::with_rate`].
pub struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// `2 / sum(window)`: a full-scale sine in a bin centre reads 1.0.
    scale: f32,
    plan: Vec<BandPlan>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Spectrum {
    /// A spectrum for a 44.1 kHz tap.
    pub fn new() -> Self {
        Self::with_rate(DEFAULT_RATE)
    }

    /// A spectrum for a tap at `rate` Hz (the band edges stay in Hz).
    pub fn with_rate(rate: u32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_LEN);
        let window: Vec<f32> = (0..FFT_LEN)
            .map(|n| {
                let phase = 2.0 * std::f64::consts::PI * n as f64 / FFT_LEN as f64;
                (0.5 - 0.5 * phase.cos()) as f32
            })
            .collect();
        let sum: f32 = window.iter().sum();
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        Spectrum {
            fft,
            window,
            scale: 2.0 / sum,
            plan: band_plan(f64::from(rate.max(1))),
            input,
            output,
            scratch,
        }
    }

    /// Analyse the last [`FFT_LEN`] samples of `samples` (zero-padded at the front when there
    /// are fewer) and write one level per band into `bands` (0..=1). Callers pass
    /// [`SPECTRUM_BANDS`] values; entries past the planned bands are written as 0.
    pub fn analyze(&mut self, samples: &[f32], bands: &mut [f32]) {
        let n = samples.len().min(FFT_LEN);
        let src = &samples[samples.len() - n..];
        let pad = FFT_LEN - n;
        self.input[..pad].fill(0.0);
        for ((x, &s), &w) in self.input[pad..]
            .iter_mut()
            .zip(src)
            .zip(&self.window[pad..])
        {
            *x = s * w;
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
            .is_err()
        {
            bands.fill(0.0);
            return;
        }
        for (i, out) in bands.iter_mut().enumerate() {
            *out = match self.plan.get(i) {
                Some(p) => self.level(*p),
                None => 0.0,
            };
        }
    }

    /// Largest magnitude in the band's bins (or its fallback bin), as a 0..=1 level.
    fn level(&self, p: BandPlan) -> f32 {
        let mag = if p.lo < p.hi {
            self.output[p.lo..p.hi]
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max)
        } else {
            self.output[p.fallback].norm()
        };
        let db = 20.0 * (mag * self.scale).max(1e-9).log10();
        ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

/// Bin ranges for [`SPECTRUM_BANDS`] log-spaced bands at `rate` Hz. Bin `k` has centre
/// `k * rate / FFT_LEN`; band `i` takes bins with centre in `[edge_i, edge_i+1)`, with edges
/// `40 * 400^(i/32)` Hz.
fn band_plan(rate: f64) -> Vec<BandPlan> {
    let edge = |i: usize| F_LOW * (F_HIGH / F_LOW).powf(i as f64 / SPECTRUM_BANDS as f64);
    let bins_per_hz = FFT_LEN as f64 / rate;
    let max_bin = FFT_LEN / 2;
    (0..SPECTRUM_BANDS)
        .map(|i| {
            let (a, b) = (edge(i), edge(i + 1));
            let lo = ((a * bins_per_hz).ceil() as usize).min(max_bin + 1);
            let hi = ((b * bins_per_hz).ceil() as usize).min(max_bin + 1);
            let centre = (a * b).sqrt();
            let fallback = ((centre * bins_per_hz).round() as usize).clamp(1, max_bin);
            BandPlan { lo, hi, fallback }
        })
        .collect()
}

pub(crate) struct TapReader {
    tap: PcmTap,
    /// Last `FFT_LEN` mono samples as f32 in -1..=1, oldest first.
    ring: VecDeque<f32>,
    /// The spectrum and the tap rate it was planned for.
    spectrum: (u32, Spectrum),
    last_packet: Option<Instant>,
}

impl TapReader {
    pub(crate) fn new(tap: PcmTap) -> Self {
        let rate = tap.rate();
        TapReader {
            tap,
            ring: VecDeque::with_capacity(FFT_LEN),
            spectrum: (rate, Spectrum::with_rate(rate)),
            last_packet: None,
        }
    }

    fn drain(&mut self, now: Instant) {
        let (ring, last_packet) = (&mut self.ring, &mut self.last_packet);
        self.tap.drain(|packet| {
            *last_packet = Some(now);
            let keep = packet.len().min(FFT_LEN);
            let tail = &packet[packet.len() - keep..];
            let overflow = (ring.len() + keep).saturating_sub(FFT_LEN);
            ring.drain(..overflow);
            ring.extend(tail.iter().copied());
        });
    }

    /// The spectrum for the tap's current rate, rebuilt (with the old samples dropped) when the
    /// rate changed.
    fn spectrum(&mut self) -> &mut Spectrum {
        let rate = self.tap.rate();
        if rate != self.spectrum.0 {
            self.spectrum = (rate, Spectrum::with_rate(rate));
            self.ring.clear();
        }
        &mut self.spectrum.1
    }

    /// Fill `out`; `playing` is the player's play state.
    pub(crate) fn fill(&mut self, playing: bool, out: &mut SpectrumFrame) {
        let now = Instant::now();
        // The rate first: samples queued at a new rate must not be analysed at the old one.
        self.spectrum();
        self.drain(now);
        out.bands.clear();
        out.bands.resize(SPECTRUM_BANDS, 0.0);
        out.waveform.clear();
        out.waveform.resize(WAVEFORM_LEN, 0.0);
        let fresh = self
            .last_packet
            .is_some_and(|t| now.saturating_duration_since(t) <= STALE);
        out.active = playing && fresh;
        if out.active {
            let len = self.ring.len();
            let shown = len.min(WAVEFORM_LEN);
            let start = WAVEFORM_LEN - shown;
            for (o, &s) in out.waveform[start..]
                .iter_mut()
                .zip(self.ring.iter().skip(len - shown))
            {
                *o = s;
            }
            let samples = self.ring.make_contiguous();
            self.spectrum.1.analyze(samples, &mut out.bands);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, EngineConfig};

    const RATE: f64 = 44_100.0;

    fn sine_at(rate: f64, freq: f64, amp: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin()) as f32)
            .collect()
    }

    fn sine(freq: f64, amp: f64, n: usize) -> Vec<f32> {
        sine_at(RATE, freq, amp, n)
    }

    /// Index of the band whose edges contain `freq` (log-spaced, see `band_plan`).
    fn band_of(freq: f64) -> usize {
        (SPECTRUM_BANDS as f64 * (freq / F_LOW).ln() / (F_HIGH / F_LOW).ln()).floor() as usize
    }

    fn argmax(bands: &[f32]) -> usize {
        bands
            .iter()
            .enumerate()
            .fold(
                (0, f32::MIN),
                |best, (i, &v)| if v > best.1 { (i, v) } else { best },
            )
            .0
    }

    #[test]
    fn spectrum_sine_lands_in_its_band_at_its_level() {
        let mut s = Spectrum::new();
        let mut bands = vec![0.0f32; SPECTRUM_BANDS];
        s.analyze(&sine(1000.0, 0.5, FFT_LEN), &mut bands);
        let target = band_of(1000.0);
        assert_eq!(target, 17);
        assert_eq!(argmax(&bands), target, "largest band must hold 1 kHz");
        let expected = (-6.0206 + 70.0) / 70.0;
        assert!(
            (bands[target] - expected).abs() < 0.03,
            "level {} vs expected {expected}",
            bands[target]
        );
        for (i, &v) in bands.iter().enumerate() {
            if i.abs_diff(target) >= 3 {
                assert!(v < 0.5 * bands[target], "band {i} leaks: {v}");
            }
        }
    }

    #[test]
    fn spectrum_at_48_khz_puts_1khz_in_band_17() {
        let mut s = Spectrum::with_rate(48_000);
        let mut bands = vec![0.0f32; SPECTRUM_BANDS];
        s.analyze(&sine_at(48_000.0, 1000.0, 0.5, FFT_LEN), &mut bands);
        assert_eq!(argmax(&bands), 17);
        // The 44.1 kHz plan would put the same samples elsewhere.
        let mut wrong = Spectrum::new();
        wrong.analyze(&sine_at(48_000.0, 1000.0, 0.5, FFT_LEN), &mut bands);
        assert_ne!(argmax(&bands), 17);
    }

    #[test]
    fn spectrum_silence_is_all_zero() {
        let mut s = Spectrum::new();
        let mut bands = vec![1.0f32; SPECTRUM_BANDS];
        s.analyze(&vec![0.0f32; FFT_LEN], &mut bands);
        assert!(bands.iter().all(|&v| v == 0.0), "{bands:?}");
    }

    #[test]
    fn spectrum_is_deterministic() {
        let input = sine(440.0, 0.3, 3000);
        let mut s = Spectrum::new();
        let mut a = vec![0.0f32; SPECTRUM_BANDS];
        let mut b = vec![0.0f32; SPECTRUM_BANDS];
        s.analyze(&input, &mut a);
        s.analyze(&input, &mut b);
        assert_eq!(a, b);
        let mut fresh = Spectrum::new();
        fresh.analyze(&input, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn spectrum_through_the_engine_tap_follows_the_tap_rate() {
        let config = EngineConfig {
            external_tap_rate: 48_000,
            ..EngineConfig::default()
        };
        let parts = Engine::new(config, None);
        let mut writer = parts.source.take_tap().expect("tap writer");
        let mut stereo = Vec::with_capacity(4096 * 2);
        for s in sine_at(48_000.0, 1000.0, 0.5, 4096) {
            stereo.push(f64::from(s));
            stereo.push(f64::from(s));
        }
        writer.push_stereo(&stereo);
        let mut reader = TapReader::new(parts.tap);
        let mut frame = SpectrumFrame::default();
        reader.fill(true, &mut frame);
        assert!(frame.active);
        assert_eq!(argmax(&frame.bands), band_of(1000.0));
        assert_eq!(frame.bands.len(), SPECTRUM_BANDS);
        assert_eq!(frame.waveform.len(), WAVEFORM_LEN);
        // Not playing: silence whatever the tap holds.
        reader.fill(false, &mut frame);
        assert!(!frame.active);
        assert!(frame.bands.iter().all(|&v| v == 0.0));
    }
}
