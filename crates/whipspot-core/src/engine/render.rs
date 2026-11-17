//! The renderer: the real-time consumer an output adapter calls from its audio callback.
//!
//! `render` never allocates, frees, locks, logs, does I/O or panics (I1). Everything it gets
//! from other threads is `Copy` (marks, atomics); its scratch is allocated at construction and
//! longer callbacks are processed in blocks of at most `max_block` frames.
//!
//! The chain per block: pull (with the pause fade and the flush and interrupt declick, and the
//! track's ReplayGain from the frame its `Track` mark sits at) -> pre-gain and EQ (ramped) ->
//! tap -> volume and balance (ramped) -> limiter -> peak meter -> quantiser. Every stage has a
//! branch that leaves the samples untouched and records whether it ran in [`StageFlags`].

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use rtrb::Consumer;
use triple_buffer::{Input, Output};

use super::clock::DriftClock;
use super::controls::DspParams;
use super::eq::{EqFilter, Stages};
use super::error::OutputError;
use super::format::{DeviceFormat, MAX_RATE, MIN_RATE, ReplayGainMode, SampleFormat};
use super::gain::{GainRamp, RAMP_SECONDS};
use super::limiter::Limiter;
use super::output::{OutBuf, RenderTiming};
use super::quantise::{Tpdf, holds_source, quantise};
use super::shared::{FaultSignal, Mark, MarkKind, Shared};
use super::status::{EngineStatus, StageFlags, TrackInfo};
use super::tap::TapWriter;

/// Pause and resume fade length.
const FADE_SECONDS: f64 = 0.005;
/// Peak meter fall: 12 dB per second, as a natural-log factor per second.
const PEAK_FALL_LN_PER_S: f64 = 12.0 / 20.0 * std::f64::consts::LN_10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    /// No `Track` mark applied yet.
    Idle,
    /// A `Track` mark applied, no `End` since: a short callback is an underrun.
    Playing,
    /// `End` applied: silence after the tail is by design.
    Ended,
}

/// The real-time consumer of the engine's rings. Owned by the engine while no output is open
/// and by the output adapter's callback while one is.
pub struct Renderer {
    shared: Arc<Shared>,
    audio: Consumer<[f64; 2]>,
    marks: Consumer<Mark>,
    /// Frames popped or discarded (matches the producer's frame counter).
    read_pos: u64,
    content_acked: u64,
    format_acked: u64,
    track: Option<TrackInfo>,
    stream: Stream,
    device: Option<DeviceFormat>,
    scratch: Box<[[f64; 2]]>,
    /// Current pause/resume fade gain (1.0 = no fade) and its per-frame step.
    fade_gain: f64,
    fade_step: f64,
    /// Declick (H26, D13): old frames still to render on the tail ramp before a flush's jump
    /// or an interrupt's discard, the tail's gain and its per-frame step. Separate from
    /// `fade_gain`, which restarts at 0 for the new content.
    tail_left: usize,
    tail_gain: f64,
    tail_step: f64,
    /// Newest content epoch whose interrupt got its fade-out.
    declicked: u64,
    tap: Option<TapWriter>,
    status: Input<EngineStatus>,
    // DSP state (all sized at construction).
    params: Output<DspParams>,
    eq: EqFilter,
    gains: GainRamp,
    limiter: Limiter,
    dither: Tpdf,
    /// ReplayGain (D8): the mode last read from the parameters, the linear gain applied now,
    /// the track's target and a linear ramp toward it after a mode change (a track change
    /// jumps at its first frame). 1.0 with no ramp left bypasses the stage.
    rg_mode: ReplayGainMode,
    rg_gain: f64,
    rg_target: f64,
    rg_step: f64,
    rg_left: u32,
    rg_ramp: u32,
    // Counters.
    callbacks: u64,
    frames: u64,
    underruns: u64,
    clips: u64,
    limiter_frames: u64,
    cpu_total_ns: u64,
    /// Longest callback since the last open (H18).
    cpu_max_ns: u64,
    /// The output clock's drift fit since the last open (H18, MD138). `Copy`; nothing here
    /// allocates.
    clock: DriftClock,
    peak: f64,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("read_pos", &self.read_pos)
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

/// Per-callback flags gathered while pulling.
#[derive(Default)]
struct Pulled {
    filled: usize,
    short: bool,
    faded: bool,
    discarding: bool,
    /// The ReplayGain stage scaled some of the frames.
    rg: bool,
}

impl Renderer {
    pub(crate) fn new(
        shared: Arc<Shared>,
        audio: Consumer<[f64; 2]>,
        marks: Consumer<Mark>,
        max_block: usize,
        tap: Option<TapWriter>,
        status: Input<EngineStatus>,
        params: Output<DspParams>,
    ) -> Self {
        Renderer {
            shared,
            audio,
            marks,
            read_pos: 0,
            content_acked: 0,
            format_acked: 0,
            track: None,
            stream: Stream::Idle,
            device: None,
            scratch: vec![[0.0; 2]; max_block.max(1)].into_boxed_slice(),
            fade_gain: 1.0,
            fade_step: 1.0,
            tail_left: 0,
            tail_gain: 0.0,
            tail_step: 0.0,
            declicked: 0,
            tap,
            status,
            params,
            eq: EqFilter::default(),
            gains: GainRamp::default(),
            limiter: Limiter::default(),
            dither: Tpdf::default(),
            rg_mode: ReplayGainMode::Off,
            rg_gain: 1.0,
            rg_target: 1.0,
            rg_step: 0.0,
            rg_left: 0,
            rg_ramp: 1,
            callbacks: 0,
            frames: 0,
            underruns: 0,
            clips: 0,
            limiter_frames: 0,
            cpu_total_ns: 0,
            cpu_max_ns: 0,
            clock: DriftClock::default(),
            peak: 0.0,
        }
    }

