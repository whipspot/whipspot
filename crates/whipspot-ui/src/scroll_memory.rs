//! Scroll memory (MD202): back and forward return a page to where it was scrolled.
//!
//! luneui v0.7.0 has no absolute scroll setter, so a restore is a reveal: `scroll_to` with the
//! span `y .. y + viewport`, which from the top of a fresh area lands the offset exactly on `y`
//! (the span's end meets the viewport's end). The target is clamped to the content's last
//! offset, because luneui ignores a reveal whose span starts past the content: a page that is
//! still loading is walked down to its end, which draws the last row and so asks for the next
//! page, until the wanted offset fits.
//!
//! [`History`](crate::history::History) holds each entry's offset and hands it back on an
//! arrival; [`Browse`](crate::browse) runs a [`Restore`] per frame with [`step`], and each view
//! issues the reveal (before its scroll call) and reports a [`Scrolled`].

use std::hash::Hash;
use std::time::Duration;

use lune_ui::input::{Event, Key, NamedKey};
use lune_ui::paint::Rect;
use lune_ui::ui::Ui;
use lune_ui::widgets::scroll_area::{Reveal, Target, max_offset};

/// A restore gives up after this long (a page that never loads, a list that never grows).
pub(crate) const RESTORE_FOR: Duration = Duration::from_millis(3000);
/// An offset this close to the wanted one is there (px).
const CLOSE: f64 = 1.0;
/// The key of the scroll area inside luneui v0.7.0's virtual list (`virtual_list.rs:51`, its
/// private `const AREA`), under the list's own scope. A luneui upgrade that renames it fails the
/// `reveal_list` test, not the app.
const LIST_AREA: &str = "area";

/// What a view reports of its scroll area in a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scrolled {
    pub offset: f64,
    pub extent: f64,
    pub viewport: f64,
    /// More content may still arrive (the list is loading or has a next page).
    pub can_grow: bool,
}

/// A restore in progress: the offset to return to, when it began and the history generation it
/// belongs to (a push, back or forward since drops it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Restore {
    pub y: f64,
    pub since: Duration,
    pub generation: u64,
}

/// What a frame of a restore does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Ask the view to reveal the offset again (an animation in flight is not disturbed).
    Reveal,
    Done,
}

/// The next move of `r` given what the view reported this frame (`None`: it has not drawn yet).
pub(crate) fn step(r: &Restore, s: Option<Scrolled>, now: Duration, user_moved: bool) -> Step {
    if user_moved || now.saturating_sub(r.since) > RESTORE_FOR {
        return Step::Done;
    }
    let Some(s) = s else {
        return Step::Reveal;
    };
    if (s.offset - r.y).abs() <= CLOSE {
        return Step::Done;
    }
    let end = max_offset(s.extent, s.viewport);
    if !s.can_grow && s.offset >= end - CLOSE && end < r.y {
        // As far as the page goes, and it will not get longer.
        return Step::Done;
    }
    Step::Reveal
}

/// The target of a restore to `y`, clamped to the content's last offset.
fn target(y: f64, viewport: f64, extent: f64) -> Target {
    Target::span_y(y.min(max_offset(extent, viewport)), viewport)
}

/// Asks the translated scroll area under `key` to go to offset `y` (clamped to its end, given
/// the `extent` the view is about to give it). Call before the area runs in the frame.
pub(crate) fn reveal_area(ui: &mut Ui<'_>, key: impl Hash, y: f64, viewport: f64, extent: f64) {
    ui.scroll_to(key, target(y, viewport, extent), Reveal::Nearest);
}

/// [`reveal_area`] for the scroll area inside the virtual list under `list_key`, through
/// luneui's private area key ([`LIST_AREA`]). Call before the list runs in the frame.
pub(crate) fn reveal_list(
    ui: &mut Ui<'_>,
    list_key: impl Hash,
    y: f64,
    viewport: f64,
    extent: f64,
) {
    ui.scope(list_key, |ui| {
        ui.scroll_to(LIST_AREA, target(y, viewport, extent), Reveal::Nearest);
    });
}

