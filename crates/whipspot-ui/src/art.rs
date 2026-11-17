//! The UI's art cache (MD73, C4 (b)): what the core hands out once (C1 `art`) is registered with
//! the host's image store, named by URL and [`Variant`], drawn by [`cover`] and released when it
//! is no longer drawn.
//!
//! The view asks on every frame a URL is drawn (`get`); while the core's answer is `Pending` each
//! ask renews the core's want (MD62). A `Ready` image is queued and registered by `sync`, which
//! the host calls before the frame with its image store, so the frame after the answer shows it.
//! `Plain` is the picture itself, halved while it is far larger than its slot (the toolkit keeps
//! no mipmaps); `Soft` is the background's blurred, small copy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lune_ui::image::{Fit, ImageAlign, ImageData, ImageId, ImageStore};
use lune_ui::paint::Rect;
use whipspot_core::art::ArtImage;
use whipspot_core::player::{ArtStatus, ArtUrgency, PlayerHandle};
use whipspot_core::provider::ImageRef;

use crate::anim;
use crate::draw::{P, alpha};
use crate::icons::{Icon, icon};
use crate::palette::{CoverAccent, cover_accent, cover_luma};
use crate::theme::Look;

/// A failed URL (or a refused registration) is not asked for again for this long.
const FAILED_FOR: Duration = Duration::from_secs(60);
/// A registered image not drawn for this long is released.
const UNDRAWN_FOR: Duration = Duration::from_secs(120);
/// Registered pictures are kept under this many bytes (w·h·4 each); the least recently drawn go
/// first.
const BUDGET: usize = 128 * 1024 * 1024;
/// A `Soft` picture is halved to at most this many pixels on its larger side before blurring (the
/// background's cover is asked for at 640 px: it keeps that size).
const SOFT_SIDE: u32 = 640;
/// The soft copy's box blur: radius and passes. Moderate on purpose (a sigma of about 4.5 px at
/// 640 px): the cover stays recognisable behind the interface but loses its fine detail. v1
/// feedback: radius 4 x 3 passes at 320 px (sigma about 9 px at 640) left only a colour wash,
/// radius 2 x 2 (sigma 2 px) was too sharp; this sits between them.
const BLUR_RADIUS: usize = 4;
const BLUR_PASSES: usize = 3;
/// The CPU copies of the current covers kept so a second variant needs no new ask.
const KEPT: usize = 2;

/// A registered picture: the id to draw and its size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Art {
    pub id: ImageId,
    pub width: u32,
    pub height: u32,
    /// How far the picture has faded in since it was registered (0..=1; 1 at rest): a cover
    /// arriving from the network comes in from the placeholder.
    pub alpha: f32,
}

/// Which copy of a cover is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// The picture, halved while it is over twice the slot it was asked for.
    Plain,
    /// Halved to at most 640 px and softly blurred, for the background.
    Soft,
}

impl Variant {
    fn index(self) -> usize {
        match self {
            Variant::Plain => 0,
            Variant::Soft => 1,
        }
    }
}

/// Where the cache asks for pictures: the player's C1 `art`.
pub trait ArtPull {
    fn art(&self, image: &ImageRef, urgency: ArtUrgency) -> ArtStatus;
}

impl ArtPull for PlayerHandle {
    fn art(&self, image: &ImageRef, urgency: ArtUrgency) -> ArtStatus {
        PlayerHandle::art(self, image, urgency)
    }
}

/// One copy's state.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Empty,
    /// Asked in frame `frame`, the core had not got it yet.
    Pending {
        frame: u64,
    },
    /// The pixels wait for the next `sync`.
    Queued,
    Ready {
        art: Art,
        bytes: usize,
        drawn: Duration,
        /// When the picture was registered (a larger copy keeps its predecessor's).
        since: Duration,
        /// The larger side of the picture it was made from.
        source: u32,
        /// A larger copy asked for because a slot is wider than this one (F2).
        upgrade: Upgrade,
    },
    Failed {
        until: Duration,
    },
}

/// A larger copy of a registered `Plain` picture that was halved below a slot it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Upgrade {
    None,
    /// Asked in frame `frame`, the core had not got it yet.
    Pending {
        frame: u64,
    },
    /// The ask failed (or the store refused it): the smaller copy stays until then.
    Failed {
        until: Duration,
    },
}

/// Everything known of one URL.
#[derive(Debug)]
struct Entry {
    slots: [Slot; 2],
    accent: Option<CoverAccent>,
    accent_done: bool,
    /// The cover's mean luminance, taken with the accent.
    luma: Option<f32>,
    /// The last time this URL was asked for.
    last: Duration,
}

