//! The player service (C1): the one interface every front-end uses.
//!
//! [`Player::start`] spawns a control loop and a transport task and returns a [`PlayerHandle`].
//! A front-end sends [`PlayerAction`]s with [`PlayerHandle::dispatch`] (never blocks), reads the
//! immutable [`PlayerState`] snapshot, drains [`PlayerNotice`]s, and reads the visualiser and
//! diagnostics. The wake callback tells it when to look again.
//!
//! Concurrency: the control loop is the only writer of the state; every `Playback` call runs on
//! the transport task in order, so volume, EQ and queue edits never wait behind a slow load. A
//! load commits (current item, queue, active provider) only after it succeeded. Login, logout,
//! prefill, status, library, lyrics and likes requests run as their own tasks (lyrics and like
//! changes report back to the loop, which alone publishes); cover art is fetched by a bounded
//! worker fed by the art book (see `art.rs`). The wake callback is called
//! after every state change or new notice, with no player lock held.

mod art;
mod control;
mod likes;
mod lyrics;
mod output;
mod queue;
mod spectrum;
mod state;
mod transport;
mod updates;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

pub use art::{ArtStatus, ArtUrgency};
pub use spectrum::{FFT_LEN, SPECTRUM_BANDS, Spectrum, SpectrumFrame, WAVEFORM_LEN};
pub use state::{
    EqState, LibraryPage, LibraryQuery, LyricsState, NowPlaying, OutputStatus, PlayState,
    PlayerAction, PlayerNotice, PlayerState, ProviderEntry, QueueState, RepeatMode, RequestId,
};

use crate::art::ArtSource;
use crate::diagnostics::AudioDiagnostics;
use crate::engine::{
    AudioOutput, BAND_COUNT, Engine, EngineConfig, EngineMonitor, EqSettings, OutputInfo,
    OutputMode, OutputRequest, OwnerSwitch, PcmTap, ReplayGainMode, format_text,
};
use crate::events::UiEvent;
use crate::lyrics::LyricsSource;
use crate::power::{PowerControl, PowerStatus};
use crate::provider::{ProviderContext, ProviderRegistry};
use crate::update::{self, Updater};

/// The fixed texts of [`PlayerNotice::Error`].
pub mod texts {
    pub const NOT_A_TRACK: &str = "not a supported track";
    pub const LOAD_FAILED: &str = "could not play this track";
    pub const PLAYBACK_FAILED: &str = "the playback command failed";
    pub const TRACK_UNAVAILABLE: &str = "this track is unavailable";
    pub const SESSION_LOST: &str = "the provider session was lost";
    pub const PROVIDER_ERROR: &str = "the provider reported a playback error";
    pub const UNKNOWN_PROVIDER: &str = "unknown provider";
    pub const NO_LIBRARY: &str = "this provider has no library";
    pub const LOGOUT_FAILED: &str = "logout failed";
    pub const STATUS_FAILED: &str = "could not read the login status";
    pub const OUTPUT_OPEN_FAILED: &str = "the audio output could not be opened";
    pub const OUTPUT_LOST: &str = "the audio output stopped";
    pub const OUTPUT_EXCLUSIVE_FALLBACK: &str =
        "exclusive mode is unavailable; playing in shared mode";
    pub const OUTPUT_DEVICE_MISSING: &str =
        "the chosen output device was not found; playing on the default device";
    pub const LIKE_FAILED: &str = "could not change the like";
    pub const LIKE_REFUSED: &str = "the music service refused this change";
}

/// The output summary after a failed open.
const NO_OUTPUT: &str = "No audio output";
/// The output summary after a stream fault.
const OUTPUT_STOPPED: &str = "Audio output stopped";

/// Notices kept for a front-end that does not drain them; older ones are dropped.
const MAX_NOTICES: usize = 256;

/// Player options; the output is chosen by [`EngineSetup`].
#[derive(Clone)]
#[non_exhaustive]
pub struct PlayerConfig {
    art: Option<Arc<dyn ArtSource>>,
    lyrics: Option<Arc<dyn LyricsSource>>,
    lyrics_enabled: bool,
    exclusive_pause_release: Duration,
    power: Option<Arc<dyn PowerControl>>,
    power_enabled: bool,
    hidden_pause_release: Duration,
    updater: Option<Arc<dyn Updater>>,
    update_check: bool,
}

