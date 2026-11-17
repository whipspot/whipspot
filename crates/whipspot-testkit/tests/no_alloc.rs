//! The render callback never allocates (hard rule 14, H5): a counting global allocator counts
//! allocations, reallocations and deallocations on the thread that armed it, and every scenario
//! below runs `Renderer::render` armed, on preallocated buffers. The first call after `prepare`
//! is measured too. The engine's control thread, the source's writer and this test's own setup
//! allocate freely: the counters are thread-local and only count while armed.
//!
//! Out of reach here: the WASAPI fill (it needs a COM audio client) and the cpal callback guard
//! (it lives in `whipspot-audio-out`, whose lints are not relaxed for a test); both only
//! forward to `Renderer::render`, which this covers.
//!
//! This file holds the one `unsafe` in the testkit (Q24): a `GlobalAlloc` that forwards to the
//! system allocator and bumps a counter.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::{Arc, Mutex};

use whipspot_core::engine::{
    AudioOutput, BAND_COUNT, DeviceFormat, Engine, EngineConfig, EqSettings, OpenFailed, OutBuf,
    OutputDevice, OutputError, OutputInfo, OutputMode, OutputRequest, PcmTap, RenderTiming,
    Renderer, SampleFormat, SourceInfo, SourceWriter, StageFlags, StreamFormat,
};

thread_local! {
    // Const-initialised cells without destructors: touching them never allocates.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static REALLOCS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn bump(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = counter.try_with(|c| c.set(c.get() + 1));
        }
    });
}

struct Counting;

// SAFETY: every method forwards to `System` with the same arguments and returns its result
// unchanged; the only extra work is bumping thread-local `Cell` counters, which neither
// allocates nor touches the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract for `layout`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract for `layout`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump(&REALLOCS);
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract; `ptr` came from `System`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump(&DEALLOCS);
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract; `ptr` came from `System`
        // with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    allocs: usize,
    reallocs: usize,
    deallocs: usize,
}

/// Runs `f` with this thread's counters armed; what it allocated, reallocated and freed.
fn counted<R>(f: impl FnOnce() -> R) -> (R, Counts) {
    ALLOCS.set(0);
    REALLOCS.set(0);
    DEALLOCS.set(0);
    ARMED.set(true);
    let result = f();
    ARMED.set(false);
    let counts = Counts {
        allocs: ALLOCS.get(),
        reallocs: REALLOCS.get(),
        deallocs: DEALLOCS.get(),
    };
    (result, counts)
}

// ---- the counter itself ----------------------------------------------------------------------

#[test]
fn the_counter_catches_planted_allocations() {
    let ((), counts) = counted(|| {
        let v: Vec<u8> = Vec::with_capacity(64);
        black_box(&v);
        let mut grown = black_box(vec![1u8; 8]);
        grown.reserve(4096);
        black_box(&grown);
    });
    assert!(counts.allocs >= 2, "allocations not counted: {counts:?}");
    assert!(counts.reallocs >= 1, "reallocation not counted: {counts:?}");
    assert!(
        counts.deallocs >= 2,
        "deallocations not counted: {counts:?}"
    );
    // And it is clean when nothing allocates.
    let (sum, counts) = counted(|| black_box([1u32, 2, 3]).iter().sum::<u32>());
    assert_eq!(sum, 6);
    assert_eq!(counts, Counts::default());
}

#[test]
fn the_counter_ignores_other_threads_and_unarmed_time() {
    let ((), counts) = counted(|| {
        std::thread::spawn(|| {
            for _ in 0..100 {
                black_box(vec![0u8; 128]);
            }
        })
        .join()
        .unwrap();
    });
    // Only the spawn itself (on this thread) may count: nothing near the other thread's 100.
    assert!(counts.allocs < 20, "{counts:?}");
    // Not armed: nothing counts.
    ALLOCS.set(0);
    black_box(vec![0u8; 128]);
    assert_eq!(ALLOCS.get(), 0);
}

// ---- the rig ---------------------------------------------------------------------------------

const RATE: u32 = 48_000;
/// A callback of 10 ms.
const BLOCK: usize = 480;
/// Longer than the renderer's largest block (8192), so the split path runs.
const LONG: usize = 20_000;

/// An output that only keeps the renderer, so the test calls `render` itself.
struct Lender(Arc<Mutex<Option<Renderer>>>, DeviceFormat);

impl AudioOutput for Lender {
    fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        _request: &OutputRequest,
        mut renderer: Renderer,
    ) -> Result<OutputInfo, OpenFailed> {
        if let Err(error) = renderer.prepare(self.1) {
            return Err(OpenFailed { error, renderer });
        }
        *self.0.lock().unwrap() = Some(renderer);
        Ok(OutputInfo {
            host_api: "lender".into(),
            device: "lender".into(),
            mode: OutputMode::Shared,
            format: self.1,
            buffer_frames: None,
            latency_ms: None,
        })
    }

    fn close(&mut self) -> Option<Renderer> {
        self.0.lock().unwrap().take()
    }
}

