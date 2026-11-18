//! The control loop: the only writer of the player state. It reads actions, provider events and
//! task results; applies controls and queue edits at once; hands transport to the transport task
//! and commits a load only when that task reports success for the newest load. A superseded load
//! commits nothing while a newer one is pending; if the newest fails, the newest superseded load
//! that succeeded (the one really playing) is committed in place of the old state.
//!
//! Remote mode: a provider's remote (another device of the user's) takes control with
//! `UiEvent::RemoteStarted`. Then the remote chooses the tracks: its `Remote*` events set what
//! the state shows, the provider's plain state events are ignored, and the transport buttons go
//! to the remote. Stop, a local play or `RemoteEnded` leave remote mode; leaving never sets an
//! engine owner, the next load sets its own (D13).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::Shared;
use super::likes::{self, LikeQueues};
use super::lyrics::{Lookup, LyricsLookups, Step};
use super::output::{self, Outcome};
use super::queue::Queue;
use super::state::{
    LibraryPage, LibraryQuery, NowPlaying, PlayState, PlayerAction, PlayerNotice, PlayerState,
    RepeatMode, RequestId,
};
use super::transport::{Op, TransportCmd, TransportReport, can_take_over};
use super::{OUTPUT_STOPPED, texts};
use crate::engine::{BAND_COUNT, Engine, EqSettings, OutputMode, OutputRequest};
use crate::events::{RemotePlay, UiEvent};
use crate::lyrics::{Lyrics, LyricsError, LyricsQuery};
use crate::provider::{
    AuthFailure, LibErrorKind, LibraryError, Provider, ProviderAuthStatus, ProviderRegistry,
    RemoteCommand, Track,
};

/// Results sent back to the control loop by the transport task and the request tasks.
pub(crate) enum Internal {
    Transport(TransportReport),
    Status {
        provider: &'static str,
        result: Result<ProviderAuthStatus, AuthFailure>,
    },
    Login {
        provider: String,
        step: String,
        result: Result<ProviderAuthStatus, AuthFailure>,
    },
    Logout {
        provider: &'static str,
        result: Result<ProviderAuthStatus, AuthFailure>,
    },
    /// The source asked for an open (the engine's request hook); serviced by `check_output`.
    OutputRequest,
    /// A lyrics lookup ended.
    Lyrics {
        lookup: Lookup,
        result: Result<Lyrics, LyricsError>,
    },
    /// The old track's queued audio of hold `id` has been heard (H25): the switch is due.
    HoldDue(u64),
    /// A `set_liked` call ended; `writable` is `can_change_likes()` after it.
    LikeDone {
        provider: &'static str,
        uri: String,
        liked: bool,
        result: Result<(), LibErrorKind>,
        writable: bool,
    },
    /// A likes read ended: `saved` is None on an error or a panic; `writable` is
    /// `can_change_likes()` after it.
    LikedRead {
        read: u64,
        provider: &'static str,
        uris: Vec<String>,
        saved: Option<Vec<bool>>,
        writable: bool,
    },
}

/// The fallback states already announced, so each notice is posted once per change (MD40).
#[derive(Default)]
struct Announced {
    exclusive: bool,
    device: bool,
    failed: bool,
}

/// What a pending load commits to the queue when it succeeds.
enum Commit {
    /// `PlayItem`: the queue becomes `items`, `index` current.
    Replace { items: Vec<Track>, index: usize },
    /// `Next`, `Previous`, end of track: the list item at `index` becomes current.
    Jump(usize),
    /// Replaying a track that is no longer in the queue: the queue stays as it is.
    Keep,
}

/// One load handed to the transport task.
struct Load {
    seq: u64,
    track: Track,
    commit: Commit,
    /// `Control::queue_edits` when the load started.
    edits: u64,
    /// Where the track starts, and whether it plays or waits paused there (0 and playing,
    /// except a seek during a held gapless switch, which reloads the heard track).
    start: Start,
}

/// Where a load starts its track.
#[derive(Clone, Copy)]
struct Start {
    position_ms: u32,
    play: bool,
}

impl Start {
    const PLAY: Start = Start {
        position_ms: 0,
        play: true,
    };

    fn play_state(self) -> PlayState {
        if self.play {
            PlayState::Playing
        } else {
            PlayState::Paused
        }
    }
}

/// A superseded load that succeeded: its track is what the active provider plays.
struct Fallback {
    load: Load,
    provider: &'static str,
}

struct Pending {
    /// The newest load; only its success commits.
    load: Load,
    /// Older loads still on the transport task, oldest first (it reports in order).
    earlier: VecDeque<Load>,
    /// The newest superseded load that succeeded; committed if the newest load fails.
    fallback: Option<Fallback>,
    /// The play state of what really plays (the old current item, or the fallback), shown if
    /// the newest load fails (it shows `Loading` meanwhile). Every event and op result about
    /// what really plays that arrives while the load is pending lands here, not in the state.
    restore: PlayState,
    /// The position of what really plays, as `(position_ms, at)`, shown with `restore`.
    restore_pos: (u32, Instant),
}

impl Pending {
    /// The position of what really plays now.
    fn position_now(&self) -> u32 {
        let (ms, at) = self.restore_pos;
        if self.restore != PlayState::Playing {
            return ms;
        }
        let elapsed = u32::try_from(at.elapsed().as_millis()).unwrap_or(u32::MAX);
        ms.saturating_add(elapsed)
    }

    /// A play state (and position, if given; else the one it has now) of what really plays.
    fn record(&mut self, ps: PlayState, position_ms: Option<u32>) {
        let ms = position_ms.unwrap_or_else(|| self.position_now());
        self.restore = ps;
        self.restore_pos = (ms, Instant::now());
    }
}

/// How often a hold's watcher reads the renderer's frame counter (bounds the switch's lateness).
const HOLD_POLL: Duration = Duration::from_millis(5);
/// The most output latency a hold waits out on top of the ring.
const MAX_HOLD_LATENCY: Duration = Duration::from_millis(500);

/// A gapless switch held back until the old track's queued audio is heard (H25, D12). The end
/// of track starts the next load at once (the provider keeps decoding gaplessly); only the
/// now-playing switch waits, until the renderer passes the writer's frame counter as it was
/// when the end of track arrived, plus the output's latency. Pause holds it (the renderer
/// stops consuming).
struct Held {
    id: u64,
    /// The load the end of track started (`Control::seq`).
    seq: u64,
    /// That load once it succeeded, and its provider.
    loaded: Option<Fallback>,
    /// Ends the watcher.
    cancel: Arc<AtomicBool>,
}

pub(crate) struct Control {
    shared: Arc<Shared>,
    registry: Arc<ProviderRegistry>,
    engine: Engine,
    transport: UnboundedSender<TransportCmd>,
    internal: UnboundedSender<Internal>,
    queue: Queue,
    seq: u64,
    pending: Option<Pending>,
    /// Bumped by every queue edit (enqueue, remove, move, clear).
    queue_edits: u64,
    /// The user's output choice (also in `PlayerState.output`).
    device: Option<String>,
    mode: OutputMode,
    announced: Announced,
    lyrics: LyricsLookups,
    likes: LikeQueues,
    /// The gapless switch being held, if any.
    held: Option<Held>,
    holds: u64,
    /// A held switch's loaded track that a manual load supersedes: what really plays until that
    /// load commits (its fallback). Taken by the next `start_load`, else applied.
    carry: Option<Fallback>,
    /// How long an exclusive output may stay open while paused (H28, D14).
    pause_release: Duration,
    /// When the exclusive output is given back if the pause lasts; armed while paused with an
    /// exclusive output open, cleared by anything else (and re-armed by each action).
    pause_deadline: Option<tokio::time::Instant>,
    /// The provider whose remote controls playback (also `PlayerState.remote`): its transport
    /// buttons go to the remote and no local load is pending, held or carried.
    remote: Option<&'static str>,
    /// Remote mode ended and no local load has succeeded since: the transport has no active
    /// provider (the takeover took it), so the track shown (the remote's) is loaded nowhere and
    /// an op would reach no one. [`Control::op`] handles the ops itself then.
    unloaded: bool,
}

