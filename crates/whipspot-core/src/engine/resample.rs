//! Producer-side sample-rate conversion (MD30): rubato's asynchronous sinc resampler on the
//! source's thread, in fixed 1024-frame input chunks.
//!
//! The resampler keeps running across same-rate track boundaries (gapless); `reset` (flush,
//! interrupt acknowledgement) restarts it; `drain` (finish, rate change) lets the tail out with
//! exactly `round(input × device / source)` frames since the last reset. The output delay is
//! trimmed after every reset. Not real-time code: construction allocates (buffers, sinc table).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters};

use super::error::SourceError;

/// Input frames per resampler call.
pub(crate) const CHUNK: usize = 1024;
const OVERSAMPLING: usize = 256;

/// The algorithm and quality as diagnostics show them.
pub(crate) const ALGORITHM: &str = "rubato sinc";
pub(crate) const QUALITY: &str = "256 taps, 256x oversampling, cubic, Blackman-Harris 2";

pub(crate) struct Resample {
    r: Async<f64>,
    src_rate: u32,
    dev_rate: u32,
    /// Interleaved stereo input waiting for a full chunk (capacity `2 * CHUNK`).
    staged: Vec<f64>,
    /// Interleaved stereo output of one call.
    out: Vec<f64>,
    /// Output frames still to drop (the resampler's delay) since the last reset.
    skip: usize,
    in_total: u64,
    out_total: u64,
}

impl Resample {
    pub(crate) fn new(src_rate: u32, dev_rate: u32) -> Result<Self, SourceError> {
        let params = SincInterpolationParameters::default().oversampling_factor(OVERSAMPLING);
        let ratio = f64::from(dev_rate) / f64::from(src_rate);
        let r = Async::<f64>::new_sinc(ratio, 1.0, &params, CHUNK, 2, FixedAsync::Input)
            .map_err(|_| SourceError::Unsupported)?;
        let out = vec![0.0; 2 * r.output_frames_max()];
        let skip = r.output_delay();
        Ok(Resample {
            r,
            src_rate,
            dev_rate,
            staged: Vec::with_capacity(2 * CHUNK),
            out,
            skip,
            in_total: 0,
            out_total: 0,
        })
    }

    pub(crate) fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Forget all state (staged input, filter history); the next output is trimmed again.
    pub(crate) fn reset(&mut self) {
        self.r.reset();
        self.staged.clear();
        self.skip = self.r.output_delay();
        self.in_total = 0;
        self.out_total = 0;
    }

