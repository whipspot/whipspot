//! cpal shared-mode output (Windows WASAPI, macOS CoreAudio) for the engine's [`Renderer`].
//!
//! The stream runs at exactly the device's default (mix) format; the engine resamples to it.
//! A cpal `Stream` is `!Send` on macOS, so it is created, played and dropped on one
//! adapter-owned thread; the renderer lives in the data callback and comes back through a
//! drop guard when the stream is torn down.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BuildStreamError, DefaultStreamConfigError, Device, OutputCallbackInfo, PlayStreamError,
    Sample, Stream, StreamError,
};
use whipspot_core::engine::{
    AudioOutput, DeviceFormat, FaultSignal, OpenFailed, OutBuf, OutputDevice, OutputError,
    OutputFault, OutputInfo, OutputMode, OutputRequest, RenderTiming, Renderer, SampleFormat,
};

/// How long `close` waits for the renderer to come back from the audio thread.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// The cpal shared-mode output. Opens nothing and starts no thread until `open`.
pub struct CpalOutput {
    open: Option<OpenStream>,
}

/// Handles to a running stream's owner thread (all `Send`).
struct OpenStream {
    /// Dropping it tells the owner thread to drop the stream.
    stop: Sender<()>,
    back: Receiver<Renderer>,
    thread: JoinHandle<()>,
}

impl CpalOutput {
    pub fn new() -> Self {
        Self { open: None }
    }
}

impl Default for CpalOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput for CpalOutput {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        let host = cpal::default_host();
        let default = host.default_output_device().and_then(|d| d.name().ok());
        let devices = host.output_devices().map_err(|_| OutputError::Failed)?;
        Ok(devices
            .filter_map(|d| d.name().ok())
            .map(|name| OutputDevice {
                is_default: default.as_deref() == Some(name.as_str()),
                id: name.clone(),
                name,
            })
            .collect())
    }

    #[allow(clippy::result_large_err)]
    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        if self.open.is_some() {
            return Err(OpenFailed {
                error: OutputError::Busy,
                renderer,
            });
        }
        if request.mode == OutputMode::Exclusive {
            return Err(OpenFailed {
                error: OutputError::Unsupported,
                renderer,
            });
        }

        let (back_tx, back_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let guard = Guard::new(renderer, back_tx);
        let device = request.device.clone();
        let spawned = thread::Builder::new()
            .name("whipspot-cpal-out".into())
            .spawn(move || owner_main(device, guard, ready_tx, stop_rx));
        let thread = match spawned {
            Ok(thread) => thread,
            // The closure (and its guard) was dropped, which sent the renderer back.
            Err(_) => return Err(failed(&back_rx, OutputError::Failed)),
        };

        match ready_rx.recv() {
            Ok(Ok(info)) => {
                self.open = Some(OpenStream {
                    stop: stop_tx,
                    back: back_rx,
                    thread,
                });
                Ok(info)
            }
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(failed(&back_rx, error))
            }
            Err(_) => {
                let _ = thread.join();
                Err(failed(&back_rx, OutputError::Failed))
            }
        }
    }

    fn close(&mut self) -> Option<Renderer> {
        let OpenStream { stop, back, thread } = self.open.take()?;
        drop(stop);
        let renderer = back.recv_timeout(CLOSE_WAIT).ok();
        if renderer.is_some() {
            // The stream is gone; the owner thread is finishing.
            let _ = thread.join();
        }
        renderer
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        // Dropping the stop sender makes the owner thread drop the stream and end; never
        // block a drop on it.
        self.open.take();
    }
}

/// Build the `OpenFailed` for a failure after the renderer went to the owner thread. The
/// guard sends the renderer back on every failure path before the thread ends.
#[allow(clippy::result_large_err)]
fn failed(back: &Receiver<Renderer>, error: OutputError) -> OpenFailed {
    let renderer = back
        .recv()
        .expect("the guard returns the renderer on every open failure path");
    OpenFailed { error, renderer }
}