/// How long an exclusive output stays open while paused before the device is given back
/// (H28, D14).
pub const EXCLUSIVE_PAUSE_RELEASE: Duration = Duration::from_secs(10 * 60);

/// How long any output stays open while paused with every window hidden and the efficiency
/// setting on, before it is given back so the OS sees the app as silent (efficiency mode D3).
pub const HIDDEN_PAUSE_RELEASE: Duration = Duration::from_secs(60);

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            art: None,
            lyrics: None,
            lyrics_enabled: true,
            exclusive_pause_release: EXCLUSIVE_PAUSE_RELEASE,
            power: None,
            power_enabled: true,
            hidden_pause_release: HIDDEN_PAUSE_RELEASE,
            updater: None,
            update_check: true,
        }
    }
}

impl PlayerConfig {
    /// Fetch cover art through `source`.
    pub fn with_art(mut self, source: Arc<dyn ArtSource>) -> Self {
        self.art = Some(source);
        self
    }

    /// Look lyrics up through `source`; `enabled` is the initial on/off setting.
    pub fn with_lyrics(mut self, source: Arc<dyn LyricsSource>, enabled: bool) -> Self {
        self.lyrics = Some(source);
        self.lyrics_enabled = enabled;
        self
    }

    /// Give an exclusive device back after this long paused without any action (default
    /// [`EXCLUSIVE_PAUSE_RELEASE`]); resuming reopens it and continues where it paused.
    pub fn with_exclusive_pause_release(mut self, after: Duration) -> Self {
        self.exclusive_pause_release = after;
        self
    }

    /// Ask the OS for a power mode through `control` (efficiency mode); `enabled` is the initial
    /// setting. The player calls [`PowerControl::set`] only when the wanted mode changes.
    pub fn with_power(mut self, control: Arc<dyn PowerControl>, enabled: bool) -> Self {
        self.power = Some(control);
        self.power_enabled = enabled;
        self
    }

    /// Give any output back after this long paused while every window is hidden and the
    /// efficiency setting is on (default [`HIDDEN_PAUSE_RELEASE`]); an exclusive output uses
    /// the shorter of this and its own release.
    pub fn with_hidden_pause_release(mut self, after: Duration) -> Self {
        self.hidden_pause_release = after;
        self
    }

    /// Update the app through `updater`; `check` is the initial scheduled-checks setting (the
    /// first check runs [`update::FIRST_CHECK_AFTER`] after start, then every
    /// [`update::CHECK_EVERY`]). Without one the update status stays `Unavailable` and no check
    /// ever runs.
    pub fn with_updater(mut self, updater: Arc<dyn Updater>, check: bool) -> Self {
        self.updater = Some(updater);
        self.update_check = check;
        self
    }
}

impl std::fmt::Debug for PlayerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerConfig")
            .field("art", &self.art.is_some())
            .field("lyrics", &self.lyrics.is_some())
            .field("lyrics_enabled", &self.lyrics_enabled)
            .field("exclusive_pause_release", &self.exclusive_pause_release)
            .field("power", &self.power.as_ref().map(|p| p.name()))
            .field("power_enabled", &self.power_enabled)
            .field("hidden_pause_release", &self.hidden_pause_release)
            .field("updater", &self.updater.is_some())
            .field("update_check", &self.update_check)
            .finish()
    }
}

/// How the audio engine gets its output.
pub enum EngineSetup {
    /// The engine plays through `output`. The request's `device` and `mode` are the initial
    /// output choice: shared opens at [`Player::start`] (falling back to the default device);
    /// exclusive stays released until audio arrives (MD41).
    Output {
        output: Box<dyn AudioOutput>,
        request: OutputRequest,
    },
}

/// The player's half of the audio and event plumbing; pass it to [`Player::start`].
pub struct PlayerWiring {
    engine: Engine,
    tap: PcmTap,
    events: broadcast::Receiver<UiEvent>,
    /// What to open at start.
    request: OutputRequest,
    /// The engine's source owner (D13), set by the transport around each load.
    owner: OwnerSwitch,
}

