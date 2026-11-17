//! The resize drag of a panel's grip: follows the desktop pointer, snaps the size to the panel's
//! lattice, resizes the window and moves what is docked below or right of it. Pure over
//! [`PanelWindows`], so it runs on a fake in tests.

use whipspot_core::skin::{PanelWindows, ScreenPoint, ScreenRect, Size, Skin};

use super::dock::visible;

/// A resize in progress.
struct Held {
    panel: usize,
    /// The desktop pointer at the press.
    start: ScreenPoint,
    /// The panel's size at the press, in skin pixels.
    size: (u32, u32),
    /// Physical pixels per skin pixel (the panel set's pixel scale).
    k: u32,
}

/// Resizes one panel while its grip is held.
#[derive(Default)]
pub(crate) struct ResizeController {
    held: Option<Held>,
}

impl ResizeController {
    pub fn resizing(&self) -> bool {
        self.held.is_some()
    }

    /// Starts resizing `panel` (live size `size` skin pixels) with the desktop pointer at
    /// `pointer`. A panel that cannot resize, a hidden one or an unknown one starts nothing.
    pub fn press(
        &mut self,
        panel: usize,
        pointer: ScreenPoint,
        size: (u32, u32),
        k: u32,
        (windows, skin): (&dyn PanelWindows, &Skin),
    ) {
        self.held = None;
        let resizable = skin.panels.get(panel).is_some_and(|p| p.resize.is_some());
        if resizable && panel < windows.count() && windows.is_visible(panel) {
            self.held = Some(Held {
                panel,
                start: pointer,
                size,
                k: k.max(1),
            });
        }
    }

    /// Ends the resize.
    pub fn release(&mut self) {
        self.held = None;
    }

    /// Follows the desktop pointer: the size is the start size plus the pointer's travel in skin
    /// pixels, snapped to the lattice and bounded by the largest work area. A new size resizes the
    /// window (the live size in `sizes` follows only when that worked) and moves the panels docked
    /// below or right of it by the change.
    pub fn moved(&mut self, windows: &mut dyn PanelWindows, skin: &Skin, sizes: &mut [(u32, u32)]) {
        let Some(h) = &self.held else {
            return;
        };
        let (Some(pointer), Some(panel), Some(live)) = (
            windows.pointer(),
            skin.panels.get(h.panel),
            sizes.get_mut(h.panel),
        ) else {
            return;
        };
        let Some(resize) = &panel.resize else {
            return;
        };
        let unit = h.k as f32 * windows.units_per_pixel();
        if !unit.is_finite() || unit <= 0.0 {
            return;
        }
        let travel = |d: i32, from: u32| -> u32 {
            let px = (d as f32 / unit).round() as i64;
            (i64::from(from) + px).clamp(0, i64::from(u32::MAX)) as u32
        };
        let want = Size {
            w: travel(pointer.x.saturating_sub(h.start.x), h.size.0),
            h: travel(pointer.y.saturating_sub(h.start.y), h.size.1),
        };
        // The most that fits: the largest work area, in skin pixels.
        let max = windows
            .work_areas()
            .iter()
            .max_by_key(|a| i64::from(a.w) * i64::from(a.h))
            .map_or(
                Size {
                    w: u32::MAX,
                    h: u32::MAX,
                },
                |a| Size {
                    w: (a.w.max(0) as f32 / unit).floor() as u32,
                    h: (a.h.max(0) as f32 / unit).floor() as u32,
                },
            );
        let new = resize.snap(panel.size, want, max);
        if (new.w, new.h) == *live {
            return;
        }
        let Some(old) = windows.frame(h.panel) else {
            return;
        };
        let (nw, nh) = (
            (new.w as f32 * unit).round() as i32,
            (new.h as f32 * unit).round() as i32,
        );
        let moves = followers(windows, h.panel, old, (nw - old.w, nh - old.h));
        if windows.resize_panel(h.panel, nw, nh).is_err() {
            return;
        }
        *live = (new.w, new.h);
        if !moves.is_empty() {
            // A refused batch leaves them where they were; the next move tries again.
            let _ = windows.move_panels(&moves);
        }
    }
}

