//! The tray / menu-bar icon (D7, D8): the luneui `Tray` item, its click rules and the popover
//! window the menu fills. The rows, texts and picks are `whipspot_ui::tray`; this file owns the OS
//! item and the window, shaped after luneui's gallery (`apps/gallery/src/popover.rs`).
//!
//! The popover is a second window on the host's one `Gpu` and `TextCache`, made on the first click
//! and reused after. Its guard restores the OS window before it is freed, so everything here is
//! dropped on the main thread, the popover before the item.
//!
//! [`smoke`] is the demo's `--smoke-tray`: the same control on a bare event loop, driven by a
//! programmatic click, on the real desktop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use lune_ui::frame::FrameRenderer;
use lune_ui::input::{OutputState, Repaint, Translator, control_flow};
use lune_ui::lunegit_dark::THEME;
use lune_ui::material::{self, Material};
use lune_ui::paint::{Painter, Rect};
use lune_ui::popover::PopoverWindow;
use lune_ui::shortcuts::Shortcuts;
use lune_ui::surface::{Acquired, WindowSurface};
use lune_ui::tray::{
    Tray, TrayAction, TrayButton, TrayError, TrayEvent, TrayImage, TrayRect, TraySurface,
    TrayToggle, bar_from_tray, place_surface, set_event_handler,
};
use lune_ui::ui::Ui;
use lune_ui::window::popover_attributes;
use whipspot_core::player::PlayerHandle;
use whipspot_ui::tray::{
    Page, Pick, TrayFrame, TrayMenu, TrayRequest, icon_rgba, tooltip as tooltip_for,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use super::frame::{UiState, params};
use super::{HostEvent, Shared, WindowView, make_surface, platform};

/// The side of the picture the OS gets (it scales it for the bar).
const ICON_SIDE: u32 = 32;
/// The popover window's first size, logical px (the first open sizes it to the menu).
const FIRST_SIZE: (f64, f64) = (240.0, 320.0);
/// The corner radius of the OS glass behind the menu.
const GLASS_RADIUS: f64 = 9.0;

/// Sends the tray item's clicks to the event loop, as luneui asks. Call it before the first
/// [`Tray::new`]: only the first call takes effect.
pub(super) fn route_tray_events(proxy: EventLoopProxy<HostEvent>) {
    set_event_handler(move |event| {
        let _ = proxy.send_event(HostEvent::Tray(event));
    });
}

/// The menu as a window view: it draws only while the menu is open, so closing it lets the exit
/// play before the window hides.
pub(super) struct MenuView {
    menu: TrayMenu,
    open: bool,
    last: TrayFrame,
}

impl WindowView for MenuView {
    fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        self.last = if self.open {
            let area = ui.painter().window;
            self.menu.frame(ui, area, player)
        } else {
            TrayFrame::default()
        };
    }
}

/// What one popover frame did.
struct Drawn {
    repaint: Repaint,
    /// The window was hidden after this frame: the menu had closed and gone still.
    hidden: bool,
}

/// The window the menu fills. Fields drop in order: the surface (it holds a handle to the window),
/// then the guard, which restores the window, then the rest.
struct PopoverWin {
    surface: WindowSurface,
    guard: PopoverWindow,
    frame: FrameRenderer,
    ui: UiState,
    translator: Translator,
    output: OutputState,
    visible: bool,
    /// The window gained focus since the menu opened: only then can a loss of it be real.
    had_focus: bool,
    frames: u64,
}

impl PopoverWin {
    fn new(
        event_loop: &ActiveEventLoop,
        shared: &mut Option<Shared>,
        system_fonts: bool,
    ) -> Result<Self, String> {
        let window = Arc::new(
            event_loop
                .create_window(popover_attributes(FIRST_SIZE))
                .map_err(|e| format!("popover window: {e}"))?,
        );
        let guard = PopoverWindow::wrap(window.clone())?;
        if material::apply_rounded(&window, Material::Vibrancy, GLASS_RADIUS).is_err() {
            log::warn!("the tray menu's glass is unavailable");
        }
        let raw = make_surface(shared, event_loop, &window, system_fonts)?;
        let Some(shared) = shared.as_mut() else {
            return Err("gpu: not initialised".into());
        };
        let size = window.inner_size();
        let surface = WindowSurface::new(&shared.gpu, raw, (size.width, size.height))?;
        let frame = FrameRenderer::new(&mut shared.gpu, surface.format());
        Ok(Self {
            translator: Translator::new(size, window.scale_factor()),
            surface,
            guard,
            frame,
            ui: UiState::new(Shortcuts::default()),
            output: OutputState::new(),
            visible: false,
            had_focus: false,
            frames: 0,
        })
    }

