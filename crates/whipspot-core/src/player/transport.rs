//! The transport task: every [`Playback`](crate::provider::Playback) call runs here, one after
//! another (whipspot's transport mutex as a queue). It owns the active provider; the control
//! loop owns everything the state shows and commits a load only when told it succeeded.
//!
//! Remote mode: `Takeover` stops the active provider and gives the engine to a provider's
//! remote (its own owner id, D13); `Remote` forwards a command and `Release` gives remote
//! control up. Every [`Remote`](crate::provider::Remote) call runs here too, so a takeover, a
//! release and the next load happen in the order the control loop sent them.

use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use super::control::Internal;
use crate::engine::OwnerSwitch;
use crate::provider::{Provider, ProviderRegistry, RemoteCommand};

pub(crate) enum TransportCmd {
    Load {
        seq: u64,
        uri: String,
        play: bool,
        position_ms: u32,
    },
    Op(Op),
    /// Ask the active provider to prepare `uri` as the next track (MD35); never reported.
    Preload {
        uri: String,
    },
    /// Stop the active provider and give the engine to `provider`'s remote owner. Ignored for an
    /// unknown provider, one with no remote, or a remote whose owner is a provider id.
    Takeover {
        provider: &'static str,
    },
    /// Forward a command to the remote taken over; with none, nothing runs.
    Remote(RemoteCommand),
    /// Give remote control up; with no remote taken over, nothing runs. The engine owner stays
    /// as it is: the next load sets its own.
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Play,
    Pause,
    Stop,
    Seek(u32),
}

pub(crate) enum TransportReport {
    /// `provider` is the id that loaded it, `None` when the load failed (or nothing resolved).
    Loaded {
        seq: u64,
        provider: Option<&'static str>,
    },
    /// An op ran on the active provider. Ops with no active provider run nothing and report
    /// nothing.
    Done { op: Op, ok: bool },
    /// A remote command ran; `ok` is its result.
    Remote { ok: bool },
}

pub(crate) async fn run(
    registry: Arc<ProviderRegistry>,
    owner: OwnerSwitch,
    mut rx: UnboundedReceiver<TransportCmd>,
    report: UnboundedSender<Internal>,
) {
    let mut active: Option<Arc<dyn Provider>> = None;
    // The provider whose remote was taken over (it has a `remote()`).
    let mut remote: Option<Arc<dyn Provider>> = None;
    while let Some(cmd) = rx.recv().await {
        let r = match cmd {
            TransportCmd::Load {
                seq,
                uri,
                play,
                position_ms,
            } => {
                let provider = match registry.resolve_uri(&uri).cloned() {
                    None => None,
                    Some(p) => load(&mut active, &owner, p, &uri, play, position_ms).await,
                };
                TransportReport::Loaded { seq, provider }
            }
            TransportCmd::Preload { uri } => {
                let owner = registry.resolve_uri(&uri).map(|p| p.info().id);
                if let Some(p) = active.as_ref().filter(|p| Some(p.info().id) == owner) {
                    let _ = p.playback().preload(&uri).await;
                }
                continue;
            }
            TransportCmd::Takeover { provider } => {
                if let Some(p) = registry.get(provider).cloned() {
                    takeover(&registry, &mut active, &owner, p, &mut remote).await;
                }
                continue;
            }
            TransportCmd::Remote(cmd) => {
                let Some(r) = remote.as_ref().and_then(|p| p.remote()) else {
                    continue;
                };
                TransportReport::Remote {
                    ok: r.command(cmd).await.is_ok(),
                }
            }
            TransportCmd::Release => {
                if let Some(r) = remote.take() {
                    if let Some(r) = r.remote() {
                        r.release().await;
                    }
                }
                continue;
            }
            TransportCmd::Op(op) => {
                let Some(p) = active.as_ref() else {
                    continue;
                };
                let pb = p.playback();
                let res = match op {
                    Op::Play => pb.play().await,
                    Op::Pause => pb.pause().await,
                    Op::Stop => pb.stop().await,
                    Op::Seek(ms) => pb.seek(ms).await,
                };
                TransportReport::Done {
                    op,
                    ok: res.is_ok(),
                }
            }
        };
        if report.send(Internal::Transport(r)).is_err() {
            break;
        }
    }
}

