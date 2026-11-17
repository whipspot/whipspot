//! Playback: one decode thread owned by the provider, driven by a command channel. It reads a
//! resolved stream through [`RangeSource`], decodes the AAC with Symphonia and writes the samples
//! into the engine's [`SourceWriter`]. Modelled on `whipspot-local`'s player, without gapless
//! and ReplayGain (D8 of the youtube-music task): a track that ends is simply over.
//!
//! The protocol against the writer (whose `write` blocks while the ring is full):
//! - `load` of a new track, `seek` and `stop` call `interrupt` on the caller's side before the
//!   command is sent, so a write the thread is blocked in returns at once (its data dropped).
//! - `pause` and `play` are done by the thread between two writes, so a write never waits on a
//!   paused output.
//! - `load` and `stop` also raise the cancel counter, so a stream read the thread is about to
//!   make is refused. A request already in flight ends at its 30 s limit.
//! - The thread holds no lock while it writes: the decoder state is its own.
//!
//! Every decode step runs under `catch_unwind`; a hostile stream ends its track with a fixed text.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::{Error as SymError, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};
use tokio::sync::{broadcast, oneshot};
use whipspot_core::engine::{SourceInfo, SourceWriter, StreamFormat};
use whipspot_core::events::UiEvent;

use crate::http::Http;
use crate::resolve::Stream;
use crate::source::{Cancel, RangeSource, Refresh};

/// Most packets in a row that fail to decode before the stream counts as corrupt.
const MAX_DECODE_ERRORS: u32 = 32;
/// How often a position event is sent while playing.
const POSITION_EVERY: Duration = Duration::from_secs(1);
/// How long before the end the player is asked for the next track.
const PRELOAD_BEFORE_MS: u64 = 10_000;
/// How long `load` waits for the thread to open the stream (a few ranged requests).
const LOAD_TIMEOUT: Duration = Duration::from_secs(90);
/// How long dropping the player waits for the thread.
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);

pub const CANNOT_OPEN: &str = "this track could not be played";
pub const NO_OUTPUT: &str = "the audio output is not available";
const NO_ANSWER: &str = "the player did not answer";
const STREAM_FAILED: &str = "this track's stream failed";
const OUTPUT_STOPPED: &str = "the audio output stopped";

/// What the thread needs to open one track.
pub struct Track {
    pub uri: String,
    pub stream: Stream,
    /// Resolves the stream again, once, when the link expires.
    pub refresh: Refresh,
}

enum Cmd {
    Load {
        track: Track,
        play: bool,
        position_ms: u32,
        cancel: Cancel,
        reply: oneshot::Sender<Result<(), &'static str>>,
    },
    Play,
    Pause,
    Stop,
    Seek(u32),
    Shutdown,
}

/// State the callers and the thread share; atomics and short-held locks only.
struct Flags {
    /// A track is loaded and has not ended (a load must interrupt it).
    sounding: AtomicBool,
    /// The current track's format label.
    label: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Player {
    tx: Sender<Cmd>,
    writer: SourceWriter,
    flags: Arc<Flags>,
    cancel: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn new(
        writer: SourceWriter,
        events: broadcast::Sender<UiEvent>,
        http: Arc<dyn Http>,
    ) -> Player {
        let (tx, rx) = mpsc::channel();
        let flags = Arc::new(Flags {
            sounding: AtomicBool::new(false),
            label: Mutex::new(None),
        });
        let mut worker = Worker {
            rx,
            writer: writer.clone(),
            events,
            http,
            flags: flags.clone(),
            cur: None,
            playing: false,
            last_position: Instant::now(),
            preload_sent: false,
        };
        let thread = std::thread::Builder::new()
            .name("whipspot-ytmusic-play".into())
            .spawn(move || worker.run())
            .ok();
        Player {
            tx,
            writer,
            flags,
            cancel: Arc::new(AtomicU64::new(0)),
            thread,
        }
    }

    pub async fn load(
        &self,
        track: Track,
        play: bool,
        position_ms: u32,
    ) -> Result<(), &'static str> {
        // The previous track's next read is refused, and a write it is blocked in returns.
        self.cancel.fetch_add(1, Ordering::AcqRel);
        if self.flags.sounding.load(Ordering::Acquire) {
            self.writer.interrupt();
        }
        let cancel = Cancel::new(self.cancel.clone());
        let (reply, answer) = oneshot::channel();
        self.send(Cmd::Load {
            track,
            play,
            position_ms,
            cancel,
            reply,
        })?;
        match tokio::time::timeout(LOAD_TIMEOUT, answer).await {
            Ok(Ok(result)) => result,
            _ => {
                // Do not let the thread start a track nobody waits for.
                self.cancel.fetch_add(1, Ordering::AcqRel);
                Err(NO_ANSWER)
            }
        }
    }