    fn window(&self) -> &Arc<Window> {
        self.guard.window()
    }

    /// The monitor the tray rect is on (the one holding its centre, else the primary): its
    /// rect in physical px and its scale.
    fn screen_of(&self, tray: TrayRect) -> (Rect, f64) {
        let window = self.window();
        let centre = (tray.x + tray.w / 2.0, tray.y + tray.h / 2.0);
        let rect_of = |m: &winit::monitor::MonitorHandle| {
            let (p, s) = (m.position(), m.size());
            (
                Rect::new(p.x as f32, p.y as f32, s.width as f32, s.height as f32),
                m.scale_factor(),
            )
        };
        window
            .available_monitors()
            .map(|m| rect_of(&m))
            .find(|(r, _)| r.contains((centre.0 as f32, centre.1 as f32)))
            .or_else(|| window.primary_monitor().map(|m| rect_of(&m)))
            .unwrap_or((Rect::new(0.0, 0.0, 1920.0, 1080.0), window.scale_factor()))
    }

    /// Sizes the window to `logical` and puts it where the menu goes from the item at `tray`.
    fn place(&mut self, kind: TraySurface, tray: TrayRect, logical: (f32, f32)) {
        let (screen, scale) = self.screen_of(tray);
        let at = placed_position(kind, screen, scale, tray, logical);
        let window = self.window().clone();
        // Size first, then move: macOS keeps a resized window's bottom edge where it was.
        let _ =
            window.request_inner_size(LogicalSize::new(f64::from(logical.0), f64::from(logical.1)));
        window.set_outer_position(PhysicalPosition::new(at.0.round(), at.1.round()));
    }

    /// The monitor scale at the item, for sizing the menu before the window is shown.
    fn scale_at(&self, tray: TrayRect) -> f64 {
        self.screen_of(tray).1
    }

    /// Shows the window (if it is not) for a menu just opened.
    fn show(&mut self) {
        let window = self.window().clone();
        // A hidden window gets `Focused(true)` when shown, and a late `Focused(false)` from its last
        // hide must not count; one still shown keeps the focus it has.
        self.had_focus = had_focus_at_open(self.visible, window.has_focus());
        if !self.visible {
            self.guard.show();
            self.visible = true;
        }
        window.request_redraw();
    }

    /// Feeds a window event; whether it was a real focus loss while the menu was open.
    fn window_event(&mut self, event: &WindowEvent, open: bool) -> bool {
        if matches!(event, WindowEvent::Focused(true)) && open {
            self.had_focus = true;
        }
        let lost = is_focus_loss(event, open, self.visible, self.had_focus);
        // The window has no close button; a resize reaches the surface in `render`.
        if !matches!(
            event,
            WindowEvent::CloseRequested | WindowEvent::RedrawRequested
        ) {
            self.translator.window_event(event);
            self.window().request_redraw();
        }
        lost
    }

