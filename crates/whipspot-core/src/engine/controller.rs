//! The control side: builds the rings, opens and closes the output, sets volume, balance and
//! EQ, reads the status.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rtrb::RingBuffer;

use super::controls::{Controls, DspParams};
use super::eq::EqSettings;
use super::error::{OpenFailed, OutputError, OutputFault};
use super::format::{
    MAX_RATE, OutputDevice, OutputInfo, OutputMode, OutputRequest, ReplayGainMode,
};
use super::monitor::EngineMonitor;
use super::output::AudioOutput;
use super::render::Renderer;
use super::shared::{CLOSED, FAULTED, OPEN, OPENING, RELEASED, Shared};
use super::source::SourceWriter;
use super::status::EngineStatus;
use super::tap::{PcmTap, tap_channel};

/// Mark ring capacity (marks are rare: track starts, flushes, ends, format acknowledgements).
const MARK_CAPACITY: usize = 1024;

/// Engine sizing.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Soft ring depth in milliseconds at the open device rate.
    pub ring_ms: u32,
    /// How long a blocked `write` waits without ring progress before `Err(Stalled)`.
    pub stall_timeout: Duration,
    /// Tap capacity in mono samples.
    pub tap_capacity: usize,
    /// Largest block the renderer processes at once (longer callbacks are split).
    pub max_block: usize,
    /// The tap's rate when the engine has no output adapter.
    pub external_tap_rate: u32,
    /// How long the source waits for an answer to an open request (I12).
    pub request_timeout: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            ring_ms: 250,
            stall_timeout: Duration::from_secs(2),
            tap_capacity: 16_384,
            max_block: 8_192,
            external_tap_rate: 44_100,
            request_timeout: Duration::from_secs(3),
        }
    }
}

/// What [`Engine::new`] returns: the engine (control), the source's writer and the tap reader.
pub struct EngineParts {
    pub engine: Engine,
    pub source: SourceWriter,
    pub tap: PcmTap,
}

/// What [`Engine::reopen`] did: the index and info of the request that opened, if any, and
/// the errors of the attempts that failed, in attempt order.
#[derive(Clone, Debug, PartialEq)]
pub struct Reopened {
    pub opened: Option<(usize, OutputInfo)>,
    pub errors: Vec<OutputError>,
}

/// The audio engine's control handle.
pub struct Engine {
    shared: Arc<Shared>,
    config: EngineConfig,
    capacity: usize,
    output: Option<Box<dyn AudioOutput>>,
    /// The renderer while no stream runs; `None` while open (or if an adapter lost it).
    renderer: Option<Renderer>,
    info: Option<OutputInfo>,
    monitor: EngineMonitor,
    controls: Controls,
}