const DB_MAX: f32 = 12.0;

fn valid_db(db: f32) -> bool {
    db.is_finite() && (-DB_MAX..=DB_MAX).contains(&db)
}

impl Control {
    pub(crate) fn new(
        shared: Arc<Shared>,
        registry: Arc<ProviderRegistry>,
        engine: Engine,
        transport: UnboundedSender<TransportCmd>,
        internal: UnboundedSender<Internal>,
        lyrics: LyricsLookups,
        pause_release: Duration,
    ) -> Self {
        Control {
            shared,
            registry,
            engine,
            transport,
            internal,
            queue: Queue::default(),
            seq: 0,
            pending: None,
            queue_edits: 0,
            device: None,
            mode: OutputMode::Shared,
            announced: Announced::default(),
            lyrics,
            likes: LikeQueues::default(),
            held: None,
            holds: 0,
            carry: None,
            pause_release,
            pause_deadline: None,
            remote: None,
            unloaded: false,
        }
    }

    /// Take the initial output choice and act on it: shared opens
    /// now, exclusive is released until audio arrives (MD41). Runs once, before the loop.
    pub(crate) fn init_output(&mut self, request: OutputRequest) {
        let tx = self.internal.clone();
        self.engine.set_request_hook(Arc::new(move || {
            let _ = tx.send(Internal::OutputRequest);
        }));
        let modes = self.engine.modes();
        self.update(|s| s.output.modes = modes);
        self.device = request.device;
        self.mode = request.mode;
        let (device, mode) = (self.device.clone(), self.mode);
        self.update(|s| {
            s.output.device = device;
            s.output.mode = mode;
        });
        match self.mode {
            OutputMode::Exclusive => {
                self.engine.set_follow_rate(true);
                self.release_idle();
            }
            OutputMode::Shared => self.reopen(None),
        }
    }

    /// Reopen the chosen output (`rate` for an exclusive choice; 0 = any) through its fallback
    /// chain and show what opened. All attempts failing leaves the engine released (MD41).
    fn reopen(&mut self, rate: Option<u32>) {
        let rate = rate.filter(|&r| r != 0);
        let requests = output::chain(self.device.as_deref(), self.mode, rate);
        let reopened = self.engine.reopen(&requests);
        let o = output::outcome(&requests, &reopened);
        self.apply(o);
    }

    fn apply(&mut self, o: Outcome) {
        match o.opened {
            Some(mode) => {
                self.engine.set_follow_rate(mode == OutputMode::Exclusive);
                self.announced.failed = false;
                if o.exclusive_lost && !self.announced.exclusive {
                    self.error(texts::OUTPUT_EXCLUSIVE_FALLBACK);
                }
                if o.device_lost && !self.announced.device {
                    self.error(texts::OUTPUT_DEVICE_MISSING);
                }
                self.announced.exclusive = o.exclusive_lost;
                self.announced.device = o.device_lost;
            }
            None => {
                self.engine.release();
                if !self.announced.failed {
                    self.error(texts::OUTPUT_OPEN_FAILED);
                }
                self.announced.failed = true;
            }
        }
        self.update(|s| {
            s.output_summary = o.summary;
            s.output.fallback = o.fallback;
            s.output.idle = false;
        });
    }

    /// Release the output and show the exclusive idle summary.
    fn release_idle(&mut self) {
        self.engine.release();
        let summary = output::idle_summary(self.device.as_deref());
        self.update(|s| {
            s.output_summary = summary;
            s.output.fallback = None;
            s.output.idle = true;
        });
    }

    /// The play state became Stopped by the user's Stop or the end of the queue: an exclusive
    /// choice gives the device back (MD41). Pause keeps it.
    fn release_when_stopped(&mut self) {
        if self.mode == OutputMode::Exclusive
            && self.pending.is_none()
            && self.state().play_state == PlayState::Stopped
            && !self.engine.is_released()
        {
            self.release_idle();
        }
    }

    /// Whether the pause release applies now: paused with nothing loading, an exclusive output
    /// open (a shared one, or an exclusive choice that fell back to shared, is never released
    /// on pause).
    fn pause_release_applies(&self) -> bool {
        // A remote's pause is the remote's: no local seek or release behind its back.
        self.remote.is_none()
            && self.pending.is_none()
            && self.state().play_state == PlayState::Paused
            && self
                .engine
                .output()
                .is_some_and(|i| i.mode == OutputMode::Exclusive)
    }

    /// Keep the pause timer in step with the state: running (from when the pause began) while
    /// the release applies, else off.
    fn arm_pause_timer(&mut self) {
        if !self.pause_release_applies() {
            self.pause_deadline = None;
        } else if self.pause_deadline.is_none() {
            self.pause_deadline = Some(tokio::time::Instant::now() + self.pause_release);
        }
    }

    /// The pause lasted: give the exclusive device back (H28). The queued audio goes with it,
    /// so the provider is sought back to the position shown, and resuming reopens the device
    /// (the engine's on-demand open, MD41) and continues from there.
    fn pause_timed_out(&mut self) {
        self.pause_deadline = None;
        if !self.pause_release_applies() {
            return;
        }
        // A held gapless switch (H25) whose load is done (one still running is `pending`, which
        // the check above declines): the provider holds the next track, unheard, while the
        // state shows the old one. The old one's tail goes with the device, so the next track
        // becomes now-playing, paused at 0, and the seek below puts the provider there.
        self.apply_hold();
        let shown = self.state().position_now();
        self.release_idle();
        self.op(Op::Seek(shown));
    }

    /// `SetOutput`: store the choice (also when unchanged, to retry a fallback) and apply it.
    /// The ring's queued audio is discarded by the switch (at most the ring depth, I7).
    fn set_output(&mut self, device: Option<String>, mode: OutputMode) {
        self.device = device.clone();
        self.mode = mode;
        self.announced = Announced::default();
        self.update(|s| {
            s.output.device = device;
            s.output.mode = mode;
        });
        match mode {
            OutputMode::Shared => {
                self.engine.set_follow_rate(false);
                self.reopen(None);
            }
            OutputMode::Exclusive => {
                self.engine.set_follow_rate(true);
                let active = matches!(
                    self.state().play_state,
                    PlayState::Playing | PlayState::Paused
                );
                match self.engine.status().track {
                    Some(track) if active => self.reopen(Some(track.format.rate)),
                    _ => self.release_idle(),
                }
            }
        }
    }

    fn refresh_output_devices(&mut self) {
        let devices = self.engine.devices().unwrap_or_default();
        self.update(|s| s.output.devices = devices);
    }

    pub(crate) async fn run(
        mut self,
        mut actions: UnboundedReceiver<PlayerAction>,
        mut internal: UnboundedReceiver<Internal>,
        mut events: broadcast::Receiver<UiEvent>,
    ) {
        let mut events_open = true;
        loop {
            let deadline = self.pause_deadline;
            tokio::select! {
                a = actions.recv() => match a {
                    Some(a) => {
                        // Any action restarts the pause countdown.
                        self.pause_deadline = None;
                        self.on_action(a);
                    }
                    // Every handle is gone.
                    None => break,
                },
                Some(m) = internal.recv() => self.on_internal(m),
                ev = events.recv(), if events_open => match ev {
                    Ok(e) => self.on_event(e),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => events_open = false,
                },
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.pause_timed_out(),
            }
            self.check_output();
            self.arm_pause_timer();
        }
    }