    /// Feed interleaved samples of `channels` (1 or 2) channels. `emit` receives interleaved
    /// stereo output and returns `Ok(false)` when the data was dropped (interrupt or reopen):
    /// then feeding stops and `Ok(false)` is returned.
    pub(crate) fn push(
        &mut self,
        interleaved: &[f64],
        channels: usize,
        emit: &mut dyn FnMut(&[f64]) -> Result<bool, SourceError>,
    ) -> Result<bool, SourceError> {
        for f in interleaved.chunks_exact(channels) {
            let (l, r) = if channels == 1 {
                (f[0], f[0])
            } else {
                (f[0], f[1])
            };
            self.staged.push(l);
            self.staged.push(r);
            if self.staged.len() == 2 * CHUNK {
                let n = self.process(None)?;
                self.in_total += CHUNK as u64;
                self.staged.clear();
                if !self.deliver(n, None, emit)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Let the tail out: the staged frames, then silence, until exactly
    /// `round(input × device / source)` frames came out since the last reset; then reset.
    pub(crate) fn drain(
        &mut self,
        emit: &mut dyn FnMut(&[f64]) -> Result<bool, SourceError>,
    ) -> Result<bool, SourceError> {
        let staged = self.staged.len() / 2;
        let total_in = self.in_total + staged as u64;
        let (src, dev) = (u64::from(self.src_rate), u64::from(self.dev_rate));
        let want = (total_in * dev + src / 2) / src;
        let mut partial = staged;
        let mut alive = true;
        while alive && self.out_total < want {
            self.staged.resize(2 * CHUNK, 0.0);
            let n = self.process(Some(partial))?;
            partial = 0;
            alive = self.deliver(n, Some(want), emit)?;
        }
        self.reset();
        Ok(alive)
    }

    /// One resampler call on `staged` (a full chunk, or `partial` real frames).
    fn process(&mut self, partial: Option<usize>) -> Result<usize, SourceError> {
        let input = InterleavedSlice::new(&self.staged[..], 2, CHUNK)
            .map_err(|_| SourceError::Unsupported)?;
        let frames = self.out.len() / 2;
        let mut output = InterleavedSlice::new_mut(&mut self.out[..], 2, frames)
            .map_err(|_| SourceError::Unsupported)?;
        let indexing = partial.map(|p| Indexing::new().partial_len(p));
        let (_, n) = self
            .r
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|_| SourceError::Unsupported)?;
        Ok(n)
    }

    /// Hand `out[..n]` to `emit` after the delay trim, at most up to `limit` frames in total.
    fn deliver(
        &mut self,
        n: usize,
        limit: Option<u64>,
        emit: &mut dyn FnMut(&[f64]) -> Result<bool, SourceError>,
    ) -> Result<bool, SourceError> {
        let drop = self.skip.min(n);
        self.skip -= drop;
        let mut k = n - drop;
        if let Some(limit) = limit {
            k = k.min(limit.saturating_sub(self.out_total) as usize);
        }
        if k == 0 {
            return Ok(true);
        }
        self.out_total += k as u64;
        emit(&self.out[2 * drop..2 * (drop + k)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: u32, dev: u32, frames: usize, split: usize) -> Vec<f64> {
        let mut rs = Resample::new(src, dev).unwrap();
        let input: Vec<f64> = (0..frames)
            .flat_map(|i| {
                let v = (i as f64 * 0.01).sin() * 0.5;
                [v, -v]
            })
            .collect();
        let mut out = Vec::new();
        let mut emit = |d: &[f64]| {
            out.extend_from_slice(d);
            Ok(true)
        };
        let (a, b) = input.split_at(2 * split);
        assert!(rs.push(a, 2, &mut emit).unwrap());
        assert!(rs.push(b, 2, &mut emit).unwrap());
        assert!(rs.drain(&mut emit).unwrap());
        out
    }

    #[test]
    fn drained_length_is_exact() {
        for (src, dev, frames) in [
            (44_100, 48_000, 88_200),
            (44_100, 48_000, 1_000),
            (48_000, 44_100, 10_001),
            (96_000, 48_000, 5_555),
        ] {
            let out = run(src, dev, frames, frames / 3);
            let want = (frames as u64 * u64::from(dev) + u64::from(src) / 2) / u64::from(src);
            assert_eq!(out.len() as u64, 2 * want, "{src} -> {dev}, {frames}");
            assert!(out.iter().all(|x| x.is_finite()));
        }
    }

    #[test]
    fn write_boundaries_do_not_matter_and_reset_restarts() {
        assert_eq!(
            run(44_100, 48_000, 9_000, 1),
            run(44_100, 48_000, 9_000, 4_321)
        );
        let mut rs = Resample::new(44_100, 48_000).unwrap();
        let mut n = 0;
        let mut emit = |d: &[f64]| {
            n += d.len();
            Ok(true)
        };
        rs.push(&[0.1; 2 * 3000], 2, &mut emit).unwrap();
        rs.reset();
        rs.push(&[0.1; 2 * 100], 2, &mut emit).unwrap();
        rs.drain(&mut emit).unwrap();
        assert!(n > 0);
        assert_eq!(rs.in_total, 0);
        assert_eq!(rs.skip, rs.r.output_delay());
    }
}
