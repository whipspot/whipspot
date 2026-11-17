//! The idle view's lyrics: synced lines that follow the position, plain lyrics that scroll in
//! proportion to it (MD75).
//!
//! The drawing keeps no state: which line is current, and how far the eased scroll between two
//! lines has got, follow from the position alone, so a seek lands on the right picture at once. The
//! text is already safe to draw (the core sanitises it, MD68); this module only places it, inside
//! the rect it is given.

use std::time::Duration;

use lune_ui::color::Rgba;
use lune_ui::paint::Rect;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::Ui;
use whipspot_core::lyrics::{LyricLine, Lyrics};

use crate::draw::{P, alpha, led, vgradient, with_text_shadow};
use crate::spectrum::FRAME_INTERVAL;
use crate::theme::{Indicator, Look};

/// The current line's size and the others'.
const NOW_SIZE: f32 = 34.0;
const OTHER_SIZE: f32 = 23.0;
/// The gap between two synced lines, and between two plain ones.
const GAP: f32 = 20.0;
const PLAIN_GAP: f32 = 10.0;
/// Space kept free at the rect's top and bottom edges.
const EDGE: f32 = 10.0;
/// How long the scroll from one line to the next takes.
const SCROLL_MS: u64 = 360;
/// A repaint step while the scroll moves.
const ANIM_FRAME: Duration = Duration::from_millis(16);
/// How far left of the rect the current line's mark sits, and the room kept for it.
const MARK_DX: f32 = 24.0;
const MARK_ROOM: f32 = 32.0;
/// A plain line's opacity (one value, so the scroll never re-shapes the text).
const PLAIN_ALPHA: u8 = 0xC8;
/// Synced lines fade out over this share of the rect's height at its top (past lines vanish well
/// before the window's top edge) and at its bottom (far upcoming lines).
const SYNCED_TOP_FADE: f32 = 0.35;
const SYNCED_BOTTOM_FADE: f32 = 0.25;
/// The same for plain lyrics while they scroll.
const PLAIN_EDGE_FADE: f32 = 0.18;

/// The index of the line current at `position_ms`: the last line with `at_ms` at or before it;
/// `None` before the first. Equal timestamps give the last of them. `lines` is in time order.
pub(crate) fn current_index(lines: &[LyricLine], position_ms: u64) -> Option<usize> {
    lines
        .partition_point(|l| l.at_ms <= position_ms)
        .checked_sub(1)
}

/// How far a plain lyric has scrolled: the position's share of the duration, in 0..=1 (0 when the
/// duration is 0).
pub(crate) fn plain_fraction(position_ms: u64, duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        return 0.0;
    }
    (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0) as f32
}