    fn render(
        &mut self,
        shared: &mut Shared,
        view: &mut MenuView,
        player: &PlayerHandle,
        now: Duration,
    ) -> Result<Option<Drawn>, String> {
        if !self.visible {
            return Ok(None);
        }
        let window = self.window().clone();
        // A size asked for a moment ago has not always arrived as an event yet.
        let inner = window.inner_size();
        if (inner.width, inner.height) != self.surface.size() {
            self.surface.resize(&shared.gpu, inner.width, inner.height);
            self.translator.resized(inner);
        }
        let frame = match self.surface.acquire(&shared.gpu)? {
            Acquired::Frame(frame) => frame,
            Acquired::Skip => return Ok(None),
            Acquired::Redraw => {
                window.request_redraw();
                return Ok(None);
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = self.surface.size();
        let scale = window.scale_factor() as f32;
        let input = self.translator.take(now);
        let (scene, out) = self.ui.run(&input, &mut shared.text, view, player);
        self.output.apply(&window, &out, scale);
        self.frame
            .draw(
                &mut shared.gpu,
                &mut shared.text,
                &scene,
                &target,
                params(size, scale),
            )
            .map_err(|e| format!("render: {e}"))?;
        window.pre_present_notify();
        self.surface.present(&shared.gpu, frame);
        shared.gpu.end_frame(&mut shared.text);
        self.frames += 1;
        if view.last.dismissed || view.last.closed {
            view.open = false;
        }
        // Closed and still: the exit has played out, so the window can go.
        let hidden = !view.open && out.repaint == Repaint::Idle;
        if hidden {
            self.guard.hide();
            self.visible = false;
        }
        Ok(Some(Drawn {
            repaint: out.repaint,
            hidden,
        }))
    }
}

/// Where the window of `kind`, `logical` px across, goes, in physical desktop pixels. The tray
/// item's physical rect and the monitor's become logical ones in one place: `place_surface` and
/// `bar_from_tray` work in logical px in one coordinate space, so their gaps and margins are
/// logical whatever the monitor's scale. The origin stays physical (and global, so a second
/// monitor's offset is its own); only the offsets from it are divided by the monitor's scale, then
/// the result is multiplied back.
fn placed_position(
    kind: TraySurface,
    screen: Rect,
    scale: f64,
    tray: TrayRect,
    logical: (f32, f32),
) -> (f64, f64) {
    let (ox, oy) = (f64::from(screen.x), f64::from(screen.y));
    let screen_l = Rect::new(
        0.0,
        0.0,
        (f64::from(screen.w) / scale) as f32,
        (f64::from(screen.h) / scale) as f32,
    );
    let tray_l = Rect::new(
        ((tray.x - ox) / scale) as f32,
        ((tray.y - oy) / scale) as f32,
        (tray.w / scale) as f32,
        (tray.h / scale) as f32,
    );
    let placed = place_surface(kind, screen_l, bar_from_tray(screen_l, tray_l), logical);
    (
        ox + f64::from(placed.x) * scale,
        oy + f64::from(placed.y) * scale,
    )
}

/// Whether the popover's window counts as focused when the menu opens: a window not shown starts
/// without focus; one still shown has the focus the window reports.
fn had_focus_at_open(visible: bool, window_has_focus: bool) -> bool {
    visible && window_has_focus
}

/// A real focus loss: the window lost focus while the menu was open and shown, and had had it.
fn is_focus_loss(event: &WindowEvent, open: bool, visible: bool, had_focus: bool) -> bool {
    matches!(event, WindowEvent::Focused(false)) && open && visible && had_focus
}

/// The item in the tray, its click rules, the menu and, once first opened, the window of it.
pub(super) struct TrayControl {
    // The window goes before the item.
    popover: Option<PopoverWin>,
    tray: Tray,
    toggle: TrayToggle,
    view: MenuView,
    /// The tooltip the item shows now.
    tooltip: String,
    /// Where the item was when the menu was opened, and the size the window was given.
    anchor: Option<TrayRect>,
    shown_size: (f32, f32),
    /// What the popover's last frame asked of the loop.
    flow: ControlFlow,
}

impl TrayControl {
    /// Puts the item in the tray. Call [`route_tray_events`] first.
    pub(super) fn new(menu: TrayMenu, player: &PlayerHandle) -> Result<Self, TrayError> {
        let tooltip = tooltip_for(&player.state());
        let image = TrayImage {
            width: ICON_SIDE,
            height: ICON_SIDE,
            rgba: icon_rgba(ICON_SIDE),
        };
        let tray = Tray::new(image, &tooltip)?;
        Ok(Self {
            popover: None,
            tray,
            toggle: TrayToggle::new(),
            view: MenuView {
                menu,
                open: false,
                last: TrayFrame::default(),
            },
            tooltip,
            anchor: None,
            shown_size: (0.0, 0.0),
            flow: ControlFlow::Wait,
        })
    }

    /// The item's rect, once it is on a monitor.
    pub(super) fn placed_rect(&self, event_loop: &ActiveEventLoop) -> Option<TrayRect> {
        let rect = self.tray.rect()?;
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        event_loop
            .available_monitors()
            .any(|m| {
                let (p, s) = (m.position(), m.size());
                cx >= f64::from(p.x)
                    && cx < f64::from(p.x) + f64::from(s.width)
                    && cy >= f64::from(p.y)
                    && cy < f64::from(p.y) + f64::from(s.height)
            })
            .then_some(rect)
    }

    pub(super) fn window_id(&self) -> Option<WindowId> {
        self.popover.as_ref().map(|p| p.window().id())
    }

    pub(super) fn is_visible(&self) -> bool {
        self.popover.as_ref().is_some_and(|p| p.visible)
    }

    pub(super) fn frames(&self) -> u64 {
        self.popover.as_ref().map_or(0, |p| p.frames)
    }

    pub(super) fn page(&self) -> Page {
        self.view.menu.page()
    }

    /// The window's inner size, logical px, while it exists.
    pub(super) fn window_size(&self) -> Option<(f64, f64)> {
        let w = self.popover.as_ref()?.window();
        let s = w.inner_size().to_logical::<f64>(w.scale_factor());
        Some((s.width, s.height))
    }

    pub(super) fn flow(&self) -> ControlFlow {
        self.flow
    }

    /// A show or quit the menu asked for, once.
    pub(super) fn take_request(&mut self) -> Option<TrayRequest> {
        self.view.menu.take_request()
    }

    /// The size the current page needs, logical px, at the monitor scale of the item.
    fn wanted_size(
        &mut self,
        shared: &mut Shared,
        player: &PlayerHandle,
        scale: f64,
    ) -> (f32, f32) {
        let win = Rect::new(0.0, 0.0, 4096.0, 4096.0);
        let mut p: Painter<'_, ()> = Painter::new(scale as f32, win, &mut shared.text, None, THEME);
        self.view.menu.size(&mut p, &player.state())
    }

    /// A click on the item at `now`: the click rules say whether to open, close or ignore it.
    pub(super) fn click(
        &mut self,
        event_loop: &ActiveEventLoop,
        shared: &mut Option<Shared>,
        system_fonts: bool,
        event: TrayEvent,
        player: &PlayerHandle,
        now: Duration,
    ) -> Result<(), String> {
        let TrayEvent::Click { button, rect } = event;
        if matches!(button, TrayButton::Other) {
            return Ok(());
        }
        match self.toggle.click(platform::tray_surface(), now) {
            TrayAction::Open(kind) => {
                if self.popover.is_none() {
                    self.popover = Some(PopoverWin::new(event_loop, shared, system_fonts)?);
                }
                let (Some(shared), Some(popover)) = (shared.as_mut(), self.popover.as_mut()) else {
                    return Ok(());
                };
                self.view.menu.open();
                let scale = popover.scale_at(rect);
                let mut p: Painter<'_, ()> = Painter::new(
                    scale as f32,
                    Rect::new(0.0, 0.0, 4096.0, 4096.0),
                    &mut shared.text,
                    None,
                    THEME,
                );
                let logical = self.view.menu.size(&mut p, &player.state());
                popover.place(kind, rect, logical);
                self.anchor = Some(rect);
                self.shown_size = logical;
                self.view.open = true;
                popover.show();
            }
            TrayAction::Close => self.close(),
            TrayAction::None => {}
        }
        Ok(())
    }

    /// Asks the menu to close: its exit plays, then the window hides.
    pub(super) fn close(&mut self) {
        self.view.open = false;
        if let Some(p) = &self.popover {
            p.window().request_redraw();
        }
    }

    /// A pick as if the row had been pressed (the smoke's injected picks): its player action goes
    /// to `player`. Whether the page offered the pick.
    pub(super) fn inject(&mut self, pick: &Pick, player: &PlayerHandle) -> bool {
        let state = player.state();
        let Some(index) = self.view.menu.index_of(&state, pick) else {
            return false;
        };
        let (outcome, action) = self.view.menu.pick_at(index, &state);
        if let Some(action) = action {
            player.dispatch(action);
        }
        if outcome == whipspot_ui::tray::Outcome::Close {
            self.toggle.closed();
            self.close();
        } else if let Some(p) = &self.popover {
            p.window().request_redraw();
        }
        true
    }

    /// A window event of the popover (the caller checked the window).
    pub(super) fn window_event(&mut self, event: &WindowEvent, now: Duration) {
        let open = self.view.open;
        let Some(p) = &mut self.popover else { return };
        if p.window_event(event, open) {
            // The click rules need to know the menu closed from this, so the click that took the
            // focus does not reopen it.
            self.toggle.focus_lost(now);
            self.close();
        }
    }

    /// Draws the popover's frame (if shown) and keeps its size and the loop's schedule right.
    pub(super) fn render(
        &mut self,
        shared: &mut Shared,
        player: &PlayerHandle,
        origin: Instant,
    ) -> Result<(), String> {
        // A page change or an answer to the skins listing changes the rows: size the window again.
        if self.view.open
            && let Some(anchor) = self.anchor
        {
            let scale = match &self.popover {
                Some(p) => p.scale_at(anchor),
                None => 1.0,
            };
            let wanted = self.wanted_size(shared, player, scale);
            if wanted != self.shown_size {
                self.shown_size = wanted;
                if let Some(p) = &mut self.popover {
                    p.place(platform::tray_surface(), anchor, wanted);
                }
            }
        }
        let Some(p) = &mut self.popover else {
            return Ok(());
        };
        let drawn = p.render(shared, &mut self.view, player, origin.elapsed())?;
        if let Some(d) = drawn {
            if self.view.last.dismissed || self.view.last.closed {
                self.toggle.closed();
            }
            let (flow, redraw) = control_flow(d.repaint, origin);
            self.flow = if d.hidden { ControlFlow::Wait } else { flow };
            if redraw && !d.hidden {
                p.window().request_redraw();
            }
        }
        Ok(())
    }

    /// A redraw the popover asked for at a time that has come.
    pub(super) fn due(&mut self, now: Instant) {
        if matches!(self.flow, ControlFlow::WaitUntil(t) if t <= now + Duration::from_millis(2)) {
            self.flow = ControlFlow::Wait;
            if let Some(p) = &self.popover {
                p.window().request_redraw();
            }
        }
    }

    /// The player changed: the tooltip follows (only when it differs), and a shown menu redraws.
    pub(super) fn wake(&mut self, player: &PlayerHandle) {
        let tip = tooltip_for(&player.state());
        if tip != self.tooltip {
            if self.tray.set_tooltip(&tip).is_err() {
                log::warn!("the tray tooltip could not be set");
            }
            self.tooltip = tip;
        }
        if self.view.open
            && let Some(p) = &self.popover
        {
            p.window().request_redraw();
        }
    }
}

// ---- the demo's smoke -------------------------------------------------------------------------

/// How long the whole smoke may take before it fails.
const SMOKE_DEADLINE: Duration = Duration::from_secs(10);
/// The most the item may take to be placed by the OS.
const PLACE_WITHIN: Duration = Duration::from_secs(3);
/// Frames the first open must draw, and how long it stays open at least.
const OPEN_FRAMES: u64 = 10;
const OPEN_FOR: Duration = Duration::from_millis(300);
/// Frames each page change must draw.
const PAGE_FRAMES: u64 = 4;
/// How long a close may take to hide the window.
const HIDE_WITHIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Place,
    Open,
    Skins,
    Back,
    Close,
    Done,
}