impl Entry {
    fn new(now: Duration) -> Self {
        Self {
            slots: [Slot::Empty; 2],
            accent: None,
            accent_done: false,
            luma: None,
            last: now,
        }
    }

    fn holds_gpu(&self) -> bool {
        self.slots
            .iter()
            .any(|s| matches!(s, Slot::Ready { .. } | Slot::Queued))
    }
}

/// Pixels waiting to be registered.
struct Upload {
    url: String,
    variant: Variant,
    phys_px: u32,
    image: Arc<ArtImage>,
}

/// The cache. One per window view; the frame calls `note_time` and `get`, the host calls `sync`.
pub struct ArtCache {
    entries: HashMap<String, Entry>,
    uploads: Vec<Upload>,
    /// The CPU copies of the last [`KEPT`] current covers, newest last.
    kept: Vec<(String, Arc<ArtImage>)>,
    now: Duration,
    frame: u64,
    next_id: u64,
    budget: usize,
    /// A picture drawn in this frame is still fading in.
    fading: bool,
}

impl std::fmt::Debug for ArtCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtCache")
            .field("entries", &self.entries.len())
            .field("uploads", &self.uploads.len())
            .finish()
    }
}

impl Default for ArtCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            uploads: Vec::new(),
            kept: Vec::new(),
            now: Duration::ZERO,
            frame: 0,
            next_id: 1,
            budget: BUDGET,
            fading: false,
        }
    }

    #[cfg(test)]
    fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            ..Self::new()
        }
    }

    /// Starts a frame at app time `t` (`ui.time()`).
    pub fn note_time(&mut self, t: Duration) {
        self.now = t;
        self.frame += 1;
        self.fading = false;
    }

    /// Whether a picture drawn since the last [`ArtCache::note_time`] is still fading in: the
    /// view asks for another frame.
    pub fn fading(&self) -> bool {
        self.fading
    }

    /// The picture for `image` in `variant`, drawn about `phys_px` physical pixels wide, if it is
    /// registered; otherwise `None` (the caller draws the placeholder) after asking `pull`.
    ///
    /// A `Plain` copy is never smaller than the largest slot that asks for it while the source
    /// allows (F2): asks of one frame raise the queued copy's size, and a registered copy halved
    /// below a later, wider ask is drawn until a larger one (asked for again) replaces it.
    pub fn get(
        &mut self,
        pull: &dyn ArtPull,
        image: &ImageRef,
        variant: Variant,
        urgency: ArtUrgency,
        phys_px: u32,
    ) -> Option<Art> {
        let now = self.now;
        let vi = variant.index();
        if !self.entries.contains_key(&image.url) {
            self.entries.insert(image.url.clone(), Entry::new(now));
        }
        let entry = self.entries.get_mut(&image.url)?;
        entry.last = now;
        // The registered copy drawn while a larger one is on its way.
        let mut shown = None;
        match &mut entry.slots[vi] {
            Slot::Ready {
                art,
                drawn,
                since,
                source,
                upgrade,
                ..
            } => {
                *drawn = now;
                let faded = fade_in(*art, *since, now, &mut self.fading);
                let side = art.width.max(art.height);
                let short = variant == Variant::Plain && side < phys_px && *source > side;
                let waiting = matches!(*upgrade, Upgrade::Failed { until } if now < until);
                if !short || waiting || raise(&mut self.uploads, &image.url, variant, phys_px) {
                    return Some(faded);
                }
                shown = Some(faded);
            }
            Slot::Queued => {
                raise(&mut self.uploads, &image.url, variant, phys_px);
                return None;
            }
            Slot::Failed { until } if now < *until => return None,
            _ => {}
        }
        let (decoded, keep) =
            if let Some((_, kept)) = self.kept.iter().find(|(u, _)| *u == image.url) {
                (Some(kept.clone()), false)
            } else {
                match pull.art(image, urgency) {
                    ArtStatus::Ready(img) => (Some(img), urgency == ArtUrgency::Current),
                    ArtStatus::Pending => {
                        set_waiting(&mut entry.slots[vi], Upgrade::Pending { frame: self.frame });
                        (None, false)
                    }
                    ArtStatus::Failed => {
                        set_waiting(
                            &mut entry.slots[vi],
                            Upgrade::Failed {
                                until: now + FAILED_FOR,
                            },
                        );
                        (None, false)
                    }
                }
            };
        if let Some(img) = decoded {
            if !entry.accent_done {
                entry.accent_done = true;
                entry.accent = cover_accent(img.rgba(), img.width(), img.height());
                entry.luma = cover_luma(img.rgba(), img.width(), img.height());
            }
            // A registered copy stays drawn until the larger one replaces it at `sync`.
            if let Slot::Ready { upgrade, .. } = &mut entry.slots[vi] {
                *upgrade = Upgrade::None;
            } else {
                entry.slots[vi] = Slot::Queued;
            }
            if keep {
                self.kept.retain(|(u, _)| *u != image.url);
                self.kept.push((image.url.clone(), img.clone()));
                if self.kept.len() > KEPT {
                    self.kept.remove(0);
                }
            }
            self.uploads.push(Upload {
                url: image.url.clone(),
                variant,
                phys_px,
                image: img,
            });
        }
        shown
    }

    /// Whether the cover at `url` in `variant` failed (the core's answer or the store's) and is
    /// not asked for again yet: the caller stops waiting for it.
    pub fn failed(&self, url: &str, variant: Variant) -> bool {
        self.entries.get(url).is_some_and(
            |e| matches!(e.slots[variant.index()], Slot::Failed { until } if self.now < until),
        )
    }

    /// The registered copy of `url` in `variant`, marked drawn, without asking for it; `None`
    /// when it is not registered (any longer).
    pub fn held(&mut self, url: &str, variant: Variant) -> Option<Art> {
        let now = self.now;
        match &mut self.entries.get_mut(url)?.slots[variant.index()] {
            Slot::Ready {
                art, drawn, since, ..
            } => {
                *drawn = now;
                Some(fade_in(*art, *since, now, &mut self.fading))
            }
            _ => None,
        }
    }

    /// The two accents of the cover at `url`, computed when its pixels arrived; `None` while the
    /// cover is unknown, near-greyscale or no longer held.
    pub fn accent(&self, url: &str) -> Option<CoverAccent> {
        self.entries.get(url)?.accent
    }

    /// The mean luminance (0..1) of the cover at `url`, computed with its accents; `None` while
    /// its pixels have not arrived.
    pub fn luma(&self, url: &str) -> Option<f32> {
        self.entries.get(url)?.luma
    }

    /// Whether pixels wait for the next [`ArtCache::sync`]: the view asks for another frame.
    pub fn has_queued(&self) -> bool {
        !self.uploads.is_empty()
    }

    /// Nothing queued for upload and no URL drawn in the last frame still waiting for the core.
    pub fn settled(&self) -> bool {
        self.uploads.is_empty()
            && !self.entries.values().any(|e| {
                e.slots.iter().any(|s| match s {
                    Slot::Pending { frame }
                    | Slot::Ready {
                        upgrade: Upgrade::Pending { frame },
                        ..
                    } => *frame == self.frame,
                    _ => false,
                })
            })
    }

    /// Registers what arrived, then releases pictures not drawn for 120 s and, least recently
    /// drawn first, any beyond the budget. Runs before the frame, with the host's store.
    pub fn sync(&mut self, store: &mut dyn ImageStore) {
        for up in std::mem::take(&mut self.uploads) {
            let (width, height, rgba) = prepare(&up.image, up.variant, up.phys_px);
            let bytes = rgba.len();
            let source = up.image.width().max(up.image.height());
            let id = ImageId(self.next_id);
            self.next_id += 1;
            let registered = store
                .register(
                    id,
                    ImageData {
                        width,
                        height,
                        rgba,
                    },
                )
                .is_ok();
            let Some(e) = self.entries.get_mut(&up.url) else {
                if registered {
                    store.release(id);
                }
                continue;
            };
            let slot = &mut e.slots[up.variant.index()];
            match (registered, *slot) {
                (true, old) => {
                    // A larger copy replaces the one drawn until now, and does not fade in again.
                    let mut since = self.now;
                    if let Slot::Ready {
                        art, since: first, ..
                    } = old
                    {
                        store.release(art.id);
                        since = first;
                    }
                    *slot = Slot::Ready {
                        art: Art {
                            id,
                            width,
                            height,
                            alpha: 1.0,
                        },
                        bytes,
                        drawn: self.now,
                        since,
                        source,
                        upgrade: Upgrade::None,
                    };
                }
                (false, Slot::Ready { .. }) => set_waiting(
                    slot,
                    Upgrade::Failed {
                        until: self.now + FAILED_FOR,
                    },
                ),
                (false, _) => {
                    *slot = Slot::Failed {
                        until: self.now + FAILED_FOR,
                    }
                }
            }
        }

        let now = self.now;
        for e in self.entries.values_mut() {
            for s in &mut e.slots {
                match *s {
                    Slot::Ready { art, drawn, .. } if now.saturating_sub(drawn) >= UNDRAWN_FOR => {
                        store.release(art.id);
                        *s = Slot::Empty;
                    }
                    Slot::Failed { until } if now >= until => *s = Slot::Empty,
                    _ => {}
                }
            }
        }

        let mut total: usize = 0;
        let mut ready: Vec<(Duration, String, usize, usize)> = Vec::new();
        for (url, e) in &self.entries {
            for (i, s) in e.slots.iter().enumerate() {
                if let Slot::Ready { bytes, drawn, .. } = s {
                    total += bytes;
                    ready.push((*drawn, url.clone(), i, *bytes));
                }
            }
        }
        if total > self.budget {
            ready.sort_by_key(|(drawn, ..)| *drawn);
            for (_, url, i, bytes) in ready {
                if total <= self.budget {
                    break;
                }
                if let Some(e) = self.entries.get_mut(&url)
                    && let Slot::Ready { art, .. } = e.slots[i]
                {
                    store.release(art.id);
                    e.slots[i] = Slot::Empty;
                    total -= bytes;
                }
            }
        }

        // An entry nothing holds and nobody asked for lately is forgotten (its accent with it).
        self.entries
            .retain(|_, e| e.holds_gpu() || now.saturating_sub(e.last) < UNDRAWN_FOR);
    }
}