impl Engine {
    /// Build the rings, the renderer and the writer. With `output: None` the tap writer is left
    /// for [`SourceWriter::take_tap`]; with an adapter the renderer feeds the tap.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: EngineConfig, output: Option<Box<dyn AudioOutput>>) -> EngineParts {
        let tap_drops = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(Shared::new(tap_drops.clone()));
        let capacity =
            ((u64::from(MAX_RATE) * u64::from(config.ring_ms.max(1))) / 1000).max(1) as usize;
        let (audio_p, audio_c) = RingBuffer::new(capacity);
        let (marks_p, marks_c) = RingBuffer::new(MARK_CAPACITY);
        let (tap_w, tap) = tap_channel(config.tap_capacity, config.external_tap_rate, tap_drops);
        let (status_in, status_out) = triple_buffer::triple_buffer(&EngineStatus::default());
        let (params_in, params_out) = triple_buffer::triple_buffer(&DspParams::default());
        let (render_tap, source_tap) = if output.is_some() {
            (Some(tap_w), None)
        } else {
            (None, Some(tap_w))
        };
        let renderer = Renderer::new(
            shared.clone(),
            audio_c,
            marks_c,
            config.max_block,
            render_tap,
            status_in,
            params_out,
        );
        let source = SourceWriter::new(
            shared.clone(),
            audio_p,
            marks_p,
            config.stall_timeout,
            config.request_timeout,
            source_tap,
        );
        EngineParts {
            engine: Engine {
                shared: shared.clone(),
                config,
                capacity,
                output,
                renderer: Some(renderer),
                info: None,
                monitor: EngineMonitor {
                    shared: shared.clone(),
                    status: Arc::new(Mutex::new(status_out)),
                    info: Arc::new(Mutex::new(None)),
                },
                controls: Controls::new(params_in),
            },
            source,
            tap,
        }
    }

    /// Open the output. Discards everything queued; the source acknowledges the new format at
    /// its next call (I7). Answers a pending open request.
    pub fn open(&mut self, request: &OutputRequest) -> Result<OutputInfo, OutputError> {
        let result = if self.output.is_none() {
            Err(OutputError::NoAdapter)
        } else if self.info.is_some() {
            Err(OutputError::Busy)
        } else {
            let result = self.attempt(request);
            if result.is_err() && self.shared.state() == OPENING {
                self.shared.set_state(CLOSED);
            }
            result
        };
        self.shared.answer_requests();
        result
    }

    /// Close the output (the renderer comes back with the ring intact). After a fault the
    /// source keeps failing with `OutputFailed` until the next open. Answers a pending open
    /// request.
    pub fn close(&mut self) {
        self.close_stream();
        if self.shared.state() != FAULTED {
            self.shared.set_state(CLOSED);
        }
        self.shared.answer_requests();
    }

    /// Close the stream (as [`Engine::close`]) and stay RELEASED: the source's next data asks
    /// for an open instead of failing (I13, I16). Answers pending open requests.
    pub fn release(&mut self) {
        self.close_stream();
        self.shared.set_state(RELEASED);
        self.shared.answer_requests();
    }

    /// Whether the output is released (see [`Engine::release`]).
    pub fn is_released(&self) -> bool {
        self.shared.state() == RELEASED
    }

    /// Close the open stream, if any, and open the first of `requests` that opens, trying them
    /// in order. The output stays OPENING from before the old stream closes until an attempt
    /// succeeds (OPEN) or all fail (CLOSED), so a blocked writer waits instead of failing; its
    /// queued data is dropped (I15). Answers pending open requests.
    pub fn reopen(&mut self, requests: &[OutputRequest]) -> Reopened {
        let mut reopened = Reopened {
            opened: None,
            errors: Vec::new(),
        };
        if self.output.is_none() {
            reopened.errors = vec![OutputError::NoAdapter; requests.len()];
            self.shared.answer_requests();
            return reopened;
        }
        self.shared.set_state(OPENING);
        self.close_stream();
        for (i, request) in requests.iter().enumerate() {
            match self.attempt(request) {
                Ok(info) => {
                    reopened.opened = Some((i, info));
                    break;
                }
                Err(e) => reopened.errors.push(e),
            }
        }
        if reopened.opened.is_none() {
            self.shared.set_state(CLOSED);
        }
        self.shared.answer_requests();
        reopened
    }

    /// Turn rate follow on or off (I14): with it on, a track at another rate than the device's
    /// asks for an open at its rate.
    pub fn set_follow_rate(&mut self, on: bool) {
        self.shared.follow_rate.store(on, Ordering::Release);
    }

    /// Set the function the source calls (on its own thread) after it raised an open request.
    /// The first call wins; later calls are ignored.
    pub fn set_request_hook(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.hook.set(hook);
    }

    /// The wanted rate (0 = any) of the newest unanswered open request.
    pub fn open_request(&self) -> Option<u32> {
        self.shared.open_request()
    }

    /// Answer every open request raised so far without opening (declines them).
    pub fn answer_request(&mut self) {
        self.shared.answer_requests();
    }

    /// The adapter's devices; `Err(NoAdapter)` without one.
    pub fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        match self.output.as_mut() {
            Some(output) => output.devices(),
            None => Err(OutputError::NoAdapter),
        }
    }

    /// The modes the adapter can open; empty without one.
    pub fn modes(&self) -> Vec<OutputMode> {
        self.output.as_ref().map_or_else(Vec::new, |o| o.modes())
    }

    /// One open attempt (M3's open body). On failure the state is left as it was set: OPENING
    /// once the adapter was asked, untouched when the renderer was lost.
    fn attempt(&mut self, request: &OutputRequest) -> Result<OutputInfo, OutputError> {
        let Some(output) = self.output.as_mut() else {
            return Err(OutputError::NoAdapter);
        };
        // An adapter that could not give the renderer back lost the ring for good.
        let Some(mut renderer) = self.renderer.take() else {
            return Err(OutputError::Failed);
        };
        let shared = &self.shared;
        shared.clear_fault();
        shared.set_state(OPENING);
        shared.format_epoch.fetch_add(1, Ordering::AcqRel);
        renderer.discard_readable();
        match output.open(request, renderer) {
            Ok(info) => {
                let depth = (u64::from(info.format.rate) * u64::from(self.config.ring_ms.max(1))
                    / 1000)
                    .clamp(1, self.capacity as u64) as u32;
                shared
                    .device_rate
                    .store(info.format.rate, Ordering::Release);
                shared.depth.store(depth, Ordering::Release);
                shared.set_state(OPEN);
                // The renderer bypasses parameters designed for another rate until these land.
                self.controls.set_rate(info.format.rate);
                self.info = Some(info.clone());
                self.set_monitor_info(Some(info.clone()));
                Ok(info)
            }
            Err(OpenFailed { error, renderer }) => {
                self.renderer = Some(renderer);
                Err(error)
            }
        }
    }

    /// Stop the open stream and take the renderer back; the output state is left alone.
    fn close_stream(&mut self) {
        if self.info.take().is_some() {
            self.set_monitor_info(None);
            if let Some(output) = self.output.as_mut() {
                self.renderer = output.close();
            }
        }
    }

    /// Volume 0..=100 (above 100 is 100); gain `(v/100)^2`, ramped over 10 ms.
    pub fn set_volume(&mut self, volume: u8) {
        self.controls.set_volume(&self.shared, volume);
    }

    /// Balance −100..=100 (clamped); the far channel falls linearly to 0.
    pub fn set_balance(&mut self, balance: i8) {
        self.controls.set_balance(&self.shared, balance);
    }

    /// Equaliser settings. Non-finite values are ignored as a whole; dB values are clamped to
    /// ±12. Settings persist across `open`.
    pub fn set_eq(&mut self, eq: EqSettings) {
        self.controls.set_eq(eq);
    }

    /// The ReplayGain mode (D8; default off). Each track's gain comes from its
    /// [`SourceInfo::replay_gain`](super::SourceInfo); a change ramps over 10 ms.
    pub fn set_replay_gain(&mut self, mode: ReplayGainMode) {
        self.controls.set_replay_gain(mode);
    }

    /// What the open output runs at; `None` while closed.
    pub fn output(&self) -> Option<&OutputInfo> {
        self.info.as_ref()
    }

    /// What the source wrote that is not heard yet (H25, D12): the frames between the writer
    /// and the renderer at the device rate, plus the latency the output reported (none when it
    /// reports none). `None` while no output is open. Lock-free: two atomic loads.
    pub fn queued(&self) -> Option<Duration> {
        if self.shared.state() != OPEN {
            return None;
        }
        let info = self.info.as_ref()?;
        // The renderer's position first: the writer's, read after it, is at least as new
        // except for a commit whose store is still on its way (clamped to nothing queued).
        let read = self.shared.read_pos.load(Ordering::Relaxed);
        let write = self.shared.write_pos.load(Ordering::Acquire);
        let frames = write.saturating_sub(read);
        let nanos = u128::from(frames) * 1_000_000_000 / u128::from(info.format.rate.max(1));
        let ring = Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
        Some(ring + self.output_latency())
    }

    /// The latency the open output reported; zero when it reports none or nothing is open.
    pub fn output_latency(&self) -> Duration {
        self.info
            .as_ref()
            .and_then(|i| i.latency_ms)
            .filter(|ms| ms.is_finite() && *ms > 0.0)
            .map_or(Duration::ZERO, |ms| {
                Duration::from_secs_f64(f64::from(ms.min(10_000.0)) / 1000.0)
            })
    }

    /// Device-rate frames the source has committed so far (its frame counter).
    pub fn written_frames(&self) -> u64 {
        self.shared.write_pos.load(Ordering::Acquire)
    }

    /// A reader of the renderer's frame counter (as of its last callback) for any thread:
    /// `None` while no output is open. Lock-free.
    pub fn render_probe(&self) -> impl Fn() -> Option<u64> + Send + Sync + 'static {
        let shared = self.shared.clone();
        move || (shared.state() == OPEN).then(|| shared.read_pos.load(Ordering::Relaxed))
    }

    /// A fault raised by the output since the last poll (each reported once); the faulted
    /// stream is closed.
    pub fn poll_fault(&mut self) -> Option<OutputFault> {
        let fault = self.shared.take_fault()?;
        self.close();
        Some(fault)
    }

    /// The renderer's last published status, with the source-side counters current.
    pub fn status(&self) -> EngineStatus {
        self.monitor.status()
    }

    /// A shareable read-only view: status and diagnostics (any thread).
    pub fn monitor(&self) -> EngineMonitor {
        self.monitor.clone()
    }

    fn set_monitor_info(&self, info: Option<OutputInfo>) {
        *self.monitor.info.lock().unwrap_or_else(|e| e.into_inner()) = info;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close();
        self.shared.set_state(CLOSED);
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{FaultSignal, TapWriter};

    #[test]
    fn thread_bounds() {
        fn send<T: Send>() {}
        fn send_sync<T: Send + Sync>() {}
        send::<Engine>();
        send::<Renderer>();
        send::<TapWriter>();
        send::<PcmTap>();
        send_sync::<SourceWriter>();
        send_sync::<crate::engine::EngineMonitor>();
        send_sync::<FaultSignal>();
    }

    #[test]
    fn non_finite_eq_is_ignored_as_a_whole() {
        let mut engine = Engine::new(EngineConfig::default(), None).engine;
        let mut eq = EqSettings::default();
        eq.bands_db[2] = 20.0;
        engine.set_eq(eq);
        assert_eq!(engine.controls.eq.bands_db[2], 12.0);
        let mut bad = EqSettings {
            preamp_db: f32::NAN,
            ..EqSettings::default()
        };
        bad.bands_db[0] = 3.0;
        engine.set_eq(bad);
        assert_eq!(engine.controls.eq.bands_db[2], 12.0);
        assert_eq!(engine.controls.eq.bands_db[0], 0.0);
    }

    #[test]
    fn no_adapter_and_dropped_engine() {
        let parts = Engine::new(EngineConfig::default(), None);
        let (mut engine, source) = (parts.engine, parts.source);
        assert_eq!(
            engine.open(&OutputRequest::default()),
            Err(OutputError::NoAdapter)
        );
        assert!(source.take_tap().is_some());
        assert!(source.take_tap().is_none());
        assert_eq!(source.gains(), (1.0, 1.0));
        engine.set_volume(150);
        assert_eq!(source.gains(), (1.0, 1.0));
        engine.set_volume(50);
        engine.set_balance(-120);
        assert_eq!(source.gains(), (0.25, 0.0));
        assert_eq!(engine.queued(), None);
        drop(engine);
        assert_eq!(
            source.write(&[0.0, 0.0]),
            Err(crate::engine::SourceError::NoOutput)
        );
    }
}