    /// Set the device format the adapter opened. Call before the stream starts.
    pub fn prepare(&mut self, format: DeviceFormat) -> Result<(), OutputError> {
        if format.channels == 0
            || !(MIN_RATE..=MAX_RATE).contains(&format.rate)
            || !format.bits_consistent()
        {
            return Err(OutputError::Unsupported);
        }
        self.device = Some(format);
        self.fade_step = 1.0 / (f64::from(format.rate) * FADE_SECONDS);
        self.rg_ramp = ((f64::from(format.rate) * RAMP_SECONDS).round() as u32).max(1);
        self.limiter.configure(&format);
        if let Some(tap) = &self.tap {
            tap.set_rate(format.rate);
        }
        Ok(())
    }

    /// A handle the adapter uses to report a failed stream (any thread).
    pub fn fault_signal(&self) -> FaultSignal {
        FaultSignal {
            shared: self.shared.clone(),
        }
    }

    /// Fill `out` (interleaved, device channels). A buffer that does not match the prepared
    /// format, or an unprepared renderer, gets zeros.
    pub fn render(&mut self, mut out: OutBuf<'_>, timing: RenderTiming) {
        let t0 = Instant::now();
        let Some(dev) = self.device else {
            out.zero();
            return;
        };
        let matches = matches!(
            (&out, dev.sample),
            (OutBuf::F32(_), SampleFormat::F32)
                | (OutBuf::I16(_), SampleFormat::I16)
                | (OutBuf::I32(_), SampleFormat::I32)
        );
        if !matches {
            out.zero();
            return;
        }
        self.clock.tick(t0);
        let channels = dev.channels as usize;
        let total = out.len() / channels;
        if out.len() % channels != 0 {
            // A partial trailing frame: zero it, render the whole frames.
            out.zero();
        }

        self.declick_flush();

        // New parameters are taken at callback boundaries; the preamp and the EQ bands ramp
        // toward them (H8). A design for another rate is never applied (everything is bypassed
        // until the control side republishes for this device, then applied at once).
        let params = *self.params.read();
        if params.replay_gain != self.rg_mode {
            self.rg_mode = params.replay_gain;
            self.rg_retarget();
        }
        let rate_ok = params.eq.rate == dev.rate;
        let Stages { pre, eq: eq_on } = if rate_ok {
            self.eq.arm(&params.eq, params.pre_gain)
        } else {
            self.eq.reset();
            Stages::default()
        };
        let target = [
            f64::from_bits(self.shared.gain_l.load(Ordering::Relaxed)),
            f64::from_bits(self.shared.gain_r.load(Ordering::Relaxed)),
        ];
        let ramp_frames = (f64::from(dev.rate) * RAMP_SECONDS).round() as u32;
        self.gains.retarget(target, ramp_frames);
        let integer = dev.sample != SampleFormat::F32;

        let mut applied = StageFlags {
            exact: true,
            ..StageFlags::default()
        };
        let mut short = false;
        let mut discarding = false;
        let mut limited = 0u64;
        let mut block_peak = 0.0f64;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(self.scratch.len());
            let p = self.pull(n);
            short |= p.short;
            discarding |= p.discarding;
            let block = &mut self.scratch[..n];
            if pre || eq_on {
                self.eq.process(block);
            }
            // The tap is post-EQ, pre-volume (D36).
            if p.filled > 0 {
                if let Some(tap) = &mut self.tap {
                    tap.push_stereo(block[..p.filled].as_flattened());
                }
            }
            let gains_on = !self.gains.is_unity();
            if gains_on {
                let g0 = self.gains.current();
                let ramping = self.gains.ramping();
                self.gains.process(block);
                let g1 = self.gains.current();
                applied.volume |= ramping || g0[0].max(g0[1]) != 1.0 || g1[0].max(g1[1]) != 1.0;
                applied.balance |= g0[0] != g0[1] || g1[0] != g1[1];
            }
            let chain = pre || eq_on || gains_on || p.rg;
            if chain {
                limited += self.limiter.process(block);
            } else {
                self.limiter.reset();
            }
            for f in &block[..p.filled] {
                block_peak = block_peak.max(f[0].abs()).max(f[1].abs());
            }
            let resampled = self.track.is_some_and(|t| t.resampled);
            let holds = self.track.is_none_or(|t| holds_source(&t.format, &dev));
            let exact = !chain && !p.faded && channels >= 2 && !resampled && holds;
            // Dither follows DSP that changes the steady-state signal, not transport fades
            // (MD27 as decided in step 2): a fade alone rounds without dither.
            let dither = integer && (chain || channels < 2 || resampled || !holds);
            quantise(
                block,
                &mut out,
                done,
                &dev,
                &mut self.clips,
                if dither { Some(&mut self.dither) } else { None },
            );
            applied.replay_gain |= p.rg;
            applied.limiter |= chain;
            applied.fade |= p.faded;
            applied.dither |= dither;
            applied.exact &= exact;
            done += n;
        }
        applied.pre_gain = pre;
        applied.eq = eq_on;
        applied.limiter_engaged = limited > 0;