/// `art` as drawn at `now` when it was registered at `since`: its fade-in share in `alpha` (1 at
/// rest, and always when the frame's pace is still); `fading` is set while it is below 1.
fn fade_in(mut art: Art, since: Duration, now: Duration, fading: &mut bool) -> Art {
    art.alpha = anim::enter(Some(since), now, anim::ART);
    if art.alpha < 1.0 {
        *fading = true;
    }
    art
}

/// Raises the size of the queued copy of `url` in `variant` to `phys_px`; whether one is queued.
fn raise(uploads: &mut [Upload], url: &str, variant: Variant, phys_px: u32) -> bool {
    match uploads
        .iter_mut()
        .find(|u| u.url == url && u.variant == variant)
    {
        Some(up) => {
            up.phys_px = up.phys_px.max(phys_px);
            true
        }
        None => false,
    }
}

/// The core has no picture yet (`Pending`) or none (`Failed`): a registered copy keeps being
/// drawn and only notes the state of its upgrade; any other slot takes the state itself.
fn set_waiting(slot: &mut Slot, state: Upgrade) {
    match slot {
        Slot::Ready { upgrade, .. } => *upgrade = state,
        _ => {
            *slot = match state {
                Upgrade::Pending { frame } => Slot::Pending { frame },
                Upgrade::Failed { until } => Slot::Failed { until },
                Upgrade::None => Slot::Empty,
            }
        }
    }
}

