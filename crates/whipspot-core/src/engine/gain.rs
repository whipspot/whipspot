//! Volume and balance (MD29): whipspot's laws as per-channel targets, ramped linearly on the
//! renderer.

/// Volume ramp length.
pub(crate) const RAMP_SECONDS: f64 = 0.010;

/// Volume 0..=100 (above 100 is 100) → linear gain `(v/100)^2`; 100 gives exactly 1.0.
pub(crate) fn volume_gain(volume: u8) -> f64 {
    let x = f64::from(volume.min(100)) / 100.0;
    x * x
}

/// Balance −100..=100 (clamped) → (left, right): the far channel falls linearly to 0.
pub(crate) fn balance_gains(balance: i8) -> (f64, f64) {
    let b = f64::from(balance.clamp(-100, 100)) / 100.0;
    (
        if b > 0.0 { 1.0 - b } else { 1.0 },
        if b < 0.0 { 1.0 + b } else { 1.0 },
    )
}

/// The per-channel targets of `volume` and `balance`.
pub(crate) fn targets(volume: u8, balance: i8) -> (f64, f64) {
    let g = volume_gain(volume);
    let (l, r) = balance_gains(balance);
    (g * l, g * r)
}

/// The renderer's current gains and the linear ramp toward the newest target.
pub(crate) struct GainRamp {
    cur: [f64; 2],
    target: [f64; 2],
    step: [f64; 2],
    left: u32,
}

impl Default for GainRamp {
    fn default() -> Self {
        GainRamp {
            cur: [1.0; 2],
            target: [1.0; 2],
            step: [0.0; 2],
            left: 0,
        }
    }
}

impl GainRamp {
    /// Ramp toward `target` over `frames` frames, from the current gains. A target already
    /// being approached (or reached) is left alone.
    pub(crate) fn retarget(&mut self, target: [f64; 2], frames: u32) {
        if target == self.target {
            return;
        }
        self.target = target;
        if frames == 0 || self.cur == target {
            self.cur = target;
            self.left = 0;
            return;
        }
        let n = f64::from(frames);
        self.step = [(target[0] - self.cur[0]) / n, (target[1] - self.cur[1]) / n];
        self.left = frames;
    }

    /// Exactly unity and not ramping: the stage is bypassed.
    pub(crate) fn is_unity(&self) -> bool {
        self.left == 0 && self.cur == [1.0, 1.0]
    }

    pub(crate) fn ramping(&self) -> bool {
        self.left > 0
    }

    pub(crate) fn current(&self) -> [f64; 2] {
        self.cur
    }

    /// Apply the gains to `block`; the last ramp frame assigns the target exactly.
    pub(crate) fn process(&mut self, block: &mut [[f64; 2]]) {
        for f in block.iter_mut() {
            if self.left > 0 {
                self.left -= 1;
                if self.left == 0 {
                    self.cur = self.target;
                } else {
                    self.cur[0] += self.step[0];
                    self.cur[1] += self.step[1];
                }
            }
            f[0] *= self.cur[0];
            f[1] *= self.cur[1];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_gains_match_webamp() {
        assert_eq!(balance_gains(0), (1.0, 1.0));
        assert_eq!(balance_gains(-100), (1.0, 0.0));
        assert_eq!(balance_gains(100), (0.0, 1.0));
        assert_eq!(balance_gains(50), (0.5, 1.0));
        assert_eq!(balance_gains(-50), (1.0, 0.5));
        assert_eq!(balance_gains(127), (0.0, 1.0));
        assert_eq!(balance_gains(-128), (1.0, 0.0));
    }

    #[test]
    fn volume_law() {
        assert_eq!(volume_gain(100), 1.0);
        assert_eq!(volume_gain(255), 1.0);
        assert_eq!(volume_gain(50), 0.25);
        assert_eq!(volume_gain(0), 0.0);
        assert_eq!(targets(50, 50), (0.125, 0.25));
        assert_eq!(targets(100, 0), (1.0, 1.0));
    }

    #[test]
    fn ramp_lands_exactly_and_bypasses_at_unity() {
        let mut r = GainRamp::default();
        assert!(r.is_unity());
        r.retarget([0.25, 0.25], 441);
        let mut block = vec![[1.0, 1.0]; 500];
        r.process(&mut block);
        let mut prev = 1.0;
        for (i, f) in block.iter().enumerate() {
            assert!(f[0] <= prev, "frame {i}");
            // The ramp is linear: 0.75/441 per frame; measured 1.5e-14 over the rule (2026-11-14, 1.92 x86_64);
            // bound rule + 1e-12.
            assert!(prev - f[0] <= 0.75 / 441.0 + 1e-12, "frame {i}");
            prev = f[0];
        }
        assert!(block[439][0] > 0.25);
        assert!(block[440..].iter().all(|f| f[0] == 0.25));
        r.retarget([1.0, 1.0], 441);
        r.process(&mut vec![[1.0, 1.0]; 441]);
        assert!(r.is_unity());
    }

    #[test]
    fn retarget_mid_ramp_starts_from_current() {
        let mut r = GainRamp::default();
        r.retarget([0.0, 0.0], 100);
        r.process(&mut vec![[1.0, 1.0]; 50]);
        let mid = r.current()[0];
        // measured 4.4e-16 (2026-11-14, 1.92 x86_64); bound 1e-12 (rounding).
        assert!((mid - 0.5).abs() < 1e-12);
        r.retarget([1.0, 1.0], 100);
        let mut block = vec![[1.0, 1.0]; 100];
        r.process(&mut block);
        // From 0.5 to 1.0 over 100 frames is 0.005 a frame: measured 0.005 (2026-11-14, 1.92 x86_64);
        // bound the rule + 1e-12.
        assert!(block[0][0] > mid && block[0][0] - mid < 0.005 + 1e-12);
        assert_eq!(block[99][0], 1.0);
        assert!(r.is_unity());
    }
}
