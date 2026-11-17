//! [`ConnectMixer`]: the volume librespot's Connect device reports and is told (D5).
//!
//! The Connect player plays at full scale (`NoOpVolume`); the engine owns the volume. The mixer's
//! level is Spirc's own: only Spirc's `Mixer::set_volume` (and, with no session, [`ConnectMixer::idle`])
//! changes it, so Spirc's "unchanged level" skip check stays true. A level the core forwards
//! ([`ConnectMixer::forward`]) comes back through that call; it is not reported (no echo that
//! would fight a slider being dragged). Spirc drops every command while it is not the active
//! device, so a level forwarded then never comes back: the levels in flight are forgotten at each
//! start and at each activation ([`ConnectMixer::reset`]).
//!
//! Invariant: once Spirc has applied every forwarded level, the core's level (as far as the
//! mixer knows: the last one forwarded or reported) equals Spirc's. So a `set_volume` is reported
//! as `UiEvent::RemoteVolume` exactly when its percent changed and differs from the core's, except an echo
//! with newer forwarded levels still in flight (a drag passing through): that one is suppressed,
//! the last echo settles it. A level the core sent comes back reported only when a remote change
//! landed between and Spirc's final level is not the core's.

use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU16, Ordering},
    },
};

use librespot_core::Error;
use librespot_playback::mixer::{Mixer, MixerConfig};
use tokio::sync::broadcast;

use whipspot_core::events::UiEvent;

use crate::provider::ID;

/// librespot's own default Connect volume (50 %).
pub(crate) const DEFAULT_LEVEL: u16 = u16::MAX / 2;
/// Most forwarded levels in flight (Spirc applies one per command and sends a network notify
/// after each, so a drag can run ahead of it). Consecutive equal levels are not sent, so this
/// is a long drag of distinct levels; past it the oldest is forgotten, and the last echo still
/// sets the core right.
const MAX_IN_FLIGHT: usize = 512;

/// librespot's level (0..=65535) as a percent: `round(v * 100 / 65535)`.
pub(crate) fn to_percent(v: u16) -> u8 {
    ((u32::from(v) * 100 + 32_767) / 65_535) as u8
}

/// A percent (capped at 100) as librespot's level: `round(p * 65535 / 100)`. Exact round trip
/// with [`to_percent`] for 0..=100.
pub(crate) fn to_level(p: u8) -> u16 {
    ((u32::from(p.min(100)) * 65_535 + 50) / 100) as u16
}

pub(crate) struct ConnectMixer {
    /// Spirc's level (what `Mixer::volume` answers). Written under `echo`.
    level: AtomicU16,
    echo: Mutex<Echo>,
    events: broadcast::Sender<UiEvent>,
}

struct Echo {
    /// Levels sent to Spirc that it has not applied yet, oldest first.
    in_flight: VecDeque<u16>,
    /// The core's level as far as the mixer knows: the last one forwarded or reported.
    core: u16,
}

impl ConnectMixer {
    pub(crate) fn new(level: u16, events: broadcast::Sender<UiEvent>) -> Self {
        ConnectMixer {
            level: AtomicU16::new(level),
            echo: Mutex::new(Echo {
                in_flight: VecDeque::new(),
                core: level,
            }),
            events,
        }
    }

    fn echo(&self) -> std::sync::MutexGuard<'_, Echo> {
        self.echo.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The core set `percent` while a Connect session runs: the level to send to Spirc, expected
    /// back; `None` when Spirc already has it (or will, once the levels in flight land), so
    /// nothing is sent. The mixer's level is left for Spirc to set.
    pub(crate) fn forward(&self, percent: u8) -> Option<u16> {
        let v = to_level(percent);
        let mut e = self.echo();
        e.core = v;
        let ahead = e
            .in_flight
            .back()
            .copied()
            .unwrap_or_else(|| self.level.load(Ordering::SeqCst));
        if ahead == v {
            return None;
        }
        if e.in_flight.len() == MAX_IN_FLIGHT {
            e.in_flight.pop_front();
        }
        e.in_flight.push_back(v);
        Some(v)
    }

    /// The core set `percent` with no Connect session: it becomes the level the next session
    /// starts at.
    pub(crate) fn idle(&self, percent: u8) {
        let v = to_level(percent);
        let mut e = self.echo();
        e.core = v;
        self.level.store(v, Ordering::SeqCst);
    }

