//! The librespot side of the Connect speaker behind [`ConnectBackend`], so the supervisor is
//! tested on a fake. [`LibrespotConnect`] is the only code that names `librespot_connect`.
//!
//! * Each start builds a new `Session` (cache `None`: no reusable credentials on disk) and hands
//!   it, unconnected, to `Spirc::new`, which connects it after its dealer listeners are in place.
//! * The Connect `Player` is built once, on the first start, by step 3's `build_player` (a
//!   `CoreSink` over the connect context's source, `NoOpVolume`), and moved to each later
//!   session with `set_session`; its event pump runs for its whole life.
//! * Before `Spirc::new` the session's dealer is subscribed to [`HINT_URIS`] (the library
//!   watch's hints, D12): each message, whose content is never read, calls the hint. The
//!   listener tasks end with [`SpircLike::close`].
//! * [`SpircLike::close`] ends everything of one start: the Spirc task, the dealer listeners,
//!   the session and what the player plays. Nothing here logs; a library error is dropped for
//!   [`StartError`].

use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig, authentication::Credentials, config::DeviceType};
use librespot_playback::{mixer::Mixer, player::Player};
use tokio::{sync::broadcast, task::AbortHandle};

use whipspot_core::{engine::SourceWriter, events::UiEvent, provider::BoxFuture, secret::Secret};

use super::{mixer::ConnectMixer, pump};
use crate::{provider::Quality, session::build_player, trace};

/// Longest a start (session connect, dealer, first tokens) may take.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// The dealer prefixes whose messages hint that the user's library changed (D12).
const HINT_URIS: [&str; 2] = ["hm://playlist/", "hm://collection/"];

/// Called on each library hint from the dealer.
pub(crate) type Hint = Arc<dyn Fn() + Send + Sync>;

/// What one start needs besides the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartCfg {
    /// The device name the user's Spotify apps show.
    pub(crate) name: String,
    /// The stable device id (D10).
    pub(crate) device_id: String,
    pub(crate) quality: Quality,
}

/// A start failed (login, network, no audio, timeout). Retried by the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartError;

/// The Spirc task is gone; the command was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gone;

/// The controls of one running Connect device (librespot's `Spirc` handle).
pub(crate) trait SpircLike: Send + Sync {
    fn play(&self) -> Result<(), Gone>;
    fn pause(&self) -> Result<(), Gone>;
    fn next(&self) -> Result<(), Gone>;
    fn prev(&self) -> Result<(), Gone>;
    fn set_position_ms(&self, position_ms: u32) -> Result<(), Gone>;
    fn set_volume(&self, volume: u16) -> Result<(), Gone>;
    /// Stop being the active device (the controlling app sees it go inactive).
    fn disconnect(&self, pause: bool) -> Result<(), Gone>;
    /// Pause the Connect player itself, whatever Spirc's state. librespot-connect 0.8.0 drops
    /// every command while it is not the active device, and it can go inactive without stopping
    /// its player or sending `SessionDisconnected` (a failed `became_inactive` request, or a
    /// cluster update without a cluster).
    fn pause_player(&self);
    /// Ask the Spirc task to pause, go inactive and end (its `done` then resolves).
    fn shutdown(&self);
    /// End everything of this start at once: the task, the session and the player's track.
    /// Idempotent; called after every run, also after a clean `shutdown`.
    fn close(&self);
}

/// One started Connect device.
pub(crate) struct Running {
    pub(crate) control: Arc<dyn SpircLike>,
    /// Resolves when the Spirc task ended (its only failure signal).
    pub(crate) done: BoxFuture<'static, ()>,
}

/// Starts Connect devices (faked in tests).
pub(crate) trait ConnectBackend: Send + Sync {
    fn start<'a>(
        &'a self,
        access: &'a Secret,
        cfg: &'a StartCfg,
    ) -> BoxFuture<'a, Result<Running, StartError>>;
}

/// The real backend.
pub(crate) struct LibrespotConnect {
    source: SourceWriter,
    events: broadcast::Sender<UiEvent>,
    mixer: Arc<ConnectMixer>,
    hint: Hint,
    /// Built on the first start, kept for every later one (its sink stays on the source).
    player: tokio::sync::Mutex<Option<Arc<Player>>>,
}

impl LibrespotConnect {
    pub(crate) fn new(
        source: SourceWriter,
        events: broadcast::Sender<UiEvent>,
        mixer: Arc<ConnectMixer>,
        hint: Hint,
    ) -> Self {
        LibrespotConnect {
            source,
            events,
            mixer,
            hint,
            player: tokio::sync::Mutex::new(None),
        }
    }

