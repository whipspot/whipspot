//! The renderer's per-callback status snapshot (published lock-free; every field is `Copy`).

use super::clock::DriftFit;
use super::format::{DeviceFormat, StreamFormat};

/// Which stages touched the samples in the last callback.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StageFlags {
    /// The track's ReplayGain scaled the samples (D8).
    pub replay_gain: bool,
    pub pre_gain: bool,
    pub eq: bool,
    pub volume: bool,
    pub balance: bool,
    pub limiter: bool,
    pub limiter_engaged: bool,
    pub dither: bool,
    pub fade: bool,
    /// No stage touched the samples, the track is not resampled and the device format holds
    /// the source: the output is the source quantised once, without dither.
    pub exact: bool,
}

/// The track the renderer is playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackInfo {
    pub format: StreamFormat,
    pub resampled: bool,
}

/// What the engine reports about its output path.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct EngineStatus {
    pub callbacks: u64,
    pub frames: u64,
    pub underruns: u64,
    pub tap_drops: u64,
    pub clips: u64,
    pub limiter_frames: u64,
    pub applied: StageFlags,
    pub track: Option<TrackInfo>,
    /// The track's `End` mark was reached.
    pub ending: bool,
    pub paused: bool,
    pub discarding: bool,
    pub ring_fill: u32,
    pub ring_depth: u32,
    /// Linear peak of the final pre-quantiser output (a hold falling about 12 dB/s).
    pub peak: f64,
    pub cpu_avg_ns: u64,
    /// The longest callback since the stream opened (H18).
    pub cpu_max_ns: u64,
    /// The fit of frames rendered against callback start times since the stream opened,
    /// after the warm-up: the clock drift (H18, MD138).
    pub clock: DriftFit,
    pub latency_ns: Option<u64>,
    pub block_frames: u32,
    pub device: Option<DeviceFormat>,
    /// Frames the source wrote.
    pub source_frames: u64,
    /// The EQ preamp minus the preamp actually applied (dB; MD28).
    pub headroom_db: f32,
    /// The ReplayGain the current track gets under the current mode (dB; 0 when bypassed).
    pub replay_gain_db: f32,
}