/// The share of the move from the previous line to the current one that is done `since_ms` after
/// the current line began: 0 at the start, 1 from `SCROLL_MS` on, eased out (fast at first, then
/// settling). It drives the scroll and the cross-fade of the two lines that change style.
fn scroll_progress(since_ms: u64) -> f32 {
    let t = (since_ms as f32 / SCROLL_MS as f32).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// `a` (an alpha, 0..=255) scaled by `share` (0..=1).
fn scaled(a: u8, share: f32) -> u8 {
    (f32::from(a) * share.clamp(0.0, 1.0)).round() as u8
}

/// When the view must next be drawn again while playing: soon while a scroll moves or plain
/// lyrics scroll with the position, else when the next line begins. `None` when nothing changes.
pub(crate) fn repaint_in(lyrics: &Lyrics, position_ms: u64) -> Option<Duration> {
    match lyrics {
        Lyrics::Synced(lines) => {
            let cur = current_index(lines, position_ms);
            if let Some(i) = cur
                && i > 0
                && position_ms.saturating_sub(lines[i].at_ms) < SCROLL_MS
            {
                return Some(ANIM_FRAME);
            }
            let next = lines.get(cur.map_or(0, |i| i + 1))?;
            Some(Duration::from_millis(
                next.at_ms.saturating_sub(position_ms).max(1),
            ))
        }
        Lyrics::Plain(lines) if !lines.is_empty() => Some(FRAME_INTERVAL),
        _ => None,
    }
}

/// The alpha of a line `d` lines from the current one (`d` 0 is the current line). A past line
/// more than four above the current one is gone.
fn fade(d: usize, before: bool) -> u8 {
    if before {
        const PAST: [u8; 5] = [0xFF, 0x74, 0x4A, 0x2A, 0x14];
        PAST.get(d).copied().unwrap_or(0)
    } else {
        const NEXT: [u8; 4] = [0xFF, 0x9C, 0x6A, 0x3E];
        NEXT[d.min(3)]
    }
}

/// How much of its alpha a line whose middle is at `mid` keeps inside `area`: 0 at the rect's top
/// edge rising smoothly to 1 at `top` of the height below it, and the same towards the bottom edge
/// over `bottom` of the height.
fn edge_share(mid: f32, area: Rect, top: f32, bottom: f32) -> f32 {
    let ramp = |dist: f32, span: f32| {
        if span <= 0.0 {
            return 1.0;
        }
        let t = (dist / span).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    ramp(mid - area.y, area.h * top).min(ramp(area.bottom() - mid, area.h * bottom))
}

/// One line of the layout: its index, top and height.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placed {
    idx: usize,
    y: f32,
    h: f32,
}

/// Where the lines sit when `cur` is the current line (`None`: none yet, the first line is
/// upcoming): the current line's box centred on `cy`, the others stacked out from it until the
/// rect's edges. `height(i, now)` is line `i`'s height in the current (`now`) or the other style.
fn place(
    n: usize,
    cur: Option<usize>,
    area: Rect,
    cy: f32,
    mut height: impl FnMut(usize, bool) -> f32,
) -> Vec<Placed> {
    let mut out = Vec::new();
    let ch = match cur {
        Some(i) => height(i, true),
        None => (NOW_SIZE * 1.25).round(),
    };
    let top = cy - ch / 2.0;
    if let Some(i) = cur {
        out.push(Placed {
            idx: i,
            y: top,
            h: ch,
        });
        let mut y = top - GAP;
        for idx in (0..i).rev() {
            let h = height(idx, false);
            y -= h;
            if y < area.y + EDGE {
                break;
            }
            out.push(Placed { idx, y, h });
            y -= GAP;
        }
    }
    let mut y = top + ch + GAP + 6.0;
    for idx in cur.map_or(0, |i| i + 1)..n {
        let h = height(idx, false);
        if y + h > area.bottom() - EDGE {
            break;
        }
        out.push(Placed { idx, y, h });
        y += h + GAP;
    }
    out
}

fn spec(text: &str, now: bool, color: Rgba, w: f32) -> Spec {
    let size = if now { NOW_SIZE } else { OTHER_SIZE };
    let mut st = Style::ui(size, color);
    st.line = (size * 1.25).round();
    if now {
        st = st.medium();
    }
    Spec::plain(text, st).wrapped(w)
}

/// The height of `text` wrapped to `w` in the current or the other style (measured in one colour,
/// so the shaped text is shared with every opacity).
fn measure(p: &mut P<'_>, text: &str, now: bool, w: f32) -> f32 {
    let size = if now { NOW_SIZE } else { OTHER_SIZE };
    p.measure(&spec(text, now, [0xFF; 4], w))
        .1
        .max((size * 1.25).round())
}

/// Draws `lyrics` in `rect`, the current line (synced) centred vertically on `cy` (the art's
/// centre, as in round 3; the rect only bounds the lines); returns
/// whether anything was drawn (`false` for Instrumental, None and empty lists). Everything is cut
/// to `rect` plus the room left of it for the current line's mark.
pub(crate) fn lyrics(
    ui: &mut Ui<'_>,
    rect: Rect,
    cy: f32,
    look: &Look,
    lyrics: &Lyrics,
    position_ms: u64,
    duration_ms: u64,
) -> bool {
    let p = ui.painter();
    p.push_clip(Rect::new(
        rect.x - MARK_ROOM,
        rect.y,
        rect.w + MARK_ROOM,
        rect.h,
    ));
    let drawn = match lyrics {
        Lyrics::Synced(lines) if !lines.is_empty() => {
            synced(p, rect, cy, look, lines, position_ms);
            true
        }
        Lyrics::Plain(lines) if !lines.is_empty() => {
            plain(p, rect, look, lines, position_ms, duration_ms);
            true
        }
        _ => false,
    };
    p.pop_clip();
    drawn
}

fn synced(p: &mut P<'_>, area: Rect, cy: f32, k: &Look, lines: &[LyricLine], position_ms: u64) {
    let w = area.w;
    let cur = current_index(lines, position_ms);
    let height = |p: &mut P<'_>, i: usize, now: bool| measure(p, &lines[i].text, now, w);
    let laid = place(lines.len(), cur, area, cy, |i, now| height(p, i, now));
    // Between two lines every line moves from where it sat when the previous one was current.
    let eased = match cur {
        Some(i) if i > 0 => {
            let e = scroll_progress(position_ms.saturating_sub(lines[i].at_ms));
            (e < 1.0).then(|| {
                let before = place(lines.len(), Some(i - 1), area, cy, |j, now| {
                    height(p, j, now)
                });
                (e, before)
            })
        }
        _ => None,
    };
    let ink = k.ink.text;
    let mut mark = None;
    for pl in &laid {
        let is_cur = Some(pl.idx) == cur;
        let y = match &eased {
            Some((e, before)) => match before.iter().find(|b| b.idx == pl.idx) {
                Some(b) => b.y + (pl.y - b.y) * e,
                None => pl.y,
            },
            None => pl.y,
        };
        let text = &lines[pl.idx].text;
        let a = match cur {
            Some(c) if pl.idx <= c => fade(c - pl.idx, true),
            Some(c) => fade(pl.idx - c, false),
            None => fade(pl.idx + 1, false),
        };
        // The line fades out towards the rect's edges, following its eased position.
        let edge = edge_share(y + pl.h / 2.0, area, SYNCED_TOP_FADE, SYNCED_BOTTOM_FADE);
        let a = scaled(a, edge);
        if text.is_empty() || (a == 0 && !is_cur) {
            // An instrumental gap: the space, no text, no mark.
            continue;
        }
        let draw = |p: &mut P<'_>, now: bool, a: u8| {
            with_text_shadow(p, alpha(ink, a), |p, c, dx, dy| {
                p.text(&spec(text, now, c, w), area.x + dx, y + dy);
            });
        };
        match (&eased, cur) {
            // The two lines that change style cross-fade between the old style and the new one
            // while they move, instead of snapping when the line begins.
            (Some((e, _)), Some(_)) if is_cur => {
                draw(p, false, scaled(fade(1, false), 1.0 - e));
                draw(p, true, scaled(a, *e));
            }
            (Some((e, _)), Some(c)) if pl.idx + 1 == c => {
                draw(p, true, scaled(0xFF, 1.0 - e));
                draw(p, false, scaled(a, *e));
            }
            _ => draw(p, is_cur, a),
        }
        if is_cur {
            mark = Some((y, pl.h));
        }
    }
    if let Some((top, ch)) = mark {
        draw_mark(p, k, area.x, top, ch);
    }
}