        if short {
            self.underruns += 1;
        }
        self.callbacks += 1;
        self.frames += total as u64;
        self.limiter_frames += limited;
        // Peak hold falling about 12 dB/s.
        let fall = (-PEAK_FALL_LN_PER_S * total as f64 / f64::from(dev.rate)).exp();
        self.peak = block_peak.max(self.peak * fall);
        let cpu_ns = t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.cpu_total_ns = self.cpu_total_ns.saturating_add(cpu_ns);
        self.cpu_max_ns = self.cpu_max_ns.max(cpu_ns);
        self.clock.rendered(total);

        let status = EngineStatus {
            callbacks: self.callbacks,
            frames: self.frames,
            underruns: self.underruns,
            tap_drops: self.shared.tap_drops.load(Ordering::Relaxed),
            clips: self.clips,
            limiter_frames: self.limiter_frames,
            applied,
            track: self.track,
            ending: self.stream == Stream::Ended,
            paused: self.shared.paused.load(Ordering::Relaxed),
            discarding,
            ring_fill: self.audio.slots().min(u32::MAX as usize) as u32,
            ring_depth: self.shared.depth.load(Ordering::Relaxed),
            peak: self.peak,
            cpu_avg_ns: self.cpu_total_ns / self.callbacks,
            cpu_max_ns: self.cpu_max_ns,
            clock: self.clock.fit(),
            latency_ns: timing.latency_ns,
            block_frames: total.min(u32::MAX as usize) as u32,
            device: Some(dev),
            source_frames: self.shared.frames_written.load(Ordering::Relaxed),
            headroom_db: if rate_ok { params.headroom_db } else { 0.0 },
            replay_gain_db: if self.rg_target == 1.0 {
                0.0
            } else {
                (20.0 * self.rg_target.log10()) as f32
            },
        };
        self.status.write(status);
        // The one store the queued-duration reading costs the callback (H25).
        self.shared.read_pos.store(self.read_pos, Ordering::Relaxed);
    }

    /// Discard everything readable, applying the state of every mark (control side, at open).
    pub(crate) fn discard_readable(&mut self) {
        // A tail in progress is abandoned: the reopen silences (D13). The callback CPU maximum
        // and the drift anchor start over with the new stream (H18).
        self.tail_left = 0;
        self.cpu_max_ns = 0;
        self.clock.reset();
        self.discard_marks_and_audio();
        self.shared.read_pos.store(self.read_pos, Ordering::Relaxed);
    }

