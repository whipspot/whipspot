//! Short eased transitions where the view would otherwise snap (v1 round 3): a page change, a
//! tooltip, a new track, a cover arriving, the progress ring starting over.
//!
//! Everything here is closed-form in the frame's time: a start instant and `now` give the value, so
//! no state is stepped per frame and a transition costs a repaint request only while it runs. The
//! easings are the toolkit's (`lune_ui::motion::Easing`), the durations its motion scale's tokens
//! (fast 180 ms, base 250 ms, moderate 350 ms) or close to them.
//!
//! The motion tier (`ui.tier()`) is honoured through [`set_pace`], which the shell calls once a
//! frame: `Off` and the demo's forced views settle at once, `Reduced` keeps the fades and drops the
//! slides.

use std::cell::Cell;
use std::time::Duration;

use lune_ui::motion::{Easing, Kind, Motion, MotionTier};
use lune_ui::paint::Xform;
use lune_ui::ui::Ui;

/// A page change in the browse sheet: a fade and a short slide.
pub(crate) const PAGE: Duration = Duration::from_millis(200);
/// How far a page slides up into place, logical px (Full tier only).
pub(crate) const PAGE_SLIDE: f32 = 6.0;
/// A tooltip card appearing.
pub(crate) const TIP: Duration = Duration::from_millis(140);
/// The now-playing title and cover coming in with a new track.
pub(crate) const TRACK: Duration = Duration::from_millis(280);
/// A cover arriving from the network, from the placeholder.
pub(crate) const ART: Duration = Duration::from_millis(250);
/// The progress ring sweeping back when a new track starts.
pub(crate) const SWEEP: Duration = Duration::from_millis(380);
/// A progress ring that had not got this far is not swept back.
const SWEEP_MIN: f32 = 0.02;

/// How much motion the view plays now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pace {
    /// Fades and slides.
    Full,
    /// Fades only.
    Fades,
    /// Everything settles at once (tier Off, and the demo's dumps).
    Still,
}

thread_local! {
    static PACE: Cell<Pace> = const { Cell::new(Pace::Full) };
}

/// Sets the pace of this frame's transitions (the shell does, from the tier and the demo's
/// forced state).
pub(crate) fn set_pace(pace: Pace) {
    PACE.with(|p| p.set(pace));
}

/// The pace of this frame's transitions.
pub(crate) fn pace() -> Pace {
    PACE.with(Cell::get)
}

/// The pace the toolkit's motion tier asks for; `forced` (the demo) is always still.
pub(crate) fn pace_of(tier: MotionTier, forced: bool) -> Pace {
    match (tier, forced) {
        (_, true) | (MotionTier::Off, _) => Pace::Still,
        (MotionTier::Reduced, _) => Pace::Fades,
        (MotionTier::Full, _) => Pace::Full,
    }
}

/// The share (0..=1, linear) of `dur` that has passed between `since` and `now`; 1 for a zero `dur`.
pub(crate) fn progress(since: Duration, now: Duration, dur: Duration) -> f32 {
    if dur.is_zero() {
        return 1.0;
    }
    (now.saturating_sub(since).as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
}

/// The eased (quad-out) share of an enter transition begun at `since`: 1 when there is none or
/// the frame's pace is still.
pub(crate) fn enter(since: Option<Duration>, now: Duration, dur: Duration) -> f32 {
    match since {
        Some(s) if pace() != Pace::Still => Easing::QuadOut.eval(progress(s, now, dur)),
        _ => 1.0,
    }
}

/// Whether an enter transition begun at `since` still runs at `now` (the view asks for frames
/// while it does).
pub(crate) fn running(since: Option<Duration>, now: Duration, dur: Duration) -> bool {
    pace() != Pace::Still && since.is_some_and(|s| now < s + dur)
}

/// The transform of a page at enter share `e`: faded to `e`, and (Full tier) `PAGE_SLIDE` px
/// lower at the start.
pub(crate) fn page_xform(e: f32) -> Xform {
    let slide = if pace() == Pace::Full {
        PAGE_SLIDE * (1.0 - e)
    } else {
        0.0
    };
    Xform {
        origin: (0.0, 0.0),
        scale: 1.0,
        dx: 0.0,
        dy: slide,
        alpha: e.clamp(0.0, 1.0),
    }
}

/// Runs `draw` under the page transform of enter share `e` (none at rest).
pub(crate) fn page<T>(ui: &mut Ui<'_>, e: f32, draw: impl FnOnce(&mut Ui<'_>) -> T) -> T {
    if e >= 1.0 {
        return draw(ui);
    }
    ui.painter().push_xform(page_xform(e));
    let out = draw(ui);
    ui.painter().pop_xform();
    out
}

