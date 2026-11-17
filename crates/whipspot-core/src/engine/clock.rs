//! The output clock's drift against the wall clock (H18, MD138).
//!
//! A stream's first callbacks fill the device buffer at once (WASAPI asks for a full buffer
//! before it starts), so frames rendered from the first callback run ahead of the wall time by
//! about one buffer. Divided by a few seconds that offset reads as thousands of ppm. So the
//! clock skips a warm-up after the first callback and then fits a line through every
//! callback's (start time, frames rendered before it) pair: the slope is the device's real
//! rate, any constant offset falls out, and the callbacks' wake-up jitter averages down.
//!
//! [`DriftClock`] runs inside the render callback: `Copy` state, a few flops per callback, no
//! allocation.

use std::time::{Duration, Instant};

/// Time after the stream's first callback before points are fitted: the start-up fill is over
/// by then.
pub(crate) const WARMUP: Duration = Duration::from_millis(500);

/// Fitted time before the drift is shown: shorter spans are dominated by the jitter.
pub(crate) const MIN_SPAN_SECONDS: f64 = 2.0;

/// The running least-squares fit of frames rendered against seconds since the anchor
/// (Welford's update, stable for long sessions). Published with the engine status.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DriftFit {
    n: u64,
    span_s: f64,
    mean_x: f64,
    mean_y: f64,
    m2_x: f64,
    c_xy: f64,
}

impl DriftFit {
    fn add(&mut self, x: f64, y: f64) {
        self.n += 1;
        let n = self.n as f64;
        let dx = x - self.mean_x;
        self.mean_x += dx / n;
        self.mean_y += (y - self.mean_y) / n;
        self.c_xy += dx * (y - self.mean_y);
        self.m2_x += dx * (x - self.mean_x);
        self.span_s = x;
    }

    /// Seconds between the first and the last fitted callback.
    pub fn span_seconds(&self) -> f64 {
        self.span_s
    }

    /// The device's measured rate in frames per second; `None` with fewer than two points.
    pub fn frames_per_second(&self) -> Option<f64> {
        (self.n >= 2 && self.m2_x > 0.0).then(|| self.c_xy / self.m2_x)
    }

    /// The drift in ppm against `rate` (negative when the device runs slow), once the fit
    /// spans [`MIN_SPAN_SECONDS`].
    pub fn drift_ppm(&self, rate: u32) -> Option<f32> {
        if rate == 0 || self.span_s < MIN_SPAN_SECONDS {
            return None;
        }
        self.frames_per_second()
            .map(|fps| ((fps / f64::from(rate) - 1.0) * 1e6) as f32)
    }
}

/// The renderer's side: call [`DriftClock::tick`] at each callback's start and
/// [`DriftClock::rendered`] with its frame count at its end; [`DriftClock::reset`] at open.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DriftClock {
    warmup: Duration,
    first: Option<Instant>,
    /// The anchor callback's start and the frames rendered before it.
    anchor: Option<(Instant, u64)>,
    /// Frames rendered since the open, before the current callback.
    frames: u64,
    fit: DriftFit,
}

impl Default for DriftClock {
    fn default() -> Self {
        DriftClock::with_warmup(WARMUP)
    }
}

impl DriftClock {
    pub(crate) fn with_warmup(warmup: Duration) -> Self {
        DriftClock {
            warmup,
            first: None,
            anchor: None,
            frames: 0,
            fit: DriftFit::default(),
        }
    }

    pub(crate) fn tick(&mut self, t0: Instant) {
        let first = *self.first.get_or_insert(t0);
        if self.anchor.is_none() && t0.saturating_duration_since(first) >= self.warmup {
            self.anchor = Some((t0, self.frames));
        }
        if let Some((at, frames)) = self.anchor {
            let x = t0.saturating_duration_since(at).as_secs_f64();
            self.fit.add(x, (self.frames - frames) as f64);
        }
    }

    pub(crate) fn rendered(&mut self, frames: usize) {
        self.frames += frames as u64;
    }

