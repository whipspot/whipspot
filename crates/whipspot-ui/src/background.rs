//! The full-window cover background with its slow Ken Burns motion and theme C's colours from the
//! cover (MD74, MD48). The cover is the current track's, in its `Soft` variant (softly blurred:
//! the picture stays recognisable); each theme shades it its own way (`Backdrop.shade`).
//! Without a cover the glass background of the theme's soft lights stays.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use lune_ui::color::{Rgba, mix, premul};
use lune_ui::image::{Fit, ImageAlign};
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use whipspot_core::player::ArtUrgency;
use whipspot_core::provider::ImageRef;

use crate::art::{Art, ArtCache, ArtPull, Variant};
use crate::draw::{P, alpha, blob, strip_edge};
use crate::theme::{Look, Shade};

/// The cover is asked for at this width (provider sizes are picked with `ImageRef::pick`).
const COVER_PX: u32 = 640;
/// A new cover fades in over this long (seconds).
const FADE_SECS: f32 = 0.6;
/// The clock advances by at most this much per frame (a stalled app does not jump the picture).
const MAX_STEP: f32 = 0.1;

/// Zoom: the square's side is the window's larger side times `ZOOM ± ZOOM_SWING`, one period of
/// [`ZOOM_PERIOD`] seconds.
const ZOOM: f32 = 1.16;
const ZOOM_SWING: f32 = 0.05;
const ZOOM_PERIOD: f32 = 80.0;
/// The share of the slack the centre swings within, and its two periods and the second's phase.
const DRIFT: f32 = 0.8;
const DRIFT_X_PERIOD: f32 = 112.0;
const DRIFT_Y_PERIOD: f32 = 96.0;
const DRIFT_Y_PHASE: f32 = 1.1;

/// The turn of the warm theme's picture (radians) and the extra zoom that keeps the window
/// covered under it (cos + sin of the turn, rounded up).
const WARM_TURN: f32 = -0.018;
const WARM_COVER: f32 = 1.02;

/// A scrim gradient has one strip per this many px, at least 1 and at most [`MAX_SCRIM_STRIPS`].
const SCRIM_STEP: f32 = 3.0;
const MAX_SCRIM_STRIPS: usize = 128;

/// The picture's square at clock `t` (seconds of play): a square of side
/// `max(w, h) × (1.16 + 0.05 sin(2πt/80))` whose centre swings within 80 % of the slack round the
/// window's centre, so it always contains `win`.
pub(crate) fn ken_burns(t: f32, win: Rect) -> Rect {
    let side = win.w.max(win.h) * (ZOOM + ZOOM_SWING * (TAU * t / ZOOM_PERIOD).sin());
    let cx = win.x + win.w / 2.0 + DRIFT * (side - win.w) / 2.0 * (TAU * t / DRIFT_X_PERIOD).sin();
    let cy = win.y
        + win.h / 2.0
        + DRIFT * (side - win.h) / 2.0 * (TAU * t / DRIFT_Y_PERIOD + DRIFT_Y_PHASE).sin();
    Rect::new(cx - side / 2.0, cy - side / 2.0, side, side)
}

/// Which cover the background shows, the cross-fade from the one before and the motion clock.
///
/// It keeps URLs, never image ids: every frame each one is looked up in the [`ArtCache`], so a
/// released picture is never drawn. The previous track's cover stays only while the new one is
/// on its way; when the new one failed (or its copy was released) the glass shows (F1, MD74).
#[derive(Debug, Default)]
pub(crate) struct CoverBg {
    /// Seconds of play, advanced by [`CoverBg::tick`].
    clock: f32,
    last: Option<Duration>,
    /// The URL of the cover drawn now.
    shown: Option<String>,
    /// The URL of the cover before it, drawn under the new one until the fade ends.
    previous: Option<String>,
    fade_from: Duration,
}

impl CoverBg {
    /// Advances the motion clock to app time `now`, by at most [`MAX_STEP`]; only while `playing`.
    pub(crate) fn tick(&mut self, now: Duration, playing: bool) {
        let dt = self
            .last
            .map_or(0.0, |l| now.saturating_sub(l).as_secs_f32())
            .min(MAX_STEP);
        self.last = Some(now);
        if playing {
            self.clock += dt;
        }
    }

