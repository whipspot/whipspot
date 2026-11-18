//! Artist and album links in running text.
//!
//! At rest a link is the text it replaces: the same colour, weight and glyph positions. On hover
//! or keyboard focus its letters take the theme's accent gradient, a -> b -> c -> a, moving left to
//! right once every [`LOOP_SECS`]. The toolkit has no gradient text fill, so a link is always drawn
//! as one run per grapheme (D4), at rest too, and only the colours change on hover: the geometry
//! never does.
//!
//! [`line`] is the one entry point: it draws a display string whose names may carry an
//! [`EntityRef`] each, interacts with every name that does, and queues the clicked ref for
//! [`take_clicks`]. Names without a ref, and the text between names, are plain text exactly as
//! [`crate::draw::text`] draws them.

use std::cell::{Cell, RefCell};
use std::hash::Hash;
use std::time::Duration;

use lune_ui::color::Rgba;
use lune_ui::input::{CursorIcon, NamedKey};
use lune_ui::paint::Rect;
use lune_ui::text::{Spec, Style, Unit};
use lune_ui::ui::{Sense, Ui};
use whipspot_core::provider::EntityRef;

use crate::anim::{self, Pace};
use crate::draw::{P, text, text_shadow, text_shadowed};
use crate::theme::Look;

/// Seconds for one pass of the gradient over a link.
pub(crate) const LOOP_SECS: f32 = 6.0;
/// How far a link's gradient reaches, in link widths: one loop of three stops spans this many
/// widths of the link (the mockup's `background-size: 300%`).
const SPAN: f32 = 3.0;
/// The gap between repaints while a link is animating (about 30 fps).
const REPAINT: Duration = Duration::from_millis(33);
/// The keyboard-focus outline: its distance from the text box, its width and corner radius.
const OUTLINE_GAP: f32 = 3.0;
const OUTLINE_W: f32 = 2.0;
const OUTLINE_RADIUS: f32 = 4.0;
/// The mark an elided text ends in (`lune_ui::text::elide`).
const ELLIPSIS_LEN: usize = '\u{2026}'.len_utf8();

/// How the demo forces a link: the `nth` link drawn in a frame (counted in drawing order) is
/// hovered or keyboard-focused, with its gradient at `phase` (0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ForcedLink {
    pub nth: usize,
    pub focus: bool,
    pub phase: f32,
}

thread_local! {
    static FORCED: Cell<Option<ForcedLink>> = const { Cell::new(None) };
    static DRAWN: Cell<usize> = const { Cell::new(0) };
    static CLICKS: RefCell<Vec<EntityRef>> = const { RefCell::new(Vec::new()) };
    static RECTS: RefCell<Vec<Rect>> = const { RefCell::new(Vec::new()) };
    static TRACE: RefCell<Option<Vec<LinkGlyph>>> = const { RefCell::new(None) };
}

/// One glyph of a lit link as it was drawn: the demo prints these to show the gradient.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkGlyph {
    /// The grapheme.
    pub text: String,
    /// Its centre, x in logical px of the window.
    pub x: f32,
    /// The colour it was drawn in.
    pub color: Rgba,
    /// The gradient's phase (0..1 of a loop).
    pub phase: f32,
    /// The stops the gradient runs through (a, b, c).
    pub stops: [Rgba; 3],
}

/// Starts (or stops) recording the glyphs of lit links, the last frame's only.
pub(crate) fn trace(on: bool) {
    TRACE.with(|t| *t.borrow_mut() = on.then(Vec::new));
}

/// The glyphs of the lit links of the last frame, when tracing.
pub(crate) fn take_trace() -> Vec<LinkGlyph> {
    TRACE.with(|t| {
        t.borrow_mut()
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    })
}

/// Starts a frame: the link count restarts and the demo's forced link applies.
pub(crate) fn begin_frame(forced: Option<ForcedLink>) {
    FORCED.with(|f| f.set(forced));
    DRAWN.with(|d| d.set(0));
    RECTS.with(|r| r.borrow_mut().clear());
    TRACE.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t.clear();
        }
    });
}

/// Whether `pos` is over a link drawn this frame (a press there is the link's, not its row's).
pub(crate) fn over(pos: (f32, f32)) -> bool {
    RECTS.with(|r| r.borrow().iter().any(|r| r.contains(pos)))
}

/// The boxes (x, y, w, h in logical px) of the links drawn in the last frame.
pub(crate) fn boxes() -> Vec<[f32; 4]> {
    RECTS.with(|r| r.borrow().iter().map(|r| [r.x, r.y, r.w, r.h]).collect())
}

