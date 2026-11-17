//! Fake audio outputs: [`FakeOutput`], whose "callback" is driven by the test thread (clones
//! share state, so a test keeps one clone and hands the engine another), and [`ClockedOutput`],
//! which drives the callback on its own thread at real-time pace. No device is opened.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use whipspot_core::engine::{
    AudioOutput, DeviceFormat, FaultSignal, OpenFailed, OutBuf, OutputDevice, OutputError,
    OutputFault, OutputInfo, OutputMode, OutputRequest, RenderTiming, Renderer, SampleFormat,
};

#[derive(Clone)]
pub struct FakeOutput {
    state: Arc<Mutex<State>>,
}

struct State {
    format: DeviceFormat,
    mode: OutputMode,
    fail_open: bool,
    renderer: Option<Renderer>,
    signal: Option<FaultSignal>,
    rates: Vec<u32>,
    fail_modes: Vec<(OutputMode, OutputError)>,
    fail_devices: Vec<String>,
    report_request_mode: bool,
    modes: Vec<OutputMode>,
    devices: Option<Vec<OutputDevice>>,
    requests: Vec<OutputRequest>,
}

impl FakeOutput {
    /// An output that opens at exactly `format` in `mode`.
    pub fn new(format: DeviceFormat, mode: OutputMode) -> Self {
        FakeOutput {
            state: Arc::new(Mutex::new(State {
                format,
                mode,
                fail_open: false,
                renderer: None,
                signal: None,
                rates: Vec::new(),
                fail_modes: Vec::new(),
                fail_devices: Vec::new(),
                report_request_mode: false,
                modes: vec![mode],
                devices: None,
                requests: Vec::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make the next opens fail with `OutputError::Failed`.
    pub fn fail_open(&self, fail: bool) {
        self.lock().fail_open = fail;
    }

    /// Rates an open may run at: a request whose `rate` is one of them opens at it, any other
    /// at the constructed format. Empty (the default): always the constructed format.
    pub fn set_rates(&self, rates: &[u32]) {
        self.lock().rates = rates.to_vec();
    }

    /// Make opens requesting `mode` fail with `error` (`None` lifts it).
    pub fn fail_mode(&self, mode: OutputMode, error: Option<OutputError>) {
        let mut st = self.lock();
        st.fail_modes.retain(|(m, _)| *m != mode);
        if let Some(e) = error {
            st.fail_modes.push((mode, e));
        }
    }

    /// Make opens requesting the device `id` fail with `NoDevice`.
    pub fn fail_device(&self, id: &str) {
        self.lock().fail_devices.push(id.to_owned());
    }

    /// Report the request's mode in `OutputInfo.mode` instead of the constructed one.
    pub fn report_request_mode(&self, on: bool) {
        self.lock().report_request_mode = on;
    }

    /// What `modes()` returns (default: the constructed mode).
    pub fn set_modes(&self, modes: Vec<OutputMode>) {
        self.lock().modes = modes;
    }

    /// What `devices()` returns (default: one device "fake").
    pub fn set_devices(&self, devices: Vec<OutputDevice>) {
        self.lock().devices = Some(devices);
    }

    /// Every `open` call's request, in order.
    pub fn requests(&self) -> Vec<OutputRequest> {
        self.lock().requests.clone()
    }

    /// Whether a stream is open (the fake holds the renderer).
    pub fn is_open(&self) -> bool {
        self.lock().renderer.is_some()
    }

    fn channels(&self) -> usize {
        usize::from(self.lock().format.channels)
    }

    /// Run one callback of `frames` frames (interleaved F32); zeros while closed.
    pub fn render_f32(&self, frames: usize) -> Vec<f32> {
        let mut buf = vec![0.0; frames * self.channels()];
        if let Some(r) = self.lock().renderer.as_mut() {
            r.render(OutBuf::F32(&mut buf), RenderTiming::default());
        }
        buf
    }

    /// Run one callback of `frames` frames (interleaved I16); zeros while closed.
    pub fn render_i16(&self, frames: usize) -> Vec<i16> {
        let mut buf = vec![0; frames * self.channels()];
        if let Some(r) = self.lock().renderer.as_mut() {
            r.render(OutBuf::I16(&mut buf), RenderTiming::default());
        }
        buf
    }

    /// Run one callback of `frames` frames (interleaved I32); zeros while closed.
    pub fn render_i32(&self, frames: usize) -> Vec<i32> {
        let mut buf = vec![0; frames * self.channels()];
        if let Some(r) = self.lock().renderer.as_mut() {
            r.render(OutBuf::I32(&mut buf), RenderTiming::default());
        }
        buf
    }

    /// Raise `fault` through the open renderer's fault signal (as a real stream's error
    /// callback would). Does nothing if the output was never opened.
    pub fn fault(&self, fault: OutputFault) {
        let signal = self.lock().signal.clone();
        if let Some(s) = signal {
            s.raise(fault);
        }
    }
}

impl AudioOutput for FakeOutput {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        if let Some(devices) = &self.lock().devices {
            return Ok(devices.clone());
        }
        Ok(vec![OutputDevice {
            id: "fake".into(),
            name: "fake".into(),
            is_default: true,
        }])
    }

    fn modes(&self) -> Vec<OutputMode> {
        self.lock().modes.clone()
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        mut renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        let mut st = self.lock();
        st.requests.push(request.clone());
        if st.renderer.is_some() {
            return Err(OpenFailed {
                error: OutputError::Busy,
                renderer,
            });
        }
        if st.fail_open {
            return Err(OpenFailed {
                error: OutputError::Failed,
                renderer,
            });
        }
        if let Some((_, error)) = st.fail_modes.iter().find(|(m, _)| *m == request.mode) {
            return Err(OpenFailed {
                error: *error,
                renderer,
            });
        }
        if let Some(device) = &request.device
            && st.fail_devices.contains(device)
        {
            return Err(OpenFailed {
                error: OutputError::NoDevice,
                renderer,
            });
        }
        let mut format = st.format;
        if let Some(rate) = request.rate
            && st.rates.contains(&rate)
        {
            format.rate = rate;
        }
        if let Err(error) = renderer.prepare(format) {
            return Err(OpenFailed { error, renderer });
        }
        st.signal = Some(renderer.fault_signal());
        st.renderer = Some(renderer);
        let mode = if st.report_request_mode {
            request.mode
        } else {
            st.mode
        };
        Ok(OutputInfo {
            host_api: "fake".into(),
            device: "fake".into(),
            mode,
            format,
            buffer_frames: None,
            latency_ms: None,
        })
    }

    fn close(&mut self) -> Option<Renderer> {
        self.lock().renderer.take()
    }
}

/// Length of one [`ClockedOutput`] callback.
const CLOCK_BLOCK: Duration = Duration::from_millis(10);

/// An output with a clock instead of a device: a thread calls the renderer with stereo `F32`
/// blocks of 10 ms at real-time pace and discards what it renders. For the demo, which has the
/// engine's whole path (ring, resampler, tap) running without a device.
pub struct ClockedOutput {
    rate: u32,
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Renderer>,
}

impl ClockedOutput {
    /// An output that runs at `rate` Hz once opened.
    pub fn new(rate: u32) -> Self {
        ClockedOutput {
            rate,
            running: None,
        }
    }

    fn stop(&mut self) -> Option<Renderer> {
        let running = self.running.take()?;
        running.stop.store(true, Ordering::Release);
        running.thread.join().ok()
    }
}

impl Drop for ClockedOutput {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl AudioOutput for ClockedOutput {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        _request: &OutputRequest,
        mut renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        if self.running.is_some() {
            return Err(OpenFailed {
                error: OutputError::Busy,
                renderer,
            });
        }
        let format = DeviceFormat {
            rate: self.rate,
            channels: 2,
            sample: SampleFormat::F32,
            valid_bits: 32,
        };
        if let Err(error) = renderer.prepare(format) {
            return Err(OpenFailed { error, renderer });
        }
        let frames = (u64::from(self.rate) * CLOCK_BLOCK.as_millis() as u64 / 1000).max(1) as usize;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0.0f32; frames * 2];
            let mut next = Instant::now();
            while !flag.load(Ordering::Acquire) {
                renderer.render(OutBuf::F32(&mut buf), RenderTiming::default());
                next += CLOCK_BLOCK;
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            renderer
        });
        self.running = Some(Running { stop, thread });
        Ok(OutputInfo {
            host_api: "demo clock (no device)".into(),
            device: "none".into(),
            mode: OutputMode::Shared,
            format,
            buffer_frames: None,
            latency_ms: None,
        })
    }

    fn close(&mut self) -> Option<Renderer> {
        self.stop()
    }
}