/// Port of whipspot's `load_impl`: the new provider loads first; only on success does it become
/// active, and the previous one (if different) is stopped, best effort.
///
/// Source ownership (D13): the new provider owns the engine before its load runs, so the
/// previous provider's stop (asynchronous, from its own thread) can never pause, end or
/// interrupt the new audio. A failed load gives the engine back to the previous owner.
async fn load(
    active: &mut Option<Arc<dyn Provider>>,
    owner: &OwnerSwitch,
    p: Arc<dyn Provider>,
    uri: &str,
    play: bool,
    position_ms: u32,
) -> Option<&'static str> {
    let id = p.info().id;
    let previous = owner.owner();
    owner.set_owner(id);
    if p.playback().load(uri, play, position_ms).await.is_err() {
        match previous {
            Some(prev) => owner.set_owner(&prev),
            None => owner.clear_owner(),
        }
        return None;
    }
    if let Some(prev) = active.replace(p) {
        if prev.info().id != id {
            let _ = prev.playback().stop().await;
        }
    }
    Some(id)
}

/// Whether `p` can be taken over by its remote: it has one, and the remote's owner is neither
/// empty nor the id of a provider (its own included), so the owner switch separates the two.
pub(crate) fn can_take_over(registry: &ProviderRegistry, p: &dyn Provider) -> bool {
    p.remote().is_some_and(|r| {
        let id = r.owner();
        !id.is_empty() && id != p.info().id && registry.get(id).is_none()
    })
}

/// A remote takes over: the engine is its owner's first (so the stopped provider's late calls
/// from its own thread are dropped, D13), then the active provider is stopped, best effort.
/// Nothing happens for a provider that [`can_take_over`] declines.
async fn takeover(
    registry: &ProviderRegistry,
    active: &mut Option<Arc<dyn Provider>>,
    owner: &OwnerSwitch,
    p: Arc<dyn Provider>,
    remote: &mut Option<Arc<dyn Provider>>,
) {
    let Some(r) = p.remote().filter(|_| can_take_over(registry, p.as_ref())) else {
        return;
    };
    owner.set_owner(r.owner());
    if let Some(prev) = active.take() {
        let _ = prev.playback().stop().await;
    }
    *remote = Some(p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, EngineConfig};
    use crate::provider::fake::FakeProvider;

    fn calls(p: &FakeProvider) -> Vec<String> {
        p.calls.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn load_takes_the_source_first_and_gives_it_back_on_failure() {
        let parts = Engine::new(EngineConfig::default(), None);
        let owner = parts.source.owner_switch();
        let a = Arc::new(FakeProvider::new("a", false).with_owner_probe(owner.clone()));
        let b = Arc::new(FakeProvider::new("b", false).with_owner_probe(owner.clone()));
        let mut active: Option<Arc<dyn Provider>> = None;

        // The very first load fails: back to no owner.
        a.fail_loads(true);
        assert_eq!(
            load(&mut active, &owner, a.clone(), "a:1", true, 0).await,
            None
        );
        assert_eq!(owner.owner(), None);
        a.fail_loads(false);

        // A owns the engine during its own load.
        assert_eq!(
            load(&mut active, &owner, a.clone(), "a:2", true, 0).await,
            Some("a")
        );
        assert_eq!(calls(&a)[2..], ["load a:2 true 0", "owner a"]);
        assert_eq!(owner.owner().as_deref(), Some("a"));

        // B's load fails: B owned the engine while it ran, then A again; A keeps playing.
        b.fail_loads(true);
        assert_eq!(
            load(&mut active, &owner, b.clone(), "b:1", true, 0).await,
            None
        );
        assert_eq!(calls(&b), ["load b:1 true 0", "owner b"]);
        assert_eq!(owner.owner().as_deref(), Some("a"));
        assert!(!calls(&a).contains(&"stop".to_owned()));

        // B's load succeeds: B owns the engine, and A is stopped after it.
        b.fail_loads(false);
        assert_eq!(
            load(&mut active, &owner, b.clone(), "b:2", false, 5).await,
            Some("b")
        );
        assert_eq!(owner.owner().as_deref(), Some("b"));
        assert_eq!(calls(&b)[2..], ["load b:2 false 5", "owner b"]);
        assert_eq!(calls(&a).last().map(String::as_str), Some("stop"));
    }
}
