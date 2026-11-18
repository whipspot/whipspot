//! The open panel set of a running skin: one OS window per panel, grouped under the first (the
//! leader, the only one with a taskbar button), each sized `size * k` physical px and shaped in
//! skin px at scale `k` (D4). [`HostPanelWindows`] is the core's `PanelWindows` seam over them,
//! on `lune_ui::desktop` and `lune_ui::group`; the skin front-end drives the windows only
//! through it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lune_ui::desktop::{self, DesktopPoint, DesktopRect, ShapeFollower, ShapeMode};
use lune_ui::group::WindowGroup;
use whipspot_core::skin::{
    MAX_PANEL_SIDE, PanelWindows, PanelWindowsError, ScreenPoint, ScreenRect,
};
use whipspot_ui::skin::PanelSpec;
use winit::dpi::PhysicalSize;
use winit::event::MouseButton;
use winit::window::{Window, WindowId};

/// How often a set shaped by the pointer (macOS) asks where the pointer is.
pub(super) const FOLLOW_TICK: Duration = Duration::from_millis(33);

/// The physical inner size of a panel of `size` skin px at `k` physical px per skin px.
pub(super) fn physical(size: (u32, u32), k: u32) -> PhysicalSize<u32> {
    PhysicalSize::new(size.0.saturating_mul(k), size.1.saturating_mul(k))
}

fn screen_rect(r: DesktopRect) -> ScreenRect {
    ScreenRect {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
    }
}

/// Desktop units per physical pixel of `window` now: 1 where the desktop is in physical pixels
/// (Windows), `1 / scale` where it is in points (macOS).
fn units_per_pixel(window: &Window) -> f32 {
    let scale = window.scale_factor() as f32;
    let upp = desktop::units_per_logical(window) / scale;
    if upp.is_finite() && upp > 0.0 {
        upp
    } else {
        1.0
    }
}

/// Which mouse buttons are down on each panel window, from the window's own button events. While
/// one is, the window keeps taking the pointer, so the release of a press it took comes back to
/// it (a pointer-shaped window, macOS). Losing focus clears a window's buttons, so a release that
/// never came cannot keep it taking clicks for ever.
#[derive(Debug, Default)]
pub(super) struct Buttons {
    down: Vec<u8>,
}

impl Buttons {
    fn bit(button: MouseButton) -> u8 {
        match button {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Back => 8,
            MouseButton::Forward => 16,
            MouseButton::Other(_) => 32,
        }
    }

    fn push(&mut self) {
        self.down.push(0);
    }

    /// Button `button` of panel `panel` went down or up; true when that left no button down where
    /// one was.
    pub(super) fn set(&mut self, panel: usize, button: MouseButton, pressed: bool) -> bool {
        let Some(d) = self.down.get_mut(panel) else {
            return false;
        };
        let was = *d != 0;
        if pressed {
            *d |= Self::bit(button);
        } else {
            *d &= !Self::bit(button);
        }
        was && *d == 0
    }

    /// Forgets panel `panel`'s buttons; true when one was down.
    pub(super) fn clear(&mut self, panel: usize) -> bool {
        self.down
            .get_mut(panel)
            .is_some_and(|d| std::mem::take(d) != 0)
    }

    /// Whether some button is down on panel `panel`.
    pub(super) fn any(&self, panel: usize) -> bool {
        self.down.get(panel).is_some_and(|&d| d != 0)
    }

    /// Whether some button is down on any panel.
    pub(super) fn any_panel(&self) -> bool {
        self.down.iter().any(|&d| d != 0)
    }

    /// The buttons held on panel `panel` that the OS's button mask `os` says are up: the releases
    /// the window never got. None for an unknown panel or an OS that cannot say.
    pub(super) fn os_released(&self, panel: usize, os: Option<u64>) -> Vec<MouseButton> {
        os_released(self.down.get(panel).copied().unwrap_or(0), os)
    }
}

/// The buttons of the held set `held` (the [`Buttons`] bits) that the OS's button mask `os` (bit 0
/// left, 1 right, 2 middle, 3 back, 4 forward, then the other buttons by number) says are up, in
/// bit order. `None`, an OS that cannot say, releases nothing. The held "other" buttons share one
/// bit, so they count as up only when the OS holds none of its buttons from number 5 on.
pub(super) fn os_released(held: u8, os: Option<u64>) -> Vec<MouseButton> {
    let Some(os) = os else {
        return Vec::new();
    };
    const NAMED: [(MouseButton, u64); 5] = [
        (MouseButton::Left, 1),
        (MouseButton::Right, 1 << 1),
        (MouseButton::Middle, 1 << 2),
        (MouseButton::Back, 1 << 3),
        (MouseButton::Forward, 1 << 4),
    ];
    let mut up: Vec<MouseButton> = NAMED
        .into_iter()
        .filter(|&(b, mask)| held & Buttons::bit(b) != 0 && os & mask == 0)
        .map(|(b, _)| b)
        .collect();
    let other = MouseButton::Other(5);
    if held & Buttons::bit(other) != 0 && os >> 5 == 0 {
        up.push(other);
    }
    up
}

