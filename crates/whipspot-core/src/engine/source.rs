//! The source side: what a provider's decoder thread writes into.
//!
//! The producer half of both rings sits behind one mutex (rtrb's `Producer` is not `Sync`).
//! Only atomics are read while it is held (I10); `interrupt`, `pause`, `resume` and `gains`
//! never take it.
//!
//! A track whose rate differs from the device's is resampled here, on the producer's thread
//! (MD30, `resample`).
//!
//! Open requests (MD38, I12-I14): `start_track` and `write` (and only they) ask the control
//! side for an open while the output is RELEASED, and `start_track` asks for the track's rate
//! when rate follow is on. The request is raised and waited for with the producer mutex held;
//! the control side never takes it (I17).
//!
//! Source ownership (D13): each provider writes through its own handle
//! ([`SourceWriter::for_owner`]); once an owner is set ([`OwnerSwitch::set_owner`]) a handle
//! that is not the owner's does nothing. The check is an atomic load, taken under the producer
//! mutex for the calls that hold it, so a stale call never interleaves with the new owner's.
//! A plain handle (never `for_owner`) and an engine whose owner was never set accept every
//! call, as before. The renderer never reads the owner.
//!
//! A non-owner's `write` is paced (F1): it sleeps for the dropped audio's duration at that
//! owner's last track rate, at most [`PACE_CAP`] per call, without the producer mutex, and
//! returns early when the handle owns the engine again. So the previous provider's decoder runs
//! in real time during the next provider's load instead of racing through its track. Only the
//! current owner acknowledges an owner change (F2): `start_track` and `write` re-check the
//! owner after their output wait, with the content epoch read first, so a switch that lands
//! during the wait is flushed by the new owner's `start_track`.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rtrb::Producer;

use super::error::SourceError;
use super::format::{MAX_RATE, MIN_RATE, StreamFormat};
use super::resample::Resample;
use super::shared::{CLOSED, FAULTED, Mark, MarkKind, OPEN, OPENING, RELEASED, Shared};
use super::status::TrackInfo;
use super::tap::TapWriter;

/// How often a blocked call re-checks interrupt, format and output state.
const POLL: Duration = Duration::from_millis(2);

/// The longest a non-owner's `write` sleeps in one call (F1). Bounds Stop latency for a decoder
/// blocked in a paced write; a decoder's packets (tens of ms) are paced in full.
pub const PACE_CAP: Duration = Duration::from_millis(100);

/// The writing end of the engine. Cloneable; every clone writes into the same rings (and
/// keeps the owner it was made for).
#[derive(Clone)]
pub struct SourceWriter {
    inner: Arc<Inner>,
    /// The owner token this handle writes for; 0 for a plain handle (D13).
    owner: u64,
}

/// Changes the engine's source owner (D13). Cloneable; any thread. The transport sets the
/// owner before a provider's load and restores the previous one if the load fails.
#[derive(Clone)]
pub struct OwnerSwitch {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    producer: Mutex<ProducerState>,
    /// The tap writer when the engine has no output adapter (for the fallback output).
    tap: Mutex<Option<TapWriter>>,
    stall_timeout: Duration,
    /// How long an open request waits for its answer.
    request_timeout: Duration,
    capacity: usize,
    /// Interned owner ids: token = index + 1. Also serialises owner changes.
    owners: Mutex<Vec<String>>,
    /// Per owner token (index + 1): the (rate, channels) of the last track it started, owner
    /// or not; paces its writes while it is not the owner (F1). Never held while sleeping.
    paces: Mutex<Vec<Option<(u32, u16)>>>,
}

struct ProducerState {
    audio: Producer<[f64; 2]>,
    marks: Producer<Mark>,
    /// Frames pushed (never reset; marks are positioned by it).
    write_pos: u64,
    track: Option<StreamFormat>,
    /// An `End` was pushed since the last `Track` mark.
    ended: bool,
    /// The resampler while the track rate differs from the device rate.
    resample: Option<Resample>,
    content_acked: u64,
    /// The format epoch this producer last acknowledged.
    format_seen: u64,
    device_rate: u32,
    depth: usize,
    /// The owner token of the handle that started the current track (0 = a plain handle).
    track_owner: u64,
}