    fn discard_marks_and_audio(&mut self) {
        loop {
            let avail = self.audio.slots();
            match self.marks.peek().ok().copied() {
                Some(m) if m.at <= self.read_pos => {
                    let _ = self.marks.pop();
                    self.apply(m);
                }
                Some(m) => {
                    let k = avail.min((m.at - self.read_pos) as usize);
                    if k == 0 || !self.skip(k) {
                        return;
                    }
                }
                None => {
                    if avail == 0 || !self.skip(avail) {
                        return;
                    }
                }
            }
        }
    }

    /// A `Flush` mark visible at the callback start (seek, manual skip, an acknowledged
    /// interrupt) starts the declick tail: up to 5 ms of the old content still ahead of it on
    /// a ramp to zero, then the jump (H26). Nothing while a tail runs (it ends with the jump to
    /// the newest flush).
    fn declick_flush(&mut self) {
        if self.tail_left > 0 {
            return;
        }
        // Audio level first, then marks (see `shared`).
        let avail = self.audio.slots();
        if let Some(room) = self.tail_room(avail, true) {
            self.begin_tail(room);
        }
    }

    /// Frames the declick tail may take: what is readable (`avail`) up to the first `Flush` or
    /// `Format` mark (the old content ends there; `Track` and `End` marks inside are applied
    /// on the way as usual). `None` when `need_flush` and no `Flush` mark is visible.
    fn tail_room(&mut self, avail: usize, need_flush: bool) -> Option<usize> {
        let slots = self.marks.slots();
        let mut room = avail;
        let mut flush = false;
        if slots > 0 {
            if let Ok(chunk) = self.marks.read_chunk(slots) {
                let (a, b) = chunk.as_slices();
                flush = a
                    .iter()
                    .chain(b)
                    .any(|m| matches!(m.kind, MarkKind::Flush(_)));
                let cut = a
                    .iter()
                    .chain(b)
                    .find(|m| matches!(m.kind, MarkKind::Flush(_) | MarkKind::Format { .. }));
                if let Some(m) = cut {
                    room = room.min(m.at.saturating_sub(self.read_pos) as usize);
                }
                // Only peeked (dropped without a commit).
            }
        }
        (flush || !need_flush).then_some(room)
    }

    /// Start a tail of at most `room` frames from the gain the output has now (`fade_gain`,
    /// below 1 in a pause or resume fade, 0 when silent), at no steeper a slope than the
    /// pause fade's. A zero-length tail ends at once.
    fn begin_tail(&mut self, room: usize) {
        let g = self.fade_gain;
        let t = if g > 0.0 {
            ((g / self.fade_step).ceil() as usize).min(room)
        } else {
            0
        };
        if t == 0 {
            self.end_tail();
            return;
        }
        self.tail_left = t;
        self.tail_gain = g;
        self.tail_step = g / (t + 1) as f64;
    }

    /// The tail is over: jump to the newest visible flush (none after a bare interrupt, whose
    /// discard follows in `pull`) and fade the new content in from zero.
    fn end_tail(&mut self) {
        self.tail_left = 0;
        self.jump_to_last_flush();
        self.fade_gain = 0.0;
    }

