//! State shared by the source (producer), the renderer (real-time consumer) and the engine
//! (control): atomics only, plus the `Copy` marks that travel on the mark ring.
//!
//! Protocol (MD23):
//! - The producer commits the audio before a mark positioned after it, and pushes a mark before
//!   the audio that follows it. The renderer therefore reads the audio fill level *before* it
//!   peeks the mark ring: every frame it counted precedes any mark it has not seen yet.
//! - `content_epoch` is bumped by `interrupt()`; the renderer discards until a `Flush` mark with
//!   an epoch at least as new. `format_epoch` is bumped by every open; the renderer discards
//!   until a `Format` mark at least as new.
//! - Open requests (MD38, I12): the producer stores `req_rate`, then bumps `req_seq` and calls
//!   the hook; the control side reads `req_seq`, then `req_rate`, and answers by storing the
//!   `req_seq` it saw into `ans_seq`. The renderer reads none of these.
//! - Source ownership (D13): `owner` is the token of the provider whose writer handle may
//!   feed the engine (0 = never set: every handle may). A change of owner stores
//!   `owner_epoch`, then bumps `content_epoch` to that value, as an interrupt does. The
//!   renderer reads neither.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use super::error::OutputFault;
use super::status::TrackInfo;

/// Output states (`Shared::output_state`).
pub(crate) const CLOSED: u8 = 0;
pub(crate) const OPENING: u8 = 1;
pub(crate) const OPEN: u8 = 2;
pub(crate) const FAULTED: u8 = 3;
/// Closed on purpose while idle: data to play asks the control side for an open (I13).
pub(crate) const RELEASED: u8 = 4;

/// Called by the producer after it raised an open request (never by the renderer).
pub(crate) type RequestHook = Arc<dyn Fn() + Send + Sync>;

/// Fault codes (`Shared::fault`); 0 = none.
const FAULT_NONE: u8 = 0;
const FAULT_DEVICE_LOST: u8 = 1;
const FAULT_FAILED: u8 = 2;

pub(crate) struct Shared {
    pub(crate) content_epoch: AtomicU64,
    pub(crate) format_epoch: AtomicU64,
    pub(crate) output_state: AtomicU8,
    fault: AtomicU8,
    /// Rate of the open device (valid while `output_state` is `OPEN`).
    pub(crate) device_rate: AtomicU32,
    /// Soft ring depth in frames at the open device rate.
    pub(crate) depth: AtomicU32,
    pub(crate) paused: AtomicBool,
    /// Frames the source pushed (statistics only).
    pub(crate) frames_written: AtomicU64,
    /// Device-rate frames the source committed to the audio ring (the producer's frame
    /// counter, stored after each commit; H25).
    pub(crate) write_pos: AtomicU64,
    /// Frames the renderer took or discarded (its `read_pos`), stored once per callback and
    /// after a discard at open (H25). `write_pos - read_pos` is what is queued, not heard yet.
    pub(crate) read_pos: AtomicU64,
    /// Volume and balance targets as `f64` bits.
    pub(crate) gain_l: AtomicU64,
    pub(crate) gain_r: AtomicU64,
    pub(crate) tap_drops: Arc<AtomicU64>,
    /// Rate follow (I14): set by the control side, read by the producer.
    pub(crate) follow_rate: AtomicBool,
    /// Wanted rate of the newest request (0 = any); stored before `req_seq` is bumped.
    pub(crate) req_rate: AtomicU32,
    /// Requests raised so far (producer bumps).
    pub(crate) req_seq: AtomicU64,
    /// Requests answered so far (control stores the `req_seq` it answered).
    pub(crate) ans_seq: AtomicU64,
    pub(crate) hook: OnceLock<RequestHook>,
    /// The current source owner's token (0 = never set; D13). Never read by the renderer.
    pub(crate) owner: AtomicU64,
    /// The content epoch the newest owner change produced. Never read by the renderer.
    pub(crate) owner_epoch: AtomicU64,
}

impl Shared {
    pub(crate) fn new(tap_drops: Arc<AtomicU64>) -> Self {
        Shared {
            content_epoch: AtomicU64::new(0),
            format_epoch: AtomicU64::new(0),
            output_state: AtomicU8::new(CLOSED),
            fault: AtomicU8::new(FAULT_NONE),
            device_rate: AtomicU32::new(0),
            depth: AtomicU32::new(0),
            paused: AtomicBool::new(false),
            frames_written: AtomicU64::new(0),
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            gain_l: AtomicU64::new(1.0f64.to_bits()),
            gain_r: AtomicU64::new(1.0f64.to_bits()),
            tap_drops,
            follow_rate: AtomicBool::new(false),
            req_rate: AtomicU32::new(0),
            req_seq: AtomicU64::new(0),
            ans_seq: AtomicU64::new(0),
            hook: OnceLock::new(),
            owner: AtomicU64::new(0),
            owner_epoch: AtomicU64::new(0),
        }
    }

    pub(crate) fn state(&self) -> u8 {
        self.output_state.load(Ordering::Acquire)
    }

    pub(crate) fn set_state(&self, state: u8) {
        self.output_state.store(state, Ordering::Release);
    }

    /// Take the pending fault, if any (each fault is reported once).
    pub(crate) fn take_fault(&self) -> Option<OutputFault> {
        match self.fault.swap(FAULT_NONE, Ordering::AcqRel) {
            FAULT_DEVICE_LOST => Some(OutputFault::DeviceLost),
            FAULT_FAILED => Some(OutputFault::Failed),
            _ => None,
        }
    }

    pub(crate) fn clear_fault(&self) {
        self.fault.store(FAULT_NONE, Ordering::Release);
    }

    /// The wanted rate of the newest unanswered request.
    pub(crate) fn open_request(&self) -> Option<u32> {
        let req = self.req_seq.load(Ordering::Acquire);
        let rate = self.req_rate.load(Ordering::Acquire);
        (req > self.ans_seq.load(Ordering::Acquire)).then_some(rate)
    }

    /// Answer every request raised so far.
    pub(crate) fn answer_requests(&self) {
        let req = self.req_seq.load(Ordering::Acquire);
        self.ans_seq.fetch_max(req, Ordering::AcqRel);
    }
}

/// Raises an output fault from the adapter (any thread, including the audio callback).
#[derive(Clone)]
pub struct FaultSignal {
    pub(crate) shared: Arc<Shared>,
}

impl FaultSignal {
    /// Record `fault` (the first one wins until the engine polls it) and fail the output:
    /// later writes return `Err(OutputFailed)` until the engine reopens.
    pub fn raise(&self, fault: OutputFault) {
        let code = match fault {
            OutputFault::DeviceLost => FAULT_DEVICE_LOST,
            OutputFault::Failed => FAULT_FAILED,
        };
        let _ = self.shared.fault.compare_exchange(
            FAULT_NONE,
            code,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.shared.set_state(FAULTED);
    }
}

impl std::fmt::Debug for FaultSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FaultSignal")
    }
}

/// A mark on the mark ring, positioned at the producer's frame counter when it was pushed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
    pub(crate) at: u64,
    pub(crate) kind: MarkKind,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MarkKind {
    /// A track starts here.
    Track(TrackInfo),
    /// The source flushed (seek, stop, track change) or acknowledged an interrupt.
    Flush(u64),
    /// The source adopted the format of open number `epoch`; `content` is the source's
    /// acknowledged content epoch at that time.
    Format { epoch: u64, content: u64 },
    /// The track is over (more may follow gaplessly).
    End,
}