/// Result of waiting for ring space.
enum Space {
    Room,
    /// An interrupt or a reopen happened: drop the data.
    Dropped,
}

impl SourceWriter {
    pub(crate) fn new(
        shared: Arc<Shared>,
        audio: Producer<[f64; 2]>,
        marks: Producer<Mark>,
        stall_timeout: Duration,
        request_timeout: Duration,
        tap: Option<TapWriter>,
    ) -> Self {
        let capacity = audio.buffer().capacity();
        SourceWriter {
            inner: Arc::new(Inner {
                shared,
                producer: Mutex::new(ProducerState {
                    audio,
                    marks,
                    write_pos: 0,
                    track: None,
                    ended: false,
                    resample: None,
                    content_acked: 0,
                    format_seen: 0,
                    device_rate: 0,
                    depth: 1,
                    track_owner: 0,
                }),
                tap: Mutex::new(tap),
                stall_timeout,
                request_timeout,
                capacity,
                owners: Mutex::new(Vec::new()),
                paces: Mutex::new(Vec::new()),
            }),
            owner: 0,
        }
    }

    /// A handle on the same engine that writes for the owner `id` (D13): while another owner
    /// is set, its calls do nothing (`write` returns `Ok` with the data dropped).
    pub fn for_owner(&self, id: &str) -> SourceWriter {
        SourceWriter {
            inner: self.inner.clone(),
            owner: token(&mut lock_owners(&self.inner), id),
        }
    }

    /// The switch that sets this engine's owner.
    pub fn owner_switch(&self) -> OwnerSwitch {
        OwnerSwitch {
            inner: self.inner.clone(),
        }
    }

    /// Whether this handle may act now: a plain handle, no owner set, or the owner's own.
    fn owns(&self) -> bool {
        if self.owner == 0 {
            return true;
        }
        let current = self.inner.shared.owner.load(Ordering::Acquire);
        current == 0 || current == self.owner
    }

    fn lock(&self) -> MutexGuard<'_, ProducerState> {
        self.inner
            .producer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Start a track: acknowledges a pending interrupt in band, then marks the track start.
    /// `Err(Unsupported)` for channels other than 1 or 2 or a rate outside 8–384 kHz. A rate
    /// other than the device's is resampled; a same-rate track continues the resampler
    /// (gapless), a different rate drains it first and then rebuilds it.
    ///
    /// While the output is released this asks the control side for an open at the track's rate
    /// and waits (`Err(NoOutput)` when declined or unanswered, I13). With rate follow on, a rate
    /// other than the device's lets the queued audio play out and then asks for the track's
    /// rate (I14); declined, the track is resampled.
    pub fn start_track(&self, format: StreamFormat) -> Result<(), SourceError> {
        if !(1..=2).contains(&format.channels) || !(MIN_RATE..=MAX_RATE).contains(&format.rate) {
            return Err(SourceError::Unsupported);
        }
        self.record_pace(format.rate, format.channels);
        let mut st = self.lock();
        if !self.owns() {
            return Ok(());
        }
        self.ready(&mut st, Some(format.rate))?;
        let epoch = self.inner.shared.content_epoch.load(Ordering::Acquire);
        // Re-check after the wait, the epoch read first (F2): a switch after this check bumps
        // the epoch past `epoch`, so the new owner's `start_track` still flushes.
        if !self.owns() {
            return Ok(());
        }
        let interrupted = epoch > st.content_acked;
        if interrupted {
            self.push_mark(&mut st, MarkKind::Flush(epoch))?;
            st.content_acked = epoch;
            if let Some(rs) = st.resample.as_mut() {
                rs.reset();
            }
        }
        let keep = st
            .resample
            .as_ref()
            .is_some_and(|rs| rs.src_rate() == format.rate);
        let mut drained = false;
        if !keep
            && format.rate != st.device_rate
            && self.inner.shared.follow_rate.load(Ordering::Acquire)
        {
            if !interrupted {
                self.drain(&mut st)?;
            }
            st.resample = None;
            drained = true;
            // The previous track is done with: adopting a new format below must not rebuild a
            // resampler (or push a mark) for it.
            let previous = st.track.take();
            if interrupted || self.wait_ring_empty(&st) {
                // Declined or unanswered: carry on at the current rate (resampled).
                let _ = self.request_and_wait(format.rate);
            }
            // Adopts the reopened format (I7), or any other open that happened meanwhile.
            if let Err(e) = self.ready(&mut st, Some(format.rate)) {
                st.track = previous;
                return Err(e);
            }
        }
        if !keep {
            if !interrupted && !drained {
                self.drain(&mut st)?;
            }
            st.resample = None;
            if format.rate != st.device_rate {
                st.resample = Some(Resample::new(format.rate, st.device_rate)?);
            }
        }
        let resampled = st.resample.is_some();
        self.push_mark(&mut st, MarkKind::Track(TrackInfo { format, resampled }))?;
        st.track = Some(format);
        st.track_owner = self.owner;
        st.ended = false;
        Ok(())
    }