/// Owns the renderer inside the callback closure; sends it back when the closure is dropped
/// (stream torn down, build failed, or the thread never started).
struct Guard {
    renderer: Option<Renderer>,
    back: Sender<Renderer>,
}

impl Guard {
    fn new(renderer: Renderer, back: Sender<Renderer>) -> Self {
        Self {
            renderer: Some(renderer),
            back,
        }
    }

    fn fault_signal(&self) -> Option<FaultSignal> {
        self.renderer.as_ref().map(Renderer::fault_signal)
    }

    fn prepare(&mut self, format: DeviceFormat) -> Result<(), OutputError> {
        match self.renderer.as_mut() {
            Some(r) => r.prepare(format),
            None => Err(OutputError::Failed),
        }
    }

    /// Audio-thread entry: run `f` on the renderer with the callback's timing; `false` when
    /// the renderer is gone (the caller writes silence).
    fn with(
        &mut self,
        info: &OutputCallbackInfo,
        f: impl FnOnce(&mut Renderer, RenderTiming),
    ) -> bool {
        let Some(r) = self.renderer.as_mut() else {
            return false;
        };
        let ts = info.timestamp();
        let latency_ns = ts
            .playback
            .duration_since(&ts.callback)
            .and_then(|d| u64::try_from(d.as_nanos()).ok());
        f(r, RenderTiming { latency_ns });
        true
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(renderer) = self.renderer.take() {
            let _ = self.back.send(renderer);
        }
    }
}

/// The owner thread: build and play the stream, report, hold it until `stop` closes, drop it.
fn owner_main(
    device: Option<String>,
    guard: Guard,
    ready: Sender<Result<OutputInfo, OutputError>>,
    stop: Receiver<()>,
) {
    // `build` consumes the guard; on failure it is dropped there, returning the renderer
    // before the error is reported.
    match build(device.as_deref(), guard) {
        Ok((stream, info)) => {
            let _ = ready.send(Ok(info));
            // Blocks until the adapter's `close` or drop drops the sender.
            let _ = stop.recv();
            // Joins cpal's audio thread; the callback closure (and its guard) drops there.
            drop(stream);
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    }
}

fn build(device: Option<&str>, mut guard: Guard) -> Result<(Stream, OutputInfo), OutputError> {
    let host = cpal::default_host();
    let device = find_device(&host, device)?;
    let supported = device.default_output_config().map_err(map_default_config)?;
    let sample = supported.sample_format();
    let config = supported.config();
    let format = device_format(config.sample_rate.0, config.channels, sample)
        .ok_or(OutputError::Unsupported)?;
    let fault = guard.fault_signal().ok_or(OutputError::Failed)?;
    guard.prepare(format)?;

    let on_error = move |e: StreamError| fault.raise(map_stream_error(&e));
    let stream = match format.sample {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], info: &OutputCallbackInfo| {
                if !guard.with(info, |r, t| r.render(OutBuf::F32(data), t)) {
                    data.fill(Sample::EQUILIBRIUM);
                }
            },
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], info: &OutputCallbackInfo| {
                if !guard.with(info, |r, t| r.render(OutBuf::I16(data), t)) {
                    data.fill(Sample::EQUILIBRIUM);
                }
            },
            on_error,
            None,
        ),
        SampleFormat::I32 => device.build_output_stream(
            &config,
            move |data: &mut [i32], info: &OutputCallbackInfo| {
                if !guard.with(info, |r, t| r.render(OutBuf::I32(data), t)) {
                    data.fill(Sample::EQUILIBRIUM);
                }
            },
            on_error,
            None,
        ),
    }
    .map_err(map_build)?;
    stream.play().map_err(map_play)?;

    let info = OutputInfo {
        host_api: host_label().to_owned(),
        device: device
            .name()
            .unwrap_or_else(|_| "unknown device".to_owned()),
        mode: OutputMode::Shared,
        format,
        buffer_frames: None,
        latency_ms: None,
    };
    Ok((stream, info))
}

