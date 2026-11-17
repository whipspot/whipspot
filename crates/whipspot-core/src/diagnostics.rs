//! Audio path diagnostics ("nerd stats", D26): what the source, decoder, DSP chain and output
//! really do, and the verdict on the path. Every value is an `Option`: `None` means unknown, never
//! a guess. In M1 nothing reports yet, so the snapshot is the default (all unknown).

/// A DSP stage's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Active,
    Bypassed,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct SourceDiagnostics {
    pub provider: Option<String>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub bitrate_nominal_kbps: Option<u32>,
    pub bitrate_actual_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u8>,
    pub channels: Option<u8>,
    pub lossless: Option<bool>,
    /// The provider's quality cap with its reason ("Ogg Vorbis 320 kbps maximum").
    pub quality_cap: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct DecodeDiagnostics {
    pub decoder: Option<String>,
    pub frames_decoded: Option<u64>,
    pub decode_errors: Option<u64>,
}

/// The resampler in the path, if any.
#[derive(Debug, Clone, PartialEq)]
pub enum Resampler {
    None,
    Active {
        algorithm: String,
        ratio: f64,
        quality: String,
    },
}

/// The dither in the path, if any.
#[derive(Debug, Clone, PartialEq)]
pub enum Dither {
    None,
    Active(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct DspDiagnostics {
    pub eq: Option<Stage>,
    pub preamp: Option<Stage>,
    pub replay_gain: Option<Stage>,
    /// ReplayGain applied, dB.
    pub replay_gain_db: Option<f32>,
    pub volume: Option<Stage>,
    pub balance: Option<Stage>,
    pub headroom_db: Option<f32>,
    pub peak_dbfs: Option<f32>,
    pub clip_count: Option<u64>,
    pub resampler: Option<Resampler>,
    pub dither: Option<Dither>,
    /// The safety limiter (runs whenever a gain stage is active).
    pub limiter: Option<Stage>,
    /// Frames the limiter turned down.
    pub limiter_frames: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct OutputDiagnostics {
    /// Host API ("WASAPI", "CoreAudio").
    pub host_api: Option<String>,
    /// Mode ("exclusive", "shared", "hog").
    pub mode: Option<String>,
    pub device_name: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub format: Option<String>,
    pub bit_depth: Option<u8>,
    pub buffer_frames: Option<u32>,
    pub latency_ms: Option<f32>,
    pub underruns: Option<u64>,
    /// Spectrum tap samples dropped because its reader fell behind.
    pub tap_drops: Option<u64>,
}

/// The path verdict, with its reasons.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    BitPerfect,
    /// DSP active, no resampling, no loss beyond quantisation.
    LosslessProcessed {
        reasons: Vec<String>,
    },
    Resampled {
        reasons: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct LiveDiagnostics {
    /// Output buffer fill, 0..=1.
    pub buffer_fill: Option<f32>,
    pub callback_cpu_avg_us: Option<u32>,
    pub callback_cpu_max_us: Option<u32>,
    pub clock_drift_ppm: Option<f32>,
}

/// One diagnostics snapshot (docs/ARCHITECTURE.md, "Audio diagnostics").
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct AudioDiagnostics {
    pub source: SourceDiagnostics,
    pub decode: DecodeDiagnostics,
    pub dsp: DspDiagnostics,
    pub output: OutputDiagnostics,
    pub verdict: Option<Verdict>,
    /// Shown beside the verdict: a lossy source can still reach the device untouched.
    pub lossy_source: Option<bool>,
    pub live: LiveDiagnostics,
}
