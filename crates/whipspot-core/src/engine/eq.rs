//! The 10-band equaliser (MD28, ported from the copied `core::eq`): WebAudio's RBJ cookbook
//! biquads — a low shelf at band 0 and a high shelf at band 9 (slope S = 1), peaking bands 1–8
//! (Q = 1) — designed for the open device rate.
//!
//! The design ([`EqDesign`]) is computed on the control side and published to the renderer;
//! [`EqFilter`] holds the per-band, per-channel state on the renderer and never allocates.

use std::f64::consts::{PI, SQRT_2};

/// Number of EQ bands.
pub const BAND_COUNT: usize = 10;
/// Centre / corner frequencies (Hz); the first is a low shelf, the last a high shelf.
pub const BAND_HZ: [f64; BAND_COUNT] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
/// Largest boost or cut of the preamp and of each band (dB).
pub(crate) const MAX_DB: f32 = 12.0;

/// Equaliser settings in dB. A band is on iff its value is not exactly 0.0; a disabled EQ
/// bypasses its preamp too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; BAND_COUNT],
}

impl Default for EqSettings {
    fn default() -> Self {
        EqSettings {
            enabled: true,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        }
    }
}

impl EqSettings {
    /// `None` when any value is not finite (the settings are ignored as a whole); otherwise
    /// every dB value clamped to ±12.
    pub(crate) fn sanitised(&self) -> Option<Self> {
        if !self.preamp_db.is_finite() || !self.bands_db.iter().all(|b| b.is_finite()) {
            return None;
        }
        let clamp = |v: f32| v.clamp(-MAX_DB, MAX_DB);
        Some(EqSettings {
            enabled: self.enabled,
            preamp_db: clamp(self.preamp_db),
            bands_db: self.bands_db.map(clamp),
        })
    }
}

/// Normalised biquad coefficients (`a0` = 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coeffs {
    /// `|H(e^{jw})|` at the normalised angular frequency `w` (radians per sample).
    fn magnitude(&self, w: f64) -> f64 {
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// The frequency band `band` runs at for `rate` (clamped to 0.45 · rate).
pub(crate) fn band_freq(band: usize, rate: u32) -> f64 {
    BAND_HZ[band].min(0.45 * f64::from(rate))
}

/// RBJ cookbook biquad as WebAudio computes it: Q = 1 for the peaking bands, shelf slope S = 1.
pub(crate) fn coeffs(band: usize, gain_db: f64, rate: u32) -> Coeffs {
    let a = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * band_freq(band, rate) / f64::from(rate);
    let (sin, cos) = w0.sin_cos();
    let (b0, b1, b2, a0, a1, a2);
    if band == 0 || band == BAND_COUNT - 1 {
        let alpha = sin / 2.0 * SQRT_2;
        let t = 2.0 * a.sqrt() * alpha;
        if band == 0 {
            b0 = a * ((a + 1.0) - (a - 1.0) * cos + t);
            b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos);
            b2 = a * ((a + 1.0) - (a - 1.0) * cos - t);
            a0 = (a + 1.0) + (a - 1.0) * cos + t;
            a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos);
            a2 = (a + 1.0) + (a - 1.0) * cos - t;
        } else {
            b0 = a * ((a + 1.0) + (a - 1.0) * cos + t);
            b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos);
            b2 = a * ((a + 1.0) + (a - 1.0) * cos - t);
            a0 = (a + 1.0) - (a - 1.0) * cos + t;
            a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos);
            a2 = (a + 1.0) - (a - 1.0) * cos - t;
        }
    } else {
        let alpha = sin / 2.0; // Q = 1
        b0 = 1.0 + alpha * a;
        b1 = -2.0 * cos;
        b2 = 1.0 - alpha * a;
        a0 = 1.0 + alpha / a;
        a1 = -2.0 * cos;
        a2 = 1.0 - alpha / a;
    }
    Coeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

/// The bands of one [`EqSettings`] designed for one device rate (`Copy`; published to the
/// renderer). `rate` 0 is never a device rate, so the default design is never applied.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct EqDesign {
    pub(crate) rate: u32,
    pub(crate) on: [bool; BAND_COUNT],
    /// Each band's gain in dB (0.0 for a band that is off): the renderer's ramp target.
    pub(crate) db: [f64; BAND_COUNT],
    pub(crate) coef: [Coeffs; BAND_COUNT],
    /// Enabled and at least one band on.
    pub(crate) active: bool,
}

