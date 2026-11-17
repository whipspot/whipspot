//! The media control (C6, MD53): the glass panel along the window's bottom. Left, the art slot
//! (no art in M5) with the title and artist; centre, the transport round the progress ring (a
//! press on the ring seeks, a press inside it toggles play); right, the quality pill (its hover
//! shows the signal path), the device and queue icons and the volume wedge. There are no time
//! labels: the times live in the ring's tooltip.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use lune_ui::color::Rgba;
use lune_ui::input::CursorIcon;
use lune_ui::paint::{Rect, StrokeStyle};
use lune_ui::scene::LayerId;
use lune_ui::ui::{Response, Sense, Ui};
use whipspot_core::diagnostics::AudioDiagnostics;
use whipspot_core::player::{
    ArtUrgency, PlayState, PlayerAction, PlayerHandle, PlayerState, RepeatMode,
};
use whipspot_core::provider::ImageRef;

use crate::anim::{self, Reveal, Sweep};
use crate::art::{ArtCache, NoArt, Variant, cover};
use crate::draw::{
    P, alpha, caps, caps_width, dial_ticks, disc, glass, glow, knob, led, mono, mono_width, shadow,
    text, text_c, vgradient, well, width,
};
use crate::icons::{Icon, icon, pause_filled, play_filled, skip_filled};
use crate::likes::{LIKE_HINT, LikeBook, heart};
use crate::now_playing::format_time;
use crate::signal::{self, Kind, kind_color};
use crate::spectrum::FRAME_INTERVAL;
use crate::theme::{Glyphs, Indicator, Look, Pill, PlayButton, Ring, Volume};
use crate::{FADE_FRAME, Hover, with_alpha};

/// The play and pause glyphs' cross-fade.
const GLYPH_SWAP: Duration = Duration::from_millis(140);
/// The line icons' stroke.
const HAIR: f32 = 1.5;
/// The progress ring's radius (the decorations sit round it).
const RING_R: f32 = 28.0;
/// How far the ring's hit band reaches either side of its radius.
const ANNULUS: f32 = 8.0;
/// A transport button's hit square (half side).
const BUTTON_HALF: f32 = 14.0;
/// The window width from which the right group has its full size and the pill is shown.
const WIDE: f32 = 1000.0;
/// The cover's side in the panel.
const THUMB: f32 = 56.0;

/// The space the like slot reserves after the title column: the gap to the heart's centre and
/// the heart's size (MD77, MOCK).
const LIKE_GAP: f32 = 26.0;
const LIKE_SIZE: f32 = 17.0;
const LIKE_SLOT: f32 = LIKE_GAP + LIKE_SIZE;
/// The most the title column may be.
const TITLE_MAX: f32 = 240.0;
/// The space kept between the groups of the panel.
const GROUP_GAP: f32 = 16.0;
/// How long a value the user just set is kept on show while the player has not caught up.
const HOLD: Duration = Duration::from_millis(600);
/// A shown seek position this near the player's counts as caught up, ms.
const SEEK_CAUGHT_UP: f32 = 1500.0;
/// Volume change per scroll notch.
const SCROLL_STEP: i32 = 5;
/// Pixels of touchpad scroll that count as one notch.
const SCROLL_PIXELS_PER_NOTCH: f32 = 40.0;

/// What the media control asks of the shell in one frame.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct MediaOut {
    /// The quality pill was pressed: open or close the Audio window.
    pub toggle_audio: bool,
    /// The queue icon was pressed: show the Queue route.
    pub open_queue: bool,
    /// The queue icon's hit rect.
    pub queue_rect: Option<Rect>,
    /// The quality pill's rect; `None` when the pill is not drawn.
    pub pill_rect: Option<Rect>,
}

/// Where a pointer is on the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    /// Inside the ring: a press toggles play.
    Disc,
    /// On the ring's band: a press starts a seek.
    Annulus,
}

/// A seek gesture in progress: the press landed in `zone`; `fraction` follows the pointer.
#[derive(Debug, Clone, Copy)]
struct Gesture {
    zone: Zone,
    fraction: f32,
}

/// A value the user just set, shown until the player's state shows it too (or [`HOLD`] passes), so
/// the control does not jump back between the gesture and the state update.
#[derive(Debug, Clone, Copy)]
struct Held {
    value: f32,
    until: Duration,
}

impl Held {
    fn new(value: f32, now: Duration) -> Self {
        Held {
            value,
            until: now + HOLD,
        }
    }

    /// The value to show: the held one while it is live and the player's is not near it.
    fn show(held: Option<Held>, actual: f32, now: Duration, tolerance: f32) -> f32 {
        match held {
            Some(h) if now < h.until && (actual - h.value).abs() > tolerance => h.value,
            _ => actual,
        }
    }
}

/// The media control's retained state.
#[derive(Debug, Default)]
pub struct MediaControl {
    seek: Option<Gesture>,
    seek_held: Option<Held>,
    volume_held: Option<Held>,
    /// The ring's sweep back to the start after a track change, the change it began for, and the
    /// share the ring showed last frame (where the next sweep starts from).
    sweep: Option<Sweep>,
    swept_for: Option<Duration>,
    last_at: f32,
    /// Whether the play button showed the pause glyph last frame, and when that changed.
    busy: Option<bool>,
    busy_since: Option<Duration>,
    /// The three cards that fade in when they start being shown.
    ring_tip: Reveal,
    pill_tip: Reveal,
    like_tip: Reveal,
}

// ---------------------------------------------------------------------------------------------
// Pure helpers.

/// The fraction of the turn (0..1, 12 o'clock = 0, clockwise) at which `pointer` lies about
/// `center`.
pub(crate) fn ring_fraction(center: (f32, f32), pointer: (f32, f32)) -> f32 {
    let (dx, dy) = (pointer.0 - center.0, pointer.1 - center.1);
    let turn = dx.atan2(-dy) / TAU;
    let f = if turn < 0.0 { turn + 1.0 } else { turn };
    f.clamp(0.0, 1.0)
}

/// The position a seek to `fraction` of a track of `duration_ms` lands on (clamped to the track).
pub(crate) fn seek_ms(fraction: f32, duration_ms: u64) -> u32 {
    let f = f64::from(fraction.clamp(0.0, 1.0));
    let ms = (f * duration_ms as f64).round().min(duration_ms as f64);
    ms.min(f64::from(u32::MAX)) as u32
}