/// Where the panels that ride on `panel`'s right and bottom edges go when the panel grows by
/// `(dw, dh)` desktop units from its old frame `old`: a panel touching the old right edge moves
/// right by `dw`, one touching the old bottom edge moves down by `dh`, and a panel touching the
/// right or bottom edge of one of those rides with it by the same shift. Panels docked to other
/// panels (a library beside the main window) and panels on the left or above never move.
fn followers(
    windows: &dyn PanelWindows,
    panel: usize,
    old: ScreenRect,
    (dw, dh): (i32, i32),
) -> Vec<(usize, ScreenPoint)> {
    let Some((vis, _)) = visible(windows) else {
        return Vec::new();
    };
    // The panels that can still be reached, with their frames.
    let mut rest: Vec<(usize, ScreenRect)> = vis
        .iter()
        .filter(|&&p| p != panel)
        .filter_map(|&p| Some((p, windows.frame(p)?)))
        .collect();
    // The reached panels, each with its shift; the queue holds the frame to look from.
    let mut moves: Vec<(usize, ScreenPoint)> = Vec::new();
    let mut queue: Vec<(ScreenRect, (i32, i32))> = vec![(old, (0, 0))];
    let mut resized = true;
    while !queue.is_empty() {
        let (from, shift) = queue.remove(0);
        let (bottom, right) = (from.y.saturating_add(from.h), from.x.saturating_add(from.w));
        let mut i = 0;
        while i < rest.len() {
            let (p, f) = rest[i];
            let overlap_y = from.y < f.y.saturating_add(f.h) && f.y < bottom;
            let overlap_x = from.x < f.x.saturating_add(f.w) && f.x < right;
            let by = if f.x == right && overlap_y {
                Some(if resized { (dw, 0) } else { shift })
            } else if f.y == bottom && overlap_x {
                Some(if resized { (0, dh) } else { shift })
            } else {
                None
            };
            let Some((sx, sy)) = by else {
                i += 1;
                continue;
            };
            rest.swap_remove(i);
            if (sx, sy) != (0, 0) {
                moves.push((
                    p,
                    ScreenPoint {
                        x: f.x.saturating_add(sx),
                        y: f.y.saturating_add(sy),
                    },
                ));
            }
            queue.push((f, (sx, sy)));
        }
        resized = false;
    }
    moves
}

#[cfg(test)]
mod tests {
    use whipspot_core::skin::{Edge, Resize, SkinInfo, SkinTheme};

    use super::*;
    use crate::skin::dock::DragController;
    use crate::skin::fake::{FakeWindows, panel};

    fn pt(x: i32, y: i32) -> ScreenPoint {
        ScreenPoint { x, y }
    }

