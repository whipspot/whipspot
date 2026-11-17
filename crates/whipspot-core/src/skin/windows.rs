//! The panel-windows seam: what a skin front-end needs from the OS to show a skin's panels as
//! separate windows. The front-end decides where panels go (docking, snapping, layout) and drives
//! the windows only through [`PanelWindows`]; the host implements it. Everything is in the
//! toolkit's desktop units, so the front-end's layout logic is testable on a fake.

use std::fmt;

/// A point on the desktop. Units are the toolkit's desktop units: physical pixels on Windows,
/// points on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// A rectangle on the desktop, top-left corner and size, in the same desktop units as
/// [`ScreenPoint`]: physical pixels on Windows, points on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Why the panel windows could not be moved or resized. The text is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelWindowsError {
    /// A move or resize was refused: an index was out of range, a size was not positive or the
    /// OS rejected it.
    Failed,
}

impl fmt::Display for PanelWindowsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the panel windows could not be moved")
    }
}

impl std::error::Error for PanelWindowsError {}

/// The windows of one open skin, one per panel; a panel index is the index in `Skin::panels`.
/// Main thread only (no `Send` bound). The first panel is the group's leader: it owns the others,
/// is the only one with a taskbar button, and is never hidden by a front-end.
pub trait PanelWindows {
    /// The number of panel windows.
    fn count(&self) -> usize;

    /// The outer frame of a panel, also when it is hidden. `None` when `panel` is out of range or
    /// the OS cannot say.
    fn frame(&self, panel: usize) -> Option<ScreenRect>;

    /// Whether the panel is shown; `false` when `panel` is out of range.
    fn is_visible(&self, panel: usize) -> bool;

    /// The usable area of every monitor (the screen minus taskbars and docks).
    fn work_areas(&self) -> Vec<ScreenRect>;

    /// The pointer on the desktop, `None` when the OS cannot say.
    fn pointer(&self) -> Option<ScreenPoint>;

    /// Desktop units per physical pixel: 1.0 on Windows.
    fn units_per_pixel(&self) -> f32;

    /// Moves panels to new outer top-left corners in one batch. Any index out of range makes the
    /// call `Err` and nothing moves.
    fn move_panels(&mut self, moves: &[(usize, ScreenPoint)]) -> Result<(), PanelWindowsError>;

    /// Resizes one panel's window to the outer size `w` by `h` desktop units, keeping its
    /// top-left corner, also when it is hidden. `Err` and nothing changes when `panel` is out of
    /// range or `w` or `h` is not positive. The front-end passes sizes that are whole skin
    /// pixels at the set's scale, so the OS has nothing to round.
    fn resize_panel(&mut self, panel: usize, w: i32, h: i32) -> Result<(), PanelWindowsError>;

    /// Shows or hides one panel. Out-of-range indices do nothing.
    fn set_visible(&mut self, panel: usize, visible: bool);

    /// Records whether a panel is docked to the leader (the OS may then move them together).
    /// Out-of-range indices do nothing.
    fn set_docked(&mut self, panel: usize, docked: bool);

    /// Keeps every panel above other windows, or not.
    fn set_always_on_top(&mut self, on: bool);

    /// Minimises the whole set.
    fn minimise(&mut self);
}