/// Theme B's readout under the wedge: the level's gain in dB by MD29's law (`40 log10(v/100)`),
/// whole dB.
pub(crate) fn volume_db_label(volume: u8) -> String {
    if volume == 0 {
        return "\u{2212}\u{221e} dB".to_owned();
    }
    let db = (40.0 * (f64::from(volume.min(100)) / 100.0).log10()).round() as i32;
    if db == 0 {
        "0 dB".to_owned()
    } else {
        format!("\u{2212}{} dB", -db)
    }
}

/// The volume (0..=100) the pointer at `x` picks on a wedge spanning `left`..`left + w`.
fn volume_at(x: f32, left: f32, w: f32) -> u8 {
    if w <= 0.0 {
        return 0;
    }
    (100.0 * ((x - left) / w).clamp(0.0, 1.0)).round() as u8
}

/// Where `p` is on a ring of radius `r` about `c`, or `None` outside its band.
fn zone_of(c: (f32, f32), r: f32, p: (f32, f32)) -> Option<Zone> {
    let d = (p.0 - c.0).hypot(p.1 - c.1);
    if d < r - ANNULUS {
        Some(Zone::Disc)
    } else if d <= r + ANNULUS {
        Some(Zone::Annulus)
    } else {
        None
    }
}

/// The ring's radius in the look's style (where a seek lands).
fn ring_radius(k: &Look) -> f32 {
    match k.decor().ring {
        Ring::Bold => RING_R + 3.0,
        Ring::Ticked => RING_R + 4.0,
        Ring::Hairline => RING_R - 1.0,
    }
}

/// The point at fraction `f` of the ring's turn from 12 o'clock, at radius `r`.
fn on_ring(c: (f32, f32), r: f32, f: f32) -> (f32, f32) {
    let a = -PI / 2.0 + f * TAU;
    (c.0 + r * a.cos(), c.1 + r * a.sin())
}

/// Where the parts of the panel sit at one window width (all of it derived from the panel's
/// rectangle, the window's width and the look).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geo {
    cx: f32,
    cy: f32,
    /// Skip buttons sit at `cx ± dx`, shuffle and repeat at `cx ± sx`.
    dx: f32,
    sx: f32,
    thumb: Rect,
    text_x: f32,
    /// The widest the title and artist may be.
    title_max: f32,
    wedge: Rect,
    /// The right edge of the quality pill.
    pill_right: f32,
    /// The left edge of the centre group (shuffle's hit square), and its right edge.
    centre_left: f32,
    centre_right: f32,
}

/// The cover's corner radius: concentric with the panel's corner (the panel's radius less the
/// cover's inset), so the gap stays even around the corner; the theme's thumb radius when the
/// inset is larger than the panel's radius.
fn thumb_radius(panel: Rect, thumb: Rect, k: &Look) -> f32 {
    let inset = thumb.x - panel.x;
    (k.radii().panel - inset).max(k.radii().thumb)
}

fn geo(rect: Rect, win_w: f32, k: &Look) -> Geo {
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let dx = if k.decor().ring == Ring::Ticked {
        76.0
    } else {
        68.0
    };
    let sx = dx + 58.0;
    let narrow = win_w < WIDE;
    let (vw, vh) = match k.decor().volume {
        Volume::Scaled => (if narrow { 96.0 } else { 120.0 }, 28.0),
        _ => (96.0, 20.0),
    };
    // The panel is as narrow as the sheet now (32 px a side), so below the wide size the wedge sits
    // closer to the panel's edge to keep the right group clear of the centre one.
    let inset = if narrow { 12.0 } else { 24.0 };
    let wedge = Rect::new(rect.right() - inset - vw, cy - vh / 2.0, vw, vh);
    // The cover sits the same distance from the panel's left edge as from its top and bottom.
    let thumb_inset = ((rect.h - THUMB) / 2.0).max(0.0);
    let thumb = Rect::new(rect.x + thumb_inset, cy - THUMB / 2.0, THUMB, THUMB);
    let text_x = thumb.right() + 16.0;
    let centre_left = cx - sx - 9.0;
    let title_max = (centre_left - GROUP_GAP - LIKE_SLOT - text_x).clamp(0.0, TITLE_MAX);
    Geo {
        cx,
        cy,
        dx,
        sx,
        thumb,
        text_x,
        title_max,
        wedge,
        // The pill keeps the gap it had to the (now hidden) device icon, to the queue icon.
        pill_right: wedge.x - 62.0,
        centre_left,
        centre_right: cx + sx + 9.0,
    }
}

/// The left edge of the right group's icons (the queue icon's).
fn right_left(g: &Geo) -> f32 {
    g.wedge.x - 34.0 - 10.5
}

/// A pill `pill_w` wide is shown when the window is wide enough and it leaves
/// [`GROUP_GAP`] to the centre group.
fn pill_fits(g: &Geo, win_w: f32, pill_w: f32) -> bool {
    win_w >= WIDE && g.pill_right - pill_w >= g.centre_right + GROUP_GAP - 4.0
}

// ---------------------------------------------------------------------------------------------
// Drawing parts (the round-3 mockup's media panel).

/// An arc of the ring from `f0` to `f1` (fractions of the turn) in the look's ramp, `w` wide.
fn ramp_arc(p: &mut P<'_>, k: &Look, c: (f32, f32), r: f32, f0: f32, f1: f32, w: f32) {
    if f1 - f0 <= 0.0005 {
        return;
    }
    let segs = 64;
    let start = -PI / 2.0 + f0 * TAU;
    let sweep = (f1 - f0) * TAU;
    for i in 0..segs {
        let t0 = i as f32 / segs as f32;
        let t1 = (i as f32 + 1.06) / segs as f32;
        let col = k.at(f0 + (f1 - f0) * (t0 + t1) / 2.0);
        p.arc(
            [c.0, c.1],
            r,
            start + sweep * t0,
            sweep * (t1 - t0),
            StrokeStyle::solid(col, w),
        );
    }
}

/// A disc of diameter `d` drawn as a stroke: a closed circle of radius `d / 4` stroked `d / 2`
/// wide. The toolkit draws a layer's strokes after its fills, in call order, so a knob made this
/// way sits over the ring's arc while staying on the panel's own layer, under every menu and
/// dropdown the toolkit opens above it (a fixed layer above the panel's would draw over them).
fn knob_disc(p: &mut P<'_>, c: (f32, f32), d: f32, color: Rgba) {
    p.arc(
        [c.0, c.1],
        d / 4.0,
        0.0,
        TAU,
        StrokeStyle::solid(color, d / 2.0),
    );
}