impl EqDesign {
    pub(crate) fn new(settings: &EqSettings, rate: u32) -> Self {
        let mut d = EqDesign {
            rate,
            ..EqDesign::default()
        };
        if rate == 0 {
            return d;
        }
        for b in 0..BAND_COUNT {
            let db = settings.bands_db[b];
            d.on[b] = settings.enabled && db != 0.0;
            if d.on[b] {
                d.db[b] = f64::from(db);
                d.coef[b] = coeffs(b, d.db[b], rate);
            }
        }
        d.active = d.on.iter().any(|&on| on);
        d
    }

    /// The combined response of the bands that are on, in dB, at `freq` Hz.
    pub(crate) fn response_db(&self, freq: f64) -> f64 {
        if self.rate == 0 {
            return 0.0;
        }
        let w = 2.0 * PI * freq / f64::from(self.rate);
        (0..BAND_COUNT)
            .filter(|&b| self.on[b])
            .map(|b| 20.0 * self.coef[b].magnitude(w).log10())
            .sum()
    }
}

#[derive(Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

/// Length of a gain change of the EQ bands and the preamp (H8, D7).
const RAMP_SECONDS: f64 = 0.020;
/// Frames per ramp step: a ramping band's coefficients are recomputed once per sub-block.
const SUB_BLOCK: usize = 32;
/// A band ramped to 0 dB is bypassed once its state is below this: −240 dBFS, far under the
/// step of every integer format (32-bit: about 4.7e-10).
const SETTLED: f64 = 1e-12;

/// Which stages the renderer runs this callback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stages {
    pub(crate) pre: bool,
    pub(crate) eq: bool,
}

/// The renderer's preamp (pre-gain) and EQ state: per band, per channel, transposed direct
/// form II; never allocates.
///
/// Gains ramp in dB toward the newest design over [`RAMP_SECONDS`] on a grid of
/// [`SUB_BLOCK`] frames that runs across callbacks (so a ramp does not depend on the callback
/// size). A ramping band's coefficients are the RBJ design at its current dB, recomputed per
/// sub-block; the preamp is interpolated per frame between the sub-block's end points. A band
/// turning on starts at 0 dB with zero state (the RBJ design at 0 dB is the identity); a band
/// turning off ramps to 0 dB and is bypassed once its state has decayed, so a flat or disabled
/// EQ ends bit-exact. A design for a new rate is applied at once, never ramped into.
pub(crate) struct EqFilter {
    state: [[State; 2]; BAND_COUNT],
    /// The rate the state belongs to; 0 before the first design (applied at once).
    rate: u32,
    live: [bool; BAND_COUNT],
    cur_db: [f64; BAND_COUNT],
    tgt_db: [f64; BAND_COUNT],
    step_db: [f64; BAND_COUNT],
    /// Ramp steps still to take, and sub-blocks to wait before the first one.
    left: [u32; BAND_COUNT],
    wait: [u32; BAND_COUNT],
    coef: [Coeffs; BAND_COUNT],
    /// Preamp: the dB at the end of the current sub-block, its target and step.
    pre_db: f64,
    pre_tgt: f64,
    pre_step_db: f64,
    pre_left: u32,
    /// Linear preamp at the start and the end of the current sub-block.
    pre_from: f64,
    pre_to: f64,
    /// Frames into the current sub-block.
    phase: usize,
}

impl Default for EqFilter {
    fn default() -> Self {
        EqFilter {
            state: [[State::default(); 2]; BAND_COUNT],
            rate: 0,
            live: [false; BAND_COUNT],
            cur_db: [0.0; BAND_COUNT],
            tgt_db: [0.0; BAND_COUNT],
            step_db: [0.0; BAND_COUNT],
            left: [0; BAND_COUNT],
            wait: [0; BAND_COUNT],
            coef: [Coeffs::default(); BAND_COUNT],
            pre_db: 0.0,
            pre_tgt: 1.0,
            pre_step_db: 0.0,
            pre_left: 0,
            pre_from: 1.0,
            pre_to: 1.0,
            phase: 0,
        }
    }
}

impl EqFilter {
    /// Forget everything (the published design is not for the open device): every stage is
    /// bypassed and the next design is applied at once.
    pub(crate) fn reset(&mut self) {
        *self = EqFilter::default();
    }