    /// The Connect player on `session`: the kept one moved over, or a new one with its pump.
    async fn player_on(
        &self,
        session: &Session,
        quality: Quality,
    ) -> Result<Arc<Player>, StartError> {
        let mut slot = self.player.lock().await;
        if let Some(p) = slot.as_ref() {
            p.set_session(session.clone());
            return Ok(p.clone());
        }
        let (player, channel) = build_player(quality, session.clone(), self.source.clone())
            .await
            .map_err(|_| StartError)?;
        tokio::spawn(pump::run(
            channel,
            self.source.clone(),
            self.events.clone(),
            self.mixer.clone(),
        ));
        *slot = Some(player.clone());
        Ok(player)
    }
}

impl LibrespotConnect {
    /// Subscribe to the library hints on `session`'s dealer, which is not started yet (Spirc
    /// starts it). A subscription that fails is left out: the timer and the focus check remain.
    /// With the `--connect-trace` flag on, the namespaces of [`trace::PREFIXES`] are listened to
    /// as well, only to print each message's redacted URI (the content is dropped unread).
    fn listen_for_hints(&self, session: &Session) -> Vec<AbortHandle> {
        let mut tasks: Vec<AbortHandle> = HINT_URIS
            .iter()
            .filter_map(|uri| {
                let mut messages = session
                    .dealer()
                    .listen_for(*uri, |_message| Ok::<(), librespot_core::Error>(()))
                    .ok()?;
                let hint = self.hint.clone();
                let task = tokio::spawn(async move {
                    while messages.next().await.is_some() {
                        hint();
                    }
                });
                Some(task.abort_handle())
            })
            .collect();
        if trace::enabled() {
            tasks.extend(trace::PREFIXES.iter().filter_map(|prefix| {
                let mut messages = session
                    .dealer()
                    .listen_for(*prefix, |message| {
                        Ok::<_, librespot_core::Error>(message.uri)
                    })
                    .ok()?;
                let task = tokio::spawn(async move {
                    while let Some(item) = messages.next().await {
                        if let Ok(uri) = item {
                            trace::record(&uri);
                        }
                    }
                });
                Some(task.abort_handle())
            }));
        }
        tasks
    }
}

impl ConnectBackend for LibrespotConnect {
    fn start<'a>(
        &'a self,
        access: &'a Secret,
        cfg: &'a StartCfg,
    ) -> BoxFuture<'a, Result<Running, StartError>> {
        Box::pin(async move {
            let session = Session::new(
                SessionConfig {
                    device_id: cfg.device_id.clone(),
                    ..SessionConfig::default()
                },
                None,
            );
            let player = self.player_on(&session, cfg.quality).await?;
            let listeners = self.listen_for_hints(&session);
            let config = ConnectConfig {
                name: cfg.name.clone(),
                device_type: DeviceType::Computer,
                initial_volume: self.mixer.volume(),
                ..ConnectConfig::default()
            };
            let mixer: Arc<dyn Mixer> = self.mixer.clone();
            let credentials = Credentials::with_access_token(access.expose());
            let started = tokio::time::timeout(
                START_TIMEOUT,
                Spirc::new(config, session.clone(), credentials, player.clone(), mixer),
            )
            .await;
            let (spirc, task) = match started {
                Ok(Ok(started)) => started,
                Ok(Err(_)) | Err(_) => {
                    listeners.iter().for_each(AbortHandle::abort);
                    session.shutdown();
                    return Err(StartError);
                }
            };
            let handle = tokio::spawn(task);
            let control = Arc::new(LiveSpirc {
                spirc,
                session,
                player,
                task: handle.abort_handle(),
                listeners,
            });
            Ok(Running {
                control,
                done: Box::pin(async move {
                    let _ = handle.await;
                }),
            })
        })
    }
}

struct LiveSpirc {
    spirc: Spirc,
    session: Session,
    player: Arc<Player>,
    task: AbortHandle,
    /// The dealer listeners of the library hints.
    listeners: Vec<AbortHandle>,
}

fn sent<E>(r: Result<(), E>) -> Result<(), Gone> {
    r.map_err(|_| Gone)
}

impl SpircLike for LiveSpirc {
    fn play(&self) -> Result<(), Gone> {
        sent(self.spirc.play())
    }
    fn pause(&self) -> Result<(), Gone> {
        sent(self.spirc.pause())
    }
    fn next(&self) -> Result<(), Gone> {
        sent(self.spirc.next())
    }
    fn prev(&self) -> Result<(), Gone> {
        sent(self.spirc.prev())
    }
    fn set_position_ms(&self, position_ms: u32) -> Result<(), Gone> {
        sent(self.spirc.set_position_ms(position_ms))
    }
    fn set_volume(&self, volume: u16) -> Result<(), Gone> {
        sent(self.spirc.set_volume(volume))
    }
    fn disconnect(&self, pause: bool) -> Result<(), Gone> {
        sent(self.spirc.disconnect(pause))
    }
    fn pause_player(&self) {
        self.player.pause();
    }
    fn shutdown(&self) {
        let _ = self.spirc.shutdown();
    }
    fn close(&self) {
        self.task.abort();
        self.listeners.iter().for_each(AbortHandle::abort);
        self.player.stop();
        self.session.shutdown();
    }
}