/// `image` as the pixels to register for `variant` (see the module docs).
fn prepare(image: &ArtImage, variant: Variant, phys_px: u32) -> (u32, u32, Vec<u8>) {
    let (mut w, mut h) = (image.width(), image.height());
    let mut px = image.rgba().to_vec();
    let limit = match variant {
        Variant::Plain => phys_px.max(1).saturating_mul(2),
        Variant::Soft => SOFT_SIDE,
    };
    while w.max(h) > limit && w >= 2 && h >= 2 {
        (w, h, px) = half(&px, w, h);
    }
    if variant == Variant::Soft {
        soften(&mut px, w, h, BLUR_RADIUS, BLUR_PASSES);
    }
    (w, h, px)
}

/// `src` (`w` x `h`, straight RGBA) halved by a 2x2 box; an odd side rounds up and its last
/// column or row counts twice.
pub fn half(src: &[u8], w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    let (mw, mh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0u8; mw * mh * 4];
    for y in 0..mh {
        let (y0, y1) = (2 * y, (2 * y + 1).min(h - 1));
        for x in 0..mw {
            let (x0, x1) = (2 * x, (2 * x + 1).min(w - 1));
            for ch in 0..4 {
                let at = |xx: usize, yy: usize| u32::from(src[(yy * w + xx) * 4 + ch]);
                let sum = at(x0, y0) + at(x1, y0) + at(x0, y1) + at(x1, y1);
                out[(y * mw + x) * 4 + ch] = ((sum + 2) / 4) as u8;
            }
        }
    }
    (mw as u32, mh as u32, out)
}

