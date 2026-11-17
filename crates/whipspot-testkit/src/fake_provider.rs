//! A provider implementing every trait with canned answers, for tests of anything that takes a
//! [`Provider`]. For player tests it can fail or delay loads per uri and emit [`UiEvent`]s
//! through the [`ProviderContext`] it was given. With [`FakeProvider::with_remote`] it also has a
//! [`Remote`] that logs what it is sent.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use whipspot_core::events::UiEvent;

use crate::Gate;
use whipspot_core::provider::{
    AlbumPage, AuthFailure, AuthFlow, AuthStepInfo, AuthStepStatus, BoxFuture, Capabilities,
    LibErrorKind, Library, LibraryError, LoginInput, Playback, PlaybackError, PlaylistPage,
    Provider, ProviderAuthStatus, ProviderContext, ProviderInfo, Remote, RemoteCommand, TrackPage,
    model::LoginState,
};

pub struct FakeProvider {
    id: &'static str,
    name: &'static str,
    glyph: whipspot_core::provider::ProviderGlyph,
    has_library: bool,
    caps_library: bool,
    /// Every trait call in order (`load <uri> <play> <pos>`, `play`, `pause`, `stop`,
    /// `seek <ms>`, `preload:<uri>`, `login <step>`, `logout <step>`, `liked <n>`,
    /// `set_liked <uri> <bool>`, `check_changes`, and with a remote `remote play`, `remote pause`, `remote next`,
    /// `remote previous`, `remote seek <ms>`, `remote volume <v>`, `release`). A load is logged
    /// when it starts.
    pub calls: Mutex<Vec<String>>,
    fail_uris: Mutex<HashSet<String>>,
    load_delay: Mutex<Option<Duration>>,
    ctx: Option<ProviderContext>,
    source_format: Option<&'static str>,
    likes: Mutex<Likes>,
    /// `liked` reads the state at once and answers after this gate opens.
    liked_gate: Mutex<Option<Gate>>,
    /// The owner id of its remote; `None`: no remote.
    remote_owner: Option<&'static str>,
    /// `check_changes` waits for this gate to open (a check that never ends: leave it shut).
    check_gate: Mutex<Option<Gate>>,
}

/// The in-memory likes of [`FakeProvider::with_likes`].
#[derive(Default)]
struct Likes {
    on: bool,
    saved: HashSet<String>,
    writable: bool,
    /// Make `set_liked` fail with this kind, then set `writable` to the second value.
    fail: Option<(LibErrorKind, bool)>,
}

impl FakeProvider {
    pub fn new(id: &'static str, library: bool) -> Self {
        Self {
            id,
            name: "Fake",
            glyph: whipspot_core::provider::ProviderGlyph::Other,
            has_library: library,
            caps_library: library,
            calls: Mutex::new(vec![]),
            fail_uris: Mutex::new(HashSet::new()),
            load_delay: Mutex::new(None),
            ctx: None,
            source_format: None,
            likes: Mutex::new(Likes::default()),
            liked_gate: Mutex::new(None),
            remote_owner: None,
            check_gate: Mutex::new(None),
        }
    }

    /// Hold every `check_changes` pending until `gate` opens; the call is logged when it starts.
    pub fn gate_check_changes(self, gate: Gate) -> Self {
        *self.check_gate.lock().unwrap() = Some(gate);
        self
    }

    /// Give it a [`Remote`] whose audio writes as `owner` (any string, a provider's id included,
    /// so a test can check that such a remote is refused). Its calls go to the call log.
    pub fn with_remote(mut self, owner: &'static str) -> Self {
        self.remote_owner = Some(owner);
        self
    }

    /// The glyph `info()` reports (default `Other`).
    pub fn with_glyph(mut self, glyph: whipspot_core::provider::ProviderGlyph) -> Self {
        self.glyph = glyph;
        self
    }

    /// The name `info()` reports (default `"Fake"`).
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// Hold every `liked` answer until `gate` opens; the state is read when the call starts.
    pub fn gate_liked(self, gate: Gate) -> Self {
        *self.liked_gate.lock().unwrap() = Some(gate);
        self
    }

