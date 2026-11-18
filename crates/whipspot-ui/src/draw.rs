//! Drawing helpers shared by the native UI's views: text, discs, shadows, gradients, glass sheets,
//! LEDs, wells, knobs and dial ticks, all over a [`Painter`] and a resolved [`Look`].

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use lune_ui::color::Rgba;
use lune_ui::color::premul;
use lune_ui::paint::{Falloff, Painter, Rect, StrokeStyle};
use lune_ui::text::{Spec, Style};
use lune_ui::ui::WidgetId;

use crate::theme::Look;

pub(crate) type P<'a> = Painter<'a, WidgetId>;

/// What a recorded page box is: text, or a control or picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    Text,
    Control,
}

/// The boxes of what an artist or album page draws (the demo's layout check reads them).
#[derive(Default)]
struct PageBoxes {
    enabled: bool,
    active: bool,
    boxes: Vec<(BoxKind, Rect)>,
}

thread_local! {
    static PAGE: std::cell::RefCell<PageBoxes> = std::cell::RefCell::new(PageBoxes::default());
}

/// Starts or stops recording the boxes a page draws (off by default: drawing pays one flag test).
pub(crate) fn page_boxes_enable(on: bool) {
    PAGE.with(|p| {
        let mut p = p.borrow_mut();
        p.enabled = on;
        p.active = false;
        p.boxes.clear();
    });
}

/// A frame starts: the recorded boxes of the last one go.
pub(crate) fn page_boxes_frame() {
    PAGE.with(|p| {
        let mut p = p.borrow_mut();
        p.active = false;
        p.boxes.clear();
    });
}

/// A page starts (`true`) or stops drawing: only what it draws is recorded.
pub(crate) fn page_boxes_scope(on: bool) {
    PAGE.with(|p| {
        let mut p = p.borrow_mut();
        p.active = on && p.enabled;
    });
}

/// Records a box while a page is drawing.
pub(crate) fn page_box(kind: BoxKind, r: Rect) {
    PAGE.with(|p| {
        let mut p = p.borrow_mut();
        if p.active {
            p.boxes.push((kind, r));
        }
    });
}

/// The boxes recorded in the last frame.
pub(crate) fn page_boxes() -> Vec<(BoxKind, Rect)> {
    PAGE.with(|p| p.borrow().boxes.clone())
}

/// The measuring colour (any opaque colour; widths do not depend on it).
const INK: Rgba = [0xFF, 0xFF, 0xFF, 0xFF];

/// `c` at alpha `a`.
pub(crate) fn alpha(c: Rgba, a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

/// Draws `s` with its top-left at `x`, `y`, cut to `max_w`; returns its width.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    medium: bool,
    x: f32,
    y: f32,
    max_w: f32,
) -> f32 {
    let mut style = Style::ui(size, color);
    if medium {
        style = style.medium();
    }
    let spec = Spec::plain(s, style);
    let spec = if max_w > 0.0 {
        p.elide(&spec, max_w)
    } else {
        spec
    };
    let w = p.text(&spec, x, y).0;
    page_box(BoxKind::Text, Rect::new(x, y, w, size * 1.25));
    w
}

/// The idle view's text shadow: copies of the text in black, each as `(dx, dy, share of the full
/// alpha)`; together a soft, low halo under the text with a drop of about 1.5 px.
const TEXT_SHADOW: [(f32, f32, f32); 4] = [
    (0.0, 1.5, 0.45),
    (1.5, 1.5, 0.30),
    (-1.0, 1.5, 0.30),
    (0.0, 3.0, 0.22),
];
/// The shadow's alpha at full text opacity.
const TEXT_SHADOW_ALPHA: f32 = 0.6;

/// The shadow copies of text coloured `color`: offsets and black colours, their alpha following
/// the text's own so a dimmed line casts a weaker shadow.
pub(crate) fn text_shadow(color: Rgba) -> impl Iterator<Item = (f32, f32, Rgba)> {
    let a = color[3] as f32 / 255.0 * TEXT_SHADOW_ALPHA;
    TEXT_SHADOW
        .into_iter()
        .map(move |(dx, dy, k)| (dx, dy, [0, 0, 0, (255.0 * a * k).round() as u8]))
}