    /// Drop everything up to the newest visible `Flush` mark, applying the state of every
    /// mark on the way.
    fn jump_to_last_flush(&mut self) {
        let slots = self.marks.slots();
        if slots == 0 {
            return;
        }
        let last = match self.marks.read_chunk(slots) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                let is_flush = |m: &Mark| matches!(m.kind, MarkKind::Flush(_));
                b.iter()
                    .rposition(is_flush)
                    .map(|i| a.len() + i)
                    .or_else(|| a.iter().rposition(is_flush))
            }
            Err(_) => None,
        };
        // The chunk was only peeked (dropped without a commit).
        let Some(last) = last else { return };
        for _ in 0..=last {
            let Ok(m) = self.marks.pop() else { return };
            if m.at > self.read_pos {
                // The producer commits audio before the mark that follows it.
                let k = ((m.at - self.read_pos) as usize).min(self.audio.slots());
                let _ = self.skip(k);
            }
            self.apply(m);
        }
    }

    fn apply(&mut self, m: Mark) {
        match m.kind {
            MarkKind::Track(info) => {
                self.track = Some(info);
                self.stream = Stream::Playing;
                // The new track's gain from its first frame (gapless: the sample it starts at).
                self.rg_target = self.rg_track_gain();
                self.rg_gain = self.rg_target;
                self.rg_left = 0;
            }
            MarkKind::End => self.stream = Stream::Ended,
            MarkKind::Flush(epoch) => self.content_acked = self.content_acked.max(epoch),
            MarkKind::Format { epoch, content } => {
                self.format_acked = self.format_acked.max(epoch);
                self.content_acked = self.content_acked.max(content);
            }
        }
    }

    /// Drop `k` readable frames; false when they are not readable (nothing dropped).
    fn skip(&mut self, k: usize) -> bool {
        match self.audio.read_chunk(k) {
            Ok(chunk) => {
                chunk.commit_all();
                self.read_pos += k as u64;
                true
            }
            Err(_) => false,
        }
    }

    /// Fill `scratch[..n]` from the ring (zeros after what is available).
    fn pull(&mut self, n: usize) -> Pulled {
        let want_c = self.shared.content_epoch.load(Ordering::Acquire);
        let want_f = self.shared.format_epoch.load(Ordering::Acquire);
        let paused = self.shared.paused.load(Ordering::Acquire);
        let mut p = Pulled::default();
        let mut i = 0;
        loop {
            // Audio level first, then marks (see `shared`).
            let avail = self.audio.slots();
            let next = self.marks.peek().ok().copied();
            if self.tail_left == 0 {
                // Declick (H26, D13): a new interrupt (not during a reopen, which silences
                // anyway) or a flush reached mid-callback fades out what is ahead first.
                let interrupt = want_c > self.content_acked
                    && want_c > self.declicked
                    && want_f <= self.format_acked;
                let flush = next.is_some_and(|m| matches!(m.kind, MarkKind::Flush(_)));
                if interrupt || flush {
                    self.declicked = self.declicked.max(want_c);
                    let room = self.tail_room(avail, false).unwrap_or(0);
                    self.begin_tail(room);
                    continue;
                }
            }
            if let Some(m) = next {
                if m.at <= self.read_pos {
                    let _ = self.marks.pop();
                    self.apply(m);
                    continue;
                }
            }
            let limit = next.map_or(usize::MAX, |m| (m.at - self.read_pos) as usize);
            if self.tail_left == 0 && (want_c > self.content_acked || want_f > self.format_acked) {
                p.discarding = true;
                let k = avail.min(limit);
                if k == 0 || !self.skip(k) {
                    break;
                }
                continue;
            }
            p.discarding = false;
            if i >= n || (paused && self.fade_gain <= 0.0 && self.tail_left == 0) {
                break;
            }
            let mut k = avail.min(limit).min(n - i);
            if self.tail_left > 0 {
                if k == 0 {
                    // Unreachable (the tail's frames were readable when it began); never
                    // leave it hanging.
                    self.end_tail();
                    continue;
                }
                // The tail ignores pause: it is already on its way to zero.
                k = k.min(self.tail_left);
                let Ok(chunk) = self.audio.read_chunk(k) else {
                    self.end_tail();
                    continue;
                };
                let (a, b) = chunk.as_slices();
                self.scratch[i..i + a.len()].copy_from_slice(a);
                self.scratch[i + a.len()..i + k].copy_from_slice(b);
                chunk.commit_all();
                self.read_pos += k as u64;
                p.faded = true;
                p.rg |= self.replay_gain(i, k);
                self.tail(i, k);
                i += k;
                if self.tail_left == 0 {
                    self.end_tail();
                }
                continue;
            }
            if k == 0 {
                p.short = self.stream == Stream::Playing && !paused;
                break;
            }
            if paused {
                // Fade out: consume only while the gain is above zero.
                let left = (self.fade_gain / self.fade_step).ceil() as usize;
                k = k.min(left.max(1));
            }
            let Ok(chunk) = self.audio.read_chunk(k) else {
                break;
            };
            let (a, b) = chunk.as_slices();
            self.scratch[i..i + a.len()].copy_from_slice(a);
            self.scratch[i + a.len()..i + k].copy_from_slice(b);
            chunk.commit_all();
            self.read_pos += k as u64;
            p.rg |= self.replay_gain(i, k);
            if paused || self.fade_gain < 1.0 {
                p.faded = true;
                self.fade(i, k, paused);
            }
            i += k;
        }
        self.scratch[i..n].fill([0.0; 2]);
        p.filled = i;
        p
    }

    /// The current track's ReplayGain under the current mode (1.0: bypassed).
    fn rg_track_gain(&self) -> f64 {
        self.track
            .and_then(|t| t.format.source.replay_gain)
            .and_then(|rg| rg.linear(self.rg_mode))
            .unwrap_or(1.0)
    }

    /// The mode changed: ramp to the current track's new gain (no click).
    fn rg_retarget(&mut self) {
        self.rg_target = self.rg_track_gain();
        if self.rg_target == self.rg_gain {
            self.rg_left = 0;
            return;
        }
        self.rg_left = self.rg_ramp;
        self.rg_step = (self.rg_target - self.rg_gain) / f64::from(self.rg_ramp);
    }

    /// Apply ReplayGain to `scratch[i..i+k]`; `false` (nothing touched) while bypassed.
    fn replay_gain(&mut self, i: usize, k: usize) -> bool {
        if self.rg_left == 0 {
            if self.rg_gain == 1.0 {
                return false;
            }
            let g = self.rg_gain;
            for f in &mut self.scratch[i..i + k] {
                f[0] *= g;
                f[1] *= g;
            }
            return true;
        }
        for f in &mut self.scratch[i..i + k] {
            if self.rg_left > 0 {
                self.rg_left -= 1;
                self.rg_gain = if self.rg_left == 0 {
                    self.rg_target
                } else {
                    self.rg_gain + self.rg_step
                };
            }
            f[0] *= self.rg_gain;
            f[1] *= self.rg_gain;
        }
        true
    }

    /// Apply the pause (toward 0) or resume (toward 1) ramp to `scratch[i..i+k]`.
    fn fade(&mut self, i: usize, k: usize, out: bool) {
        let mut g = self.fade_gain;
        for f in &mut self.scratch[i..i + k] {
            g = if out {
                (g - self.fade_step).max(0.0)
            } else {
                (g + self.fade_step).min(1.0)
            };
            if out && g < 1e-12 {
                g = 0.0;
            }
            f[0] *= g;
            f[1] *= g;
        }
        self.fade_gain = g;
    }

    /// Apply the declick tail's ramp toward zero to `scratch[i..i+k]` (k <= `tail_left`).
    fn tail(&mut self, i: usize, k: usize) {
        let mut g = self.tail_gain;
        for f in &mut self.scratch[i..i + k] {
            g = (g - self.tail_step).max(0.0);
            f[0] *= g;
            f[1] *= g;
        }
        self.tail_gain = g;
        self.tail_left -= k;
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;
    use std::sync::atomic::AtomicU64;

    use rtrb::{Producer, RingBuffer};
    use triple_buffer::triple_buffer;

    use super::*;
    use crate::engine::controls::make_params;
    use crate::engine::eq::EqSettings;
    use crate::engine::format::{ReplayGain, SourceInfo, StreamFormat};

    const RATE: u32 = 44_100;
    /// 10 ms callbacks; every parameter change lands on a callback boundary.
    const CALLBACK: usize = 441;

    /// A renderer on an F32 stereo device, its rings' producers and its parameter input.
    struct Rig {
        renderer: Renderer,
        audio: Producer<[f64; 2]>,
        marks: Producer<Mark>,
        params: Input<DspParams>,
        status: Output<EngineStatus>,
    }

    fn rig(frames: usize) -> Rig {
        let shared = Arc::new(Shared::new(Arc::new(AtomicU64::new(0))));
        let (audio, audio_c) = RingBuffer::new(frames);
        let (marks, marks_c) = RingBuffer::new(4);
        let (status, status_out) = triple_buffer(&EngineStatus::default());
        let (params, params_out) = triple_buffer(&DspParams::default());
        let mut renderer = Renderer::new(shared, audio_c, marks_c, 8_192, None, status, params_out);
        renderer
            .prepare(DeviceFormat {
                rate: RATE,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32,
            })
            .unwrap();
        Rig {
            renderer,
            audio,
            marks,
            params,
            status: status_out,
        }
    }

    fn eq(f: impl FnOnce(&mut EqSettings)) -> EqSettings {
        let mut s = EqSettings::default();
        f(&mut s);
        s
    }

    /// RBJ high-pass, Q = 1/√2, as (b0, b1, b2, a1, a2).
    fn highpass(freq: f64) -> [f64; 5] {
        let w0 = 2.0 * PI * freq / f64::from(RATE);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / 2.0 * std::f64::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        [
            (1.0 + cos) / 2.0 / a0,
            -(1.0 + cos) / a0,
            (1.0 + cos) / 2.0 / a0,
            -2.0 * cos / a0,
            (1.0 - alpha) / a0,
        ]
    }

    /// The left channel above 8 kHz (four cascaded high-passes, 48 dB per octave): a 1 kHz
    /// tone leaves about −144 dB, a click leaves its broadband energy.
    fn above_8k(x: &[f64]) -> Vec<f64> {
        let c = highpass(8_000.0);
        let mut y = x.to_vec();
        for _ in 0..4 {
            let (mut z1, mut z2) = (0.0, 0.0);
            for s in y.iter_mut() {
                let v = c[0] * *s + z1;
                z1 = c[1] * *s - c[3] * v + z2;
                z2 = c[2] * *s - c[4] * v;
                *s = v;
            }
        }
        y
    }

    /// Click energy of `hf` after each change: the high-passed energy in the 40 ms after it
    /// minus that of the 40 ms before (the steady signal's own).
    fn clicks(hf: &[f64], changes: &[usize]) -> Vec<f64> {
        let w = RATE as usize / 25;
        let energy = |r: std::ops::Range<usize>| hf[r].iter().map(|v| v * v).sum::<f64>();
        changes
            .iter()
            .map(|&at| (energy(at..at + w) - energy(at - w..at)).max(0.0))
            .collect()
    }

    #[test]
    fn eq_change_is_click_free() {
        let frames = RATE as usize * 4 / 5;
        // 1 kHz at −6 dBFS, 1 rad off a zero crossing at every change (each change lands on
        // a whole number of cycles): a swap at a zero crossing would hide its step.
        let input: Vec<[f64; 2]> = (0..frames)
            .map(|i| {
                let v = 0.5 * (2.0 * PI * 1000.0 * i as f64 / f64::from(RATE) + 1.0).sin();
                [v, v]
            })
            .collect();
        // The 1 kHz band 0 → +12 dB, back to 0, then the lowest band (the 60 Hz shelf) to −12.
        let changes = [
            (RATE as usize / 5, eq(|s| s.bands_db[4] = 12.0)),
            (RATE as usize * 2 / 5, EqSettings::default()),
            (RATE as usize * 3 / 5, eq(|s| s.bands_db[0] = -12.0)),
        ];
        let at: Vec<usize> = changes.iter().map(|c| c.0).collect();
        assert!(at.iter().all(|a| a % CALLBACK == 0));

        // Through the renderer: the parameters are published between callbacks.
        let mut r = rig(frames);
        for f in &input {
            r.audio.push(*f).unwrap();
        }
        r.params.write(make_params(&EqSettings::default(), RATE));
        let mut out = vec![0.0f32; 2 * frames];
        for (k, buf) in out.chunks_mut(2 * CALLBACK).enumerate() {
            if let Some((_, s)) = changes.iter().find(|c| c.0 == k * CALLBACK) {
                r.params.write(make_params(s, RATE));
            }
            r.renderer.render(OutBuf::F32(buf), RenderTiming::default());
        }
        let ramped: Vec<f64> = out.iter().step_by(2).map(|&v| f64::from(v)).collect();

        // The same changes as a hard coefficient swap (the filter before H8).
        let mut hard = input.clone();
        let mut f = EqFilter::default();
        let flat = make_params(&EqSettings::default(), RATE);
        f.hard_arm(&flat.eq, flat.pre_gain);
        let mut from = 0;
        for (to, s) in &changes {
            f.process(&mut hard[from..*to]);
            let p = make_params(s, RATE);
            f.hard_arm(&p.eq, p.pre_gain);
            from = *to;
        }
        f.process(&mut hard[from..]);
        let hard: Vec<f64> = hard.iter().map(|f| f64::from(f[0] as f32)).collect();

        let soft = clicks(&above_8k(&ramped), &at);
        let swap = clicks(&above_8k(&hard), &at);
        // Measured (debug and release alike): ramped [1.9e-6, 2.1e-6, 6.7e-9], hard swap
        // [1.5e-2, 0, 2.3e-7]. The ramped figures are the 32-frame coefficient steps' images,
        // about −83 dB under the tone over the 40 ms. At the band centre the headroom cancels
        // the boost exactly with zero phase, so the hard swap back to flat is itself silent
        // there; for that change only the absolute bound applies (about 10× the measured).
        for (i, (s, h)) in soft.iter().zip(&swap).enumerate() {
            assert!(*s < 2e-5, "change {i}: ramped {s:e}");
            if i != 1 {
                assert!(*s <= h / 4.0, "change {i}: ramped {s:e}, hard swap {h:e}");
            }
        }
        let (s_all, h_all) = (soft.iter().sum::<f64>(), swap.iter().sum::<f64>());
        assert!(s_all <= h_all / 4.0, "{s_all:e} vs {h_all:e}");
        // Flat again after the ramp back and the band's decay: bypassed and bit-exact.
        let settled = at[1] + RATE as usize * 3 / 20;
        assert!((settled..at[2]).all(|i| ramped[i] == f64::from(input[i][0] as f32)));
    }

    fn rg_track(rg: Option<ReplayGain>) -> TrackInfo {
        TrackInfo {
            format: StreamFormat {
                rate: RATE,
                channels: 2,
                source: SourceInfo {
                    codec: "flac",
                    bits: Some(24),
                    bitrate_kbps: None,
                    lossless: true,
                    replay_gain: rg,
                },
            },
            resampled: false,
        }
    }

    #[test]
    fn replay_gain_scales_in_f64_from_the_track_mark() {
        let n = 4_410;
        let mut r = rig(n);
        let rg = ReplayGain {
            track_db: Some(-6.02),
            ..ReplayGain::default()
        };
        r.marks
            .push(Mark {
                at: 0,
                kind: MarkKind::Track(rg_track(Some(rg))),
            })
            .unwrap();
        let input: Vec<[f64; 2]> = (0..n)
            .map(|i| {
                let v = 0.9 * (i as f64 * 0.013).sin();
                [v, -0.5 * v]
            })
            .collect();
        for f in &input {
            r.audio.push(*f).unwrap();
        }
        r.params.write(DspParams {
            replay_gain: ReplayGainMode::Track,
            ..make_params(&EqSettings::default(), RATE)
        });
        let mut out = vec![0.0f32; 2 * n];
        r.renderer
            .render(OutBuf::F32(&mut out), RenderTiming::default());
        // Volume is at unity, so the block after the chain is the ReplayGain stage's output.
        let g = 10f64.powf(-6.02 / 20.0);
        for (i, (got, want)) in r.renderer.scratch[..n].iter().zip(&input).enumerate() {
            for c in 0..2 {
                assert!((got[c] - want[c] * g).abs() < 1e-12, "frame {i} ch {c}");
            }
        }
        let s = *r.status.read();
        assert!(s.applied.replay_gain && !s.applied.exact, "{:?}", s.applied);
        // measured 0 dB difference (f32 of the same value); bound 1e-4 (f32 rounding).
        assert!(
            (s.replay_gain_db + 6.02).abs() < 1e-4,
            "{}",
            s.replay_gain_db
        );
    }

    #[test]
    fn callback_cpu_max_and_drift_anchor_restart_at_open() {
        let frames = 200_000;
        let mut r = rig(frames);
        // No warm-up, so the four callbacks are all fitted.
        r.renderer.clock = DriftClock::with_warmup(std::time::Duration::ZERO);
        for i in 0..frames {
            let v = 0.25 * (i as f64 * 0.05).sin();
            r.audio.push([v, v]).unwrap();
        }
        // The EQ makes the long callback slow enough to dwarf a one-frame one.
        r.params
            .write(make_params(&eq(|s| s.bands_db[4] = 6.0), RATE));
        let mut big = vec![0.0f32; 2 * (frames - 4)];
        r.renderer
            .render(OutBuf::F32(&mut big), RenderTiming::default());
        let mut small = [0.0f32; 2];
        for _ in 0..3 {
            r.renderer
                .render(OutBuf::F32(&mut small), RenderTiming::default());
        }
        let s = *r.status.read();
        assert_eq!(s.callbacks, 4);
        assert!(s.cpu_max_ns >= s.cpu_avg_ns, "{s:?}");
        assert!(s.cpu_max_ns > 0);
        // The drift fit has its points (the long first callback lies well apart in time).
        assert!(s.clock.frames_per_second().is_some(), "{:?}", s.clock);
        let before = s.cpu_max_ns;

        r.renderer.discard_readable();
        r.renderer
            .render(OutBuf::F32(&mut small), RenderTiming::default());
        let s = *r.status.read();
        assert!(s.cpu_max_ns < before, "{} !< {before}", s.cpu_max_ns);
        // The fit starts over: one point, no slope yet.
        assert_eq!(s.clock.frames_per_second(), None);
    }
}