    /// Turn on `caps.likes`: the library keeps `liked` (the uris saved now) in memory;
    /// `writable` is what `can_change_likes` answers. Needs a library.
    pub fn with_likes(self, liked: &[&str], writable: bool) -> Self {
        *self.likes.lock().unwrap() = Likes {
            on: true,
            saved: liked.iter().map(|u| (*u).to_owned()).collect(),
            writable,
            fail: None,
        };
        self
    }

    /// Make every `set_liked` fail with `kind` and afterwards leave `can_change_likes` at
    /// `writable_after`. Turns likes on if they are not.
    pub fn fail_set_liked(self, kind: LibErrorKind, writable_after: bool) -> Self {
        {
            let mut l = self.likes.lock().unwrap();
            l.on = true;
            l.fail = Some((kind, writable_after));
        }
        self
    }

    /// The label `Playback::source_format` returns (default: none).
    pub fn with_source_format(mut self, label: &'static str) -> Self {
        self.source_format = Some(label);
        self
    }

    /// Keep the context the app hands a provider, so [`Self::emit`] can send events.
    pub fn with_context(mut self, ctx: ProviderContext) -> Self {
        self.ctx = Some(ctx);
        self
    }

    /// Make `load` of `uri` fail (or succeed again).
    pub fn fail_load(&self, uri: &str, fail: bool) {
        let mut f = self.fail_uris.lock().unwrap();
        if fail {
            f.insert(uri.to_owned());
        } else {
            f.remove(uri);
        }
    }

    /// Make every `load` take this long before it answers (`None`: answer at once).
    pub fn set_load_delay(&self, delay: Option<Duration>) {
        *self.load_delay.lock().unwrap() = delay;
    }

    /// Send a provider event, as a real provider does. Without a context it is dropped.
    pub fn emit(&self, ev: UiEvent) {
        if let Some(ctx) = &self.ctx {
            let _ = ctx.events.send(ev);
        }
    }

    /// A copy of the call log.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Make `caps.library` disagree with `library()`.
    pub fn with_caps_library(mut self, v: bool) -> Self {
        self.caps_library = v;
        self
    }

    fn log(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
}

impl Provider for FakeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id,
            name: self.name,
            glyph: self.glyph,
            caps: Capabilities {
                library: self.caps_library,
                pcm_tap: false,
                eq: false,
                search: self.caps_library,
                likes: self.likes.lock().unwrap().on,
            },
            auth_steps: vec![AuthStepInfo {
                id: "main",
                label: "Log in",
                inputs: vec![],
                hint: None,
                switch: false,
            }],
        }
    }
    fn auth(&self) -> &dyn AuthFlow {
        self
    }
    fn playback(&self) -> &dyn Playback {
        self
    }
    fn library(&self) -> Option<&dyn Library> {
        self.has_library.then_some(self as &dyn Library)
    }
    fn remote(&self) -> Option<&dyn Remote> {
        self.remote_owner.map(|_| self as &dyn Remote)
    }
}

impl Remote for FakeProvider {
    fn owner(&self) -> &str {
        self.remote_owner.unwrap_or("")
    }
    fn command(&self, cmd: RemoteCommand) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.log(match cmd {
                RemoteCommand::Play => "remote play".to_owned(),
                RemoteCommand::Pause => "remote pause".to_owned(),
                RemoteCommand::Next => "remote next".to_owned(),
                RemoteCommand::Previous => "remote previous".to_owned(),
                RemoteCommand::Seek(ms) => format!("remote seek {ms}"),
                RemoteCommand::Volume(v) => format!("remote volume {v}"),
            });
            Ok(())
        })
    }
    fn release(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            self.log("release");
        })
    }
}

fn status() -> ProviderAuthStatus {
    ProviderAuthStatus {
        steps: vec![AuthStepStatus {
            id: "main".into(),
            state: LoginState::LoggedOut,
            action: None,
            detail: None,
        }],
    }
}