    pub(crate) fn clock(&self) -> f32 {
        self.clock
    }

    /// Asks for the cover of a track with `images` (every frame it is drawn, MD62) and returns the
    /// one to draw: the new cover once it is registered; the one before while the new one is
    /// pending and the old one is still registered; nothing when the track has no image, its
    /// cover failed or its copy was released.
    pub(crate) fn update(
        &mut self,
        art: &mut ArtCache,
        pull: &dyn ArtPull,
        images: Option<&[ImageRef]>,
        now: Duration,
    ) -> Option<Art> {
        let Some(wanted) = images.and_then(|i| ImageRef::pick(i, COVER_PX)) else {
            self.shown = None;
            self.previous = None;
            return None;
        };
        if let Some(got) = art.get(pull, wanted, Variant::Soft, ArtUrgency::Current, COVER_PX) {
            if self.shown.as_deref() != Some(wanted.url.as_str()) {
                self.previous = self.shown.replace(wanted.url.clone());
                self.fade_from = now;
            }
            return Some(got);
        }
        // The wanted cover is not registered: its own copy was released (it is asked again), or
        // it failed: nothing of another track is drawn for it.
        if self.shown.as_deref() == Some(wanted.url.as_str())
            || art.failed(&wanted.url, Variant::Soft)
        {
            self.shown = None;
            self.previous = None;
            return None;
        }
        // Pending: the cover before stays while it is still registered.
        let old = self
            .shown
            .as_deref()
            .and_then(|url| art.held(url, Variant::Soft));
        if old.is_none() {
            self.shown = None;
            self.previous = None;
        }
        old
    }

    /// The URL of the cover drawn now (the accent comes from it).
    pub(crate) fn shown_url(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    /// The cover under the new one while the fade runs and it is still registered, with the new
    /// one's progress (0 to 1).
    pub(crate) fn previous(&mut self, art: &mut ArtCache, now: Duration) -> Option<(Art, f32)> {
        let f = self.progress(now)?;
        let old = self
            .previous
            .as_deref()
            .and_then(|url| art.held(url, Variant::Soft));
        if old.is_none() {
            self.previous = None;
        }
        old.map(|a| (a, f))
    }

    /// The idle veil's base alpha (before the idle fade): the shown cover's [`idle_dim`], blended
    /// from the previous cover's over the cross-fade so it does not jump when the cover changes.
    /// `progress` is the one [`CoverBg::previous`] returned with the old cover (`None` when that
    /// copy is gone or no fade runs: the shown cover's alone).
    pub(crate) fn dim(&self, art: &ArtCache, progress: Option<f32>) -> f32 {
        let luma = |url: Option<&str>| url.and_then(|u| art.luma(u));
        blend_dim(
            luma(self.previous.as_deref()),
            luma(self.shown.as_deref()),
            progress,
        )
    }

    /// Whether a cross-fade is running (the view asks for frames meanwhile).
    pub(crate) fn fading(&self, now: Duration) -> bool {
        self.progress(now).is_some()
    }

    fn progress(&self, now: Duration) -> Option<f32> {
        self.previous.as_ref()?;
        let f = now.saturating_sub(self.fade_from).as_secs_f32() / FADE_SECS;
        (f < 1.0).then_some(f.clamp(0.0, 1.0))
    }
}

/// A fill above the layer's images, in colour `c` (straight alpha).
fn over(p: &mut P<'_>, r: Rect, c: Rgba) {
    p.fill_over(r, 0.0, premul(c));
}

/// A vertical gradient above the layer's images, in strips that share snapped edges.
fn over_v(p: &mut P<'_>, r: Rect, top: Rgba, bottom: Rgba) {
    let n = ((r.h / SCRIM_STEP).ceil() as usize).clamp(1, MAX_SCRIM_STRIPS);
    for i in 0..n {
        let t = (i as f32 + 0.5) / n as f32;
        let (y0, y1) = (
            strip_edge(p, r.y, r.h, i, n),
            strip_edge(p, r.y, r.h, i + 1, n),
        );
        if y1 > y0 {
            over(p, Rect::new(r.x, y0, r.w, y1 - y0), mix(top, bottom, t));
        }
    }
}

/// A horizontal gradient above the layer's images, in strips that share snapped edges.
fn over_h(p: &mut P<'_>, r: Rect, left: Rgba, right: Rgba) {
    let n = ((r.w / SCRIM_STEP).ceil() as usize).clamp(1, MAX_SCRIM_STRIPS);
    for i in 0..n {
        let t = (i as f32 + 0.5) / n as f32;
        let (x0, x1) = (
            strip_edge(p, r.x, r.w, i, n),
            strip_edge(p, r.x, r.w, i + 1, n),
        );
        if x1 > x0 {
            over(p, Rect::new(x0, r.y, x1 - x0, r.h), mix(left, right, t));
        }
    }
}

/// `r` grown by `k` about its centre.
fn grown(r: Rect, k: f32) -> Rect {
    let (w, h) = (r.w * k, r.h * k);
    Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h)
}

