//! The safety limiter (MD28): linked stereo, instant attack, one-pole release (100 ms time
//! constant). The output magnitude never exceeds the ceiling.

use super::format::{DeviceFormat, SampleFormat};

const RELEASE_SECONDS: f64 = 0.1;
/// An envelope this close to 1 snaps to 1 (the release would otherwise stall just below it).
const SNAP: f64 = 1e-9;

pub(crate) struct Limiter {
    env: f64,
    release: f64,
    ceiling: f64,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter {
            env: 1.0,
            release: 1.0,
            ceiling: 1.0,
        }
    }
}

impl Limiter {
    /// The ceiling for `device`: 1.0 for float, 1 − 1.5 LSB for integer formats (dither can
    /// never push a limited sample past the range).
    pub(crate) fn ceiling_for(device: &DeviceFormat) -> f64 {
        match device.sample {
            SampleFormat::F32 => 1.0,
            SampleFormat::I16 | SampleFormat::I32 => {
                1.0 - 1.5 / f64::from(1u32 << (device.valid_bits - 1))
            }
        }
    }

    pub(crate) fn configure(&mut self, device: &DeviceFormat) {
        self.ceiling = Self::ceiling_for(device);
        self.release = 1.0 - (-1.0 / (RELEASE_SECONDS * f64::from(device.rate))).exp();
        self.env = 1.0;
    }

    /// Forget the envelope (the limiter did not run).
    pub(crate) fn reset(&mut self) {
        self.env = 1.0;
    }

    /// Limit `block` in place; returns the number of frames with a gain below 1.
    pub(crate) fn process(&mut self, block: &mut [[f64; 2]]) -> u64 {
        let ceil = self.ceiling;
        let mut limited = 0;
        for f in block.iter_mut() {
            let pk = f[0].abs().max(f[1].abs());
            let want = if pk > ceil { ceil / pk } else { 1.0 };
            self.env = if want < self.env {
                want
            } else {
                let e = (self.env + (1.0 - self.env) * self.release).min(want);
                if e > 1.0 - SNAP { 1.0 } else { e }
            };
            if self.env < 1.0 {
                f[0] = (f[0] * self.env).clamp(-ceil, ceil);
                f[1] = (f[1] * self.env).clamp(-ceil, ceil);
                limited += 1;
            }
        }
        limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(sample: SampleFormat, valid_bits: u8) -> DeviceFormat {
        DeviceFormat {
            rate: 48_000,
            channels: 2,
            sample,
            valid_bits,
        }
    }

    #[test]
    fn never_exceeds_the_ceiling_and_releases_to_unity() {
        for d in [
            dev(SampleFormat::F32, 32),
            dev(SampleFormat::I16, 16),
            dev(SampleFormat::I32, 24),
        ] {
            let mut l = Limiter::default();
            l.configure(&d);
            let ceil = Limiter::ceiling_for(&d);
            let mut block: Vec<[f64; 2]> = (0..4800)
                .map(|i| {
                    let v = 1.7 * (i as f64 * 0.05).sin();
                    [v, -0.5 * v]
                })
                .collect();
            assert!(l.process(&mut block) > 0);
            assert!(block.iter().flatten().all(|x| x.abs() <= ceil));
            // Quiet input: the envelope recovers to exactly 1.
            let mut quiet = vec![[0.1, 0.1]; 48_000 * 3];
            l.process(&mut quiet);
            assert_eq!(l.env, 1.0);
            assert_eq!(*quiet.last().unwrap(), [0.1, 0.1]);
        }
        assert_eq!(
            Limiter::ceiling_for(&dev(SampleFormat::I16, 16)),
            1.0 - 1.5 / 32_768.0
        );
    }
}