/// The preallocated device buffer.
enum Buf {
    F32(Vec<f32>),
    I16(Vec<i16>),
    I32(Vec<i32>),
}

impl Buf {
    fn new(sample: SampleFormat) -> Buf {
        let n = LONG * 2;
        match sample {
            SampleFormat::F32 => Buf::F32(vec![0.0; n]),
            SampleFormat::I16 => Buf::I16(vec![0; n]),
            SampleFormat::I32 => Buf::I32(vec![0; n]),
        }
    }

    fn out(&mut self, frames: usize) -> OutBuf<'_> {
        match self {
            Buf::F32(b) => OutBuf::F32(&mut b[..frames * 2]),
            Buf::I16(b) => OutBuf::I16(&mut b[..frames * 2]),
            Buf::I32(b) => OutBuf::I32(&mut b[..frames * 2]),
        }
    }
}

struct Rig {
    engine: Engine,
    source: SourceWriter,
    renderer: Renderer,
    _tap: PcmTap,
    buf: Buf,
    /// Stereo sine, one second.
    tone: Vec<f64>,
    pos: usize,
    sample: SampleFormat,
    label: String,
}

fn stream() -> StreamFormat {
    StreamFormat {
        rate: RATE,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: Some(16),
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// One second of a 1 kHz stereo sine of amplitude `amp`.
fn tone(amp: f64) -> Vec<f64> {
    (0..RATE as usize)
        .flat_map(|i| {
            let v = amp * (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(RATE)).sin();
            [v, v]
        })
        .collect()
}

impl Rig {
    fn new(sample: SampleFormat, valid_bits: u8, amp: f64) -> Rig {
        let format = DeviceFormat {
            rate: RATE,
            channels: 2,
            sample,
            valid_bits,
        };
        let slot = Arc::new(Mutex::new(None));
        let parts = Engine::new(
            EngineConfig::default(),
            Some(Box::new(Lender(slot.clone(), format))),
        );
        let mut engine = parts.engine;
        engine.open(&OutputRequest::default()).unwrap();
        let renderer = slot
            .lock()
            .unwrap()
            .take()
            .expect("the lender kept the renderer");
        parts.source.start_track(stream()).unwrap();
        Rig {
            engine,
            source: parts.source,
            renderer,
            _tap: parts.tap,
            buf: Buf::new(sample),
            tone: tone(amp),
            pos: 0,
            sample,
            label: String::new(),
        }
    }

    /// Writes `frames` frames of the tone (outside the measurement).
    fn feed(&mut self, frames: usize) {
        let total = self.tone.len() / 2;
        let mut left = frames;
        while left > 0 {
            let n = left.min(total - self.pos);
            self.source
                .write(&self.tone[self.pos * 2..(self.pos + n) * 2])
                .unwrap();
            self.pos = (self.pos + n) % total;
            left -= n;
        }
    }

    /// One armed callback of `frames` frames; panics naming the scenario if it allocated.
    fn render(&mut self, scenario: &str, frames: usize) {
        let timing = RenderTiming {
            latency_ns: Some(10_000_000),
        };
        let out = self.buf.out(frames);
        let ((), counts) = counted(|| self.renderer.render(out, timing));
        assert_eq!(
            counts,
            Counts::default(),
            "render allocated: scenario {scenario:?} ({:?}, {}), callback of {frames} frames",
            self.sample,
            self.label
        );
    }

    /// Feeds then renders `rounds` callbacks of `BLOCK` frames.
    fn play(&mut self, scenario: &str, rounds: usize) {
        for _ in 0..rounds {
            self.feed(BLOCK);
            self.render(scenario, BLOCK);
        }
    }

    fn applied(&self) -> StageFlags {
        self.engine.status().applied
    }
}

fn eq(preamp_db: f32, bands: [f32; BAND_COUNT]) -> EqSettings {
    EqSettings {
        enabled: true,
        preamp_db,
        bands_db: bands,
    }
}

/// Every render scenario on one device format.
fn scenarios(sample: SampleFormat, valid_bits: u8) {
    let mut rig = Rig::new(sample, valid_bits, 0.5);

    // Steady play: the very first callback after `prepare`, then the bit-exact path.
    rig.label = "neutral".into();
    rig.feed(BLOCK * 5);
    rig.render("first callback after prepare", BLOCK);
    rig.play("steady play, neutral", 20);
    assert!(rig.applied().exact, "the neutral scenario must be exact");

    // A callback longer than the largest block takes the split path.
    rig.feed(LONG / 2);
    rig.render("long callback (split)", LONG);

    // Volume and balance ramp, then steady with the chain on (dither on integer outputs).
    rig.label = "volume and balance".into();
    rig.engine.set_volume(35);
    rig.engine.set_balance(-40);
    rig.play("volume and balance ramping", 8);
    rig.play("steady play, volume and balance", 8);
    assert!(rig.applied().volume || rig.applied().balance);
    if sample != SampleFormat::F32 {
        assert!(rig.applied().dither, "the dithered scenario must dither");
    }
    rig.engine.set_volume(100);
    rig.engine.set_balance(0);
    rig.play("volume and balance back to unity", 8);

    // EQ: on (ramping up), changing while playing, then off (ramping down).
    rig.label = "equalizer".into();
    let mut bands = [0.0; BAND_COUNT];
    bands[2] = 6.0;
    bands[6] = -5.0;
    rig.engine.set_eq(eq(-2.0, bands));
    rig.play("EQ turning on (ramp)", 6);
    assert!(rig.applied().eq, "the EQ scenario must run the EQ");
    bands[2] = -4.0;
    bands[9] = 8.0;
    rig.engine.set_eq(eq(0.0, bands));
    rig.play("EQ bands changing (ramp)", 6);
    rig.play("EQ steady", 6);
    rig.engine.set_eq(EqSettings {
        enabled: false,
        ..eq(0.0, bands)
    });
    rig.play("EQ turning off (ramp)", 8);
    rig.engine.set_eq(eq(0.0, [0.0; BAND_COUNT]));
    rig.play("EQ flat", 6);

    // The limiter engaged: a source over full scale through a boosted band.
    rig.label = "limiter".into();
    rig.tone = tone(1.25);
    let mut loud = [0.0; BAND_COUNT];
    loud[4] = 3.0;
    rig.engine.set_eq(eq(0.0, loud));
    rig.play("limiter engaging", 12);
    rig.play("limiter engaged, steady", 12);
    assert!(
        rig.engine.status().limiter_frames > 0,
        "the limiter scenario must limit"
    );
    rig.engine.set_eq(eq(0.0, [0.0; BAND_COUNT]));
    rig.play("limiter releasing", 12);
    rig.tone = tone(0.5);

    // Pause fade and resume.
    rig.label = "pause".into();
    rig.source.pause();
    rig.play("pause fading out", 3);
    rig.render("paused, nothing consumed", BLOCK);
    rig.source.resume();
    rig.play("resume fading in", 4);

    // A gapless Track mark (no flush) and the end of a track.
    rig.label = "track marks".into();
    rig.source.start_track(stream()).unwrap();
    rig.play("gapless Track mark", 4);
    rig.source.end();
    rig.play("End mark", 4);
    rig.source.start_track(stream()).unwrap();
    rig.play("track after End", 4);
    rig.source.finish();
    rig.render("tail after finish", BLOCK);
    rig.render("ended, ring empty", BLOCK);

    // Flush (seek, skip) and interrupt, each with the declick, then new content.
    rig.label = "flush and interrupt".into();
    rig.source.start_track(stream()).unwrap();
    rig.play("play before flush", 4);
    rig.feed(BLOCK * 3);
    rig.source.flush();
    rig.render("flush declick", BLOCK);
    rig.play("after flush", 4);
    rig.feed(BLOCK * 3);
    rig.source.interrupt();
    rig.render("interrupt declick", BLOCK);
    rig.render("interrupted, discarding", BLOCK);
    rig.source.flush();
    rig.play("after interrupt and flush", 4);
    rig.source.interrupt();
    rig.source.start_track(stream()).unwrap();
    rig.play("after interrupt and start_track", 4);

    // An underrun: the ring runs dry mid-track.
    rig.label = "underrun".into();
    rig.render("underrun (ring dry)", BLOCK);
    rig.render("underrun again", BLOCK);
    assert!(
        rig.engine.status().underruns >= 2,
        "the underrun scenario must underrun"
    );
    rig.play("recovering from underrun", 4);

    // The tap with no reader draining it: it fills and drops.
    rig.label = "tap".into();
    rig.play("tap filling", 40);
    rig.feed(LONG / 2);
    rig.render("tap full (drops)", LONG);
    assert!(
        rig.engine.status().tap_drops > 0,
        "the tap scenario must drop"
    );

    let status = rig.engine.status();
    assert!(status.cpu_max_ns > 0 && status.cpu_max_ns >= status.cpu_avg_ns);
    assert!(status.callbacks > 100);
}

#[test]
fn render_never_allocates_f32() {
    scenarios(SampleFormat::F32, 32);
}

#[test]
fn render_never_allocates_i16() {
    scenarios(SampleFormat::I16, 16);
}

#[test]
fn render_never_allocates_i32_24() {
    scenarios(SampleFormat::I32, 24);
}

#[test]
fn render_never_allocates_i32_32() {
    scenarios(SampleFormat::I32, 32);
}

/// The instrument check for this file's scenarios: an allocation planted inside a measured,
/// render-shaped call is reported.
#[test]
fn a_planted_allocation_inside_a_measured_call_is_reported() {
    let mut rig = Rig::new(SampleFormat::F32, 32, 0.5);
    rig.feed(BLOCK);
    let out = rig.buf.out(BLOCK);
    let ((), counts) = counted(|| {
        rig.renderer.render(out, RenderTiming::default());
        black_box(vec![0u8; 32]);
    });
    assert!(counts.allocs >= 1 && counts.deallocs >= 1, "{counts:?}");
}