    pub(crate) fn reset(&mut self) {
        *self = DriftClock::with_warmup(self.warmup);
    }

    pub(crate) fn fit(&self) -> DriftFit {
        self.fit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device at `rate * (1 + ppm)` with a `period`-frame callback whose first callback fills
    /// `prefill` extra frames, and a deterministic wake-up jitter of up to `jitter_us`.
    fn simulate(
        clock: &mut DriftClock,
        rate: u32,
        ppm: f64,
        period: usize,
        prefill: usize,
        jitter_us: u64,
        seconds: f64,
    ) -> (u64, Duration) {
        let base = Instant::now();
        let true_rate = f64::from(rate) * (1.0 + ppm * 1e-6);
        let callbacks = (seconds * true_rate / period as f64) as u64;
        let mut seed = 0x2545_f491_u64;
        let mut last = Duration::ZERO;
        let mut frames = 0u64;
        for k in 0..callbacks {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let jitter = Duration::from_micros(seed % (jitter_us + 1));
            let at = Duration::from_secs_f64(k as f64 * period as f64 / true_rate) + jitter;
            clock.tick(base + at);
            let n = if k == 0 { period + prefill } else { period };
            clock.rendered(n);
            frames += n as u64;
            last = at;
        }
        (frames, last)
    }

    #[test]
    fn the_start_up_fill_does_not_read_as_drift() {
        // 44.1 kHz shared: a 441-frame period, a full buffer prefilled, up to 1 ms of wake-up
        // jitter, a device 40 ppm fast, five seconds (a self-test run). The slope's standard
        // error is the jitter's (289 us for uniform 0..1 ms) times sqrt(12 / N) over the span:
        // about 9.5 ppm for N = 450 callbacks over 4.5 s; the bound is four of those.
        let mut clock = DriftClock::default();
        let (frames, last) = simulate(&mut clock, 44_100, 40.0, 441, 441, 1000, 5.0);
        let ppm = clock.fit().drift_ppm(44_100).unwrap();
        assert!((ppm - 40.0).abs() < 40.0, "{ppm}");
        // What the first-callback anchor read on the same callbacks (the bug): thousands.
        let naive = (frames as f64 / 44_100.0 - last.as_secs_f64()) / last.as_secs_f64() * 1e6;
        assert!(naive > 1000.0, "{naive}");
    }

    #[test]
    fn a_slow_device_reads_negative() {
        let mut clock = DriftClock::default();
        // Exclusive 96 kHz: up to 0.5 ms of jitter, a standard error of about 5 ppm.
        simulate(&mut clock, 96_000, -120.0, 960, 960, 500, 5.0);
        let ppm = clock.fit().drift_ppm(96_000).unwrap();
        assert!((ppm + 120.0).abs() < 20.0, "{ppm}");
    }

    #[test]
    fn nothing_is_shown_before_the_warm_up_and_two_seconds() {
        let mut clock = DriftClock::default();
        simulate(&mut clock, 48_000, 0.0, 480, 480, 0, 2.4);
        // 2.4 s minus the 0.5 s warm-up is under the two seconds.
        assert_eq!(clock.fit().drift_ppm(48_000), None);
        assert!(clock.fit().span_seconds() > 1.8);
        let mut clock = DriftClock::default();
        simulate(&mut clock, 48_000, 0.0, 480, 480, 0, 0.4);
        assert_eq!(clock.fit(), DriftFit::default());
        assert_eq!(clock.fit().drift_ppm(0), None);
    }

    #[test]
    fn reset_starts_over_with_its_warm_up() {
        let mut clock = DriftClock::with_warmup(Duration::ZERO);
        simulate(&mut clock, 48_000, 0.0, 480, 0, 0, 3.0);
        assert!(clock.fit().drift_ppm(48_000).is_some());
        clock.reset();
        assert_eq!(clock.fit(), DriftFit::default());
        assert_eq!(clock.warmup, Duration::ZERO);
    }
}