impl AuthFlow for FakeProvider {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async { Ok(status()) })
    }
    fn login<'a>(
        &'a self,
        step: &'a str,
        _input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            self.log(format!("login {step}"));
            Ok(status())
        })
    }
    fn logout<'a>(
        &'a self,
        step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        Box::pin(async move {
            self.log(format!("logout {step}"));
            Ok(status())
        })
    }
}

impl Playback for FakeProvider {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.log(format!("load {uri} {play} {position_ms}"));
            let delay = *self.load_delay.lock().unwrap();
            if let Some(d) = delay {
                Sleep::new(d).await;
            }
            if self.fail_uris.lock().unwrap().contains(uri) {
                return Err(PlaybackError("could not load".into()));
            }
            Ok(())
        })
    }
    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async {
            self.log("play");
            Ok(())
        })
    }
    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async {
            self.log("pause");
            Ok(())
        })
    }
    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async {
            self.log("stop");
            Ok(())
        })
    }
    fn seek(&self, position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.log(format!("seek {position_ms}"));
            Ok(())
        })
    }
    fn source_format(&self) -> Option<String> {
        self.source_format.map(str::to_owned)
    }
    fn preload<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            self.log(format!("preload:{uri}"));
            Ok(())
        })
    }
}

/// The fake is always logged out (see `status`), so every library call fails the way a real
/// provider's does while logged out.
fn logged_out() -> LibraryError {
    LibraryError::new(LibErrorKind::WebNotLoggedIn, "not logged in")
}

impl Library for FakeProvider {
    fn playlists(&self, _offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn liked_tracks(&self, _offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn playlist_items<'a>(
        &'a self,
        _id: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn albums(&self, _offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn album_tracks<'a>(
        &'a self,
        _id: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn search<'a>(
        &'a self,
        _q: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Err(logged_out()) })
    }
    fn liked<'a>(&'a self, uris: &'a [String]) -> BoxFuture<'a, Result<Vec<bool>, LibraryError>> {
        Box::pin(async move {
            self.log(format!("liked {}", uris.len()));
            let answer = {
                let l = self.likes.lock().unwrap();
                if !l.on {
                    return Err(likes_unsupported());
                }
                uris.iter().map(|u| l.saved.contains(u)).collect()
            };
            let gate = self.liked_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(answer)
        })
    }
    fn set_liked<'a>(
        &'a self,
        uri: &'a str,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), LibraryError>> {
        Box::pin(async move {
            self.log(format!("set_liked {uri} {liked}"));
            let mut l = self.likes.lock().unwrap();
            if !l.on {
                return Err(likes_unsupported());
            }
            if let Some((kind, writable_after)) = l.fail {
                l.writable = writable_after;
                return Err(LibraryError::new(kind, "like change failed"));
            }
            if !l.writable {
                return Err(LibraryError::new(
                    LibErrorKind::AccessDenied,
                    "likes are read-only",
                ));
            }
            if liked {
                l.saved.insert(uri.to_owned());
            } else {
                l.saved.remove(uri);
            }
            Ok(())
        })
    }
    fn can_change_likes(&self) -> bool {
        let l = self.likes.lock().unwrap();
        l.on && l.writable
    }
    fn check_changes(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.log("check_changes");
            let gate = self.check_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
        })
    }
}

fn likes_unsupported() -> LibraryError {
    LibraryError::new(LibErrorKind::InvalidArgument, "likes are not supported")
}

/// A runtime-free timer: a helper thread wakes the task at the deadline (testkit takes no async
/// runtime dependency).
pub(crate) struct Sleep {
    deadline: Instant,
    waker: Option<Arc<Mutex<Waker>>>,
}

impl Sleep {
    pub(crate) fn new(d: Duration) -> Self {
        Sleep {
            deadline: Instant::now() + d,
            waker: None,
        }
    }
}

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            return Poll::Ready(());
        }
        match &self.waker {
            Some(w) => *w.lock().unwrap() = cx.waker().clone(),
            None => {
                let w = Arc::new(Mutex::new(cx.waker().clone()));
                let deadline = self.deadline;
                let thread_waker = w.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    thread_waker.lock().unwrap().wake_by_ref();
                });
                self.waker = Some(w);
            }
        }
        Poll::Pending
    }
}