    pub fn play(&self) -> Result<(), &'static str> {
        self.send(Cmd::Play)
    }

    pub fn pause(&self) -> Result<(), &'static str> {
        self.send(Cmd::Pause)
    }

    pub fn stop(&self) -> Result<(), &'static str> {
        self.cancel.fetch_add(1, Ordering::AcqRel);
        self.writer.interrupt();
        self.send(Cmd::Stop)
    }

    pub fn seek(&self, position_ms: u32) -> Result<(), &'static str> {
        self.writer.interrupt();
        self.send(Cmd::Seek(position_ms))
    }

    pub fn label(&self) -> Option<String> {
        lock(&self.flags.label).clone()
    }

    fn send(&self, cmd: Cmd) -> Result<(), &'static str> {
        self.tx.send(cmd).map_err(|_| NO_ANSWER)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        // A read or a write the thread is about to make ends at once.
        self.cancel.fetch_add(1, Ordering::AcqRel);
        self.writer.interrupt();
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + JOIN_TIMEOUT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
        // Otherwise it is detached: it ends when its request does (at most 30 s).
    }
}

/// One decoded chunk's outcome.
enum Step {
    /// Samples are in `Decoder::buf` (possibly none after trimming).
    Data,
    End,
    Failed,
}

enum SeekOutcome {
    Done,
    PastEnd,
    Failed,
}

struct Decoder {
    uri: String,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    rate: u32,
    channels: u16,
    time_base: Option<TimeBase>,
    n_frames: Option<u64>,
    duration_ms: u64,
    info: SourceInfo,
    /// Interleaved samples of the last chunk.
    buf: Vec<f64>,
    /// Frames handed out since the start (or the seek target).
    pos_frames: u64,
    /// The end of the last decoded packet, in frames.
    decoded_frames: u64,
    /// After a seek: the first frame to hand out.
    skip_to: Option<u64>,
    errors_in_row: u32,
    /// Turns true when a stop or a newer load replaces this track.
    cancel: Cancel,
}