fn find_device(host: &cpal::Host, name: Option<&str>) -> Result<Device, OutputError> {
    match name {
        None => host.default_output_device().ok_or(OutputError::NoDevice),
        Some(name) => host
            .output_devices()
            .map_err(|_| OutputError::Failed)?
            .find(|d| d.name().is_ok_and(|n| n == name))
            .ok_or(OutputError::NoDevice),
    }
}

/// The host label shown in diagnostics.
fn host_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "WASAPI shared (cpal)"
    } else if cfg!(target_os = "macos") {
        "CoreAudio shared (cpal)"
    } else {
        "cpal shared"
    }
}

/// A cpal sample format as the engine's device format; `None` for formats the engine
/// does not render to.
fn device_format(rate: u32, channels: u16, sample: cpal::SampleFormat) -> Option<DeviceFormat> {
    let (sample, valid_bits) = match sample {
        cpal::SampleFormat::F32 => (SampleFormat::F32, 32),
        cpal::SampleFormat::I16 => (SampleFormat::I16, 16),
        cpal::SampleFormat::I32 => (SampleFormat::I32, 32),
        _ => return None,
    };
    Some(DeviceFormat {
        rate,
        channels,
        sample,
        valid_bits,
    })
}

fn map_default_config(e: DefaultStreamConfigError) -> OutputError {
    match e {
        DefaultStreamConfigError::DeviceNotAvailable => OutputError::NoDevice,
        DefaultStreamConfigError::StreamTypeNotSupported => OutputError::Unsupported,
        _ => OutputError::Failed,
    }
}

fn map_build(e: BuildStreamError) -> OutputError {
    match e {
        BuildStreamError::DeviceNotAvailable => OutputError::NoDevice,
        BuildStreamError::StreamConfigNotSupported => OutputError::Unsupported,
        _ => OutputError::Failed,
    }
}

fn map_play(e: PlayStreamError) -> OutputError {
    match e {
        PlayStreamError::DeviceNotAvailable => OutputError::NoDevice,
        _ => OutputError::Failed,
    }
}

fn map_stream_error(e: &StreamError) -> OutputFault {
    match e {
        StreamError::DeviceNotAvailable => OutputFault::DeviceLost,
        _ => OutputFault::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_output_is_send() {
        fn is_send<T: Send>() {}
        is_send::<CpalOutput>();
    }

    #[test]
    fn new_opens_nothing() {
        assert!(CpalOutput::new().open.is_none());
    }

    #[test]
    fn format_mapping() {
        let f = |s| device_format(48_000, 2, s);
        assert_eq!(
            f(cpal::SampleFormat::F32),
            Some(DeviceFormat {
                rate: 48_000,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32
            })
        );
        assert_eq!(
            f(cpal::SampleFormat::I16).map(|d| (d.sample, d.valid_bits)),
            Some((SampleFormat::I16, 16))
        );
        assert_eq!(
            f(cpal::SampleFormat::I32).map(|d| (d.sample, d.valid_bits)),
            Some((SampleFormat::I32, 32))
        );
        for s in [
            cpal::SampleFormat::I8,
            cpal::SampleFormat::U8,
            cpal::SampleFormat::U16,
            cpal::SampleFormat::I64,
            cpal::SampleFormat::F64,
        ] {
            assert_eq!(f(s), None, "{s:?}");
        }
    }

    #[test]
    fn host_label_per_os() {
        let label = host_label();
        if cfg!(target_os = "windows") {
            assert_eq!(label, "WASAPI shared (cpal)");
        } else if cfg!(target_os = "macos") {
            assert_eq!(label, "CoreAudio shared (cpal)");
        } else {
            assert_eq!(label, "cpal shared");
        }
    }

    #[test]
    fn stream_error_mapping() {
        assert_eq!(
            map_stream_error(&StreamError::DeviceNotAvailable),
            OutputFault::DeviceLost
        );
        let other = StreamError::BackendSpecific {
            err: cpal::BackendSpecificError {
                description: String::new(),
            },
        };
        assert_eq!(map_stream_error(&other), OutputFault::Failed);
    }
}
