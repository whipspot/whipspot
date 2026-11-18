//! Where panels go: the start layout from the skin's attach rules, the drag that moves a docked
//! set or one panel with snapping, and the docked-set bookkeeping. Pure over [`PanelWindows`],
//! so it runs on a fake in tests.
//!
//! Two index spaces: the panel index `p` (the skin's panel order, the `PanelWindows` index) and
//! the visible index `v` the dock maths sees (visible panels only, ascending `p`). A hidden
//! panel's frame never reaches the dock maths, so it neither moves nor docks.

use lune_ui::desktop::{DesktopPoint, DesktopRect};
use lune_ui::dock::{DockDrag, docked_sets, docked_with};
use whipspot_core::skin::{Edge, PanelWindows, ScreenPoint, ScreenRect, Skin};

use super::trace;

/// The snap distance in skin pixels (scaled by the pixel scale and desktop units per pixel).
const SNAP_SKIN_PX: f32 = 10.0;

fn to_rect(r: ScreenRect) -> DesktopRect {
    DesktopRect::new(r.x, r.y, r.w, r.h)
}

fn to_point(p: ScreenPoint) -> DesktopPoint {
    DesktopPoint::new(p.x, p.y)
}

fn from_point(p: DesktopPoint) -> ScreenPoint {
    ScreenPoint { x: p.x, y: p.y }
}

/// The start origin of every panel. The first panel sits at `leader`; an attached panel sits
/// against its `to`'s edge (the touching edges aligned at their left or top), resolved in
/// dependency order; a panel without a resolvable attach is stacked below the lowest placed
/// panel at the leader's x. Hidden panels are placed too. Sizes come from `frames` (a missing
/// frame counts as empty).
pub(crate) fn attach_layout(
    skin: &Skin,
    frames: &[ScreenRect],
    leader: ScreenPoint,
) -> Vec<ScreenPoint> {
    let n = skin.panels.len();
    let size = |p: usize| frames.get(p).map_or((0, 0), |f| (f.w, f.h));
    let target = |p: usize| -> Option<(usize, Edge)> {
        let a = skin.panels[p].dock.attach.as_ref()?;
        let to = skin.panels.iter().position(|q| q.id == a.to)?;
        (to != p).then_some((to, a.edge))
    };
    let mut pos: Vec<Option<ScreenPoint>> = vec![None; n];
    if n == 0 {
        return Vec::new();
    }
    pos[0] = Some(leader);
    loop {
        // Resolve every attach whose target is placed, until nothing changes.
        let mut changed = true;
        while changed {
            changed = false;
            for p in 1..n {
                if pos[p].is_some() {
                    continue;
                }
                let Some((to, edge)) = target(p) else {
                    continue;
                };
                let Some(t) = pos[to] else {
                    continue;
                };
                let (tw, th) = size(to);
                let (w, h) = size(p);
                pos[p] = Some(match edge {
                    Edge::Bottom => ScreenPoint {
                        x: t.x,
                        y: t.y.saturating_add(th),
                    },
                    Edge::Top => ScreenPoint {
                        x: t.x,
                        y: t.y.saturating_sub(h),
                    },
                    Edge::Right => ScreenPoint {
                        x: t.x.saturating_add(tw),
                        y: t.y,
                    },
                    Edge::Left => ScreenPoint {
                        x: t.x.saturating_sub(w),
                        y: t.y,
                    },
                });
                changed = true;
            }
        }
        // Stack the next unplaced panel: one without a resolvable attach first, else (a cycle)
        // any.
        let unplaced = (1..n).filter(|&p| pos[p].is_none());
        let next = unplaced
            .clone()
            .find(|&p| target(p).is_none())
            .or_else(|| unplaced.clone().next());
        let Some(p) = next else {
            break;
        };
        let bottom = (0..n)
            .filter_map(|q| pos[q].map(|o| o.y.saturating_add(size(q).1)))
            .max()
            .unwrap_or(leader.y);
        pos[p] = Some(ScreenPoint {
            x: leader.x,
            y: bottom,
        });
    }
    pos.into_iter().map(|p| p.unwrap_or(leader)).collect()
}

/// A drag in progress.
struct Active {
    drag: DockDrag,
    /// Visible index to panel index, as at the press.
    vis: Vec<usize>,
    /// Snap distance in desktop units.
    within: i32,
}

