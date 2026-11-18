//! Spotify Connect: Whipspot as a speaker the user's Spotify apps can play on (D1 to D6).
//!
//! The Connect stack is its own librespot `Session`, `Player` and `Spirc`, beside the local
//! playback stack and never shared with it (D1). Its audio goes into the core chain as the engine
//! owner [`OWNER`] (D3), its volume is the engine's (D5, [`mixer`]), its player events become the
//! core's remote events ([`pump`]), and the core drives it through [`Remote`] on the
//! [`ConnectHost`].
//!
//! Lifecycle (one supervisor task per host): Connect runs exactly while it is turned on and a
//! Playback login is stored. Every stop and restart bumps an epoch; the supervisor closes every
//! run of an older epoch (graceful `shutdown`, at most 3 s, then `close`) and sends
//! `RemoteEnded` once per run, and [`ConnectHost::stop`] returns only after that (the MD137
//! class: no Connect session outlives or straddles a login change). A Spirc task that ends by
//! itself is retried after 5 s, 15 s, 60 s, then every 300 s, each attempt after the first on a
//! force-refreshed token; not logged in stops the retries until the next Playback login. No
//! mutex is held across an await.

pub(crate) mod backend;
pub(crate) mod mixer;
pub(crate) mod pump;

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use tokio::{
    sync::{Notify, broadcast, watch},
    time::Instant,
};

use whipspot_core::{
    events::UiEvent,
    provider::{
        BoxFuture, PlaybackError, ProviderContext, Remote, RemoteCommand,
        model::{AuthStepStatus, LoginState},
    },
};

use crate::{
    auth_manager::{AuthError, PlaybackTokens, TokenSource},
    provider::{ID, Quality, STEP_CONNECT},
};
use backend::{ConnectBackend, Running, SpircLike, StartCfg};
use mixer::ConnectMixer;

/// The engine owner the Connect audio writes as (D3): the composition root builds the Connect
/// context with `ctx.for_owner(OWNER)`. Never a provider id.
pub const OWNER: &str = "spotify.connect";

/// How the composition root sets the Connect speaker up.
pub struct ConnectOptions {
    /// Turned on (the `connect` step's state; saved by the app).
    pub enabled: bool,
    /// The device name the user's Spotify apps show (D9).
    pub name: String,
    /// The stable device id (D10).
    pub device_id: String,
    /// Whipspot's saved volume, 0..=100: the level the phone shows before the first takeover
    /// (D5, [`mixer::to_level`]).
    pub volume: u8,
    /// `ctx.for_owner(OWNER)`: the Connect player's source handle and the event sender.
    pub context: ProviderContext,
}

/// The host's settings ([`ConnectOptions`] without the context, plus the local quality).
pub(crate) struct HostCfg {
    pub(crate) enabled: bool,
    pub(crate) name: String,
    pub(crate) device_id: String,
    pub(crate) quality: Quality,
}

/// Waits before the retries after a failure; the last repeats.
const BACKOFF: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(300),
];
/// A run that lasted this long starts the retries from the first wait again.
const STABLE_RUN: Duration = Duration::from_secs(600);
/// How long a stopped Spirc gets to go inactive before it is closed.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);
/// Longest [`ConnectHost::stop`] waits for the supervisor (a start in flight is bounded by its
/// own timeout, the shutdown by [`SHUTDOWN_WAIT`]).
const SETTLE_LIMIT: Duration = Duration::from_secs(40);

/// A command reached a Spirc task that has already ended.
const NOT_RUNNING: &str = "Spotify Connect is not running";
const ACTION_ON: &str = "Turn on";
const ACTION_OFF: &str = "Turn off";
const DETAIL_STARTING: &str = "Starting";
const DETAIL_RETRYING: &str = "Could not reach Spotify; trying again";
const DETAIL_NEEDS_LOGIN: &str = "Log in to Playback first";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Off,
    Starting,
    Running,
    Retrying,
    /// Not logged in: no retries until the next Playback login.
    NeedsLogin,
}