/// Runs `draw(p, color, dx, dy)` for the text's shadow copies and then for the text itself
/// (offset 0, 0) with `color`; returns what the last call returned. One helper, so every idle
/// text casts the same shadow (the idle view only; the browse view's text has none).
pub(crate) fn with_text_shadow<T>(
    p: &mut P<'_>,
    color: Rgba,
    mut draw: impl FnMut(&mut P<'_>, Rgba, f32, f32) -> T,
) -> T {
    for (dx, dy, c) in text_shadow(color) {
        draw(p, c, dx, dy);
    }
    draw(p, color, 0.0, 0.0)
}

/// [`text`] with the idle view's shadow under it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn text_shadowed(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    medium: bool,
    x: f32,
    y: f32,
    max_w: f32,
) -> f32 {
    with_text_shadow(p, color, |p, c, dx, dy| {
        text(p, s, size, c, medium, x + dx, y + dy, max_w)
    })
}

/// The width of `s`.
pub(crate) fn width(p: &mut P<'_>, s: &str, size: f32, medium: bool) -> f32 {
    let mut style = Style::ui(size, INK);
    if medium {
        style = style.medium();
    }
    p.measure(&Spec::plain(s, style)).0
}

/// Draws `s` with its top-right at `right`, `y`.
pub(crate) fn text_r(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    medium: bool,
    right: f32,
    y: f32,
) {
    let w = width(p, s, size, medium);
    text(p, s, size, color, medium, right - w, y, 0.0);
}

/// Draws `s` centred on `cx`.
pub(crate) fn text_c(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    medium: bool,
    cx: f32,
    y: f32,
) {
    let w = width(p, s, size, medium);
    text(p, s, size, color, medium, cx - w / 2.0, y, 0.0);
}

/// A disc of diameter `d` centred on `c`.
pub(crate) fn disc(p: &mut P<'_>, c: (f32, f32), d: f32, color: Rgba) {
    p.fill(
        Rect::new(c.0 - d / 2.0, c.1 - d / 2.0, d, d),
        d / 2.0,
        color,
    );
}

/// A soft drop shadow under `r` (draw before what casts it).
pub(crate) fn shadow(p: &mut P<'_>, r: Rect, radius: f32, spread: f32, strength: f32) {
    let steps = 10;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let k = spread * t;
        let a = (strength * (1.0 - t).powf(1.5) * 34.0)
            .round()
            .clamp(0.0, 255.0) as u8;
        p.fill(
            Rect::new(
                r.x - k,
                r.y - k + spread * 0.35,
                r.w + 2.0 * k,
                r.h + 2.0 * k,
            ),
            radius + k,
            [0, 0, 0, a],
        );
    }
}

/// A vertical gradient over `r`, `top` to `bottom`: one [`Painter::linear`], evaluated per pixel
/// and dithered by luneui, so a long dark ramp shows no bands. Below the layer's text, like a fill.
pub(crate) fn vgradient(p: &mut P<'_>, r: Rect, top: Rgba, bottom: Rgba) {
    p.linear(r, 0.0, top, bottom, FRAC_PI_2);
}

/// The edge `i` of `n` equal strips over `len` px from `start`: the outer edges (0 and `n`) are
/// exact, the inner ones are snapped to the device pixel at the frame's scale so neighbouring
/// strips share one edge and anti-aliasing leaves no seam between them.
pub(crate) fn strip_edge(p: &P<'_>, start: f32, len: f32, i: usize, n: usize) -> f32 {
    if i == 0 {
        start
    } else if i >= n {
        start + len
    } else {
        let v = start + len * i as f32 / n as f32;
        (v * p.s).round() / p.s
    }
}

/// A horizontal gradient over `r`, `left` to `right`: one [`Painter::linear`] (see [`vgradient`]).
pub(crate) fn hgradient(p: &mut P<'_>, r: Rect, left: Rgba, right: Rgba) {
    p.linear(r, 0.0, left, right, 0.0);
}