/// The cover's mean luminance from which the idle view starts to darken the background, and the
/// one at which the darkening is full.
const DIM_FROM: f32 = 0.32;
const DIM_TO: f32 = 0.72;
/// The most the idle view darkens the background on a bright cover (alpha, 0..1).
const DIM_MAX: f32 = 0.5;

/// How much darker the idle view makes the background (the alpha of a black veil over it) for a
/// cover of mean luminance `luma`: none for dark covers, up to [`DIM_MAX`] for near-white ones,
/// smooth in between, so the idle text keeps its contrast on any cover. Unknown luminance: none.
pub(crate) fn idle_dim(luma: Option<f32>) -> f32 {
    let Some(l) = luma else {
        return 0.0;
    };
    let t = ((l - DIM_FROM) / (DIM_TO - DIM_FROM)).clamp(0.0, 1.0);
    DIM_MAX * t * t * (3.0 - 2.0 * t)
}

/// The veil's alpha over a cross-fade at `progress` (0 the old cover, 1 the new): the dim of the
/// `previous` cover's luminance moved to the `shown` one's. No progress: the shown one's dim.
pub(crate) fn blend_dim(previous: Option<f32>, shown: Option<f32>, progress: Option<f32>) -> f32 {
    let to = idle_dim(shown);
    match progress {
        Some(f) => {
            let from = idle_dim(previous);
            from + (to - from) * f.clamp(0.0, 1.0)
        }
        None => to,
    }
}

/// The window background: with a `cover`, the ground, the soft picture at Ken Burns clock `t`
/// under `look`'s scrim, and the `previous` cover under it while `(cover, progress)` cross-fades;
/// without one, the glass background of the theme's soft lights.
pub(crate) fn background(
    p: &mut P<'_>,
    k: &Look,
    win: Rect,
    cover: Option<Art>,
    previous: Option<(Art, f32)>,
    t: f32,
    dim: f32,
) {
    p.on(LayerId::Base);
    let Some(cover) = cover else {
        glass_background(p, k, win);
        return;
    };
    let bd = k.theme.backdrop;
    let mut r = ken_burns(t, win);
    let (base, ground) = match bd.shade {
        Shade::CoverDeep => (0.88, k.deep),
        _ => (1.0, bd.ground),
    };
    // The deep shade lets the picture through: the old one thins out as the new one comes in.
    let (old_alpha, new_alpha) = match previous {
        Some((_, f)) if bd.shade == Shade::CoverDeep => (base * (1.0 - f), base * f),
        Some((_, f)) => (base, base * f),
        None => (base, base),
    };
    let turn = if bd.shade == Shade::Warm {
        r = grown(r, WARM_COVER);
        WARM_TURN
    } else {
        0.0
    };

    p.push_clip(win);
    p.fill(win, 0.0, ground);
    if let Some((old, _)) = previous {
        p.image_rotated(r, old.id, Fit::Contain, ImageAlign::CENTER, old_alpha, turn);
    }
    p.image_rotated(
        r,
        cover.id,
        Fit::Contain,
        ImageAlign::CENTER,
        new_alpha,
        turn,
    );
    p.pop_clip();

    match bd.shade {
        Shade::Neutral => {
            let black = [0x06, 0x07, 0x0A, 0xFF];
            over_v(p, win, alpha(black, 0x50), alpha(black, 0xB0));
        }
        Shade::Warm => {
            let pit = [0x06, 0x05, 0x04, 0xFF];
            over(p, win, [0x0C, 0x09, 0x07, 0x5C]);
            over(p, win, [0x3A, 0x1E, 0x08, 0x20]);
            let top = Rect::new(win.x, win.y, win.w, 200.0_f32.min(win.h));
            over_v(p, top, alpha(pit, 0x78), alpha(pit, 0));
            let bh = 420.0_f32.min(win.h);
            let bottom = Rect::new(win.x, win.bottom() - bh, win.w, bh);
            over_v(p, bottom, alpha(pit, 0), alpha(pit, 0xA0));
            let sw = 320.0_f32.min(win.w / 2.0);
            over_h(
                p,
                Rect::new(win.x, win.y, sw, win.h),
                alpha(pit, 0x50),
                alpha(pit, 0),
            );
            over_h(
                p,
                Rect::new(win.right() - sw, win.y, sw, win.h),
                alpha(pit, 0),
                alpha(pit, 0x50),
            );
        }
        Shade::CoverDeep => over_v(p, win, alpha(k.deep, 0x4A), alpha(k.deep, 0xB4)),
    }
    // The idle view's extra veil on a bright cover.
    if dim > 0.002 {
        over(p, win, [0x05, 0x05, 0x08, (dim * 255.0).round() as u8]);
    }
}

