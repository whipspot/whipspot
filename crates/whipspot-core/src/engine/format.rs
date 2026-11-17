//! Stream and device formats, output requests and what an opened output reports.

/// What the source honestly knows about its audio before decoding (shown to the user as is).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceInfo {
    /// Codec name, e.g. `"vorbis"`, `"flac"`, `"pcm"`.
    pub codec: &'static str,
    /// Integer depth of an integer source (16, 24, 32); `None` for decoder float output.
    pub bits: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub lossless: bool,
    /// The track's ReplayGain tags, when it has any (D8).
    pub replay_gain: Option<ReplayGain>,
}

/// A track's ReplayGain tags (D8): gains in dB and linear sample peaks, each optional. Tags are
/// untrusted input: a non-finite gain or a peak that is not finite and positive counts as absent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayGain {
    pub track_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_db: Option<f64>,
    pub album_peak: Option<f64>,
}

/// Which ReplayGain value the engine applies (D8). Off by default: the stage is bypassed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    /// The album gain, falling back to the track gain.
    Album,
}

/// The range a ReplayGain tag's dB value is clamped to (a hostile tag never asks for more).
const REPLAY_GAIN_MIN_DB: f64 = -60.0;
const REPLAY_GAIN_MAX_DB: f64 = 24.0;

impl ReplayGain {
    /// The linear gain `mode` applies to a track with these tags; `None` when the stage is
    /// bypassed (mode off, no usable gain, or a gain of exactly 1). Album falls back to track.
    /// A positive gain is lowered so that peak × gain ≤ 1 when the peak is known, never below 1.
    pub fn linear(&self, mode: ReplayGainMode) -> Option<f64> {
        let finite = |v: Option<f64>| v.filter(|x| x.is_finite());
        let peak = |v: Option<f64>| v.filter(|x| x.is_finite() && *x > 0.0);
        let track = finite(self.track_db).map(|db| (db, peak(self.track_peak)));
        let album = finite(self.album_db).map(|db| (db, peak(self.album_peak)));
        let (db, pk) = match mode {
            ReplayGainMode::Off => return None,
            ReplayGainMode::Track => track?,
            ReplayGainMode::Album => album.or(track)?,
        };
        let mut g = 10f64.powf(db.clamp(REPLAY_GAIN_MIN_DB, REPLAY_GAIN_MAX_DB) / 20.0);
        if let Some(pk) = pk {
            if g > 1.0 {
                g = g.min(1.0 / pk).max(1.0);
            }
        }
        (g != 1.0).then_some(g)
    }
}

/// The format of the samples a source writes: rate, channel count and their origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamFormat {
    pub rate: u32,
    pub channels: u16,
    pub source: SourceInfo,
}

/// Sample type of an output device buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    I32,
}

/// The format an output stream runs at.
///
/// `valid_bits`: F32 → 32, I16 → 16, I32 → 24 or 32 (24 means left-justified in 32 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
    pub sample: SampleFormat,
    pub valid_bits: u8,
}