    /// Write interleaved samples in the current track's channel count (mono is duplicated to
    /// both channels; an odd trailing sample is dropped). Blocks while the ring holds more than
    /// its depth. `Ok(())` with the data dropped after an interrupt (until the next `flush` or
    /// `start_track`) or a reopen. While the output is released this asks the control side for
    /// an open and waits (I13).
    ///
    /// Ownership (D13): `Ok(())` with the data dropped from a handle that is not the owner's,
    /// or while the current track was started by another owner. When the newest interrupt is
    /// an owner change, the owner's handle acknowledges it in band and plays on (an owner
    /// restored after a failed load keeps playing; its audio queued before is gone). Dropped
    /// data is paced (F1): the call sleeps for its duration, at most [`PACE_CAP`].
    pub fn write(&self, interleaved: &[f64]) -> Result<(), SourceError> {
        let mut st = self.lock();
        if !self.owns() {
            drop(st);
            self.pace(interleaved.len(), true);
            return Ok(());
        }
        let wanted = st.track.map_or(st.device_rate, |t| t.rate);
        self.ready(&mut st, Some(wanted))?;
        // The owner may have changed during the wait (F2).
        if !self.owns() {
            drop(st);
            self.pace(interleaved.len(), true);
            return Ok(());
        }
        let Some(track) = st.track else {
            return Err(SourceError::NoTrack);
        };
        if self.owner != 0
            && st.track_owner != 0
            && st.track_owner != self.owner
            && self.inner.shared.owner.load(Ordering::Acquire) != 0
        {
            // The track was started by a previous owner: never play this data in its format.
            drop(st);
            self.pace(interleaved.len(), false);
            return Ok(());
        }
        if self.interrupted(&st) && !self.ack_owner_change(&mut st)? {
            return Ok(());
        }
        let channels = usize::from(track.channels);
        let total = interleaved.len() / channels;
        self.inner
            .shared
            .frames_written
            .fetch_add(total as u64, Ordering::Relaxed);
        if let Some(mut rs) = st.resample.take() {
            let input = &interleaved[..total * channels];
            let result = rs.push(input, channels, &mut |d| self.push_stereo(&mut st, d));
            st.resample = Some(rs);
            return result.map(|_| ());
        }
        let max_chunk = (st.depth / 4).max(1);
        let mut done = 0;
        while done < total {
            let n = (total - done).min(max_chunk);
            if let Space::Dropped = self.wait_space(&mut st, n)? {
                return Ok(());
            }
            let src = &interleaved[done * channels..(done + n) * channels];
            let Ok(mut chunk) = st.audio.write_chunk(n) else {
                // Unreachable: `wait_space` saw room for `n` frames.
                return Err(SourceError::Stalled);
            };
            let (a, b) = chunk.as_mut_slices();
            let mut frames = src.chunks_exact(channels);
            for dst in a.iter_mut().chain(b.iter_mut()) {
                if let Some(f) = frames.next() {
                    *dst = if channels == 1 {
                        [f[0], f[0]]
                    } else {
                        [f[0], f[1]]
                    };
                }
            }
            chunk.commit_all();
            self.committed(&mut st, n);
            done += n;
        }
        Ok(())
    }