/// `target` moved to by the toolkit's `move` transition (fast, sine-in-out) under the widget
/// `key`: the first call returns `target`, a changed target glides there. A still pace snaps.
pub(crate) fn glide(ui: &mut Ui<'_>, key: &'static str, target: f32) -> f32 {
    let id = ui.id(key);
    if pace() == Pace::Still {
        ui.snap_f32(id, target)
    } else {
        ui.animate_f32(id, target, &Motion::of(Kind::Move))
    }
}

/// A card that appears when something starts being shown (a tooltip): it fades in over [`TIP`]
/// and is gone the frame it stops being shown.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Reveal {
    since: Option<Duration>,
}

impl Reveal {
    /// The card's opacity this frame: 0 while not `shown`; while shown, the eased share since it
    /// began.
    pub fn update(&mut self, shown: bool, now: Duration) -> f32 {
        if !shown {
            self.since = None;
            return 0.0;
        }
        let since = *self.since.get_or_insert(now);
        enter(Some(since), now, TIP)
    }

    /// Whether the fade-in still runs at `now`.
    pub fn running(&self, now: Duration) -> bool {
        running(self.since, now, TIP)
    }
}

/// Which track is current, and since when it changed: the first observation is not a change.
#[derive(Debug, Default)]
pub(crate) struct TrackWatch {
    seen: bool,
    uri: Option<String>,
    since: Option<Duration>,
}

impl TrackWatch {
    /// Records `uri` (the current track's, if any) at `now`; whether it differs from the last
    /// call's. The first call only records.
    pub fn update(&mut self, uri: Option<&str>, now: Duration) -> bool {
        if !self.seen {
            self.seen = true;
            self.uri = uri.map(str::to_owned);
            return false;
        }
        if self.uri.as_deref() == uri {
            return false;
        }
        self.uri = uri.map(str::to_owned);
        self.since = Some(now);
        true
    }

    /// When the track last changed (`None` before any change).
    pub fn since(&self) -> Option<Duration> {
        self.since
    }
}

/// The ring's sweep back to the start of a new track: from the value it showed to the live one,
/// eased in and out over [`SWEEP`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sweep {
    from: f32,
    since: Duration,
}

impl Sweep {
    /// A sweep from `from` begun at `since`; `None` when `from` is too small to see (or the pace
    /// is still).
    pub fn new(from: f32, since: Duration) -> Option<Sweep> {
        (pace() != Pace::Still && from > SWEEP_MIN).then_some(Sweep { from, since })
    }