struct Shared {
    enabled: bool,
    /// Set by [`ConnectHost::shutdown`]: the supervisor ends.
    closed: bool,
    /// Bumped by every stop and restart; a run of an older epoch is closed.
    epoch: u64,
    phase: Phase,
    quality: Quality,
    /// The running Spirc's controls (only while registered by the supervisor).
    control: Option<Arc<dyn SpircLike>>,
}

/// The Connect speaker: its supervisor's state and the core's [`Remote`].
pub(crate) struct ConnectHost {
    tokens: PlaybackTokens,
    backend: Arc<dyn ConnectBackend>,
    events: broadcast::Sender<UiEvent>,
    mixer: Arc<ConnectMixer>,
    name: String,
    device_id: String,
    shared: Mutex<Shared>,
    /// Wakes the supervisor after a change of `shared`. One waiter, so a change made while it is
    /// not waiting leaves a permit.
    wake: Notify,
    /// The epoch the supervisor has settled: nothing of an older epoch is alive.
    settled: watch::Sender<u64>,
}

impl ConnectHost {
    /// The host and its supervisor task. Must run inside a tokio runtime.
    pub(crate) fn spawn(
        tokens: PlaybackTokens,
        backend: Arc<dyn ConnectBackend>,
        mixer: Arc<ConnectMixer>,
        events: broadcast::Sender<UiEvent>,
        cfg: HostCfg,
    ) -> Arc<Self> {
        let HostCfg {
            enabled,
            name,
            device_id,
            quality,
        } = cfg;
        let host = Arc::new(ConnectHost {
            tokens,
            backend,
            events,
            mixer,
            name,
            device_id,
            shared: Mutex::new(Shared {
                enabled,
                closed: false,
                epoch: 0,
                phase: if enabled { Phase::Starting } else { Phase::Off },
                quality,
                control: None,
            }),
            wake: Notify::new(),
            settled: watch::Sender::new(0),
        });
        tokio::spawn(host.clone().supervise());
        host
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    fn control(&self) -> Option<Arc<dyn SpircLike>> {
        self.lock().control.clone()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.lock().enabled
    }

    /// The bitrate cap of the next start (the local player's, MD88).
    pub(crate) fn set_quality(&self, quality: Quality) {
        self.lock().quality = quality;
    }

    /// End whatever runs (a new epoch), apply `f`, and wait until the supervisor has closed it.
    async fn change(&self, f: impl FnOnce(&mut Shared)) {
        let target = {
            let mut s = self.lock();
            if s.closed {
                // The supervisor has ended: nothing runs and nothing would settle.
                return;
            }
            s.epoch += 1;
            f(&mut s);
            s.epoch
        };
        self.wake.notify_one();
        let mut rx = self.settled.subscribe();
        let _ = tokio::time::timeout(SETTLE_LIMIT, rx.wait_for(|e| *e >= target)).await;
    }

    /// The Playback login changed to a (possibly other) account: the running Connect ends and,
    /// when turned on, starts again on the new login.
    pub(crate) async fn restart(&self) {
        self.change(|s| {
            if s.phase == Phase::NeedsLogin {
                self.set_phase(&mut *s, Phase::Starting);
            }
        })
        .await;
    }

    /// The Playback login ended (logout, or a failed write that cleared it): Connect ends. When
    /// it is turned on, the supervisor finds no login and waits for the next one.
    pub(crate) async fn stop(&self) {
        self.change(|_| {}).await;
    }

    /// Turn Connect on (starts when a Playback login is stored) or off (ends it).
    pub(crate) async fn set_enabled(&self, on: bool) {
        if on {
            {
                let mut s = self.lock();
                if s.enabled {
                    return;
                }
                s.enabled = true;
                self.set_phase(&mut s, Phase::Starting);
            }
            self.wake.notify_one();
        } else {
            self.change(|s| {
                s.enabled = false;
                self.set_phase(&mut *s, Phase::Off);
            })
            .await;
        }
    }

    /// End Connect for good (app exit): the running device goes inactive and the supervisor
    /// ends. The on/off setting is kept.
    pub(crate) async fn shutdown(&self) {
        self.change(|s| s.closed = true).await;
    }

    /// The `connect` step: logged in while turned on, with its button and one status line.
    /// `playback` is the Playback login's state.
    pub(crate) fn step(&self, playback: &LoginState) -> AuthStepStatus {
        let s = self.lock();
        let (state, action, detail) = if !s.enabled {
            (LoginState::LoggedOut, ACTION_ON, None)
        } else {
            let detail = match (playback, s.phase) {
                (LoginState::LoggedOut, _) | (_, Phase::NeedsLogin) => {
                    DETAIL_NEEDS_LOGIN.to_owned()
                }
                (_, Phase::Running) => format!("Shown as {} in your Spotify apps", self.name),
                (_, Phase::Retrying) => DETAIL_RETRYING.to_owned(),
                (_, Phase::Starting | Phase::Off) => DETAIL_STARTING.to_owned(),
            };
            (LoginState::LoggedIn, ACTION_OFF, Some(detail))
        };
        AuthStepStatus {
            id: STEP_CONNECT.to_owned(),
            state,
            action: Some(action.to_owned()),
            detail,
        }
    }

    /// Set the phase; a change is announced so the Accounts view reads the step again (its
    /// status line follows the phase).
    fn set_phase(&self, s: &mut Shared, phase: Phase) {
        if s.phase != phase {
            log::info!("spotify connect: {:?} -> {phase:?}", s.phase);
            s.phase = phase;
            // No receivers is fine.
            let _ = self.events.send(UiEvent::AuthChanged {
                provider: ID.to_owned(),
            });
        }
    }

    fn cfg(&self) -> StartCfg {
        StartCfg {
            name: self.name.clone(),
            device_id: self.device_id.clone(),
            quality: self.lock().quality,
        }
    }

    async fn supervise(self: Arc<Self>) {
        // 0 for the first attempt after a (re)start; counts failures since.
        let mut attempt = 0usize;
        loop {
            let epoch = {
                let mut s = self.lock();
                self.settled.send_replace(s.epoch);
                if s.closed {
                    self.set_phase(&mut s, Phase::Off);
                    return;
                }
                if !s.enabled {
                    self.set_phase(&mut s, Phase::Off);
                    None
                } else if s.phase == Phase::NeedsLogin {
                    None
                } else {
                    self.set_phase(
                        &mut s,
                        if attempt == 0 {
                            Phase::Starting
                        } else {
                            Phase::Retrying
                        },
                    );
                    Some(s.epoch)
                }
            };
            let Some(epoch) = epoch else {
                self.wake.notified().await;
                attempt = 0;
                continue;
            };
            // Never cancelled: a refresh dropped half-way could lose a rotated refresh token.
            let token = if attempt == 0 {
                self.tokens.access_token().await
            } else {
                self.tokens.force_refresh().await
            };
            if self.epoch() != epoch {
                attempt = 0;
                continue;
            }
            let token = match token {
                Ok(t) => t,
                Err(AuthError::NotLoggedIn) => {
                    let mut s = self.lock();
                    if s.epoch == epoch {
                        self.set_phase(&mut s, Phase::NeedsLogin);
                    }
                    attempt = 0;
                    continue;
                }
                Err(e) => {
                    log::warn!("spotify connect: no playback token: {e}");
                    attempt = self.back_off(epoch, attempt).await;
                    continue;
                }
            };
            self.mixer.reset();
            let cfg = self.cfg();
            let started = self.backend.start(&token, &cfg).await;
            drop(token);
            if started.is_err() {
                log::warn!(
                    "spotify connect: speaker start failed (attempt {})",
                    attempt + 1
                );
            }
            let Ok(run) = started else {
                attempt = if self.epoch() == epoch {
                    self.back_off(epoch, attempt).await
                } else {
                    0
                };
                continue;
            };
            let since = Instant::now();
            if self.run(epoch, run).await {
                attempt = 0;
                continue;
            }
            // The task ended by itself.
            if since.elapsed() >= STABLE_RUN {
                attempt = 0;
            }
            attempt = self.back_off(epoch, attempt).await;
        }
    }

    /// Register `run` and wait until its task ends or the epoch moves on; then close it and send
    /// `RemoteEnded`. True when a stop or restart ended it.
    async fn run(&self, epoch: u64, run: Running) -> bool {
        let Running { control, mut done } = run;
        let registered = {
            let mut s = self.lock();
            let current = s.epoch == epoch;
            if current {
                s.control = Some(control.clone());
                self.set_phase(&mut s, Phase::Running);
            }
            current
        };
        let requested = if registered {
            loop {
                tokio::select! {
                    _ = &mut done => break false,
                    _ = self.wake.notified() => {
                        if self.epoch() != epoch {
                            break true;
                        }
                    }
                }
            }
        } else {
            true
        };
        // Only the supervisor sets it, and one run at a time: this is ours.
        self.lock().control = None;
        if requested {
            control.shutdown();
            let _ = tokio::time::timeout(SHUTDOWN_WAIT, &mut done).await;
        }
        control.close();
        // No receivers is fine. Idempotent in the core (ignored outside remote mode).
        let _ = self.events.send(UiEvent::RemoteEnded {
            provider: ID.to_owned(),
        });
        requested
    }

    /// Wait before the next attempt; returns its attempt number, or 0 when a stop or restart
    /// cut the wait short.
    async fn back_off(&self, epoch: u64, attempt: usize) -> usize {
        {
            let mut s = self.lock();
            if s.epoch == epoch {
                self.set_phase(&mut s, Phase::Retrying);
            }
        }
        let sleep = tokio::time::sleep(BACKOFF[attempt.min(BACKOFF.len() - 1)]);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return attempt + 1,
                _ = self.wake.notified() => {
                    if self.epoch() != epoch {
                        return 0;
                    }
                }
            }
        }
    }
}