    /// Called once per callback with the newest design and linear pre-gain (exactly 1.0 is
    /// no pre-gain). Starts a ramp toward any changed target; returns the stages to run.
    pub(crate) fn arm(&mut self, design: &EqDesign, pre_gain: f64) -> Stages {
        if design.rate != self.rate {
            self.jump(design, pre_gain);
        } else {
            let idle = !self.ramping() && self.pre_from == self.pre_to;
            let mut started = false;
            let subs = ramp_subs(self.rate);
            let mut pre_down = false;
            if pre_gain != self.pre_tgt {
                started = true;
                let tgt_db = 20.0 * pre_gain.log10();
                pre_down = tgt_db < self.pre_db;
                self.pre_tgt = pre_gain;
                self.pre_step_db = (tgt_db - self.pre_db) / f64::from(subs);
                self.pre_left = subs;
            }
            for b in 0..BAND_COUNT {
                let target = if design.on[b] { design.db[b] } else { 0.0 };
                if target == self.tgt_db[b] {
                    continue;
                }
                started = true;
                if !self.live[b] {
                    self.live[b] = true;
                    self.state[b] = [State::default(); 2];
                    self.cur_db[b] = 0.0;
                    self.coef[b] = coeffs(b, 0.0, self.rate);
                }
                self.tgt_db[b] = target;
                self.step_db[b] = (target - self.cur_db[b]) / f64::from(subs);
                self.left[b] = subs;
                // A preamp going down (more headroom) leads the bands by one step, one going
                // up trails them, so the sum stays under the new headroom while it ramps.
                self.wait[b] = u32::from(pre_down);
            }
            // A ramp from idle starts at once rather than at the next sub-block.
            if started && idle {
                self.phase = 0;
            }
        }
        Stages {
            pre: self.pre_left > 0 || self.pre_from != 1.0 || self.pre_to != 1.0,
            eq: self.live.iter().any(|&l| l),
        }
    }

    /// Apply `design` and `pre_gain` at once (a new rate, or the first design).
    fn jump(&mut self, design: &EqDesign, pre_gain: f64) {
        *self = EqFilter::default();
        self.rate = design.rate;
        for b in 0..BAND_COUNT {
            if design.on[b] {
                self.live[b] = true;
                self.cur_db[b] = design.db[b];
                self.tgt_db[b] = design.db[b];
                self.coef[b] = design.coef[b];
            }
        }
        self.pre_db = if pre_gain == 1.0 {
            0.0
        } else {
            20.0 * pre_gain.log10()
        };
        self.pre_tgt = pre_gain;
        self.pre_from = pre_gain;
        self.pre_to = pre_gain;
    }

    /// Any band or the preamp still stepping.
    fn ramping(&self) -> bool {
        self.pre_left > 0 || (0..BAND_COUNT).any(|b| self.left[b] > 0 || self.wait[b] > 0)
    }

    /// The step at the start of a sub-block: advance every ramp, land exactly on the target,
    /// bypass bands that reached 0 dB and settled.
    fn step(&mut self) {
        for b in 0..BAND_COUNT {
            if !self.live[b] {
                continue;
            }
            if self.wait[b] > 0 {
                self.wait[b] -= 1;
            } else if self.left[b] > 0 {
                self.left[b] -= 1;
                self.cur_db[b] = if self.left[b] == 0 {
                    self.tgt_db[b]
                } else {
                    self.cur_db[b] + self.step_db[b]
                };
                self.coef[b] = coeffs(b, self.cur_db[b], self.rate);
            } else if self.tgt_db[b] == 0.0
                && self.state[b]
                    .iter()
                    .all(|s| s.z1.abs() < SETTLED && s.z2.abs() < SETTLED)
            {
                self.live[b] = false;
            }
        }
        if self.pre_left > 0 {
            self.pre_left -= 1;
            if self.pre_left == 0 {
                self.pre_to = self.pre_tgt;
                self.pre_db = if self.pre_tgt == 1.0 {
                    0.0
                } else {
                    20.0 * self.pre_tgt.log10()
                };
            } else {
                self.pre_db += self.pre_step_db;
                self.pre_to = 10f64.powf(self.pre_db / 20.0);
            }
        }
    }