struct Smoke<'p> {
    player: &'p PlayerHandle,
    menu: Option<TrayMenu>,
    control: Option<TrayControl>,
    shared: Option<Shared>,
    step: Step,
    step_at: Instant,
    frames_at: u64,
    start: Instant,
    main_size: Option<(f64, f64)>,
    check_fails: bool,
    error: Option<String>,
}

/// Runs the tray on a bare event loop on the real desktop: creates the icon, opens the menu by a
/// programmatic click at its rect, draws frames, switches to the skins page and back by injected
/// picks, closes the menu and removes the icon. With `check_fails` the run ends in an error
/// instead (the smoke's own failure path must be able to fail).
pub fn smoke(menu: TrayMenu, player: &PlayerHandle, check_fails: bool) -> anyhow::Result<()> {
    let event_loop = EventLoop::<HostEvent>::with_user_event()
        .build()
        .map_err(|e| anyhow!("event loop: {e}"))?;
    route_tray_events(event_loop.create_proxy());
    let now = Instant::now();
    let mut app = Smoke {
        player,
        menu: Some(menu),
        control: None,
        shared: None,
        step: Step::Place,
        step_at: now,
        frames_at: 0,
        start: now,
        main_size: None,
        check_fails,
        error: None,
    };
    let result = event_loop.run_app(&mut app);
    // The popover and the item go before the loop's state does.
    app.control = None;
    if let Some(e) = app.error.take() {
        return Err(anyhow!(e));
    }
    result.map_err(|e| anyhow!("event loop: {e}"))
}