/// Build the audio engine and the event channel. The returned [`ProviderContext`] goes to every
/// provider, each through [`ProviderContext::for_owner`] with its own id (D13); the event
/// receiver is subscribed here, before any provider exists, so no event is missed. The output
/// is not opened until [`Player::start`].
pub fn wiring(engine: EngineSetup, event_capacity: usize) -> (ProviderContext, PlayerWiring) {
    let (events, rx) = broadcast::channel(event_capacity.max(1));
    let EngineSetup::Output { output, request } = engine;
    let parts = Engine::new(EngineConfig::default(), Some(output));
    let owner = parts.source.owner_switch();
    (
        ProviderContext {
            source: parts.source,
            events,
        },
        PlayerWiring {
            engine: parts.engine,
            tap: parts.tap,
            events: rx,
            request,
            owner,
        },
    )
}

/// "Shared mode, <device>, 44.1 kHz, 32-bit float".
fn output_summary(info: &OutputInfo) -> String {
    let mode = match info.mode {
        OutputMode::Shared => "Shared",
        OutputMode::Exclusive => "Exclusive",
    };
    let khz = f64::from(info.format.rate) / 1000.0;
    let format = format_text(info.format.sample, info.format.valid_bits);
    format!("{mode} mode, {}, {khz} kHz, {format}", info.device)
}

type Wake = Arc<dyn Fn() + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the handles and the loops share. Only the control loop calls [`Shared::update`].
pub(crate) struct Shared {
    state: Mutex<Arc<PlayerState>>,
    notices: Mutex<VecDeque<PlayerNotice>>,
    wake: Mutex<Option<Wake>>,
    playing: AtomicBool,
    tap: Mutex<spectrum::TapReader>,
    monitor: EngineMonitor,
    /// The art book and the worker's doorbell; `None` without an art source.
    art: Option<art::ArtHub>,
}

impl Shared {
    fn wake(&self) {
        let w = lock(&self.wake).clone();
        if let Some(w) = w {
            w();
        }
    }

    /// Change the state: copy, edit, bump `revision`, publish; then wake with no lock held.
    pub(crate) fn update(&self, f: impl FnOnce(&mut PlayerState)) {
        {
            let mut cur = lock(&self.state);
            let mut next = PlayerState::clone(&cur);
            f(&mut next);
            next.revision = cur.revision + 1;
            self.playing
                .store(next.play_state == PlayState::Playing, Ordering::Relaxed);
            *cur = Arc::new(next);
        }
        self.wake();
    }

    pub(crate) fn state(&self) -> Arc<PlayerState> {
        lock(&self.state).clone()
    }

    pub(crate) fn notify(&self, n: PlayerNotice) {
        {
            let mut q = lock(&self.notices);
            if q.len() >= MAX_NOTICES {
                q.pop_front();
            }
            q.push_back(n);
        }
        self.wake();
    }
}

/// Starts players.
pub struct Player;