impl Remote for ConnectHost {
    fn owner(&self) -> &str {
        OWNER
    }

    fn command(&self, cmd: RemoteCommand) -> BoxFuture<'_, Result<(), PlaybackError>> {
        Box::pin(async move {
            let Some(c) = self.control() else {
                // No session: the level is the next start's.
                if let RemoteCommand::Volume(p) = cmd {
                    self.mixer.idle(p);
                }
                return Ok(());
            };
            let sent = match cmd {
                RemoteCommand::Play => c.play(),
                RemoteCommand::Pause => c.pause(),
                RemoteCommand::Next => c.next(),
                RemoteCommand::Previous => c.prev(),
                RemoteCommand::Seek(ms) => c.set_position_ms(ms),
                // Expected back from Spirc before it is sent, so it is not reported as the
                // remote's; not sent when Spirc has it already.
                RemoteCommand::Volume(p) => match self.mixer.forward(p) {
                    Some(level) => c.set_volume(level),
                    None => Ok(()),
                },
            };
            sent.map_err(|_| PlaybackError(NOT_RUNNING.to_owned()))
        })
    }

    fn release(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(c) = self.control() {
                let _ = c.disconnect(true);
                // Spirc may have gone inactive without `SessionDisconnected` and its player still
                // plays; it drops the disconnect then. Pausing the player here silences it
                // whatever Spirc's state; on an active Spirc it is the pause its disconnect makes.
                c.pause_player();
            }
        })
    }
}

#[cfg(test)]
mod tests;