/// How far a [`glow`]'s blurred shape is grown past its caster, and its Gaussian sigma, as shares
/// of the spread: fitted to the old stack of 14 translucent rounded rects, whose alpha is flat
/// under the (shifted) caster and falls off as `1 - exp(-0.55 * strength * (1 - x)^2.6)` at
/// `x * spread` outside it, within 0.035 alpha over that whole profile.
const GLOW_GROW: f32 = 0.29;
const GLOW_SIGMA: f32 = 0.19;

/// A soft coloured glow under `r` (draw before what casts it): one [`Painter::shadow`] in `color`
/// of `r` grown by a share of `spread` and moved down by a quarter of it, whose blur fades out
/// over about `spread`. The colour's alpha is the old stack's flat value, `1 - exp(-0.52 * strength)`.
pub(crate) fn glow(p: &mut P<'_>, r: Rect, radius: f32, spread: f32, color: Rgba, strength: f32) {
    let a = ((1.0 - (-0.52 * strength.max(0.0)).exp()) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    let g = spread * GLOW_GROW;
    let shape = Rect::new(
        r.x - g,
        r.y - g + spread * 0.25,
        r.w + 2.0 * g,
        r.h + 2.0 * g,
    );
    p.shadow(shape, radius + g, spread * GLOW_SIGMA, alpha(color, a));
}

/// A soft round light of `color` centred on `c`, `radius` wide: one [`Painter::radial`] fading
/// linearly to nothing at the rim. The centre's alpha is what the old stack of 28 discs
/// composited to there, `1 - exp(-0.9 * strength)`.
pub(crate) fn blob(p: &mut P<'_>, c: (f32, f32), radius: f32, color: Rgba, strength: f32) {
    let centre = 1.0 - (-0.9 * strength.max(0.0)).exp();
    let a = (centre * 255.0).round().clamp(0.0, 255.0) as u8;
    p.radial(
        [c.0, c.1],
        [radius, radius],
        alpha(color, a),
        alpha(color, 0),
        Falloff::Linear,
    );
}

/// [`Painter::backdrop_with`] for `r`, only where the painter's clip lets it show (M9-S7-1).
/// luneui's backdrop request has no clip of its own, so a card scrolled out of its scroll area
/// would still blur the scene over whatever lies outside (the caption band). A card cut by the
/// clip asks for its visible part with square corners (the cut edge must be straight; the frost
/// and the rim drawn over it are clipped by luneui and keep the card's own shape); one fully
/// outside asks for nothing. Until luneui clips backdrops itself (notes.md: BackdropRequest
/// clip), this is the fix.
fn backdrop_in_clip(p: &mut P<'_>, r: Rect, radius: f32, sigma: f32, saturation: f32) {
    const EPS: f32 = 0.01;
    let Some(clip) = p.clip_rect() else {
        p.backdrop_with(r, radius, sigma, saturation);
        return;
    };
    let visible = r.intersect(&clip);
    if visible.w <= 0.0 || visible.h <= 0.0 {
        return;
    }
    let cut = visible.w < r.w - EPS || visible.h < r.h - EPS;
    if cut {
        p.backdrop_with(visible, 0.0, sigma, saturation);
    } else {
        p.backdrop_with(r, radius, sigma, saturation);
    }
}

/// A glass sheet in the look's surface: the frosted scene below, the tint at `tint_alpha`, the top
/// sheen, brushed streaks (on a faceplate, where `face` and the theme has them), a lit bevel, and
/// the rim.
pub(crate) fn glass(p: &mut P<'_>, k: &Look, r: Rect, radius: f32, tint_alpha: u8, face: bool) {
    let s = k.surface();
    backdrop_in_clip(p, r, radius, s.sigma, 1.25);
    p.frost(r, radius, premul(alpha(s.tint, tint_alpha)), true);
    p.push_clip_rounded(r, radius);
    if s.sheen > 0 {
        let lit = alpha(k.ink.light, s.sheen);
        vgradient(
            p,
            Rect::new(r.x, r.y, r.w, (r.h * 0.5).min(110.0)),
            lit,
            alpha(lit, 0),
        );
    }
    if s.brushed && face {
        let mut y = r.y + 1.0;
        let mut row = 0u32;
        while y < r.bottom() - 1.0 {
            for i in 0..3u32 {
                let a = hash32(row * 3 + i, 11);
                let b = hash32(row * 3 + i, 29);
                let x = r.x + a * r.w * 0.85;
                let w = (0.15 + b * 0.6) * r.w;
                let col = if hash32(row * 3 + i, 47) > 0.5 {
                    [0xFF, 0xF2, 0xE4, 3 + (b * 5.0) as u8]
                } else {
                    [0x00, 0x00, 0x00, 4 + (a * 6.0) as u8]
                };
                p.fill(Rect::new(x, y, w.min(r.right() - x), 1.0), 0.0, col);
            }
            y += 1.0;
            row += 1;
        }
    }
    p.pop_clip();
    p.ring(r, radius, 1.0, s.rim, false);
    if s.bevel {
        let inset = radius * 0.9;
        let half = (r.w - 2.0 * inset) / 2.0;
        let lit = alpha(k.ink.light, 0x40);
        hgradient(
            p,
            Rect::new(r.x + inset, r.y + 0.5, half, 1.0),
            alpha(lit, 0),
            lit,
        );
        hgradient(
            p,
            Rect::new(r.x + inset + half, r.y + 0.5, half, 1.0),
            lit,
            alpha(lit, 0),
        );
    }
}

/// A plain outlined pill or box, no frost (inside a glass sheet).
pub(crate) fn outline(p: &mut P<'_>, r: Rect, radius: f32, fill: Rgba, rim: Rgba) {
    p.fill(r, radius, fill);
    p.ring(r, radius, 1.0, rim, false);
}

/// A deterministic hash of `i` to 0..1.
pub(crate) fn hash32(i: u32, seed: u32) -> f32 {
    let mut h = i.wrapping_mul(2_654_435_761) ^ seed.wrapping_mul(40_503);
    h = (h ^ (h >> 15)).wrapping_mul(2_246_822_519);
    h ^= h >> 13;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Draws `s` in the monospaced face at `x`, `y`; returns its width.
pub(crate) fn mono(p: &mut P<'_>, s: &str, size: f32, color: Rgba, x: f32, y: f32) -> f32 {
    let spec = Spec::plain(s, Style::mono(size, size * 1.3, color));
    p.text(&spec, x, y).0
}

/// The width of mono `s`.
pub(crate) fn mono_width(p: &mut P<'_>, s: &str, size: f32) -> f32 {
    p.measure(&Spec::plain(s, Style::mono(size, size * 1.3, INK)))
        .0
}

/// The width of `s` set as tracked caps (see [`caps`]).
pub(crate) fn caps_width(p: &mut P<'_>, s: &str, size: f32, track: f32) -> f32 {
    let mut w = 0.0;
    let mut n = 0;
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        w += width(p, ch.encode_utf8(&mut buf), size, true);
        n += 1;
    }
    w + track * (n.max(1) - 1) as f32
}

/// `s` letter-spaced by `track` px (the toolkit has no tracking, so each letter is placed
/// alone); returns its width.
pub(crate) fn caps(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    x: f32,
    y: f32,
    track: f32,
) -> f32 {
    let mut cx = x;
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        cx += text(p, ch.encode_utf8(&mut buf), size, color, true, cx, y, 0.0) + track;
    }
    cx - x - track
}

