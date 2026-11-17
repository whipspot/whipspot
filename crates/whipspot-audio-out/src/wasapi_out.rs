//! WASAPI exclusive, event-driven output for the engine's [`Renderer`] (Windows; MD42).
//!
//! Every COM object (enumerator, device, audio client, render client) is `!Send` and bound to
//! the apartment that made it, so each open runs on its own render thread that creates, uses
//! and releases them; only the stop flag, the channels and the join handle live in
//! [`WasapiOutput`]. The renderer comes back through a drop guard on every exit path (MD26).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use audio_thread_priority::{
    RtPriorityHandle, demote_current_thread_from_real_time, promote_current_thread_to_real_time,
};
use wasapi::{
    AudioClient, AudioRenderClient, Device, DeviceEnumerator, Direction, Handle, SampleType,
    StreamMode, WasapiError, WaveFormat, calculate_period_100ns, deinitialize, initialize_mta,
};
use whipspot_core::engine::{
    AudioOutput, DeviceFormat, FaultSignal, OpenFailed, OutBuf, OutputDevice, OutputError,
    OutputFault, OutputInfo, OutputMode, OutputRequest, RenderTiming, Renderer, SampleFormat,
};

use crate::exclusive::{
    BUFFER_SIZE_NOT_ALIGNED, Candidate, DEVICE_INVALIDATED, OutSamples, map_hresult, negotiate,
    pack,
};

/// How long `close` waits for the renderer to come back from the render thread.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// How long the render thread waits for one device event before it calls the stream failed.
const EVENT_WAIT_MS: u32 = 500;
/// The shortest period asked for: 10 ms in 100 ns units.
const MIN_PERIOD_HNS: i64 = 100_000;
/// Buffer alignment in bytes (Intel HDA devices need 128).
const ALIGN_BYTES: u32 = 128;

/// The WASAPI exclusive output. Opens nothing and starts no thread until `open`.
pub struct WasapiOutput {
    open: Option<OpenStream>,
}

/// Handles to a running render thread (all `Send`).
struct OpenStream {
    stop: Arc<AtomicBool>,
    back: Receiver<Renderer>,
    thread: JoinHandle<()>,
}

impl WasapiOutput {
    pub fn new() -> Self {
        Self { open: None }
    }

    /// Whether a stream is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

impl Default for WasapiOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioOutput for WasapiOutput {
    /// Devices are listed by the router (through cpal, same friendly names, V12).
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        Err(OutputError::Unsupported)
    }

    #[allow(clippy::result_large_err)]
    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        if request.mode != OutputMode::Exclusive {
            return Err(OpenFailed {
                error: OutputError::Unsupported,
                renderer,
            });
        }
        if self.open.is_some() {
            return Err(OpenFailed {
                error: OutputError::Busy,
                renderer,
            });
        }

        let (back_tx, back_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let guard = Guard::new(renderer, back_tx);
        let request = request.clone();
        let thread_stop = stop.clone();
        let spawned = thread::Builder::new()
            .name("whipspot-wasapi-out".into())
            .spawn(move || thread_main(&request, guard, &thread_stop, &ready_tx));
        let thread = match spawned {
            Ok(thread) => thread,
            // The closure (and its guard) was dropped, which sent the renderer back.
            Err(_) => return Err(failed(&back_rx, OutputError::Failed)),
        };

        match ready_rx.recv() {
            Ok(Ok(info)) => {
                self.open = Some(OpenStream {
                    stop,
                    back: back_rx,
                    thread,
                });
                Ok(info)
            }
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(failed(&back_rx, error))
            }
            // The thread ended without answering (it panicked): the guard was dropped.
            Err(_) => {
                let _ = thread.join();
                Err(failed(&back_rx, OutputError::Failed))
            }
        }
    }

    fn close(&mut self) -> Option<Renderer> {
        let OpenStream { stop, back, thread } = self.open.take()?;
        stop.store(true, Ordering::Release);
        let renderer = back.recv_timeout(CLOSE_WAIT).ok();
        if renderer.is_some() {
            // The device is released; the thread is finishing.
            let _ = thread.join();
        }
        renderer
    }

    fn modes(&self) -> Vec<OutputMode> {
        vec![OutputMode::Exclusive]
    }
}