    /// A fault the output raised (device lost, stream failed): the engine closed it; say so
    /// once and release, so the next audio asks for an open (MD41). Then service an open
    /// request from the source: declined while stopped with no load pending (a stale request
    /// included), else the chosen output reopens (at the wanted rate when exclusive).
    fn check_output(&mut self) {
        if self.engine.poll_fault().is_some() {
            self.error(texts::OUTPUT_LOST);
            self.engine.release();
            self.update(|s| {
                s.output_summary = OUTPUT_STOPPED.to_owned();
                s.output.fallback = None;
                s.output.idle = false;
            });
        }
        if let Some(rate) = self.engine.open_request() {
            // A remote may start its audio while the state shows it stopped: always answered.
            if self.pending.is_none()
                && self.remote.is_none()
                && self.state().play_state == PlayState::Stopped
            {
                self.engine.answer_request();
            } else {
                self.reopen(Some(rate));
            }
        }
    }

    fn state(&self) -> Arc<PlayerState> {
        self.shared.state()
    }

    fn update(&self, f: impl FnOnce(&mut PlayerState)) {
        self.shared.update(f);
    }

    fn error(&self, text: &'static str) {
        self.shared.notify(PlayerNotice::Error(text));
    }

    /// Send `op` to the active provider. With none after remote mode (`unloaded`, no load
    /// pending) nothing would get it: Play loads the track shown at the position shown, Seek
    /// moves the position shown (Play starts there), Stop is the stop it already is, and Pause
    /// has nothing to pause.
    fn op(&mut self, op: Op) {
        if !self.unloaded || self.pending.is_some() {
            let _ = self.transport.send(TransportCmd::Op(op));
            return;
        }
        match op {
            Op::Play => {
                let st = self.state();
                if let Some(np) = st.current.as_ref() {
                    let start = Start {
                        position_ms: st.position_ms,
                        play: true,
                    };
                    self.start_load_at(np.track.clone(), Commit::Keep, start);
                }
            }
            Op::Seek(ms) => self.set_position(ms),
            Op::Stop => {
                self.set_play_state(PlayState::Stopped, Some(0));
                self.release_when_stopped();
            }
            Op::Pause => {}
        }
    }

    fn publish_queue(&self) {
        let st = self.state();
        let q = self.queue.snapshot(st.repeat, st.current.is_some());
        self.update(|s| s.queue = q);
    }

    /// A play state reported by a provider or an op. While a load is pending the visible state
    /// stays `Loading`; the reported one becomes the state to restore if the load fails.
    fn set_play_state(&mut self, ps: PlayState, position_ms: Option<u32>) {
        if let Some(p) = self.pending.as_mut() {
            p.record(ps, position_ms);
            return;
        }
        self.update(|s| {
            if let Some(ms) = position_ms {
                s.position_ms = ms;
            } else {
                s.position_ms = s.position_now();
            }
            s.position_at = Instant::now();
            s.play_state = ps;
        });
    }

    /// A position (seek or progress) of what really plays; kept for the restore while a load is
    /// pending.
    fn set_position(&mut self, ms: u32) {
        if let Some(p) = self.pending.as_mut() {
            p.restore_pos = (ms, Instant::now());
            return;
        }
        self.update(|s| {
            s.position_ms = ms;
            s.position_at = Instant::now();
        });
    }

    fn on_action(&mut self, a: PlayerAction) {
        let Some(a) = self.remote_action(a) else {
            return;
        };
        match a {
            PlayerAction::Play => self.op(Op::Play),
            PlayerAction::Pause => self.op(Op::Pause),
            PlayerAction::Toggle => match self.state().play_state {
                PlayState::Playing | PlayState::Loading => self.op(Op::Pause),
                PlayState::Paused | PlayState::Stopped => self.op(Op::Play),
            },
            PlayerAction::Stop => {
                self.cancel_hold();
                self.settle_carry();
                self.op(Op::Stop);
            }
            PlayerAction::Seek { position_ms } => {
                // During a held switch the bar is the heard track's, but the provider already
                // plays the next one: a seek there reloads the heard track at the target.
                let held = self.held.is_some();
                self.cancel_hold();
                let reloaded = held && self.reload_heard(position_ms);
                self.settle_carry();
                if !reloaded {
                    self.op(Op::Seek(position_ms));
                }
            }
            PlayerAction::Next => {
                self.cancel_hold();
                self.advance();
                self.settle_carry();
            }
            PlayerAction::Previous => {
                let held = self.held.is_some();
                self.cancel_hold();
                self.previous(held);
                self.settle_carry();
            }
            PlayerAction::PlayItem { context, index } => {
                self.cancel_hold();
                if let Some(track) = context.get(index).cloned() {
                    self.start_load(
                        track,
                        Commit::Replace {
                            items: context,
                            index,
                        },
                    );
                }
                self.settle_carry();
            }
            PlayerAction::Enqueue(tracks) => {
                if !tracks.is_empty() {
                    self.queue.enqueue(tracks);
                    self.queue_edits += 1;
                    self.publish_queue();
                }
            }
            PlayerAction::RemoveFromQueue { index } => {
                if self.queue.remove(index) {
                    self.queue_edits += 1;
                    self.publish_queue();
                }
            }
            PlayerAction::MoveInQueue { from, to } => {
                if from != to && self.queue.move_item(from, to) {
                    self.queue_edits += 1;
                    self.publish_queue();
                }
            }
            PlayerAction::ClearQueue => {
                self.queue.clear();
                self.queue_edits += 1;
                self.publish_queue();
            }
            PlayerAction::SetShuffle(on) => {
                if self.state().shuffle != on {
                    self.queue.set_shuffle(on);
                    let st = self.state();
                    let q = self.queue.snapshot(st.repeat, st.current.is_some());
                    self.update(|s| {
                        s.shuffle = on;
                        s.queue = q;
                    });
                }
            }
            PlayerAction::SetRepeat(r) => {
                if self.state().repeat != r {
                    let q = self.queue.snapshot(r, self.state().current.is_some());
                    self.update(|s| {
                        s.repeat = r;
                        s.queue = q;
                    });
                }
            }
            PlayerAction::SetVolume(v) => {
                if v <= 100 {
                    self.engine.set_volume(v);
                    self.update(|s| s.volume = v);
                }
            }
            PlayerAction::SetBalance(b) => {
                if (-100..=100).contains(&b) {
                    self.engine.set_balance(b);
                    self.update(|s| s.balance = b);
                }
            }
            PlayerAction::SetReplayGain(mode) => {
                if self.state().replay_gain != mode {
                    self.engine.set_replay_gain(mode);
                    self.update(|s| s.replay_gain = mode);
                }
            }
            PlayerAction::SetEqEnabled(on) => self.set_eq(|e| e.enabled = on),
            PlayerAction::SetEqPreamp(db) => {
                if valid_db(db) {
                    self.set_eq(|e| e.preamp_db = db);
                }
            }
            PlayerAction::SetEqBand { index, gain_db } => {
                if index < BAND_COUNT && valid_db(gain_db) {
                    self.set_eq(|e| e.bands_db[index] = gain_db);
                }
            }
            PlayerAction::Login {
                provider,
                step,
                input,
            } => self.login(provider, step, input),
            PlayerAction::Logout { provider, step } => self.logout(provider, step),
            PlayerAction::Prefill {
                request,
                provider,
                step,
                input,
            } => self.prefill(request, provider, step, input),
            PlayerAction::RefreshAuth => self.refresh_auth(),
            PlayerAction::Library {
                request,
                provider,
                query,
            } => self.library(request, provider, query),
            PlayerAction::SetOutput { device, mode } => self.set_output(device, mode),
            PlayerAction::RefreshOutputDevices => self.refresh_output_devices(),
            PlayerAction::SetLyrics(on) => self.set_lyrics(on),
            PlayerAction::RequestLiked(uris) => {
                for (id, uris) in likes::group(&self.registry, uris) {
                    self.read_liked(id, uris);
                }
            }
            PlayerAction::SetLiked { uri, liked } => self.set_liked(uri, liked),
            PlayerAction::CheckLibrary => self.check_library(),
        }
    }