impl Smoke<'_> {
    fn fail(&mut self, why: String) {
        self.error.get_or_insert(why);
        self.go(Step::Done);
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.step_at = Instant::now();
        self.frames_at = self.control.as_ref().map_or(0, TrayControl::frames);
    }

    /// The frames the popover drew since the step began.
    fn drawn(&self) -> u64 {
        self.control
            .as_ref()
            .map_or(0, TrayControl::frames)
            .saturating_sub(self.frames_at)
    }

    fn click(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(control), now) = (&mut self.control, self.start.elapsed()) else {
            return;
        };
        let Some(rect) = control.placed_rect(event_loop) else {
            return self.fail("the tray item has no rect".into());
        };
        let click = TrayEvent::Click {
            button: TrayButton::Right,
            rect,
        };
        if let Err(e) = control.click(event_loop, &mut self.shared, false, click, self.player, now)
        {
            self.fail(format!("click: {e}"));
        }
    }

    fn advance(&mut self, event_loop: &ActiveEventLoop) {
        if self.start.elapsed() > SMOKE_DEADLINE && self.error.is_none() {
            return self.fail(format!("not finished after {} s", SMOKE_DEADLINE.as_secs()));
        }
        let waited = self.step_at.elapsed();
        match self.step {
            Step::Place => {
                let placed = self
                    .control
                    .as_ref()
                    .and_then(|c| c.placed_rect(event_loop));
                if placed.is_some() {
                    self.click(event_loop);
                    self.go(Step::Open);
                } else if waited > PLACE_WITHIN {
                    self.fail("the tray item was not placed by the OS".into());
                }
            }
            Step::Open => {
                let Some(control) = &self.control else { return };
                if control.is_visible() && self.drawn() >= OPEN_FRAMES && waited >= OPEN_FOR {
                    self.main_size = control.window_size();
                    println!(
                        "smoke-tray: {} frames drawn in the first open, window {:?} logical px",
                        self.drawn(),
                        self.main_size
                    );
                    if let Some(c) = &mut self.control
                        && !c.inject(&Pick::OpenSkins, self.player)
                    {
                        return self.fail("the main page has no Skin row".into());
                    }
                    self.go(Step::Skins);
                } else if waited > HIDE_WITHIN {
                    let n = self.drawn();
                    self.fail(format!("only {n} frames drew in the first open"));
                }
            }
            Step::Skins => {
                let Some(control) = &self.control else { return };
                if self.drawn() >= PAGE_FRAMES && waited >= OPEN_FOR {
                    if control.page() != Page::Skins {
                        return self.fail("the skins page did not open".into());
                    }
                    println!(
                        "smoke-tray: skins page window {:?} logical px",
                        control.window_size()
                    );
                    if self.check_fails {
                        return self.fail("forced failure".into());
                    }
                    if let Some(c) = &mut self.control
                        && !c.inject(&Pick::Back, self.player)
                    {
                        return self.fail("the skins page has no Back row".into());
                    }
                    self.go(Step::Back);
                } else if waited > HIDE_WITHIN {
                    self.fail("the skins page drew too few frames".into());
                }
            }
            Step::Back => {
                let Some(control) = &self.control else { return };
                if self.drawn() >= PAGE_FRAMES && waited >= OPEN_FOR {
                    if control.page() != Page::Main {
                        return self.fail("the main page did not come back".into());
                    }
                    if control.window_size() != self.main_size {
                        let (a, b) = (control.window_size(), self.main_size);
                        return self.fail(format!("the window is {a:?} again, not {b:?}"));
                    }
                    // A second click on the item closes the menu.
                    self.click(event_loop);
                    self.go(Step::Close);
                } else if waited > HIDE_WITHIN {
                    self.fail("the main page drew too few frames".into());
                }
            }
            Step::Close => {
                if self.control.as_ref().is_some_and(|c| !c.is_visible()) {
                    self.go(Step::Done);
                } else if waited > HIDE_WITHIN {
                    self.fail("the window did not hide after a close".into());
                }
            }
            Step::Done => {
                self.control = None;
                event_loop.exit();
            }
        }
    }
}