/// The windows of one open skin, leader first.
pub(super) struct PanelSet {
    /// The skin generation the windows were opened for.
    pub(super) generation: u64,
    /// Physical px per skin px for every panel of the set (D4): from the leader's scale.
    pub(super) k: u32,
    specs: Vec<PanelSpec>,
    /// Leader first; dropped members first (`close`).
    windows: Vec<Arc<Window>>,
    group: Option<WindowGroup>,
    /// What `set_visible` last asked, for an OS that cannot say.
    visible: Vec<bool>,
    modes: Vec<ShapeMode>,
    followers: Vec<ShapeFollower>,
    /// The group was minimised by a panel's control and not yet restored.
    minimised: bool,
    /// When the pointer followers ask next (only while some panel is shaped by the pointer).
    pub(super) next_tick: Instant,
    /// The mouse buttons down per panel.
    pub(super) buttons: Buttons,
    /// The leader's desktop units per physical pixel at its last scale (at the open, then at each
    /// of its scale changes): what the docked places were laid out in.
    upp: f32,
}

impl PanelSet {
    /// A set of `leader` alone; the members come with [`PanelSet::add`].
    pub(super) fn new(generation: u64, k: u32, specs: Vec<PanelSpec>, leader: Arc<Window>) -> Self {
        let mut buttons = Buttons::default();
        buttons.push();
        PanelSet {
            upp: units_per_pixel(&leader),
            buttons,
            generation,
            k,
            specs,
            group: Some(WindowGroup::new(leader.clone())),
            windows: vec![leader],
            visible: vec![false],
            modes: vec![ShapeMode::Unsupported],
            followers: vec![ShapeFollower::default()],
            minimised: false,
            next_tick: Instant::now() + FOLLOW_TICK,
        }
    }

    /// The group, for the attributes of a member window.
    pub(super) fn group(&self) -> Option<&WindowGroup> {
        self.group.as_ref()
    }

    /// Adds a member window (built with the group's member attributes).
    pub(super) fn add(&mut self, member: Arc<Window>) -> Result<(), String> {
        let group = self.group.as_mut().ok_or("panel group: closed")?;
        group
            .add(member.clone())
            .map_err(|_| "panel group: a member was refused".to_owned())?;
        self.windows.push(member);
        self.visible.push(false);
        self.modes.push(ShapeMode::Unsupported);
        self.followers.push(ShapeFollower::default());
        self.buttons.push();
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.windows.len()
    }

    pub(super) fn ids(&self) -> Vec<WindowId> {
        self.windows.iter().map(|w| w.id()).collect()
    }

    /// Gives panel `i` its shape at the set's `k` (the OS does not rescale a shape, so this runs
    /// after every size or scale change). A refused shape leaves the whole frame.
    pub(super) fn reshape(&mut self, i: usize) {
        let (Some(window), Some(spec)) = (self.windows.get(i), self.specs.get(i)) else {
            return;
        };
        self.modes[i] = desktop::set_window_shape(window, spec.shape.as_ref(), self.k as f32)
            .unwrap_or(ShapeMode::Unsupported);
    }

    pub(super) fn reshape_all(&mut self) {
        for i in 0..self.windows.len() {
            self.reshape(i);
        }
    }

    /// Asks panel `i`'s window for its size at the set's `k`.
    pub(super) fn resize(&self, i: usize) {
        if let (Some(window), Some(spec)) = (self.windows.get(i), self.specs.get(i)) {
            let _ = window.request_inner_size(physical(spec.size, self.k));
        }
    }

    /// The physical inner size of panel `i` at the set's `k`.
    pub(super) fn size_of(&self, i: usize) -> Option<PhysicalSize<u32>> {
        self.specs.get(i).map(|s| physical(s.size, self.k))
    }

    /// Whether some panel is shaped by following the pointer (macOS): the host then ticks.
    pub(super) fn follows(&self) -> bool {
        self.modes.contains(&ShapeMode::Pointer)
    }

    /// Whether the host ticks for the set: a panel follows the pointer, or a button is held on a
    /// panel and the OS can say when it goes up (`os_buttons`).
    pub(super) fn ticks(&self, os_buttons: bool) -> bool {
        self.follows() || (os_buttons && self.buttons.any_panel())
    }