/// Moves panels while one is dragged and keeps track of which are docked to the leader.
pub(crate) struct DragController {
    /// Physical pixels per skin pixel (the panel set's pixel scale).
    k: u32,
    active: Option<Active>,
    /// Per panel: docked to the leader at the last recompute.
    docked: Vec<bool>,
}

/// The visible panels, ascending, and their frames; `None` when any frame is unknown.
pub(super) fn visible(windows: &dyn PanelWindows) -> Option<(Vec<usize>, Vec<DesktopRect>)> {
    let vis: Vec<usize> = (0..windows.count())
        .filter(|&p| windows.is_visible(p))
        .collect();
    let rects = vis
        .iter()
        .map(|&p| windows.frame(p).map(to_rect))
        .collect::<Option<Vec<_>>>()?;
    Some((vis, rects))
}

impl DragController {
    pub fn new(k: u32) -> Self {
        DragController {
            k: k.max(1),
            active: None,
            docked: Vec::new(),
        }
    }

    pub fn dragging(&self) -> bool {
        self.active.is_some()
    }

    /// Whether `panel` was docked to the leader at the last recompute.
    pub fn is_docked(&self, panel: usize) -> bool {
        self.docked.get(panel).copied().unwrap_or(false)
    }

    /// Starts a drag of `panel` with the desktop pointer at `pointer`. Pressing the first panel
    /// moves its docked set (the members in its dock group); any other panel moves alone. A
    /// hidden or unknown panel starts nothing.
    pub fn press(
        &mut self,
        panel: usize,
        pointer: ScreenPoint,
        windows: &dyn PanelWindows,
        skin: &Skin,
    ) {
        self.active = None;
        let Some(spec) = skin.panels.get(panel) else {
            return;
        };
        let Some((vis, rects)) = visible(windows) else {
            return;
        };
        let Some(v) = vis.iter().position(|&p| p == panel) else {
            return;
        };
        let group = skin.panels[0].dock.group.as_ref();
        let moving: Vec<usize> = match group {
            Some(g) if panel == 0 => docked_with(v, &rects)
                .into_iter()
                .filter(|&u| {
                    u == v
                        || skin
                            .panels
                            .get(vis[u])
                            .is_some_and(|q| q.dock.group.as_ref() == Some(g))
                })
                .collect(),
            _ => vec![v],
        };
        let within = if spec.dock.snap {
            let upp = windows.units_per_pixel();
            let d = (SNAP_SKIN_PX * self.k as f32 * upp).round();
            if d.is_finite() { d as i32 } else { 0 }
        } else {
            0
        };
        if trace::enabled() {
            trace::line(format_args!(
                "press panel={panel} pointer={} leader={} k={} upp={} within={within} moving={}",
                trace::point(Some(pointer)),
                trace::rect(windows.frame(0)),
                self.k,
                windows.units_per_pixel(),
                moving.len()
            ));
        }
        self.active = Some(Active {
            drag: DockDrag::start(to_point(pointer), &rects, &moving),
            vis,
            within,
        });
    }

    /// Follows the desktop pointer: moves the dragged panels, snapped, changing only origins that
    /// differ. Does nothing without a drag, a pointer or the frames.
    pub fn moved(&mut self, windows: &mut dyn PanelWindows) {
        let Some(active) = &self.active else {
            return;
        };
        let Some(pointer) = windows.pointer() else {
            return;
        };
        let Some(rects) = active
            .vis
            .iter()
            .map(|&p| windows.frame(p).map(to_rect))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let areas: Vec<DesktopRect> = windows.work_areas().into_iter().map(to_rect).collect();
        let moves: Vec<(usize, ScreenPoint)> = active
            .drag
            .update(to_point(pointer), &rects, &areas, active.within)
            .into_iter()
            .filter(|&(v, o)| rects[v].origin() != o)
            .map(|(v, o)| (active.vis[v], from_point(o)))
            .collect();
        if trace::enabled() {
            trace::line(format_args!(
                "move pointer={} leader={} upp={} moves={}",
                trace::point(Some(pointer)),
                trace::rect(windows.frame(0)),
                windows.units_per_pixel(),
                trace::moves(&moves)
            ));
        }
        if !moves.is_empty() {
            // A refused batch is retried by the next move.
            let _ = windows.move_panels(&moves);
        }
    }