    /// Forget the levels in flight: at each start (the old Spirc is gone) and at each activation
    /// (the pump, on `SessionConnected`, before the core hears `RemoteStarted` and sends its
    /// takeover level; what Spirc was sent while not active it dropped). A level Spirc still
    /// applies after this comes back as a remote one: reported only when it differs from the
    /// core's, and the last one Spirc applies settles the core.
    pub(crate) fn reset(&self) {
        self.echo().in_flight.clear();
    }
}

impl Mixer for ConnectMixer {
    /// Never called by Spirc (it takes the mixer as built); the trait requires it.
    fn open(_: MixerConfig) -> Result<Self, Error> {
        Ok(ConnectMixer::new(DEFAULT_LEVEL, broadcast::channel(1).0))
    }

    fn volume(&self) -> u16 {
        self.level.load(Ordering::SeqCst)
    }

    fn set_volume(&self, volume: u16) {
        let report = {
            let mut e = self.echo();
            let old = self.level.swap(volume, Ordering::SeqCst);
            // Spirc applies commands in order: an echo also settles every older level (applied,
            // or skipped as unchanged).
            let echo = match e.in_flight.iter().position(|v| *v == volume) {
                Some(i) => {
                    e.in_flight.drain(..=i);
                    true
                }
                None => false,
            };
            let passing = echo && !e.in_flight.is_empty();
            let report = !passing
                && to_percent(volume) != to_percent(old)
                && to_percent(volume) != to_percent(e.core);
            if report {
                e.core = volume;
            }
            report
        };
        if report {
            // No receivers is fine.
            let _ = self.events.send(UiEvent::RemoteVolume {
                provider: ID.to_owned(),
                volume: to_percent(volume),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer(level: u16) -> (ConnectMixer, broadcast::Receiver<UiEvent>) {
        let (tx, rx) = broadcast::channel(64);
        (ConnectMixer::new(level, tx), rx)
    }

    fn reported(rx: &mut broadcast::Receiver<UiEvent>) -> Vec<u8> {
        let mut out = vec![];
        while let Ok(ev) = rx.try_recv() {
            match ev {
                UiEvent::RemoteVolume { provider, volume } => {
                    assert_eq!(provider, "spotify");
                    out.push(volume);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        out
    }

    #[test]
    fn percent_round_trip_is_exact() {
        for p in 0..=100u8 {
            assert_eq!(to_percent(to_level(p)), p, "{p}");
        }
        assert_eq!(to_level(0), 0);
        assert_eq!(to_level(100), u16::MAX);
        assert_eq!(to_level(250), u16::MAX);
        assert_eq!(to_level(40), 26_214);
        assert_eq!(to_percent(u16::MAX), 100);
        assert_eq!(to_percent(DEFAULT_LEVEL), 50);
    }

    #[test]
    fn a_remote_level_is_reported_only_on_a_change() {
        let (m, mut rx) = mixer(to_level(50));
        m.set_volume(to_level(50));
        assert!(reported(&mut rx).is_empty(), "unchanged level");
        m.set_volume(to_level(70));
        assert_eq!(reported(&mut rx), [70]);
        assert_eq!(m.volume(), to_level(70));
        m.set_volume(to_level(70));
        // A different level inside the same percent is no change either.
        m.set_volume(to_level(70) + 1);
        assert!(reported(&mut rx).is_empty());
        m.set_volume(0);
        assert_eq!(reported(&mut rx), [0]);
    }

    /// What the core does in remote mode: forward, and the level Spirc is sent.
    fn sent(m: &ConnectMixer, p: u8) -> u16 {
        m.forward(p).expect("sent to Spirc")
    }

    #[test]
    fn a_forwarded_level_does_not_echo() {
        // D5: no echo while a slider is dragged.
        let (m, mut rx) = mixer(to_level(50));
        let a = sent(&m, 40);
        let b = sent(&m, 41);
        let c = sent(&m, 42);
        // The level stays Spirc's until it applies them.
        assert_eq!(m.volume(), to_level(50));
        m.set_volume(a);
        m.set_volume(b);
        m.set_volume(c);
        assert!(reported(&mut rx).is_empty());
        assert_eq!(m.volume(), c);
        // Then the remote moves it: reported.
        m.set_volume(to_level(60));
        assert_eq!(reported(&mut rx), [60]);
    }

    #[test]
    fn twenty_forwards_then_twenty_echoes_report_nothing() {
        let (m, mut rx) = mixer(to_level(50));
        let levels: Vec<u16> = (20..40u8).map(|p| sent(&m, p)).collect();
        for v in &levels {
            m.set_volume(*v);
        }
        assert!(reported(&mut rx).is_empty());
        assert_eq!(m.volume(), to_level(39));
        assert!(m.echo().in_flight.is_empty());
    }

    #[test]
    fn a_drag_ends_where_the_user_let_go() {
        // Every frame forwards, often the same percent; there and back across the range.
        let (m, mut rx) = mixer(to_level(50));
        let path: Vec<u8> = (50..=100).chain((0..=100).rev()).chain(0..=37).collect();
        let mut in_spirc = vec![];
        for p in path {
            for _ in 0..3 {
                in_spirc.extend(m.forward(p));
            }
        }
        // Equal consecutive levels are sent once.
        assert!(in_spirc.windows(2).all(|w| w[0] != w[1]));
        for v in in_spirc {
            m.set_volume(v);
        }
        assert!(reported(&mut rx).is_empty());
        assert_eq!(m.volume(), to_level(37));
        // The next remote change is the remote's.
        m.set_volume(to_level(80));
        assert_eq!(reported(&mut rx), [80]);
    }

    #[test]
    fn a_level_spirc_has_is_not_sent_and_hides_no_remote_change() {
        // Takeover at the device's own level: Spirc would skip it, so it is not sent.
        let (m, mut rx) = mixer(to_level(50));
        assert_eq!(m.forward(50), None);
        // A remote change to another level and back: both reported.
        m.set_volume(to_level(70));
        m.set_volume(to_level(50));
        assert_eq!(reported(&mut rx), [70, 50]);
    }

    #[test]
    fn a_level_spirc_never_applies_hides_no_remote_change() {
        // Sent, but Spirc drops it (not active): it stays in flight.
        let (m, mut rx) = mixer(to_level(50));
        let _ = sent(&m, 40);
        // The remote lands on it: the core has 40 already, nothing to report.
        m.set_volume(to_level(40));
        assert!(reported(&mut rx).is_empty());
        // Every later remote change is reported, the forwarded level included.
        m.set_volume(to_level(60));
        m.set_volume(to_level(40));
        assert_eq!(reported(&mut rx), [60, 40]);
    }

    #[test]
    fn a_remote_change_during_a_drag_leaves_the_core_at_the_devices_level() {
        let (m, mut rx) = mixer(to_level(50));
        let a = sent(&m, 40);
        let b = sent(&m, 41);
        // Spirc takes the remote's level before the forwarded ones.
        m.set_volume(to_level(70));
        assert_eq!(reported(&mut rx), [70]);
        m.set_volume(a);
        assert!(reported(&mut rx).is_empty(), "a drag passing through");
        // Spirc ends at the forwarded 41, the core was told 70: it is told 41.
        m.set_volume(b);
        assert_eq!(reported(&mut rx), [41]);
        assert_eq!(m.volume(), b);
    }

    #[test]
    fn in_flight_levels_are_bounded_and_the_last_echo_settles_the_core() {
        let (m, mut rx) = mixer(0);
        let mut in_spirc = vec![];
        // Distinct consecutive levels, more than are kept.
        for i in 0..MAX_IN_FLIGHT + 20 {
            in_spirc.push(sent(&m, if i % 2 == 0 { 10 } else { 90 }));
        }
        assert_eq!(m.echo().in_flight.len(), MAX_IN_FLIGHT);
        for v in &in_spirc {
            m.set_volume(*v);
        }
        // The core ends at the level Spirc ends at, whatever was reported on the way.
        let last = *in_spirc.last().unwrap();
        let got = reported(&mut rx);
        assert!(got.last().is_none_or(|p| *p == to_percent(last)), "{got:?}");
        assert_eq!(to_percent(m.echo().core), to_percent(last));
    }

    #[test]
    fn reset_forgets_the_levels_in_flight() {
        let (m, mut rx) = mixer(to_level(50));
        let _ = sent(&m, 5);
        m.reset();
        assert!(m.echo().in_flight.is_empty());
        // A new session starts at the mixer's level: nothing to report.
        m.set_volume(m.volume());
        assert!(reported(&mut rx).is_empty());
    }

    #[test]
    fn with_no_session_a_level_is_the_next_starts() {
        let (m, mut rx) = mixer(to_level(50));
        m.idle(30);
        assert_eq!(m.volume(), to_level(30));
        assert!(m.echo().in_flight.is_empty());
        // Spirc's start sets it: no report.
        m.set_volume(to_level(30));
        assert!(reported(&mut rx).is_empty());
    }
}