impl ApplicationHandler<HostEvent> for Smoke<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.control.is_some() {
            return;
        }
        if event_loop.available_monitors().next().is_none() {
            self.error = Some("no monitor: there is no GUI session".to_owned());
            return event_loop.exit();
        }
        let Some(menu) = self.menu.take() else { return };
        match TrayControl::new(menu, self.player) {
            Ok(control) => self.control = Some(control),
            Err(e) => {
                self.error = Some(format!("the tray item could not be created: {e}"));
                return event_loop.exit();
            }
        }
        self.step_at = Instant::now();
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: StartCause) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.control.is_none() {
            return;
        }
        self.advance(event_loop);
        // Frames are forced while the menu is up, so the count does not depend on how still it gets.
        if let Some(c) = &self.control
            && c.is_visible()
            && let Some(p) = &c.popover
        {
            p.window().request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(16),
        ));
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: HostEvent) {
        // A real click during the run is not part of it: the run clicks for itself.
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if event_loop.exiting() {
            return;
        }
        let now = self.start.elapsed();
        let Some(control) = &mut self.control else {
            return;
        };
        if control.window_id() != Some(id) {
            return;
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            let Some(shared) = &mut self.shared else {
                return;
            };
            if let Err(e) = control.render(shared, self.player, self.start) {
                self.fail(format!("render: {e}"));
            }
        } else {
            control.window_event(&event, now);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.control = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOST: WindowEvent = WindowEvent::Focused(false);

    fn tray(x: f64, y: f64, w: f64, h: f64) -> TrayRect {
        TrayRect { x, y, w, h }
    }

    #[test]
    fn a_window_shown_for_the_first_time_has_no_focus_until_it_gets_it() {
        let had = had_focus_at_open(false, false);
        assert!(!is_focus_loss(&LOST, true, true, had));
        assert!(is_focus_loss(&LOST, true, true, true));
    }

    #[test]
    fn a_reopen_of_a_shown_window_keeps_its_focus_so_the_next_loss_counts() {
        let had = had_focus_at_open(true, true);
        assert!(had);
        assert!(is_focus_loss(&LOST, true, true, had));
        assert!(!is_focus_loss(
            &LOST,
            true,
            true,
            had_focus_at_open(true, false)
        ));
    }

    #[test]
    fn a_loss_counts_only_while_a_menu_is_open_and_shown() {
        assert!(!is_focus_loss(&LOST, false, true, true));
        assert!(!is_focus_loss(&LOST, true, false, true));
        assert!(!is_focus_loss(
            &WindowEvent::Focused(true),
            true,
            true,
            true
        ));
    }

    #[test]
    fn the_windows_menu_keeps_its_logical_gap_and_margin_at_150_percent() {
        // 2880 x 1620 physical at 150% is 1920 x 1080 logical; the taskbar's item at (2460, 1560,
        // 48, 60) physical is (1640, 1040, 32, 40) logical: the menu is 8 logical px above the bar
        // (bottom 1032) and 12 in from the right.
        let at = placed_position(
            TraySurface::MenuOverBar,
            Rect::new(0.0, 0.0, 2880.0, 1620.0),
            1.5,
            tray(2460.0, 1560.0, 48.0, 60.0),
            (200.0, 300.0),
        );
        assert_eq!(at, ((1920.0 - 12.0 - 200.0) * 1.5, (1032.0 - 300.0) * 1.5));
    }

    #[test]
    fn a_second_monitor_places_by_its_own_origin() {
        let first = placed_position(
            TraySurface::MenuUnderBar,
            Rect::new(0.0, 0.0, 4800.0, 2700.0),
            2.0,
            tray(3726.0, -2.0, 68.0, 66.0),
            (220.0, 270.0),
        );
        let second = placed_position(
            TraySurface::MenuUnderBar,
            Rect::new(4800.0, 0.0, 4800.0, 2700.0),
            2.0,
            tray(4800.0 + 3726.0, -2.0, 68.0, 66.0),
            (220.0, 270.0),
        );
        assert_eq!(second, (first.0 + 4800.0, first.1));
    }

    // ---- the menu's picks drive a real player (on the testkit's fake provider) ---------------

    use std::sync::Arc;

    use whipspot_core::engine::{DeviceFormat, OutputMode, OutputRequest, SampleFormat};
    use whipspot_core::player::{self, EngineSetup, PlayState, Player, PlayerAction, PlayerConfig};
    use whipspot_core::provider::{ProviderRegistry, Track};
    use whipspot_core::skin::SkinRegistry;
    use whipspot_testkit::{FakeOutput, FakeProvider};
    use whipspot_ui::skin::SkinLibrary;

    fn wait_state(h: &PlayerHandle, done: impl Fn(&whipspot_core::player::PlayerState) -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done(&h.state()) {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn tray_picks_pause_resume_and_step_a_real_player() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let output = FakeOutput::new(
            DeviceFormat {
                rate: 44_100,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32,
            },
            OutputMode::Shared,
        );
        let engine = EngineSetup::Output {
            output: Box::new(output),
            request: OutputRequest::default(),
        };
        let (ctx, wiring) = player::wiring(engine, 64);
        let mut registry = ProviderRegistry::new();
        registry
            .register(Arc::new(FakeProvider::new("fakea", true).with_context(ctx)))
            .expect("register");
        let h = Player::start(registry, wiring, PlayerConfig::default(), rt.handle());
        let track = |n: u32| Track {
            uri: format!("fakea:track:{n}"),
            name: format!("Track {n}"),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 200_000,
            images: vec![],
            year: None,
        };
        h.dispatch(PlayerAction::PlayItem {
            context: vec![track(1), track(2)],
            index: 0,
        });
        wait_state(&h, |s| s.play_state == PlayState::Playing);
        let library = SkinLibrary::new(None, Arc::new(SkinRegistry::new()));
        let mut menu = TrayMenu::new(library, false);
        let mut pick = |pick: Pick| {
            let state = h.state();
            let index = menu.index_of(&state, &pick).expect("the row exists");
            let (_, action) = menu.pick_at(index, &state);
            if let Some(action) = action {
                h.dispatch(action);
            }
        };
        // Pause, then play again.
        pick(Pick::Toggle);
        wait_state(&h, |s| s.play_state == PlayState::Paused);
        pick(Pick::Toggle);
        wait_state(&h, |s| s.play_state == PlayState::Playing);
        // Next moves to the second track, Previous back to the first.
        pick(Pick::Next);
        wait_state(&h, |s| {
            s.current
                .as_ref()
                .is_some_and(|c| c.track.uri == "fakea:track:2")
        });
        pick(Pick::Previous);
        wait_state(&h, |s| {
            s.current
                .as_ref()
                .is_some_and(|c| c.track.uri == "fakea:track:1")
        });
        // The tooltip names what plays.
        assert!(
            whipspot_ui::tray::tooltip(&h.state()).contains("Track 1"),
            "tooltip"
        );
        drop(h);
        rt.shutdown_timeout(Duration::from_secs(2));
    }
}