    /// Ends the drag and recomputes which panels are docked.
    pub fn release(&mut self, windows: &mut dyn PanelWindows) {
        if trace::enabled() && self.active.is_some() {
            trace::line(format_args!(
                "release pointer={} leader={}",
                trace::point(windows.pointer()),
                trace::rect(windows.frame(0))
            ));
        }
        self.active = None;
        self.dock_all(windows);
    }

    /// Recomputes the docked flags: a visible panel is docked when it is in the leader's docked
    /// set; hidden panels count as not docked (and are not told).
    pub fn dock_all(&mut self, windows: &mut dyn PanelWindows) {
        let count = windows.count();
        self.docked = vec![false; count];
        let Some((vis, rects)) = visible(windows) else {
            return;
        };
        let Some(v0) = vis.iter().position(|&p| p == 0) else {
            return;
        };
        let sets = docked_sets(&rects);
        for (v, &p) in vis.iter().enumerate() {
            if p == 0 {
                continue;
            }
            let docked = sets[v] == sets[v0];
            self.docked[p] = docked;
            windows.set_docked(p, docked);
        }
    }

    /// The pixel scale changed to `new_k`, and the desktop units per skin pixel from `old_unit`
    /// (`units_per_pixel() * k` before the change) to `units_per_pixel() * new_k` now: every
    /// docked member keeps its place relative to the leader, scaled by that ratio (rounded toward
    /// the leader). On a desktop in physical pixels (Windows) the ratio is `new_k / old_k`; on one
    /// in points (macOS) a move between a 2x and a 1x screen leaves the places as they are.
    pub fn rescale(&mut self, old_unit: f32, new_k: u32, windows: &mut dyn PanelWindows) {
        self.k = new_k.max(1);
        let new_unit = windows.units_per_pixel() * self.k as f32;
        let ratio = f64::from(new_unit) / f64::from(old_unit);
        let leader = windows.frame(0);
        let mut moves: Vec<(usize, ScreenPoint)> = Vec::new();
        if let Some(l) = leader
            && ratio.is_finite()
            && ratio > 0.0
        {
            let scale = |o: i32, l: i32| -> i32 {
                let d = (f64::from(o) - f64::from(l)) * ratio;
                // A ratio a float step off a whole one must not lose a unit to truncation.
                let d = if (d - d.round()).abs() < 1e-3 {
                    d.round()
                } else {
                    d.trunc()
                };
                (f64::from(l) + d).clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
            };
            moves = (1..windows.count())
                .filter(|&p| self.is_docked(p))
                .filter_map(|p| windows.frame(p).map(|f| (p, f)))
                .map(|(p, f)| {
                    (
                        p,
                        ScreenPoint {
                            x: scale(f.x, l.x),
                            y: scale(f.y, l.y),
                        },
                    )
                })
                .filter(|&(p, o)| windows.frame(p).is_some_and(|f| (f.x, f.y) != (o.x, o.y)))
                .collect();
        }
        if trace::enabled() {
            trace::line(format_args!(
                "rescale leader={} k={} upp={} unit={old_unit}->{new_unit} moves={}",
                trace::rect(leader),
                self.k,
                windows.units_per_pixel(),
                trace::moves(&moves)
            ));
        }
        if !moves.is_empty() {
            let _ = windows.move_panels(&moves);
        }
    }
}

#[cfg(test)]
mod tests {
    use whipspot_core::skin::{SkinInfo, SkinTheme};

    use super::*;
    use crate::skin::fake::{FakeWindows, panel};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> ScreenRect {
        ScreenRect { x, y, w, h }
    }

    fn pt(x: i32, y: i32) -> ScreenPoint {
        ScreenPoint { x, y }
    }