    fn set_eq(&mut self, f: impl FnOnce(&mut super::state::EqState)) {
        let mut eq = self.state().eq;
        f(&mut eq);
        self.engine.set_eq(EqSettings {
            enabled: eq.enabled,
            preamp_db: eq.preamp_db,
            bands_db: eq.bands_db,
        });
        self.update(|s| s.eq = eq);
    }

    // ------------------------------------------------------------ transport and queue

    fn start_load(&mut self, track: Track, commit: Commit) {
        self.start_load_at(track, commit, Start::PLAY);
    }

    /// Load `track` starting at `start`; false (with a notice) when no provider owns its uri.
    fn start_load_at(&mut self, track: Track, commit: Commit, start: Start) -> bool {
        if self.registry.resolve_uri(&track.uri).is_none() {
            self.error(texts::NOT_A_TRACK);
            return false;
        }
        self.seq += 1;
        let cmd = TransportCmd::Load {
            seq: self.seq,
            uri: track.uri.clone(),
            play: start.play,
            position_ms: start.position_ms,
        };
        let load = Load {
            seq: self.seq,
            track,
            commit,
            edits: self.queue_edits,
            start,
        };
        self.pending = Some(match self.pending.take() {
            Some(mut p) => {
                p.earlier.push_back(std::mem::replace(&mut p.load, load));
                p
            }
            None => match self.carry.take() {
                // A held switch's track plays (H25): it is what to restore if this load fails.
                Some(fallback) => Pending {
                    load,
                    earlier: VecDeque::new(),
                    fallback: Some(fallback),
                    restore: PlayState::Playing,
                    restore_pos: (0, Instant::now()),
                },
                None => {
                    let st = self.state();
                    Pending {
                        load,
                        earlier: VecDeque::new(),
                        fallback: None,
                        restore: st.play_state,
                        restore_pos: (st.position_ms, st.position_at),
                    }
                }
            },
        });
        let _ = self.transport.send(cmd);
        // A held gapless switch keeps showing the old track playing until its audio is heard.
        if self.held.is_none() {
            self.update(|s| s.play_state = PlayState::Loading);
        }
        true
    }

    fn on_loaded(&mut self, seq: u64, provider: Option<&'static str>) {
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        // One of this pending's loads succeeded: the transport has an active provider again.
        if provider.is_some() && (seq == p.load.seq || p.earlier.iter().any(|l| l.seq == seq)) {
            self.unloaded = false;
        }
        if seq != p.load.seq {
            // A superseded load commits nothing while a newer one is pending. If it succeeded,
            // its track is what plays now: keep it in case the newer load fails.
            while let Some(l) = p.earlier.pop_front() {
                if l.seq < seq {
                    continue;
                }
                if l.seq > seq {
                    p.earlier.push_front(l);
                } else if let Some(provider) = provider {
                    // Events about it arrive only from now on (they match its uri only once
                    // it is the fallback), so nothing recorded for it is overwritten here.
                    p.restore = l.start.play_state();
                    p.restore_pos = (l.start.position_ms, Instant::now());
                    p.fallback = Some(Fallback { load: l, provider });
                }
                break;
            }
            return;
        }
        let Some(p) = self.pending.take() else {
            return;
        };
        if let Some(provider) = provider {
            if let Some(h) = self.held.as_mut().filter(|h| h.seq == seq) {
                // Committed once the old track's queued audio has been heard; meanwhile the
                // state shows what was recorded for the old track while the load ran.
                h.loaded = Some(Fallback {
                    load: p.load,
                    provider,
                });
                self.update(|s| {
                    s.play_state = p.restore;
                    (s.position_ms, s.position_at) = p.restore_pos;
                });
                return;
            }
            let start = p.load.start;
            self.commit(
                p.load,
                provider,
                start.play_state(),
                (start.position_ms, Instant::now()),
                false,
            );
            return;
        }
        self.drop_hold();
        match p.fallback {
            Some(f) => self.commit(f.load, f.provider, p.restore, p.restore_pos, true),
            None => self.update(|s| {
                s.play_state = p.restore;
                (s.position_ms, s.position_at) = p.restore_pos;
            }),
        }
        self.error(texts::LOAD_FAILED);
    }

    /// Make a loaded track current. A fallback (a superseded load committed after the newest
    /// failed) replaces the queue only if nothing edited it since that load started; otherwise
    /// it marks its track current by uri in the queue as it is, like `Jump`.
    fn commit(
        &mut self,
        load: Load,
        provider: &'static str,
        play_state: PlayState,
        (position_ms, at): (u32, Instant),
        fallback: bool,
    ) {
        match load.commit {
            Commit::Replace { items, index } if !fallback || load.edits == self.queue_edits => {
                self.queue.replace(items, index)
            }
            Commit::Replace { index, .. } | Commit::Jump(index) => {
                self.queue.set_current(index, &load.track.uri)
            }
            Commit::Keep => {}
        }
        // The loaded track becomes the now-playing item in the update below.
        let st = self.state();
        let q = self.queue.snapshot(st.repeat, true);
        // Lyrics and the liked state are asked only when the uri changes (not on a replay).
        let new_uri = st
            .current
            .as_ref()
            .is_none_or(|c| c.track.uri != load.track.uri);
        let track = new_uri.then(|| load.track.clone());
        let source_format = self
            .registry
            .get(provider)
            .and_then(|p| p.playback().source_format());
        self.update(|s| {
            s.current = Some(NowPlaying {
                track: load.track,
                provider,
                source_format,
            });
            s.play_state = play_state;
            s.position_ms = position_ms;
            s.position_at = at;
            s.queue = q;
        });
        if let Some(track) = track {
            self.new_current(&track, provider);
        }
    }

    fn on_done(&mut self, op: Op, ok: bool) {
        if !ok {
            self.error(texts::PLAYBACK_FAILED);
            return;
        }
        // An op sent before a remote took over: about the provider it stopped.
        if self.remote.is_some() {
            return;
        }
        match op {
            Op::Play => self.set_play_state(PlayState::Playing, None),
            Op::Pause => self.set_play_state(PlayState::Paused, None),
            Op::Stop => {
                self.set_play_state(PlayState::Stopped, Some(0));
                self.release_when_stopped();
            }
            Op::Seek(ms) => self.set_position(ms),
        }
    }

    fn jump(&mut self, index: usize) {
        if let Some(t) = self.queue.get(index).cloned() {
            self.start_load(t, Commit::Jump(index));
        }
    }

    /// Replay the current item; false when nothing is current.
    fn replay(&mut self) -> bool {
        if let Some(c) = self.queue.current() {
            self.jump(c);
            return true;
        }
        match self.state().current.as_ref() {
            Some(np) => {
                self.start_load(np.track.clone(), Commit::Keep);
                true
            }
            None => false,
        }
    }

    /// `Next` and end of track: repeat One replays, All wraps, Off stops at the end.
    fn advance(&mut self) {
        let repeat = self.state().repeat;
        if repeat == RepeatMode::One && self.replay() {
            return;
        }
        match self.queue.next(repeat == RepeatMode::All) {
            Some(i) => self.jump(i),
            None => self.op(Op::Stop),
        }
    }