    /// The value to show at `now` when the live one is `target`, or `None` once the sweep is
    /// over.
    pub fn value(&self, target: f32, now: Duration) -> Option<f32> {
        let t = progress(self.since, now, SWEEP);
        if t >= 1.0 {
            return None;
        }
        Some(self.from + (target - self.from) * Easing::SineInOut.eval(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The tests read the thread's pace, so each one sets it first.
    fn full() {
        set_pace(Pace::Full);
    }

    #[test]
    fn a1_enter_runs_from_nothing_to_all_over_its_duration_and_never_back() {
        full();
        assert_eq!(enter(Some(ms(1000)), ms(1000), PAGE), 0.0);
        assert_eq!(enter(Some(ms(1000)), ms(1000) + PAGE, PAGE), 1.0);
        assert_eq!(enter(Some(ms(1000)), ms(9000), PAGE), 1.0);
        // Before the start (a clock that moved back) is still the start.
        assert_eq!(enter(Some(ms(1000)), ms(500), PAGE), 0.0);
        // Quad-out: more than half done at half the time, rising all the way.
        assert!(enter(Some(ms(0)), PAGE / 2, PAGE) > 0.7);
        let mut last = 0.0;
        for t in (0..=200).step_by(4) {
            let v = enter(Some(ms(0)), ms(t), PAGE);
            assert!(v >= last, "{t}");
            last = v;
        }
        // No transition at all is rest.
        assert_eq!(enter(None, ms(5), PAGE), 1.0);
        assert_eq!(progress(ms(0), ms(10), Duration::ZERO), 1.0);
    }

    #[test]
    fn a2_a_still_pace_settles_at_once_and_asks_for_no_frames() {
        set_pace(Pace::Still);
        assert_eq!(enter(Some(ms(0)), ms(1), PAGE), 1.0);
        assert!(!running(Some(ms(0)), ms(1), PAGE));
        assert_eq!(Sweep::new(0.8, ms(0)), None);
        full();
        assert!(running(Some(ms(0)), ms(1), PAGE));
        assert!(!running(Some(ms(0)), PAGE, PAGE));
        assert!(!running(None, ms(0), PAGE));
    }

    #[test]
    fn a3_the_tier_and_the_demo_choose_the_pace() {
        assert_eq!(pace_of(MotionTier::Full, false), Pace::Full);
        assert_eq!(pace_of(MotionTier::Reduced, false), Pace::Fades);
        assert_eq!(pace_of(MotionTier::Off, false), Pace::Still);
        assert_eq!(pace_of(MotionTier::Full, true), Pace::Still);
    }

    #[test]
    fn a4_a_page_fades_and_slides_in_full_and_only_fades_when_reduced() {
        full();
        let start = page_xform(0.0);
        assert_eq!((start.alpha, start.dy), (0.0, PAGE_SLIDE));
        let end = page_xform(1.0);
        assert_eq!((end.alpha, end.dy), (1.0, 0.0));
        let mid = page_xform(0.5);
        assert!(mid.alpha == 0.5 && mid.dy == PAGE_SLIDE / 2.0);
        set_pace(Pace::Fades);
        assert_eq!(page_xform(0.0).dy, 0.0);
        assert_eq!(page_xform(0.3).alpha, 0.3);
        full();
    }

    #[test]
    fn a5_a_reveal_starts_when_shown_and_resets_when_not() {
        full();
        let mut r = Reveal::default();
        assert_eq!(r.update(false, ms(0)), 0.0);
        assert_eq!(r.update(true, ms(100)), 0.0);
        assert!(r.running(ms(100)));
        let mid = r.update(true, ms(100) + TIP / 2);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(r.update(true, ms(100) + TIP), 1.0);
        assert!(!r.running(ms(100) + TIP));
        // Hidden: gone at once, and a later show starts from nothing again.
        assert_eq!(r.update(false, ms(400)), 0.0);
        assert_eq!(r.update(true, ms(500)), 0.0);
    }

    #[test]
    fn a6_the_track_watch_ignores_the_first_sight_and_marks_each_change() {
        let mut w = TrackWatch::default();
        assert!(!w.update(None, ms(0)), "first sight");
        assert_eq!(w.since(), None);
        assert!(!w.update(None, ms(10)), "still nothing");
        assert!(w.update(Some("a"), ms(20)), "playback starts");
        assert_eq!(w.since(), Some(ms(20)));
        assert!(!w.update(Some("a"), ms(30)));
        assert_eq!(w.since(), Some(ms(20)));
        assert!(w.update(Some("b"), ms(40)), "a new track");
        assert!(w.update(None, ms(50)), "playback ends");
        // A watch whose first sight is a track does not call it a change.
        let mut w = TrackWatch::default();
        assert!(!w.update(Some("a"), ms(0)));
        assert_eq!(w.since(), None);
    }

    #[test]
    fn a7_the_sweep_goes_from_where_it_was_to_the_live_value_and_ends() {
        full();
        let s = Sweep::new(0.8, ms(1000)).unwrap();
        // At the start it still shows the old value, at the end it is gone.
        assert_eq!(s.value(0.0, ms(1000)), Some(0.8));
        assert_eq!(s.value(0.0, ms(1000) + SWEEP), None);
        // In between it falls monotonically towards the live value.
        let mut last = 0.8;
        for t in (0..380).step_by(10) {
            let v = s.value(0.0, ms(1000 + t)).unwrap();
            assert!(v <= last + 1e-6 && (0.0..=0.8).contains(&v), "{t}: {v}");
            last = v;
        }
        // It follows a live value that moves meanwhile.
        let late = s.value(0.01, ms(1000) + SWEEP - ms(1)).unwrap();
        assert!((late - 0.01).abs() < 0.01, "{late}");
        // A ring that had barely started is not swept.
        assert_eq!(Sweep::new(0.01, ms(0)), None);
        assert_eq!(Sweep::new(0.0, ms(0)), None);
        assert!((300..=450).contains(&(SWEEP.as_millis() as u64)));
    }
}
