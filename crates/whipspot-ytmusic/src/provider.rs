//! The YouTube Music provider: the `Provider` of this adapter.
//!
//! The logins, the InnerTube client, the library and the playback are here.

use std::sync::Arc;

use tokio::sync::broadcast;
use whipspot_core::credential_store::CredentialStore;
use whipspot_core::engine::SourceWriter;
use whipspot_core::events::UiEvent;
use whipspot_core::provider::model::{Capabilities, ProviderGlyph, ProviderInfo};
use whipspot_core::provider::{AuthFlow, BoxFuture, Library, Playback, PlaybackError, Provider};

use crate::auth::{YtAuth, auth_steps};
use crate::http::Http;
use crate::innertube::InnerTube;
use crate::library::YtLibrary;
use crate::parse::IMAGE_HOSTS;
use crate::playback::{Player, Track};
use crate::resolve::{Resolver, Stream};
use crate::run::Runner;
use crate::tools::{ToolEnv, Tools};
use whipspot_core::secret::Secret;

/// The provider id: also the scheme of its track URIs.
pub const ID: &str = "ytmusic";

/// What the composition root hands the provider.
pub struct YtDeps {
    pub http: Arc<dyn Http>,
    pub runner: Arc<dyn Runner>,
    pub env: ToolEnv,
    pub store: Arc<dyn CredentialStore>,
    /// Where decoded audio goes (the engine's writer, already for this provider's id).
    pub source: SourceWriter,
    /// Where playback reports (the one UI event sender).
    pub events: broadcast::Sender<UiEvent>,
}

pub struct YtMusicProvider {
    auth: YtAuth,
    playback: YtPlayback,
    library: YtLibrary,
    tools: Arc<Tools>,
}

/// What [`YtMusicProvider::startup_update`] did. No variant carries a message of the network or
/// the tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupUpdate {
    /// The stream helper is switched off (D10): nothing was fetched.
    HelperOff,
    /// No managed copy is installed: installing is the user's button, nothing was fetched.
    NoManagedCopy,
    /// An update succeeded in the last day.
    NotDue,
    /// The check ran and the tools are current.
    Checked,
    /// The check ran and failed; the old copies are kept.
    Failed,
}

impl YtMusicProvider {
    pub fn new(deps: YtDeps) -> YtMusicProvider {
        let tools = Arc::new(Tools::new(deps.env, deps.http.clone()));
        // A run that was killed left its cookies file behind: nothing of an old session is
        // running now, so every file there is stale.
        crate::resolve::clear_run_files(tools.tools_dir());
        let resolver = Arc::new(Resolver::new(tools.clone(), deps.runner));
        let auth = YtAuth::new(deps.store, tools.clone(), resolver.clone());
        let tube = Arc::new(InnerTube::new(deps.http.clone(), auth.clone()));
        YtMusicProvider {
            library: YtLibrary::new(tube, auth.clone()),
            playback: YtPlayback {
                player: Player::new(deps.source, deps.events, deps.http),
                resolver,
                auth: auth.clone(),
            },
            auth,
            tools,
        }
    }

    /// The logins, for the start-up update (step 8) to ask whether the helper is switched on.
    pub fn auth_state(&self) -> &YtAuth {
        &self.auth
    }

    /// The daily tool update (D5), for the host to run once after the window is up. It waits on
    /// the network and on disk: call it off the UI thread. It never runs while the helper is
    /// switched off, nor when there is no managed copy to look after.
    pub fn startup_update(&self) -> StartupUpdate {
        if !self.auth.helper_enabled() {
            return StartupUpdate::HelperOff;
        }
        if !self.tools.has_managed_copy() {
            return StartupUpdate::NoManagedCopy;
        }
        match self.tools.update_if_due() {
            Ok(None) => StartupUpdate::NotDue,
            Ok(Some(_)) => StartupUpdate::Checked,
            Err(_) => StartupUpdate::Failed,
        }
    }
}