    /// Back to the start when past 3 s (or at the start of the queue), else the item before.
    /// `held`: a gapless switch was held (and has just been cancelled); the state, its position
    /// and the queue's current item are still the heard track's.
    fn previous(&mut self, held: bool) {
        let st = self.state();
        let back = if st.position_now() > 3000 {
            None
        } else {
            self.queue.previous(st.repeat == RepeatMode::All)
        };
        match back {
            Some(i) => self.jump(i),
            // The provider plays the next track already: a seek would restart that one.
            None if held && self.reload_heard(0) => {}
            None => self.op(Op::Seek(0)),
        }
    }

    /// During a held gapless switch (H25) the provider already plays the next track while the
    /// state shows the heard one: a seek or a restart of the heard track loads it again, at
    /// `position_ms`, playing or paused as it is now. False (nothing started) when nothing is
    /// current or no provider owns it.
    fn reload_heard(&mut self, position_ms: u32) -> bool {
        let st = self.state();
        let Some(np) = st.current.as_ref() else {
            return false;
        };
        // While the next track's load still runs, a pause lands in `restore`, not the state.
        let ps = self.pending.as_ref().map_or(st.play_state, |p| p.restore);
        let start = Start {
            position_ms,
            play: ps != PlayState::Paused,
        };
        let commit = match self.queue.current() {
            Some(i) if self.queue.get(i).is_some_and(|t| t.uri == np.track.uri) => Commit::Jump(i),
            _ => Commit::Keep,
        };
        self.start_load_at(np.track.clone(), commit, start)
    }

    // ------------------------------------------------------------ provider events

    fn on_event(&mut self, e: UiEvent) {
        // In remote mode the remote's own events say what plays; the plain state events are
        // the stopped local playback's (or a remote's echo of them) and change nothing.
        if self.remote.is_some()
            && matches!(
                e,
                UiEvent::Loading { .. }
                    | UiEvent::Playing { .. }
                    | UiEvent::Paused { .. }
                    | UiEvent::Stopped { .. }
                    | UiEvent::Seeked { .. }
                    | UiEvent::Position { .. }
                    | UiEvent::TrackChanged { .. }
                    | UiEvent::EndOfTrack { .. }
                    | UiEvent::PreloadNext { .. }
                    | UiEvent::Unavailable { .. }
            )
        {
            return;
        }
        let (uri, change) = match e {
            UiEvent::SessionLost => {
                self.set_play_state(PlayState::Stopped, None);
                self.error(texts::SESSION_LOST);
                return;
            }
            UiEvent::Error { .. } => {
                // Never forward the provider's text.
                self.error(texts::PROVIDER_ERROR);
                return;
            }
            UiEvent::Loading { uri, position_ms } => {
                (uri, Ev::State(PlayState::Loading, Some(position_ms)))
            }
            UiEvent::Playing { uri, position_ms } => {
                (uri, Ev::State(PlayState::Playing, Some(position_ms)))
            }
            UiEvent::Paused { uri, position_ms } => {
                (uri, Ev::State(PlayState::Paused, Some(position_ms)))
            }
            UiEvent::Stopped { uri } => (uri, Ev::State(PlayState::Stopped, None)),
            UiEvent::Seeked { uri, position_ms } | UiEvent::Position { uri, position_ms } => {
                (uri, Ev::Position(position_ms))
            }
            UiEvent::PreloadNext { uri } => {
                self.preload(&uri);
                return;
            }
            UiEvent::TrackChanged { .. } => return,
            UiEvent::AuthChanged { provider } => {
                if let Some(p) = self.registry.get(&provider) {
                    self.refresh_auth_of(p.clone());
                }
                return;
            }
            UiEvent::LibraryChanged { provider } => {
                self.shared
                    .notify(PlayerNotice::LibraryChanged { provider });
                return;
            }
            UiEvent::EndOfTrack { uri } => (uri, Ev::End(false)),
            UiEvent::Unavailable { uri } => (uri, Ev::End(true)),
            UiEvent::RemoteStarted { provider } => {
                self.remote_started(&provider);
                return;
            }
            UiEvent::RemoteTrack { provider, track } => {
                if let Some(id) = self.remote_of(&provider) {
                    self.remote_track(id, track);
                }
                return;
            }
            UiEvent::RemoteState {
                provider,
                uri,
                state,
                position_ms,
            } => {
                if self.remote_of(&provider).is_some() {
                    self.remote_state(&uri, state, position_ms);
                }
                return;
            }
            UiEvent::RemoteVolume { provider, volume } => {
                if self.remote_of(&provider).is_some() {
                    // Applied, never forwarded back (that would echo, D5).
                    let v = volume.min(100);
                    self.engine.set_volume(v);
                    self.update(|s| s.volume = v);
                }
                return;
            }
            UiEvent::RemoteEnded { provider } => {
                if self.remote_of(&provider).is_some() {
                    self.end_remote(false);
                    self.release_when_stopped();
                }
                return;
            }
        };
        // The held track ends before its switch (shorter than what was queued): switch now.
        if matches!(change, Ev::End(_))
            && self
                .held
                .as_ref()
                .and_then(|h| h.loaded.as_ref())
                .is_some_and(|f| f.load.track.uri == uri)
        {
            self.apply_hold();
        }
        // Only events about what really plays count (the current item, or a superseded load
        // that succeeded while a newer one is pending); anything else is stale.
        let is_current = match self.pending.as_ref().and_then(|p| p.fallback.as_ref()) {
            Some(f) => f.load.track.uri == uri,
            None => self
                .state()
                .current
                .as_ref()
                .is_some_and(|c| c.track.uri == uri),
        };
        if !is_current {
            return;
        }
        match change {
            Ev::State(ps, pos) => {
                let pos = pos.map(|ms| self.heard(ms));
                self.set_play_state(ps, pos);
            }
            Ev::Position(ms) => {
                let ms = self.heard(ms);
                self.set_position(ms);
            }
            Ev::End(unavailable) => {
                if unavailable {
                    self.error(texts::TRACK_UNAVAILABLE);
                }
                // A load already on its way replaces the ending track; if it fails, nothing
                // plays any more.
                if self.pending.is_some() {
                    self.set_play_state(PlayState::Stopped, Some(0));
                    return;
                }
                if !unavailable {
                    self.begin_hold();
                }
                self.advance();
                // No load started (the end of the queue stops): nothing to hold.
                if self.held.as_ref().is_some_and(|h| h.seq != self.seq) {
                    self.drop_hold();
                }
            }
        }
    }

    /// The provider is about to need the next track (MD35): when the current track is `uri`, no
    /// load is pending and the active provider owns the track that follows, tell it to preload
    /// that one. The provider's answer is ignored.
    fn preload(&mut self, uri: &str) {
        if self.pending.is_some() {
            return;
        }
        let st = self.state();
        let Some(now) = st.current.as_ref().filter(|c| c.track.uri == uri) else {
            return;
        };
        let next = match st.repeat {
            RepeatMode::One => Some(now.track.uri.clone()),
            repeat => self
                .queue
                .next(repeat == RepeatMode::All)
                .and_then(|i| self.queue.get(i))
                .map(|t| t.uri.clone()),
        };
        let Some(next) = next else { return };
        let owned = self
            .registry
            .resolve_uri(&next)
            .is_some_and(|p| p.info().id == now.provider);
        if owned {
            let _ = self.transport.send(TransportCmd::Preload { uri: next });
        }
    }

    // ------------------------------------------------------------ remote mode