/// The progress ring round the play button, in the look's style. `show` false draws the track
/// only (nothing current). Draw order: the track, the progress arc, then the knob (a stroked
/// disc, see [`knob_disc`]).
fn progress_ring(p: &mut P<'_>, k: &Look, c: (f32, f32), at: f32, show: bool) {
    let at = if show { at.clamp(0.0, 1.0) } else { 0.0 };
    match k.decor().ring {
        Ring::Bold => {
            let r = RING_R + 3.0;
            p.arc(
                [c.0, c.1],
                r,
                0.0,
                TAU,
                StrokeStyle::solid(alpha(k.ink.light, 0x22), 3.5),
            );
            if show {
                ramp_arc(p, k, c, r, 0.0, at, 3.5);
                knob_disc(p, on_ring(c, r, at), 10.0, k.ink.light);
            }
        }
        Ring::Ticked => {
            dial_ticks(
                p,
                k,
                c,
                // Inside the 76 px panel (half 38) with the ring's black groove.
                RING_R + 6.8,
                RING_R + 9.6,
                60,
                5,
                if show { at } else { -1.0 },
            );
            let r = RING_R + 4.0;
            p.arc(
                [c.0, c.1],
                r,
                0.0,
                TAU,
                StrokeStyle::solid([0x00, 0x00, 0x00, 0x70], 4.6),
            );
            p.arc(
                [c.0, c.1],
                r,
                0.0,
                TAU,
                StrokeStyle::solid(alpha(k.ink.light, 0x1C), 2.6),
            );
            if show {
                ramp_arc(p, k, c, r, 0.0, at, 2.6);
                let head = on_ring(c, r, at);
                knob_disc(p, head, 12.0, alpha(k.a, 0x22));
                knob_disc(p, head, 7.3, alpha(k.a, 0x50));
                knob_disc(p, head, 4.4, k.hi);
            }
        }
        Ring::Hairline => {
            let r = RING_R - 1.0;
            p.arc(
                [c.0, c.1],
                r,
                0.0,
                TAU,
                StrokeStyle::solid(alpha(k.ink.light, 0x26), 2.0),
            );
            if show {
                ramp_arc(p, k, c, r, 0.0, at, 3.0);
                let hc = on_ring(c, r, at);
                disc(p, (c.0, c.1 - r), 3.0, k.a);
                knob_disc(p, hc, 16.0, alpha(k.b, 0x40));
                knob_disc(p, hc, 8.0, k.ink.light);
            }
        }
    }
}

/// `color` with its alpha scaled by `share` (0..=1).
fn with_share(color: Rgba, share: f32) -> Rgba {
    [
        color[0],
        color[1],
        color[2],
        (f32::from(color[3]) * share.clamp(0.0, 1.0)).round() as u8,
    ]
}

/// The play/pause button inside the ring: the pause glyph while `busy`, the play glyph
/// otherwise; in `ink.faint` while nothing is current.
///
/// `mix` is how far a change of `busy` has got (0..=1; 1 at rest): the glyph it replaces fades
/// out as the new one fades in.
fn play_button(p: &mut P<'_>, k: &Look, c: (f32, f32), busy: bool, enabled: bool, mix: f32) {
    let glyph = |p: &mut P<'_>, busy: bool, size: f32, color: Rgba| {
        if busy {
            pause_filled(p, c, size, color);
        } else {
            play_filled(p, (c.0 - size * 0.08, c.1), size, color);
        }
    };
    // The glyph this frame shows, with the one it replaced fading out under it.
    let swap = |p: &mut P<'_>, draw: &dyn Fn(&mut P<'_>, bool, f32)| {
        if mix < 1.0 {
            draw(p, !busy, 1.0 - mix);
        }
        draw(p, busy, mix);
    };
    match k.decor().play {
        PlayButton::Disc => {
            disc(p, c, 48.0, if enabled { k.ink.light } else { k.ink.faint });
            swap(p, &|p, b, a| {
                glyph(p, b, 22.0, with_share([0x0B, 0x0C, 0x0F, 0xFF], a));
            });
        }
        PlayButton::Knob => {
            knob(p, c, 50.0);
            let color = if enabled { k.ink.text } else { k.ink.faint };
            swap(p, &|p, b, a| glyph(p, b, 19.0, with_share(color, a)));
        }
        PlayButton::Bare => {
            let color = if enabled { k.ink.text } else { k.ink.faint };
            swap(p, &|p, b, a| {
                let which = if b { Icon::Pause } else { Icon::Play };
                icon(p, which, c, 18.0, with_share(color, a), 2.0);
            });
        }
    }
}

/// A skip button (next, or previous when `!next`) in the look's glyph style.
fn skip(p: &mut P<'_>, k: &Look, c: (f32, f32), size: f32, color: Rgba, next: bool) {
    match k.decor().glyphs {
        Glyphs::Solid => skip_filled(p, c, size, color, next),
        Glyphs::Line => icon(
            p,
            if next { Icon::Next } else { Icon::Prev },
            c,
            size,
            color,
            HAIR,
        ),
    }
}

