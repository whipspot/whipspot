//! The library watch (D12, D13): notices that the user's Spotify library changed somewhere else
//! (the phone's app) and tells the front-end with one [`UiEvent::LibraryChanged`], so its lists
//! load again.
//!
//! A check asks for a [`Fingerprint`] (playlist version stamps, liked total and newest
//! `added_at`) and compares it with the last one. The first check only stores it. A failed or
//! malformed answer changes nothing. Checks are asked for by
//!
//! * the front-end, on window focus ([`LibraryWatch::check`], at most one per 30 s),
//! * a 5-minute timer ([`LibraryWatch::spawn_timer`]),
//! * the Connect session's dealer, on any message under `hm://playlist/` or `hm://collection/`
//!   ([`LibraryWatch::hint`], coalesced for 2 s; the message itself is never read).
//!
//! At most one check runs at a time. A Library login or logout ([`LibraryWatch::reset`]) drops
//! the stored fingerprint, and a check that was in flight across it is discarded (it may have
//! been another account's): the MD137 rule. Nothing here logs.

use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering::SeqCst},
    },
    time::Duration,
};

use tokio::{sync::broadcast, time::Instant};

use crate::{
    provider::ID,
    webapi::{self, Fingerprint, WebApi},
};
use whipspot_core::{events::UiEvent, provider::BoxFuture};

/// The least time between two checks asked for by the front-end (window focus).
pub(crate) const FOCUS_GAP: Duration = Duration::from_secs(30);
/// The time between the timer's checks.
pub(crate) const TIMER_PERIOD: Duration = Duration::from_secs(300);
/// Dealer messages that arrive within this time of the first one make one check.
pub(crate) const HINT_WINDOW: Duration = Duration::from_secs(2);

/// Where a fingerprint comes from (faked in tests).
pub(crate) trait FingerprintSource: Send + Sync {
    fn fingerprint(&self) -> BoxFuture<'_, Result<Fingerprint, webapi::Error>>;
}

impl FingerprintSource for WebApi {
    fn fingerprint(&self) -> BoxFuture<'_, Result<Fingerprint, webapi::Error>> {
        Box::pin(WebApi::fingerprint(self))
    }
}

#[derive(Default)]
struct State {
    last: Option<Fingerprint>,
    /// When the last check started, for the focus gap.
    started: Option<Instant>,
    /// Bumped by [`LibraryWatch::reset`]; a check that read an older one stores nothing.
    generation: u64,
}

pub(crate) struct LibraryWatch {
    source: Arc<dyn FingerprintSource>,
    events: broadcast::Sender<UiEvent>,
    state: Mutex<State>,
    /// Held for the length of a check: at most one at a time.
    running: tokio::sync::Mutex<()>,
    /// A hint's wait is under way; later hints join it.
    hint_armed: AtomicBool,
}

impl LibraryWatch {
    pub(crate) fn new(
        source: Arc<dyn FingerprintSource>,
        events: broadcast::Sender<UiEvent>,
    ) -> Arc<Self> {
        Arc::new(LibraryWatch {
            source,
            events,
            state: Mutex::new(State::default()),
            running: tokio::sync::Mutex::new(()),
            hint_armed: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forget the stored fingerprint: the Library login changed. The next check is a first one.
    pub(crate) fn reset(&self) {
        let mut s = self.lock();
        s.generation += 1;
        s.last = None;
        s.started = None;
    }

    /// A check the front-end asked for: skipped when one ran in the last [`FOCUS_GAP`] or one
    /// runs now.
    pub(crate) async fn check_limited(&self) {
        let Ok(_running) = self.running.try_lock() else {
            return;
        };
        {
            let mut s = self.lock();
            let now = Instant::now();
            if s.started.is_some_and(|t| now.duration_since(t) < FOCUS_GAP) {
                return;
            }
            s.started = Some(now);
        }
        self.compare().await;
    }

    /// A check that ignores the gap (the timer, a dealer hint); waits for one that runs now.
    async fn check_now(&self) {
        let _running = self.running.lock().await;
        self.lock().started = Some(Instant::now());
        self.compare().await;
    }

    /// Fetch, compare, store, and send at most one event. The caller holds `running`.
    async fn compare(&self) {
        let generation = self.lock().generation;
        let Ok(fresh) = self.source.fingerprint().await else {
            return;
        };
        let changed = {
            let mut s = self.lock();
            if s.generation != generation {
                return;
            }
            let changed = s.last.as_ref().is_some_and(|old| *old != fresh);
            s.last = Some(fresh);
            changed
        };
        if changed {
            // No receivers is fine.
            let _ = self.events.send(UiEvent::LibraryChanged {
                provider: ID.to_owned(),
            });
        }
    }

    /// The dealer saw a playlist or collection message: one check after [`HINT_WINDOW`], however
    /// many messages come meanwhile. Must run inside a tokio runtime.
    pub(crate) fn hint(self: &Arc<Self>) {
        if self.hint_armed.swap(true, SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(HINT_WINDOW).await;
            let Some(watch) = weak.upgrade() else {
                return;
            };
            // Disarmed before the check, so a message that arrives during it makes the next.
            watch.hint_armed.store(false, SeqCst);
            watch.check_now().await;
        });
    }

    /// A check at once and then one every `period`, for as long as the watch lives. Must run
    /// inside a tokio runtime.
    pub(crate) fn spawn_timer(self: &Arc<Self>, period: Duration) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                watch.check_now().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering::SeqCst},
    };
    use tokio::sync::Notify;