/// Whether the user moved a page this frame: the wheel, a primary press in `sheet`, or a key
/// that scrolls (PageUp, PageDown, Home, End, Up, Down).
pub(crate) fn user_moved(ui: &Ui<'_>, sheet: Rect) -> bool {
    ui.input().events.iter().any(|e| match e {
        Event::Scroll { .. } => true,
        Event::PointerButton {
            pos,
            button: lune_ui::input::PointerButton::Primary,
            pressed: true,
            ..
        } => sheet.contains(*pos),
        Event::Key {
            key: Key::Named(k),
            pressed: true,
            ..
        } => matches!(
            k,
            NamedKey::PageUp
                | NamedKey::PageDown
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
        ),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use lune_ui::testing::UiHarness;
    use lune_ui::widgets::scroll_area::{Extent, ScrollOptions};
    use lune_ui::widgets::virtual_list::{Heights, ListOptions, RowKey, SelectionMode};

    use super::*;

    const ROW: f32 = 30.0;
    const ROWS: usize = 40;
    const VH: f32 = 300.0;

    fn restore(y: f64) -> Restore {
        Restore {
            y,
            since: Duration::from_millis(1000),
            generation: 0,
        }
    }

    fn at(offset: f64, extent: f64, can_grow: bool) -> Option<Scrolled> {
        Some(Scrolled {
            offset,
            extent,
            viewport: 300.0,
            can_grow,
        })
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn step_is_done_within_a_pixel_of_the_wanted_offset() {
        let r = restore(420.0);
        assert_eq!(
            step(&r, at(420.0, 2000.0, false), ms(1100), false),
            Step::Done
        );
        assert_eq!(
            step(&r, at(419.2, 2000.0, true), ms(1100), false),
            Step::Done
        );
        assert_eq!(
            step(&r, at(421.0, 2000.0, true), ms(1100), false),
            Step::Done
        );
        assert_eq!(
            step(&r, at(300.0, 2000.0, true), ms(1100), false),
            Step::Reveal
        );
    }

    #[test]
    fn step_is_done_when_clamped_and_nothing_more_will_load() {
        let r = restore(900.0);
        // The page ends at 500 (extent 800 - viewport 300) and has no more to come.
        assert_eq!(
            step(&r, at(500.0, 800.0, false), ms(1100), false),
            Step::Done
        );
        assert_eq!(
            step(&r, at(499.5, 800.0, false), ms(1100), false),
            Step::Done
        );
        // Still short of its end: keep going.
        assert_eq!(
            step(&r, at(300.0, 800.0, false), ms(1100), false),
            Step::Reveal
        );
        // At its end but more may load: keep revealing (the next page lengthens it).
        assert_eq!(
            step(&r, at(500.0, 800.0, true), ms(1100), false),
            Step::Reveal
        );
    }

    #[test]
    fn step_is_done_on_user_input_and_after_three_seconds() {
        let r = restore(900.0);
        assert_eq!(step(&r, at(0.0, 2000.0, true), ms(1100), true), Step::Done);
        assert_eq!(step(&r, None, ms(1100), true), Step::Done);
        assert_eq!(
            step(&r, at(0.0, 2000.0, true), ms(4000), false),
            Step::Reveal
        );
        assert_eq!(step(&r, at(0.0, 2000.0, true), ms(4001), false), Step::Done);
        assert_eq!(step(&r, None, ms(4001), false), Step::Done);
    }

    #[test]
    fn step_reveals_until_the_view_has_reported() {
        let r = restore(900.0);
        assert_eq!(step(&r, None, ms(1100), false), Step::Reveal);
    }

    /// A translated area of `ROWS` rows; one frame at `t` ms. Returns the area's offset.
    fn area_frame(h: &mut UiHarness, t: u64, want: Option<f64>) -> f64 {
        h.at_ms(t);
        let mut offset = 0.0;
        let vp = Rect::new(0.0, 0.0, 200.0, VH);
        let extent = f64::from(ROW) * ROWS as f64;
        h.frame(|ui| {
            if let Some(y) = want {
                reveal_area(ui, "page", y, f64::from(VH), extent);
            }
            let area = ui.scroll_area("page", vp, &ScrollOptions::default(), |ui, _| {
                for i in 0..ROWS {
                    let r = Rect::new(0.0, i as f32 * ROW, 200.0, ROW);
                    ui.painter().fill(r, 0.0, [10, 10, 10, 255]);
                }
                (
                    (),
                    Extent {
                        width: 200.0,
                        height: extent,
                    },
                )
            });
            offset = area.offset.1;
        });
        offset
    }

    fn settle(
        h: &mut UiHarness,
        from: u64,
        want: f64,
        frame: fn(&mut UiHarness, u64, Option<f64>) -> f64,
    ) -> f64 {
        let mut offset = 0.0;
        for i in 0..60 {
            offset = frame(h, from + i * 16, Some(want));
        }
        offset
    }

    #[test]
    fn reveal_area_reaches_the_offset_on_a_fresh_area_and_stops_at_the_end() {
        let mut h = UiHarness::new((200.0, VH));
        area_frame(&mut h, 0, None);
        let got = settle(&mut h, 16, 420.0, area_frame);
        assert!((got - 420.0).abs() <= 1.0, "reached {got}");

        // Beyond the end settles at the last offset (extent 1200 - viewport 300).
        let mut h = UiHarness::new((200.0, VH));
        area_frame(&mut h, 0, None);
        let got = settle(&mut h, 16, 3000.0, area_frame);
        assert!((got - 900.0).abs() <= 1.0, "reached {got}");
    }

    /// The same through a virtual list: its offset is `ListResponse.offset`.
    fn list_frame(h: &mut UiHarness, t: u64, want: Option<f64>) -> f64 {
        h.at_ms(t);
        let mut offset = 0.0;
        let rect = Rect::new(0.0, 0.0, 200.0, VH);
        let extent = f64::from(ROW) * ROWS as f64;
        h.frame(|ui| {
            if let Some(y) = want {
                reveal_list(ui, "rows", y, f64::from(VH), extent);
            }
            let opts = ListOptions {
                count: ROWS,
                heights: Heights::Fixed(ROW),
                selection: SelectionMode::None,
                index_of: None,
                label: "Rows",
            };
            let resp = ui.virtual_list("rows", rect, &opts, |i| RowKey(i as u64), |_, _| ROW);
            offset = resp.offset;
        });
        offset
    }

    #[test]
    fn reveal_list_reaches_the_offset_through_the_lists_own_area() {
        let mut h = UiHarness::new((200.0, VH));
        list_frame(&mut h, 0, None);
        let got = settle(&mut h, 16, 420.0, list_frame);
        assert!((got - 420.0).abs() <= 1.0, "reached {got}");

        let mut h = UiHarness::new((200.0, VH));
        list_frame(&mut h, 0, None);
        let got = settle(&mut h, 16, 3000.0, list_frame);
        assert!((got - 900.0).abs() <= 1.0, "reached {got}");
    }
}
