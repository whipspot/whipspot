//! Shared by the measurement tests: the instruments and the engine rigs. Each test crate that
//! includes this uses a part of it, so unused items are expected.
#![allow(dead_code)]

pub mod measure;

use std::f64::consts::PI;

use whipspot_core::engine::{
    DeviceFormat, Engine, EngineConfig, OutputMode, OutputRequest, SampleFormat, SourceInfo,
    SourceWriter, StreamFormat,
};
use whipspot_testkit::FakeOutput;

pub fn device(rate: u32, sample: SampleFormat, valid_bits: u8) -> DeviceFormat {
    DeviceFormat {
        rate,
        channels: 2,
        sample,
        valid_bits,
    }
}

pub fn format(rate: u32) -> StreamFormat {
    StreamFormat {
        rate,
        channels: 2,
        source: SourceInfo {
            codec: "vorbis",
            bits: None,
            bitrate_kbps: None,
            lossless: false,
            replay_gain: None,
        },
    }
}

/// An engine on an opened fake output at `dev`, a lossy track at `src_rate` started.
pub fn rig(
    src_rate: u32,
    dev: DeviceFormat,
    mode: OutputMode,
) -> (Engine, SourceWriter, FakeOutput) {
    let fake = FakeOutput::new(dev, mode);
    let parts = Engine::new(EngineConfig::default(), Some(Box::new(fake.clone())));
    let mut engine = parts.engine;
    engine.open(&OutputRequest::default()).unwrap();
    parts.source.start_track(format(src_rate)).unwrap();
    (engine, parts.source, fake)
}

/// `sum amp_k sin(2 pi f_k n / rate)` as interleaved stereo (both channels equal).
pub fn tones(tones: &[(f64, f64)], rate: u32, frames: usize) -> Vec<f64> {
    (0..frames)
        .flat_map(|i| {
            let v: f64 = tones
                .iter()
                .map(|&(f, a)| a * (2.0 * PI * f * i as f64 / f64::from(rate)).sin())
                .sum();
            [v, v]
        })
        .collect()
}

/// Frames written per round by `resample`.
const CHUNK: usize = 1_000;

/// Play `input` (interleaved stereo at `src_rate`) through the engine into a shared `F32`
/// fake at `dev_rate`, rendering while writing so the ring never runs short, then drain.
/// Returns the left channel of everything rendered, as `f64`, and the frame count; the
/// resampler is the engine's own.
pub fn resample(src_rate: u32, dev_rate: u32, input: &[f64]) -> Vec<f64> {
    let (engine, src, fake) = rig(
        src_rate,
        device(dev_rate, SampleFormat::F32, 32),
        OutputMode::Shared,
    );
    let frames = input.len() / 2;
    let mut out: Vec<f32> = Vec::new();
    let (mut rendered, mut written) = (0usize, 0usize);
    while written < frames {
        let next = (written + CHUNK).min(frames);
        src.write(&input[2 * written..2 * next]).unwrap();
        written = next;
        // Stay behind what the producer has pushed by the resampler's staging (one input chunk
        // plus this write) and a margin, at the device rate.
        let lag =
            ((1_024 + CHUNK) as u64 * u64::from(dev_rate) / u64::from(src_rate)) as usize + 1_000;
        let target = (written as u64 * u64::from(dev_rate) / u64::from(src_rate)) as usize;
        let target = target.saturating_sub(lag);
        if target > rendered {
            out.extend(fake.render_f32(target - rendered));
            rendered = target;
        }
    }
    src.finish();
    fake.render_f32(0); // publish the ring fill
    let queued = engine.status().ring_fill as usize;
    out.extend(fake.render_f32(queued));
    assert_eq!(engine.status().underruns, 0, "{src_rate} -> {dev_rate}");
    out.chunks_exact(2).map(|f| f64::from(f[0])).collect()
}

/// `x` without its first and last `ms` milliseconds at `rate` (filter warm-up and drain).
pub fn trim(x: &[f64], rate: u32, ms: usize) -> &[f64] {
    let k = rate as usize * ms / 1_000;
    &x[k..x.len() - k]
}