    /// Move paused time on and let the tasks it wakes run.
    async fn advance(d: Duration) {
        tokio::time::advance(d).await;
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    /// A hint, and the task it spawns runs far enough to start its wait.
    async fn hint(w: &Arc<LibraryWatch>) {
        w.hint();
        tokio::task::yield_now().await;
    }

    fn fp(playlists: &[(&str, &str)], total: u32, newest: Option<&str>) -> Fingerprint {
        Fingerprint {
            playlists: playlists
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect(),
            liked_total: total,
            liked_newest: newest.map(str::to_owned),
        }
    }

    fn base() -> Fingerprint {
        fp(&[("a", "s1"), ("b", "s2")], 10, Some("2026-01-01"))
    }

    /// Answers from a script (`base()` when it runs dry), counts the asks, and can hold an ask
    /// until released.
    #[derive(Default)]
    struct Fake {
        script: Mutex<VecDeque<Result<Fingerprint, webapi::Error>>>,
        asks: AtomicUsize,
        gate: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    }

    impl Fake {
        fn push(&self, r: Result<Fingerprint, webapi::Error>) {
            self.script.lock().unwrap().push_back(r);
        }
        fn asks(&self) -> usize {
            self.asks.load(SeqCst)
        }
    }

    impl FingerprintSource for Fake {
        fn fingerprint(&self) -> BoxFuture<'_, Result<Fingerprint, webapi::Error>> {
            Box::pin(async move {
                self.asks.fetch_add(1, SeqCst);
                let gate = self.gate.lock().unwrap().clone();
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                self.script
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Ok(base()))
            })
        }
    }

    fn rig() -> (Arc<LibraryWatch>, Arc<Fake>, broadcast::Receiver<UiEvent>) {
        let fake = Arc::new(Fake::default());
        let (tx, rx) = broadcast::channel(16);
        (LibraryWatch::new(fake.clone(), tx), fake, rx)
    }

    fn events(rx: &mut broadcast::Receiver<UiEvent>) -> Vec<UiEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    fn changed() -> UiEvent {
        UiEvent::LibraryChanged {
            provider: "spotify".to_owned(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn first_check_stores_and_sends_nothing() {
        let (w, fake, mut rx) = rig();
        w.check_now().await;
        assert_eq!(fake.asks(), 1);
        assert!(events(&mut rx).is_empty());
        assert_eq!(w.lock().last, Some(base()));
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_sends_nothing() {
        let (w, _fake, mut rx) = rig();
        w.check_now().await;
        w.check_now().await;
        assert!(events(&mut rx).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn each_kind_of_change_sends_exactly_one_event_and_is_stored() {
        let changes = [
            // A playlist added, one removed, a snapshot id changed.
            fp(
                &[("a", "s1"), ("b", "s2"), ("c", "s3")],
                10,
                Some("2026-01-01"),
            ),
            fp(&[("a", "s1")], 10, Some("2026-01-01")),
            fp(&[("a", "s1"), ("b", "s9")], 10, Some("2026-01-01")),
            // The liked total, the newest liked song, a first liked song.
            fp(&[("a", "s1"), ("b", "s2")], 11, Some("2026-01-01")),
            fp(&[("a", "s1"), ("b", "s2")], 10, Some("2026-02-02")),
            fp(&[("a", "s1"), ("b", "s2")], 10, None),
        ];
        for fresh in changes {
            let (w, fake, mut rx) = rig();
            w.check_now().await;
            fake.push(Ok(fresh.clone()));
            w.check_now().await;
            assert_eq!(events(&mut rx), [changed()], "{fresh:?}");
            assert_eq!(w.lock().last, Some(fresh.clone()));
            // The same answer again is no change.
            fake.push(Ok(fresh));
            w.check_now().await;
            assert!(events(&mut rx).is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_check_keeps_the_old_fingerprint_and_sends_nothing() {
        let (w, fake, mut rx) = rig();
        w.check_now().await;
        for e in [
            webapi::Error::WebNotLoggedIn,
            webapi::Error::UnexpectedShape,
            webapi::Error::Network,
            webapi::Error::RateLimited {
                retry_after_secs: 5,
            },
        ] {
            fake.push(Err(e));
            w.check_now().await;
        }
        assert!(events(&mut rx).is_empty());
        assert_eq!(w.lock().last, Some(base()));
        // The old one is still the baseline: a change against it is seen.
        fake.push(Ok(fp(&[], 0, None)));
        w.check_now().await;
        assert_eq!(events(&mut rx), [changed()]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_focus_check_waits_thirty_seconds_but_a_forced_one_does_not() {
        let (w, fake, _rx) = rig();
        w.check_limited().await;
        w.check_limited().await;
        assert_eq!(fake.asks(), 1);
        advance(Duration::from_secs(29)).await;
        w.check_limited().await;
        assert_eq!(fake.asks(), 1);
        w.check_now().await;
        assert_eq!(fake.asks(), 2);
        // The forced one restarted the gap.
        advance(Duration::from_secs(29)).await;
        w.check_limited().await;
        assert_eq!(fake.asks(), 2);
        advance(Duration::from_secs(2)).await;
        w.check_limited().await;
        assert_eq!(fake.asks(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_check_still_counts_for_the_gap() {
        let (w, fake, _rx) = rig();
        fake.push(Err(webapi::Error::Network));
        w.check_limited().await;
        w.check_limited().await;
        assert_eq!(fake.asks(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn three_hints_within_two_seconds_make_one_check() {
        let (w, fake, _rx) = rig();
        hint(&w).await;
        advance(Duration::from_millis(500)).await;
        hint(&w).await;
        advance(Duration::from_millis(500)).await;
        hint(&w).await;
        // Still inside the window.
        advance(Duration::from_millis(900)).await;
        assert_eq!(fake.asks(), 0);
        advance(Duration::from_millis(200)).await;
        assert_eq!(fake.asks(), 1);
        // A hint after the window makes the next one, past the focus gap.
        hint(&w).await;
        advance(HINT_WINDOW + Duration::from_millis(1)).await;
        assert_eq!(fake.asks(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hint_checks_even_inside_the_focus_gap() {
        let (w, fake, _rx) = rig();
        w.check_limited().await;
        hint(&w).await;
        advance(HINT_WINDOW + Duration::from_millis(1)).await;
        assert_eq!(fake.asks(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_watch_ends_a_waiting_hint() {
        let (w, fake, _rx) = rig();
        hint(&w).await;
        drop(w);
        advance(HINT_WINDOW + Duration::from_millis(1)).await;
        assert_eq!(fake.asks(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_timer_checks_at_once_and_then_every_period() {
        let (w, fake, mut rx) = rig();
        w.spawn_timer(TIMER_PERIOD);
        tokio::task::yield_now().await;
        assert_eq!(fake.asks(), 1);
        advance(TIMER_PERIOD - Duration::from_secs(1)).await;
        assert_eq!(fake.asks(), 1);
        fake.push(Ok(fp(&[], 0, None)));
        advance(Duration::from_secs(1)).await;
        assert_eq!(fake.asks(), 2);
        assert_eq!(events(&mut rx), [changed()]);
        advance(TIMER_PERIOD).await;
        assert_eq!(fake.asks(), 3);
        // The timer ends with the watch.
        drop(w);
        advance(TIMER_PERIOD * 2).await;
        assert_eq!(fake.asks(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_login_change_clears_the_baseline() {
        let (w, fake, mut rx) = rig();
        w.check_now().await;
        w.reset();
        // Another account's library: a first check again, so nothing is announced.
        fake.push(Ok(fp(&[("z", "s")], 1, None)));
        w.check_now().await;
        assert!(events(&mut rx).is_empty());
        assert_eq!(w.lock().last, Some(fp(&[("z", "s")], 1, None)));
        // The gap is clear too: a focus check right after a login runs.
        w.reset();
        w.check_limited().await;
        assert_eq!(fake.asks(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_check_in_flight_across_a_login_change_is_discarded() {
        let (w, fake, mut rx) = rig();
        w.check_now().await;
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *fake.gate.lock().unwrap() = Some((entered.clone(), release.clone()));
        fake.push(Ok(fp(&[("old", "s")], 1, None)));
        let w2 = w.clone();
        let task = tokio::spawn(async move { w2.check_now().await });
        entered.notified().await;
        w.reset();
        release.notify_one();
        task.await.unwrap();
        // Neither stored nor announced.
        assert!(events(&mut rx).is_empty());
        assert_eq!(w.lock().last, None);
    }

    #[tokio::test(start_paused = true)]
    async fn at_most_one_check_runs_at_a_time() {
        let (w, fake, _rx) = rig();
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *fake.gate.lock().unwrap() = Some((entered.clone(), release.clone()));
        let w2 = w.clone();
        let first = tokio::spawn(async move { w2.check_limited().await });
        entered.notified().await;
        // A focus check meanwhile is skipped at once.
        w.check_limited().await;
        assert_eq!(fake.asks(), 1);
        // A forced one waits for the first, then runs.
        let w3 = w.clone();
        let forced = tokio::spawn(async move { w3.check_now().await });
        tokio::task::yield_now().await;
        assert_eq!(fake.asks(), 1);
        *fake.gate.lock().unwrap() = None;
        release.notify_one();
        first.await.unwrap();
        forced.await.unwrap();
        assert_eq!(fake.asks(), 2);
    }
}