    /// Main over the equalizer over the resizable playlist, a library right of main and a
    /// "side" panel left of the playlist.
    fn skin() -> Skin {
        let mut playlist = panel("playlist", 275, 116, Some(("equalizer", Edge::Bottom)));
        playlist.resize = Some(Resize {
            min: Size { w: 275, h: 116 },
            step: Size { w: 25, h: 29 },
        });
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![
                panel("main", 275, 116, None),
                panel("equalizer", 275, 116, Some(("main", Edge::Bottom))),
                playlist,
                // Below the playlist.
                panel("under", 275, 50, Some(("playlist", Edge::Bottom))),
                // Right of the playlist.
                panel("beside", 100, 116, Some(("playlist", Edge::Right))),
                // Left of the playlist.
                panel("side", 60, 116, Some(("playlist", Edge::Left))),
                // Right of the main window, so its left edge is the playlist's right edge.
                panel("library", 200, 116, Some(("main", Edge::Right))),
            ],
            images: Vec::new(),
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    fn laid_out(skin: &Skin, k: i32) -> FakeWindows {
        let mut frames: Vec<ScreenRect> = skin
            .panels
            .iter()
            .map(|p| ScreenRect {
                x: 0,
                y: 0,
                w: p.size.w as i32 * k,
                h: p.size.h as i32 * k,
            })
            .collect();
        // The "side" panel sits left of the playlist, so the set starts at x = 160.
        let origins = crate::skin::dock::attach_layout(skin, &frames, pt(160, 50));
        for (f, o) in frames.iter_mut().zip(origins) {
            f.x = o.x;
            f.y = o.y;
        }
        let mut w = FakeWindows::new(frames);
        DragController::new(k as u32).dock_all(&mut w);
        w
    }

    fn sizes(skin: &Skin) -> Vec<(u32, u32)> {
        skin.panels.iter().map(|p| (p.size.w, p.size.h)).collect()
    }

    /// Presses the playlist grip, then moves the pointer by (`dx`, `dy`) desktop units.
    fn drag(
        c: &mut ResizeController,
        w: &mut FakeWindows,
        s: &Skin,
        live: &mut [(u32, u32)],
        k: u32,
        (dx, dy): (i32, i32),
    ) {
        c.press(2, pt(500, 400), live[2], k, (&*w, s));
        w.pointer = Some(pt(500 + dx, 400 + dy));
        c.moved(w, s, live);
    }

    #[test]
    fn a_drag_snaps_to_the_lattice_and_the_frame_reads_back() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        let before = w.frames[2];
        drag(&mut c, &mut w, &s, &mut live, 1, (60, 40));
        // +60 x +40 rounds to +50 x +29: 325 x 145.
        assert_eq!(live[2], (325, 145));
        assert_eq!(w.resizes, [(2, 325, 145)]);
        let f = w.frames[2];
        assert_eq!((f.x, f.y, f.w, f.h), (before.x, before.y, 325, 145));
        // Held on: the same size asks for nothing more.
        w.pointer = Some(pt(500 + 62, 400 + 38));
        c.moved(&mut w, &s, &mut live);
        assert_eq!(w.resizes.len(), 1);
    }