/// [`caps`] with the idle view's shadow under it.
pub(crate) fn caps_shadowed(
    p: &mut P<'_>,
    s: &str,
    size: f32,
    color: Rgba,
    x: f32,
    y: f32,
    track: f32,
) -> f32 {
    with_text_shadow(p, color, |p, c, dx, dy| {
        caps(p, s, size, c, x + dx, y + dy, track)
    })
}

/// An LED: a lit dot with its halo, or a dark one.
pub(crate) fn led(p: &mut P<'_>, c: (f32, f32), d: f32, color: Rgba, hi: Rgba, lit: bool) {
    if lit {
        disc(p, c, d * 3.2, alpha(color, 0x18));
        disc(p, c, d * 2.0, alpha(color, 0x30));
        disc(p, c, d, color);
        disc(
            p,
            (c.0 - d * 0.15, c.1 - d * 0.15),
            d * 0.4,
            alpha(hi, 0xC0),
        );
    } else {
        disc(p, c, d, [0x3A, 0x32, 0x2A, 0xFF]);
    }
}

/// An inset well (a display window cut into a faceplate); `lip` adds the lit lower edge and the
/// dark rim.
pub(crate) fn well(p: &mut P<'_>, r: Rect, radius: f32, lip: bool) {
    p.fill(r, radius, [0x06, 0x05, 0x04, 0xB0]);
    p.push_clip_rounded(r, radius);
    vgradient(
        p,
        Rect::new(r.x, r.y, r.w, r.h * 0.45),
        [0x00, 0x00, 0x00, 0x50],
        [0x00, 0x00, 0x00, 0x00],
    );
    p.pop_clip();
    if lip {
        p.ring(r, radius, 1.0, [0x00, 0x00, 0x00, 0x70], false);
        p.fill(
            Rect::new(r.x + radius, r.bottom() - 1.0, r.w - 2.0 * radius, 1.0),
            0.0,
            [0xFF, 0xEE, 0xDC, 0x22],
        );
    }
}