impl Decoder {
    fn open(http: Arc<dyn Http>, track: Track, cancel: Cancel) -> Result<Decoder, &'static str> {
        catch_unwind(AssertUnwindSafe(|| {
            Decoder::open_inner(http, track, cancel)
        }))
        .unwrap_or(Err(CANNOT_OPEN))
    }

    fn open_inner(
        http: Arc<dyn Http>,
        track: Track,
        cancel: Cancel,
    ) -> Result<Decoder, &'static str> {
        let Track {
            uri,
            stream,
            refresh,
        } = track;
        let abr = stream.abr_kbps;
        let source =
            RangeSource::open(http, stream, refresh, cancel.clone()).map_err(|_| CANNOT_OPEN)?;
        let mss = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("m4a");
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|_| CANNOT_OPEN)?;
        let t = format.default_track(TrackType::Audio).ok_or(CANNOT_OPEN)?;
        let params = t
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(CANNOT_OPEN)?
            .clone();
        // Only AAC is read from this service (the decoder has no other codec).
        if params.codec != CODEC_ID_AAC {
            return Err(CANNOT_OPEN);
        }
        let (track_id, time_base, n_frames) = (t.id, t.time_base, t.num_frames);
        let rate = params.sample_rate.ok_or(CANNOT_OPEN)?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .ok_or(CANNOT_OPEN)?;
        let channels = u16::try_from(channels)
            .ok()
            .filter(|c| (1..=2).contains(c))
            .ok_or(CANNOT_OPEN)?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|_| CANNOT_OPEN)?;
        let n_frames = n_frames.filter(|n| *n > 0);
        let duration_ms = n_frames.map_or(0, |n| n.saturating_mul(1000) / u64::from(rate));
        Ok(Decoder {
            uri,
            format,
            decoder,
            track_id,
            rate,
            channels,
            time_base,
            n_frames,
            duration_ms,
            // The bitrate is the itag's, only when yt-dlp's figure agreed with it (rule 15).
            info: SourceInfo {
                codec: "AAC",
                bits: None,
                bitrate_kbps: abr,
                lossless: false,
                replay_gain: None,
            },
            buf: Vec::new(),
            pos_frames: 0,
            decoded_frames: 0,
            skip_to: None,
            errors_in_row: 0,
            cancel,
        })
    }

    fn stream_format(&self) -> StreamFormat {
        StreamFormat {
            rate: self.rate,
            channels: self.channels,
            source: self.info,
        }
    }

    fn label(&self) -> String {
        let khz = f64::from(self.rate) / 1000.0;
        let rate = if self.rate % 1000 == 0 {
            format!("{} kHz", self.rate / 1000)
        } else {
            format!("{khz:.1} kHz")
        };
        match self.info.bitrate_kbps {
            Some(k) => format!("{} {k} kbps {rate}", self.info.codec),
            None => format!("{} {rate}", self.info.codec),
        }
    }

    fn position_ms(&self) -> u32 {
        u32::try_from(self.pos_frames.saturating_mul(1000) / u64::from(self.rate))
            .unwrap_or(u32::MAX)
    }

    /// A timestamp in the track's time base as a frame index.
    fn ts_frames(&self, ts: Timestamp) -> u64 {
        let ts = u64::try_from(ts.get()).unwrap_or(0);
        match self.time_base {
            Some(tb) => {
                let v = u128::from(ts) * u128::from(tb.numer.get()) * u128::from(self.rate)
                    / u128::from(tb.denom.get());
                u64::try_from(v).unwrap_or(u64::MAX)
            }
            None => ts,
        }
    }

    fn next(&mut self) -> Step {
        catch_unwind(AssertUnwindSafe(|| self.next_inner())).unwrap_or(Step::Failed)
    }

    fn next_inner(&mut self) -> Step {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Step::End,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // The reader gives a clean end of file as `Ok(0)`, which the demuxer reports
                    // so; a failed fetch is another error kind and fails the track.
                    return match self.n_frames {
                        Some(n) if self.decoded_frames < n => Step::Failed,
                        _ => Step::End,
                    };
                }
                Err(_) => return Step::Failed,
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let start = self.ts_frames(packet.pts);
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) => {
                    self.errors_in_row += 1;
                    if self.errors_in_row > MAX_DECODE_ERRORS {
                        return Step::Failed;
                    }
                    continue;
                }
                Err(_) => return Step::Failed,
            };
            self.errors_in_row = 0;
            let frames = decoded.frames();
            if frames == 0 {
                continue;
            }
            self.buf.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut self.buf);
            let ch = usize::from(self.channels);
            let have = self.buf.len() / ch;
            self.buf.truncate(have * ch);
            self.decoded_frames = start + have as u64;
            if let Some(target) = self.skip_to {
                let end = start + have as u64;
                if end <= target {
                    continue;
                }
                let drop = target.saturating_sub(start) as usize;
                self.buf.drain(..drop * ch);
                self.skip_to = None;
            }
            self.pos_frames += (self.buf.len() / ch) as u64;
            return Step::Data;
        }
    }

    fn seek(&mut self, ms: u32) -> SeekOutcome {
        catch_unwind(AssertUnwindSafe(|| self.seek_inner(ms))).unwrap_or(SeekOutcome::Failed)
    }

    fn seek_inner(&mut self, ms: u32) -> SeekOutcome {
        let target = (u64::from(ms) * u64::from(self.rate) + 500) / 1000;
        if self.n_frames.is_some_and(|n| target >= n) {
            return SeekOutcome::PastEnd;
        }
        let to = SeekTo::Time {
            time: Time::from_millis_u64(u64::from(ms)),
            track_id: Some(self.track_id),
        };
        match self.format.seek(SeekMode::Accurate, to) {
            Ok(_) => {}
            Err(SymError::SeekError(SeekErrorKind::OutOfRange)) => return SeekOutcome::PastEnd,
            Err(_) => return SeekOutcome::Failed,
        }
        self.decoder.reset();
        self.skip_to = Some(target);
        self.pos_frames = target;
        self.decoded_frames = 0;
        self.errors_in_row = 0;
        SeekOutcome::Done
    }
}