    #[test]
    fn a_small_drag_does_nothing() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        drag(&mut c, &mut w, &s, &mut live, 1, (10, 10));
        assert!(w.resizes.is_empty() && w.moves.is_empty());
        assert_eq!(live[2], (275, 116));
    }

    #[test]
    fn a_shrink_stops_at_the_minimum() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        drag(&mut c, &mut w, &s, &mut live, 1, (-400, -300));
        assert!(w.resizes.is_empty());
        assert_eq!(live[2], (275, 116));
        // Grow, then pull back past the minimum.
        drag(&mut c, &mut w, &s, &mut live, 1, (100, 60));
        assert_eq!(live[2], (375, 174));
        w.pointer = Some(pt(500 - 900, 400 - 900));
        c.moved(&mut w, &s, &mut live);
        assert_eq!(live[2], (275, 116));
        assert_eq!(w.resizes.last(), Some(&(2, 275, 116)));
    }

    #[test]
    fn panels_below_and_right_follow_and_the_one_on_the_left_stays() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        let at = |w: &FakeWindows| (3..6).map(|p| w.origin(p)).collect::<Vec<_>>();
        let before = at(&w);
        let main_before = w.origin(0);
        let library_before = w.origin(6);
        // The library's left edge is the playlist's right edge, and it starts at the top.
        assert_eq!(library_before.0, w.frames[2].x + w.frames[2].w);
        drag(&mut c, &mut w, &s, &mut live, 1, (60, 40));
        // dw = 50, dh = 29.
        let after = at(&w);
        assert_eq!(
            after[0],
            (before[0].0, before[0].1 + 29),
            "below moves down"
        );
        assert_eq!(
            after[1],
            (before[1].0 + 50, before[1].1),
            "right moves right"
        );
        assert_eq!(after[2], before[2], "left stays");
        assert_eq!(w.origin(0), main_before);
        assert_eq!(
            w.origin(6),
            library_before,
            "docked to main, not to the playlist"
        );
        // One batch.
        assert_eq!(w.moves.len(), 1);
        // The set is still docked after the release.
        let mut d = DragController::new(1);
        c.release();
        d.dock_all(&mut w);
        assert!(w.docked[3] && w.docked[4] && w.docked[5] && w.docked[6]);
        assert!(!c.resizing());
    }

    #[test]
    fn panels_docked_below_a_follower_ride_with_it() {
        let mut s = skin();
        // Below "under", and right of "under" (touching it only).
        s.panels
            .push(panel("deep", 275, 40, Some(("under", Edge::Bottom))));
        s.panels
            .push(panel("tail", 80, 50, Some(("under", Edge::Right))));
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        let before: Vec<_> = (7..9).map(|p| w.origin(p)).collect();
        drag(&mut c, &mut w, &s, &mut live, 1, (0, 58));
        // dh = 58: both ride down with "under".
        assert_eq!(w.origin(7), (before[0].0, before[0].1 + 58));
        assert_eq!(w.origin(8), (before[1].0, before[1].1 + 58));
    }

    #[test]
    fn a_panel_that_is_not_docked_stays_and_a_shrink_pulls_followers_back() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        // Pull "under" away from the set.
        w.frames[3].y += 500;
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        let far = w.origin(3);
        drag(&mut c, &mut w, &s, &mut live, 1, (0, 58));
        assert_eq!(w.origin(3), far);
        let beside = w.origin(4);
        drag(&mut c, &mut w, &s, &mut live, 1, (-0, -58));
        // Back to the minimum height: the right-hand panel did not move down (it starts at the
        // old top, not at or under the bottom).
        assert_eq!(w.origin(4), beside);
    }

    #[test]
    fn units_follow_the_pixel_scale_and_the_desktop_unit_size() {
        let s = skin();
        // k = 2 and 1.5 desktop units per physical pixel: one skin pixel is 3 units.
        let mut w = laid_out(&s, 2);
        w.units_per_pixel = 1.5;
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        // 150 units is 50 skin px, 87 units is 29 skin px.
        drag(&mut c, &mut w, &s, &mut live, 2, (150, 87));
        assert_eq!(live[2], (325, 145));
        let f = w.frames[2];
        assert_eq!((f.w, f.h), (975, 435));
    }

    #[test]
    fn the_size_is_bounded_by_the_largest_work_area() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        w.areas = vec![
            ScreenRect {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            ScreenRect {
                x: 800,
                y: 0,
                w: 400,
                h: 300,
            },
        ];
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        drag(&mut c, &mut w, &s, &mut live, 1, (5000, 5000));
        // 275 + 21 * 25 = 800, 116 + 16 * 29 = 580.
        assert_eq!(live[2], (800, 580));
    }

    #[test]
    fn a_panel_that_cannot_resize_never_does() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        c.press(0, pt(500, 400), live[0], 1, (&w, &s));
        assert!(!c.resizing());
        w.pointer = Some(pt(900, 900));
        c.moved(&mut w, &s, &mut live);
        assert!(w.resizes.is_empty());
        // Nor a hidden one.
        w.visible[2] = false;
        c.press(2, pt(500, 400), live[2], 1, (&w, &s));
        assert!(!c.resizing());
    }

    #[test]
    fn a_window_that_has_gone_changes_nothing() {
        let s = skin();
        let mut w = laid_out(&s, 1);
        let mut live = sizes(&s);
        let mut c = ResizeController::default();
        c.press(2, pt(500, 400), live[2], 1, (&w, &s));
        w.pointer = Some(pt(600, 500));
        w.frames.truncate(2);
        c.moved(&mut w, &s, &mut live);
        assert_eq!(live[2], (275, 116));
        assert!(w.moves.is_empty());
    }
}