impl Player {
    /// Spawn the control loop and the transport task on `rt`.
    pub fn start(
        registry: ProviderRegistry,
        wiring: PlayerWiring,
        config: PlayerConfig,
        rt: &tokio::runtime::Handle,
    ) -> PlayerHandle {
        let PlayerWiring {
            mut engine,
            tap,
            events,
            request,
            owner,
        } = wiring;
        // The engine starts with the state's values (volume 100, balance 0, EQ on and flat).
        let eq = EqState {
            enabled: true,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        };
        engine.set_volume(100);
        engine.set_balance(0);
        engine.set_eq(EqSettings {
            enabled: eq.enabled,
            preamp_db: eq.preamp_db,
            bands_db: eq.bands_db,
        });
        let lyrics = lyrics::LyricsLookups::new(config.lyrics.clone(), config.lyrics_enabled);
        let initial = PlayerState {
            revision: 0,
            play_state: PlayState::Stopped,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume: 100,
            balance: 0,
            eq,
            replay_gain: ReplayGainMode::Off,
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: QueueState::default(),
            providers: registry
                .iter()
                .map(|p| {
                    let info = p.info();
                    ProviderEntry {
                        likes_writable: info.caps.likes
                            && p.library().is_some_and(|l| l.can_change_likes()),
                        info,
                        auth: None,
                    }
                })
                .collect(),
            output_summary: String::new(),
            output: OutputStatus::default(),
            lyrics: lyrics.idle(),
            lyrics_enabled: lyrics.enabled(),
            remote: None,
            power: PowerStatus {
                name: config.power.as_ref().map(|p| p.name()),
                enabled: config.power_enabled,
                ..PowerStatus::default()
            },
            update: match config.updater {
                Some(_) => update::UpdateStatus::Idle,
                None => update::UpdateStatus::Unavailable,
            },
            update_check: config.update_check,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(Arc::new(initial)),
            notices: Mutex::new(VecDeque::new()),
            wake: Mutex::new(None),
            playing: AtomicBool::new(false),
            tap: Mutex::new(spectrum::TapReader::new(tap)),
            monitor: engine.monitor(),
            art: config.art.as_ref().map(|_| art::ArtHub::new()),
        });
        let registry = Arc::new(registry);
        let (tx, rx) = mpsc::unbounded_channel();
        let (ttx, trx) = mpsc::unbounded_channel();
        let (itx, irx) = mpsc::unbounded_channel();
        rt.spawn(transport::run(registry.clone(), owner, trx, itx.clone()));
        if let Some(source) = config.art.clone() {
            rt.spawn(art::worker(shared.clone(), source, itx.clone()));
        }
        let mut control = control::Control::new(
            shared.clone(),
            registry,
            engine,
            ttx,
            itx,
            lyrics,
            control::Policy {
                pause_release: config.exclusive_pause_release,
                hidden_pause_release: config.hidden_pause_release,
                power: config.power.clone(),
                power_enabled: config.power_enabled,
                update: config.updater.clone().map(|updater| updates::UpdateSetup {
                    updater,
                    check: config.update_check,
                }),
            },
        );
        control.init_output(request);
        rt.spawn(control.run(rx, irx, events));
        PlayerHandle {
            inner: Arc::new(HandleInner { tx, shared }),
        }
    }
}

struct HandleInner {
    tx: mpsc::UnboundedSender<PlayerAction>,
    shared: Arc<Shared>,
}

/// A front-end's handle on the player. Cheap to clone; usable from any thread. The player runs
/// until every handle is dropped (or its runtime shuts down).
#[derive(Clone)]
pub struct PlayerHandle {
    inner: Arc<HandleInner>,
}

impl PlayerHandle {
    /// Queue an action. Never blocks; a stopped player ignores it.
    pub fn dispatch(&self, action: PlayerAction) {
        let _ = self.inner.tx.send(action);
    }

    /// The latest snapshot.
    pub fn state(&self) -> Arc<PlayerState> {
        self.inner.shared.state()
    }

    /// Set the one wake callback (replacing any earlier one). It runs on a player thread after
    /// every state change or new notice, with no player lock held, so it may call [`Self::state`]
    /// or [`Self::drain_notices`]; it should only schedule work, not do it.
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.inner.shared.wake) = Some(wake);
    }

    /// Take the notices posted since the last call, oldest first.
    pub fn drain_notices(&self) -> Vec<PlayerNotice> {
        lock(&self.inner.shared.notices).drain(..).collect()
    }

    /// Fill `out` with the current visualiser frame. Never blocks: when another thread is
    /// reading the tap, `out` is left unchanged.
    pub fn spectrum(&self, out: &mut SpectrumFrame) {
        let shared = &self.inner.shared;
        let mut tap = match shared.tap.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        tap.fill(shared.playing.load(Ordering::Relaxed), out);
    }

    /// The audio path diagnostics: the engine's, plus the provider playing and its quality cap
    /// (the source format it reports). Every value the engine does not know is `None`.
    pub fn diagnostics(&self) -> AudioDiagnostics {
        let shared = &self.inner.shared;
        let mut d = shared.monitor.diagnostics();
        let state = shared.state();
        if let Some(now) = &state.current {
            let name = state
                .providers
                .iter()
                .find(|p| p.info.id == now.provider)
                .map_or(now.provider, |p| p.info.name);
            d.source.provider = Some(name.to_owned());
            d.source.quality_cap = now.source_format.clone();
        }
        d
    }
}
