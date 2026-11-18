//! The skinned front-end: the core's skin model drawn as one OS window per panel, driven only
//! through the player's actions and state (C1) and the core's panel-windows seam. Format-neutral:
//! whatever engine built the skin, this module sees the model only.
//!
//! The logic is split by what it needs: the binding mappings (`bind`), bitmap-font text layout
//! (`text`), panel layout and docking (`dock`), the skin loader (`library`), pointer input
//! (`input`), the spectrum cells (`vis`) and drawing (`draw`). [`SkinUi`] ties them together for
//! the host: it draws a panel per frame, turns pointer events into player actions and queued
//! window operations, and tells the host when the set of panel windows has to change.

use std::sync::Arc;
use std::time::Duration;

use lune_ui::desktop::WindowShape;
use lune_ui::image::{ImageData, ImageId as LuneImage, ImageStore};
use lune_ui::input::{Event, PointerButton};
use lune_ui::shortcuts::{CommandId, Shortcuts};
use lune_ui::ui::Ui;
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerNotice, PlayerState};

use crate::library::{Access, LibraryAnswer, ProviderChoice};
use whipspot_core::skin::{
    MAX_PANEL_SIDE, PanelWindows, ScreenPoint, ScreenRect, Shape, Size, Skin,
};

mod bind;
mod dock;
mod draw;
#[cfg(test)]
mod fake;
mod input;
mod library;
mod lists;
mod resize;
mod text;
mod vis;

/// The `--skin-trace` diagnostic (D3): one stderr line per panel drag event (press, move, release,
/// lost release, rescale) and resize event (resize press, move, release; the host adds button,
/// OS release and scale lines) with the pointer, the leader's
/// frame, the pixel scale, the desktop units per pixel and the moves sent. Numbers only. A user-run flag of the real app in a debug build; a
/// release build never turns it on, and without [`enable`] nothing is printed.
pub mod trace {
    use std::fmt;
    use std::sync::atomic::{AtomicBool, Ordering};

    use whipspot_core::skin::{ScreenPoint, ScreenRect};

    static ENABLED: AtomicBool = AtomicBool::new(false);

    /// Turns the trace on (debug builds only; a release build ignores it).
    pub fn enable() {
        if cfg!(debug_assertions) {
            ENABLED.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the trace prints.
    pub fn enabled() -> bool {
        cfg!(debug_assertions) && ENABLED.load(Ordering::Relaxed)
    }

    /// Prints `skin-trace <args>` to stderr when the trace is on.
    pub fn line(args: fmt::Arguments<'_>) {
        if enabled() {
            eprintln!("skin-trace {args}");
        }
    }

    /// `x,y`, or `-` for none.
    pub(crate) fn point(p: Option<ScreenPoint>) -> String {
        p.map_or_else(|| "-".to_owned(), |p| format!("{},{}", p.x, p.y))
    }

    /// `x,y wxh`, or `-` for none.
    pub(crate) fn rect(r: Option<ScreenRect>) -> String {
        r.map_or_else(
            || "-".to_owned(),
            |r| format!("{},{} {}x{}", r.x, r.y, r.w, r.h),
        )
    }

    /// `[panel:x,y ...]`.
    pub(crate) fn moves(moves: &[(usize, ScreenPoint)]) -> String {
        let items: Vec<String> = moves
            .iter()
            .map(|&(p, o)| format!("{p}:{},{}", o.x, o.y))
            .collect();
        format!("[{}]", items.join(" "))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_trace_formats_numbers_only() {
            assert_eq!(point(Some(ScreenPoint { x: -3, y: 4 })), "-3,4");
            assert_eq!(point(None), "-");
            assert_eq!(
                rect(Some(ScreenRect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4
                })),
                "1,2 3x4"
            );
            assert_eq!(
                moves(&[
                    (1, ScreenPoint { x: 5, y: 6 }),
                    (2, ScreenPoint { x: 7, y: 8 })
                ]),
                "[1:5,6 2:7,8]"
            );
        }
    }
}

use dock::DragController;
use draw::{Ctx, Scrolls};
use input::{Effect, Metrics, PanelInput};
use lists::Lists;
use resize::ResizeController;
use vis::VisState;

pub use library::{
    LOADER_STOPPED, LoadedSkin, NO_SKINS_FOLDER, OPEN_FAILED, SkinLibrary, SkinStatus,
};

/// The most physical pixels per skin pixel.
const MAX_PIXEL_SCALE: u32 = 8;

/// Physical pixels per skin pixel for an OS scale factor (D4): the nearest whole number, at least
/// 1 and at most 8, so nearest-sampled sprites stay crisp and panel edges land on whole pixels.
/// A non-finite factor gives 1.
pub fn pixel_scale(scale_factor: f64) -> u32 {
    if !scale_factor.is_finite() {
        return 1;
    }
    scale_factor.round().clamp(1.0, f64::from(MAX_PIXEL_SCALE)) as u32
}

/// Where skin images start in the toolkit's image ids; the native view's cover cache counts from
/// 1 (D8).
pub const SKIN_IMAGE_BASE: u64 = 1 << 40;

/// The toolkit id of skin image `index` of the skin loaded as `generation`: generations never
/// share an id, so the old set can be released after the new one is registered.
fn image_id(generation: u64, index: usize) -> LuneImage {
    LuneImage(SKIN_IMAGE_BASE + (generation << 8) + index as u64)
}

/// The frame interval while something on a panel moves (about 30 Hz).
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// How long an error replaces the scrolling title, and a hint.
const ERROR_SHOWN: Duration = Duration::from_secs(4);
const HINT_SHOWN: Duration = Duration::from_secs(6);

/// One panel's place as the user left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPanel {
    pub id: String,
    /// The outer top-left corner in desktop units.
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// The panel's size in skin pixels; `Some` only for a panel the skin lets the user resize.
    pub size: Option<(u32, u32)>,
}

/// The panels of the skin `skin` as the user left them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedLayout {
    /// The skin's file name.
    pub skin: String,
    pub panels: Vec<SavedPanel>,
}

/// What the front-end starts with.
#[derive(Debug, Clone, Default)]
pub struct SkinPrefs {
    /// The panels stay above other windows.
    pub on_top: bool,
    pub saved: Option<SavedLayout>,
}

/// One panel window the host opens.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelSpec {
    pub title: String,
    /// The base size in skin pixels; the window's inner size is this times the pixel scale.
    pub size: (u32, u32),
    /// The outline in skin pixels; `None` for the whole rectangle.
    pub shape: Option<WindowShape>,
    /// Shown when the set opens.
    pub visible: bool,
}

/// The panel windows of one skin, the first being the group's leader.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelPlan {
    /// Grows with every skin loaded.
    pub generation: u64,
    pub panels: Vec<PanelSpec>,
}

/// What the host has to do about the windows.
#[derive(Debug, Clone, PartialEq)]
pub enum FrontChange {
    /// Open (or replace the open set with) these panel windows.
    Panels(PanelPlan),
    /// Close the panel windows; the native look is back.
    Native,
}

/// What the host saves about the skin front-end.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkinReport {
    /// The running skin's file name, `None` for the native look.
    pub skin: Option<String>,
    pub on_top: bool,
    pub panels: Vec<SavedPanel>,
}

/// One panel of the demo's composite dump (see [`SkinUi::compose_layout`]).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposedPanel {
    pub panel: usize,
    /// Physical pixels from the composite's left and top.
    pub x: i32,
    pub y: i32,
    /// The panel's physical size.
    pub w: u32,
    pub h: u32,
}

/// States the demo's dumps pin (nothing else uses it).
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct SkinForce {
    /// Panels shown or hidden, by index.
    pub visible: Vec<(usize, bool)>,
}