    /// Apply the preamp, then the live bands, to `block` in place.
    pub(crate) fn process(&mut self, block: &mut [[f64; 2]]) {
        let mut i = 0;
        while i < block.len() {
            if self.phase == 0 {
                self.step();
            }
            let n = (SUB_BLOCK - self.phase).min(block.len() - i);
            let chunk = &mut block[i..i + n];
            if self.pre_from != self.pre_to {
                let d = (self.pre_to - self.pre_from) / SUB_BLOCK as f64;
                for (k, f) in chunk.iter_mut().enumerate() {
                    let g = self.pre_from + d * (self.phase + k + 1) as f64;
                    f[0] *= g;
                    f[1] *= g;
                }
            } else if self.pre_to != 1.0 {
                for f in chunk.iter_mut() {
                    f[0] *= self.pre_to;
                    f[1] *= self.pre_to;
                }
            }
            for frame in chunk.iter_mut() {
                for (ch, s) in frame.iter_mut().enumerate() {
                    let mut x = *s;
                    for b in 0..BAND_COUNT {
                        if !self.live[b] {
                            continue;
                        }
                        let c = &self.coef[b];
                        let st = &mut self.state[b][ch];
                        let y = c.b0 * x + st.z1;
                        st.z1 = c.b1 * x - c.a1 * y + st.z2;
                        st.z2 = c.b2 * x - c.a2 * y;
                        x = y;
                    }
                    *s = x;
                }
            }
            self.phase += n;
            if self.phase == SUB_BLOCK {
                self.phase = 0;
                self.pre_from = self.pre_to;
            }
            i += n;
        }
    }
}

#[cfg(test)]
impl EqFilter {
    /// The coefficient swap before H8 (the reference the click test measures against): every
    /// target applied at once, a band turning on from zero state, one turning off dropped.
    pub(crate) fn hard_arm(&mut self, design: &EqDesign, pre_gain: f64) {
        if design.rate != self.rate {
            self.jump(design, pre_gain);
            return;
        }
        for b in 0..BAND_COUNT {
            if design.on[b] && !self.live[b] {
                self.state[b] = [State::default(); 2];
            }
            self.live[b] = design.on[b];
            self.cur_db[b] = design.db[b];
            self.tgt_db[b] = design.db[b];
            self.coef[b] = design.coef[b];
            (self.left[b], self.wait[b]) = (0, 0);
        }
        self.pre_tgt = pre_gain;
        self.pre_from = pre_gain;
        self.pre_to = pre_gain;
        self.pre_left = 0;
        self.pre_db = 20.0 * pre_gain.log10();
    }
}

