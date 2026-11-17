//! The control side of the DSP chain (non-real-time; may allocate): volume, balance and EQ
//! settings become per-channel gain targets (two atomics) and a `Copy` [`DspParams`] for the
//! open device rate, published to the renderer through a triple buffer.

use std::sync::atomic::Ordering;

use triple_buffer::Input;

use super::eq::{BAND_COUNT, EqDesign, EqSettings, band_freq};
use super::format::ReplayGainMode;
use super::gain::targets;
use super::shared::Shared;

/// Points of the headroom grid (log-spaced, 10 Hz to 0.49 · rate) besides the band centres.
const GRID_POINTS: usize = 512;
const GRID_LOW_HZ: f64 = 10.0;

/// What the renderer applies before the gains: pre-gain and EQ for one device rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DspParams {
    pub(crate) eq: EqDesign,
    /// Linear pre-gain; exactly 1.0 bypasses the stage.
    pub(crate) pre_gain: f64,
    /// Preamp minus the effective preamp (dB).
    pub(crate) headroom_db: f32,
    /// Which ReplayGain value the renderer applies per track (D8); independent of the rate.
    pub(crate) replay_gain: ReplayGainMode,
}

impl Default for DspParams {
    fn default() -> Self {
        DspParams {
            eq: EqDesign::default(),
            pre_gain: 1.0,
            headroom_db: 0.0,
            replay_gain: ReplayGainMode::Off,
        }
    }
}

/// The largest steady-state response of the EQ's bands over the headroom grid (dB); 0 when no
/// band is on.
pub(crate) fn max_response_db(eq: &EqDesign) -> f64 {
    if !eq.active || eq.rate == 0 {
        return 0.0;
    }
    let hi = 0.49 * f64::from(eq.rate);
    let ratio = hi / GRID_LOW_HZ;
    let grid = (0..GRID_POINTS)
        .map(|i| GRID_LOW_HZ * ratio.powf(i as f64 / (GRID_POINTS - 1) as f64))
        .chain((0..BAND_COUNT).map(|b| band_freq(b, eq.rate)));
    grid.map(|f| eq.response_db(f)).fold(f64::MIN, f64::max)
}

/// Design the DSP parameters of `eq` for `rate` (MD28 headroom: preamp plus EQ never exceeds
/// 0 dB; a positive preamp only compensates cuts; a disabled EQ bypasses its preamp too).
pub(crate) fn make_params(eq: &EqSettings, rate: u32) -> DspParams {
    let design = EqDesign::new(eq, rate);
    if !eq.enabled || rate == 0 {
        return DspParams {
            eq: design,
            ..DspParams::default()
        };
    }
    let max_db = max_response_db(&design);
    let preamp = f64::from(eq.preamp_db);
    let pre_db = preamp.min(-max_db);
    let pre_gain = if pre_db == 0.0 {
        1.0
    } else {
        10f64.powf(pre_db / 20.0)
    };
    DspParams {
        eq: design,
        pre_gain,
        headroom_db: (preamp - pre_db) as f32,
        ..DspParams::default()
    }
}

/// The engine's control state: what the user set, and the publication to the renderer.
pub(crate) struct Controls {
    pub(crate) eq: EqSettings,
    pub(crate) volume: u8,
    pub(crate) balance: i8,
    pub(crate) replay_gain: ReplayGainMode,
    params: Input<DspParams>,
    /// The device rate the published parameters are for (0 while none is open).
    rate: u32,
}

impl Controls {
    pub(crate) fn new(params: Input<DspParams>) -> Self {
        Controls {
            eq: EqSettings::default(),
            volume: 100,
            balance: 0,
            replay_gain: ReplayGainMode::Off,
            params,
            rate: 0,
        }
    }

    pub(crate) fn set_volume(&mut self, shared: &Shared, volume: u8) {
        self.volume = volume.min(100);
        self.store_gains(shared);
    }

    pub(crate) fn set_balance(&mut self, shared: &Shared, balance: i8) {
        self.balance = balance.clamp(-100, 100);
        self.store_gains(shared);
    }

    /// Non-finite settings are ignored as a whole; dB values are clamped to ±12.
    pub(crate) fn set_eq(&mut self, eq: EqSettings) {
        if let Some(eq) = eq.sanitised() {
            self.eq = eq;
            self.publish();
        }
    }

    /// The ReplayGain mode (D8); applied from the next callback.
    pub(crate) fn set_replay_gain(&mut self, mode: ReplayGainMode) {
        self.replay_gain = mode;
        self.publish();
    }

    /// Recompute the parameters for a newly opened device rate.
    pub(crate) fn set_rate(&mut self, rate: u32) {
        self.rate = rate;
        self.publish();
    }

    fn publish(&mut self) {
        let params = DspParams {
            replay_gain: self.replay_gain,
            ..make_params(&self.eq, self.rate)
        };
        self.params.write(params);
    }

    fn store_gains(&self, shared: &Shared) {
        let (l, r) = targets(self.volume, self.balance);
        shared.gain_l.store(l.to_bits(), Ordering::Relaxed);
        shared.gain_r.store(r.to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq(f: impl FnOnce(&mut EqSettings)) -> EqSettings {
        let mut s = EqSettings::default();
        f(&mut s);
        s
    }

    #[test]
    fn flat_or_disabled_bypasses_preamp() {
        // Enabled, flat bands, positive preamp: nothing to compensate.
        let p = make_params(&eq(|s| s.preamp_db = 3.0), 44_100);
        assert_eq!(p.pre_gain, 1.0);
        assert!(!p.eq.active);
        assert_eq!(p.headroom_db, 3.0);
        // Disabled: preamp and bands bypassed.
        let p = make_params(
            &eq(|s| {
                s.enabled = false;
                s.preamp_db = 12.0;
                s.bands_db = [12.0; BAND_COUNT];
            }),
            44_100,
        );
        assert_eq!(p.pre_gain, 1.0);
        assert!(!p.eq.active);
        assert_eq!(p.headroom_db, 0.0);
        // A negative preamp alone applies.
        let p = make_params(&eq(|s| s.preamp_db = -6.0), 44_100);
        // measured 8.9e-16 dB (2026-11-14, 1.92 x86_64); bound 1e-12 (rounding).
        assert!((20.0 * p.pre_gain.log10() + 6.0).abs() < 1e-12);
    }

    #[test]
    fn boost_is_cancelled_and_cut_compensated() {
        let p = make_params(&eq(|s| s.bands_db[4] = 12.0), 48_000);
        // measured 0 and 8.9e-15 dB (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!((p.headroom_db - 12.0).abs() < 1e-9, "{}", p.headroom_db);
        assert!((20.0 * p.pre_gain.log10() + 12.0).abs() < 1e-9);
        // All bands cut 6 dB: a +12 dB preamp is allowed only up to the cut.
        let p = make_params(
            &eq(|s| {
                s.preamp_db = 12.0;
                s.bands_db = [-6.0; BAND_COUNT];
            }),
            48_000,
        );
        let pre_db = 20.0 * p.pre_gain.log10();
        assert!(pre_db > 0.0 && pre_db < 12.0, "{pre_db}");
        let max = max_response_db(&p.eq);
        // measured 4.4e-16 dB (2026-11-14, 1.92 x86_64); bound 1e-9 (rounding).
        assert!((pre_db + max).abs() < 1e-9);
    }
}