    /// Push interleaved stereo frames into the ring (blocking as `write` does). `Ok(false)`
    /// when they were dropped by an interrupt or a reopen.
    fn push_stereo(
        &self,
        st: &mut ProducerState,
        interleaved: &[f64],
    ) -> Result<bool, SourceError> {
        let total = interleaved.len() / 2;
        let max_chunk = (st.depth / 4).max(1);
        let mut done = 0;
        while done < total {
            let n = (total - done).min(max_chunk);
            if let Space::Dropped = self.wait_space(st, n)? {
                return Ok(false);
            }
            let Ok(mut chunk) = st.audio.write_chunk(n) else {
                // Unreachable: `wait_space` saw room for `n` frames.
                return Err(SourceError::Stalled);
            };
            let (a, b) = chunk.as_mut_slices();
            let mut frames = interleaved[2 * done..2 * (done + n)].chunks_exact(2);
            for dst in a.iter_mut().chain(b.iter_mut()) {
                if let Some(f) = frames.next() {
                    *dst = [f[0], f[1]];
                }
            }
            chunk.commit_all();
            self.committed(st, n);
            done += n;
        }
        Ok(true)
    }

    /// `n` frames were committed to the audio ring: advance the frame counter and publish it
    /// (the renderer's counterpart is `Shared::read_pos`; H25).
    fn committed(&self, st: &mut ProducerState, n: usize) {
        st.write_pos += n as u64;
        self.inner
            .shared
            .write_pos
            .store(st.write_pos, Ordering::Release);
    }

    /// Let the resampler's tail out (no-op without one).
    fn drain(&self, st: &mut ProducerState) -> Result<(), SourceError> {
        if let Some(mut rs) = st.resample.take() {
            let result = rs.drain(&mut |d| self.push_stereo(st, d));
            st.resample = Some(rs);
            result?;
        }
        Ok(())
    }

    /// When the newest interrupt is an owner change and this is the current owner's handle,
    /// acknowledge it in band (a flush) and return `true`; otherwise `false` (the data is
    /// dropped). The owner is read after the epoch (F2): a later switch bumps the epoch past
    /// the one acknowledged here, so the next owner's `start_track` still flushes.
    fn ack_owner_change(&self, st: &mut ProducerState) -> Result<bool, SourceError> {
        let shared = &self.inner.shared;
        let epoch = shared.content_epoch.load(Ordering::Acquire);
        if self.owner == 0
            || epoch != shared.owner_epoch.load(Ordering::Acquire)
            || shared.owner.load(Ordering::Acquire) != self.owner
        {
            return Ok(false);
        }
        if let Some(rs) = st.resample.as_mut() {
            rs.reset();
        }
        self.push_mark(st, MarkKind::Flush(epoch))?;
        st.content_acked = epoch;
        Ok(true)
    }

    /// In-band flush: nothing written before it plays after the next callback. Acknowledges a
    /// pending interrupt.
    pub fn flush(&self) {
        let mut st = self.lock();
        if !self.owns() {
            return;
        }
        if let Some(rs) = st.resample.as_mut() {
            rs.reset();
        }
        let epoch = self.inner.shared.content_epoch.load(Ordering::Acquire);
        if self.push_mark(&mut st, MarkKind::Flush(epoch)).is_ok() {
            st.content_acked = epoch;
        }
    }