/// The volume wedge in `r` at `level` (0..1); its style by theme.
fn volume(p: &mut P<'_>, k: &Look, r: Rect, level: f32, readout: &str) {
    match k.decor().volume {
        Volume::Solid => {
            let (x, b) = (r.x, r.bottom());
            p.fill_path(
                &[[x, b], [r.right(), b], [r.right(), r.y]],
                alpha(k.ink.light, 0x26),
            );
            let f = level * r.w;
            let fh = r.h * level;
            p.fill_path(&[[x, b], [x + f, b], [x + f, b - fh]], k.a);
            p.fill(
                Rect::new(x + f - 1.0, b - fh - 4.0, 2.5, fh + 4.0),
                1.0,
                k.ink.light,
            );
        }
        Volume::Scaled => {
            let (x, top, w, h) = (r.x, r.y + 6.0, r.w, r.h - 6.0);
            for i in 0..=20 {
                let tx = x + w * i as f32 / 20.0;
                let major = i % 5 == 0;
                let th = if major { 6.0 } else { 3.0 };
                let col = if (i as f32 / 20.0) <= level {
                    alpha(k.a, if major { 0xE0 } else { 0x90 })
                } else {
                    alpha(k.ink.light, if major { 0x50 } else { 0x2C })
                };
                p.fill(Rect::new(tx - 0.5, top - 4.0 - th, 1.0, th), 0.0, col);
            }
            let tri = [[x, top], [x + w, top], [x + w, top + h]];
            p.fill_path(&tri, alpha(k.ink.light, 0x16));
            p.stroke(
                &[[x, top], [x + w, top], [x + w, top + h], [x, top]],
                true,
                StrokeStyle::solid(alpha(k.ink.light, 0x22), 1.0),
            );
            let slices = 32;
            let lit_w = w * level;
            for i in 0..slices {
                let x0 = x + lit_w * i as f32 / slices as f32;
                let x1 = x + lit_w * (i + 1) as f32 / slices as f32 + 0.3;
                let y0 = top + h * (x0 - x) / w;
                let y1 = top + h * (x1 - x) / w;
                let col = k.at(0.15 + 0.85 * level * (i as f32 + 0.5) / slices as f32);
                p.fill_path(&[[x0, top], [x1, top], [x1, y1], [x0, y0]], col);
            }
            let nx = x + lit_w;
            let ny = top + h * level;
            p.fill(
                Rect::new(nx - 0.75, top - 3.0, 1.5, ny - top + 6.0),
                0.75,
                k.hi,
            );
            disc(p, (nx, ny + 3.0), 5.0, k.hi);
            mono(p, readout, 9.5, k.ink.muted, x, top + h - 6.0);
        }
        Volume::Outline => {
            let lw = (r.w * level).round();
            for i in 0..lw as usize {
                let x = r.x + i as f32;
                let h = r.h * (i as f32 + 1.0) / r.w;
                p.fill_over(
                    Rect::new(x, r.bottom() - h, 1.0, h),
                    0.0,
                    lune_ui::color::premul(k.at(i as f32 / r.w)),
                );
            }
            p.stroke(
                &[[r.x, r.bottom()], [r.right(), r.bottom()], [r.right(), r.y]],
                true,
                StrokeStyle::solid(alpha(k.ink.light, 0x50), 1.0),
            );
            let kx = r.x + lw;
            let kh = r.h * level;
            p.fill_over(
                Rect::new(kx - 1.0, r.bottom() - kh - 4.0, 2.0, kh + 4.0),
                1.0,
                lune_ui::color::premul(k.ink.light),
            );
        }
    }
}

/// The quality pill's rectangle for `label`, its right edge at `right`.
fn pill_rect(p: &mut P<'_>, k: &Look, label: &str, right: f32, cy: f32) -> Rect {
    let h = 30.0;
    let w = match k.decor().pill {
        Pill::Glass => width(p, label, 12.5, true) + 32.0,
        Pill::Led => caps_width(p, &label.to_uppercase(), 9.5, 1.5) + 46.0,
        Pill::Dot => width(p, label, 12.0, true) + 40.0,
    };
    Rect::new(right - w, cy - h / 2.0, w, h)
}

/// Draws the quality pill in `r`; the indicator wears the verdict's colour; `hover` lifts it.
fn draw_pill(p: &mut P<'_>, k: &Look, label: &str, kind: Kind, r: Rect, hover: bool) {
    let cy = r.y + r.h / 2.0;
    let h = r.h;
    let col = kind_color(k, kind);
    let perfect = kind == Kind::BitPerfect;
    match k.decor().pill {
        Pill::Glass => {
            let fill = alpha(k.ink.light, if hover { 0x24 } else { 0x12 });
            p.fill(r, h / 2.0, fill);
            text(p, label, 12.5, col, true, r.x + 16.0, cy - 8.5, 0.0);
        }
        Pill::Led => {
            if perfect {
                glow(p, r, h / 2.0, 12.0, k.a, if hover { 0.8 } else { 0.45 });
            }
            well(p, r, h / 2.0, false);
            p.push_clip_rounded(r, h / 2.0);
            let lit = if perfect || kind == Kind::Resampled {
                col
            } else {
                k.ink.light
            };
            vgradient(
                p,
                r,
                alpha(lit, if hover { 0x14 } else { 0x08 }),
                alpha(lit, if hover { 0x2C } else { 0x18 }),
            );
            p.pop_clip();
            let nc = match kind {
                Kind::BitPerfect => k.hi,
                Kind::Unknown => k.ink.faint,
                _ => col,
            };
            icon(p, Icon::Note, (r.x + 16.0, cy), 13.0, nc, 1.6);
            let tc = if perfect { k.hi } else { k.ink.text2 };
            caps(p, &label.to_uppercase(), 9.5, tc, r.x + 30.0, cy - 6.5, 1.5);
        }
        Pill::Dot => {
            let fill = alpha(k.ink.light, if hover { 0x1E } else { 0x0E });
            p.fill(r, h / 2.0, fill);
            icon(p, Icon::Note, (r.x + 16.0, cy), 13.0, col, 1.6);
            text(p, label, 12.0, k.ink.text2, true, r.x + 27.0, cy - 8.0, 0.0);
        }
    }
}

/// A time in the look's number face; returns its width.
fn number(p: &mut P<'_>, k: &Look, s: &str, size: f32, color: Rgba, x: f32, y: f32) -> f32 {
    if k.decor().mono_numbers {
        mono(p, s, size, color, x, y)
    } else {
        text(p, s, size, color, true, x, y, 0.0)
    }
}

fn number_width(p: &mut P<'_>, k: &Look, s: &str, size: f32) -> f32 {
    if k.decor().mono_numbers {
        mono_width(p, s, size)
    } else {
        width(p, s, size, true)
    }
}

/// What the ring's tooltip shows.
#[derive(Debug, Clone, Copy)]
struct RingTip {
    /// The pointer's place on the band (the seek target), or `None` over the disc.
    seek: Option<f32>,
    /// The position shown as elapsed, ms.
    elapsed_ms: u32,
    duration_ms: u64,
    /// The fraction the ring's progress is at.
    at: f32,
}

/// The read-only heart's hint as a card above the panel (what the pointer's tooltip shows, for a
/// dump where no pointer is over the heart): its notch points at the heart at `hx`.
fn draw_like_tip(p: &mut P<'_>, k: &Look, win_w: f32, panel: Rect, hx: f32) {
    p.on(LayerId::Modal);
    let tw = width(p, LIKE_HINT, 12.5, false);
    let (w, h) = (tw + 28.0, 32.0);
    let x = (hx - w / 2.0)
        .round()
        .clamp(12.0, (win_w - 12.0 - w).max(12.0));
    let r = Rect::new(x, panel.y - 14.0 - h, w, h);
    let rad = k.radii().popup.min(h / 2.0);
    shadow(p, r, rad, 26.0, if k.surface().bevel { 1.5 } else { 0.9 });
    glass(p, k, r, rad, k.surface().popup_alpha, false);
    let nc = hx.clamp(r.x + rad + 8.0, r.right() - rad - 8.0);
    p.fill_path(
        &[
            [nc - 7.0, r.bottom() - 0.5],
            [nc + 7.0, r.bottom() - 0.5],
            [nc, r.bottom() + 6.0],
        ],
        alpha(k.surface().tint, 0xE0),
    );
    text(
        p,
        LIKE_HINT,
        12.5,
        k.ink.text,
        false,
        r.x + 14.0,
        r.y + 8.0,
        0.0,
    );
}