    /// `RemoteStarted`: a provider that [`can_take_over`] enters remote mode. Nothing local
    /// commits or switches any more (a pending load's report finds nothing pending; the
    /// transport stops whatever it loaded at the takeover, which runs after it). The queue is
    /// kept with nothing current; the remote gets Whipspot's volume once (D5). An unknown
    /// provider, one with no remote, or the one already in remote mode changes nothing.
    fn remote_started(&mut self, provider: &str) {
        let Some(p) = self.registry.get(provider).cloned() else {
            return;
        };
        if !can_take_over(&self.registry, p.as_ref()) {
            return;
        }
        let id = p.info().id;
        if self.remote == Some(id) {
            return;
        }
        if self.remote.is_some() {
            let _ = self.transport.send(TransportCmd::Release);
        }
        self.pending = None;
        self.drop_hold();
        self.carry = None;
        self.pause_deadline = None;
        self.remote = Some(id);
        self.unloaded = false;
        self.queue.detach();
        let st = self.state();
        let q = self.queue.snapshot(st.repeat, st.current.is_some());
        self.update(|s| {
            s.remote = Some(id);
            s.play_state = PlayState::Loading;
            s.queue = q;
        });
        let _ = self.transport.send(TransportCmd::Takeover { provider: id });
        self.remote_cmd(RemoteCommand::Volume(st.volume));
    }

