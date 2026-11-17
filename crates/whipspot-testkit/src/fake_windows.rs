//! An in-memory [`PanelWindows`]: frames, visibility and flags kept in plain fields, so layout and
//! docking logic can be tested without a desktop.

use whipspot_core::skin::{PanelWindows, PanelWindowsError, ScreenPoint, ScreenRect};

/// Fake panel windows: every panel starts visible; `move_panels` and `resize_panel` update the frames exactly.
#[derive(Debug, Clone)]
pub struct FakeWindows {
    frames: Vec<ScreenRect>,
    visible: Vec<bool>,
    docked: Vec<bool>,
    work_areas: Vec<ScreenRect>,
    pointer: Option<ScreenPoint>,
    on_top: bool,
    minimised: bool,
}

impl FakeWindows {
    /// One window per frame, all visible and undocked; no pointer.
    pub fn new(frames: Vec<ScreenRect>, work_areas: Vec<ScreenRect>) -> Self {
        let n = frames.len();
        Self {
            frames,
            visible: vec![true; n],
            docked: vec![false; n],
            work_areas,
            pointer: None,
            on_top: false,
            minimised: false,
        }
    }

    pub fn set_pointer(&mut self, pointer: Option<ScreenPoint>) {
        self.pointer = pointer;
    }

    /// Whether a panel was last marked docked; `false` out of range.
    pub fn docked(&self, panel: usize) -> bool {
        self.docked.get(panel).copied().unwrap_or(false)
    }

    /// Whether always-on-top was last switched on.
    pub fn on_top(&self) -> bool {
        self.on_top
    }

    /// Whether `minimise` was called.
    pub fn minimised(&self) -> bool {
        self.minimised
    }
}

impl PanelWindows for FakeWindows {
    fn count(&self) -> usize {
        self.frames.len()
    }

    fn frame(&self, panel: usize) -> Option<ScreenRect> {
        self.frames.get(panel).copied()
    }

    fn is_visible(&self, panel: usize) -> bool {
        self.visible.get(panel).copied().unwrap_or(false)
    }

    fn work_areas(&self) -> Vec<ScreenRect> {
        self.work_areas.clone()
    }

    fn pointer(&self) -> Option<ScreenPoint> {
        self.pointer
    }

    fn units_per_pixel(&self) -> f32 {
        1.0
    }

    fn move_panels(&mut self, moves: &[(usize, ScreenPoint)]) -> Result<(), PanelWindowsError> {
        if moves.iter().any(|(i, _)| *i >= self.frames.len()) {
            return Err(PanelWindowsError::Failed);
        }
        for (i, p) in moves {
            self.frames[*i].x = p.x;
            self.frames[*i].y = p.y;
        }
        Ok(())
    }

    fn resize_panel(&mut self, panel: usize, w: i32, h: i32) -> Result<(), PanelWindowsError> {
        match self.frames.get_mut(panel) {
            Some(frame) if w > 0 && h > 0 => {
                frame.w = w;
                frame.h = h;
                Ok(())
            }
            _ => Err(PanelWindowsError::Failed),
        }
    }

    fn set_visible(&mut self, panel: usize, visible: bool) {
        if let Some(v) = self.visible.get_mut(panel) {
            *v = visible;
        }
    }

    fn set_docked(&mut self, panel: usize, docked: bool) {
        if let Some(d) = self.docked.get_mut(panel) {
            *d = docked;
        }
    }

    fn set_always_on_top(&mut self, on: bool) {
        self.on_top = on;
    }

    fn minimise(&mut self) {
        self.minimised = true;
    }
}