/// A machined knob face of diameter `d` at `c`: a warm metal disc lit from above.
pub(crate) fn knob(p: &mut P<'_>, c: (f32, f32), d: f32) {
    let r = Rect::new(c.0 - d / 2.0, c.1 - d / 2.0, d, d);
    shadow(p, r, d / 2.0, d * 0.16, 1.0);
    p.push_clip_rounded(r, d / 2.0);
    vgradient(p, r, [0x4A, 0x40, 0x36, 0xFF], [0x18, 0x14, 0x11, 0xFF]);
    p.pop_clip();
    let f = r.inset(3.0, 3.0);
    p.push_clip_rounded(f, f.w / 2.0);
    vgradient(p, f, [0x24, 0x1F, 0x1A, 0xFF], [0x36, 0x2E, 0x27, 0xFF]);
    p.pop_clip();
    p.ring(r, d / 2.0, 1.0, [0xFF, 0xEE, 0xDC, 0x30], false);
}

/// Tick marks round `c` between radii `r0` and `r1` (a major every `every`), lit in the look's
/// ramp up to `lit` (fraction of the turn from 12 o'clock).
#[allow(clippy::too_many_arguments)]
pub(crate) fn dial_ticks(
    p: &mut P<'_>,
    k: &Look,
    c: (f32, f32),
    r0: f32,
    r1: f32,
    n: usize,
    every: usize,
    lit: f32,
) {
    let dim = alpha(k.ink.light, 0x30);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let a = -PI / 2.0 + t * TAU;
        let major = i % every == 0;
        let outer = if major { r1 } else { r0 + (r1 - r0) * 0.55 };
        let (s, co) = a.sin_cos();
        let pts = [
            [c.0 + co * r0, c.1 + s * r0],
            [c.0 + co * outer, c.1 + s * outer],
        ];
        let col = if t <= lit { alpha(k.at(t), 0xE0) } else { dim };
        let w = if major { 1.4 } else { 1.0 };
        p.stroke(&pts, false, StrokeStyle::solid(col, w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_shadow_is_black_and_follows_the_texts_alpha() {
        let full: Vec<_> = text_shadow([0xFF, 0xFF, 0xFF, 0xFF]).collect();
        assert_eq!(full.len(), TEXT_SHADOW.len());
        assert!(full.iter().all(|(_, _, c)| c[..3] == [0, 0, 0]));
        let top = full.iter().map(|(_, _, c)| c[3]).max().unwrap();
        // The strongest copy is 45 % of 60 % of full alpha: about 69 of 255.
        assert!((60..=80).contains(&top), "{top}");
        let half: u8 = text_shadow([0xFF, 0xFF, 0xFF, 0x80])
            .map(|(_, _, c)| c[3])
            .max()
            .unwrap();
        assert!(half < top && half >= top / 2 - 2, "{half} vs {top}");
        assert!(text_shadow([0xFF, 0xFF, 0xFF, 0]).all(|(_, _, c)| c[3] == 0));
    }
}
