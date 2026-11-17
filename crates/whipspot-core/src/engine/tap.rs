//! The PCM tap: mono `f32` copies of what the output plays, for the spectrum. Lock-free, lossy:
//! a full tap drops (and counts) samples rather than block the writer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

/// Writing half of the tap (the renderer's, or the fallback output's when the engine has none).
pub struct TapWriter {
    ring: Producer<f32>,
    drops: Arc<AtomicU64>,
    rate: Arc<AtomicU32>,
}

/// Reading half of the tap.
pub struct PcmTap {
    ring: Consumer<f32>,
    rate: Arc<AtomicU32>,
}

pub(crate) fn tap_channel(
    capacity: usize,
    rate: u32,
    drops: Arc<AtomicU64>,
) -> (TapWriter, PcmTap) {
    let (p, c) = RingBuffer::new(capacity.max(1));
    let rate = Arc::new(AtomicU32::new(rate));
    (
        TapWriter {
            ring: p,
            drops,
            rate: rate.clone(),
        },
        PcmTap { ring: c, rate },
    )
}

impl TapWriter {
    /// Push interleaved stereo as mono `(L+R)/2`; what does not fit is dropped and counted.
    /// An odd trailing sample is ignored.
    pub fn push_stereo(&mut self, interleaved: &[f64]) {
        let frames = interleaved.len() / 2;
        let room = self.ring.slots().min(frames);
        if let Ok(mut chunk) = self.ring.write_chunk(room) {
            let (a, b) = chunk.as_mut_slices();
            let mut src = interleaved.chunks_exact(2);
            for dst in a.iter_mut().chain(b.iter_mut()) {
                if let Some(f) = src.next() {
                    *dst = ((f[0] + f[1]) * 0.5) as f32;
                }
            }
            chunk.commit_all();
        }
        if frames > room {
            self.drops
                .fetch_add((frames - room) as u64, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Ordering::Relaxed);
    }
}

impl PcmTap {
    /// Sample rate of the tapped audio (the open device's, or the fallback's).
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Hand everything queued to `f` (in at most two slices) and consume it.
    pub fn drain(&mut self, mut f: impl FnMut(&[f32])) {
        let n = self.ring.slots();
        if n == 0 {
            return;
        }
        if let Ok(chunk) = self.ring.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            if !a.is_empty() {
                f(a);
            }
            if !b.is_empty() {
                f(b);
            }
            chunk.commit_all();
        }
    }
}
