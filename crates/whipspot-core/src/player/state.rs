//! What the player publishes: the state snapshot, the notices and the action type front-ends send.

use std::sync::Arc;
use std::time::Instant;

use crate::engine::{BAND_COUNT, OutputDevice, OutputMode, ReplayGainMode};
use crate::lyrics::Lyrics;
use crate::provider::{
    AlbumPage, LibraryError, LoginInput, PlaylistPage, ProviderAuthStatus, ProviderInfo, Track,
    TrackPage,
};

/// Identifies one library or prefill request; the matching notice carries it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// The track the active provider has loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub track: Track,
    /// Id of the provider playing it.
    pub provider: &'static str,
    /// Codec / format of the source as the provider reports it; `None` when unknown (M1).
    pub source_format: Option<String>,
}

/// Equaliser state in dB (-12..=12); bands at [`crate::engine::BAND_HZ`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqState {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; BAND_COUNT],
}

/// The play queue in list order. `current` indexes `items`; `None` when nothing in the list is
/// current (empty queue, or the current item was removed while it plays on).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueueState {
    pub items: Arc<Vec<Track>>,
    pub current: Option<usize>,
    /// The list index that plays when the current item ends; `None` when the queue would stop.
    pub next: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEntry {
    pub info: ProviderInfo,
    /// `None` until the first status answer (see [`PlayerAction::RefreshAuth`]).
    pub auth: Option<ProviderAuthStatus>,
    /// Likes can be changed now: the likes capability and the library says it can (the login
    /// allows it). Refreshed after every likes call and login result.
    pub likes_writable: bool,
}

/// The lyrics of the current item.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LyricsState {
    /// No lyrics source, or lyrics are switched off.
    #[default]
    Off,
    /// Nothing is playing.
    Idle,
    /// Looking `uri` up.
    Searching { uri: String },
    /// Found for `uri` (sanitised; may be `Lyrics::None` or `Instrumental`).
    Found { uri: String, lyrics: Arc<Lyrics> },
    /// The lookup for `uri` failed.
    Failed { uri: String },
}

/// One immutable snapshot of the player. `revision` grows strictly with every change.
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub revision: u64,
    pub play_state: PlayState,
    pub current: Option<NowPlaying>,
    /// Position at `position_at`; while `Playing` it advances with the clock from there.
    pub position_ms: u32,
    pub position_at: Instant,
    /// 0..=100.
    pub volume: u8,
    /// -100 (left) ..= 100 (right).
    pub balance: i8,
    pub eq: EqState,
    /// Which ReplayGain value is applied (D8; default off).
    pub replay_gain: ReplayGainMode,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub queue: QueueState,
    pub providers: Vec<ProviderEntry>,
    pub output_summary: String,
    pub output: OutputStatus,
    pub lyrics: LyricsState,
    /// Lyrics are switched on (and a source exists).
    pub lyrics_enabled: bool,
    /// The id of the provider whose remote controls playback now (`UiEvent::RemoteStarted`):
    /// the transport buttons go to that remote, `current` is its track and the queue is kept
    /// with nothing in it current. `None` when Whipspot plays its own queue.
    pub remote: Option<&'static str>,
}

/// The output the user chose and what the player knows about it.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputStatus {
    /// The chosen device ([`OutputDevice::id`]); `None` for the system default.
    pub device: Option<String>,
    /// The chosen mode.
    pub mode: OutputMode,
    /// The modes the output adapter can open.
    pub modes: Vec<OutputMode>,
    /// The devices last listed.
    pub devices: Vec<OutputDevice>,
    /// Why what plays is not what was chosen; `None` when it is.
    pub fallback: Option<&'static str>,
    /// The output is released while idle.
    pub idle: bool,
}

impl PlayerState {
    /// The position now: `position_ms` plus the time since `position_at` while playing.
    pub fn position_now(&self) -> u32 {
        if self.play_state != PlayState::Playing {
            return self.position_ms;
        }
        let elapsed = u32::try_from(self.position_at.elapsed().as_millis()).unwrap_or(u32::MAX);
        self.position_ms.saturating_add(elapsed)
    }
}