impl Provider for YtMusicProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID,
            name: "YouTube Music",
            glyph: ProviderGlyph::Stream,
            caps: Capabilities {
                library: true,
                pcm_tap: true,
                eq: true,
                search: true,
                likes: true,
                explore: true,
                save_albums: true,
            },
            auth_steps: auth_steps(),
        }
    }

    fn auth(&self) -> &dyn AuthFlow {
        &self.auth
    }

    fn playback(&self) -> &dyn Playback {
        &self.playback
    }

    fn library(&self) -> Option<&dyn Library> {
        Some(&self.library)
    }

    fn image_hosts(&self) -> &'static [&'static str] {
        &IMAGE_HOSTS
    }
}

const BAD_URI: &str = "this is not a YouTube Music track";
const HELPER_OFF: &str = "Turn on the stream helper in Accounts first";
const NO_STREAM_TASK: &str = "the stream helper did not answer";

/// Playback of one track at a time: resolves the stream with yt-dlp, then the player thread
/// reads, decodes and writes it.
struct YtPlayback {
    player: Player,
    resolver: Arc<Resolver>,
    auth: YtAuth,
}

fn failed(text: &str) -> PlaybackError {
    PlaybackError(text.to_owned())
}

/// The stored cookies, when there are any; a failing store means none.
fn cookies_of(auth: &YtAuth) -> Option<Secret> {
    auth.cookies_secret().ok().flatten()
}

impl YtPlayback {
    /// A refresh for one track: resolves it anew, dropping what is cached.
    fn refresher(&self, id: &str) -> Box<dyn FnMut() -> Option<Stream> + Send> {
        let (resolver, auth, id) = (self.resolver.clone(), self.auth.clone(), id.to_owned());
        Box::new(move || resolver.refresh(&id, cookies_of(&auth).as_ref()).ok())
    }
}

impl Playback for YtPlayback {
    fn load<'a>(
        &'a self,
        uri: &'a str,
        play: bool,
        position_ms: u32,
    ) -> BoxFuture<'a, Result<(), PlaybackError>> {
        Box::pin(async move {
            let id = crate::ids::parse_track_uri(uri).ok_or_else(|| failed(BAD_URI))?;
            if !self.auth.helper_enabled() {
                return Err(failed(HELPER_OFF));
            }
            let (resolver, auth, video) = (self.resolver.clone(), self.auth.clone(), id.to_owned());
            // yt-dlp runs for seconds: not on the async thread.
            let stream = tokio::task::spawn_blocking(move || {
                resolver.resolve(&video, cookies_of(&auth).as_ref())
            })
            .await
            .map_err(|_| failed(NO_STREAM_TASK))?
            .map_err(|e| failed(&e.to_string()))?;
            let track = Track {
                uri: uri.to_owned(),
                stream,
                refresh: self.refresher(id),
            };
            self.player
                .load(track, play, position_ms)
                .await
                .map_err(failed)
        })
    }

    fn play(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.play().map_err(failed);
        Box::pin(async move { r })
    }

    fn pause(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.pause().map_err(failed);
        Box::pin(async move { r })
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.stop().map_err(failed);
        Box::pin(async move { r })
    }

    fn seek(&self, position_ms: u32) -> BoxFuture<'_, Result<(), PlaybackError>> {
        let r = self.player.seek(position_ms).map_err(failed);
        Box::pin(async move { r })
    }

    fn source_format(&self) -> Option<String> {
        self.player.label()
    }

    /// Resolves the next track's stream, so its start waits only for the audio. Does not fetch
    /// audio, and does not wait for yt-dlp: the transport that calls it has other commands to
    /// serve, so the resolve runs on a blocking thread of its own and a failure is dropped
    /// (the load will say it again).
    fn preload<'a>(&'a self, uri: &'a str) -> BoxFuture<'a, Result<(), PlaybackError>> {
        let id = crate::ids::parse_track_uri(uri).map(str::to_owned);
        Box::pin(async move {
            let id = id.ok_or_else(|| failed(BAD_URI))?;
            if self.auth.helper_enabled() {
                let (resolver, auth) = (self.resolver.clone(), self.auth.clone());
                drop(tokio::task::spawn_blocking(move || {
                    let _ = resolver.resolve(&id, cookies_of(&auth).as_ref());
                }));
            }
            Ok(())
        })
    }
}