/// A window operation the frame asked for; the host's windows are not at hand in a frame, so
/// [`SkinUi::drive`] runs them.
enum Op {
    DragPress(usize),
    DragRelease,
    ResizePress(usize),
    ResizeRelease,
    Visible(usize, bool),
    DockAll,
    OnTop(bool),
    Minimise,
    /// The desktop units per skin pixel before the change, and the new pixel scale.
    Rescale(f32, u32),
}

/// A text shown in place of the scrolling title for a while.
struct TitleOverride {
    text: String,
    len: Duration,
    /// Set at the first frame that shows it.
    until: Option<Duration>,
}

impl TitleOverride {
    fn new(text: impl Into<String>, len: Duration) -> Self {
        TitleOverride {
            text: text.into(),
            len,
            until: None,
        }
    }
}

/// The text to show now and when it ends; clears an expired override.
fn current_title(slot: &mut Option<TitleOverride>, now: Duration) -> Option<(String, Duration)> {
    let t = slot.as_mut()?;
    let until = *t.until.get_or_insert(now + t.len);
    if now >= until {
        *slot = None;
        return None;
    }
    Some((t.text.clone(), until))
}

/// What one panel keeps between frames.
struct PanelState {
    input: PanelInput,
    vis: VisState,
    scrolls: Scrolls,
    has_vis: bool,
}

/// A loaded skin in use.
struct Active {
    generation: u64,
    name: String,
    skin: Arc<Skin>,
    plan: PanelPlan,
    visible: Vec<bool>,
    /// Each panel's size now in skin pixels: the plan's until a resize.
    size: Vec<(u32, u32)>,
    /// Focus pinned by `focused`, else the frame's own.
    focus: Vec<Option<bool>>,
    /// The toolkit image per skin image; `None` when it could not be registered.
    ids: Vec<Option<LuneImage>>,
    registered: bool,
    panels: Vec<PanelState>,
    drag: DragController,
    resize: ResizeController,
    /// The panel whose drag area or resize grip is held, and which of the two.
    holder: Option<(usize, Hold)>,
    /// The docked flags want recomputing at the next `drive`.
    need_dock: bool,
    ops: Vec<Op>,
    quit: bool,
}

/// What a held press moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    Drag,
    Resize,
}

/// The shape of a panel for the window: its polygons in skin pixels; `None` for a rectangle or
/// when the toolkit refuses a polygon.
fn plan_shape(shape: &Shape) -> Option<WindowShape> {
    let Shape::Polygons(polygons) = shape else {
        return None;
    };
    let mut out = WindowShape::new();
    for polygon in polygons {
        let points: Vec<(f32, f32)> = polygon.iter().map(|p| (p.x as f32, p.y as f32)).collect();
        out = out.with_polygon(&points).ok()?;
    }
    (!out.is_empty()).then_some(out)
}

impl Active {
    fn new(loaded: LoadedSkin, saved: Option<&SavedLayout>, k: u32, on_top: bool) -> Active {
        let skin = loaded.skin;
        let saved = saved.filter(|s| s.skin == loaded.name);
        let visible: Vec<bool> = skin
            .panels
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // The first panel is never hidden (it owns the others).
                i == 0
                    || saved
                        .and_then(|s| s.panels.iter().find(|sp| sp.id == p.id.as_str()))
                        .map_or(p.visible_at_start, |sp| sp.visible)
            })
            .collect();
        // A resizable panel opens at its saved size, snapped to its lattice; a fixed one never
        // takes a saved size.
        let size: Vec<(u32, u32)> = skin
            .panels
            .iter()
            .map(|p| {
                let saved_size = saved
                    .and_then(|s| s.panels.iter().find(|sp| sp.id == p.id.as_str()))
                    .and_then(|sp| sp.size);
                match (&p.resize, saved_size) {
                    (Some(resize), Some((w, h))) => {
                        let side = Size {
                            w: MAX_PANEL_SIDE,
                            h: MAX_PANEL_SIDE,
                        };
                        let s = resize.snap(p.size, Size { w, h }, side);
                        (s.w, s.h)
                    }
                    _ => (p.size.w, p.size.h),
                }
            })
            .collect();
        let plan = PanelPlan {
            generation: loaded.generation,
            panels: skin
                .panels
                .iter()
                .zip(&visible)
                .zip(&size)
                .map(|((p, &visible), &size)| PanelSpec {
                    title: p.title.clone(),
                    size,
                    shape: plan_shape(&p.shape),
                    visible,
                })
                .collect(),
        };
        let panels = skin
            .panels
            .iter()
            .map(|p| PanelState {
                input: PanelInput::default(),
                vis: VisState::default(),
                scrolls: Scrolls::default(),
                has_vis: p
                    .widgets
                    .iter()
                    .any(|w| matches!(w.kind, whipspot_core::skin::WidgetKind::Visualiser)),
            })
            .collect();
        Active {
            generation: loaded.generation,
            name: loaded.name,
            visible,
            size,
            focus: vec![None; skin.panels.len()],
            ids: vec![None; skin.images.len()],
            registered: false,
            panels,
            drag: DragController::new(k),
            resize: ResizeController::default(),
            holder: None,
            need_dock: true,
            ops: vec![Op::OnTop(on_top)],
            quit: false,
            plan,
            skin,
        }
    }

    fn set_visible(&mut self, panel: usize, visible: bool) {
        if self.visible.get(panel).is_some_and(|&v| v != visible) {
            self.visible[panel] = visible;
            self.ops.push(Op::Visible(panel, visible));
            self.ops.push(Op::DockAll);
        }
    }

    /// Turns one frame's raw events of panel `panel` into effects. A drag or resize whose release
    /// never came is ended as a release would end it: luneui's pointer events carry no button
    /// state, but the OS sends a held button's events to the window that took the press, so a
    /// pointer event at another panel, or a new press anywhere, means the button was let go
    /// where no panel saw it (a click let through by a pointer-shaped window, macOS).
    fn events(
        &mut self,
        panel: usize,
        events: &[Event],
        m: Metrics,
        state: &PlayerState,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        if panel >= self.panels.len() {
            return effects;
        }
        for event in events {
            if let Some((holder, hold)) = self.holder {
                let lost = match event {
                    Event::PointerMoved { .. } => holder != panel,
                    Event::PointerButton {
                        button: PointerButton::Primary,
                        pressed,
                        ..
                    } => holder != panel || *pressed,
                    _ => false,
                };
                if lost {
                    if trace::enabled() {
                        trace::line(format_args!("lost-release holder={holder} seen-at={panel}"));
                    }
                    effects.push(self.drop_hold(holder, hold));
                }
            }
            let before = effects.len();
            self.panels[panel]
                .input
                .event(event, m, &self.skin, panel, state, &mut effects);
            // A press or release of this event sets the holder for the events after it.
            for e in &effects[before..] {
                match e {
                    Effect::DragPress => self.holder = Some((panel, Hold::Drag)),
                    Effect::ResizePress => self.holder = Some((panel, Hold::Resize)),
                    Effect::DragRelease | Effect::ResizeRelease => self.holder = None,
                    _ => {}
                }
            }
        }
        effects
    }

    /// Ends the hold of `holder` without its release event: its press is dropped and the release
    /// effect (for `apply`) returned.
    fn drop_hold(&mut self, holder: usize, hold: Hold) -> Effect {
        if let Some(h) = self.panels.get_mut(holder) {
            h.input.cancel();
        }
        self.holder = None;
        match hold {
            Hold::Drag => Effect::DragRelease,
            Hold::Resize => Effect::ResizeRelease,
        }
    }

    /// Applies what a panel's pointer events asked for; returns the player actions to dispatch.
    fn apply(&mut self, panel: usize, effects: Vec<Effect>) -> Vec<PlayerAction> {
        let mut actions = Vec::new();
        for effect in effects {
            match effect {
                Effect::Action(a) => actions.push(a),
                // The first panel cannot be hidden by a toggle (P4).
                Effect::TogglePanel(i) if i != 0 => {
                    let shown = self.visible.get(i).copied().unwrap_or(true);
                    self.set_visible(i, !shown);
                }
                Effect::TogglePanel(_) => {}
                // Exactly one of the two ends up shown; the first panel is never part of a swap.
                Effect::SwapPanels(a, b) if a != 0 && b != 0 && a != b => {
                    let a_shown = self.visible.get(a).copied().unwrap_or(false);
                    self.set_visible(a, !a_shown);
                    self.set_visible(b, a_shown);
                }
                Effect::SwapPanels(..) => {}
                Effect::ClosePanel if panel == 0 => self.quit = true,
                Effect::ClosePanel => self.set_visible(panel, false),
                Effect::Minimize => self.ops.push(Op::Minimise),
                Effect::DragPress => self.ops.push(Op::DragPress(panel)),
                Effect::DragRelease => self.ops.push(Op::DragRelease),
                Effect::ResizePress => self.ops.push(Op::ResizePress(panel)),
                Effect::ResizeRelease => self.ops.push(Op::ResizeRelease),
                // The list owns the scroll position: `frame_panel` sends these to it.
                Effect::Scroll(_) => {}
            }
        }
        actions
    }
}