    /// Four panels: equalizer under main, playlist under equalizer, library right of main
    /// (hidden at start).
    fn skin() -> Skin {
        let mut library = panel("library", 200, 300, Some(("main", Edge::Right)));
        library.visible_at_start = false;
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![
                panel("main", 275, 116, None),
                panel("equalizer", 275, 116, Some(("main", Edge::Bottom))),
                panel("playlist", 275, 232, Some(("equalizer", Edge::Bottom))),
                library,
            ],
            images: Vec::new(),
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    fn sized(skin: &Skin, k: i32) -> Vec<ScreenRect> {
        skin.panels
            .iter()
            .map(|p| rect(0, 0, p.size.w as i32 * k, p.size.h as i32 * k))
            .collect()
    }

    /// Windows laid out by `attach_layout` at (100, 50), all visible, docks recomputed.
    fn laid_out(skin: &Skin) -> (FakeWindows, DragController) {
        let mut frames = sized(skin, 1);
        let origins = attach_layout(skin, &frames, pt(100, 50));
        for (f, o) in frames.iter_mut().zip(origins) {
            f.x = o.x;
            f.y = o.y;
        }
        let mut w = FakeWindows::new(frames);
        let mut c = DragController::new(1);
        c.dock_all(&mut w);
        (w, c)
    }

    #[test]
    fn attach_layout_places_every_panel_exactly() {
        let s = skin();
        let o = attach_layout(&s, &sized(&s, 1), pt(100, 50));
        assert_eq!(o, [pt(100, 50), pt(100, 166), pt(100, 282), pt(375, 50)]);
        // Sizes come from the frames (k = 2).
        let o = attach_layout(&s, &sized(&s, 2), pt(0, 0));
        assert_eq!(o, [pt(0, 0), pt(0, 232), pt(0, 464), pt(550, 0)]);
    }

    #[test]
    fn attach_layout_resolves_out_of_order_and_stacks_the_rest() {
        let mut s = skin();
        // Playlist listed before the equalizer it hangs under; the library attaches nowhere.
        s.panels.swap(1, 2);
        s.panels[3].dock.attach = None;
        s.panels
            .push(panel("top", 50, 10, Some(("main", Edge::Top))));
        s.panels
            .push(panel("left", 30, 40, Some(("main", Edge::Left))));
        let o = attach_layout(&s, &sized(&s, 1), pt(100, 50));
        assert_eq!(o[1], pt(100, 282)); // playlist under the equalizer
        assert_eq!(o[2], pt(100, 166)); // equalizer under main
        assert_eq!(o[3], pt(100, 514)); // stacked under the lowest (playlist ends at 514)
        assert_eq!(o[4], pt(100, 40));
        assert_eq!(o[5], pt(70, 50));
    }

    #[test]
    fn leader_drag_moves_the_docked_set_and_leaves_a_detached_panel() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        // Detach the library: move it away first.
        w.frames[3].x = 900;
        c.dock_all(&mut w);
        assert_eq!(w.docked, [false, true, true, false]);
        c.press(0, pt(110, 55), &w, &s);
        w.pointer = Some(pt(140, 75));
        c.moved(&mut w);
        assert_eq!(w.origin(0), (130, 70));
        assert_eq!(w.origin(1), (130, 186));
        assert_eq!(w.origin(2), (130, 302));
        assert_eq!(w.origin(3), (900, 50));
        c.release(&mut w);
        assert_eq!(w.docked, [false, true, true, false]);
    }

    #[test]
    fn leader_drag_leaves_a_member_of_another_group() {
        let mut s = skin();
        s.panels[2].dock.group = Some("other".to_owned());
        let (mut w, mut c) = laid_out(&s);
        c.press(0, pt(110, 55), &w, &s);
        w.pointer = Some(pt(410, 355));
        c.moved(&mut w);
        assert_eq!(w.origin(1), (400, 466));
        assert_eq!(w.origin(2), (100, 282));
    }

