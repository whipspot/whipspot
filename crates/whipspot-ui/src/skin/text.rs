//! Bitmap-font text layout in skin pixels: which glyph sprite goes where inside a text widget's
//! rect. Every character takes one cell; the drawer clips partial glyphs to the rect.

use std::time::Duration;

use whipspot_core::skin::{BitmapFont, SpriteRect};

/// How fast scrolling text moves, in skin pixels per second.
pub(crate) const SCROLL_PX_PER_S: u64 = 20;

/// Spaces between the end of scrolling text and its next start.
const SCROLL_GAP: usize = 3;

/// The sprite for `c`: its own glyph, else the fallback's, else `None`.
pub(crate) fn glyph(font: &BitmapFont, c: char) -> Option<SpriteRect> {
    let find = |c: char| font.glyphs.iter().find(|(g, _)| *g == c).map(|&(_, s)| s);
    find(c).or_else(|| font.fallback.and_then(find))
}

/// Where text that fits its rect sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    /// At the rect's left edge (the track title).
    Left,
    /// Against the rect's right edge (the numbers: bit rate, sample rate, time).
    Right,
}

/// The glyphs of `text` in a rect `rect_w` skin px wide, as (x from the rect's left, sprite).
/// Text that fits sits left or right as `fit` says; longer text loops left with a gap when `scroll` is set (at
/// time `t`), else it is clipped from its start. Glyphs wholly outside the rect are left out;
/// characters without a sprite take their cell and draw nothing.
pub(crate) fn layout(
    font: &BitmapFont,
    text: &str,
    rect_w: u32,
    fit: Fit,
    scroll: bool,
    t: Duration,
) -> Vec<(i32, SpriteRect)> {
    let cell = i64::from(font.cell.w);
    let rect_w = i64::from(rect_w);
    let chars: Vec<char> = text.chars().collect();
    let width = chars.len() as i64 * cell;
    let mut out = Vec::new();
    if cell == 0 {
        return out;
    }
    let place = |start: i64, chars: &[char], out: &mut Vec<(i32, SpriteRect)>| {
        for (i, &c) in chars.iter().enumerate() {
            let x = start + i as i64 * cell;
            if x + cell <= 0 || x >= rect_w {
                continue;
            }
            if let Some(s) = glyph(font, c) {
                out.push((x as i32, s));
            }
        }
    };
    if width <= rect_w {
        let start = match fit {
            Fit::Left => 0,
            Fit::Right => rect_w - width,
        };
        place(start, &chars, &mut out);
    } else if scroll {
        let mut looped = chars;
        looped.extend(std::iter::repeat_n(' ', SCROLL_GAP));
        let period = looped.len() as i64 * cell;
        let travelled = (t.as_millis() * u128::from(SCROLL_PX_PER_S) / 1000) % period as u128;
        let mut start = -(travelled as i64);
        while start < rect_w {
            place(start, &looped, &mut out);
            start += period;
        }
    } else {
        place(0, &chars, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use whipspot_core::skin::{ImageId, Rect, Size};

    use super::*;

    fn sprite(i: u32) -> SpriteRect {
        SpriteRect {
            image: ImageId(0),
            rect: Rect {
                x: i * 5,
                y: 0,
                w: 5,
                h: 6,
            },
        }
    }

    /// Glyphs "A".."E" and a space, cells 5x6; fallback "?" when given.
    fn font(fallback: Option<char>) -> BitmapFont {
        let mut glyphs: Vec<(char, SpriteRect)> = "ABCDE ?"
            .chars()
            .enumerate()
            .map(|(i, c)| (c, sprite(i as u32)))
            .collect();
        glyphs.retain(|(c, _)| fallback.is_some() || *c != '?');
        BitmapFont {
            cell: Size { w: 5, h: 6 },
            glyphs,
            fallback,
        }
    }

    fn xs(v: &[(i32, SpriteRect)]) -> Vec<i32> {
        v.iter().map(|(x, _)| *x).collect()
    }

    #[test]
    fn glyph_exact_then_fallback_then_none() {
        assert_eq!(glyph(&font(None), 'B'), Some(sprite(1)));
        assert_eq!(glyph(&font(Some('?')), 'z'), Some(sprite(6)));
        assert_eq!(glyph(&font(None), 'z'), None);
        // A fallback the font lacks draws nothing.
        let mut f = font(None);
        f.fallback = Some('#');
        assert_eq!(glyph(&f, 'z'), None);
    }

    #[test]
    fn short_text_is_right_aligned() {
        let l = layout(
            &font(None),
            "ABC",
            40,
            Fit::Right,
            true,
            Duration::from_secs(9),
        );
        assert_eq!(xs(&l), [25, 30, 35]);
        assert_eq!(l[0].1, sprite(0));
        // A missing glyph keeps its cell and draws nothing.
        let l = layout(&font(None), "AzC", 40, Fit::Right, false, Duration::ZERO);
        assert_eq!(xs(&l), [25, 35]);
        let l = layout(
            &font(Some('?')),
            "AzC",
            40,
            Fit::Right,
            false,
            Duration::ZERO,
        );
        assert_eq!(l[1], (30, sprite(6)));
    }

    #[test]
    fn short_text_is_left_aligned_when_asked() {
        let l = layout(
            &font(None),
            "ABC",
            40,
            Fit::Left,
            true,
            Duration::from_secs(9),
        );
        assert_eq!(xs(&l), [0, 5, 10]);
        // Text that exactly fills the rect is the same either way.
        let l = layout(&font(None), "ABCD", 20, Fit::Left, false, Duration::ZERO);
        let r = layout(&font(None), "ABCD", 20, Fit::Right, false, Duration::ZERO);
        assert_eq!(l, r);
        // Long text is unaffected by the choice: clipped from its start, or scrolling.
        let l = layout(&font(None), "ABCDEA", 12, Fit::Left, false, Duration::ZERO);
        assert_eq!(xs(&l), [0, 5, 10]);
        let a = layout(
            &font(None),
            "ABCDE",
            12,
            Fit::Left,
            true,
            Duration::from_millis(500),
        );
        let b = layout(
            &font(None),
            "ABCDE",
            12,
            Fit::Right,
            true,
            Duration::from_millis(500),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn long_text_without_scroll_is_clipped_from_the_start() {
        // 6 chars = 30 px in 12 px: glyphs at 0, 5, 10 (the last partly inside).
        let l = layout(
            &font(None),
            "ABCDEA",
            12,
            Fit::Right,
            false,
            Duration::from_secs(3),
        );
        assert_eq!(xs(&l), [0, 5, 10]);
    }

    #[test]
    fn long_text_scrolls_left_with_a_gap() {
        let f = font(None);
        // "ABCDE" + 3 spaces = 40 px period, rect 12 px.
        let at0 = layout(&f, "ABCDE", 12, Fit::Right, true, Duration::ZERO);
        assert_eq!(xs(&at0), [0, 5, 10]);
        // 0.5 s at 20 px/s = 10 px: 'C' at 0.
        let l = layout(
            &f,
            "ABCDE",
            12,
            Fit::Right,
            true,
            Duration::from_millis(500),
        );
        assert_eq!(l[0], (0, sprite(2)));
        // 1.6 s = 32 px: inside the gap, and 'A' comes back at 8.
        let l = layout(
            &f,
            "ABCDE",
            12,
            Fit::Right,
            true,
            Duration::from_millis(1600),
        );
        assert_eq!(l.last().copied(), Some((8, sprite(0))));
        // One whole period later it repeats.
        let again = layout(&f, "ABCDE", 12, Fit::Right, true, Duration::from_secs(2));
        assert_eq!(again, at0);
        // Partial glyphs are kept: at 3 px travelled 'A' starts at -3.
        let l = layout(
            &f,
            "ABCDE",
            12,
            Fit::Right,
            true,
            Duration::from_millis(150),
        );
        assert_eq!(l[0], (-3, sprite(0)));
    }
}
