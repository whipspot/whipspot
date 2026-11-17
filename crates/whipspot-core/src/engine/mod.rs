//! The audio engine (D36, MD22–MD23): a lock-free pull engine between a provider's source
//! thread and an output adapter's real-time callback.
//!
//! - [`SourceWriter`]: the source writes interleaved `f64` samples, track starts, flushes and
//!   ends; it may block (bounded) when the ring is full.
//! - [`Renderer`]: the real-time consumer; the output adapter owns it while a stream runs and
//!   calls [`Renderer::render`] from its callback.
//! - [`Engine`]: the control handle that opens and closes the output through an
//!   [`AudioOutput`] adapter, sets volume, balance and EQ, and reads the [`EngineStatus`].
//!
//! The audio ring holds stereo frames at the device rate; a second ring carries `Copy` marks
//! positioned by the producer's frame counter. Interrupts and reopens are acknowledged in band
//! (see `shared`).

mod clock;
mod controller;
mod controls;
mod eq;
mod error;
mod format;
mod gain;
mod limiter;
mod monitor;
mod output;
mod quantise;
mod render;
mod resample;
mod shared;
mod source;
mod status;
mod tap;
mod verdict;

pub use clock::DriftFit;
pub use controller::{Engine, EngineConfig, EngineParts, Reopened};
pub use eq::{BAND_COUNT, BAND_HZ, EqSettings};
pub use error::{OpenFailed, OutputError, OutputFault, SourceError};
pub use format::{
    DeviceFormat, OutputDevice, OutputInfo, OutputMode, OutputRequest, ReplayGain, ReplayGainMode,
    SampleFormat, SourceInfo, StreamFormat,
};
pub use monitor::EngineMonitor;
pub(crate) use monitor::format_text;
pub use output::{AudioOutput, OutBuf, RenderTiming};
pub use render::Renderer;
pub use shared::FaultSignal;
pub use source::{OwnerSwitch, PACE_CAP, SourceWriter};
pub use status::{EngineStatus, StageFlags, TrackInfo};
pub use tap::{PcmTap, TapWriter};
pub use verdict::verdict;
