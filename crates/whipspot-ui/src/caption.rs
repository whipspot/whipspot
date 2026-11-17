//! The main window's caption (MD58): a centred product mark and title in the 40 px band. The
//! band's behaviour (drag, double click, buttons, edge resize) is the shell's `window_controls`.

use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::Ui;

use crate::draw::{alpha, disc, text, width};
use crate::theme::{Indicator, Look};

/// The title's font size and its widest extent, logical px.
const TITLE_SIZE: f32 = 13.0;
const TITLE_MAX_W: f32 = 560.0;
/// The mark's diameter and the gap between it and the title.
const MARK_D: f32 = 20.0;
const MARK_GAP: f32 = 10.0;
/// The mark's three bars (MD58): heights, width and pitch.
const BAR_HEIGHTS: [f32; 3] = [6.0, 10.0, 7.0];
const BAR_W: f32 = 2.0;
const BAR_PITCH: f32 = 3.4;

/// The x range the mark and title occupy when centred in a band `band_w` wide, for a title
/// `title_w` wide: `(left, right)`.
pub fn group_extent(band_w: f32, title_w: f32) -> (f32, f32) {
    let total = MARK_D + MARK_GAP + title_w.min(TITLE_MAX_W);
    let left = ((band_w - total) / 2.0).round();
    (left, left + total)
}

/// Draws the mark and `title` centred in `band` (the caption band's rect).
pub(crate) fn draw(ui: &mut Ui<'_>, band: Rect, look: &Look, title: &str) {
    let p = ui.painter();
    p.on(LayerId::Page);
    let cy = band.y + band.h / 2.0;
    let tw = width(p, title, TITLE_SIZE, true);
    let (x, _) = group_extent(band.w, tw);
    let mark = (x + MARK_D / 2.0, cy);
    if look.decor().indicator == Indicator::Led {
        disc(p, mark, MARK_D + 8.0, alpha(look.a, 0x18));
    }
    disc(p, mark, MARK_D, look.a);
    // The bars are centred on the disc.
    let bars_w = BAR_PITCH * (BAR_HEIGHTS.len() - 1) as f32 + BAR_W;
    for (i, h) in BAR_HEIGHTS.iter().enumerate() {
        let bx = mark.0 - bars_w / 2.0 + i as f32 * BAR_PITCH;
        p.fill(Rect::new(bx, cy - h / 2.0, BAR_W, *h), 1.0, look.on);
    }
    text(
        p,
        title,
        TITLE_SIZE,
        look.ink.text2,
        true,
        x + MARK_D + MARK_GAP,
        cy - 8.5,
        TITLE_MAX_W,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_group_is_centred() {
        let (left, right) = group_extent(1440.0, 120.0);
        assert!(((left + right) / 2.0 - 720.0).abs() <= 1.0);
        assert_eq!(right - left, 150.0);
        // A long title is cut at the widest extent.
        let (left, right) = group_extent(1440.0, 900.0);
        assert_eq!(right - left, 590.0);
        assert!(((left + right) / 2.0 - 720.0).abs() <= 1.0);
    }
}