/// The skin front-end: one of these draws every panel of the running skin.
pub struct SkinUi {
    library: SkinLibrary,
    on_top: bool,
    saved: Option<SavedLayout>,
    /// Physical pixels per skin pixel for the whole set (D4), from the leader's scale.
    k: u32,
    active: Option<Active>,
    /// Toolkit images of a skin that is gone, released at the next `images`.
    release: Vec<LuneImage>,
    change: Option<FrontChange>,
    title: Option<TitleOverride>,
    toggle_diagnostics: bool,
    /// The track list and library widgets and the library model they read (D10).
    lists: Lists,
    /// What the host kept for saving when the loop ended (`keep_report`).
    kept: Option<SkinReport>,
    /// The library provider the user chose, shared with the native view (D11).
    choice: ProviderChoice,
}

impl SkinUi {
    pub fn new(library: SkinLibrary, prefs: SkinPrefs) -> SkinUi {
        SkinUi {
            library,
            on_top: prefs.on_top,
            saved: prefs.saved,
            k: 1,
            active: None,
            release: Vec::new(),
            change: None,
            title: None,
            toggle_diagnostics: false,
            lists: Lists::default(),
            kept: None,
            choice: ProviderChoice::default(),
        }
    }

    /// Shares the library provider choice with the native view: the skinned library shows the
    /// provider the switch there chose.
    pub fn with_provider_choice(mut self, choice: ProviderChoice) -> Self {
        self.choice = choice;
        self
    }

    /// Whether a skin chosen before the windows opened is still on its way (loading, or loaded
    /// and not yet taken): the host keeps the native window hidden at start while it is.
    pub fn starting(&self) -> bool {
        self.active.is_none()
            && matches!(
                self.library.status(),
                SkinStatus::Loading { .. } | SkinStatus::Active { .. }
            )
    }

    /// The host could not open the windows of the plan it was given: the skin is dropped (its
    /// images are released at the next `images`) and the status reads as a fixed failure.
    pub fn open_failed(&mut self) {
        let old = self.active.take();
        self.drop_images(old);
        self.change = None;
        self.library.fail_active(OPEN_FAILED);
    }

    /// Keeps what to save from the open windows (the host calls it as the loop ends, while the
    /// windows still exist); [`SkinUi::kept_report`] reads it after the loop.
    pub fn keep_report(&mut self, windows: &dyn PanelWindows) {
        self.kept = Some(self.report(windows));
    }

    /// What `keep_report` kept; without it, the running skin with no panel places.
    pub fn kept_report(&self) -> SkinReport {
        self.kept.clone().unwrap_or_else(|| SkinReport {
            skin: self.active.as_ref().map(|a| a.name.clone()),
            on_top: self.on_top,
            panels: Vec::new(),
        })
    }

    /// Sets the pixel scale every panel of the set is drawn with (D4); call it before the set
    /// opens (`rescaled` once windows exist).
    pub fn set_pixel_scale(&mut self, k: u32) {
        self.k = k.clamp(1, MAX_PIXEL_SCALE);
        if let Some(a) = &mut self.active {
            a.drag = DragController::new(self.k);
            a.need_dock = true;
        }
    }

    /// The leader's scale changed: stores `k` and rescales the docked panels' places at the next
    /// `drive` (the host resizes and reshapes the windows). `units_per_pixel_before` is the
    /// desktop units per physical pixel before the change (the host's new one is what the
    /// windows say by then): the places scale by the change in desktop units per skin pixel.
    pub fn rescaled(&mut self, k: u32, units_per_pixel_before: f32) {
        let k = k.clamp(1, MAX_PIXEL_SCALE);
        let old = std::mem::replace(&mut self.k, k);
        if let Some(a) = &mut self.active
            && old != k
        {
            a.ops
                .push(Op::Rescale(units_per_pixel_before * old as f32, k));
        }
    }

    pub fn pixel_scale(&self) -> u32 {
        self.k
    }

    /// The host saw every mouse button of panel `panel` go up (their release, or the window losing
    /// focus): a drag or resize the panel holds ends as its release would (docks recomputed at the
    /// next `drive`), even when the toolkit's release event never comes.
    pub fn buttons_up(&mut self, panel: usize) {
        let Some(a) = &mut self.active else {
            return;
        };
        if let Some((holder, hold)) = a.holder
            && holder == panel
        {
            if trace::enabled() {
                trace::line(format_args!("buttons-up holder={holder}"));
            }
            let release = a.drop_hold(holder, hold);
            let _ = a.apply(holder, vec![release]);
        }
    }

    /// Whether a change can still come that the host has not taken: a load or a listing still
    /// runs, a finished load waits for [`SkinUi::take_change`], or the native look was chosen
    /// while a skin runs. The host polls with a timer while it does (D5, P1).
    pub fn pending(&self) -> bool {
        self.library.pending()
            || self.library.has_loaded()
            || (self.active.is_some() && self.library.status() == SkinStatus::Native)
    }

    /// The library this front-end loads skins from.
    pub fn library(&self) -> &SkinLibrary {
        &self.library
    }

    /// Takes the loader's answer, and what the host must do about the windows. At most one
    /// change per call; a newer skin replaces an older one.
    pub fn take_change(&mut self) -> Option<FrontChange> {
        if let Some(loaded) = self.library.take_loaded() {
            let old = self.active.take();
            self.drop_images(old);
            let a = Active::new(loaded, self.saved.as_ref(), self.k, self.on_top);
            self.lists.reset_scrolls();
            self.change = Some(FrontChange::Panels(a.plan.clone()));
            self.active = Some(a);
        } else if self.active.is_some() && self.library.status() == SkinStatus::Native {
            let old = self.active.take();
            self.drop_images(old);
            self.lists.reset_scrolls();
            self.change = Some(FrontChange::Native);
        }
        self.change.take()
    }

    /// Queues the toolkit images of a skin that is gone for release.
    fn drop_images(&mut self, old: Option<Active>) {
        if let Some(a) = old {
            self.release.extend(a.ids.into_iter().flatten());
        }
    }

    /// The running skin's panels, `None` for the native look.
    pub fn plan(&self) -> Option<&PanelPlan> {
        self.active.as_ref().map(|a| &a.plan)
    }