/// Ramp steps for `rate`: [`RAMP_SECONDS`] rounded up to whole sub-blocks.
fn ramp_subs(rate: u32) -> u32 {
    ((RAMP_SECONDS * f64::from(rate)) / SUB_BLOCK as f64)
        .ceil()
        .max(1.0) as u32
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::controls::make_params;

    const RATE: u32 = 44_100;
    const N: usize = 44_100;

    fn sine(freq: f64, frames: usize) -> Vec<[f64; 2]> {
        (0..frames)
            .map(|i| {
                let v = 0.1 * (2.0 * PI * freq * i as f64 / f64::from(RATE)).sin();
                [v, v]
            })
            .collect()
    }

    fn rms_tail(buf: &[[f64; 2]]) -> f64 {
        let tail = &buf[buf.len() / 2..];
        (tail.iter().map(|f| f[0] * f[0]).sum::<f64>() / tail.len() as f64).sqrt()
    }

    fn settings(f: impl FnOnce(&mut EqSettings)) -> EqSettings {
        let mut s = EqSettings::default();
        f(&mut s);
        s
    }

    /// Run `block` through a fresh filter armed with `s`; whether it ran.
    fn run(s: &EqSettings, block: &mut [[f64; 2]]) -> bool {
        let d = EqDesign::new(s, RATE);
        let mut f = EqFilter::default();
        let on = f.arm(&d, 1.0).eq;
        if on {
            f.process(block);
        }
        on
    }

    fn gain_db(s: &EqSettings, freq: f64) -> f64 {
        let input = sine(freq, N);
        let mut out = input.clone();
        run(s, &mut out);
        20.0 * (rms_tail(&out) / rms_tail(&input)).log10()
    }

    #[test]
    fn flat_is_bit_exact() {
        let input = sine(1000.0, 1000);
        let mut out = input.clone();
        assert!(!run(&EqSettings::default(), &mut out));
        assert_eq!(input, out);
    }

    #[test]
    fn disabled_bypasses_bands() {
        let s = settings(|s| {
            s.enabled = false;
            s.preamp_db = 12.0;
            s.bands_db = [12.0; BAND_COUNT];
        });
        let d = EqDesign::new(&s, RATE);
        assert!(!d.active && d.on.iter().all(|&on| !on));
        let input = sine(1000.0, 1000);
        let mut out = input.clone();
        assert!(!run(&s, &mut out));
        assert_eq!(input, out);
    }

    #[test]
    fn peaking_band_boosts_its_frequency_only() {
        let s = settings(|s| s.bands_db[4] = 12.0);
        let g = gain_db(&s, 1000.0);
        // measured 7.1e-15 dB (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!((g - 12.0).abs() < 1e-9, "1 kHz: {g}");
        let g = gain_db(&s, 100.0);
        // measured 0.161 dB (2026-11-14, 1.92 x86_64), the RBJ peak's skirt (Q 1); bound 0.65 (4x).
        assert!(g.abs() < 0.65, "100 Hz: {g}");
    }

    #[test]
    fn peaking_band_cuts() {
        let g = gain_db(&settings(|s| s.bands_db[4] = -12.0), 1000.0);
        // measured 1.8e-14 dB (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!((g + 12.0).abs() < 1e-9, "{g}");
    }

    #[test]
    fn low_shelf_boosts_below_its_corner() {
        let s = settings(|s| s.bands_db[0] = 12.0);
        let g = gain_db(&s, 10.0);
        // measured -0.0125 dB (2026-11-14, 1.92 x86_64), the shelf's skirt; bound 0.05 (4x).
        assert!((g - 12.0).abs() < 0.05, "{g}");
        let g = gain_db(&s, 40.0);
        // measured 9.69 dB (2026-11-14, 1.92 x86_64), between the plateau and the corner; bound +-0.2 dB.
        assert!((9.5..9.9).contains(&g), "{g}");
        let g = gain_db(&s, 5000.0);
        // measured 2.8e-7 dB (2026-11-14, 1.92 x86_64); bound 1e-6.
        assert!(g.abs() < 1e-6, "{g}");
    }

    #[test]
    fn high_shelf_boosts_above_its_corner() {
        let s = settings(|s| s.bands_db[9] = 12.0);
        let g = gain_db(&s, 20000.0);
        // measured -0.166 dB (2026-11-14, 1.92 x86_64): 20 kHz at 48 kHz is still on the shelf's skirt; bound 0.7 (4x).
        assert!((g - 12.0).abs() < 0.7, "{g}");
        let g = gain_db(&s, 1000.0);
        // measured 1.9e-5 dB (2026-11-14, 1.92 x86_64); bound 1e-4.
        assert!(g.abs() < 1e-4, "{g}");
    }

    #[test]
    fn response_matches_measurement() {
        let s = settings(|s| s.bands_db[4] = 6.0);
        let d = EqDesign::new(&s, RATE);
        // measured 4.4e-15 and 4.4e-14 dB (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!((d.response_db(1000.0) - 6.0).abs() < 1e-9);
        assert!((gain_db(&s, 2000.0) - d.response_db(2000.0)).abs() < 1e-9);
    }

    #[test]
    fn split_buffers_equal_one_buffer() {
        let s = settings(|s| {
            s.bands_db = [9.6, -7.2, 7.2, 0.0, 12.0, -12.0, 3.6, -4.8, 8.4, 10.8];
        });
        let d = EqDesign::new(&s, RATE);
        let input = sine(777.0, 4000);
        let mut whole = input.clone();
        let mut f = EqFilter::default();
        assert!(f.arm(&d, 1.0).eq);
        f.process(&mut whole);
        let mut split = input.clone();
        let mut f = EqFilter::default();
        f.arm(&d, 1.0);
        let (a, b) = split.split_at_mut(1234);
        f.process(a);
        f.arm(&d, 1.0);
        f.process(b);
        assert_eq!(whole, split);
    }

    /// A filter that has armed a flat design at `RATE`, so the next design ramps.
    fn armed_flat() -> EqFilter {
        let mut f = EqFilter::default();
        let flat = EqDesign::new(&EqSettings::default(), RATE);
        assert_eq!(f.arm(&flat, 1.0), Stages::default());
        f
    }

    fn zero(s: &State) -> bool {
        s.z1 == 0.0 && s.z2 == 0.0
    }

    #[test]
    fn band_returning_from_flat_starts_from_zero_state() {
        // Band 1 keeps the EQ active while band 4 sits at 0 dB.
        let on = settings(|s| {
            s.bands_db[1] = 3.0;
            s.bands_db[4] = 12.0;
        });
        let mut off = on;
        off.bands_db[4] = 0.0;
        let (d_on, d_off) = (EqDesign::new(&on, RATE), EqDesign::new(&off, RATE));
        let mut f = armed_flat();
        f.arm(&d_on, 1.0);
        f.process(&mut sine(1000.0, 2000));
        f.arm(&d_off, 1.0);
        f.process(&mut sine(1000.0, 500));
        // Mid-ramp the band still runs on its state.
        assert!(f.live[4] && f.state[4].iter().any(|s| s.z1 != 0.0));
        // Ramped to 0 dB and settled: bypassed.
        f.process(&mut sine(1000.0, N / 2));
        assert!(!f.live[4] && f.live[1]);
        let band1 = f.state[1];
        f.arm(&d_on, 1.0);
        assert!(f.state[4].iter().all(zero));
        // Band 1 stayed on: its state carries over.
        assert!((0..2).all(|ch| f.state[1][ch].z1 == band1[ch].z1));
        assert!(band1[0].z1 != 0.0);
    }

    #[test]
    fn inactive_to_active_zeroes_all_state() {
        let s = settings(|s| s.bands_db[4] = 6.0);
        let d = EqDesign::new(&s, RATE);
        let off = EqDesign::new(
            &settings(|s| {
                s.enabled = false;
                s.bands_db[4] = 6.0;
            }),
            RATE,
        );
        let mut f = armed_flat();
        f.arm(&d, 1.0);
        f.process(&mut sine(1000.0, 2000));
        // Disabling ramps the band out first, then bypasses it.
        assert!(f.arm(&off, 1.0).eq);
        f.process(&mut sine(1000.0, N / 2));
        assert!(!f.arm(&off, 1.0).eq);
        assert!(f.arm(&d, 1.0).eq);
        assert!(f.state.iter().flatten().all(zero));
    }

    /// 50 Hz, 1 kHz and 15 kHz at once.
    fn mix(frames: usize) -> Vec<[f64; 2]> {
        let (a, b, c) = (
            sine(50.0, frames),
            sine(1000.0, frames),
            sine(15000.0, frames),
        );
        (0..frames)
            .map(|i| [a[i][0] + b[i][0] + c[i][0], a[i][1] + b[i][1] - c[i][1]])
            .collect()
    }

    #[test]
    fn ramp_converges_to_the_steady_response() {
        let s = settings(|s| {
            s.bands_db[0] = -9.0;
            s.bands_db[4] = 12.0;
            s.bands_db[9] = 6.0;
        });
        let p = make_params(&s, RATE);
        assert!(p.pre_gain < 1.0);
        let input = mix(N);
        let mut ramped = input.clone();
        let mut f = armed_flat();
        let start = 1000;
        f.process(&mut ramped[..start]);
        assert!(f.arm(&p.eq, p.pre_gain).eq);
        let mut end = start;
        while f.ramping() || f.pre_from != f.pre_to {
            f.process(&mut ramped[end..end + SUB_BLOCK]);
            end += SUB_BLOCK;
        }
        // 20 ms rounded up to whole sub-blocks, one more while the preamp leads.
        assert_eq!(end - start, (ramp_subs(RATE) as usize + 1) * SUB_BLOCK);
        assert!((end - start) as f64 >= RAMP_SECONDS * f64::from(RATE));
        // The ramp lands exactly on the target design.
        assert_eq!((f.pre_from, f.pre_to), (p.pre_gain, p.pre_gain));
        for b in 0..BAND_COUNT {
            assert_eq!(f.live[b], p.eq.on[b], "band {b}");
            if p.eq.on[b] {
                assert_eq!(f.coef[b], p.eq.coef[b], "band {b}");
            }
        }
        f.process(&mut ramped[end..]);
        // A fresh filter with the target design driven from the ramp's end.
        let mut fresh = input[end..].to_vec();
        let mut g = EqFilter::default();
        g.arm(&p.eq, p.pre_gain);
        g.process(&mut fresh);
        let settle = N / 4;
        let err = ramped[end + settle..]
            .iter()
            .zip(&fresh[settle..])
            .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
            .fold(0.0, f64::max);
        // measured 7.9e-15 (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!(err < 1e-9, "{err}");
    }

    #[test]
    fn toggling_within_a_ramp_lands_every_band() {
        let s = settings(|s| {
            s.preamp_db = 3.0;
            s.bands_db = [6.0, -6.0, 3.0, 0.0, 12.0, -12.0, 0.0, 9.0, -3.0, 6.0];
        });
        let mut s_off = s;
        s_off.enabled = false;
        let (on, off) = (make_params(&s, RATE), make_params(&s_off, RATE));
        for end_on in [true, false] {
            let mut f = armed_flat();
            // Eleven toggles 100 frames apart: every one lands inside the previous ramp.
            for i in 0..11 {
                let p = if i % 2 == 0 { &on } else { &off };
                f.arm(&p.eq, p.pre_gain);
                f.process(&mut sine(1000.0, 100));
                assert!(f.ramping(), "toggle {i}");
            }
            let last = if end_on { &on } else { &off };
            f.arm(&last.eq, last.pre_gain);
            f.process(&mut sine(1000.0, N));
            let st = f.arm(&last.eq, last.pre_gain);
            assert!(!f.ramping());
            assert_eq!((f.pre_from, f.pre_to), (last.pre_gain, last.pre_gain));
            for b in 0..BAND_COUNT {
                assert_eq!(f.live[b], last.eq.on[b], "band {b}, on {end_on}");
                if last.eq.on[b] {
                    assert_eq!(f.cur_db[b], last.eq.db[b]);
                    assert_eq!(f.coef[b], last.eq.coef[b]);
                }
            }
            if !end_on {
                // Disabled: everything bypassed again, the output bit-exact.
                assert_eq!(st, Stages::default());
                let input = sine(1000.0, 1000);
                let mut out = input.clone();
                f.process(&mut out);
                assert_eq!(input, out);
            }
        }
    }

    #[test]
    fn a_ramp_does_not_depend_on_the_callback_size() {
        let p = make_params(
            &settings(|s| {
                s.bands_db[0] = 9.0;
                s.bands_db[5] = -6.0;
            }),
            RATE,
        );
        let input = sine(777.0, 4000);
        let run = |sizes: &[usize]| {
            let mut f = armed_flat();
            let mut out = input.clone();
            let mut i = 0;
            for &n in sizes.iter().cycle() {
                if i == out.len() {
                    break;
                }
                let n = n.min(out.len() - i);
                f.arm(&p.eq, p.pre_gain);
                f.process(&mut out[i..i + n]);
                i += n;
            }
            out
        };
        let whole = run(&[4000]);
        assert_eq!(whole, run(&[1, 7, 31, 32, 33, 100, 513]));
        assert_ne!(whole, input);
    }

    #[test]
    fn a_new_rate_is_applied_at_once() {
        let s = settings(|s| s.bands_db[4] = 6.0);
        let mut f = armed_flat();
        f.arm(&EqDesign::new(&s, RATE), 1.0);
        f.process(&mut sine(1000.0, 100));
        assert!(f.ramping());
        let d48 = EqDesign::new(&s, 48_000);
        assert!(f.arm(&d48, 1.0).eq);
        assert!(!f.ramping() && f.state.iter().flatten().all(zero));
        assert_eq!(f.coef[4], d48.coef[4]);
    }
    #[test]
    fn band_frequency_clamped_and_wrong_rate_never_default() {
        assert_eq!(band_freq(9, 22_050), 0.45 * 22_050.0);
        assert_eq!(band_freq(9, 48_000), 16_000.0);
        let d = EqDesign::new(&settings(|s| s.bands_db[9] = 6.0), 0);
        assert!(!d.active);
    }

    #[test]
    fn non_finite_settings_are_rejected_and_values_clamped() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(settings(|s| s.preamp_db = bad).sanitised(), None);
            assert_eq!(settings(|s| s.bands_db[3] = bad).sanitised(), None);
        }
        let s = settings(|s| {
            s.preamp_db = 30.0;
            s.bands_db[0] = -40.0;
        })
        .sanitised()
        .unwrap();
        assert_eq!((s.preamp_db, s.bands_db[0]), (12.0, -12.0));
    }
}