/// `px` (`w` x `h`, straight RGBA) under `passes` box blurs of `radius` px each way: near a
/// gaussian, so the backdrop reads as light and colour rather than as edges. The size is kept.
pub fn soften(px: &mut [u8], w: u32, h: u32, radius: usize, passes: usize) {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || px.len() != w * h * 4 {
        return;
    }
    let mut tmp = px.to_vec();
    for _ in 0..passes {
        box_pass(px, &mut tmp, w, h, radius, true);
        box_pass(&tmp, px, w, h, radius, false);
    }
}

fn box_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, pos: usize| {
        if horizontal {
            (line * w + pos) * 4
        } else {
            (pos * w + line) * 4
        }
    };
    let k = (2 * radius + 1) as u32;
    let r = radius as isize;
    let last = len as isize - 1;
    for line in 0..lines {
        let mut acc = [0u32; 4];
        for b in -r..=r {
            let i = at(line, b.clamp(0, last) as usize);
            for ch in 0..4 {
                acc[ch] += u32::from(src[i + ch]);
            }
        }
        for b in 0..len as isize {
            let o = at(line, b as usize);
            for ch in 0..4 {
                dst[o + ch] = ((acc[ch] + k / 2) / k) as u8;
            }
            let add = at(line, (b + r + 1).min(last) as usize);
            let sub = at(line, (b - r).max(0) as usize);
            for ch in 0..4 {
                acc[ch] = acc[ch] + u32::from(src[add + ch]) - u32::from(src[sub + ch]);
            }
        }
    }
}

/// The placeholder where a cover is not there (yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoArt {
    /// The media control's and the idle view's small tile: a soft tile and a note.
    Thumb,
    /// The grid card's and the hero's tile: a soft tile and a larger note.
    Card,
}

/// Draws `art` contained in `r` under a rounded clip, or the `style` placeholder without it.
pub(crate) fn cover(p: &mut P<'_>, k: &Look, r: Rect, radius: f32, art: Option<Art>, style: NoArt) {
    match art {
        Some(a) => {
            // A picture still arriving comes in over the placeholder.
            if a.alpha < 1.0 {
                no_art(p, k, r, radius, style);
            }
            p.push_clip_rounded(r, radius);
            p.image(r, a.id, Fit::Contain, ImageAlign::CENTER, a.alpha);
            p.pop_clip();
        }
        None => no_art(p, k, r, radius, style),
    }
}