/// The refs clicked (or activated by Enter or Space) since the last call, in order.
pub(crate) fn take_clicks() -> Vec<EntityRef> {
    CLICKS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// What a line's text looks like at rest.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Face {
    pub size: f32,
    pub medium: bool,
    pub color: Rgba,
    /// The idle view's text shadow under the text ([`crate::draw::text_shadowed`]).
    pub shadow: bool,
}

impl Face {
    fn style(self) -> Style {
        let s = Style::ui(self.size, self.color);
        if self.medium { s.medium() } else { s }
    }
}

/// Where a line goes: its top-left corner (`centre`: its top middle) and the width it is cut to
/// (0 for none).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Place {
    pub x: f32,
    pub y: f32,
    pub centre: bool,
    pub max_w: f32,
}

/// A part of a display string: plain text, or a name that carries `refs[i]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Plain(&'a str),
    Link(&'a str, usize),
}

/// True when `c` continues a name (so a match inside a longer word is not a link).
fn wordy(c: char) -> bool {
    c.is_alphanumeric()
}

/// Splits `s` into plain text and the names of `refs`, each found in order as a whole word; a ref
/// whose name is not in the rest of `s` (or is empty) is left out and its text stays plain.
fn pieces<'a>(s: &'a str, refs: &[EntityRef]) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    let mut at = 0;
    for (i, r) in refs.iter().enumerate() {
        let name = r.name.as_str();
        if name.is_empty() {
            continue;
        }
        let mut from = at;
        let found = loop {
            let Some(k) = s[from..].find(name) else {
                break None;
            };
            let (a, b) = (from + k, from + k + name.len());
            let before = s[..a].chars().next_back().is_none_or(|c| !wordy(c));
            let after = s[b..].chars().next().is_none_or(|c| !wordy(c));
            if before && after {
                break Some((a, b));
            }
            from = a + s[a..].chars().next().map_or(1, char::len_utf8);
        };
        if let Some((a, b)) = found {
            if a > at {
                out.push(Piece::Plain(&s[at..a]));
            }
            out.push(Piece::Link(&s[a..b], i));
            at = b;
        }
    }
    if at < s.len() {
        out.push(Piece::Plain(&s[at..]));
    }
    out
}

/// `text` cut into the pieces between the sorted boundaries `bounds` (0 and the end included).
fn split<'a>(text: &'a str, bounds: &[usize]) -> Vec<&'a str> {
    bounds
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| &text[w[0]..w[1]])
        .collect()
}

/// The accent gradient at `u`: stops a, b, c and back to a, evenly spaced, seamless (`u` and
/// `u + 1` give the same colour). Linear in sRGB, which is plenty at text size.
pub(crate) fn grad(stops: [Rgba; 3], u: f32) -> Rgba {
    let u = u.rem_euclid(1.0) * 3.0;
    let i = (u.floor() as usize).min(2);
    let (a, b) = (stops[i], stops[(i + 1) % 3]);
    let t = u - i as f32;
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), m(a[3], b[3])]
}

/// The colour of a glyph whose centre is `dx` px right of the link's left edge, in a link `w` px
/// wide, at `phase` (0..1 of a loop): the gradient spans [`SPAN`] link widths and its pattern
/// moves right as the phase grows.
pub(crate) fn glyph_colour(stops: [Rgba; 3], dx: f32, w: f32, phase: f32) -> Rgba {
    grad(stops, dx / (SPAN * w.max(1.0)) - phase)
}

/// The phase (0..1) of the loop at `now`.
pub(crate) fn phase_at(now: Duration) -> f32 {
    (now.as_secs_f32() / LOOP_SECS).rem_euclid(1.0)
}

/// One link laid out: which ref, its text's byte range in the (possibly elided) line and its box.
struct Laid {
    piece: usize,
    bytes: (usize, usize),
    rect: Rect,
}