    /// The leader's scale changed: its desktop units per physical pixel before and after the
    /// change; the set keeps the one after.
    pub(super) fn leader_rescaled(&mut self) -> (f32, f32) {
        let after = self.windows.first().map_or(1.0, |w| units_per_pixel(w));
        (std::mem::replace(&mut self.upp, after), after)
    }

    /// The follower tick: each pointer-shaped panel lets clicks through outside its shape, except
    /// one with a mouse button down: it keeps taking the pointer until the release.
    pub(super) fn follow_tick(&mut self) {
        let pointer = desktop::pointer();
        for i in 0..self.windows.len() {
            if self.modes[i] == ShapeMode::Pointer && !self.buttons.any(i) {
                let shape = self.specs[i].shape.as_ref();
                let _ = self.followers[i].update(&self.windows[i], shape, self.k as f32, pointer);
            }
        }
        self.next_tick = Instant::now() + FOLLOW_TICK;
    }

    /// Window `id` of the set came to the front: the rest of the group follows (macOS), and
    /// the leader coming back from the OS's own un-minimise restores the group.
    pub(super) fn focused(&mut self, id: WindowId) {
        let Some(group) = &self.group else {
            return;
        };
        group.raise(id);
        if self.minimised && self.windows.first().is_some_and(|w| w.id() == id) {
            self.minimised = false;
            group.restore();
        }
    }

    /// Brings the set back to the front for the user: the group is restored, and the leader is
    /// shown and focused (the tray's Show).
    pub(super) fn show(&mut self) {
        if let Some(group) = &self.group {
            group.restore();
        }
        self.minimised = false;
        if let Some(leader) = self.windows.first() {
            leader.set_minimized(false);
            leader.set_visible(true);
            leader.focus_window();
        }
    }

    /// Drops the windows: the group first (its members before its leader), then the set's own
    /// handles, members first (Windows destroys owned windows with their owner). The host has
    /// dropped its own per-window state before.
    pub(super) fn close(mut self) {
        drop(self.group.take());
        while let Some(window) = self.windows.pop() {
            drop(window);
        }
    }
}

/// The core's panel-windows seam over an open [`PanelSet`].
pub(super) struct HostPanelWindows<'s> {
    set: &'s mut PanelSet,
}

impl<'s> HostPanelWindows<'s> {
    pub(super) fn new(set: &'s mut PanelSet) -> Self {
        HostPanelWindows { set }
    }
}

