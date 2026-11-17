//! Test-only provider implementing every trait, to prove the traits are implementable.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::engine::OwnerSwitch;
use crate::provider::model::LoginState;

pub struct FakeProvider {
    id: &'static str,
    has_library: bool,
    caps_library: bool,
    caps_likes: bool,
    hosts: &'static [&'static str],
    pub calls: Mutex<Vec<String>>,
    /// Logged at each load as `owner <id or ->` (the engine's source owner then).
    owner: Option<OwnerSwitch>,
    fail_loads: AtomicBool,
}

impl FakeProvider {
    pub fn new(id: &'static str, library: bool) -> Self {
        Self {
            id,
            has_library: library,
            caps_library: library,
            caps_likes: false,
            hosts: &[],
            calls: Mutex::new(vec![]),
            owner: None,
            fail_loads: AtomicBool::new(false),
        }
    }

    /// Log the engine's source owner at every load.
    pub fn with_owner_probe(mut self, owner: OwnerSwitch) -> Self {
        self.owner = Some(owner);
        self
    }

    /// Make every load fail (or succeed again).
    pub fn fail_loads(&self, fail: bool) {
        self.fail_loads.store(fail, Ordering::Relaxed);
    }

    /// Make `caps.library` disagree with `library()`.
    pub fn with_caps_library(mut self, v: bool) -> Self {
        self.caps_library = v;
        self
    }

    /// Declare `caps.likes` (the library keeps the default likes methods).
    pub fn with_caps_likes(mut self, v: bool) -> Self {
        self.caps_likes = v;
        self
    }

    pub fn with_image_hosts(mut self, hosts: &'static [&'static str]) -> Self {
        self.hosts = hosts;
        self
    }

    fn log(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
}

fn empty_tracks() -> TrackPage {
    TrackPage {
        items: vec![],
        skipped: 0,
        next_offset: None,
        total: Some(0),
        truncated: false,
    }
}

impl Provider for FakeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id,
            name: "Fake",
            glyph: super::ProviderGlyph::Other,
            caps: Capabilities {
                library: self.caps_library,
                pcm_tap: false,
                eq: false,
                search: self.caps_library,
                likes: self.caps_likes,
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
    fn image_hosts(&self) -> &'static [&'static str] {
        self.hosts
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
            if let Some(o) = &self.owner {
                self.log(format!("owner {}", o.owner().as_deref().unwrap_or("-")));
            }
            if self.fail_loads.load(Ordering::Relaxed) {
                return Err(PlaybackError("load failed".to_owned()));
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
}

impl Library for FakeProvider {
    fn playlists(&self, _offset: u32) -> BoxFuture<'_, Result<PlaylistPage, LibraryError>> {
        Box::pin(async {
            Ok(PlaylistPage {
                items: vec![],
                skipped: 0,
                next_offset: None,
                total: Some(0),
            })
        })
    }
    fn liked_tracks(&self, _offset: u32) -> BoxFuture<'_, Result<TrackPage, LibraryError>> {
        Box::pin(async { Ok(empty_tracks()) })
    }
    fn playlist_items<'a>(
        &'a self,
        _id: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Ok(empty_tracks()) })
    }
    fn albums(&self, _offset: u32) -> BoxFuture<'_, Result<AlbumPage, LibraryError>> {
        Box::pin(async {
            Ok(AlbumPage {
                items: vec![],
                skipped: 0,
                next_offset: None,
                total: Some(0),
            })
        })
    }
    fn album_tracks<'a>(
        &'a self,
        _id: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Ok(empty_tracks()) })
    }
    fn search<'a>(
        &'a self,
        _q: &'a str,
        _o: u32,
    ) -> BoxFuture<'a, Result<TrackPage, LibraryError>> {
        Box::pin(async { Ok(empty_tracks()) })
    }
}

#[tokio::test]
async fn fake_provider_drives_every_trait() {
    let p = FakeProvider::new("fake", true);
    p.auth()
        .login("main", &LoginInput::default())
        .await
        .unwrap();
    p.playback().load("fake:track:1", true, 0).await.unwrap();
    p.library().unwrap().search("q", 0).await.unwrap();
    assert_eq!(
        *p.calls.lock().unwrap(),
        vec!["login main", "load fake:track:1 true 0"]
    );
}