/// The background without a cover: the theme's ground, its soft lights, an acrylic grain and a
/// vertical darkening toward the bottom.
fn glass_background(p: &mut P<'_>, k: &Look, win: Rect) {
    let bd = k.theme.backdrop;
    p.fill(win, 0.0, bd.ground);
    for (c, radius, color, strength) in bd.lights {
        blob(p, c, radius, color, strength);
    }
    p.frost(win, 0.0, premul(alpha(bd.ground, 0x28)), true);
    let (top, bottom) = ([0x04, 0x04, 0x07, 0x20], [0x04, 0x04, 0x07, 0x90]);
    p.linear(win, 0.0, top, bottom, FRAC_PI_2);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lune_ui::testing::FakeImages;
    use whipspot_core::art::ArtImage;
    use whipspot_core::player::ArtStatus;

    use super::*;

    const WINDOWS: [(f32, f32); 3] = [(1440.0, 900.0), (760.0, 480.0), (2560.0, 1080.0)];

    fn times() -> impl Iterator<Item = f32> {
        (0..=2400).map(|i| i as f32 * 0.25)
    }

    #[test]
    fn idle_dim_follows_the_covers_brightness() {
        assert_eq!(idle_dim(None), 0.0);
        assert_eq!(idle_dim(Some(0.1)), 0.0, "a dark cover is left alone");
        assert_eq!(idle_dim(Some(DIM_FROM)), 0.0);
        assert!((idle_dim(Some(1.0)) - DIM_MAX).abs() < 1e-6);
        let mut last = 0.0;
        for i in 0..=100 {
            let d = idle_dim(Some(i as f32 / 100.0));
            assert!(d >= last - 1e-6 && d <= DIM_MAX + 1e-6, "{i}: {d}");
            last = d;
        }
    }

    #[test]
    fn the_idle_veil_follows_the_cross_fade() {
        let (dark, bright) = (Some(0.1), Some(1.0));
        assert_eq!(
            blend_dim(dark, bright, Some(0.0)),
            0.0,
            "start: the old dim"
        );
        assert!((blend_dim(dark, bright, Some(1.0)) - DIM_MAX).abs() < 1e-6);
        assert!((blend_dim(dark, bright, Some(0.5)) - DIM_MAX / 2.0).abs() < 1e-6);
        // Bright to dark is the mirror: the veil lifts over the fade, not in one frame.
        assert!((blend_dim(bright, dark, Some(0.0)) - DIM_MAX).abs() < 1e-6);
        assert!((blend_dim(bright, dark, Some(0.5)) - DIM_MAX / 2.0).abs() < 1e-6);
        // No fade (or the old copy released): the shown cover's dim; unknown luma is none.
        assert!((blend_dim(dark, bright, None) - DIM_MAX).abs() < 1e-6);
        assert_eq!(blend_dim(bright, None, Some(1.0)), 0.0);
        assert_eq!(blend_dim(None, None, None), 0.0);
    }

    #[test]
    fn kb1_the_square_always_contains_the_window() {
        for (w, h) in WINDOWS {
            let win = Rect::new(0.0, 0.0, w, h);
            for t in times() {
                let r = ken_burns(t, win);
                assert!(
                    r.x <= win.x + 1e-3
                        && r.y <= win.y + 1e-3
                        && r.right() >= win.right() - 1e-3
                        && r.bottom() >= win.bottom() - 1e-3,
                    "{w}x{h} t={t}: {r:?}"
                );
            }
        }
    }

    #[test]
    fn kb2_the_square_moves_smoothly() {
        // The constants (80 s zoom, 112 s and 96 s drift; MD74 as slowed by v1 feedback round 2)
        // give a peak speed of about 21 px/s at 1440x900 and 54 px/s at 2560x1080 (the slack grows
        // with the window), so a 1/30 s step is at most about 2 px.
        for (w, h) in WINDOWS {
            let win = Rect::new(0.0, 0.0, w, h);
            let step = 1.0 / 30.0;
            let mut worst = 0.0_f32;
            let mut t = 0.0;
            while t < 600.0 {
                let (a, b) = (ken_burns(t, win), ken_burns(t + step, win));
                for d in [
                    b.x - a.x,
                    b.y - a.y,
                    b.right() - a.right(),
                    b.bottom() - a.bottom(),
                ] {
                    worst = worst.max(d.abs());
                }
                t += step;
            }
            assert!(worst < 2.5, "{w}x{h}: {worst} px per 1/30 s");
        }
    }

    #[test]
    fn kb3_the_side_stays_within_its_band() {
        for (w, h) in WINDOWS {
            let win = Rect::new(0.0, 0.0, w, h);
            let big = w.max(h);
            for t in times() {
                let side = ken_burns(t, win).w;
                assert!(
                    (1.11 * big - 1e-2..=1.21 * big + 1e-2).contains(&side),
                    "{w}x{h} t={t}: {side}"
                );
            }
        }
    }

    #[test]
    fn the_clock_runs_only_while_playing_and_in_small_steps() {
        let mut bg = CoverBg::default();
        let s = Duration::from_secs;
        bg.tick(s(10), true);
        assert_eq!(bg.clock(), 0.0, "the first frame has no step");
        bg.tick(s(10) + Duration::from_millis(33), true);
        assert!((bg.clock() - 0.033).abs() < 1e-4);
        bg.tick(s(20), true);
        assert!((bg.clock() - 0.133).abs() < 1e-4, "a stall counts 0.1 s");
        bg.tick(s(21), false);
        bg.tick(s(22), false);
        assert!((bg.clock() - 0.133).abs() < 1e-4, "paused: no motion");
    }

    /// A pull answering per URL: `Ready` (a small picture), `Failed`, or `Pending` for the rest.
    struct Pull {
        ready: Vec<&'static str>,
        failed: Vec<&'static str>,
    }

    impl ArtPull for Pull {
        fn art(&self, image: &ImageRef, _urgency: ArtUrgency) -> ArtStatus {
            if self.ready.contains(&image.url.as_str()) {
                let img = ArtImage::new(4, 4, vec![120; 64]).unwrap();
                ArtStatus::Ready(Arc::new(img))
            } else if self.failed.contains(&image.url.as_str()) {
                ArtStatus::Failed
            } else {
                ArtStatus::Pending
            }
        }
    }

    fn images(url: &str) -> Vec<ImageRef> {
        vec![ImageRef {
            url: url.into(),
            width: Some(640),
            height: Some(640),
        }]
    }

    /// One frame at `ms`: the host's `sync`, then the view's `update`.
    fn step(
        bg: &mut CoverBg,
        art: &mut ArtCache,
        store: &mut FakeImages,
        pull: &Pull,
        url: &str,
        ms: u64,
    ) -> Option<Art> {
        let now = Duration::from_millis(ms);
        art.sync(store);
        art.note_time(now);
        bg.update(art, pull, Some(&images(url)), now)
    }

    /// The background showing cover "a" (registered) at 1 s.
    fn showing_a(pull: &Pull) -> (CoverBg, ArtCache, FakeImages) {
        let (mut bg, mut art, mut store) = (CoverBg::default(), ArtCache::new(), FakeImages::new());
        assert_eq!(step(&mut bg, &mut art, &mut store, pull, "a", 0), None);
        let a = step(&mut bg, &mut art, &mut store, pull, "a", 1000).expect("a registered");
        assert!(store.contains(a.id));
        assert_eq!(bg.shown_url(), Some("a"));
        (bg, art, store)
    }

    #[test]
    fn a_new_cover_fades_in_over_the_old_one() {
        let pull = Pull {
            ready: vec!["a", "b"],
            failed: vec![],
        };
        let (mut bg, mut art, mut store) = showing_a(&pull);
        let a = art.held("a", Variant::Soft).unwrap();
        assert_eq!(
            step(&mut bg, &mut art, &mut store, &pull, "b", 2000),
            Some(a)
        );
        let b = step(&mut bg, &mut art, &mut store, &pull, "b", 3000).expect("b registered");
        assert_ne!(a.id, b.id);
        assert_eq!(bg.shown_url(), Some("b"));
        let at = Duration::from_secs(3);
        assert_eq!(bg.previous(&mut art, at), Some((a, 0.0)));
        let mid = bg
            .previous(&mut art, at + Duration::from_millis(300))
            .unwrap()
            .1;
        assert!((mid - 0.5).abs() < 1e-3);
        assert!(
            bg.previous(&mut art, at + Duration::from_millis(600))
                .is_none()
        );
        assert!(!bg.fading(at + Duration::from_secs(1)));
    }

    // F1: the previous track's cover stays only while the new one is pending.
    #[test]
    fn a_failed_new_cover_shows_the_glass_not_the_old_cover() {
        let pull = Pull {
            ready: vec!["a"],
            failed: vec!["b"],
        };
        let (mut bg, mut art, mut store) = showing_a(&pull);
        assert_eq!(step(&mut bg, &mut art, &mut store, &pull, "b", 2000), None);
        assert_eq!(bg.shown_url(), None, "no accent from the old cover");
        assert!(bg.previous(&mut art, Duration::from_secs(2)).is_none());
        // Still the glass while the failure holds, and after the old cover was released (`sync`
        // runs before the frame, at the time of the frame before).
        for ms in [3000, 60_000, 200_000, 201_000] {
            assert_eq!(step(&mut bg, &mut art, &mut store, &pull, "b", ms), None);
        }
        assert_eq!(
            store.len(),
            0,
            "the old cover was released, nothing refers to it"
        );
    }

    #[test]
    fn a_pending_new_cover_keeps_the_old_one_until_it_fails() {
        let mut pull = Pull {
            ready: vec!["a"],
            failed: vec![],
        };
        let (mut bg, mut art, mut store) = showing_a(&pull);
        let a = art.held("a", Variant::Soft).unwrap();
        // Long past UNDRAWN_FOR: the old cover is drawn, so it stays registered.
        for ms in [2000, 60_000, 130_000, 250_000] {
            assert_eq!(step(&mut bg, &mut art, &mut store, &pull, "b", ms), Some(a));
            assert!(store.contains(a.id));
        }
        pull.failed.push("b");
        // The core's want for b was renewed every frame; its failure ends the wait.
        assert_eq!(
            step(&mut bg, &mut art, &mut store, &pull, "b", 251_000),
            None
        );
        assert_eq!(bg.shown_url(), None);
    }

    #[test]
    fn a_track_without_images_shows_the_glass() {
        let pull = Pull {
            ready: vec!["a"],
            failed: vec![],
        };
        let (mut bg, mut art, _store) = showing_a(&pull);
        art.note_time(Duration::from_secs(2));
        assert_eq!(
            bg.update(&mut art, &pull, Some(&[]), Duration::from_secs(2)),
            None
        );
        assert_eq!(bg.shown_url(), None);
    }
}