/// What a library request asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryQuery {
    Playlists { offset: u32 },
    Liked { offset: u32 },
    PlaylistItems { id: String, offset: u32 },
    Albums { offset: u32 },
    AlbumTracks { id: String, offset: u32 },
    Search { query: String, offset: u32 },
}

/// One page of a library answer.
#[derive(Debug, Clone, PartialEq)]
pub enum LibraryPage {
    Playlists(PlaylistPage),
    Tracks(TrackPage),
    Albums(AlbumPage),
}

/// Everything a front-end can ask of the player. Not `Debug` on purpose: `Login` carries
/// [`LoginInput`], which may hold secrets and is never logged.
#[derive(Clone)]
pub enum PlayerAction {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    /// Play `context[index]`; on success the queue becomes `context`.
    PlayItem {
        context: Vec<Track>,
        index: usize,
    },
    /// Append tracks to the queue. Tracks of another provider (another uri scheme) than the queued
    /// ones replace the queue instead.
    Enqueue(Vec<Track>),
    RemoveFromQueue {
        index: usize,
    },
    MoveInQueue {
        from: usize,
        to: usize,
    },
    ClearQueue,
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    /// 0..=100.
    SetVolume(u8),
    /// -100..=100.
    SetBalance(i8),
    SetEqEnabled(bool),
    /// dB, -12..=12.
    SetEqPreamp(f32),
    /// `index` < 10, dB -12..=12.
    SetEqBand {
        index: usize,
        gain_db: f32,
    },
    /// Apply each track's ReplayGain: off, track or album (D8).
    SetReplayGain(ReplayGainMode),
    Login {
        provider: String,
        step: String,
        input: LoginInput,
    },
    Logout {
        provider: String,
        step: String,
    },
    Prefill {
        request: RequestId,
        provider: String,
        step: String,
        input: String,
    },
    RefreshAuth,
    Library {
        request: RequestId,
        provider: String,
        query: LibraryQuery,
    },
    /// Choose the output device (`None` = system default) and mode.
    SetOutput {
        device: Option<String>,
        mode: OutputMode,
    },
    /// List the output devices again.
    RefreshOutputDevices,
    /// Switch lyrics lookups on or off.
    SetLyrics(bool),
    /// Ask whether these track URIs are saved (at most 200 per action); the answer comes as
    /// [`PlayerNotice::Liked`].
    RequestLiked(Vec<String>),
    /// Save (`liked`) or remove `uri`; a failure comes as [`PlayerNotice::LikeFailed`].
    SetLiked {
        uri: String,
        liked: bool,
    },
    /// A hint that a window gained focus: every provider with a library looks whether its library
    /// changed ([`crate::provider::Library::check_changes`]). Never blocks the loop and posts no
    /// notice itself.
    CheckLibrary,
}

/// Something that happened once, for the front-end to show or route. Error texts are this
/// module's fixed strings (see [`super::texts`]), never a provider or library error text.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerNotice {
    Error(&'static str),
    LoginResult {
        provider: String,
        step: String,
        ok: bool,
    },
    Prefill {
        request: RequestId,
        value: Option<String>,
    },
    LibraryPage {
        request: RequestId,
        result: Result<LibraryPage, LibraryError>,
    },
    /// The saved state of tracks, as asked by [`PlayerAction::RequestLiked`] or learnt for the
    /// current item.
    Liked(Vec<(String, bool)>),
    /// A [`PlayerAction::SetLiked`] failed; `liked` is the state that was asked for.
    LikeFailed {
        uri: String,
        liked: bool,
    },
    /// A [`PlayerAction::SetLiked`] was written: `provider`'s liked list is not what it was (a
    /// front-end that shows it loads it again).
    LikesChanged {
        provider: String,
    },
    /// The provider said its library changed ([`crate::events::UiEvent::LibraryChanged`]): a
    /// front-end showing `provider`'s lists loads them again.
    LibraryChanged {
        provider: String,
    },
}