    /// `provider`'s id when it is the one in remote mode.
    fn remote_of(&self, provider: &str) -> Option<&'static str> {
        self.remote.filter(|id| *id == provider)
    }

    fn remote_cmd(&self, cmd: RemoteCommand) {
        let _ = self.transport.send(TransportCmd::Remote(cmd));
    }

    /// The remote's track becomes now-playing (from 0 when it is a new uri, which also looks
    /// its lyrics and liked state up).
    fn remote_track(&mut self, id: &'static str, track: Track) {
        let source_format = self
            .registry
            .get(id)
            .and_then(|p| p.playback().source_format());
        let new_uri = self
            .state()
            .current
            .as_ref()
            .is_none_or(|c| c.track.uri != track.uri);
        let lookup = new_uri.then(|| track.clone());
        self.update(|s| {
            s.current = Some(NowPlaying {
                track,
                provider: id,
                source_format,
            });
            if new_uri {
                s.position_ms = 0;
                s.position_at = Instant::now();
            }
        });
        self.publish_queue();
        if let Some(track) = lookup {
            self.new_current(&track, id);
        }
    }

    /// The remote's play state, when it is about the track shown.
    fn remote_state(&mut self, uri: &str, state: RemotePlay, position_ms: u32) {
        let shown = self
            .state()
            .current
            .as_ref()
            .is_some_and(|c| c.track.uri == uri);
        if !shown {
            return;
        }
        let ps = match state {
            RemotePlay::Loading => PlayState::Loading,
            RemotePlay::Playing => PlayState::Playing,
            RemotePlay::Paused => PlayState::Paused,
            RemotePlay::Stopped => PlayState::Stopped,
        };
        let ms = self.heard(position_ms);
        self.set_play_state(ps, Some(ms));
    }

    /// In remote mode the transport buttons go to the remote (`SetVolume` is also applied here);
    /// Stop leaves remote mode, and so does a local play, before its load is sent. Returns the
    /// action when the arms of `on_action` still handle it.
    fn remote_action(&mut self, a: PlayerAction) -> Option<PlayerAction> {
        if self.remote.is_none() {
            return Some(a);
        }
        let cmd = match &a {
            PlayerAction::Play => RemoteCommand::Play,
            PlayerAction::Pause => RemoteCommand::Pause,
            PlayerAction::Toggle => match self.state().play_state {
                PlayState::Playing | PlayState::Loading => RemoteCommand::Pause,
                PlayState::Paused | PlayState::Stopped => RemoteCommand::Play,
            },
            PlayerAction::Next => RemoteCommand::Next,
            PlayerAction::Previous => RemoteCommand::Previous,
            PlayerAction::Seek { position_ms } => RemoteCommand::Seek(*position_ms),
            PlayerAction::SetVolume(v) => {
                if *v <= 100 {
                    self.remote_cmd(RemoteCommand::Volume(*v));
                }
                return Some(a);
            }
            PlayerAction::Stop => {
                self.end_remote(true);
                self.release_when_stopped();
                return None;
            }
            PlayerAction::PlayItem { context, index } => {
                // Only a play that starts a load ends remote control.
                let loads = context
                    .get(*index)
                    .is_some_and(|t| self.registry.resolve_uri(&t.uri).is_some());
                if loads {
                    self.end_remote(true);
                }
                return Some(a);
            }
            _ => return Some(a),
        };
        self.remote_cmd(cmd);
        None
    }

    /// Leave remote mode; `release` gives remote control up first (on the transport, so before
    /// any load sent after this). The engine owner stays the remote's until the next load sets
    /// its own (D13). The remote's track stays shown, stopped, and loaded nowhere (`unloaded`):
    /// Play loads it.
    fn end_remote(&mut self, release: bool) {
        if self.remote.take().is_none() {
            return;
        }
        self.unloaded = true;
        if release {
            let _ = self.transport.send(TransportCmd::Release);
        }
        self.update(|s| {
            s.remote = None;
            s.play_state = PlayState::Stopped;
            s.position_ms = 0;
            s.position_at = Instant::now();
        });
    }

    // ------------------------------------------------------------ render-side timing (H25)

    /// A provider's position (what it decoded) as the position heard now (H25, D12): less what
    /// the engine holds queued while an output is open (unchanged otherwise), and, while the
    /// track plays or is paused, never behind the position shown now nor past the reported one.
    fn heard(&self, reported: u32) -> u32 {
        let Some(queued) = self.engine.queued() else {
            return reported;
        };
        let queued = u32::try_from(queued.as_millis()).unwrap_or(u32::MAX);
        let shown = match self.pending.as_ref() {
            Some(p) => matches!(p.restore, PlayState::Playing | PlayState::Paused)
                .then(|| p.position_now()),
            None => {
                let st = self.state();
                matches!(st.play_state, PlayState::Playing | PlayState::Paused)
                    .then(|| st.position_now())
            }
        };
        heard_position(reported, queued, shown)
    }

    /// The end of the current track arrived while it plays: hold the switch to the next track
    /// until what is queued of this one has been heard (none to hold with no output open or
    /// nothing queued). A watcher on a blocking thread reads the renderer's frame counter.
    fn begin_hold(&mut self) {
        self.drop_hold();
        if self.state().play_state != PlayState::Playing {
            return;
        }
        let Some(queued) = self.engine.queued() else {
            return;
        };
        if queued.is_zero() {
            return;
        }
        let target = self.engine.written_frames();
        let probe = self.engine.render_probe();
        let latency = self.engine.output_latency().min(MAX_HOLD_LATENCY);
        self.holds += 1;
        let id = self.holds;
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                if stop.load(Ordering::Acquire) || tx.is_closed() {
                    return;
                }
                match probe() {
                    Some(pos) if pos < target => std::thread::sleep(HOLD_POLL),
                    // Heard, or the output closed (nothing more is heard of it): due.
                    _ => break,
                }
            }
            std::thread::sleep(latency);
            let _ = tx.send(Internal::HoldDue(id));
        });
        self.held = Some(Held {
            id,
            seq: self.seq + 1,
            loaded: None,
            cancel,
        });
    }

    /// Forget the held switch (its load failed, or none started).
    fn drop_hold(&mut self) {
        if let Some(h) = self.held.take() {
            h.cancel.store(true, Ordering::Release);
        }
    }

    /// The held switch is due: the loaded track becomes now-playing at position 0. A load still
    /// running shows as any load does and commits when it succeeds.
    fn apply_hold(&mut self) {
        let Some(h) = self.held.take() else {
            return;
        };
        h.cancel.store(true, Ordering::Release);
        match h.loaded {
            Some(f) => self.switch_now(f),
            None if self.pending.is_some() => self.update(|s| s.play_state = PlayState::Loading),
            None => {}
        }
    }

    /// A manual action (next, previous, play an item, stop, seek) cancels the held switch. Its
    /// loaded track, if any, is what plays: it becomes the fallback of the load the action
    /// starts (`carry`), or now-playing at once when the action starts none (`settle_carry`).
    fn cancel_hold(&mut self) {
        if let Some(h) = self.held.take() {
            h.cancel.store(true, Ordering::Release);
            self.carry = h.loaded;
        }
    }

    fn settle_carry(&mut self) {
        if let Some(f) = self.carry.take() {
            self.switch_now(f);
        }
    }

    fn switch_now(&mut self, f: Fallback) {
        let ps = match self.state().play_state {
            PlayState::Paused => PlayState::Paused,
            _ => PlayState::Playing,
        };
        self.commit(f.load, f.provider, ps, (0, Instant::now()), false);
    }

    // ------------------------------------------------------------ auth and library tasks

    fn on_internal(&mut self, m: Internal) {
        match m {
            Internal::Transport(TransportReport::Loaded { seq, provider }) => {
                self.on_loaded(seq, provider)
            }
            Internal::Transport(TransportReport::Done { op, ok }) => self.on_done(op, ok),
            Internal::Transport(TransportReport::Remote { ok }) => {
                if !ok {
                    self.error(texts::PLAYBACK_FAILED);
                }
            }
            Internal::Status { provider, result } => {
                match result {
                    Ok(st) => self.set_auth(provider, st),
                    Err(_) => self.error(texts::STATUS_FAILED),
                }
                self.refresh_writable(provider);
            }
            Internal::Login {
                provider,
                step,
                result,
            } => {
                let ok = result.is_ok();
                if let Some(id) = self.registry.get(&provider).map(|p| p.info().id) {
                    if let Ok(st) = result {
                        self.set_auth(id, st);
                    }
                    self.refresh_writable(id);
                }
                self.shared
                    .notify(PlayerNotice::LoginResult { provider, step, ok });
            }
            Internal::Logout { provider, result } => {
                match result {
                    Ok(st) => self.set_auth(provider, st),
                    Err(_) => self.error(texts::LOGOUT_FAILED),
                }
                self.refresh_writable(provider);
            }
            // Serviced by `check_output`, which runs after every message.
            Internal::OutputRequest => {}
            Internal::HoldDue(id) => {
                if self.held.as_ref().is_some_and(|h| h.id == id) {
                    self.apply_hold();
                }
            }
            Internal::Lyrics { lookup, result } => {
                let st = self.state();
                let current = st.current.as_ref().map(|c| c.track.uri.as_str());
                let step = self.lyrics.answered(lookup, result, current);
                self.lyrics_step(step);
            }
            Internal::LikeDone {
                provider,
                uri,
                liked,
                result,
                writable,
            } => self.like_done(provider, uri, liked, result, writable),
            Internal::LikedRead {
                read,
                provider,
                uris,
                saved,
                writable,
            } => {
                let pairs = self.likes.end_read(read, uris, saved);
                if !pairs.is_empty() {
                    self.shared.notify(PlayerNotice::Liked(pairs));
                }
                self.set_writable(provider, writable);
            }
        }
    }

    fn set_auth(&self, provider: &'static str, st: ProviderAuthStatus) {
        self.update(|s| {
            if let Some(e) = s.providers.iter_mut().find(|e| e.info.id == provider) {
                e.auth = Some(st);
            }
        });
    }

    fn login(&self, provider: String, step: String, input: crate::provider::LoginInput) {
        let Some(p) = self.registry.get(&provider).cloned() else {
            self.shared.notify(PlayerNotice::LoginResult {
                provider,
                step,
                ok: false,
            });
            return;
        };
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = p.auth().login(&step, &input).await;
            drop(input);
            let _ = tx.send(Internal::Login {
                provider,
                step,
                result,
            });
        });
    }

    fn logout(&self, provider: String, step: String) {
        let Some(p) = self.registry.get(&provider).cloned() else {
            self.error(texts::UNKNOWN_PROVIDER);
            return;
        };
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = p.auth().logout(&step).await;
            let _ = tx.send(Internal::Logout {
                provider: p.info().id,
                result,
            });
        });
    }

    fn prefill(&self, request: RequestId, provider: String, step: String, input: String) {
        let Some(p) = self.registry.get(&provider).cloned() else {
            self.shared.notify(PlayerNotice::Prefill {
                request,
                value: None,
            });
            return;
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let value = p.auth().prefill(&step, &input).await.ok().flatten();
            shared.notify(PlayerNotice::Prefill { request, value });
        });
    }

    fn refresh_auth(&self) {
        for p in self.registry.iter() {
            self.refresh_auth_of(p.clone());
        }
    }

    /// Read one provider's login status on its own task.
    fn refresh_auth_of(&self, p: Arc<dyn Provider>) {
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let result = p.auth().status().await;
            let _ = tx.send(Internal::Status {
                provider: p.info().id,
                result,
            });
        });
    }

    /// Ask every provider with a library to look for changes. Each check is its own task: the
    /// loop awaits nothing, and a check that never ends delays nothing else.
    fn check_library(&self) {
        for p in self.registry.iter() {
            if p.library().is_none() {
                continue;
            }
            let p = p.clone();
            tokio::spawn(async move {
                if let Some(lib) = p.library() {
                    lib.check_changes().await;
                }
            });
        }
    }

    fn library(&self, request: RequestId, provider: String, query: LibraryQuery) {
        let Some(p) = self.registry.get(&provider).cloned() else {
            self.shared.notify(PlayerNotice::LibraryPage {
                request,
                result: Err(LibraryError::new(
                    LibErrorKind::InvalidArgument,
                    texts::UNKNOWN_PROVIDER,
                )),
            });
            return;
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let result = match p.library() {
                None => Err(LibraryError::new(
                    LibErrorKind::InvalidArgument,
                    texts::NO_LIBRARY,
                )),
                Some(lib) => match query {
                    LibraryQuery::Playlists { offset } => {
                        lib.playlists(offset).await.map(LibraryPage::Playlists)
                    }
                    LibraryQuery::Liked { offset } => {
                        lib.liked_tracks(offset).await.map(LibraryPage::Tracks)
                    }
                    LibraryQuery::PlaylistItems { id, offset } => lib
                        .playlist_items(&id, offset)
                        .await
                        .map(LibraryPage::Tracks),
                    LibraryQuery::Albums { offset } => {
                        lib.albums(offset).await.map(LibraryPage::Albums)
                    }
                    LibraryQuery::AlbumTracks { id, offset } => {
                        lib.album_tracks(&id, offset).await.map(LibraryPage::Tracks)
                    }
                    LibraryQuery::Search { query, offset } => {
                        lib.search(&query, offset).await.map(LibraryPage::Tracks)
                    }
                    LibraryQuery::Artist { uri } => lib.artist(&uri).await.map(LibraryPage::Artist),
                    LibraryQuery::Album { uri } => {
                        lib.album(&uri).await.map(LibraryPage::AlbumDetail)
                    }
                    LibraryQuery::ArtistReleases { uri, group, offset } => lib
                        .artist_releases(&uri, group, offset)
                        .await
                        .map(LibraryPage::Albums),
                },
            };
            shared.notify(PlayerNotice::LibraryPage { request, result });
        });
    }
}

// ------------------------------------------------------------ lyrics and likes

impl Control {
    /// A new uri became current: look its lyrics up and read whether it is liked.
    fn new_current(&mut self, track: &Track, provider: &'static str) {
        let step = self.lyrics.current(Lookup {
            uri: track.uri.clone(),
            query: LyricsQuery::of(track),
        });
        self.lyrics_step(step);
        let has_likes = self
            .registry
            .get(provider)
            .is_some_and(|p| p.info().caps.likes && p.library().is_some());
        if has_likes {
            self.read_liked(provider, vec![track.uri.clone()]);
        }
    }