struct Worker {
    rx: Receiver<Cmd>,
    writer: SourceWriter,
    events: broadcast::Sender<UiEvent>,
    http: Arc<dyn Http>,
    flags: Arc<Flags>,
    cur: Option<Decoder>,
    playing: bool,
    last_position: Instant,
    preload_sent: bool,
}

impl Worker {
    fn run(&mut self) {
        loop {
            let cmd = if self.playing && self.cur.is_some() {
                match self.rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                // Idle or paused: wait for a command (with a timeout so a lost sender ends us).
                match self.rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            match cmd {
                Some(Cmd::Shutdown) => return,
                Some(cmd) => self.handle(cmd),
                None => self.step(),
            }
        }
    }

    fn emit(&self, e: UiEvent) {
        // No receivers is fine.
        let _ = self.events.send(e);
    }

    fn set_label(&self) {
        *lock(&self.flags.label) = self.cur.as_ref().map(Decoder::label);
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load {
                track,
                play,
                position_ms,
                cancel,
                reply,
            } => {
                let result = self.load(track, play, position_ms, cancel);
                let _ = reply.send(result);
            }
            Cmd::Play => {
                if let Some(d) = &self.cur {
                    self.playing = true;
                    self.writer.resume();
                    self.emit(UiEvent::Playing {
                        uri: d.uri.clone(),
                        position_ms: d.position_ms(),
                    });
                }
            }
            Cmd::Pause => {
                if let Some(d) = &self.cur {
                    self.playing = false;
                    self.writer.pause();
                    self.emit(UiEvent::Paused {
                        uri: d.uri.clone(),
                        position_ms: d.position_ms(),
                    });
                }
            }
            Cmd::Stop => {
                self.playing = false;
                self.flags.sounding.store(false, Ordering::Release);
                let done = self.cur.take();
                // The label first: whoever hears `Stopped` finds no format left.
                self.set_label();
                if let Some(d) = done {
                    // The caller interrupted: nothing queued plays; mark the end.
                    self.writer.finish();
                    self.emit(UiEvent::Stopped { uri: d.uri });
                }
            }
            Cmd::Seek(ms) => {
                let Some(d) = self.cur.as_mut() else {
                    return;
                };
                let outcome = d.seek(ms);
                // Acknowledges the caller's interrupt: what follows plays.
                self.writer.flush();
                match outcome {
                    SeekOutcome::Done => {
                        let (uri, position_ms) = (d.uri.clone(), d.position_ms());
                        self.last_position = Instant::now();
                        self.emit(UiEvent::Seeked { uri, position_ms });
                    }
                    SeekOutcome::PastEnd => self.end_track(),
                    SeekOutcome::Failed => self.fail_track(),
                }
            }
            Cmd::Shutdown => {}
        }
    }