/// Draws `s` (a display string whose names `refs` carry) with its links; returns the line's width.
/// `key` names the line for the toolkit's ids (stable across frames, unique among lines).
#[allow(clippy::too_many_arguments)]
pub(crate) fn line(
    ui: &mut Ui<'_>,
    key: impl Hash,
    look: &Look,
    s: &str,
    refs: &[EntityRef],
    face: Face,
    at: Place,
) -> f32 {
    let parts = pieces(s, refs);
    if !parts.iter().any(|p| matches!(p, Piece::Link(..))) {
        return plain(ui.painter(), s, face, at);
    }
    let style = face.style();

    // The spec: one run per grapheme of a link, one run per stretch of plain text.
    let (spec, x0, width, laid) = {
        let p = ui.painter();
        let mut runs: Vec<(String, Style)> = Vec::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::new();
        let mut off = 0;
        for part in &parts {
            match *part {
                Piece::Plain(t) => {
                    runs.push((t.to_owned(), style));
                    off += t.len();
                }
                Piece::Link(t, i) => {
                    let start = off;
                    let bounds = p.segments(t, Unit::Grapheme);
                    for g in split(t, &bounds) {
                        runs.push((g.to_owned(), style));
                    }
                    off += t.len();
                    spans.push((i, start, off));
                }
            }
        }
        let full = Spec { runs, wrap: None };
        let spec = if at.max_w > 0.0 {
            p.elide(&full, at.max_w)
        } else {
            full
        };
        let width = p.measure(&spec).0;
        let x0 = if at.centre { at.x - width / 2.0 } else { at.x };
        // What survives an elision: its text is a prefix of the line plus the ellipsis.
        let total = s_len(&parts);
        let joined: usize = spec.runs.iter().map(|(t, _)| t.len()).sum();
        let elided = joined != total;
        let kept = if elided {
            joined.saturating_sub(ELLIPSIS_LEN)
        } else {
            joined
        };
        let mut laid = Vec::new();
        for (i, start, end) in spans {
            if elided && start >= kept {
                continue;
            }
            let end = if elided && end > kept { joined } else { end };
            let (xa, xb) = (p.caret(&spec, start).x, p.caret(&spec, end).x);
            laid.push(Laid {
                piece: i,
                bytes: (start, end),
                rect: Rect::new(x0 + xa, at.y, (xb - xa).max(0.0), style.line),
            });
        }
        (spec, x0, width, laid)
    };

    // Interaction, then which link (if any) is lit.
    let now = ui.time();
    let forced = FORCED.with(Cell::get);
    let mut lit: Option<(usize, bool, Option<f32>)> = None;
    let mut clicked = None;
    for (n, l) in laid.iter().enumerate() {
        let id = ui.id((&key, "link", n));
        let r = ui.interact(id, l.rect, Sense::CLICK_FOCUS);
        // The registered rect: where the link is on the window, inside a scroll area too.
        RECTS.with(|rs| rs.borrow_mut().push(r.rect));
        let nth = DRAWN.with(|d| {
            let v = d.get();
            d.set(v + 1);
            v
        });
        let force = forced.filter(|f| f.nth == nth);
        let focus = r.focus_visible || force.is_some_and(|f| f.focus);
        let hover = r.hovered || force.is_some();
        if r.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        if r.clicked || ui.consume_key(id, NamedKey::Enter) || ui.consume_key(id, NamedKey::Space) {
            clicked = Some(refs[l.piece].clone());
        }
        if (hover || focus) && lit.is_none() {
            lit = Some((n, focus, force.map(|f| f.phase)));
        }
    }
    if let Some(c) = clicked {
        CLICKS.with(|q| q.borrow_mut().push(c));
    }

    // Draw: the rest-coloured spec, with the lit link's runs recoloured.
    let mut spec = spec;
    let mut outline = None;
    if let Some((n, focus, forced_phase)) = lit {
        let l = &laid[n];
        let stops = look.link_stops();
        let animated = anim::pace() == Pace::Full;
        let phase = match forced_phase {
            Some(ph) => ph,
            None if animated => phase_at(now),
            None => 0.0,
        };
        if focus {
            outline = Some((l.rect, stops[1]));
        }
        recolour(ui.painter(), &mut spec, l, x0, stops, phase);
        if animated && forced_phase.is_none() {
            ui.request_repaint_at(now + REPAINT);
        }
    }
    let p = ui.painter();
    if let Some((r, c)) = outline {
        let g = OUTLINE_GAP + OUTLINE_W;
        p.ring(
            Rect::new(r.x - g, r.y - g, r.w + 2.0 * g, r.h + 2.0 * g),
            OUTLINE_RADIUS + g,
            OUTLINE_W,
            c,
            false,
        );
    }
    if face.shadow {
        for (dx, dy, c) in text_shadow(face.color) {
            let mut under = spec.clone();
            for (_, st) in &mut under.runs {
                st.color = c;
            }
            p.text(&under, x0 + dx, at.y + dy);
        }
    }
    crate::draw::page_box(
        crate::draw::BoxKind::Text,
        Rect::new(x0, at.y, width, style.line),
    );
    p.text(&spec, x0, at.y);
    width
}