    /// `SetLyrics`: off shows `Off` and drops the waiting lookup; on looks the current item up
    /// (cache first).
    fn set_lyrics(&mut self, on: bool) {
        self.lyrics.set_enabled(on);
        let enabled = self.lyrics.enabled();
        if self.state().lyrics_enabled != enabled {
            self.update(|s| s.lyrics_enabled = enabled);
        }
        let current = self.state().current.as_ref().map(|c| Lookup {
            uri: c.track.uri.clone(),
            query: LyricsQuery::of(&c.track),
        });
        let step = match current {
            Some(lookup) if enabled => self.lyrics.current(lookup),
            _ => Step {
                publish: Some(self.lyrics.idle()),
                start: None,
            },
        };
        self.lyrics_step(step);
    }

    /// Publish a lyrics state (only when it changed) and start a lookup.
    fn lyrics_step(&mut self, step: Step) {
        if let Some(l) = step.publish {
            if self.state().lyrics != l {
                self.update(|s| s.lyrics = l);
            }
        }
        let Some(lookup) = step.start else {
            return;
        };
        let Some(source) = self.lyrics.source().cloned() else {
            return;
        };
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let query = lookup.query.clone();
            // A panicking source fails only this lookup (and frees the slot).
            let result = tokio::spawn(async move { source.lookup(&query).await })
                .await
                .unwrap_or(Err(LyricsError::Unavailable));
            let _ = tx.send(Internal::Lyrics { lookup, result });
        });
    }

    /// Ask `provider` whether `uris` are saved. The answer goes to the loop, which posts `Liked`
    /// in request order for the uris no change touched since the read started (nothing on an
    /// error or a short answer), then updates the writability.
    fn read_liked(&mut self, provider: &'static str, uris: Vec<String>) {
        let Some(p) = self.registry.get(provider).cloned() else {
            return;
        };
        let read = self.likes.begin_read(provider, &uris);
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let call = {
                let p = p.clone();
                let uris = uris.clone();
                tokio::spawn(async move {
                    match p.library() {
                        Some(lib) => lib.liked(&uris).await.ok(),
                        None => None,
                    }
                })
            };
            // A panicking provider call answers nothing (and the read is still forgotten).
            let saved = call.await.unwrap_or(None);
            let writable = p.library().is_some_and(|l| l.can_change_likes());
            let _ = tx.send(Internal::LikedRead {
                read,
                provider,
                uris,
                saved,
                writable,
            });
        });
    }

    /// `SetLiked`: refused at once (no call) for an unknown provider, one without likes or a
    /// read-only one; else queued behind the provider's running change.
    fn set_liked(&mut self, uri: String, liked: bool) {
        let target = self.registry.resolve_uri(&uri).cloned().filter(|p| {
            let info = p.info();
            info.caps.likes && p.library().is_some() && self.likes_writable(info.id)
        });
        let Some(p) = target else {
            self.shared.notify(PlayerNotice::LikeFailed { uri, liked });
            return;
        };
        let id = p.info().id;
        match self.likes.push(id, uri.clone(), liked) {
            Ok(Some((uri, liked))) => self.spawn_set_liked(p, id, uri, liked),
            Ok(None) => {}
            Err(()) => self.shared.notify(PlayerNotice::LikeFailed { uri, liked }),
        }
    }

    fn spawn_set_liked(
        &self,
        p: Arc<dyn Provider>,
        provider: &'static str,
        uri: String,
        liked: bool,
    ) {
        let tx = self.internal.clone();
        tokio::spawn(async move {
            let call = {
                let p = p.clone();
                let uri = uri.clone();
                tokio::spawn(async move {
                    match p.library() {
                        Some(lib) => lib.set_liked(&uri, liked).await.map_err(|e| e.kind),
                        None => Err(LibErrorKind::InvalidArgument),
                    }
                })
            };
            // A panicking provider call fails only this change (and frees the queue).
            let result = call.await.unwrap_or(Err(LibErrorKind::Http));
            let writable = p.library().is_some_and(|l| l.can_change_likes());
            let _ = tx.send(Internal::LikeDone {
                provider,
                uri,
                liked,
                result,
                writable,
            });
        });
    }

    fn like_done(
        &mut self,
        provider: &'static str,
        uri: String,
        liked: bool,
        result: Result<(), LibErrorKind>,
        writable: bool,
    ) {
        match result {
            Ok(()) => {
                self.shared.notify(PlayerNotice::Liked(vec![(uri, liked)]));
                self.shared.notify(PlayerNotice::LikesChanged {
                    provider: provider.to_owned(),
                });
            }
            Err(kind) => {
                self.shared.notify(PlayerNotice::LikeFailed { uri, liked });
                // Turned read-only: the heart shows the login hint instead (MD70).
                if writable {
                    self.error(if kind == LibErrorKind::AccessDenied {
                        texts::LIKE_REFUSED
                    } else {
                        texts::LIKE_FAILED
                    });
                }
            }
        }
        self.set_writable(provider, writable);
        if !self.likes_writable(provider) {
            // No request while read-only: every waiting change fails.
            for (uri, liked) in self.likes.drain(provider) {
                self.shared.notify(PlayerNotice::LikeFailed { uri, liked });
            }
        }
        if let Some((uri, liked)) = self.likes.done(provider) {
            if let Some(p) = self.registry.get(provider).cloned() {
                self.spawn_set_liked(p, provider, uri, liked);
            }
        }
    }

    fn likes_writable(&self, provider: &str) -> bool {
        self.state()
            .providers
            .iter()
            .any(|e| e.info.id == provider && e.likes_writable)
    }

    /// `likes_writable` = the likes capability and `can`; published only on a change.
    fn set_writable(&self, provider: &'static str, can: bool) {
        let st = self.state();
        let Some(e) = st.providers.iter().find(|e| e.info.id == provider) else {
            return;
        };
        let w = e.info.caps.likes && can;
        if e.likes_writable != w {
            self.update(|s| {
                if let Some(e) = s.providers.iter_mut().find(|e| e.info.id == provider) {
                    e.likes_writable = w;
                }
            });
        }
    }

    /// Ask the library whether likes can be changed now (after a login, logout or status).
    fn refresh_writable(&self, provider: &'static str) {
        let can = self
            .registry
            .get(provider)
            .and_then(|p| p.library())
            .is_some_and(|l| l.can_change_likes());
        self.set_writable(provider, can);
    }
}

/// [`Control::heard`]'s arithmetic: `reported - queued` (at least 0), then, with a position
/// `shown` now, at least that and at most `reported`.
fn heard_position(reported: u32, queued_ms: u32, shown: Option<u32>) -> u32 {
    let ms = reported.saturating_sub(queued_ms);
    match shown {
        Some(now) => ms.max(now).min(reported),
        None => ms,
    }
}

enum Ev {
    State(PlayState, Option<u32>),
    Position(u32),
    /// End of the track; `true` when the provider found it unavailable.
    End(bool),
}

#[cfg(test)]
mod tests {
    use super::heard_position;

    #[test]
    fn heard_position_is_reported_less_queued_within_shown_and_reported() {
        // Nothing queued: unchanged, whatever is shown.
        assert_eq!(heard_position(5_000, 0, None), 5_000);
        assert_eq!(heard_position(5_000, 0, Some(6_000)), 5_000);
        // Queued audio is not heard yet.
        assert_eq!(heard_position(5_000, 200, None), 4_800);
        assert_eq!(heard_position(100, 200, None), 0);
        // Never behind what is shown while it plays...
        assert_eq!(heard_position(5_000, 200, Some(4_900)), 4_900);
        assert_eq!(heard_position(5_000, 200, Some(4_700)), 4_800);
        // ...and never past the reported position.
        assert_eq!(heard_position(5_000, 200, Some(5_100)), 5_000);
    }
}