/// A scripted backend for the crate's tests: records starts and calls, never touches the
/// network.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    use tokio::sync::{Notify, oneshot};

    /// One fake Spirc: records calls; its task ends on `shutdown` or [`FakeSpirc::end`].
    #[derive(Default)]
    pub(crate) struct FakeSpirc {
        calls: Mutex<Vec<String>>,
        end: Mutex<Option<oneshot::Sender<()>>>,
        /// `shutdown` leaves the task running (a Spirc that never answers).
        pub(crate) stuck: std::sync::atomic::AtomicBool,
        /// Not the active device: as librespot-connect 0.8.0, every command but the shutdown is
        /// dropped (recorded as `dropped <command>`); the player still takes its own calls.
        pub(crate) inactive: std::sync::atomic::AtomicBool,
    }

    impl FakeSpirc {
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        /// The Spirc task ends by itself (the dealer dropped, the session died).
        pub(crate) fn end(&self) {
            if let Some(tx) = self.end.lock().unwrap().take() {
                let _ = tx.send(());
            }
        }
        fn call(&self, c: impl Into<String>) -> Result<(), Gone> {
            self.calls.lock().unwrap().push(c.into());
            Ok(())
        }
        /// A command to the Spirc task: taken (`Ok`), then dropped while inactive.
        fn command(&self, c: impl Into<String>) -> Result<(), Gone> {
            let c = c.into();
            if self.inactive.load(std::sync::atomic::Ordering::SeqCst) {
                return self.call(format!("dropped {c}"));
            }
            self.call(c)
        }
    }

    impl SpircLike for FakeSpirc {
        fn play(&self) -> Result<(), Gone> {
            self.command("play")
        }
        fn pause(&self) -> Result<(), Gone> {
            self.command("pause")
        }
        fn next(&self) -> Result<(), Gone> {
            self.command("next")
        }
        fn prev(&self) -> Result<(), Gone> {
            self.command("prev")
        }
        fn set_position_ms(&self, ms: u32) -> Result<(), Gone> {
            self.command(format!("seek {ms}"))
        }
        fn set_volume(&self, v: u16) -> Result<(), Gone> {
            self.command(format!("volume {v}"))
        }
        fn disconnect(&self, pause: bool) -> Result<(), Gone> {
            self.command(format!("disconnect {pause}"))
        }
        fn pause_player(&self) {
            let _ = self.call("pause player");
        }
        fn shutdown(&self) {
            let _ = self.call("shutdown");
            if !self.stuck.load(std::sync::atomic::Ordering::SeqCst) {
                self.end();
            }
        }
        fn close(&self) {
            let _ = self.call("close");
            self.end();
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeConnect {
        /// The access token and config of each start.
        starts: Mutex<Vec<(String, StartCfg)>>,
        /// Scripted start results, `Ok` when empty.
        pub(crate) results: Mutex<VecDeque<Result<(), StartError>>>,
        runs: Mutex<Vec<Arc<FakeSpirc>>>,
        /// When set, a start says it entered (the first `Notify`) and waits until released.
        pub(crate) gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    }

    impl FakeConnect {
        pub(crate) fn tokens(&self) -> Vec<String> {
            self.starts
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.0.clone())
                .collect()
        }
        pub(crate) fn cfgs(&self) -> Vec<StartCfg> {
            self.starts
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.1.clone())
                .collect()
        }
        pub(crate) fn runs(&self) -> Vec<Arc<FakeSpirc>> {
            self.runs.lock().unwrap().clone()
        }
        pub(crate) fn last(&self) -> Arc<FakeSpirc> {
            self.runs().last().cloned().expect("a run")
        }
    }

    impl ConnectBackend for FakeConnect {
        fn start<'a>(
            &'a self,
            access: &'a Secret,
            cfg: &'a StartCfg,
        ) -> BoxFuture<'a, Result<Running, StartError>> {
            Box::pin(async move {
                self.starts
                    .lock()
                    .unwrap()
                    .push((access.expose().to_owned(), cfg.clone()));
                let gate = self.gate.lock().unwrap().clone();
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
                let (tx, rx) = oneshot::channel();
                let spirc = Arc::new(FakeSpirc {
                    end: Mutex::new(Some(tx)),
                    ..FakeSpirc::default()
                });
                self.runs.lock().unwrap().push(spirc.clone());
                Ok(Running {
                    control: spirc,
                    done: Box::pin(async move {
                        let _ = rx.await;
                    }),
                })
            })
        }
    }
}