/// The ring card above the panel; returns its rectangle. With a seek target: the preview on the
/// ring, a ghost knob and a "Seek to" row; over the disc, the times only.
fn draw_ring_tip(
    p: &mut P<'_>,
    k: &Look,
    win_w: f32,
    panel: Rect,
    c: (f32, f32),
    tip: RingTip,
) -> Rect {
    p.on(LayerId::Modal);
    let ink = k.ink;
    let rr = ring_radius(k);
    let at = tip.at.clamp(0.0, 1.0);
    // The card is centred over the play button (the ring's centre), whatever the pointer's place
    // on the band; the ghost knob alone shows where the seek would land.
    let anchor_x = c.0;
    if let Some(f) = tip.seek {
        let (f0, f1) = if f > at { (at, f) } else { (f, at) };
        // The preview arc and, after it, the ghost knob (stroked discs) are on the panel's
        // layer, the card on this one.
        p.on(LayerId::Popup);
        let segs = 40;
        for i in 0..segs {
            let t0 = f0 + (f1 - f0) * i as f32 / segs as f32;
            let t1 = f0 + (f1 - f0) * (i as f32 + 1.05) / segs as f32;
            p.arc(
                [c.0, c.1],
                rr,
                -PI / 2.0 + t0 * TAU,
                (t1 - t0) * TAU,
                StrokeStyle::solid(alpha(ink.light, 0x50), 3.0),
            );
        }
        let hp = on_ring(c, rr, f);
        knob_disc(p, hp, 22.0, alpha(k.a, 0x38));
        knob_disc(p, hp, 12.0, ink.light);
        p.arc([hp.0, hp.1], 5.25, 0.0, TAU, StrokeStyle::solid(k.a, 1.5));
    }
    p.on(LayerId::Modal);

    let elapsed = format_time(u64::from(tip.elapsed_ms));
    let total = format_time(tip.duration_ms);
    let now_s = format!("{elapsed} / {total}");
    let size = 15.0;
    let nw = number_width(p, k, &now_s, size);
    let tracked = k.decor().labels == crate::theme::Labels::Tracked;
    let target = tip
        .seek
        .map(|f| format_time(u64::from(seek_ms(f, tip.duration_ms))));
    let seek_lw = if tracked {
        caps_width(p, "SEEK TO", 8.5, 1.6)
    } else {
        width(p, "Seek to", 12.0, false)
    };
    let tw = target
        .as_deref()
        .map_or(0.0, |t| number_width(p, k, t, 13.0));
    let content = if target.is_some() {
        nw.max(seek_lw + 8.0 + tw)
    } else {
        nw
    };
    let w = (content + 40.0).max(150.0);
    let h = if target.is_some() { 66.0 } else { 44.0 };
    let x = (anchor_x - w / 2.0)
        .round()
        .clamp(12.0, (win_w - 12.0 - w).max(12.0));
    let r = Rect::new(x, panel.y - 14.0 - h, w, h);
    let rad = k.radii().popup;
    shadow(p, r, rad, 26.0, if k.surface().bevel { 1.5 } else { 0.9 });
    glass(p, k, r, rad, k.surface().popup_alpha, false);
    // The notch towards the ring.
    let nc = anchor_x.clamp(r.x + rad + 8.0, r.right() - rad - 8.0);
    p.fill_path(
        &[
            [nc - 7.0, r.bottom() - 0.5],
            [nc + 7.0, r.bottom() - 0.5],
            [nc, r.bottom() + 6.0],
        ],
        alpha(k.surface().tint, 0xE0),
    );
    // Both rows are centred in the card.
    let x0 = (r.x + (w - nw) / 2.0).round();
    let ew = number_width(p, k, &format!("{elapsed} "), size);
    let ty = if target.is_some() {
        r.y + 12.0
    } else {
        r.y + 13.0
    };
    number(p, k, &elapsed, size, ink.text, x0, ty);
    number(p, k, &format!("/ {total}"), size, ink.muted, x0 + ew, ty);
    if let Some(target) = target {
        let sy = r.y + 40.0;
        let row_w = if tracked {
            12.0 + seek_lw + 8.0 + tw
        } else {
            14.0 + seek_lw + 6.0 + tw
        };
        let x0 = (r.x + (w - row_w) / 2.0).round();
        if tracked {
            led(p, (x0 + 3.0, sy + 6.5), 4.0, k.a, k.hi, true);
            caps(p, "SEEK TO", 8.5, ink.muted, x0 + 12.0, sy + 2.0, 1.6);
            number(
                p,
                k,
                &target,
                12.5,
                k.hi,
                x0 + 12.0 + seek_lw + 8.0,
                sy - 1.0,
            );
        } else {
            icon(p, Icon::ChevR, (x0 + 4.0, sy + 8.0), 9.0, k.a, 1.6);
            text(p, "Seek to", 12.0, ink.muted, false, x0 + 14.0, sy, 0.0);
            number(
                p,
                k,
                &target,
                12.5,
                k.hi,
                x0 + 14.0 + seek_lw + 6.0,
                sy - 0.5,
            );
        }
    }
    r
}

/// A square hit region of half side `half` about `c`, with the pointer cursor over it.
fn button(ui: &mut Ui<'_>, key: &str, c: (f32, f32), half: f32, enabled: bool) -> Response {
    let id = ui.id(key);
    let rect = Rect::new(c.0 - half, c.1 - half, 2.0 * half, 2.0 * half);
    let r = ui.with_enabled(enabled, |ui| ui.interact(id, rect, Sense::CLICK));
    if enabled && r.hovered {
        ui.set_cursor(CursorIcon::Pointer);
    }
    r
}

/// `lit` when hovered, else `base`.
fn lift(k: &Look, base: Rgba, hovered: bool) -> Rgba {
    if hovered { k.ink.text } else { base }
}