/// The accent mark left of the current line: the hardware theme's LED and gradient hairline, the
/// others' 3 px bar from the first accent to the second.
fn draw_mark(p: &mut P<'_>, k: &Look, x: f32, top: f32, ch: f32) {
    match k.decor().indicator {
        Indicator::Led => {
            vgradient(
                p,
                Rect::new(x - MARK_DX + 1.0, top + 4.0, 1.0, (ch - 8.0).max(0.0)),
                alpha(k.a, 0x90),
                alpha(k.a, 0x20),
            );
            led(p, (x - MARK_DX + 1.5, top + 22.0), 6.0, k.a, k.hi, true);
        }
        Indicator::Check => {
            vgradient(
                p,
                Rect::new(x - MARK_DX, top + 4.0, 3.0, (ch - 8.0).max(0.0)),
                k.a,
                k.b,
            );
        }
    }
}

fn plain(
    p: &mut P<'_>,
    area: Rect,
    k: &Look,
    lines: &[String],
    position_ms: u64,
    duration_ms: u64,
) {
    let w = area.w;
    let heights: Vec<f32> = lines.iter().map(|l| measure(p, l, false, w)).collect();
    let total: f32 =
        heights.iter().sum::<f32>() + PLAIN_GAP * heights.len().saturating_sub(1) as f32;
    let view = (area.h - 2.0 * EDGE).max(0.0);
    let top = if total <= view {
        // Short lyrics stay put, centred in the rect.
        area.y + (area.h - total) / 2.0
    } else {
        area.y + EDGE - plain_fraction(position_ms, duration_ms) * (total - view)
    };
    let scrolls = total > view;
    let mut y = top;
    for (line, h) in lines.iter().zip(&heights) {
        if y + h >= area.y && y <= area.bottom() && !line.is_empty() {
            let share = if scrolls {
                edge_share(y + h / 2.0, area, PLAIN_EDGE_FADE, PLAIN_EDGE_FADE)
            } else {
                1.0
            };
            let color = alpha(k.ink.text, scaled(PLAIN_ALPHA, share));
            with_text_shadow(p, color, |p, c, dx, dy| {
                p.text(&spec(line, false, c, w), area.x + dx, y + dy);
            });
        }
        y += h + PLAIN_GAP;
        if y > area.bottom() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ly(times: &[u64]) -> Vec<LyricLine> {
        times
            .iter()
            .map(|&at_ms| LyricLine {
                at_ms,
                text: format!("line {at_ms}"),
            })
            .collect()
    }

    #[test]
    fn ly1_current_index_before_between_exact_and_after() {
        let lines = ly(&[1000, 2000, 3000]);
        assert_eq!(current_index(&lines, 0), None);
        assert_eq!(current_index(&lines, 999), None);
        assert_eq!(current_index(&lines, 1000), Some(0));
        assert_eq!(current_index(&lines, 1500), Some(0));
        assert_eq!(current_index(&lines, 2000), Some(1));
        assert_eq!(current_index(&lines, 2999), Some(1));
        assert_eq!(current_index(&lines, 3000), Some(2));
        assert_eq!(current_index(&lines, 999_999), Some(2));
        assert_eq!(current_index(&[], 5), None);
    }

    #[test]
    fn ly2_equal_timestamps_make_the_last_one_current() {
        let lines = ly(&[1000, 2000, 2000, 2000, 3000]);
        assert_eq!(current_index(&lines, 1999), Some(0));
        assert_eq!(current_index(&lines, 2000), Some(3));
        assert_eq!(current_index(&lines, 2500), Some(3));
        assert_eq!(current_index(&lines, 3000), Some(4));
    }

    #[test]
    fn ly3_plain_fraction_clamps_to_zero_and_one() {
        assert_eq!(plain_fraction(0, 200_000), 0.0);
        assert_eq!(plain_fraction(50_000, 200_000), 0.25);
        assert_eq!(plain_fraction(200_000, 200_000), 1.0);
        assert_eq!(plain_fraction(900_000, 200_000), 1.0);
        assert_eq!(plain_fraction(5, 0), 0.0);
        assert_eq!(plain_fraction(0, 0), 0.0);
    }

    #[test]
    fn scroll_eases_out_from_nothing_to_all() {
        assert_eq!(scroll_progress(0), 0.0);
        assert_eq!(scroll_progress(SCROLL_MS), 1.0);
        assert_eq!(scroll_progress(10 * SCROLL_MS), 1.0);
        // Ease-out: more than half done at half the time, and never going backwards.
        assert!(scroll_progress(SCROLL_MS / 2) > 0.8);
        let mut last = 0.0;
        for ms in (0..=SCROLL_MS).step_by(5) {
            let e = scroll_progress(ms);
            assert!(e >= last, "{ms}: {e} < {last}");
            last = e;
        }
        // The move takes 300 to 400 ms.
        assert!((300..=400).contains(&SCROLL_MS));
    }

    #[test]
    fn the_cross_fade_alphas_meet_the_resting_ones() {
        // At the end of the move the two lines carry exactly their resting alphas; at the start
        // the old style carries all of it.
        for (e, old, new) in [(0.0f32, 0x9Cu8, 0u8), (1.0, 0, 0xFF)] {
            assert_eq!(scaled(fade(1, false), 1.0 - e), old);
            assert_eq!(scaled(0xFF, e), new);
        }
        assert_eq!(scaled(0xFF, 7.0), 0xFF);
        assert_eq!(scaled(0xFF, -1.0), 0);
    }

    #[test]
    fn past_lines_are_gone_beyond_four_above() {
        assert_eq!(fade(0, true), 0xFF);
        assert!(fade(4, true) > 0);
        assert_eq!(fade(5, true), 0);
        assert_eq!(fade(40, true), 0);
        // Never brighter the further from the current line.
        for d in 0..6 {
            assert!(fade(d + 1, true) <= fade(d, true));
        }
        assert_eq!(fade(9, false), fade(3, false));
    }

    #[test]
    fn edge_share_ramps_from_the_top_and_bottom_edges() {
        let area = Rect::new(0.0, 100.0, 500.0, 600.0);
        let s = |mid| edge_share(mid, area, SYNCED_TOP_FADE, SYNCED_BOTTOM_FADE);
        // Nothing at the top edge or above it; full from 35 % of the height down.
        assert_eq!(s(100.0), 0.0);
        assert_eq!(s(40.0), 0.0);
        assert_eq!(s(100.0 + 0.35 * 600.0), 1.0);
        assert_eq!(s(400.0), 1.0);
        // Rising, never going down on the way in, and low in the first part.
        let mut last = 0.0;
        for i in 0..=210 {
            let v = s(100.0 + i as f32);
            assert!(v >= last, "{i}");
            last = v;
        }
        assert!(s(100.0 + 0.1 * 600.0) < 0.25);
        // The same at the bottom edge over 25 %.
        assert_eq!(s(700.0), 0.0);
        assert_eq!(s(700.0 - 0.25 * 600.0), 1.0);
        assert!(s(690.0) < 0.02);
        // A zero span means no fade there.
        assert_eq!(edge_share(100.0, area, 0.0, 0.0), 1.0);
    }

    #[test]
    fn repaint_follows_the_next_line_and_the_scroll() {
        let s = Lyrics::Synced(ly(&[1000, 2000, 3000]));
        // Before the first line: wait for it.
        assert_eq!(repaint_in(&s, 400), Some(Duration::from_millis(600)));
        // The first line has no previous one to scroll from.
        assert_eq!(repaint_in(&s, 1100), Some(Duration::from_millis(900)));
        // Just after the second line began the scroll moves.
        assert_eq!(repaint_in(&s, 2100), Some(ANIM_FRAME));
        assert_eq!(repaint_in(&s, 2600), Some(Duration::from_millis(400)));
        // After the last line nothing changes.
        assert_eq!(repaint_in(&s, 9000), None);
        assert_eq!(
            repaint_in(&Lyrics::Plain(vec!["a".into()]), 0),
            Some(FRAME_INTERVAL)
        );
        assert_eq!(repaint_in(&Lyrics::Instrumental, 0), None);
    }

    #[test]
    fn placing_centres_the_current_line_and_stays_inside_the_rect() {
        let area = Rect::new(150.0, 40.0, 500.0, 552.0);
        let laid = place(
            30,
            Some(10),
            area,
            316.0,
            |_, now| if now { 43.0 } else { 29.0 },
        );
        let cur = laid.iter().find(|p| p.idx == 10).unwrap();
        assert_eq!(cur.y + cur.h / 2.0, 316.0);
        for p in &laid {
            assert!(p.y >= area.y + EDGE - 0.01, "{p:?}");
            assert!(p.y + p.h <= area.bottom() - EDGE + 0.01, "{p:?}");
        }
        // Lines on both sides, in order, none twice.
        assert!(laid.iter().any(|p| p.idx < 10) && laid.iter().any(|p| p.idx > 10));
        let mut idx: Vec<usize> = laid.iter().map(|p| p.idx).collect();
        idx.sort_unstable();
        idx.dedup();
        assert_eq!(idx.len(), laid.len());
        // Before the first line everything is upcoming.
        let none = place(5, None, area, 316.0, |_, _| 29.0);
        assert_eq!(none.first().map(|p| p.idx), Some(0));
    }
}