    /// The track is over; more may follow gaplessly. Audio written after it still plays.
    pub fn end(&self) {
        let mut st = self.lock();
        if !self.owns() {
            return;
        }
        if self.push_mark(&mut st, MarkKind::End).is_ok() {
            st.ended = true;
        }
    }

    /// The source has nothing more for now: drain the resampler and let the tail play out.
    /// Never asks for an open: released, it only marks the end.
    pub fn finish(&self) {
        let mut st = self.lock();
        if !self.owns() {
            return;
        }
        if self.ready(&mut st, None).is_ok() && !self.interrupted(&st) {
            let _ = self.drain(&mut st);
        }
        if self.push_mark(&mut st, MarkKind::End).is_ok() {
            st.ended = true;
        }
    }

    /// From the next callback on, output silence and discard what is queued; `write` drops data
    /// until the source's next `flush` or `start_track`. Any thread, lock-free.
    pub fn interrupt(&self) {
        if !self.owns() {
            return;
        }
        self.inner
            .shared
            .content_epoch
            .fetch_add(1, Ordering::AcqRel);
    }

    /// Fade out over 5 ms and stop consuming (the ring is kept). Any thread, lock-free.
    pub fn pause(&self) {
        if !self.owns() {
            return;
        }
        self.inner.shared.paused.store(true, Ordering::Release);
    }

    /// Fade in over 5 ms and continue with the next unconsumed frame. Any thread, lock-free.
    pub fn resume(&self) {
        if !self.owns() {
            return;
        }
        self.inner.shared.paused.store(false, Ordering::Release);
    }

    /// The (left, right) gain targets of the volume and balance stages.
    pub fn gains(&self) -> (f64, f64) {
        let s = &self.inner.shared;
        (
            f64::from_bits(s.gain_l.load(Ordering::Relaxed)),
            f64::from_bits(s.gain_r.load(Ordering::Relaxed)),
        )
    }