/// Colours the runs of `l` in `spec` (drawn from `x0`) by the gradient at `phase`.
fn recolour(p: &mut P<'_>, spec: &mut Spec, l: &Laid, x0: f32, stops: [Rgba; 3], phase: f32) {
    let left = l.rect.x - x0;
    let mut off = 0;
    for i in 0..spec.runs.len() {
        let len = spec.runs[i].0.len();
        if off >= l.bytes.0 && off < l.bytes.1 {
            let (xa, xb) = (p.caret(spec, off).x, p.caret(spec, off + len).x);
            let centre = (xa + xb) / 2.0 - left;
            let colour = glyph_colour(stops, centre, l.rect.w, phase);
            spec.runs[i].1.color = colour;
            TRACE.with(|t| {
                if let Some(t) = t.borrow_mut().as_mut() {
                    t.push(LinkGlyph {
                        text: spec.runs[i].0.clone(),
                        x: x0 + (xa + xb) / 2.0,
                        color: colour,
                        phase,
                        stops,
                    });
                }
            });
        }
        off += len;
    }
}

/// The line's length in bytes.
fn s_len(parts: &[Piece<'_>]) -> usize {
    parts
        .iter()
        .map(|p| match p {
            Piece::Plain(t) | Piece::Link(t, _) => t.len(),
        })
        .sum()
}

/// Draws plain text as [`crate::draw::text`] does (a centred one by the width it takes elided).
fn plain(p: &mut P<'_>, s: &str, face: Face, at: Place) -> f32 {
    let style = face.style();
    let spec = Spec::plain(s, style);
    let spec = if at.max_w > 0.0 {
        p.elide(&spec, at.max_w)
    } else {
        spec
    };
    let w = p.measure(&spec).0;
    let x = if at.centre { at.x - w / 2.0 } else { at.x };
    if face.shadow {
        text_shadowed(p, s, face.size, face.color, face.medium, x, at.y, at.max_w);
    } else {
        text(p, s, face.size, face.color, face.medium, x, at.y, at.max_w);
    }
    w
}

#[cfg(test)]
mod tests {
    use lune_ui::testing::FakeText;
    use lune_ui::text::TextSystem;

    use super::*;

    const STOPS: [Rgba; 3] = [
        [0x2F, 0xD3, 0xC0, 0xFF],
        [0x8A, 0xF0, 0xE0, 0xFF],
        [0x4A, 0xA8, 0xFF, 0xFF],
    ];

    fn eref(name: &str) -> EntityRef {
        EntityRef {
            uri: format!("demo:artist:{name}"),
            name: name.into(),
        }
    }

    fn graphemes(s: &str) -> Vec<&str> {
        split(s, &FakeText.segments(s, Unit::Grapheme))
    }

    #[test]
    fn link_grad_hits_the_stops_and_wraps_seamlessly() {
        assert_eq!(grad(STOPS, 0.0), STOPS[0]);
        assert_eq!(grad(STOPS, 1.0 / 3.0), STOPS[1]);
        assert_eq!(grad(STOPS, 2.0 / 3.0), STOPS[2]);
        for i in 0..=40 {
            let u = i as f32 / 40.0;
            assert_eq!(grad(STOPS, u), grad(STOPS, u + 1.0), "u {u}");
            assert_eq!(grad(STOPS, u), grad(STOPS, u - 3.0), "u {u}");
        }
        // The loop closes: just before the end the colour is next to stop a again.
        let end = grad(STOPS, 0.999);
        for ch in 0..3 {
            assert!(end[ch].abs_diff(STOPS[0][ch]) <= 2, "{end:?}");
        }
    }

    #[test]
    fn link_grad_moves_between_neighbouring_letters_by_a_small_step() {
        // A 100 px link of 10 px letters: no jump between neighbours beyond a third of a stop.
        let mut prev = glyph_colour(STOPS, 5.0, 100.0, 0.0);
        for i in 1..10 {
            let c = glyph_colour(STOPS, 5.0 + 10.0 * i as f32, 100.0, 0.0);
            for ch in 0..3 {
                assert!(c[ch].abs_diff(prev[ch]) <= 40, "{prev:?} -> {c:?}");
            }
            prev = c;
        }
    }

    #[test]
    fn link_direction_a_colour_moves_right_as_the_phase_grows() {
        let (w, x) = (120.0_f32, 30.0_f32);
        let (t1, t2) = (0.10_f32, 0.15_f32);
        let at_t1 = glyph_colour(STOPS, x, w, t1);
        // The same colour at the later phase sits to the right, by the phase step in spans.
        let x2 = x + (t2 - t1) * 3.0 * w;
        let close = |a: Rgba, b: Rgba| (0..3).all(|c| a[c].abs_diff(b[c]) <= 1);
        assert!(close(glyph_colour(STOPS, x2, w, t2), at_t1));
        assert!(x2 > x);
        // And it is not where it was.
        assert!(!close(glyph_colour(STOPS, x, w, t2), at_t1));
    }

    #[test]
    fn link_phase_loops_every_six_seconds() {
        assert_eq!(phase_at(Duration::ZERO), 0.0);
        assert!((phase_at(Duration::from_secs(3)) - 0.5).abs() < 1e-6);
        assert!(phase_at(Duration::from_secs(6)).abs() < 1e-6);
        assert!((phase_at(Duration::from_secs(9)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn link_graphemes_keep_marks_emoji_and_cjk_whole() {
        assert_eq!(graphemes("abc"), ["a", "b", "c"]);
        // A combining mark stays with its base.
        assert_eq!(graphemes("e\u{301}x"), ["e\u{301}", "x"]);
        // A ZWJ sequence is one grapheme.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(graphemes(&format!("a{family}b")), ["a", family, "b"]);
        // One per CJK character.
        assert_eq!(graphemes("\u{6771}\u{4EAC}"), ["\u{6771}", "\u{4EAC}"]);
        assert!(graphemes("").is_empty());
        // The pieces tile the text.
        let s = "Zo\u{eb} \u{1F468}\u{200D}\u{1F469} \u{6771}";
        assert_eq!(graphemes(s).concat(), s);
    }

    #[test]
    fn link_pieces_find_each_name_in_order_as_a_whole_word() {
        let refs = [eref("Ann"), eref("Bo")];
        assert_eq!(
            pieces("Ann, Bo", &refs),
            [
                Piece::Link("Ann", 0),
                Piece::Plain(", "),
                Piece::Link("Bo", 1)
            ]
        );
        // A name inside a longer word is not a link; the real one later is.
        assert_eq!(
            pieces("Annabel & Ann", &[eref("Ann")]),
            [Piece::Plain("Annabel & "), Piece::Link("Ann", 0)]
        );
        // An album line with its year: only the name is the link.
        assert_eq!(
            pieces("Oruna  \u{b7}  2019", &[eref("Oruna")]),
            [Piece::Link("Oruna", 0), Piece::Plain("  \u{b7}  2019")]
        );
        // A name not in the text, an empty name, no refs: all plain.
        assert_eq!(pieces("Cy", &[eref("Ann"), eref("")]), [Piece::Plain("Cy")]);
        assert_eq!(pieces("Cy", &[]), [Piece::Plain("Cy")]);
        assert!(pieces("", &[eref("Ann")]).is_empty());
        // Multi-byte names.
        assert_eq!(
            pieces(
                "\u{6771}\u{4EAC}, Zo\u{eb}",
                &[eref("\u{6771}\u{4EAC}"), eref("Zo\u{eb}")]
            ),
            [
                Piece::Link("\u{6771}\u{4EAC}", 0),
                Piece::Plain(", "),
                Piece::Link("Zo\u{eb}", 1)
            ]
        );
    }

    #[test]
    fn link_stops_are_fixed_in_a_and_b_and_three_distinct_hues_in_c() {
        use crate::palette::{CoverAccent, hue_of};
        use crate::theme::{Look, ThemeId, theme};
        assert_eq!(Look::new(theme(ThemeId::A), None).link_stops(), STOPS);
        let b = Look::new(theme(ThemeId::B), None).link_stops();
        assert_eq!(b[0], [0xD9, 0x8A, 0x4E, 0xFF]);
        assert_eq!(b[2], [0xE0, 0x60, 0x3A, 0xFF]);
        let apart = |x: f32, y: f32| {
            let d = (x - y).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        // The mockup's samples: pink and orange, green and yellow.
        for (a, bb) in [
            ([0xFF, 0x8F, 0xBF, 0xFF], [0xFF, 0xB3, 0x6E, 0xFF]),
            ([0x6E, 0xE0, 0x7A, 0xFF], [0xE6, 0xE0, 0x70, 0xFF]),
        ] {
            let look = Look::new(theme(ThemeId::C), Some(CoverAccent { a, b: bb }));
            let [sa, sb, sc] = look.link_stops();
            assert_eq!((sa, sb), (a, bb));
            assert!(apart(hue_of(sc), hue_of(sa)) > 45.0, "{sc:?}");
            assert!(apart(hue_of(sc), hue_of(sb)) > 45.0, "{sc:?}");
        }
    }
}
