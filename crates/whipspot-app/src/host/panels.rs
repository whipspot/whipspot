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
}

impl PanelSet {
    /// A set of `leader` alone; the members come with [`PanelSet::add`].
    pub(super) fn new(generation: u64, k: u32, specs: Vec<PanelSpec>, leader: Arc<Window>) -> Self {
        PanelSet {
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

    /// The follower tick: each pointer-shaped panel lets clicks through outside its shape.
    pub(super) fn follow_tick(&mut self) {
        let pointer = desktop::pointer();
        for i in 0..self.windows.len() {
            if self.modes[i] == ShapeMode::Pointer {
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
        let Some(leader) = self.set.windows.first() else {
            return 1.0;
        };
        let scale = leader.scale_factor() as f32;
        let upp = desktop::units_per_logical(leader) / scale;
        if upp.is_finite() && upp > 0.0 {
            upp
        } else {
            1.0
        }
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