    /// The tap writer, for an output outside the engine; `None` when the engine has an output
    /// adapter (its renderer feeds the tap) or it was already taken.
    pub fn take_tap(&self) -> Option<TapWriter> {
        self.inner
            .tap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Remember the format this handle's owner last started, for [`Self::pace`].
    fn record_pace(&self, rate: u32, channels: u16) {
        let Some(i) = self.owner_index() else {
            return;
        };
        let mut paces = self.inner.paces.lock().unwrap_or_else(|e| e.into_inner());
        if paces.len() <= i {
            paces.resize(i + 1, None);
        }
        paces[i] = Some((rate, channels));
    }

    /// The owner table index of this handle; `None` for a plain handle.
    fn owner_index(&self) -> Option<usize> {
        usize::try_from(self.owner).ok()?.checked_sub(1)
    }

    /// Sleep for the duration of `samples` dropped interleaved samples at this owner's last
    /// track format (else stereo at the device rate), at most [`PACE_CAP`], in `POLL` slices.
    /// With `until_owner`, return as soon as this handle owns the engine again. Called without
    /// the producer mutex.
    fn pace(&self, samples: usize, until_owner: bool) {
        let recorded = self.owner_index().and_then(|i| {
            let paces = self.inner.paces.lock().unwrap_or_else(|e| e.into_inner());
            paces.get(i).copied().flatten()
        });
        let (rate, channels) =
            recorded.unwrap_or_else(|| (self.inner.shared.device_rate.load(Ordering::Acquire), 2));
        if rate == 0 || channels == 0 {
            return;
        }
        let frames = (samples / usize::from(channels)) as u64;
        let nanos = frames.saturating_mul(1_000_000_000) / u64::from(rate);
        let deadline = Instant::now() + Duration::from_nanos(nanos).min(PACE_CAP);
        loop {
            if until_owner && self.owns() {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            std::thread::sleep((deadline - now).min(POLL));
        }
    }

    fn interrupted(&self, st: &ProducerState) -> bool {
        self.inner.shared.content_epoch.load(Ordering::Acquire) > st.content_acked
    }

    /// Wait out an open in progress, fail on no or a failed output, and acknowledge a new
    /// output format in band (I7). Released, a caller with data to play (`demand` = the wanted
    /// rate, 0 = any) asks for an open once and waits for the answer (I13).
    fn ready(&self, st: &mut ProducerState, demand: Option<u32>) -> Result<(), SourceError> {
        let shared = &self.inner.shared;
        let mut start = Instant::now();
        let mut asked = false;
        loop {
            match shared.state() {
                CLOSED => return Err(SourceError::NoOutput),
                FAULTED => return Err(SourceError::OutputFailed),
                RELEASED => match demand {
                    Some(rate) if !asked => {
                        asked = true;
                        self.request_and_wait(rate)?;
                        start = Instant::now();
                        continue;
                    }
                    _ => return Err(SourceError::NoOutput),
                },
                OPENING => {
                    if start.elapsed() > self.inner.stall_timeout {
                        return Err(SourceError::Stalled);
                    }
                    std::thread::sleep(POLL);
                    continue;
                }
                _ => {}
            }
            let epoch = shared.format_epoch.load(Ordering::Acquire);
            if epoch == st.format_seen {
                return Ok(());
            }
            let rate = shared.device_rate.load(Ordering::Acquire);
            let depth = shared.depth.load(Ordering::Acquire) as usize;
            // Read rate and depth of exactly this open (an open may have restarted meanwhile).
            if shared.state() != OPEN || shared.format_epoch.load(Ordering::Acquire) != epoch {
                continue;
            }
            st.device_rate = rate;
            st.depth = depth.clamp(1, self.inner.capacity);
            let content = st.content_acked;
            self.push_mark(st, MarkKind::Format { epoch, content })?;
            st.format_seen = epoch;
            // The reopen discarded the ring: rebuild the resampler for the new rate (no drain)
            // and tell the renderer when that changes whether the track is resampled.
            if let Some(track) = st.track {
                let was = st.resample.is_some();
                st.resample = None;
                if track.rate != rate {
                    st.resample = Some(Resample::new(track.rate, rate)?);
                }
                let resampled = st.resample.is_some();
                if resampled != was && !st.ended {
                    let info = TrackInfo {
                        format: track,
                        resampled,
                    };
                    self.push_mark(st, MarkKind::Track(info))?;
                }
            }
            return Ok(());
        }
    }

    /// Raise an open request for `rate` and wait until the control side answers it (I12).
    /// `Err(NoOutput)` after `request_timeout`; the caller re-checks the output state.
    fn request_and_wait(&self, rate: u32) -> Result<(), SourceError> {
        let shared = &self.inner.shared;
        shared.req_rate.store(rate, Ordering::Release);
        let seq = shared.req_seq.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(hook) = shared.hook.get() {
            hook();
        }
        let start = Instant::now();
        loop {
            if shared.ans_seq.load(Ordering::Acquire) >= seq {
                return Ok(());
            }
            if start.elapsed() > self.inner.request_timeout {
                return Err(SourceError::NoOutput);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until the renderer has taken everything queued (I14). `false` when it stopped
    /// consuming for `stall_timeout` or the output stopped being open; `true` when the ring is
    /// empty or its content is being dropped anyway (interrupt, reopen).
    fn wait_ring_empty(&self, st: &ProducerState) -> bool {
        let shared = &self.inner.shared;
        let mut start = Instant::now();
        let mut last_free = usize::MAX;
        loop {
            let free = st.audio.slots();
            if free == self.inner.capacity
                || self.interrupted(st)
                || shared.format_epoch.load(Ordering::Acquire) != st.format_seen
            {
                return true;
            }
            if shared.state() != OPEN {
                return false;
            }
            if free != last_free {
                start = Instant::now();
                last_free = free;
            }
            if start.elapsed() > self.inner.stall_timeout {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Push a mark at the current write position, waiting while the mark ring is full.
    fn push_mark(&self, st: &mut ProducerState, kind: MarkKind) -> Result<(), SourceError> {
        let mark = Mark {
            at: st.write_pos,
            kind,
        };
        let start = Instant::now();
        loop {
            if st.marks.push(mark).is_ok() {
                return Ok(());
            }
            match self.inner.shared.state() {
                CLOSED | RELEASED => return Err(SourceError::NoOutput),
                FAULTED => return Err(SourceError::OutputFailed),
                _ => {}
            }
            if start.elapsed() > self.inner.stall_timeout {
                return Err(SourceError::Stalled);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait until the ring holds at most `depth - n` frames (I8).
    fn wait_space(&self, st: &mut ProducerState, n: usize) -> Result<Space, SourceError> {
        let shared = &self.inner.shared;
        let mut start = Instant::now();
        let mut last_free = usize::MAX;
        loop {
            if self.interrupted(st) || shared.format_epoch.load(Ordering::Acquire) != st.format_seen
            {
                return Ok(Space::Dropped);
            }
            match shared.state() {
                CLOSED | RELEASED => return Err(SourceError::NoOutput),
                FAULTED => return Err(SourceError::OutputFailed),
                _ => {}
            }
            let free = st.audio.slots();
            let filled = self.inner.capacity - free;
            if filled + n <= st.depth && n <= free {
                return Ok(Space::Room);
            }
            if free != last_free {
                start = Instant::now();
                last_free = free;
            }
            if start.elapsed() > self.inner.stall_timeout {
                return Err(SourceError::Stalled);
            }
            std::thread::sleep(POLL);
        }
    }
}

fn lock_owners(inner: &Inner) -> MutexGuard<'_, Vec<String>> {
    inner.owners.lock().unwrap_or_else(|e| e.into_inner())
}

/// The token of owner `id`, interned on first use (never 0).
fn token(owners: &mut Vec<String>, id: &str) -> u64 {
    let i = match owners.iter().position(|o| o == id) {
        Some(i) => i,
        None => {
            owners.push(id.to_owned());
            owners.len() - 1
        }
    };
    i as u64 + 1
}

impl OwnerSwitch {
    /// The current owner's id; `None` while no owner is set.
    pub fn owner(&self) -> Option<String> {
        let owners = lock_owners(&self.inner);
        let t = self.inner.shared.owner.load(Ordering::Acquire);
        let i = usize::try_from(t).ok()?.checked_sub(1)?;
        owners.get(i).cloned()
    }

    /// Make `id` the owner. A change of owner drops everything queued, as an interrupt does,
    /// so the previous owner's tail never plays in front of the new owner's audio; setting the
    /// current owner again does nothing.
    pub fn set_owner(&self, id: &str) {
        let mut owners = lock_owners(&self.inner);
        let t = token(&mut owners, id);
        self.change(t);
    }

    /// Back to no owner (every handle accepted), dropping what is queued if one was set.
    pub fn clear_owner(&self) {
        let _owners = lock_owners(&self.inner);
        self.change(0);
    }

    /// Store `token` and bump the content epoch, `owner_epoch` first (see `shared`). Called
    /// with the owner table locked, so changes never interleave.
    fn change(&self, token: u64) {
        let shared = &self.inner.shared;
        if shared.owner.load(Ordering::Acquire) == token {
            return;
        }
        shared.owner.store(token, Ordering::Release);
        loop {
            // An interrupt racing this retries it; `owner_epoch` is always visible before the
            // epoch it names.
            let e = shared.content_epoch.load(Ordering::Acquire);
            shared.owner_epoch.store(e + 1, Ordering::Release);
            if shared
                .content_epoch
                .compare_exchange(e, e + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

impl std::fmt::Debug for OwnerSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerSwitch")
    }
}

impl std::fmt::Debug for SourceWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceWriter")
    }
}