    #[test]
    fn member_drag_moves_one_panel() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        c.press(2, pt(200, 300), &w, &s);
        w.pointer = Some(pt(700, 600));
        c.moved(&mut w);
        assert_eq!(w.origin(2), (600, 582));
        assert_eq!(w.origin(0), (100, 50));
        assert_eq!(w.origin(1), (100, 166));
        c.release(&mut w);
        assert!(!w.docked[2]);
        assert!(w.docked[1]);
    }

    #[test]
    fn member_released_near_main_snaps_touching_it() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        // The library starts touching main's right edge (x 375); drag it off, then back to
        // 6 px right of the edge.
        c.press(3, pt(400, 60), &w, &s);
        w.pointer = Some(pt(800, 60));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (775, 50));
        c.release(&mut w);
        assert!(!w.docked[3]);
        c.press(3, pt(800, 60), &w, &s);
        w.pointer = Some(pt(406, 63));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (375, 50));
        c.release(&mut w);
        assert!(w.docked[3]);
    }

    #[test]
    fn no_snap_when_the_panel_does_not_snap() {
        let mut s = skin();
        s.panels[3].dock.snap = false;
        let (mut w, mut c) = laid_out(&s);
        c.press(3, pt(400, 60), &w, &s);
        w.pointer = Some(pt(406, 63));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (381, 53));
        c.release(&mut w);
        assert!(!w.docked[3]);
    }

    #[test]
    fn snap_distance_scales_with_k_and_units() {
        let s = skin();
        // The library starts 5 px off main's right edge (x 380).
        let (mut w, _) = laid_out(&s);
        w.frames[3].x = 380;
        // k = 2: 20 units; dragged 10 px further (a 15 px gap), it still snaps.
        let mut c = DragController::new(2);
        c.press(3, pt(400, 60), &w, &s);
        w.pointer = Some(pt(410, 60));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (375, 50));
        // k = 1 with half a unit per pixel: 5 units; a 6 px gap stays.
        let (mut w, _) = laid_out(&s);
        w.frames[3].x = 380;
        w.units_per_pixel = 0.5;
        let mut c = DragController::new(1);
        c.press(3, pt(400, 60), &w, &s);
        w.pointer = Some(pt(401, 60));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (381, 50));
    }

    #[test]
    fn hidden_panel_neither_moves_nor_docks() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        w.visible[3] = false;
        c.press(0, pt(110, 55), &w, &s);
        w.pointer = Some(pt(140, 75));
        c.moved(&mut w);
        assert_eq!(w.origin(3), (375, 50));
        assert!(w.moves.iter().flatten().all(|&(p, _)| p != 3));
        c.release(&mut w);
        assert!(!c.is_docked(3));
        // A press on a hidden panel starts nothing.
        c.press(3, pt(400, 60), &w, &s);
        assert!(!c.dragging());
    }

    #[test]
    fn only_changed_origins_are_moved() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        c.press(0, pt(110, 55), &w, &s);
        w.pointer = Some(pt(110, 55));
        c.moved(&mut w);
        assert!(w.moves.is_empty());
        w.pointer = None;
        c.moved(&mut w);
        assert!(w.moves.is_empty());
    }

    #[test]
    fn rescale_one_to_two_doubles_docked_offsets() {
        let s = skin();
        let (mut w, mut c) = laid_out(&s);
        w.frames[3].x = 900;
        c.dock_all(&mut w);
        // A desktop in physical pixels (one unit per pixel): units per skin px 1 -> 2.
        c.rescale(1.0, 2, &mut w);
        assert_eq!(w.origin(0), (100, 50));
        assert_eq!(w.origin(1), (100, 282));
        assert_eq!(w.origin(2), (100, 514));
        assert_eq!(w.origin(3), (900, 50));
        // And back, rounded toward the leader.
        w.frames[1].y += 1;
        c.rescale(2.0, 1, &mut w);
        assert_eq!(w.origin(1), (100, 166));
    }

    #[test]
    fn rescale_on_a_point_desktop_moves_nothing_when_only_the_backing_scale_changed() {
        // macOS: a 2x screen (half a point per pixel, k 2) to a 1x one (a point per pixel, k 1).
        // A panel keeps its size in points, so the docked places stay.
        let s = skin();
        let (mut w, _) = laid_out(&s);
        w.units_per_pixel = 0.5;
        let mut c = DragController::new(2);
        c.dock_all(&mut w);
        let before: Vec<_> = (0..4).map(|p| w.origin(p)).collect();
        w.units_per_pixel = 1.0;
        c.rescale(0.5 * 2.0, 1, &mut w);
        let after: Vec<_> = (0..4).map(|p| w.origin(p)).collect();
        assert_eq!(after, before);
        assert!(w.moves.is_empty(), "nothing is moved");
        // And back to the 2x screen.
        w.units_per_pixel = 0.5;
        c.rescale(1.0, 2, &mut w);
        assert!(w.moves.is_empty());
        // Windows: one unit per pixel, k 2 -> 1 halves the offsets as before.
        let (mut w, _) = laid_out(&s);
        let mut c = DragController::new(2);
        c.dock_all(&mut w);
        c.rescale(2.0, 1, &mut w);
        assert_eq!(w.origin(0), (100, 50));
        assert_eq!(w.origin(1), (100, 108));
        assert_eq!(w.origin(2), (100, 166));
        assert_eq!(w.origin(3), (237, 50));
    }
}