impl Drop for WasapiOutput {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Build the `OpenFailed` for a failure after the renderer went to the render thread. The
/// thread has ended (joined, or never started), so its guard has sent the renderer back.
#[allow(clippy::result_large_err)]
fn failed(back: &Receiver<Renderer>, error: OutputError) -> OpenFailed {
    let renderer = back
        .recv()
        .expect("the guard returns the renderer on every open failure path");
    OpenFailed { error, renderer }
}

/// Owns the renderer on the render thread; sends it back when dropped.
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

    /// Fill `buf` with one period and pack it into `bytes`.
    fn fill(&mut self, buf: &mut TypedBuf, bytes: &mut [u8], container_bits: u8, t: RenderTiming) {
        let Some(r) = self.renderer.as_mut() else {
            bytes.fill(0);
            return;
        };
        match buf {
            TypedBuf::F32(b) => {
                r.render(OutBuf::F32(b), t);
                pack(bytes, container_bits, OutSamples::F32(b));
            }
            TypedBuf::I16(b) => {
                r.render(OutBuf::I16(b), t);
                pack(bytes, container_bits, OutSamples::I16(b));
            }
            TypedBuf::I32(b) => {
                r.render(OutBuf::I32(b), t);
                pack(bytes, container_bits, OutSamples::I32(b));
            }
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(renderer) = self.renderer.take() {
            let _ = self.back.send(renderer);
        }
    }
}

/// The renderer's buffer for one period, preallocated before the stream starts.
enum TypedBuf {
    F32(Vec<f32>),
    I16(Vec<i16>),
    I32(Vec<i32>),
}

impl TypedBuf {
    fn new(sample: SampleFormat, samples: usize) -> Self {
        match sample {
            SampleFormat::F32 => TypedBuf::F32(vec![0.0; samples]),
            SampleFormat::I16 => TypedBuf::I16(vec![0; samples]),
            SampleFormat::I32 => TypedBuf::I32(vec![0; samples]),
        }
    }
}

/// An initialised exclusive client and what it runs at. Fields drop in declaration order:
/// render client, event, client, device, enumerator.
struct Session {
    render: AudioRenderClient,
    event: Handle,
    client: AudioClient,
    _device: Device,
    _enumerator: DeviceEnumerator,
    name: String,
    format: DeviceFormat,
    container_bits: u8,
    block_align: usize,
    frames: usize,
}

type Ready = Sender<Result<OutputInfo, OutputError>>;

/// The render thread: COM up, run the stream, COM down; the guard drops last, returning the
/// renderer after every COM object is released.
fn thread_main(request: &OutputRequest, mut guard: Guard, stop: &AtomicBool, ready: &Ready) {
    if initialize_mta().is_err() {
        let _ = ready.send(Err(OutputError::Failed));
        return;
    }
    run(request, &mut guard, stop, ready);
    deinitialize();
    drop(guard);
}

/// Open, answer the opener, render until stopped or failed, release. Every COM object is
/// dropped before this returns.
fn run(request: &OutputRequest, guard: &mut Guard, stop: &AtomicBool, ready: &Ready) {
    let session = match setup(request) {
        Ok(session) => session,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let Some(fault) = guard.fault_signal() else {
        let _ = ready.send(Err(OutputError::Failed));
        return;
    };
    if let Err(error) = guard.prepare(session.format) {
        let _ = ready.send(Err(error));
        return;
    }

    // The only allocations: one period of rendered samples and of device bytes.
    let rate = session.format.rate;
    let frames = session.frames;
    let mut buf = TypedBuf::new(session.format.sample, frames * 2);
    let mut bytes = vec![0u8; frames * session.block_align];
    let period_ns = (frames as u64 * 1_000_000_000) / u64::from(rate);
    let timing = RenderTiming {
        latency_ns: Some(period_ns),
    };

    // Fill the buffer before the stream starts so playback begins with real audio.
    guard.fill(&mut buf, &mut bytes, session.container_bits, timing);
    if let Err(e) = session.render.write_to_device(&bytes, None) {
        let _ = ready.send(Err(map_error(&e)));
        return;
    }
    if let Err(e) = session.client.start_stream() {
        let _ = ready.send(Err(map_error(&e)));
        return;
    }

    let priority: Option<RtPriorityHandle> =
        promote_current_thread_to_real_time(frames as u32, rate).ok();
    let mut host_api = if priority.is_some() {
        "WASAPI exclusive (event, MMCSS)".to_owned()
    } else {
        "WASAPI exclusive (event)".to_owned()
    };
    if session.container_bits == 24 {
        host_api.push_str(" · 24-bit packed");
    }
    let _ = ready.send(Ok(OutputInfo {
        host_api,
        device: session.name.clone(),
        mode: OutputMode::Exclusive,
        format: session.format,
        buffer_frames: Some(frames as u32),
        latency_ms: Some(frames as f32 * 1000.0 / rate as f32),
    }));

    // Real-time loop: no allocation, lock, log or I/O besides the WASAPI wait and write.
    loop {
        if session.event.wait_for_event(EVENT_WAIT_MS).is_err() {
            fault.raise(OutputFault::Failed);
            break;
        }
        if stop.load(Ordering::Acquire) {
            // Let the last rendered period play: one silent period, one event.
            bytes.fill(0);
            let _ = session.render.write_to_device(&bytes, None);
            let _ = session.event.wait_for_event(EVENT_WAIT_MS);
            break;
        }
        guard.fill(&mut buf, &mut bytes, session.container_bits, timing);
        if let Err(e) = session.render.write_to_device(&bytes, None) {
            fault.raise(write_fault(&e));
            break;
        }
    }

    let _ = session.client.stop_stream();
    if let Some(handle) = priority {
        let _ = demote_current_thread_from_real_time(handle);
    }
    drop(session);
}

/// Find the device, negotiate the format and initialise the exclusive event client.
fn setup(request: &OutputRequest) -> Result<Session, OutputError> {
    let enumerator = DeviceEnumerator::new().map_err(|e| map_error(&e))?;
    let device = match request.device.as_deref() {
        None => enumerator
            .get_default_device(Direction::Render)
            .map_err(|_| OutputError::NoDevice)?,
        Some(name) => enumerator
            .get_device_collection(Direction::Render)
            .and_then(|c| c.get_device_with_name(name))
            .map_err(|_| OutputError::NoDevice)?,
    };
    let mut client = device.get_iaudioclient().map_err(|e| map_error(&e))?;

    let wanted = match request.rate {
        Some(rate) => rate,
        None => device
            .get_device_format()
            .or_else(|_| client.get_mixformat())
            .map(|f| f.get_samplespersec())
            .unwrap_or(0),
    };
    let mut accepted = None;
    let chosen = negotiate(wanted, &request.formats, |c| {
        match client.is_supported_exclusive_with_quirks(&wave_format(c)) {
            Ok(wf) => {
                accepted = Some(wf);
                true
            }
            Err(_) => false,
        }
    });
    let (Some(candidate), Some(wf)) = (chosen, accepted) else {
        return Err(OutputError::Unsupported);
    };
    let block_align = wf.get_blockalign() as usize;
    if wf.get_nchannels() != 2 || block_align != 2 * usize::from(candidate.container_bits / 8) {
        return Err(OutputError::Unsupported);
    }

    let (default_period, _min) = client.get_device_period().map_err(|e| map_error(&e))?;
    let period = client
        .calculate_aligned_period_near(default_period.max(MIN_PERIOD_HNS), Some(ALIGN_BYTES), &wf)
        .map_err(|e| map_error(&e))?;
    let mode = StreamMode::EventsExclusive { period_hns: period };
    match client.initialize_client(&wf, Direction::Render, mode, None) {
        Ok(()) => {}
        Err(e) if hresult(&e) == Some(BUFFER_SIZE_NOT_ALIGNED) => {
            // Retry once on a fresh client with the period of the next aligned buffer size.
            let frames = client.get_buffer_size().map_err(|e| map_error(&e))?;
            let aligned =
                calculate_period_100ns(i64::from(frames), i64::from(candidate.format.rate));
            client = device.get_iaudioclient().map_err(|e| map_error(&e))?;
            let mode = StreamMode::EventsExclusive {
                period_hns: aligned,
            };
            client
                .initialize_client(&wf, Direction::Render, mode, None)
                .map_err(|e| map_error(&e))?;
        }
        Err(e) => return Err(map_error(&e)),
    }

    let event = client.set_get_eventhandle().map_err(|e| map_error(&e))?;
    let render = client.get_audiorenderclient().map_err(|e| map_error(&e))?;
    let frames = client.get_buffer_size().map_err(|e| map_error(&e))? as usize;
    if frames == 0 {
        return Err(OutputError::Failed);
    }
    let name = device
        .get_friendlyname()
        .unwrap_or_else(|_| "unknown device".to_owned());
    Ok(Session {
        render,
        event,
        client,
        _device: device,
        _enumerator: enumerator,
        name,
        format: candidate.format,
        container_bits: candidate.container_bits,
        block_align,
        frames,
    })
}

/// The WASAPI format to probe for a candidate (2 channels, default mask).
fn wave_format(c: &Candidate) -> WaveFormat {
    let sample = match c.format.sample {
        SampleFormat::F32 => SampleType::Float,
        SampleFormat::I16 | SampleFormat::I32 => SampleType::Int,
    };
    WaveFormat::new(
        usize::from(c.container_bits),
        usize::from(c.format.valid_bits),
        sample,
        c.format.rate as usize,
        usize::from(c.format.channels),
        None,
    )
}

/// The HRESULT of a Windows error, as an unsigned code.
fn hresult(e: &WasapiError) -> Option<u32> {
    match e {
        WasapiError::Windows(w) => Some(w.code().0 as u32),
        _ => None,
    }
}

/// A setup error as an [`OutputError`]; the library error itself is never kept or shown.
fn map_error(e: &WasapiError) -> OutputError {
    match (e, hresult(e)) {
        (_, Some(code)) => map_hresult(code),
        (WasapiError::UnsupportedFormat, None) => OutputError::Unsupported,
        _ => OutputError::Failed,
    }
}

/// A write error in the loop as an [`OutputFault`].
fn write_fault(e: &WasapiError) -> OutputFault {
    if hresult(e) == Some(DEVICE_INVALIDATED) {
        OutputFault::DeviceLost
    } else {
        OutputFault::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasapi_output_is_send() {
        fn is_send<T: Send>() {}
        is_send::<WasapiOutput>();
    }

    #[test]
    fn new_opens_nothing() {
        let out = WasapiOutput::new();
        assert!(out.open.is_none());
        assert!(!out.is_open());
        assert_eq!(out.modes(), vec![OutputMode::Exclusive]);
    }

    #[test]
    fn devices_are_left_to_the_router() {
        assert_eq!(WasapiOutput::new().devices(), Err(OutputError::Unsupported));
    }

    #[test]
    fn error_mapping_without_a_device() {
        assert_eq!(
            map_error(&WasapiError::UnsupportedFormat),
            OutputError::Unsupported
        );
        assert_eq!(map_error(&WasapiError::EventTimeout), OutputError::Failed);
        assert_eq!(write_fault(&WasapiError::EventTimeout), OutputFault::Failed);
    }

    #[test]
    fn candidate_wave_formats() {
        let c = |sample, valid_bits, container_bits| Candidate {
            format: DeviceFormat {
                rate: 96_000,
                channels: 2,
                sample,
                valid_bits,
            },
            container_bits,
        };
        let packed = wave_format(&c(SampleFormat::I32, 24, 24));
        assert_eq!(packed.get_blockalign(), 6);
        assert_eq!(packed.get_validbitspersample(), 24);
        assert_eq!(packed.get_samplespersec(), 96_000);
        let in32 = wave_format(&c(SampleFormat::I32, 24, 32));
        assert_eq!(in32.get_blockalign(), 8);
        assert_eq!(in32.get_validbitspersample(), 24);
        assert_eq!(
            wave_format(&c(SampleFormat::I16, 16, 16)).get_blockalign(),
            4
        );
        assert_eq!(
            wave_format(&c(SampleFormat::F32, 32, 32))
                .get_subformat()
                .ok(),
            Some(SampleType::Float)
        );
    }
}