/// Lowest and highest rate the engine accepts, for sources and devices alike.
pub(crate) const MIN_RATE: u32 = 8_000;
pub(crate) const MAX_RATE: u32 = 384_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(track: Option<(f64, f64)>, album: Option<(f64, f64)>) -> ReplayGain {
        ReplayGain {
            track_db: track.map(|t| t.0),
            track_peak: track.map(|t| t.1).filter(|p| *p != 0.0),
            album_db: album.map(|a| a.0),
            album_peak: album.map(|a| a.1).filter(|p| *p != 0.0),
        }
    }

    #[test]
    fn replay_gain_modes_and_fallback() {
        let t = tags(Some((-3.0, 0.0)), Some((-5.0, 0.0)));
        assert_eq!(t.linear(ReplayGainMode::Off), None);
        assert_eq!(
            t.linear(ReplayGainMode::Track),
            Some(10f64.powf(-3.0 / 20.0))
        );
        assert_eq!(
            t.linear(ReplayGainMode::Album),
            Some(10f64.powf(-5.0 / 20.0))
        );
        // Album without an album gain: the track gain.
        let t = tags(Some((-3.0, 0.0)), None);
        assert_eq!(
            t.linear(ReplayGainMode::Album),
            Some(10f64.powf(-3.0 / 20.0))
        );
        // Neither: bypassed, in both modes.
        let t = tags(None, None);
        assert_eq!(t.linear(ReplayGainMode::Album), None);
        assert_eq!(t.linear(ReplayGainMode::Track), None);
        // 0 dB is unity: bypassed.
        assert_eq!(
            tags(Some((0.0, 0.0)), None).linear(ReplayGainMode::Track),
            None
        );
    }

    #[test]
    fn positive_replay_gain_respects_the_peak() {
        // +6 dB with peak 0.8: min(10^(6/20), 1/0.8).
        let t = tags(Some((6.0, 0.8)), None);
        let want = 10f64.powf(6.0 / 20.0).min(1.0 / 0.8);
        assert_eq!(t.linear(ReplayGainMode::Track), Some(want));
        assert_eq!(want, 1.25);
        // +6 dB with peak 0.3: the whole gain fits.
        let t = tags(Some((6.0, 0.3)), None);
        assert_eq!(
            t.linear(ReplayGainMode::Track),
            Some(10f64.powf(6.0 / 20.0))
        );
        // A positive gain on a peak above 1 never turns into a cut: bypassed.
        assert_eq!(
            tags(Some((3.0, 1.2)), None).linear(ReplayGainMode::Track),
            None
        );
        // A negative gain ignores the peak.
        let t = tags(Some((-6.0, 1.2)), None);
        assert_eq!(
            t.linear(ReplayGainMode::Track),
            Some(10f64.powf(-6.0 / 20.0))
        );
        // No peak: the positive gain applies (the limiter guards it).
        let t = tags(Some((6.0, 0.0)), None);
        assert_eq!(
            t.linear(ReplayGainMode::Track),
            Some(10f64.powf(6.0 / 20.0))
        );
    }

    #[test]
    fn hostile_replay_gain_tags_are_bounded() {
        let bad = |db: f64, peak: f64| ReplayGain {
            track_db: Some(db),
            track_peak: Some(peak),
            ..ReplayGain::default()
        };
        assert_eq!(bad(f64::NAN, 0.5).linear(ReplayGainMode::Track), None);
        assert_eq!(bad(f64::INFINITY, 0.5).linear(ReplayGainMode::Track), None);
        // A bad peak counts as absent; a huge gain is clamped to +24 dB.
        let g = bad(1e9, f64::NAN).linear(ReplayGainMode::Track).unwrap();
        assert_eq!(g, 10f64.powf(24.0 / 20.0));
        let g = bad(-1e9, -1.0).linear(ReplayGainMode::Track).unwrap();
        assert_eq!(g, 10f64.powf(-60.0 / 20.0));
    }
}

impl DeviceFormat {
    /// Whether `valid_bits` fits the sample type (see the type's docs).
    pub(crate) fn bits_consistent(&self) -> bool {
        matches!(
            (self.sample, self.valid_bits),
            (SampleFormat::F32, 32) | (SampleFormat::I16, 16) | (SampleFormat::I32, 24 | 32)
        )
    }
}

/// Shared (the OS mixes) or exclusive (the engine owns the device) output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    #[default]
    Shared,
    Exclusive,
}

/// What to open. The default is the default device, shared, at the device's own rate and format.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputRequest {
    /// An [`OutputDevice::id`]; `None` for the system default.
    pub device: Option<String>,
    pub mode: OutputMode,
    /// A rate to ask for; `None` for the device's own.
    pub rate: Option<u32>,
    /// Acceptable sample formats in order of preference; empty for the device's own.
    pub formats: Vec<SampleFormat>,
}

/// What an opened output actually runs at.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    pub host_api: String,
    pub device: String,
    pub mode: OutputMode,
    pub format: DeviceFormat,
    pub buffer_frames: Option<u32>,
    pub latency_ms: Option<f32>,
}

/// One output device an adapter can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}