    /// Where the panels start: the saved places of this skin that are still on a screen, the
    /// rest from the skin's attach rules with the first panel at `leader` (desktop units).
    /// `units_per_pixel` is the desktop units per physical pixel.
    pub fn start_layout(
        &self,
        leader: ScreenPoint,
        units_per_pixel: f32,
        work_areas: &[ScreenRect],
    ) -> Vec<ScreenPoint> {
        let Some(a) = &self.active else {
            return Vec::new();
        };
        let unit = units_per_pixel * self.k as f32;
        let frames: Vec<ScreenRect> = a
            .size
            .iter()
            .map(|&(w, h)| ScreenRect {
                x: 0,
                y: 0,
                w: (w as f32 * unit).round() as i32,
                h: (h as f32 * unit).round() as i32,
            })
            .collect();
        let mut origins = dock::attach_layout(&a.skin, &frames, leader);
        let saved = self.saved.as_ref().filter(|s| s.skin == a.name);
        for (i, panel) in a.skin.panels.iter().enumerate() {
            let Some(sp) =
                saved.and_then(|s| s.panels.iter().find(|sp| sp.id == panel.id.as_str()))
            else {
                continue;
            };
            let f = frames[i];
            let meets = work_areas.iter().any(|w| {
                sp.x < w.x.saturating_add(w.w)
                    && sp.x.saturating_add(f.w) > w.x
                    && sp.y < w.y.saturating_add(w.h)
                    && sp.y.saturating_add(f.h) > w.y
            });
            if meets {
                origins[i] = ScreenPoint { x: sp.x, y: sp.y };
            }
        }
        origins
    }