    fn load(
        &mut self,
        track: Track,
        play: bool,
        position_ms: u32,
        cancel: Cancel,
    ) -> Result<(), &'static str> {
        self.cur = None;
        self.playing = false;
        self.set_label();
        let uri = track.uri.clone();
        self.emit(UiEvent::Loading {
            uri: uri.clone(),
            position_ms,
        });
        let mut d = match Decoder::open(self.http.clone(), track, cancel.clone()) {
            Ok(d) => d,
            Err(e) => {
                self.flags.sounding.store(false, Ordering::Release);
                if cancel.is_cancelled() {
                    // Replaced while it opened: not an unavailable track.
                    return Err(NO_ANSWER);
                }
                self.emit(UiEvent::Unavailable { uri });
                return Err(e);
            }
        };
        if cancel.is_cancelled() {
            // A stop or a newer load came while the stream opened: this track never sounds.
            return Err(NO_ANSWER);
        }
        let past_end = position_ms > 0 && !matches!(d.seek(position_ms), SeekOutcome::Done);
        if self.writer.start_track(d.stream_format()).is_err() {
            self.flags.sounding.store(false, Ordering::Release);
            return Err(NO_OUTPUT);
        }
        self.flags.sounding.store(true, Ordering::Release);
        self.preload_sent = false;
        self.last_position = Instant::now();
        let pos = d.position_ms();
        self.emit(UiEvent::TrackChanged {
            uri: uri.clone(),
            duration_ms: u32::try_from(d.duration_ms).unwrap_or(u32::MAX),
        });
        self.playing = play;
        if play {
            self.writer.resume();
            self.emit(UiEvent::Playing {
                uri,
                position_ms: pos,
            });
        } else {
            self.writer.pause();
            self.emit(UiEvent::Paused {
                uri,
                position_ms: pos,
            });
        }
        d.buf.clear();
        self.cur = Some(d);
        self.set_label();
        if past_end {
            self.end_track();
        }
        Ok(())
    }

    /// Decode one chunk and write it.
    fn step(&mut self) {
        let Some(d) = self.cur.as_mut() else {
            return;
        };
        match d.next() {
            Step::Data => {
                if d.buf.is_empty() {
                    return;
                }
                if self.writer.write(&d.buf).is_err() {
                    let (uri, pos) = (d.uri.clone(), d.position_ms());
                    self.playing = false;
                    self.writer.pause();
                    self.emit(UiEvent::error(OUTPUT_STOPPED));
                    self.emit(UiEvent::Paused {
                        uri,
                        position_ms: pos,
                    });
                    return;
                }
                let (uri, pos, duration) = (d.uri.clone(), d.position_ms(), d.duration_ms);
                let left = duration.saturating_sub(d.pos_frames * 1000 / u64::from(d.rate));
                if self.last_position.elapsed() >= POSITION_EVERY {
                    self.last_position = Instant::now();
                    self.emit(UiEvent::Position {
                        uri: uri.clone(),
                        position_ms: pos,
                    });
                }
                if !self.preload_sent && duration > 0 && left <= PRELOAD_BEFORE_MS {
                    self.preload_sent = true;
                    self.emit(UiEvent::PreloadNext { uri });
                }
            }
            Step::End => self.end_track(),
            Step::Failed => self.fail_track(),
        }
    }

    /// The current track is over: nothing follows it gaplessly, so the engine is told and the
    /// thread goes idle.
    fn end_track(&mut self) {
        let Some(done) = self.cur.take() else {
            return;
        };
        self.writer.end();
        self.playing = false;
        self.flags.sounding.store(false, Ordering::Release);
        self.set_label();
        self.emit(UiEvent::EndOfTrack { uri: done.uri });
    }

    /// The current stream failed mid-track: it ends with a fixed text.
    fn fail_track(&mut self) {
        let Some(done) = self.cur.take() else {
            return;
        };
        self.playing = false;
        self.writer.end();
        self.flags.sounding.store(false, Ordering::Release);
        self.set_label();
        if done.cancel.is_cancelled() {
            // A stop or a newer load cut the read: that is the caller's doing, not a failure.
            return;
        }
        self.emit(UiEvent::error(STREAM_FAILED));
        self.emit(UiEvent::EndOfTrack { uri: done.uri });
    }
}