impl MediaControl {
    /// Draws the control in `rect` and dispatches what the user did. `forced` shows a hover
    /// state without a pointer (the demo's dumps only).
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        k: &Look,
        state: &PlayerState,
        diag: &AudioDiagnostics,
        forced: Option<Hover>,
        player: &PlayerHandle,
        art: &mut ArtCache,
        likes: &mut LikeBook,
        track_since: Option<Duration>,
    ) -> MediaOut {
        let now = ui.time();
        let win_w = ui.painter().window.w;
        let g = geo(rect, win_w, k);
        let c = (g.cx, g.cy);
        let has = state.current.is_some();
        let duration_ms = state.current.as_ref().map_or(0, |n| n.track.duration_ms);
        let busy = matches!(state.play_state, PlayState::Playing | PlayState::Loading);
        let mut out = MediaOut::default();
        let ink = k.ink;

        // The glass: on the popup layer, a backdrop on the page would blur the page's own
        // content, and the visualiser behind it is what the panel should show through.
        {
            let p = ui.painter();
            p.on(LayerId::Popup);
            let radius = k.radii().panel;
            if k.surface().bevel {
                shadow(p, rect, radius, 26.0, 1.0);
            }
            glass(p, k, rect, radius, k.surface().panel_alpha, true);
        }

        // Left: the cover, the title and the artist, then the like heart in its slot; a new track
        // comes in with a fade.
        let track_fade = anim::enter(track_since, now, anim::TRACK);
        let mut moving = anim::running(track_since, now, anim::TRACK);
        let mut text_w = None;
        with_alpha(ui, track_fade, |ui| {
            let phys = (g.thumb.w * ui.painter().s).ceil() as u32;
            let current_cover = state
                .current
                .as_ref()
                .and_then(|n| ImageRef::pick(&n.track.images, phys))
                .and_then(|i| art.get(player, i, Variant::Plain, ArtUrgency::Current, phys));
            let p = ui.painter();
            cover(
                p,
                k,
                g.thumb,
                thumb_radius(rect, g.thumb, k),
                current_cover,
                NoArt::Thumb,
            );
            if let Some(n) = &state.current
                && g.title_max >= 8.0
            {
                let tw = text(
                    p,
                    &n.track.name,
                    15.0,
                    ink.text,
                    true,
                    g.text_x,
                    g.cy - 21.0,
                    g.title_max,
                );
                let aw = text(
                    p,
                    &n.track.artist,
                    13.0,
                    ink.muted,
                    false,
                    g.text_x,
                    g.cy + 2.0,
                    g.title_max,
                );
                text_w = Some(tw.max(aw));
            }
        });
        // The heart (MD77): 17 px at the end of the text, in the slot the title clamp keeps free.
        let mut like_tip = None;
        with_alpha(ui, track_fade, |ui| {
            if let (Some(n), Some(w)) = (&state.current, text_w)
                && let Some(entry) = state.providers.iter().find(|e| e.info.id == n.provider)
                && entry.info.caps.likes
            {
                let hx = g.text_x + w + LIKE_GAP;
                let glyph = Rect::new(
                    hx - LIKE_SIZE / 2.0,
                    g.cy - LIKE_SIZE / 2.0,
                    LIKE_SIZE,
                    LIKE_SIZE,
                );
                let uri = &n.track.uri;
                let writable = entry.likes_writable;
                // The media heart always shows its outline: it is the current track's, not a row's.
                if heart(ui, "media-like", glyph, k, likes.state(uri), writable, true) {
                    likes.click(uri, writable, player);
                }
                if !writable && forced == Some(Hover::Like) {
                    like_tip = Some(hx);
                }
            }
        });

        // Centre: the ring (seek by a press on its band, toggle by a press inside it).
        let ring_r = ring_radius(k);
        let ring_id = ui.id("media-ring");
        let half = ring_r + ANNULUS;
        let ring_hit = Rect::new(c.0 - half, c.1 - half, 2.0 * half, 2.0 * half);
        let ring = ui.with_enabled(has, |ui| ui.interact(ring_id, ring_hit, Sense::CLICK));
        let pointer = ui.pointer_pos();
        let over = if has && ring.hovered {
            pointer.and_then(|pp| zone_of(c, ring_r, pp))
        } else {
            None
        };
        if over.is_some() {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if ring.press_started
            && let Some(pos) = ring.press_pos
            && let Some(zone) = zone_of(c, ring_r, pos)
        {
            self.seek = Some(Gesture {
                zone,
                fraction: ring_fraction(c, pos),
            });
        }
        if let Some(gesture) = self.seek.as_mut()
            && ring.pressed
            && gesture.zone == Zone::Annulus
            && let Some(pp) = pointer
        {
            gesture.fraction = ring_fraction(c, pp);
        }
        if let Some(gesture) = self.seek
            && !ring.pressed
        {
            self.seek = None;
            match gesture.zone {
                Zone::Annulus if duration_ms > 0 => {
                    let ms = seek_ms(gesture.fraction, duration_ms);
                    player.dispatch(PlayerAction::Seek { position_ms: ms });
                    self.seek_held = Some(Held::new(ms as f32, now));
                }
                Zone::Disc if ring.clicked => player.dispatch(PlayerAction::Toggle),
                _ => {}
            }
        }

        // Progress: the seek preview while the gesture runs, then the player's position.
        let actual_ms = state.position_now() as f32;
        let shown_ms = match self.seek {
            Some(Gesture {
                zone: Zone::Annulus,
                fraction,
            }) => fraction * duration_ms as f32,
            _ => Held::show(self.seek_held, actual_ms, now, SEEK_CAUGHT_UP),
        };
        let live = if duration_ms > 0 {
            (shown_ms / duration_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // A new track sweeps the ring back to its start instead of jumping there.
        if track_since != self.swept_for {
            self.swept_for = track_since;
            self.sweep = track_since.and_then(|s| Sweep::new(self.last_at, s));
        }
        let at = match self.sweep.and_then(|sw| sw.value(live, now)) {
            Some(v) => {
                moving = true;
                v
            }
            None => {
                self.sweep = None;
                live
            }
        };
        self.last_at = at;
        let tip = match (forced, self.seek, over, pointer) {
            (Some(Hover::Ring(f)), ..) => Some(RingTip {
                seek: Some(f.clamp(0.0, 1.0)),
                elapsed_ms: shown_ms as u32,
                duration_ms,
                at,
            }),
            (
                _,
                Some(Gesture {
                    zone: Zone::Annulus,
                    fraction,
                }),
                ..,
            ) => Some(RingTip {
                seek: Some(fraction),
                elapsed_ms: shown_ms as u32,
                duration_ms,
                at,
            }),
            (_, _, Some(Zone::Annulus), Some(pp)) => Some(RingTip {
                seek: Some(ring_fraction(c, pp)),
                elapsed_ms: shown_ms as u32,
                duration_ms,
                at,
            }),
            (_, _, Some(Zone::Disc), _) => Some(RingTip {
                seek: None,
                elapsed_ms: shown_ms as u32,
                duration_ms,
                at,
            }),
            _ => None,
        };

        // Transport buttons.
        let skip_y = (g.cy, BUTTON_HALF);
        let prev = button(ui, "media-prev", (g.cx - g.dx, skip_y.0), skip_y.1, has);
        let next = button(ui, "media-next", (g.cx + g.dx, skip_y.0), skip_y.1, has);
        let shuffle = button(ui, "media-shuffle", (g.cx - g.sx, g.cy), BUTTON_HALF, true);
        let repeat = button(ui, "media-repeat", (g.cx + g.sx, g.cy), BUTTON_HALF, true);
        if prev.clicked {
            player.dispatch(PlayerAction::Previous);
        }
        if next.clicked {
            player.dispatch(PlayerAction::Next);
        }
        if shuffle.clicked {
            player.dispatch(PlayerAction::SetShuffle(!state.shuffle));
        }
        if repeat.clicked {
            let next = match state.repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            player.dispatch(PlayerAction::SetRepeat(next));
        }

        let ring_hover = tip.is_some();
        // The play and pause glyphs cross-fade when playback starts or stops.
        if self.busy != Some(busy && has) {
            if self.busy.is_some() {
                self.busy_since = Some(now);
            }
            self.busy = Some(busy && has);
        }
        let play_mix = anim::enter(self.busy_since, now, GLYPH_SWAP);
        moving |= anim::running(self.busy_since, now, GLYPH_SWAP);
        {
            let p = ui.painter();
            if ring_hover {
                disc(p, c, 2.0 * RING_R + 22.0, alpha(ink.light, 0x0C));
            }
            progress_ring(p, k, c, at, has);
            play_button(p, k, c, busy && has, has, play_mix);

            let solid = k.decor().glyphs == Glyphs::Solid;
            let sk = if solid { 18.0 } else { 17.0 };
            let base = if has { ink.text2 } else { ink.faint };
            skip(
                p,
                k,
                (g.cx - g.dx, g.cy),
                sk,
                lift(k, base, prev.hovered && has),
                false,
            );
            skip(
                p,
                k,
                (g.cx + g.dx, g.cy),
                sk,
                lift(k, base, next.hovered && has),
                true,
            );
            let sw = if solid { 2.0 } else { HAIR };
            let (sh_x, rp_x) = (g.cx - g.sx, g.cx + g.sx);
            let sh_col = if state.shuffle {
                k.a
            } else {
                lift(k, ink.muted, shuffle.hovered)
            };
            let on_repeat = state.repeat != RepeatMode::Off;
            let rp_col = if on_repeat {
                k.a
            } else {
                lift(k, ink.muted, repeat.hovered)
            };
            icon(p, Icon::Shuffle, (sh_x, g.cy), 18.0, sh_col, sw);
            icon(p, Icon::Repeat, (rp_x, g.cy), 18.0, rp_col, sw);
            if state.repeat == RepeatMode::One {
                text_c(p, "1", 7.5, rp_col, true, rp_x, g.cy - 3.6);
            }
            match k.decor().indicator {
                Indicator::Led => {
                    led(p, (sh_x, g.cy + 16.0), 3.0, k.a, k.hi, state.shuffle);
                    led(p, (rp_x, g.cy + 16.0), 3.0, k.a, k.hi, on_repeat);
                }
                Indicator::Check => {
                    if state.shuffle {
                        disc(p, (sh_x, g.cy + 15.0), 3.5, k.a);
                    }
                    if on_repeat {
                        disc(p, (rp_x, g.cy + 15.0), 3.5, k.a);
                    }
                }
            }
        }

        // Right: the wedge, the queue and device icons, the pill.
        let actual_vol = f32::from(state.volume);
        let mut vol = Held::show(self.volume_held, actual_vol, now, 0.5).round();
        let vol_id = ui.id("media-volume");
        let w = g.wedge;
        let vol_hit = Rect::new(w.x - 4.0, w.y - 6.0, w.w + 8.0, w.h + 12.0);
        let vr = ui.interact(
            vol_id,
            vol_hit,
            Sense {
                scroll: true,
                ..Sense::DRAG
            },
        );
        if vr.hovered || vr.pressed {
            ui.set_cursor(CursorIcon::Pointer);
        }
        let px = if vr.press_started {
            vr.press_pos.map(|p| p.0)
        } else if vr.pressed {
            pointer.map(|p| p.0)
        } else {
            None
        };
        let mut wanted = px.map(|x| f32::from(volume_at(x, w.x, w.w)));
        let notches = vr.scroll_lines.1 + vr.scroll_pixels.1 / SCROLL_PIXELS_PER_NOTCH;
        if notches != 0.0 {
            let step = if notches > 0.0 {
                SCROLL_STEP
            } else {
                -SCROLL_STEP
            };
            wanted = Some((vol + step as f32).clamp(0.0, 100.0));
        }
        if let Some(v) = wanted
            && (v - vol).abs() >= 0.5
        {
            player.dispatch(PlayerAction::SetVolume(v as u8));
            self.volume_held = Some(Held::new(v, now));
            vol = v;
        }
        let readout = volume_db_label(vol as u8);
        volume(ui.painter(), k, w, vol / 100.0, &readout);

        // The device icon is hidden for now (the quality pill opens the Audio window); the
        // queue icon keeps its place.
        let queue_c = (w.x - 34.0, g.cy);
        let queue = button(ui, "media-queue", queue_c, BUTTON_HALF, true);
        icon(
            ui.painter(),
            Icon::Queue,
            queue_c,
            17.0,
            lift(k, ink.text2, queue.hovered),
            HAIR,
        );
        out.queue_rect = Some(Rect::new(
            queue_c.0 - BUTTON_HALF,
            queue_c.1 - BUTTON_HALF,
            2.0 * BUTTON_HALF,
            2.0 * BUTTON_HALF,
        ));
        ui.tooltip(&queue, "Queue");
        if queue.clicked {
            out.open_queue = true;
        }

        // The pill (hidden when it does not fit) and its signal-path tooltip; a press opens or
        // closes the Audio window.
        let kind = signal::kind_of(diag.verdict.as_ref());
        let label = signal::pill_text(diag);
        let mut pill_tip = None;
        if let Some(label) = label {
            let pr = pill_rect(ui.painter(), k, &label, g.pill_right, g.cy);
            if pill_fits(&g, win_w, pr.w) && right_left(&g) > pr.right() {
                let id = ui.id("media-pill");
                let r = ui.interact(id, pr, Sense::CLICK_FOCUS);
                out.pill_rect = Some(pr);
                let hover = r.hovered || forced == Some(Hover::Pill);
                if r.hovered {
                    ui.set_cursor(CursorIcon::Pointer);
                }
                if r.clicked {
                    out.toggle_audio = true;
                }
                if r.focus_visible {
                    ui.painter().ring(pr, pr.h / 2.0, 1.5, k.hi, true);
                }
                draw_pill(ui.painter(), k, &label, kind, pr, hover);
                if hover {
                    pill_tip = Some(pr);
                }
            }
        }

        // Keep the ring moving while a track plays (the shell's cadence).
        if state.play_state == PlayState::Playing {
            ui.request_repaint_at(now + FRAME_INTERVAL);
        }
        let tip_alphas = [
            self.pill_tip.update(pill_tip.is_some(), now),
            self.ring_tip.update(tip.is_some(), now),
            self.like_tip.update(like_tip.is_some(), now),
        ];
        moving |=
            self.pill_tip.running(now) || self.ring_tip.running(now) || self.like_tip.running(now);
        if moving {
            ui.request_repaint_at(now + FADE_FRAME);
        }

        // The cards, last, above everything.
        if let Some(pr) = pill_tip {
            let rows = signal::rows(diag, state);
            let line = signal::verdict_line(diag, state);
            with_alpha(ui, tip_alphas[0], |ui| {
                signal::draw_card(
                    ui.painter(),
                    k,
                    win_w,
                    rect,
                    pr,
                    &rows,
                    &line,
                    signal::header_device(diag),
                );
            });
        }
        if let Some(tip) = tip {
            with_alpha(ui, tip_alphas[1], |ui| {
                draw_ring_tip(ui.painter(), k, win_w, rect, c, tip);
            });
        }
        if let Some(hx) = like_tip {
            with_alpha(ui, tip_alphas[2], |ui| {
                draw_like_tip(ui.painter(), k, win_w, rect, hx);
            });
        }
        ui.painter().on(LayerId::Popup);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ThemeId, theme};

    const C: (f32, f32) = (100.0, 100.0);

    #[test]
    fn r1_ring_fraction_at_the_clock_points() {
        let f = |dx: f32, dy: f32| ring_fraction(C, (C.0 + dx, C.1 + dy));
        assert!((f(0.0, -30.0) - 0.0).abs() < 1e-5, "12 o'clock");
        assert!((f(30.0, 0.0) - 0.25).abs() < 1e-5, "3 o'clock");
        assert!((f(0.0, 30.0) - 0.5).abs() < 1e-5, "6 o'clock");
        assert!((f(-30.0, 0.0) - 0.75).abs() < 1e-5, "9 o'clock");
        assert!(
            f(-0.001, -30.0) > 0.99,
            "just before 12 is the end of the turn"
        );
    }

    #[test]
    fn r2_seek_ms_clamps_to_the_track() {
        assert_eq!(seek_ms(0.5, 200_000), 100_000);
        assert_eq!(seek_ms(1.5, 200_000), 200_000);
        assert_eq!(seek_ms(-1.0, 200_000), 0);
        assert_eq!(seek_ms(0.0, 200_000), 0);
        assert_eq!(seek_ms(0.7, 0), 0);
        assert_eq!(seek_ms(1.0, u64::MAX), u32::MAX);
    }

    #[test]
    fn v1_volume_readout_follows_the_law() {
        assert_eq!(volume_db_label(100), "0 dB");
        assert_eq!(volume_db_label(50), "\u{2212}12 dB");
        assert_eq!(volume_db_label(0), "\u{2212}\u{221e} dB");
    }

    #[test]
    fn volume_follows_the_pointer_across_the_wedge() {
        assert_eq!(volume_at(0.0, 10.0, 100.0), 0);
        assert_eq!(volume_at(60.0, 10.0, 100.0), 50);
        assert_eq!(volume_at(500.0, 10.0, 100.0), 100);
    }

    #[test]
    fn zones_split_the_ring() {
        let r = 28.0;
        assert_eq!(zone_of(C, r, (C.0, C.1)), Some(Zone::Disc));
        assert_eq!(zone_of(C, r, (C.0 + 27.0, C.1)), Some(Zone::Annulus));
        assert_eq!(zone_of(C, r, (C.0, C.1 - 35.0)), Some(Zone::Annulus));
        assert_eq!(zone_of(C, r, (C.0 + 40.0, C.1)), None);
    }

    /// At every width and theme the left column, centre group and right group keep clear of each
    /// other (the pill counts as part of the right group when it is shown).
    #[test]
    fn groups_never_overlap_down_to_the_minimum() {
        for id in ThemeId::ALL {
            let k = Look::new(theme(id), None);
            for win_w in [
                760.0f32, 900.0, 999.0, 1000.0, 1100.0, 1280.0, 1440.0, 1920.0,
            ] {
                let rect = Rect::new(32.0, 400.0, win_w - 64.0, 76.0);
                let g = geo(rect, win_w, &k);
                let left_right = g.text_x + g.title_max + LIKE_SLOT;
                assert!(
                    g.centre_left - left_right >= 12.0 - 0.01,
                    "{id:?} @{win_w}: left {left_right} centre {}",
                    g.centre_left
                );
                assert!(
                    right_left(&g) - g.centre_right >= 12.0 - 0.01,
                    "{id:?} @{win_w}: centre {} right {}",
                    g.centre_right,
                    right_left(&g)
                );
                assert!(right_left(&g) > g.centre_right);
                assert!(g.wedge.right() <= rect.right() - 12.0 + 0.01);
            }
        }
    }

    #[test]
    fn pill_hides_when_it_would_overlap_the_centre_group() {
        let k = Look::new(theme(ThemeId::B), None);
        let g = geo(Rect::new(32.0, 400.0, 1376.0, 76.0), 1440.0, &k);
        assert!(!pill_fits(&g, 999.0, 10.0), "below 1000 px it hides first");
        assert!(pill_fits(&g, 1440.0, 150.0));
        assert!(!pill_fits(&g, 1440.0, 10_000.0));
    }
}