    /// What to save: the skin, the always-on-top flag and where each panel is and whether shown.
    pub fn report(&self, windows: &dyn PanelWindows) -> SkinReport {
        let Some(a) = &self.active else {
            return SkinReport {
                skin: None,
                on_top: self.on_top,
                panels: Vec::new(),
            };
        };
        let panels = a
            .skin
            .panels
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let f = windows.frame(i)?;
                Some(SavedPanel {
                    id: p.id.as_str().to_owned(),
                    x: f.x,
                    y: f.y,
                    visible: a.visible.get(i).copied().unwrap_or(true),
                    size: p.resize.is_some().then(|| a.size.get(i).copied()).flatten(),
                })
            })
            .collect();
        SkinReport {
            skin: Some(a.name.clone()),
            on_top: self.on_top,
            panels,
        }
    }

    /// Shows `text` in place of the scrolling title for a few seconds (the hint of the native
    /// look's shortcut, D9).
    pub fn hint(&mut self, text: &str) {
        self.title = Some(TitleOverride::new(text, HINT_SHOWN));
    }

    /// Pins panel focus for the dumps (the frame's own focus is used until then).
    #[doc(hidden)]
    pub fn focused(&mut self, panel: usize, focused: bool) {
        if let Some(f) = self.active.as_mut().and_then(|a| a.focus.get_mut(panel)) {
            *f = Some(focused);
        }
    }

    /// Pins what the dumps need.
    #[doc(hidden)]
    pub fn force(&mut self, force: SkinForce) {
        if let Some(a) = &mut self.active {
            for (panel, visible) in force.visible {
                if let Some(v) = a.visible.get_mut(panel) {
                    *v = visible;
                }
            }
        }
    }

    /// The window of `panel` was asked to close (the close button of the OS, Alt+F4): the first
    /// panel quits the app (returns `true`); another is hidden.
    pub fn close_requested(&mut self, panel: usize) -> bool {
        if panel == 0 {
            return true;
        }
        if let Some(a) = &mut self.active {
            a.set_visible(panel, false);
        }
        false
    }

    /// Whether a close control of the first panel asked to quit (once).
    pub fn take_quit(&mut self) -> bool {
        self.active
            .as_mut()
            .is_some_and(|a| std::mem::take(&mut a.quit))
    }

    /// Registers the skin's images with the host's store once per skin, and releases those of a
    /// skin that is gone. Called before each frame of a panel window, and once after a
    /// `FrontChange::Native` so the old ones are freed.
    pub fn images(&mut self, store: &mut dyn ImageStore) {
        for id in self.release.drain(..) {
            store.release(id);
        }
        let Some(a) = &mut self.active else {
            return;
        };
        if a.registered {
            return;
        }
        a.registered = true;
        for (i, image) in a.skin.images.iter().enumerate() {
            let id = image_id(a.generation, i);
            let data = ImageData {
                width: image.width(),
                height: image.height(),
                rgba: image.rgba().to_vec(),
            };
            // An image the store refuses leaves its widgets undrawn.
            if store.register(id, data).is_ok() {
                a.ids[i] = Some(id);
            }
        }
    }

    /// Runs the window operations the frames queued, then follows an open drag. The host calls it
    /// after each frame of a panel window, with the windows of the current plan.
    pub fn drive(&mut self, windows: &mut dyn PanelWindows) {
        let k = self.k;
        let Some(a) = &mut self.active else {
            return;
        };
        if windows.count() != a.skin.panels.len() {
            return;
        }
        if std::mem::take(&mut a.need_dock) {
            a.drag.dock_all(windows);
        }
        for op in std::mem::take(&mut a.ops) {
            match op {
                Op::DragPress(p) => {
                    if let Some(pointer) = windows.pointer() {
                        a.drag.press(p, pointer, &*windows, &a.skin);
                    }
                }
                Op::DragRelease => a.drag.release(windows),
                Op::ResizePress(p) => {
                    if let (Some(pointer), Some(&size)) = (windows.pointer(), a.size.get(p)) {
                        a.resize.press(p, pointer, size, k, (&*windows, &a.skin));
                    }
                }
                Op::ResizeRelease => {
                    a.resize.release();
                    a.drag.dock_all(windows);
                }
                Op::Visible(p, v) => windows.set_visible(p, v),
                Op::DockAll => a.drag.dock_all(windows),
                Op::OnTop(on) => windows.set_always_on_top(on),
                Op::Minimise => windows.minimise(),
                Op::Rescale(old_unit, new) => a.drag.rescale(old_unit, new, windows),
            }
        }
        if a.drag.dragging() {
            a.drag.moved(windows);
        }
        if a.resize.resizing() {
            a.resize.moved(windows, &a.skin, &mut a.size);
        }
    }

    /// The keyboard table of the panel windows (read once when a window opens).
    pub fn shortcuts(&self) -> Shortcuts {
        input::table()
    }

    /// Acts on the shortcut commands a frame reported.
    pub fn fired(&mut self, fired: &[CommandId], text_focused: bool, player: &PlayerHandle) {
        if text_focused {
            return;
        }
        for id in fired {
            if *id == input::ON_TOP {
                self.on_top = !self.on_top;
                if let Some(a) = &mut self.active {
                    a.ops.push(Op::OnTop(self.on_top));
                }
            } else if *id == input::NATIVE_LOOK {
                self.library.choose_native();
            } else if let Some(command) = crate::shortcuts::Command::from_id(id) {
                match command.action(&player.state()) {
                    Some(action) => player.dispatch(action),
                    None => {
                        if command == crate::shortcuts::Command::ToggleDiagnostics {
                            self.toggle_diagnostics = true;
                        }
                    }
                }
            }
        }
    }

    /// Whether the user asked to open or close the diagnostics window since the last call.
    pub fn take_toggle_diagnostics(&mut self) -> bool {
        std::mem::take(&mut self.toggle_diagnostics)
    }

    /// Draws one frame of panel `panel`: drains the notices (only while a skin runs, so exactly
    /// one front-end drains them), turns this frame's pointer events into actions, and draws
    /// every widget.
    pub fn frame_panel(&mut self, panel: usize, ui: &mut Ui<'_>, player: &PlayerHandle) {
        if self.active.is_none() {
            return;
        }
        let now = ui.time();
        let state = player.state();
        let selected = self.choice.get();
        self.lists
            .model
            .sync(Access::of(&state.providers, selected.as_deref()).provider());
        for notice in player.drain_notices() {
            self.route(notice);
        }
        self.lists.model.pump(player);
        let title = current_title(&mut self.title, now);
        let Some(a) = &mut self.active else {
            return;
        };
        if panel >= a.panels.len() {
            return;
        }
        let raw = ui.input();
        let scale = if raw.scale.is_finite() && raw.scale > 0.0 {
            raw.scale
        } else {
            1.0
        };
        let m = Metrics {
            k: self.k as f32,
            scale,
        };
        let size = a.size[panel];
        a.panels[panel].input.set_size(size);
        let effects = a.events(panel, &raw.events, m, &state);
        // The scroll bar acts on the list, which keeps its own position.
        let mut rest = Vec::with_capacity(effects.len());
        for effect in effects {
            match effect {
                Effect::Scroll(op) => self.lists.scroll_op(panel, op),
                other => rest.push(other),
            }
        }
        for action in a.apply(panel, rest) {
            player.dispatch(action);
        }
        let diag = player.diagnostics();
        let focused = a.focus[panel].unwrap_or(raw.focused);
        let PanelState {
            input,
            vis,
            scrolls,
            has_vis,
        } = &mut a.panels[panel];
        if *has_vis {
            vis.update(player, now);
        }
        let ctx = Ctx {
            skin: &a.skin,
            panel,
            ids: &a.ids,
            s: m.logical_per_skin(),
            size,
            state: &state,
            source: &diag.source,
            visible: &a.visible,
            focused,
            input,
            title: title.as_ref().map(|t| t.0.as_str()),
            now,
            vis,
            theme: *ui.theme(),
            selected: selected.as_deref(),
        };
        let drawn = draw::draw_panel(ui, &ctx, scrolls, &mut self.lists);
        if drawn.animating {
            ui.request_repaint_at(now + FRAME_INTERVAL);
        }
        if let Some((_, until)) = title {
            ui.request_repaint_at(until);
        }
        let out = std::mem::take(&mut self.lists.out);
        for action in out.actions {
            player.dispatch(action);
        }
        for kind in out.more {
            self.lists.model.request(kind, player);
        }
        for kind in out.retry {
            self.lists.model.retry(kind, player);
        }
    }

    /// Where every visible panel sits in the composite [`SkinUi::compose`] draws, in physical
    /// pixels from the composite's top-left corner, with the panel's physical size, and the
    /// composite's size: the bounding box of the visible panels. The panels start at their
    /// attach-layout places (the first at the origin). Empty for the native look.
    #[doc(hidden)]
    pub fn compose_layout(&self) -> (Vec<ComposedPanel>, (u32, u32)) {
        let Some(a) = &self.active else {
            return (Vec::new(), (0, 0));
        };
        let k = self.k as i32;
        let frames: Vec<ScreenRect> = a
            .size
            .iter()
            .map(|&(w, h)| ScreenRect {
                x: 0,
                y: 0,
                w: w as i32 * k,
                h: h as i32 * k,
            })
            .collect();
        let origins = dock::attach_layout(&a.skin, &frames, ScreenPoint { x: 0, y: 0 });
        let shown: Vec<ComposedPanel> = origins
            .iter()
            .zip(&frames)
            .enumerate()
            .filter(|(i, _)| a.visible.get(*i).copied().unwrap_or(false))
            .map(|(panel, (at, f))| ComposedPanel {
                panel,
                x: at.x,
                y: at.y,
                w: f.w as u32,
                h: f.h as u32,
            })
            .collect();
        let min_x = shown.iter().map(|p| p.x).min().unwrap_or(0);
        let min_y = shown.iter().map(|p| p.y).min().unwrap_or(0);
        let shown: Vec<ComposedPanel> = shown
            .into_iter()
            .map(|p| ComposedPanel {
                x: p.x - min_x,
                y: p.y - min_y,
                ..p
            })
            .collect();
        let w = shown.iter().map(|p| p.x as u32 + p.w).max().unwrap_or(0);
        let h = shown.iter().map(|p| p.y as u32 + p.h).max().unwrap_or(0);
        (shown, (w, h))
    }

    /// Draws every visible panel at its attach-layout place in one surface (the layout of
    /// [`SkinUi::compose_layout`]); the surface's logical size times its scale must be that
    /// layout's size. For the demo's desktop dump; the host never calls it.
    #[doc(hidden)]
    pub fn compose(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        let scale = ui.input().scale;
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        for p in self.compose_layout().0 {
            ui.painter()
                .push_translate(p.x as f32 / scale, p.y as f32 / scale);
            self.frame_panel(p.panel, ui, player);
            ui.painter().pop_translate();
        }
    }

    /// Takes one notice while a skin runs (D10): a library page goes to the lists' own model, a
    /// library or likes change refreshes them, an error replaces the scrolling title for a few
    /// seconds; login results, prefills and likes are dropped (those flows live in the native
    /// settings).
    fn route(&mut self, notice: PlayerNotice) {
        match notice {
            PlayerNotice::LibraryPage { request, result } => {
                self.lists
                    .model
                    .push_page(LibraryAnswer { request, result });
            }
            PlayerNotice::LikesChanged { provider } => self.lists.model.changed(&provider, true),
            PlayerNotice::LibraryChanged { provider } => {
                self.lists.model.changed(&provider, false);
            }
            PlayerNotice::Error(text) => {
                self.title = Some(TitleOverride::new(text, ERROR_SHOWN));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_scale_rounds_to_a_whole_number() {
        assert_eq!(pixel_scale(1.0), 1);
        assert_eq!(pixel_scale(1.25), 1);
        assert_eq!(pixel_scale(1.5), 2);
        assert_eq!(pixel_scale(2.0), 2);
        assert_eq!(pixel_scale(3.0), 3);
        assert_eq!(pixel_scale(0.5), 1);
        assert_eq!(pixel_scale(0.0), 1);
        assert_eq!(pixel_scale(-2.0), 1);
        assert_eq!(pixel_scale(20.0), 8);
        assert_eq!(pixel_scale(f64::NAN), 1);
        assert_eq!(pixel_scale(f64::INFINITY), 1);
    }

    use std::collections::BTreeMap;

    use lune_ui::image::ImageError;
    use whipspot_core::skin::{Edge, Resize, SkinImage, SkinInfo, SkinRegistry, SkinTheme};

    use super::fake::{FakeWindows, panel};

    /// A store that remembers which ids are live.
    #[derive(Default)]
    struct Store {
        live: BTreeMap<u64, (u32, u32)>,
    }

    impl ImageStore for Store {
        fn register(&mut self, id: LuneImage, data: ImageData) -> Result<(), ImageError> {
            self.live.insert(id.0, (data.width, data.height));
            Ok(())
        }
        fn update(
            &mut self,
            _id: LuneImage,
            _at: (u32, u32),
            _data: &ImageData,
        ) -> Result<(), ImageError> {
            Ok(())
        }
        fn release(&mut self, id: LuneImage) {
            self.live.remove(&id.0);
        }
    }

    fn skin(images: usize) -> Skin {
        let mut library = panel("library", 100, 50, Some(("main", Edge::Right)));
        library.visible_at_start = false;
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![
                panel("main", 100, 50, None),
                panel("equalizer", 100, 50, Some(("main", Edge::Bottom))),
                library,
            ],
            images: (0..images)
                .map(|i| SkinImage::new(2 + i as u32, 2, vec![0; (2 + i) * 2 * 4]).unwrap())
                .collect(),
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    fn loaded(generation: u64, images: usize) -> LoadedSkin {
        LoadedSkin {
            generation,
            name: "x.skin".to_owned(),
            skin: Arc::new(skin(images)),
        }
    }

    fn ui() -> SkinUi {
        SkinUi::new(
            SkinLibrary::new(None, Arc::new(SkinRegistry::new())),
            SkinPrefs::default(),
        )
    }

    fn show(ui: &mut SkinUi, loaded: LoadedSkin) {
        let old = ui.active.take();
        ui.drop_images(old);
        ui.active = Some(Active::new(loaded, None, ui.k, ui.on_top));
    }

    #[test]
    fn image_ids_of_two_generations_do_not_overlap_and_the_old_ones_are_released() {
        let mut ui = ui();
        let mut store = Store::default();
        show(&mut ui, loaded(1, 3));
        ui.images(&mut store);
        let first: Vec<u64> = store.live.keys().copied().collect();
        assert_eq!(first.len(), 3);
        // Registered once, never per frame.
        ui.images(&mut store);
        assert_eq!(store.live.len(), 3);
        show(&mut ui, loaded(2, 2));
        ui.images(&mut store);
        let second: Vec<u64> = store.live.keys().copied().collect();
        assert_eq!(second.len(), 2, "the old generation is gone: {second:?}");
        assert!(first.iter().all(|id| !second.contains(id)));
        assert!(second.iter().all(|&id| id >= SKIN_IMAGE_BASE));
        // The native look releases the last set.
        let old = ui.active.take();
        ui.drop_images(old);
        ui.images(&mut store);
        assert!(store.live.is_empty());
    }

    #[test]
    fn a_swap_shows_exactly_one_of_its_two_panels() {
        let mut ui = ui();
        show(&mut ui, loaded(1, 1));
        let a = ui.active.as_mut().unwrap();
        assert_eq!(a.visible, [true, true, false]);
        a.apply(0, vec![Effect::SwapPanels(2, 1)]);
        assert_eq!(a.visible, [true, false, true]);
        a.apply(2, vec![Effect::SwapPanels(2, 1)]);
        assert_eq!(a.visible, [true, true, false]);
        // The same swap named the other way round does the same thing.
        a.apply(1, vec![Effect::SwapPanels(1, 2)]);
        assert_eq!(a.visible, [true, false, true]);
        // Both hidden: the first named is shown.
        a.apply(2, vec![Effect::ClosePanel]);
        a.apply(0, vec![Effect::SwapPanels(1, 2)]);
        assert_eq!(a.visible, [true, true, false]);
        // The first panel never takes part.
        a.apply(0, vec![Effect::SwapPanels(0, 1), Effect::SwapPanels(1, 0)]);
        assert_eq!(a.visible, [true, true, false]);
    }

    #[test]
    fn toggling_the_first_panel_leaves_it_visible_and_another_toggles() {
        let mut ui = ui();
        show(&mut ui, loaded(1, 1));
        let a = ui.active.as_mut().unwrap();
        assert_eq!(a.visible, [true, true, false]);
        a.apply(1, vec![Effect::TogglePanel(0)]);
        assert!(a.visible[0], "panel 0 stays");
        a.apply(0, vec![Effect::TogglePanel(2)]);
        assert_eq!(a.visible, [true, true, true]);
        a.apply(2, vec![Effect::ClosePanel]);
        assert_eq!(a.visible, [true, true, false]);
        assert!(!a.quit);
        a.apply(0, vec![Effect::ClosePanel]);
        assert!(a.quit);
        assert!(ui.take_quit());
        assert!(!ui.take_quit());
    }

    #[test]
    fn the_first_panel_asks_to_quit_and_a_member_only_hides() {
        let mut ui = ui();
        show(&mut ui, loaded(1, 1));
        assert!(ui.close_requested(0));
        assert!(!ui.close_requested(1));
        assert!(!ui.active.as_ref().unwrap().visible[1]);
    }

    #[test]
    fn the_plan_follows_the_skin_and_the_saved_layout_of_the_same_skin() {
        let saved = SavedLayout {
            skin: "x.skin".to_owned(),
            panels: vec![
                SavedPanel {
                    id: "library".to_owned(),
                    x: 500,
                    y: 40,
                    visible: true,
                    size: None,
                },
                // Panel 0 cannot be saved as hidden.
                SavedPanel {
                    id: "main".to_owned(),
                    x: 10,
                    y: 20,
                    visible: false,
                    size: None,
                },
                SavedPanel {
                    id: "equalizer".to_owned(),
                    x: -9000,
                    y: 0,
                    visible: true,
                    size: None,
                },
            ],
        };
        let mut ui = SkinUi::new(
            SkinLibrary::new(None, Arc::new(SkinRegistry::new())),
            SkinPrefs {
                on_top: false,
                saved: Some(saved),
            },
        );
        ui.active = Some(Active::new(loaded(1, 1), ui.saved.as_ref(), 1, false));
        let plan = ui.plan().unwrap();
        let shown: Vec<bool> = plan.panels.iter().map(|p| p.visible).collect();
        assert_eq!(shown, [true, true, true]);
        assert_eq!(plan.panels[0].size, (100, 50));
        assert!(plan.panels[0].shape.is_none());
        let areas = [ScreenRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];
        let leader = ScreenPoint { x: 100, y: 100 };
        let origins = ui.start_layout(leader, 1.0, &areas);
        // The library is saved on the screen; the equalizer's saved place is off every screen,
        // so it sits under the leader.
        assert_eq!(origins[2], ScreenPoint { x: 500, y: 40 });
        assert_eq!(origins[1], ScreenPoint { x: 100, y: 150 });
        // At k = 2 sizes double.
        ui.set_pixel_scale(2);
        let origins = ui.start_layout(leader, 1.0, &areas);
        assert_eq!(origins[1], ScreenPoint { x: 100, y: 200 });
        // A saved layout of another skin is not used.
        ui.active.as_mut().unwrap().name = "other.skin".to_owned();
        let origins = ui.start_layout(leader, 1.0, &areas);
        assert_eq!(origins[2], ScreenPoint { x: 300, y: 100 });
    }

    #[test]
    fn drive_applies_the_queued_visibility_and_on_top() {
        let mut ui = SkinUi::new(
            SkinLibrary::new(None, Arc::new(SkinRegistry::new())),
            SkinPrefs {
                on_top: true,
                saved: None,
            },
        );
        show(&mut ui, loaded(1, 1));
        let frame = |x, y| ScreenRect {
            x,
            y,
            w: 100,
            h: 50,
        };
        let mut w = FakeWindows::new(vec![frame(0, 0), frame(0, 50), frame(100, 0)]);
        w.visible = vec![true, true, false];
        ui.active
            .as_mut()
            .unwrap()
            .apply(0, vec![Effect::TogglePanel(2)]);
        ui.drive(&mut w);
        assert_eq!(w.visible, [true, true, true]);
        assert!(w.docked[1] && w.docked[2]);
        assert!(w.on_top);
    }

    #[test]
    fn a_library_page_for_an_unknown_request_is_ignored_and_an_error_takes_the_title() {
        use whipspot_core::player::{LibraryPage, RequestId};
        use whipspot_core::provider::TrackPage;
        let mut ui = ui();
        ui.lists.model.sync(Some("p"));
        ui.route(PlayerNotice::LibraryPage {
            request: RequestId(99),
            result: Ok(LibraryPage::Tracks(TrackPage {
                items: vec![whipspot_core::provider::Track {
                    uri: "t:1".into(),
                    name: "t".into(),
                    artist: "a".into(),
                    album: "b".into(),
                    duration_ms: 1000,
                    images: vec![],
                    year: None,
                    album_ref: None,
                    artists: vec![],
                }],
                skipped: 0,
                next_offset: None,
                total: None,
                truncated: false,
            })),
        });
        assert!(ui.lists.model.liked.items().is_empty());
        assert!(ui.lists.model.playlists.items().is_empty());
        assert!(ui.title.is_none());
        ui.route(PlayerNotice::Error("boom"));
        assert_eq!(ui.title.as_ref().map(|t| t.text.as_str()), Some("boom"));
    }

    /// The test skin with a resizable "playlist" (275x116 up in steps of 25x29) under the
    /// equalizer, a fixed "under" panel below it.
    fn resizable() -> LoadedSkin {
        let mut s = skin(1);
        let mut playlist = panel("playlist", 275, 116, Some(("equalizer", Edge::Bottom)));
        playlist.resize = Some(Resize {
            min: Size { w: 275, h: 116 },
            step: Size { w: 25, h: 29 },
        });
        s.panels[2] = playlist;
        s.panels
            .push(panel("under", 275, 50, Some(("playlist", Edge::Bottom))));
        LoadedSkin {
            generation: 1,
            name: "x.skin".to_owned(),
            skin: Arc::new(s),
        }
    }

    fn saved_sizes(sizes: &[(&str, Option<(u32, u32)>)]) -> SavedLayout {
        SavedLayout {
            skin: "x.skin".to_owned(),
            panels: sizes
                .iter()
                .map(|&(id, size)| SavedPanel {
                    id: id.to_owned(),
                    x: 0,
                    y: 0,
                    visible: true,
                    size,
                })
                .collect(),
        }
    }

    #[test]
    fn a_saved_size_opens_on_the_lattice_and_a_fixed_panel_ignores_it() {
        let open = |saved: &SavedLayout| {
            let a = Active::new(resizable(), Some(saved), 1, false);
            (
                a.size.clone(),
                a.plan.panels.iter().map(|p| p.size).collect::<Vec<_>>(),
            )
        };
        let (size, plan) = open(&saved_sizes(&[("playlist", Some((325, 174)))]));
        assert_eq!(size[2], (325, 174));
        assert_eq!(plan[2], (325, 174), "the window opens at it");
        assert_eq!(size[0], (100, 50));
        // A size off the lattice lands on it; one below the minimum is the minimum.
        let (size, _) = open(&saved_sizes(&[("playlist", Some((330, 180)))]));
        assert_eq!(size[2], (325, 174));
        let (size, plan) = open(&saved_sizes(&[("playlist", Some((1, 1)))]));
        assert_eq!((size[2], plan[2]), ((275, 116), (275, 116)));
        // The main panel and a panel with no size keep the plan's.
        let (size, _) = open(&saved_sizes(&[
            ("main", Some((999, 999))),
            ("under", Some((10, 10))),
        ]));
        assert_eq!((size[0], size[3]), ((100, 50), (275, 50)));
        assert_eq!(size[2], (275, 116));
        // A saved layout of another skin is not used.
        let mut other = saved_sizes(&[("playlist", Some((325, 174)))]);
        other.skin = "y.skin".to_owned();
        assert_eq!(open(&other).0[2], (275, 116));
    }

    #[test]
    fn the_report_carries_the_size_of_resizable_panels_only_and_it_round_trips() {
        let mut ui = ui();
        ui.saved = Some(saved_sizes(&[("playlist", Some((325, 174)))]));
        ui.active = Some(Active::new(resizable(), ui.saved.as_ref(), 1, false));
        let frame = |w, h| ScreenRect { x: 0, y: 0, w, h };
        let w = FakeWindows::new(vec![
            frame(100, 50),
            frame(100, 50),
            frame(325, 174),
            frame(275, 50),
        ]);
        let report = ui.report(&w);
        let sizes: Vec<Option<(u32, u32)>> = report.panels.iter().map(|p| p.size).collect();
        assert_eq!(sizes, [None, None, Some((325, 174)), None]);
        // Saved and loaded again, the size is the same.
        let again = SavedLayout {
            skin: "x.skin".to_owned(),
            panels: report.panels,
        };
        let a = Active::new(resizable(), Some(&again), 1, false);
        assert_eq!(a.size[2], (325, 174));
    }

    #[test]
    fn a_grip_drag_resizes_through_the_ops_and_a_rescale_keeps_the_size() {
        let mut ui = ui();
        ui.active = Some(Active::new(resizable(), None, 1, false));
        let frame = |x, y, w, h| ScreenRect { x, y, w, h };
        let mut w = FakeWindows::new(vec![
            frame(0, 0, 100, 50),
            frame(0, 50, 100, 50),
            frame(0, 100, 275, 116),
            frame(0, 216, 275, 50),
        ]);
        ui.drive(&mut w);
        let a = ui.active.as_mut().unwrap();
        a.apply(2, vec![Effect::ResizePress]);
        w.pointer = Some(ScreenPoint { x: 10, y: 10 });
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 70, y: 50 });
        ui.drive(&mut w);
        assert_eq!(ui.active.as_ref().unwrap().size[2], (325, 145));
        assert_eq!(w.frames[2].w, 325);
        // The panel below followed by 29.
        assert_eq!(w.origin(3), (0, 245));
        // The pixel scale changing leaves the sizes.
        ui.rescaled(2, 1.0);
        ui.drive(&mut w);
        assert_eq!(ui.active.as_ref().unwrap().size[2], (325, 145));
        // Released: no more resizing.
        ui.active
            .as_mut()
            .unwrap()
            .apply(2, vec![Effect::ResizeRelease]);
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 900, y: 900 });
        ui.drive(&mut w);
        assert_eq!(ui.active.as_ref().unwrap().size[2], (325, 145));
    }

    /// The test skin with a drag area over the whole main panel.
    fn draggable() -> LoadedSkin {
        use whipspot_core::skin::{Align, Anchor, Rect, Widget, WidgetKind};
        let mut s = skin(1);
        s.panels[0].widgets.push(Widget {
            rect: Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50,
            },
            anchor: Anchor {
                h: Align::Start,
                v: Align::Start,
            },
            kind: WidgetKind::DragArea,
        });
        LoadedSkin {
            generation: 1,
            name: "x.skin".to_owned(),
            skin: Arc::new(s),
        }
    }

    fn press_at(x: f32, y: f32, pressed: bool) -> Event {
        Event::PointerButton {
            pos: (x, y),
            button: PointerButton::Primary,
            pressed,
            modifiers: lune_ui::input::Modifiers::default(),
        }
    }

    /// Main at (0, 0), the equalizer docked under it, the library away.
    fn drag_windows() -> FakeWindows {
        let frame = |x, y| ScreenRect {
            x,
            y,
            w: 100,
            h: 50,
        };
        FakeWindows::new(vec![frame(0, 0), frame(0, 50), frame(500, 0)])
    }

    /// One frame of panel `panel` with `events`, its effects applied and the ops driven.
    fn frame(ui: &mut SkinUi, w: &mut FakeWindows, panel: usize, events: &[Event]) {
        const M: Metrics = Metrics { k: 1.0, scale: 1.0 };
        let state = super::fake::player_state();
        let a = ui.active.as_mut().unwrap();
        let effects = a.events(panel, events, M, &state);
        let _ = a.apply(panel, effects);
        ui.drive(w);
    }

    #[test]
    fn a_move_at_another_panel_ends_a_drag_whose_release_was_lost() {
        let mut ui = ui();
        ui.active = Some(Active::new(draggable(), None, 1, false));
        let mut w = drag_windows();
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 10, y: 10 });
        frame(&mut ui, &mut w, 0, &[press_at(10.0, 10.0, true)]);
        assert!(ui.active.as_ref().unwrap().drag.dragging());
        w.pointer = Some(ScreenPoint { x: 40, y: 30 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (10.0, 10.0) }],
        );
        assert_eq!(w.origin(0), (30, 20));
        assert_eq!(w.origin(1), (30, 70), "the docked equalizer follows");
        // The release went to no panel; then the pointer moves over the library: no button is
        // held there, so the drag ends as a release would.
        w.pointer = Some(ScreenPoint { x: 520, y: 10 });
        frame(
            &mut ui,
            &mut w,
            2,
            &[Event::PointerMoved { pos: (20.0, 10.0) }],
        );
        let a = ui.active.as_ref().unwrap();
        assert!(!a.drag.dragging());
        assert!(a.holder.is_none());
        assert!(!a.panels[0].input.held());
        assert!(w.docked[1], "the docks were recomputed");
        // A later move moves nothing.
        let moved = w.moves.len();
        w.pointer = Some(ScreenPoint { x: 700, y: 300 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (5.0, 5.0) }],
        );
        frame(
            &mut ui,
            &mut w,
            2,
            &[Event::PointerMoved { pos: (5.0, 5.0) }],
        );
        assert_eq!(w.moves.len(), moved);
        assert_eq!(w.origin(0), (30, 20));
    }

    #[test]
    fn a_new_press_ends_a_drag_whose_release_was_lost_and_a_real_drag_is_kept() {
        let mut ui = ui();
        ui.active = Some(Active::new(draggable(), None, 1, false));
        let mut w = drag_windows();
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 10, y: 10 });
        frame(&mut ui, &mut w, 0, &[press_at(10.0, 10.0, true)]);
        // Moves at the pressed panel keep the drag.
        w.pointer = Some(ScreenPoint { x: 60, y: 40 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (10.0, 10.0) }],
        );
        assert!(ui.active.as_ref().unwrap().drag.dragging());
        assert_eq!(w.origin(0), (50, 30));
        // A press on the library (its release never came on main): the drag ends.
        frame(&mut ui, &mut w, 2, &[press_at(5.0, 5.0, true)]);
        assert!(!ui.active.as_ref().unwrap().drag.dragging());
        let moved = w.moves.len();
        w.pointer = Some(ScreenPoint { x: 300, y: 300 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (5.0, 5.0) }],
        );
        assert_eq!(w.moves.len(), moved);
        // A press on main again while held there (the release lost) starts a fresh drag from
        // the new press, not the old one.
        w.pointer = Some(ScreenPoint { x: 55, y: 35 });
        frame(&mut ui, &mut w, 0, &[press_at(5.0, 5.0, true)]);
        w.pointer = Some(ScreenPoint { x: 65, y: 45 });
        frame(&mut ui, &mut w, 0, &[press_at(15.0, 15.0, true)]);
        assert!(ui.active.as_ref().unwrap().drag.dragging());
        w.pointer = Some(ScreenPoint { x: 165, y: 45 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (15.0, 15.0) }],
        );
        assert_eq!(w.origin(0), (150, 30));
        // A real release ends it.
        frame(&mut ui, &mut w, 0, &[press_at(5.0, 5.0, false)]);
        assert!(!ui.active.as_ref().unwrap().drag.dragging());
    }

    /// The host's release from the OS's button state (macOS): `buttons_up` at the holder, then
    /// the release it fed the panel's translator, then perhaps the window's own late release. The
    /// resize ends at the first; the two releases after it do nothing.
    #[test]
    fn an_os_release_ends_a_resize_and_the_releases_after_it_do_nothing() {
        use whipspot_core::skin::{Align, Anchor, Rect, Widget, WidgetKind};
        let mut loaded = resizable();
        let mut s = (*loaded.skin).clone();
        s.panels[2].widgets.push(Widget {
            rect: Rect {
                x: 0,
                y: 0,
                w: 275,
                h: 116,
            },
            anchor: Anchor {
                h: Align::Start,
                v: Align::Start,
            },
            kind: WidgetKind::ResizeArea,
        });
        loaded.skin = Arc::new(s);
        let mut ui = ui();
        ui.active = Some(Active::new(loaded, None, 1, false));
        let rect = |x, y, w, h| ScreenRect { x, y, w, h };
        let mut w = FakeWindows::new(vec![
            rect(0, 0, 100, 50),
            rect(0, 50, 100, 50),
            rect(0, 100, 275, 116),
            rect(0, 216, 275, 50),
        ]);
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 10, y: 110 });
        frame(&mut ui, &mut w, 2, &[press_at(10.0, 10.0, true)]);
        assert!(ui.active.as_ref().unwrap().resize.resizing());
        w.pointer = Some(ScreenPoint { x: 60, y: 110 });
        frame(
            &mut ui,
            &mut w,
            2,
            &[Event::PointerMoved { pos: (60.0, 10.0) }],
        );
        assert_eq!(ui.active.as_ref().unwrap().size[2], (325, 116));
        // The OS says the button is up: the host calls `buttons_up` at the holder.
        ui.buttons_up(2);
        w.pointer = Some(ScreenPoint { x: 200, y: 300 });
        ui.drive(&mut w);
        assert!(
            !ui.active.as_ref().unwrap().resize.resizing(),
            "ended by buttons_up"
        );
        assert_eq!(w.resizes.len(), 1);
        // The release the host fed the translator, then the window's own late one.
        frame(&mut ui, &mut w, 2, &[press_at(60.0, 10.0, false)]);
        frame(&mut ui, &mut w, 2, &[press_at(60.0, 10.0, false)]);
        let a = ui.active.as_ref().unwrap();
        assert!(!a.resize.resizing());
        assert!(a.holder.is_none());
        assert!(!a.panels[2].input.held());
        assert_eq!(a.size[2], (325, 116), "no resize after the release");
        assert_eq!(w.resizes.len(), 1);
        assert!(w.docked[3], "the docks were recomputed");
    }

    #[test]
    fn the_hosts_buttons_up_at_the_holder_ends_the_drag_whose_release_was_lost() {
        let mut ui = ui();
        ui.active = Some(Active::new(draggable(), None, 1, false));
        let mut w = drag_windows();
        ui.drive(&mut w);
        w.pointer = Some(ScreenPoint { x: 10, y: 10 });
        frame(&mut ui, &mut w, 0, &[press_at(10.0, 10.0, true)]);
        w.pointer = Some(ScreenPoint { x: 60, y: 40 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (10.0, 10.0) }],
        );
        assert_eq!(w.origin(0), (50, 30));
        // Another panel's buttons going up does nothing.
        ui.buttons_up(2);
        assert!(ui.active.as_ref().unwrap().holder.is_some());
        // The holder's buttons are up (released where the toolkit did not see it, or focus
        // lost); its moves then keep coming at the holder itself.
        ui.buttons_up(0);
        w.pointer = Some(ScreenPoint { x: 160, y: 140 });
        frame(
            &mut ui,
            &mut w,
            0,
            &[Event::PointerMoved { pos: (10.0, 10.0) }],
        );
        let a = ui.active.as_ref().unwrap();
        assert!(!a.drag.dragging());
        assert!(a.holder.is_none());
        assert!(!a.panels[0].input.held());
        assert_eq!(
            w.origin(0),
            (50, 30),
            "the move at the holder moved nothing"
        );
        assert!(w.docked[1], "the docks were recomputed");
        // The toolkit's own release arriving after it changes nothing.
        frame(&mut ui, &mut w, 0, &[press_at(10.0, 10.0, false)]);
        assert!(!ui.active.as_ref().unwrap().drag.dragging());
        assert_eq!(w.origin(0), (50, 30));
    }
}