fn no_art(p: &mut P<'_>, k: &Look, r: Rect, radius: f32, style: NoArt) {
    p.fill(r, radius, alpha(k.ink.light, 0x0E));
    p.ring(r, radius, 1.0, k.ink.line, false);
    let (size, tint, weight) = match style {
        NoArt::Thumb => ((r.w * 0.36).max(14.0), alpha(k.ink.text2, 0xA0), 1.3),
        NoArt::Card => (r.w * 0.28, alpha(k.ink.text2, 0x80), 1.6),
    };
    icon(
        p,
        Icon::Note,
        (r.x + r.w / 2.0, r.y + r.h / 2.0),
        size,
        tint,
        weight,
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};

    use lune_ui::testing::FakeImages;

    use super::*;

    /// A pull with scripted answers per URL (`Pending` once the script runs out) and a log of asks.
    #[derive(Default)]
    struct FakePull {
        script: RefCell<HashMap<String, VecDeque<ArtStatus>>>,
        asks: RefCell<Vec<String>>,
    }

    impl FakePull {
        fn then(&self, url: &str, status: ArtStatus) {
            self.script
                .borrow_mut()
                .entry(url.to_owned())
                .or_default()
                .push_back(status);
        }

        fn asks(&self, url: &str) -> usize {
            self.asks.borrow().iter().filter(|u| *u == url).count()
        }
    }

    impl ArtPull for FakePull {
        fn art(&self, image: &ImageRef, _urgency: ArtUrgency) -> ArtStatus {
            self.asks.borrow_mut().push(image.url.clone());
            self.script
                .borrow_mut()
                .get_mut(&image.url)
                .and_then(VecDeque::pop_front)
                .unwrap_or(ArtStatus::Pending)
        }
    }

    fn image_ref(url: &str) -> ImageRef {
        ImageRef {
            url: url.into(),
            width: None,
            height: None,
        }
    }

    fn picture(side: u32) -> ArtStatus {
        let rgba = vec![200u8; (side * side * 4) as usize];
        ArtStatus::Ready(Arc::new(ArtImage::new(side, side, rgba).unwrap()))
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn frame(cache: &mut ArtCache, t: u64) {
        cache.note_time(secs(t));
    }

    #[test]
    fn u1_a_drawn_pending_url_is_asked_every_frame() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        let r = image_ref("u");
        for t in 0..3 {
            frame(&mut cache, t);
            assert_eq!(
                cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64),
                None
            );
        }
        assert_eq!(pull.asks("u"), 3);
        // A URL not drawn in a frame is not asked.
        frame(&mut cache, 3);
        assert_eq!(pull.asks("u"), 3);
    }

    #[test]
    fn u3_a_cover_fades_in_from_its_registration_and_asks_for_frames_until_it_is_there() {
        anim::set_pace(anim::Pace::Full);
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        let mut store = FakeImages::default();
        pull.then("u", picture(8));
        let r = image_ref("u");
        let at = |ms: u64| Duration::from_millis(5000 + ms);
        // Asked at 5 s, registered by the sync before the next frame.
        cache.note_time(at(0));
        assert_eq!(
            cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8),
            None
        );
        cache.sync(&mut store);
        cache.note_time(at(16));
        let first = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8)
            .unwrap();
        assert!(first.alpha < 0.5, "{}", first.alpha);
        assert!(cache.fading());
        // Half way, and rising.
        cache.note_time(at(16) + anim::ART / 2);
        let mid = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8)
            .unwrap();
        assert!(mid.alpha > first.alpha && mid.alpha < 1.0);
        // Done: opaque, and no more frames asked for.
        cache.note_time(at(16) + anim::ART);
        let done = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8)
            .unwrap();
        assert_eq!(done.alpha, 1.0);
        assert!(!cache.fading());
        // A still pace (the demo's dumps, tier Off) draws it opaque from the start.
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(8));
        anim::set_pace(anim::Pace::Still);
        cache.note_time(at(0));
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8);
        cache.sync(&mut FakeImages::default());
        cache.note_time(at(16));
        let still = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 8)
            .unwrap();
        assert_eq!(still.alpha, 1.0);
        assert!(!cache.fading());
    }

    #[test]
    fn u1_settled_follows_what_is_drawn_and_pending() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        assert!(cache.settled());
        frame(&mut cache, 0);
        cache.get(
            &pull,
            &image_ref("u"),
            Variant::Plain,
            ArtUrgency::Visible,
            64,
        );
        assert!(!cache.settled());
        frame(&mut cache, 1);
        assert!(cache.settled());
    }

    #[test]
    fn u1_a_dropped_want_still_yields_ready_while_the_url_is_drawn() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", ArtStatus::Pending);
        pull.then("u", ArtStatus::Pending);
        pull.then("u", picture(8));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        let mut got = None;
        for t in 0..6 {
            frame(&mut cache, t);
            cache.sync(&mut store);
            got = cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
            if got.is_some() {
                break;
            }
        }
        assert!(got.is_some());
        assert!(store.contains(got.unwrap().id));
    }

    #[test]
    fn u1_ready_is_registered_at_the_next_sync_then_returned() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(8));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        assert_eq!(
            cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64),
            None
        );
        assert!(cache.has_queued() && !cache.settled());
        assert_eq!(store.len(), 0);
        cache.sync(&mut store);
        assert!(!cache.has_queued() && cache.settled());
        frame(&mut cache, 1);
        let art = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64)
            .expect("registered");
        assert!(store.contains(art.id));
        assert_eq!((art.width, art.height), (8, 8));
        assert_eq!(pull.asks("u"), 1);
    }

    #[test]
    fn u1_a_store_refusal_gives_the_placeholder_and_no_ask_for_60_s() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(200));
        let r = image_ref("u");
        // A side of 200 is more than the store takes (Plain at 200 px keeps it whole).
        let mut store = FakeImages::with_limit(100);
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 200);
        cache.sync(&mut store);
        assert_eq!(store.len(), 0);
        for t in [1, 30, 59] {
            frame(&mut cache, t);
            assert_eq!(
                cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 200),
                None
            );
        }
        assert_eq!(pull.asks("u"), 1);
        frame(&mut cache, 61);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 200);
        assert_eq!(pull.asks("u"), 2);
    }

    #[test]
    fn u1_a_failed_answer_is_not_asked_again_for_60_s() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", ArtStatus::Failed);
        let r = image_ref("u");
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        frame(&mut cache, 30);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        assert_eq!(pull.asks("u"), 1);
        frame(&mut cache, 61);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        assert_eq!(pull.asks("u"), 2);
    }

    #[test]
    fn u1_a_picture_not_drawn_for_120_s_is_released() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(8));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        cache.sync(&mut store);
        frame(&mut cache, 119);
        cache.sync(&mut store);
        assert_eq!(store.len(), 1);
        frame(&mut cache, 120);
        cache.sync(&mut store);
        assert_eq!(store.len(), 0);
        assert_eq!(store.freed(), 1);
    }

    #[test]
    fn u1_drawing_keeps_a_picture_and_ids_are_never_reused() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(8));
        pull.then("u", picture(8));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let first = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64)
            .unwrap();
        // Drawn at 100 s: still held at 200 s.
        frame(&mut cache, 100);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        frame(&mut cache, 200);
        cache.sync(&mut store);
        assert_eq!(store.len(), 1);
        // Undrawn for 120 s: released, then asked and registered again under a new id.
        frame(&mut cache, 330);
        cache.sync(&mut store);
        assert_eq!(store.len(), 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64);
        cache.sync(&mut store);
        frame(&mut cache, 331);
        let second = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 64)
            .unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn u1_budget_eviction_takes_the_oldest_drawn_first() {
        // Three 8 x 8 pictures are 256 bytes each; a budget of 600 keeps two.
        let (mut cache, pull) = (ArtCache::with_budget(600), FakePull::default());
        let mut store = FakeImages::new();
        let mut ids = Vec::new();
        for (t, url) in ["a", "b", "c"].into_iter().enumerate() {
            pull.then(url, picture(8));
            frame(&mut cache, t as u64);
            cache.get(
                &pull,
                &image_ref(url),
                Variant::Plain,
                ArtUrgency::Visible,
                64,
            );
            cache.sync(&mut store);
        }
        frame(&mut cache, 3);
        for url in ["a", "b", "c"] {
            let got = cache.get(
                &pull,
                &image_ref(url),
                Variant::Plain,
                ArtUrgency::Visible,
                64,
            );
            ids.push(got.map(|a| a.id));
        }
        // 3 x 256 = 768 > 600: "a" (drawn first) went, "b" and "c" stay.
        assert_eq!(store.len(), 2);
        assert!(ids[0].is_none());
        assert!(ids[1].is_some_and(|id| store.contains(id)));
        assert!(ids[2].is_some_and(|id| store.contains(id)));
    }

    #[test]
    fn u1_plain_and_soft_of_one_current_url_cost_one_ask() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(640));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        assert!(
            cache
                .get(&pull, &r, Variant::Plain, ArtUrgency::Current, 56)
                .is_none()
        );
        assert!(
            cache
                .get(&pull, &r, Variant::Soft, ArtUrgency::Current, 1000)
                .is_none()
        );
        assert_eq!(pull.asks("u"), 1);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let plain = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Current, 56)
            .unwrap();
        let soft = cache
            .get(&pull, &r, Variant::Soft, ArtUrgency::Current, 1000)
            .unwrap();
        assert_ne!(plain.id, soft.id);
        // Plain: 640 -> 320 -> 160 -> 80, the first side within 2 x 56 = 112.
        assert_eq!((plain.width, plain.height), (80, 80));
        // Soft: halved to at most 640 px (a 640 px picture stays).
        assert_eq!((soft.width, soft.height), (640, 640));
        assert_eq!(pull.asks("u"), 1);
    }

    #[test]
    fn u1_halving_stops_within_twice_the_asked_size() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(640));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 100);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let art = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 100)
            .unwrap();
        assert_eq!((art.width, art.height), (160, 160));
        assert!(art.width <= 200);
    }

    // F2: every slot of one frame is served by a copy at least as large as the largest ask.
    #[test]
    fn u1_asks_of_one_frame_register_for_the_largest_slot() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(300));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        // A row thumb at 2x (72 phys), then the media control's thumb (112 phys).
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 72);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let art = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 72)
            .unwrap();
        // 300 -> 150: within 2 x 112, and not below 112.
        assert_eq!((art.width, art.height), (150, 150));
        assert_eq!(pull.asks("u"), 1);
    }

    #[test]
    fn u1_a_later_wider_slot_gets_a_larger_copy() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(300));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 36);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let small = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 36)
            .unwrap();
        assert_eq!(small.width, 38, "300 -> 150 -> 75 -> 38");
        // A wider slot: the small copy is drawn while the core is asked again.
        pull.then("u", ArtStatus::Pending);
        pull.then("u", picture(300));
        frame(&mut cache, 2);
        assert_eq!(
            cache.get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112),
            Some(small)
        );
        assert!(!cache.settled(), "the larger copy is still awaited");
        frame(&mut cache, 3);
        assert_eq!(
            cache.get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112),
            Some(small)
        );
        assert_eq!(pull.asks("u"), 3);
        cache.sync(&mut store);
        assert!(!store.contains(small.id), "the small copy is released");
        frame(&mut cache, 4);
        let big = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112)
            .unwrap();
        assert_eq!((big.width, big.height), (150, 150));
        assert!(store.contains(big.id) && big.id != small.id);
        // The smaller slot draws the larger copy; nothing is asked any more.
        frame(&mut cache, 5);
        assert_eq!(
            cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 36),
            Some(big)
        );
        assert_eq!(pull.asks("u"), 3);
    }

    #[test]
    fn u1_a_source_smaller_than_the_slot_is_not_asked_again() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(64));
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 200);
        cache.sync(&mut store);
        for t in 1..4 {
            frame(&mut cache, t);
            let art = cache
                .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 200)
                .unwrap();
            assert_eq!(art.width, 64);
        }
        assert_eq!(pull.asks("u"), 1);
    }

    #[test]
    fn u1_a_failed_upgrade_keeps_the_smaller_copy_and_waits_60_s() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        pull.then("u", picture(300));
        pull.then("u", ArtStatus::Failed);
        let r = image_ref("u");
        let mut store = FakeImages::new();
        frame(&mut cache, 0);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 36);
        cache.sync(&mut store);
        frame(&mut cache, 1);
        let small = cache
            .get(&pull, &r, Variant::Plain, ArtUrgency::Visible, 36)
            .unwrap();
        for t in [2, 30, 60] {
            frame(&mut cache, t);
            assert_eq!(
                cache.get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112),
                Some(small)
            );
        }
        assert_eq!(pull.asks("u"), 2);
        assert!(cache.settled());
        frame(&mut cache, 62);
        cache.get(&pull, &r, Variant::Plain, ArtUrgency::Current, 112);
        assert_eq!(pull.asks("u"), 3);
    }

    #[test]
    fn u1_the_accent_is_kept_with_the_entry() {
        let (mut cache, pull) = (ArtCache::new(), FakePull::default());
        // A saturated red picture.
        let rgba: Vec<u8> = (0..16).flat_map(|_| [220u8, 20, 20, 255]).collect();
        pull.then(
            "u",
            ArtStatus::Ready(Arc::new(ArtImage::new(4, 4, rgba).unwrap())),
        );
        frame(&mut cache, 0);
        cache.get(
            &pull,
            &image_ref("u"),
            Variant::Plain,
            ArtUrgency::Visible,
            64,
        );
        assert!(cache.accent("u").is_some());
        assert!(cache.accent("other").is_none());
    }

    #[test]
    fn u2_half_rounds_odd_sizes_up() {
        let src = vec![100u8; 3 * 3 * 4];
        let (w, h, out) = half(&src, 3, 3);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out.len(), 2 * 2 * 4);
        assert!(out.iter().all(|&v| v == 100));
        let (w, h, _) = half(&[0u8; 5 * 2 * 4], 5, 2);
        assert_eq!((w, h), (3, 1));
    }

    #[test]
    fn u2_half_averages_a_two_by_two_block() {
        // Black, white / white, black -> mid grey.
        let mut src = vec![0u8; 16];
        for (i, v) in [0u8, 255, 255, 0].into_iter().enumerate() {
            src[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
        }
        let (w, h, out) = half(&src, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![128, 128, 128, 255]);
    }

    #[test]
    fn u2_soften_keeps_the_size_and_averages_a_checker() {
        let mut px = vec![0u8; 16];
        for (i, v) in [0u8, 255, 255, 0].into_iter().enumerate() {
            px[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
        }
        soften(&mut px, 2, 2, BLUR_RADIUS, BLUR_PASSES);
        assert_eq!(px.len(), 16);
        for chunk in px.chunks(4) {
            assert!((120..=135).contains(&chunk[0]), "{chunk:?}");
            assert_eq!(chunk[3], 255);
        }
        // A non-square picture is blurred whole.
        let mut wide = vec![50u8; 7 * 3 * 4];
        soften(&mut wide, 7, 3, BLUR_RADIUS, BLUR_PASSES);
        assert_eq!(wide.len(), 7 * 3 * 4);
        assert!(wide.iter().all(|&v| v == 50));
    }
}
