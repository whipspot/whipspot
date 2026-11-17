//! A read-only, shareable view of the engine (MD34): the renderer's last status and the
//! diagnostics snapshot built from it.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::diagnostics::{AudioDiagnostics, Dither, Resampler, Stage};

use super::format::{OutputInfo, OutputMode, SampleFormat};
use super::resample::{ALGORITHM, QUALITY};
use super::shared::Shared;
use super::status::EngineStatus;
use super::verdict::verdict;

/// What [`super::Engine::monitor`] returns. Cheap to clone; any thread.
#[derive(Clone)]
pub struct EngineMonitor {
    pub(crate) shared: Arc<Shared>,
    pub(crate) status: Arc<Mutex<triple_buffer::Output<EngineStatus>>>,
    pub(crate) info: Arc<Mutex<Option<OutputInfo>>>,
}

impl std::fmt::Debug for EngineMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EngineMonitor")
    }
}

fn stage(on: bool) -> Option<Stage> {
    Some(if on { Stage::Active } else { Stage::Bypassed })
}

pub(crate) fn format_text(sample: SampleFormat, valid_bits: u8) -> &'static str {
    match (sample, valid_bits) {
        (SampleFormat::F32, _) => "32-bit float",
        (SampleFormat::I16, _) => "16-bit",
        (SampleFormat::I32, 24) => "24-bit in 32",
        (SampleFormat::I32, _) => "32-bit",
    }
}

impl EngineMonitor {
    /// The renderer's last published status, with the source-side counters current.
    pub fn status(&self) -> EngineStatus {
        let mut s = *self.status.lock().unwrap_or_else(|e| e.into_inner()).read();
        s.source_frames = self.shared.frames_written.load(Ordering::Relaxed);
        s.tap_drops = self.shared.tap_drops.load(Ordering::Relaxed);
        s.paused = self.shared.paused.load(Ordering::Relaxed);
        s.ring_depth = self.shared.depth.load(Ordering::Relaxed);
        s
    }

    /// The diagnostics snapshot; all unknown while no output is open.
    pub fn diagnostics(&self) -> AudioDiagnostics {
        let Some(info) = self.info.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return AudioDiagnostics::default();
        };
        let s = self.status();
        let mut d = AudioDiagnostics::default();

        if let Some(t) = s.track {
            let f = t.format;
            d.source.codec = Some(f.source.codec.to_owned());
            d.source.sample_rate_hz = Some(f.rate);
            d.source.bit_depth = f.source.bits;
            d.source.channels = u8::try_from(f.channels).ok();
            d.source.lossless = Some(f.source.lossless);
            d.source.bitrate_nominal_kbps = f.source.bitrate_kbps;
            d.dsp.resampler = Some(if t.resampled {
                Resampler::Active {
                    algorithm: ALGORITHM.to_owned(),
                    ratio: f64::from(info.format.rate) / f64::from(f.rate),
                    quality: QUALITY.to_owned(),
                }
            } else {
                Resampler::None
            });
        }
        d.decode.frames_decoded = Some(s.source_frames);

        let a = s.applied;
        d.dsp.eq = stage(a.eq);
        d.dsp.preamp = stage(a.pre_gain);
        d.dsp.replay_gain = stage(a.replay_gain);
        d.dsp.replay_gain_db = a.replay_gain.then_some(s.replay_gain_db);
        d.dsp.volume = stage(a.volume);
        d.dsp.balance = stage(a.balance);
        d.dsp.headroom_db = Some(s.headroom_db);
        d.dsp.peak_dbfs = (s.peak > 0.0).then(|| (20.0 * s.peak.log10()) as f32);
        d.dsp.clip_count = Some(s.clips);
        d.dsp.limiter = stage(a.limiter);
        d.dsp.limiter_frames = Some(s.limiter_frames);
        d.dsp.dither = Some(if a.dither {
            Dither::Active("TPDF".to_owned())
        } else {
            Dither::None
        });

        let o = &mut d.output;
        o.host_api = Some(info.host_api.clone());
        o.mode = Some(
            match info.mode {
                OutputMode::Shared => "shared",
                OutputMode::Exclusive => "exclusive",
            }
            .to_owned(),
        );
        o.device_name = Some(info.device.clone());
        o.sample_rate_hz = Some(info.format.rate);
        o.format = Some(format_text(info.format.sample, info.format.valid_bits).to_owned());
        o.bit_depth = Some(info.format.valid_bits);
        o.buffer_frames = (s.callbacks > 0).then_some(s.block_frames);
        o.latency_ms = s.latency_ns.map(|ns| ns as f32 / 1e6);
        o.underruns = Some(s.underruns);
        o.tap_drops = Some(s.tap_drops);

        if s.ring_depth > 0 {
            d.live.buffer_fill = Some((s.ring_fill as f32 / s.ring_depth as f32).clamp(0.0, 1.0));
        }
        if s.callbacks > 0 {
            d.live.callback_cpu_avg_us =
                Some(u32::try_from(s.cpu_avg_ns / 1000).unwrap_or(u32::MAX));
            d.live.callback_cpu_max_us =
                Some(u32::try_from(s.cpu_max_ns / 1000).unwrap_or(u32::MAX));
            d.live.clock_drift_ppm = s.clock.drift_ppm(info.format.rate);
        }

        let (v, lossy) = verdict(&info, &s);
        d.verdict = v;
        d.lossy_source = lossy;
        d
    }
}