impl PanelWindows for HostPanelWindows<'_> {
    fn count(&self) -> usize {
        self.set.windows.len()
    }

    fn frame(&self, panel: usize) -> Option<ScreenRect> {
        desktop::window_frame(self.set.windows.get(panel)?).map(screen_rect)
    }

    fn is_visible(&self, panel: usize) -> bool {
        match self.set.windows.get(panel) {
            Some(w) => w.is_visible().unwrap_or(self.set.visible[panel]),
            None => false,
        }
    }

    fn work_areas(&self) -> Vec<ScreenRect> {
        match self.set.windows.first() {
            Some(leader) => desktop::monitors(leader)
                .into_iter()
                .map(|m| screen_rect(m.work))
                .collect(),
            None => Vec::new(),
        }
    }

    fn pointer(&self) -> Option<ScreenPoint> {
        desktop::pointer().map(|p| ScreenPoint { x: p.x, y: p.y })
    }

    fn units_per_pixel(&self) -> f32 {
        self.set.windows.first().map_or(1.0, |w| units_per_pixel(w))
    }

    fn move_panels(&mut self, moves: &[(usize, ScreenPoint)]) -> Result<(), PanelWindowsError> {
        let mut batch = Vec::with_capacity(moves.len());
        for &(i, at) in moves {
            let window = self.set.windows.get(i).ok_or(PanelWindowsError::Failed)?;
            batch.push((&**window, DesktopPoint::new(at.x, at.y)));
        }
        desktop::move_windows(&batch).map_err(|_| PanelWindowsError::Failed)
    }

    fn resize_panel(&mut self, panel: usize, w: i32, h: i32) -> Result<(), PanelWindowsError> {
        if panel >= self.set.windows.len() || w <= 0 || h <= 0 {
            return Err(PanelWindowsError::Failed);
        }
        // Desktop units to skin px at the set's scale; the front-end passes whole skin pixels.
        let unit = self.units_per_pixel() * self.set.k as f32;
        let skin = |v: i32| -> Result<u32, PanelWindowsError> {
            let px = (v as f32 / unit).round();
            if px.is_finite() && (1.0..=MAX_PANEL_SIDE as f32).contains(&px) {
                Ok(px as u32)
            } else {
                Err(PanelWindowsError::Failed)
            }
        };
        let size = (skin(w)?, skin(h)?);
        // The spec keeps the size, so a later scale change resizes from it; the shape does not
        // follow a size change by itself.
        self.set.specs[panel].size = size;
        self.set.resize(panel);
        self.set.reshape(panel);
        Ok(())
    }

    fn set_visible(&mut self, panel: usize, visible: bool) {
        // The leader owns the others and is the only taskbar button: never hidden.
        if panel == 0 && !visible {
            return;
        }
        if let Some(window) = self.set.windows.get(panel) {
            window.set_visible(visible);
            self.set.visible[panel] = visible;
            if visible {
                window.request_redraw();
            }
        }
    }

    fn set_docked(&mut self, panel: usize, docked: bool) {
        if panel == 0 {
            return;
        }
        if let (Some(window), Some(group)) = (self.set.windows.get(panel), &mut self.set.group) {
            let _ = group.set_docked(window.id(), docked);
        }
    }

    fn set_always_on_top(&mut self, on: bool) {
        if let Some(group) = &self.set.group {
            group.set_always_on_top(on);
        }
    }

    fn minimise(&mut self) {
        if let Some(group) = &self.set.group {
            group.minimise();
            self.set.minimised = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_holds_while_a_button_is_down_and_focus_loss_clears_it() {
        let mut b = Buttons::default();
        b.push();
        b.push();
        assert!(!b.any(0) && !b.any(1));
        assert!(!b.set(0, MouseButton::Left, true));
        assert!(b.any(0), "held after the press");
        assert!(!b.any(1), "per panel");
        // A second button and its release: the first is still down, no "all up".
        assert!(!b.set(0, MouseButton::Right, true));
        assert!(!b.set(0, MouseButton::Right, false));
        assert!(b.any(0));
        assert!(b.set(0, MouseButton::Left, false), "all up");
        assert!(!b.any(0), "follows again after the release");
        // A release with nothing down is not an "all up".
        assert!(!b.set(0, MouseButton::Left, false));
        // A release that never came: focus loss clears it.
        b.set(1, MouseButton::Left, true);
        assert!(b.clear(1));
        assert!(!b.any(1));
        assert!(!b.clear(1));
        // Out of range is ignored.
        assert!(!b.set(7, MouseButton::Left, true));
        assert!(!b.any(7));
    }

    fn held(buttons: &[MouseButton]) -> u8 {
        buttons.iter().fold(0, |m, &b| m | Buttons::bit(b))
    }

    #[test]
    fn a_held_button_the_os_says_is_up_is_released() {
        use MouseButton::{Left, Right};
        // Held left, the OS holds nothing: release left.
        assert_eq!(os_released(held(&[Left]), Some(0)), [Left]);
        // Held left, the OS holds left: nothing.
        assert!(os_released(held(&[Left]), Some(1)).is_empty());
        // Held left and right, the OS holds right: left only.
        assert_eq!(os_released(held(&[Left, Right]), Some(2)), [Left]);
        // Nothing held: nothing.
        assert!(os_released(0, Some(0)).is_empty());
        // The OS cannot say: nothing.
        assert!(os_released(held(&[Left]), None).is_empty());
        assert!(os_released(held(&[Left, Right]), None).is_empty());
        // The other buttons go up only when the OS holds none from number 5 on.
        let other = held(&[MouseButton::Other(7)]);
        assert!(os_released(other, Some(1 << 7)).is_empty());
        assert_eq!(os_released(other, Some(1)), [MouseButton::Other(5)]);
        // Back and forward by their own bits.
        let bf = held(&[MouseButton::Back, MouseButton::Forward]);
        assert_eq!(os_released(bf, Some(1 << 4)), [MouseButton::Back]);
    }

    #[test]
    fn the_os_check_reads_one_panels_held_buttons() {
        let mut b = Buttons::default();
        b.push();
        b.push();
        assert!(!b.any_panel());
        b.set(1, MouseButton::Left, true);
        assert!(b.any_panel());
        assert!(
            b.os_released(0, Some(0)).is_empty(),
            "nothing held on panel 0"
        );
        assert_eq!(b.os_released(1, Some(0)), [MouseButton::Left]);
        assert!(b.os_released(9, Some(0)).is_empty(), "unknown panel");
        // Clearing what the OS reported leaves the panel free, and a release arriving later
        // is not a second "all up".
        assert!(b.set(1, MouseButton::Left, false));
        assert!(!b.any_panel());
        assert!(!b.set(1, MouseButton::Left, false));
    }
}
