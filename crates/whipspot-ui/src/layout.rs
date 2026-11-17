//! The main window's regions, logical px: one source of rects at any window size from the
//! minimum (760 x 480) up. Colours and treatments come from the look, never from here.

use lune_ui::paint::Rect;

use crate::spectrum::HEIGHT_SHARE;

/// The main window's caption band (MD58).
pub const CAPTION_H: f32 = 40.0;
/// The media control's height and its margin from the window's bottom edge. Its x and width are
/// the browse sheet's ([`SHEET_X`]), so the two panels line up.
pub const MEDIA_H: f32 = 76.0;
pub const MEDIA_M: f32 = 12.0;
/// The browse sheet's inset from the window's sides and its top (under the caption).
pub const SHEET_X: f32 = 32.0;
pub const SHEET_TOP: f32 = 52.0;
/// The gap between the browse sheet and the media control.
pub const SHEET_GAP: f32 = 16.0;

/// The gap between the window edge and the panels (left, right) of the diagnostics window.
pub const MARGIN: f32 = 10.0;
/// The content card's left and right inset: headings, the search field, the account column.
pub const CONTENT_PAD: f32 = 20.0;
/// The heading slot's top inside the content card and its height: the page title (library,
/// account) or the search field (search) sits in it.
pub const HEADING_TOP: f32 = 12.0;
pub const HEADING_H: f32 = 32.0;
/// Where the content under the heading starts, inside the content card.
pub const BODY_TOP: f32 = 56.0;

/// The heading slot of a content card `content`.
pub fn heading_slot(content: Rect) -> Rect {
    Rect::new(
        content.x + CONTENT_PAD,
        content.y + HEADING_TOP,
        (content.w - 2.0 * CONTENT_PAD).max(0.0),
        HEADING_H,
    )
}

/// The regions of the main window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub window: Rect,
    /// The caption band across the top.
    pub caption: Rect,
    /// The media control along the bottom.
    pub media: Rect,
    /// The browse sheet between the caption and the media control.
    pub sheet: Rect,
    /// The idle view's area: the window less a caption's height top and bottom (the media control is
    /// hidden then).
    pub idle: Rect,
    /// The visualiser: full width, bottom-anchored, up to 30 % of the height.
    pub visualiser: Rect,
}

impl Layout {
    /// The layout of a window `window` (its origin is the window's).
    pub fn new(window: Rect) -> Layout {
        let media = Rect::new(
            SHEET_X,
            (window.h - MEDIA_M - MEDIA_H).max(CAPTION_H),
            (window.w - 2.0 * SHEET_X).max(0.0),
            MEDIA_H,
        );
        let sheet_bottom = media.y - SHEET_GAP;
        let vis_h = window.h * HEIGHT_SHARE;
        Layout {
            window,
            caption: Rect::new(0.0, 0.0, window.w, CAPTION_H.min(window.h)),
            media,
            sheet: Rect::new(
                SHEET_X,
                SHEET_TOP,
                (window.w - 2.0 * SHEET_X).max(0.0),
                (sheet_bottom - SHEET_TOP).max(0.0),
            ),
            // The media control is hidden in the idle view: the idle group is centred in the window
            // between the caption's height at the top and the same at the bottom.
            idle: Rect::new(
                0.0,
                CAPTION_H,
                window.w,
                (window.h - 2.0 * CAPTION_H).max(0.0),
            ),
            visualiser: Rect::new(0.0, window.h - vis_h, window.w, vis_h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_at_1440_by_900() {
        let l = Layout::new(Rect::new(0.0, 0.0, 1440.0, 900.0));
        assert_eq!(l.caption, Rect::new(0.0, 0.0, 1440.0, 40.0));
        assert_eq!(l.media, Rect::new(32.0, 812.0, 1376.0, 76.0));
        assert_eq!(l.sheet, Rect::new(32.0, 52.0, 1376.0, 744.0));
        assert_eq!((l.media.x, l.media.w), (l.sheet.x, l.sheet.w));
        assert_eq!(l.idle, Rect::new(0.0, 40.0, 1440.0, 820.0));
        assert_eq!(l.visualiser, Rect::new(0.0, 630.0, 1440.0, 270.0));
    }

    #[test]
    fn regions_hold_at_the_minimum_size() {
        let l = Layout::new(Rect::new(0.0, 0.0, 760.0, 480.0));
        assert_eq!(l.media.y, 392.0);
        assert_eq!(l.sheet.bottom(), 376.0);
        assert_eq!((l.media.x, l.media.w), (l.sheet.x, l.sheet.w));
        assert!(l.sheet.h > 200.0);
        assert!(l.visualiser.h <= 0.3 * 480.0 + 0.01);
    }
}
