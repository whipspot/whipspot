//! The window host (D31, MD13): the winit loop, one `Gpu` and one `TextCache` shared by every
//! window, and per-window surface, renderer, UI state, translator, output state and clipboard.
//!
//! Shape follows luneterm's app and luneui's gallery: windows are created hidden and shown after
//! their first frame; a frame is acquire, `Translator::take`, the view's `images`, `Ui::begin`, view, `finish`, draw,
//! `OutputState::apply`, `Clipboard::service`, present. The loop waits (`ControlFlow::Wait`)
//! whenever nothing animates. The player's wake callback captures an `EventLoopProxy` only, never
//! a `PlayerHandle`.

mod dump;
mod fonts;
mod frame;
mod panel_view;
mod panels;
mod placement;
mod platform;
mod title;
mod tray;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use lune_ui::clipboard::Clipboard;
use lune_ui::frame::FrameRenderer;
use lune_ui::gpu::{self, Gpu};
use lune_ui::image::ImageStore;
use lune_ui::input::{OutputState, Translator, WindowCommand, control_flow};
use lune_ui::material::{self, Material};
use lune_ui::shortcuts::{CommandId, Shortcuts};
use lune_ui::surface::{Acquired, WindowSurface};
use lune_ui::text::TextCache;
use lune_ui::theme::Theme;
use lune_ui::tray::TrayEvent;
use lune_ui::ui::Ui;
use lune_ui::window::{glass_attributes, panel_attributes};
use whipspot_core::player::{PlayerAction, PlayerHandle};
use whipspot_core::skin::{PanelWindows, ScreenPoint};
use whipspot_ui::skin::{
    FrontChange, OPEN_FAILED, PanelPlan, SkinStatus, SkinUi, pixel_scale, trace,
};
use whipspot_ui::title::{WindowTitle, window_title};
use whipspot_ui::tray::{TrayMenu, TrayRequest};
use whipspot_ui::{Chrome, DIAGNOSTICS_KEY, DiagnosticsUi, NativeUi};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

pub use dump::{Dumper, dump};
pub use fonts::{MONO_FONT, UI_FONT, bundled_text_cache};
pub use panel_view::PanelView;
pub use tray::smoke as tray_smoke;

use frame::UiState;
use panels::{FOLLOW_TICK, HostPanelWindows, PanelSet, physical};
use title::TitleMarquee;
use tray::{TrayControl, route_tray_events};

/// `--smoke` must reach its frame count within this long, or the run fails.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often the host asks the skin front-end while a load is pending (P1).
const SKIN_POLL: Duration = Duration::from_millis(50);

/// How long the native window stays hidden at start while the saved skin loads.
const START_FALLBACK: Duration = Duration::from_secs(5);

/// The native window's frames each later phase of a smoke with [`HostOptions::switch_back`]
/// waits for.
const SMOKE_BACK_FRAMES: u64 = 30;

/// Where a smoke with a [`PanelCheck`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokePhase {
    /// Counting panel frames of the first set (the check runs when it is laid out).
    Panels,
    /// The native look was chosen: counting the native window's frames after the set closed.
    NativeBack,
    /// The skin was chosen again and its windows' opening forced to fail: counting the native
    /// window's frames once the status reads the fixed open failure.
    FailOpen,
}

/// The hint the main panel shows when a skin opens from the Settings card (D9 as amended).
fn native_hint() -> &'static str {
    if platform::chrome().drawn_caption {
        "Ctrl+Shift+N: native look"
    } else {
        "Cmd+Shift+N: native look"
    }
}

/// A deadline this close to now counts as reached when the loop wakes at a timer.
const DEADLINE_SLACK: Duration = Duration::from_millis(2);

/// The coordinate Windows reports for a minimised window's position: a window at or below it is
/// not saved (MD90).
const MINIMISED_MARK: i32 = -32000;

/// A surface for `window`. The first one also makes the device, on that surface so the device is
/// compatible with it; later windows make theirs on the device's own instance.
fn make_surface(
    shared: &mut Option<Shared>,
    event_loop: &ActiveEventLoop,
    window: &Arc<Window>,
    system_fonts: bool,
) -> Result<wgpu::Surface<'static>, String> {
    match shared {
        Some(shared) => shared
            .gpu
            .instance
            .create_surface(window.clone())
            .map_err(|e| format!("surface: {e}")),
        none => {
            let instance = gpu::instance(Some(Box::new(event_loop.owned_display_handle())));
            let raw = instance
                .create_surface(window.clone())
                .map_err(|e| format!("surface: {e}"))?;
            let gpu = pollster::block_on(Gpu::new(instance, Some(&raw)))
                .map_err(|e| format!("gpu: {e}"))?;
            *none = Some(Shared {
                gpu,
                text: fonts::text_cache(system_fonts),
            });
            Ok(raw)
        }
    }
}

/// What a window shows: one frame over the `Ui`, plus the two things the `Ui` has no channel for.
pub trait WindowView {
    /// Draws one frame.
    fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle);

    /// Registers the pictures the view has waiting with the host's image store. Called once per
    /// frame, after the surface is acquired and before [`WindowView::frame`], so what is
    /// registered shows in that frame (C4 (b)).
    fn images(&mut self, _store: &mut dyn ImageStore) {}

    /// The keyboard shortcut table of the window's toolkit (read once, when the window opens).
    fn shortcuts(&self) -> Shortcuts {
        Shortcuts::default()
    }

    /// The shortcut commands the last frame reported; `text_focused` tells that a text field had
    /// focus in it. Called right after the frame, before [`WindowView::take_commands`].
    fn fired(&mut self, _fired: &[CommandId], _text_focused: bool, _player: &PlayerHandle) {}

    /// Whether the window is maximised; the host tells before each frame.
    fn set_maximized(&mut self, _maximized: bool) {}

    /// The window operations the last frame asked for (drag, resize, minimise, close), in order.
    fn take_commands(&mut self) -> Vec<WindowCommand> {
        Vec::new()
    }

    /// The keys of the windows the user asked to open or close since the last call (a parked
    /// window with that key opens, an open one closes). Called after each frame.
    fn take_toggles(&mut self) -> Vec<&'static str> {
        Vec::new()
    }

    /// The toolkit theme for the window's next frame; `None` keeps the dark preset.
    fn theme(&self) -> Option<Theme> {
        None
    }

    /// The title the window should show now; `None` keeps the title it was opened with. The host
    /// asks after every frame and every player wake, and sets the OS title when it changes.
    fn title(&self, _player: &PlayerHandle) -> Option<WindowTitle> {
        None
    }
}

impl WindowView for NativeUi {
    fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        NativeUi::frame(self, ui, player);
    }

    fn images(&mut self, store: &mut dyn ImageStore) {
        NativeUi::images(self, store);
    }

    fn shortcuts(&self) -> Shortcuts {
        NativeUi::shortcuts()
    }

    fn fired(&mut self, fired: &[CommandId], text_focused: bool, player: &PlayerHandle) {
        NativeUi::fired(self, fired, text_focused, player);
    }

    fn set_maximized(&mut self, maximized: bool) {
        NativeUi::set_maximized(self, maximized);
    }

    fn take_commands(&mut self) -> Vec<WindowCommand> {
        NativeUi::take_commands(self)
    }

    fn take_toggles(&mut self) -> Vec<&'static str> {
        if NativeUi::take_toggle_diagnostics(self) {
            vec![DIAGNOSTICS_KEY]
        } else {
            Vec::new()
        }
    }

    fn theme(&self) -> Option<Theme> {
        Some(self.look_theme())
    }

    fn title(&self, player: &PlayerHandle) -> Option<WindowTitle> {
        Some(window_title(&player.state()))
    }
}

impl WindowView for DiagnosticsUi {
    fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        DiagnosticsUi::frame(self, ui, player);
    }

    fn shortcuts(&self) -> Shortcuts {
        DiagnosticsUi::shortcuts()
    }

    fn fired(&mut self, fired: &[CommandId], _text_focused: bool, _player: &PlayerHandle) {
        DiagnosticsUi::fired(self, fired);
    }

    fn set_maximized(&mut self, maximized: bool) {
        DiagnosticsUi::set_maximized(self, maximized);
    }

    fn take_commands(&mut self) -> Vec<WindowCommand> {
        DiagnosticsUi::take_commands(self)
    }

    fn take_toggles(&mut self) -> Vec<&'static str> {
        if DiagnosticsUi::take_toggle(self) {
            vec![DIAGNOSTICS_KEY]
        } else {
            Vec::new()
        }
    }
}

/// The OS folder dialog (D12), for the Music folders card: the chosen folder as text, `None`
/// when cancelled. Call it on the UI thread.
pub fn pick_folder() -> Option<String> {
    platform::pick_folder()
}

/// The chrome of this OS, for `NativeUi::new`.
pub fn chrome() -> Chrome {
    platform::chrome()
}

/// One window to open.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSpec {
    /// The name toggles use ([`WindowView::take_toggles`]); unique among the windows.
    pub key: &'static str,
    /// Whether the window opens at start. The first window always does; another with `false`
    /// starts parked until a toggle opens it.
    pub open_at_start: bool,
    pub title: String,
    /// First inner size, logical px.
    pub size: (f64, f64),
    /// Minimum inner size, logical px.
    pub min_size: (f64, f64),
    pub maximized: bool,
    /// The saved top-left corner, physical px. Applied only when it lies on a monitor
    /// ([`placement::visible`]); otherwise the OS places the window (MD90).
    pub position: Option<(i32, i32)>,
}

/// A check the host runs once on the first panel set, after it is laid out and shown (the demo's
/// `--smoke-skin`); its error ends the run as the run's error.
pub type PanelCheck = fn(&mut dyn PanelWindows) -> Result<(), String>;

/// The run's error when a smoke with a [`PanelCheck`] ended without the check having run.
const PANEL_CHECK_DID_NOT_RUN: &str = "smoke: the panel check did not run";

/// The run's error when the native window drew but the OS reports it hidden.
const SMOKE_NATIVE_HIDDEN: &str = "smoke: the native window is not visible again";

/// Whether `status` is the fixed failure of a skin whose windows could not be opened.
fn open_failed(status: &SkinStatus) -> bool {
    matches!(status, SkinStatus::Failed { text, .. } if *text == OPEN_FAILED)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HostOptions {
    /// Close after this many frames (forcing a redraw after each); fail if they are not reached
    /// within 20 s. `None`: run until the window is closed. With `panel_check` set, only frames
    /// of panel windows drawn after the first panel set opened count.
    pub smoke_frames: Option<u32>,
    /// Run once on the first panel set; the smoke fails if it never ran.
    pub panel_check: Option<PanelCheck>,
    /// With `panel_check`: after the panel frames, switch to the native look and require frames
    /// from the native window again, then choose the skin again with its windows' opening forced
    /// to fail and require the fixed failure status and frames from the native window (risk A3).
    pub switch_back: bool,
    /// Add the OS fallback font files (emoji, CJK) after the bundled faces.
    pub system_fonts: bool,
    /// Called once, on the UI thread, right after the first frame of any window was presented
    /// (the update lifecycle's health confirmation, D9). It must not block.
    pub first_frame: Option<fn()>,
}

/// What the loop leaves behind.
#[derive(Debug, Clone, PartialEq)]
pub struct HostReport {
    /// Frames drawn, all windows.
    pub frames: u64,
    /// The first window's last logical size while neither maximised nor minimised (its opening
    /// size when it never was): what the window restores to.
    pub size: Option<(f64, f64)>,
    /// Whether the first window was maximised at its last frame.
    pub maximized: bool,
    /// The first window's last top-left corner, physical px, while it was neither maximised nor
    /// minimised and the OS reported a real position; `None` when it never was.
    pub position: Option<(i32, i32)>,
}

/// The user events: the player changed (look again), or the tray icon was clicked.
#[derive(Debug, Clone, Copy)]
enum HostEvent {
    Wake,
    Tray(TrayEvent),
}

/// Everything built on the one device, shared by every window.
struct Shared {
    gpu: Gpu,
    text: TextCache,
}

/// What draws into a window.
enum Slot<'a> {
    /// A view the caller handed in, and the spec it was opened from (kept to park the view when
    /// the window closes).
    View {
        view: &'a mut (dyn WindowView + 'a),
        spec: WindowSpec,
    },
    /// Panel `i` of the running skin, drawn by the skin front-end.
    Panel(usize),
}

impl Slot<'_> {
    /// The title the window should show now (see [`WindowView::title`]); the first panel names
    /// the track like the native window.
    fn title(&self, player: &PlayerHandle) -> Option<WindowTitle> {
        match self {
            Slot::View { view, .. } => view.title(player),
            Slot::Panel(0) => Some(window_title(&player.state())),
            Slot::Panel(_) => None,
        }
    }

    fn key(&self) -> Option<&'static str> {
        match self {
            Slot::View { spec, .. } => Some(spec.key),
            Slot::Panel(_) => None,
        }
    }

    fn panel(&self) -> Option<usize> {
        match self {
            Slot::Panel(i) => Some(*i),
            Slot::View { .. } => None,
        }
    }
}

/// One window and what draws into it.
struct Win<'a> {
    window: Arc<Window>,
    surface: WindowSurface,
    frame: FrameRenderer,
    ui: UiState,
    translator: Translator,
    output: OutputState,
    clipboard: Clipboard,
    slot: Slot<'a>,
    /// Hidden by the front-end switch (the native window while a skin runs): never drawn, so its
    /// view does not drain the player's notices (V7).
    hidden: bool,
    /// The OS says the window is covered or off screen (`WindowEvent::Occluded`; only macOS
    /// sends it, so elsewhere this stays false).
    occluded: bool,
    /// What the window's last frame asked of the loop.
    flow: ControlFlow,
    /// What the OS title shows, and when it rotates (MD52).
    title: TitleMarquee,
    /// Whether the window is maximised, read from the OS when it opens and on each resize, never
    /// per frame: on macOS winit answers for a borderless window by swapping its style mask to
    /// titled and back, which rebuilds the window's frame view (most of a frame's time) and drops
    /// a mouse release that is under way. A skin panel is never maximised and never asks.
    maximized: bool,
}

impl Win<'_> {
    /// Feeds the window's title proposal to the marquee and sets the OS title if it changed.
    fn refresh_title(&mut self, player: &PlayerHandle, now: Instant) {
        let proposal = self.slot.title(player);
        self.set_title(proposal, now);
    }

    fn set_title(&mut self, proposal: Option<WindowTitle>, now: Instant) {
        if let Some(proposal) = proposal {
            self.title.set(proposal, now);
        }
        self.apply_title(now);
    }

    /// Sets the OS title if the marquee's text at `now` differs from the one shown.
    fn apply_title(&mut self, now: Instant) {
        if let Some(text) = self.title.poll(now) {
            self.window.set_title(&text);
        }
    }

    fn minimized(&self) -> bool {
        self.window.is_minimized() == Some(true)
    }

    /// What decides whether the window counts as shown (D6).
    fn visibility(&self) -> WinVis {
        WinVis {
            front_hidden: self.hidden,
            minimised: self.minimized(),
            os_visible: self.window.is_visible(),
            occluded: self.occluded,
        }
    }
}

/// What the host knows of one window when it decides whether the app is shown.
#[derive(Clone, Copy, Debug)]
struct WinVis {
    /// Hidden by the front-end switch.
    front_hidden: bool,
    minimised: bool,
    /// The OS's answer, `None` when it will not say.
    os_visible: Option<bool>,
    occluded: bool,
}

/// Whether any window of ours is shown: the tray menu while open, or a window that is not
/// front-end-hidden, minimised, reported invisible by the OS or occluded. Focus does not matter.
fn any_shown(wins: impl IntoIterator<Item = WinVis>, tray_open: bool) -> bool {
    tray_open
        || wins
            .into_iter()
            .any(|w| !w.front_hidden && !w.minimised && w.os_visible != Some(false) && !w.occluded)
}

/// A window that is not open: its spec and its view (kept, so reopening shows what it knew).
type Parked<'a> = (WindowSpec, &'a mut (dyn WindowView + 'a));

struct App<'p, 'a> {
    player: &'p PlayerHandle,
    opts: HostOptions,
    pending: Vec<Parked<'a>>,
    /// The windows opened later by key and closed again: never the first.
    parked: Vec<Parked<'a>>,
    /// The windows, first opened first.
    order: Vec<WindowId>,
    wins: HashMap<WindowId, Win<'a>>,
    shared: Option<Shared>,
    start: Instant,
    frames: u64,
    first_size: Option<(f64, f64)>,
    first_maximized: bool,
    first_position: Option<(i32, i32)>,
    error: Option<String>,
    /// The last `SetShown` value sent to the player (starts true: nothing is sent at start-up
    /// while visible).
    shown_sent: bool,
    /// The first panel set is open and laid out (the smoke with a panel check counts frames from
    /// here).
    set_ready: bool,
    /// Smoke frames counted so far (see [`HostOptions::smoke_frames`]).
    smoke_count: u64,
    /// The panel check ran.
    panel_checked: bool,
    /// The skin front-end; `None` runs the given views only.
    skin: Option<&'a mut SkinUi>,
    /// The panel windows of the running skin.
    set: Option<PanelSet>,
    /// When the host asks the front-end next while a load is pending (P1).
    next_poll: Instant,
    /// While the saved skin loads at start: when the hidden native window is shown anyway.
    fallback: Option<Instant>,
    /// The smoke's phase (only advanced with [`HostOptions::switch_back`]).
    phase: SmokePhase,
    /// The skin the smoke chooses again in [`SmokePhase::FailOpen`].
    smoke_skin: Option<String>,
    /// The tray menu waiting for the icon to be made (when the loop starts), and the icon once it
    /// is. `None` of both runs without a tray.
    tray_menu: Option<TrayMenu>,
    tray: Option<TrayControl>,
}

/// Opens `windows` (those with `open_at_start`, and always the first) and runs until the first is
/// closed (or the smoke's frames are drawn). A toggle by key opens a parked window or closes an
/// open one; closing any window but the first parks it.
///
/// With `skin`, the first window is the native look and the skin front-end can replace it: its
/// panel windows open when it reports a plan, the native window is hidden (never drawn) while
/// they are open, and closing the first panel ends the loop. A skin chosen before the loop
/// starts keeps the native window hidden until its panels open, it fails, or 5 s pass.
///
/// With `tray`, an icon is put in the tray / menu bar when the loop starts; its menu acts on the
/// player and the skin library, and Show and Quit are the host's (Quit leaves the loop as closing
/// the first window does). A tray that cannot be made leaves the app running without one.
pub fn run<'a>(
    player: &PlayerHandle,
    windows: Vec<(WindowSpec, &'a mut dyn WindowView)>,
    skin: Option<&'a mut SkinUi>,
    tray: Option<TrayMenu>,
    opts: HostOptions,
) -> anyhow::Result<HostReport> {
    let event_loop = EventLoop::<HostEvent>::with_user_event()
        .build()
        .map_err(|e| anyhow!("event loop: {e}"))?;
    // The wake closure holds the proxy and nothing else (F4): never a handle on the player.
    let proxy = event_loop.create_proxy();
    if tray.is_some() {
        // Before the first `Tray::new` (luneui keeps only the first handler).
        route_tray_events(proxy.clone());
    }
    player.set_wake(Arc::new(move || {
        let _ = proxy.send_event(HostEvent::Wake);
    }));
    let first_spec = windows.first().map(|(spec, _)| (spec.size, spec.maximized));
    let mut app = App {
        player,
        opts,
        pending: windows,
        parked: Vec::new(),
        order: Vec::new(),
        wins: HashMap::new(),
        shared: None,
        start: Instant::now(),
        frames: 0,
        // Seeded from the spec: a window opened maximised and never restored reports that size.
        first_size: first_spec.map(|(size, _)| size),
        first_maximized: first_spec.is_some_and(|(_, maximized)| maximized),
        first_position: None,
        error: None,
        shown_sent: true,
        set_ready: false,
        smoke_count: 0,
        panel_checked: false,
        skin,
        set: None,
        next_poll: Instant::now(),
        fallback: None,
        phase: SmokePhase::Panels,
        smoke_skin: None,
        tray_menu: tray,
        tray: None,
    };
    let result = event_loop.run_app(&mut app);
    if let Some(e) = app.error.take() {
        return Err(anyhow!(e));
    }
    result.map_err(|e| anyhow!("event loop: {e}"))?;
    Ok(HostReport {
        frames: app.frames,
        size: app.first_size,
        maximized: app.first_maximized,
        position: app.first_position,
    })
}

/// The earlier of two control flows: a deadline beats waiting; `Poll` beats both.
fn merge_flow(a: ControlFlow, b: ControlFlow) -> ControlFlow {
    match (a, b) {
        (ControlFlow::Poll, _) | (_, ControlFlow::Poll) => ControlFlow::Poll,
        (ControlFlow::WaitUntil(x), ControlFlow::WaitUntil(y)) => ControlFlow::WaitUntil(x.min(y)),
        (ControlFlow::WaitUntil(t), ControlFlow::Wait)
        | (ControlFlow::Wait, ControlFlow::WaitUntil(t)) => ControlFlow::WaitUntil(t),
        (ControlFlow::Wait, ControlFlow::Wait) => ControlFlow::Wait,
    }
}

impl<'p, 'a> App<'p, 'a> {
    /// Stops the loop with `message` as the run's error.
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.error.get_or_insert(message);
        event_loop.exit();
    }

    /// Asks every window that is shown and not minimised to redraw (a minimised one redraws on
    /// restore; a front-end-hidden one never draws).
    fn request_redraws(&self) {
        for win in self.wins.values().filter(|w| !w.minimized() && !w.hidden) {
            win.window.request_redraw();
        }
    }

    /// Whether the host's poll timer runs: a skin load is pending, or the start-up fallback is
    /// armed (P1).
    fn polling(&self) -> bool {
        self.fallback.is_some() || self.skin.as_deref().is_some_and(SkinUi::pending)
    }

    /// Checks the buttons held on the skin panels against the OS's own button state (macOS): a
    /// button the OS says is up gets the release the window never got, through the panel's
    /// translator as a real one would (so a skin button acts and a list sees its release), and a
    /// panel left with no button down ends its drag or resize. Nothing when the OS cannot say.
    fn os_buttons_up(&mut self) {
        let Some(set) = self.set.as_mut() else {
            return;
        };
        if !set.buttons.any_panel() {
            return;
        }
        let os = platform::mouse_buttons_down();
        let ids = set.ids();
        for (i, id) in ids.into_iter().enumerate() {
            let released = set.buttons.os_released(i, os);
            if released.is_empty() {
                continue;
            }
            let mut all_up = false;
            for &button in &released {
                all_up |= set.buttons.set(i, button, false);
                if trace::enabled() {
                    trace::line(format_args!(
                        "os-release panel={i} button={}",
                        trace_button(button)
                    ));
                }
            }
            if let Some(win) = self.wins.get_mut(&id) {
                for &button in &released {
                    win.translator.mouse_input(ElementState::Released, button);
                }
                if !win.hidden {
                    win.window.request_redraw();
                }
            }
            if all_up && let Some(skin) = self.skin.as_deref_mut() {
                skin.buttons_up(i);
            }
        }
    }

    /// The loop's control flow: the earliest ask of any window, the marquee's next rotation,
    /// the smoke's deadline, the skin poll and the panels' pointer followers.
    fn set_flow(&self, event_loop: &ActiveEventLoop) {
        let mut flow = self.wins.values().fold(ControlFlow::Wait, |acc, w| {
            let acc = merge_flow(acc, w.flow);
            match w.title.due() {
                Some(due) => merge_flow(acc, ControlFlow::WaitUntil(due)),
                None => acc,
            }
        });
        if self.opts.smoke_frames.is_some() {
            flow = merge_flow(flow, ControlFlow::WaitUntil(self.start + SMOKE_TIMEOUT));
        }
        if self.polling() {
            flow = merge_flow(flow, ControlFlow::WaitUntil(self.next_poll));
        }
        if let Some(set) = self.set.as_ref().filter(|s| s.ticks(platform::OS_BUTTONS)) {
            flow = merge_flow(flow, ControlFlow::WaitUntil(set.next_tick));
        }
        if let Some(tray) = &self.tray {
            flow = merge_flow(flow, tray.flow());
        }
        event_loop.set_control_flow(flow);
    }

    /// Opens one glass window for a view. The first one also makes the device, on a surface it
    /// can present to.
    fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: WindowSpec,
        view: &'a mut (dyn WindowView + 'a),
    ) -> Result<WindowId, String> {
        let mut attrs = glass_attributes(&spec.title, spec.size, spec.min_size, false)
            .with_maximized(spec.maximized);
        if let Some((x, y)) = spec.position {
            let monitors: Vec<(i32, i32, u32, u32)> = event_loop
                .available_monitors()
                .map(|m| {
                    let (at, size) = (m.position(), m.size());
                    (at.x, at.y, size.width, size.height)
                })
                .collect();
            if placement::visible((x, y), &monitors) {
                attrs = attrs.with_position(PhysicalPosition::new(x, y));
            }
        }
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("window: {e}"))?,
        );
        if material::apply(&window, Material::Vibrancy).is_err() {
            log::warn!("window material unavailable; using the opaque fallback");
            if material::apply(&window, Material::Opaque).is_err() {
                log::warn!("the opaque window material failed");
            }
        }
        let shortcuts = view.shortcuts();
        let title = spec.title.clone();
        self.attach(
            event_loop,
            window,
            Slot::View { view, spec },
            shortcuts,
            &title,
        )
    }

    /// Makes the surface, renderer and UI state of `window` and adds it to the windows. The
    /// first window's surface is the one the device is made compatible with; later windows make
    /// theirs on the device's own instance.
    fn attach(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: Arc<Window>,
        slot: Slot<'a>,
        shortcuts: Shortcuts,
        title: &str,
    ) -> Result<WindowId, String> {
        let size = window.inner_size();
        let raw = make_surface(
            &mut self.shared,
            event_loop,
            &window,
            self.opts.system_fonts,
        )?;
        let Some(shared) = &mut self.shared else {
            return Err("gpu: not initialised".into());
        };
        let surface = WindowSurface::new(&shared.gpu, raw, (size.width, size.height))?;
        let frame = FrameRenderer::new(&mut shared.gpu, surface.format());
        let id = window.id();
        let maximized = slot.panel().is_none() && window.is_maximized();
        self.wins.insert(
            id,
            Win {
                maximized,
                translator: Translator::new(size, window.scale_factor()),
                output: OutputState::new(),
                clipboard: Clipboard::new(),
                ui: UiState::new(shortcuts),
                flow: ControlFlow::Wait,
                title: TitleMarquee::new(platform::MARQUEE, title, Instant::now()),
                hidden: false,
                occluded: false,
                slot,
                window,
                surface,
                frame,
            },
        );
        self.order.push(id);
        Ok(id)
    }

    /// Draws one frame of window `id`. Returns whether a frame was presented.
    fn render(&mut self, event_loop: &ActiveEventLoop, id: WindowId) -> Result<bool, String> {
        let Some(shared) = &mut self.shared else {
            return Ok(false);
        };
        let Some(win) = self.wins.get_mut(&id) else {
            return Ok(false);
        };
        // Before `acquire`: a front-end-hidden window is never drawn (V7).
        if win.hidden {
            win.flow = ControlFlow::Wait;
            return Ok(false);
        }
        if shared.gpu.is_lost() {
            return Err("the GPU device was lost".into());
        }
        let panel = win.slot.panel();
        let mut panel_view;
        let view: &mut dyn WindowView = match &mut win.slot {
            Slot::View { view, .. } => &mut **view,
            Slot::Panel(i) => {
                let Some(skin) = self.skin.as_deref_mut() else {
                    return Ok(false);
                };
                panel_view = PanelView::new(skin, *i);
                &mut panel_view
            }
        };
        let frame = match win.surface.acquire(&shared.gpu)? {
            Acquired::Frame(frame) => frame,
            // Hidden or minimised: nothing to draw, and the platform redraws when it can.
            Acquired::Skip => {
                win.flow = ControlFlow::Wait;
                return Ok(false);
            }
            Acquired::Redraw => {
                win.window.request_redraw();
                return Ok(false);
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = win.surface.size();
        let scale = win.window.scale_factor();
        let maximized = win.maximized;
        view.set_maximized(maximized);
        let input = win.translator.take(self.start.elapsed());
        view.images(&mut shared.gpu);
        let (scene, out) = win
            .ui
            .run(&input, &mut shared.text, &mut *view, self.player);
        let close = win.output.apply(&win.window, &out, scale as f32);
        let toggles = view.take_toggles();
        let proposal = view.title(self.player);
        // A write goes to the OS now; a read is answered with the next frame's input.
        win.clipboard.service(&out, &mut win.translator);
        win.frame
            .draw(
                &mut shared.gpu,
                &mut shared.text,
                &scene,
                &target,
                frame::params(size, scale as f32),
            )
            .map_err(|e| format!("render: {e}"))?;
        win.window.pre_present_notify();
        win.surface.present(&shared.gpu, frame);
        shared.gpu.end_frame(&mut shared.text);
        let (flow, redraw) = control_flow(out.repaint, self.start);
        // A minimised window schedules nothing: it redraws on restore.
        let minimized = win.minimized();
        win.flow = if minimized { ControlFlow::Wait } else { flow };
        win.set_title(proposal, Instant::now());
        let read_asked = out
            .clipboard
            .iter()
            .any(|r| matches!(r, lune_ui::input::ClipboardRequest::Read));
        if (redraw || read_asked) && !minimized {
            win.window.request_redraw();
        }
        if self.order.first() == Some(&id) {
            // A maximised window's size is the screen's: keep the last restored one.
            if !maximized {
                let logical = win.window.inner_size().to_logical::<f64>(scale);
                self.first_size = Some((logical.width, logical.height));
            }
            // Windows reports -32000 for a minimised window: that is not a place to save (MD90).
            if !maximized
                && !win.minimized()
                && let Ok(at) = win.window.outer_position()
                && at.x > MINIMISED_MARK
                && at.y > MINIMISED_MARK
            {
                self.first_position = Some((at.x, at.y));
            }
            self.first_maximized = maximized;
        }
        self.frames += 1;
        if self.frames == 1
            && let Some(hook) = self.opts.first_frame
        {
            hook();
        }
        // With a panel check, only panel windows of an open set count (the native window's
        // frames before the set exists must not end the run); in the later phases only the
        // native window's frames with no set open, after the failure when one is forced.
        let counts = match self.phase {
            _ if self.opts.panel_check.is_none() => true,
            SmokePhase::Panels => panel.is_some() && self.set_ready,
            SmokePhase::NativeBack => panel.is_none() && self.set.is_none(),
            SmokePhase::FailOpen => {
                panel.is_none()
                    && self.set.is_none()
                    && self
                        .skin
                        .as_deref()
                        .is_some_and(|s| open_failed(&s.library().status()))
            }
        };
        if counts {
            self.smoke_count += 1;
        }
        let native_visible = panel.is_none() && win.window.is_visible() != Some(false);
        let window = win.window.clone();
        if self.opts.smoke_frames.is_some() {
            if self.smoke_count >= self.smoke_goal() {
                self.smoke_phase_done(event_loop, native_visible);
            } else {
                window.request_redraw();
            }
        }
        if close {
            match panel {
                Some(i) => self.panel_close(event_loop, i),
                None => self.close_window(event_loop, id),
            }
        }
        for key in toggles {
            self.toggle(event_loop, key);
        }
        Ok(true)
    }

    /// Closes window `id`: the first window ends the loop, any other is parked with its view.
    /// Panel windows have their own path ([`App::panel_close`]).
    fn close_window(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        if self.order.first() == Some(&id) {
            event_loop.exit();
            return;
        }
        if self.wins.get(&id).is_some_and(|w| w.slot.panel().is_some()) {
            return;
        }
        self.order.retain(|w| *w != id);
        if let Some(win) = self.wins.remove(&id)
            && let Slot::View { spec, view } = win.slot
        {
            self.parked.push((spec, view));
        }
        self.set_flow(event_loop);
    }

    /// Panel `i` was asked to close: the first ends the loop, another is hidden by the front-end.
    fn panel_close(&mut self, event_loop: &ActiveEventLoop, i: usize) {
        if self
            .skin
            .as_deref_mut()
            .is_some_and(|s| s.close_requested(i))
        {
            event_loop.exit();
        }
    }

    /// Opens the parked window `key`, or closes (parks) the open one; an unknown key does nothing.
    fn toggle(&mut self, event_loop: &ActiveEventLoop, key: &str) {
        let open = self
            .wins
            .iter()
            .find(|(_, w)| w.slot.key() == Some(key))
            .map(|(id, _)| *id);
        if let Some(id) = open {
            // The first window is not a toggle's to close.
            if self.order.first() != Some(&id) {
                self.close_window(event_loop, id);
            }
        } else if let Some(at) = self.parked.iter().position(|(s, _)| s.key == key) {
            let (spec, view) = self.parked.remove(at);
            self.show(event_loop, spec, view, false);
        }
    }

    /// Opens a window and draws its first frame; shows it unless `hidden` (then it stays hidden
    /// by the front-end switch and is not drawn again until shown).
    fn show(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: WindowSpec,
        view: &'a mut (dyn WindowView + 'a),
        hidden: bool,
    ) {
        let id = match self.open(event_loop, spec, view) {
            Ok(id) => id,
            Err(e) => return self.fail(event_loop, e),
        };
        // The first frame is drawn here, not on a redraw event a hidden window may never get;
        // the window is shown right after it.
        if let Err(e) = self.render(event_loop, id) {
            return self.fail(event_loop, e);
        }
        if let Some(win) = self.wins.get_mut(&id) {
            if hidden {
                win.hidden = true;
            } else {
                win.window.set_visible(true);
                win.window.request_redraw();
            }
        }
    }

    /// The native window (the first one), when it is open.
    fn native(&mut self) -> Option<&mut Win<'a>> {
        let id = *self.order.first()?;
        self.wins.get_mut(&id)
    }

    fn native_hidden(&self) -> bool {
        self.order
            .first()
            .and_then(|id| self.wins.get(id))
            .is_some_and(|w| w.hidden)
    }

    /// Hides the native window while a skin runs; it is not drawn until shown again.
    fn hide_native(&mut self) {
        if let Some(win) = self.native()
            && !win.hidden
        {
            win.hidden = true;
            win.flow = ControlFlow::Wait;
            win.window.set_visible(false);
        }
    }

    /// Shows the native window again and redraws it. The start-up fallback has nothing left to
    /// do once the native window is up: it is disarmed here, on every path that shows it.
    fn show_native(&mut self) {
        self.fallback = None;
        if let Some(win) = self.native()
            && win.hidden
        {
            win.hidden = false;
            win.window.set_visible(true);
            win.window.request_redraw();
        }
    }

    /// Releases the images of a skin that is gone (D8): the front-end queued them.
    fn release_skin_images(&mut self) {
        if let (Some(skin), Some(shared)) = (self.skin.as_deref_mut(), &mut self.shared) {
            skin.images(&mut shared.gpu);
        }
    }

    /// Asks the skin front-end what changed and acts on it: quit, a new panel set, the native
    /// look back, the start-up fallback; then runs the window operations its frames queued.
    /// Runs after every frame, on every wake and on the poll timer.
    fn poll_front(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.next_poll = now + SKIN_POLL;
        let native_shown = !self.native_hidden();
        let Some(skin) = self.skin.as_deref_mut() else {
            return;
        };
        if skin.take_quit() {
            event_loop.exit();
            return;
        }
        match skin.take_change() {
            Some(FrontChange::Panels(plan)) => {
                // Shown from the Settings card (the native window was up): the hint (D9).
                if native_shown {
                    skin.hint(native_hint());
                }
                self.close_set();
                if self.open_set(event_loop, &plan).is_ok() {
                    self.fallback = None;
                    self.panel_set_ready(event_loop);
                } else {
                    log::warn!("the skin's windows could not be opened");
                    self.close_set();
                    if let Some(skin) = self.skin.as_deref_mut() {
                        skin.open_failed();
                    }
                    self.release_skin_images();
                    self.show_native();
                }
            }
            Some(FrontChange::Native) => {
                self.close_set();
                self.release_skin_images();
                self.show_native();
            }
            None => {}
        }
        // The native window waits hidden at start only while the saved skin is on its way, and
        // at most START_FALLBACK.
        if self.set.is_none() && self.native_hidden() {
            let starting = self.skin.as_deref().is_some_and(SkinUi::starting);
            if !starting || self.fallback.is_some_and(|t| now >= t) {
                self.show_native();
            }
        }
        if let (Some(skin), Some(set)) = (self.skin.as_deref_mut(), self.set.as_mut()) {
            skin.drive(&mut HostPanelWindows::new(set));
        }
    }

    /// The attributes of panel window `spec` at `k` physical px per skin px and the OS `scale`:
    /// a panel window (no decorations, transparent, fixed size, no material) of exactly
    /// `size * k` physical px, hidden.
    fn panel_attrs(title: &str, size: (u32, u32), k: u32, scale: f64) -> WindowAttributes {
        let logical = LogicalSize::new(
            f64::from(size.0) * f64::from(k) / scale,
            f64::from(size.1) * f64::from(k) / scale,
        );
        panel_attributes(title, (logical.width, logical.height), false)
            .with_inner_size(physical(size, k))
    }

    /// Opens the panel windows of `plan`: the leader where the native window is, the members
    /// owned by it, each `size * k` physical px with its shape; draws the first frame of each,
    /// hides the native window, lays the panels out and shows the visible ones. On `Err` the
    /// caller closes what opened.
    fn open_set(&mut self, event_loop: &ActiveEventLoop, plan: &PanelPlan) -> Result<(), String> {
        let first = plan.panels.first().ok_or("skin: no panels")?;
        let native = self
            .order
            .first()
            .and_then(|id| self.wins.get(id))
            .map(|w| w.window.clone())
            .ok_or("skin: no native window")?;
        let scale = native.scale_factor();
        let mut k = pixel_scale(scale);
        let mut attrs = Self::panel_attrs(&first.title, first.size, k, scale);
        if let Ok(at) = native.outer_position() {
            attrs = attrs.with_position(at);
        }
        let leader = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|_| "skin: panel window".to_owned())?,
        );
        // The leader may have opened on a monitor of another scale: its k is the set's.
        let leader_k = pixel_scale(leader.scale_factor());
        if leader_k != k {
            k = leader_k;
            let _ = leader.request_inner_size(physical(first.size, k));
        }
        let shortcuts = match self.skin.as_deref_mut() {
            Some(skin) => {
                skin.set_pixel_scale(k);
                skin.shortcuts()
            }
            None => return Err("skin: no front-end".into()),
        };
        let mut set = PanelSet::new(plan.generation, k, plan.panels.clone(), leader.clone());
        let scale = leader.scale_factor();
        let mut members = Vec::new();
        for spec in plan.panels.iter().skip(1) {
            let group = set.group().ok_or("skin: panel group")?;
            let attrs =
                group.member_attributes(Self::panel_attrs(&spec.title, spec.size, k, scale));
            let member = Arc::new(
                event_loop
                    .create_window(attrs)
                    .map_err(|_| "skin: panel window".to_owned())?,
            );
            members.push(member.clone());
            set.add(member)?;
        }
        set.reshape_all();
        let ids = set.ids();
        let windows: Vec<Arc<Window>> = std::iter::once(leader).chain(members).collect();
        // The set is in place before any window is attached, so a failure below closes it all.
        self.set = Some(set);
        for (i, (window, spec)) in windows.into_iter().zip(&plan.panels).enumerate() {
            self.attach(
                event_loop,
                window,
                Slot::Panel(i),
                shortcuts.clone(),
                &spec.title,
            )?;
        }
        // The first frame of each, still hidden.
        for &id in &ids {
            self.render(event_loop, id)?;
        }
        self.hide_native();
        // The smoke's forced failure: after the native window was hidden, so showing it again
        // on the failure path is what the smoke sees (risk A3).
        if self.opts.switch_back && self.phase == SmokePhase::FailOpen {
            return Err("smoke: forced open failure".into());
        }
        let (Some(skin), Some(set)) = (self.skin.as_deref_mut(), self.set.as_mut()) else {
            return Err("skin: panel set".into());
        };
        let mut windows = HostPanelWindows::new(set);
        let leader_at = windows
            .frame(0)
            .map(|f| ScreenPoint { x: f.x, y: f.y })
            .ok_or("skin: no leader frame")?;
        let origins =
            skin.start_layout(leader_at, windows.units_per_pixel(), &windows.work_areas());
        let moves: Vec<(usize, ScreenPoint)> = origins.into_iter().enumerate().collect();
        // A refused move leaves the panels where they opened: still usable.
        let _ = windows.move_panels(&moves);
        // The members first, the leader last, so it ends in front with the focus.
        for (i, spec) in plan.panels.iter().enumerate().rev() {
            if spec.visible || i == 0 {
                windows.set_visible(i, true);
            }
        }
        skin.drive(&mut windows);
        Ok(())
    }

    /// The first panel set is laid out and shown: the smoke's frames count from now, and the
    /// panel check (if any) runs once on the real windows; its error ends the run.
    fn panel_set_ready(&mut self, event_loop: &ActiveEventLoop) {
        self.set_ready = true;
        let (Some(check), false) = (self.opts.panel_check, self.panel_checked) else {
            return;
        };
        let Some(set) = self.set.as_mut() else {
            return;
        };
        self.panel_checked = true;
        if let Err(message) = check(&mut HostPanelWindows::new(set)) {
            self.fail(event_loop, message);
        }
    }

    /// Closes the open panel set: the host's per-window state of the members, then the
    /// leader's, then the windows themselves (the group's members before its leader). Nothing of
    /// the old generation outlives this.
    fn close_set(&mut self) {
        let Some(set) = self.set.take() else {
            return;
        };
        for id in set.ids().into_iter().rev() {
            self.order.retain(|w| *w != id);
            self.wins.remove(&id);
        }
        set.close();
    }

    /// The leader's scale changed: a new k for the whole set, every panel resized to
    /// `size * k`, reshaped, and the front-end told (it rescales the docked places). A member's
    /// own scale change keeps the set's k: its window keeps `size * k`.
    fn panel_scale_changed(
        &mut self,
        panel: usize,
        scale: f64,
        writer: &mut winit::event::InnerSizeWriter,
    ) {
        let Some(set) = self.set.as_mut() else {
            return;
        };
        if panel == 0 {
            let k = pixel_scale(scale);
            let (upp_before, upp_after) = set.leader_rescaled();
            if trace::enabled() {
                trace::line(format_args!(
                    "scale panel=0 factor={scale} k={}->{k} upp={upp_before}->{upp_after}",
                    set.k
                ));
            }
            if k != set.k {
                set.k = k;
                for i in 1..set.len() {
                    set.resize(i);
                }
                set.reshape_all();
                if let Some(skin) = self.skin.as_deref_mut() {
                    skin.rescaled(k, upp_before);
                }
            }
        } else {
            set.reshape(panel);
        }
        if let Some(size) = set.size_of(panel) {
            let _ = writer.request_inner_size(size);
        }
    }

    /// Puts the icon in the tray (the loop has started, so the first window and the device exist).
    fn start_tray(&mut self) {
        let Some(menu) = self.tray_menu.take() else {
            return;
        };
        match TrayControl::new(menu, self.player) {
            Ok(tray) => self.tray = Some(tray),
            // Fixed text, never the OS's.
            Err(_) => log::warn!("the tray icon could not be made"),
        }
    }

    /// Acts on what the tray menu asked for: show the app, or leave the loop.
    fn tray_request(&mut self, event_loop: &ActiveEventLoop) {
        match self.tray.as_mut().and_then(TrayControl::take_request) {
            Some(TrayRequest::Show) => self.show_app(),
            Some(TrayRequest::Quit) => event_loop.exit(),
            None => {}
        }
    }

    /// Brings the app to the front: the panel group's leader with a skin, else the native window
    /// (restored from a minimise and focused).
    fn show_app(&mut self) {
        if let Some(set) = self.set.as_mut() {
            set.show();
            return;
        }
        if self.native_hidden() {
            self.show_native();
        }
        if let Some(win) = self.native() {
            win.window.set_minimized(false);
            win.window.set_visible(true);
            win.window.focus_window();
        }
    }

    /// The frames the smoke's current phase waits for.
    fn smoke_goal(&self) -> u64 {
        match (self.opts.smoke_frames, self.phase) {
            (None, _) => u64::MAX,
            (Some(n), SmokePhase::Panels) => u64::from(n),
            (Some(_), SmokePhase::NativeBack | SmokePhase::FailOpen) => SMOKE_BACK_FRAMES,
        }
    }

    /// The smoke's current phase has its frames: end the run, or start the next phase
    /// ([`HostOptions::switch_back`]). `native_visible`: the frame was the native window's and
    /// the OS does not report it hidden.
    fn smoke_phase_done(&mut self, event_loop: &ActiveEventLoop, native_visible: bool) {
        if self.opts.panel_check.is_some() && !self.panel_checked {
            self.error.get_or_insert(PANEL_CHECK_DID_NOT_RUN.to_owned());
            event_loop.exit();
            return;
        }
        if !self.opts.switch_back || self.opts.panel_check.is_none() {
            event_loop.exit();
            return;
        }
        let Some(library) = self.skin.as_deref().map(|s| s.library().clone()) else {
            return self.fail(event_loop, "smoke: no skin front-end".to_owned());
        };
        match self.phase {
            SmokePhase::Panels => {
                let SkinStatus::Active { name } = library.status() else {
                    return self.fail(event_loop, "smoke: no skin is active".to_owned());
                };
                self.smoke_skin = Some(name);
                self.phase = SmokePhase::NativeBack;
                self.smoke_count = 0;
                library.choose_native();
            }
            SmokePhase::NativeBack => {
                if !native_visible {
                    return self.fail(event_loop, SMOKE_NATIVE_HIDDEN.to_owned());
                }
                let Some(name) = self.smoke_skin.take() else {
                    return self.fail(event_loop, "smoke: no skin to choose again".to_owned());
                };
                self.phase = SmokePhase::FailOpen;
                self.smoke_count = 0;
                library.choose(&name);
            }
            SmokePhase::FailOpen => {
                if !native_visible {
                    return self.fail(event_loop, SMOKE_NATIVE_HIDDEN.to_owned());
                }
                event_loop.exit();
            }
        }
    }

    /// An event of the tray menu's window: a redraw draws it (and a pick it made is acted on),
    /// anything else goes to its input.
    fn tray_window_event(&mut self, event_loop: &ActiveEventLoop, event: &WindowEvent) {
        let (Some(tray), Some(shared)) = (self.tray.as_mut(), self.shared.as_mut()) else {
            return;
        };
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Err(e) = tray.render(shared, self.player, self.start) {
                return self.fail(event_loop, e);
            }
            self.tray_request(event_loop);
            self.set_flow(event_loop);
        } else {
            tray.window_event(event, self.start.elapsed());
        }
    }

    /// Fails the smoke when its frames did not come in time.
    fn check_smoke_deadline(&mut self, event_loop: &ActiveEventLoop) {
        if self.opts.smoke_frames.is_some()
            && !event_loop.exiting()
            && self.start.elapsed() >= SMOKE_TIMEOUT
        {
            let message = match self.phase {
                _ if self.opts.panel_check.is_some() && !self.panel_checked => {
                    PANEL_CHECK_DID_NOT_RUN.to_owned()
                }
                SmokePhase::NativeBack => "smoke: the native window did not come back".to_owned(),
                SmokePhase::FailOpen => {
                    "smoke: the native window did not come back after a failed open".to_owned()
                }
                SmokePhase::Panels => format!(
                    "smoke: {} of {} frames in {} s",
                    self.smoke_count,
                    self.smoke_goal(),
                    SMOKE_TIMEOUT.as_secs()
                ),
            };
            self.fail(event_loop, message);
        }
    }
}

impl<'p, 'a> ApplicationHandler<HostEvent> for App<'p, 'a> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // A skin chosen before the loop: the native window draws its first frame but waits
        // hidden until the panels open, the load fails, or the fallback passes.
        let start_hidden = self.skin.as_deref().is_some_and(SkinUi::starting);
        for (i, (spec, view)) in std::mem::take(&mut self.pending).into_iter().enumerate() {
            if i > 0 && !spec.open_at_start {
                self.parked.push((spec, view));
            } else {
                self.show(event_loop, spec, view, i == 0 && start_hidden);
            }
            if self.error.is_some() {
                return;
            }
        }
        if start_hidden {
            self.fallback = Some(Instant::now() + START_FALLBACK);
        }
        self.start_tray();
        self.poll_front(event_loop);
        self.set_flow(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: HostEvent) {
        match event {
            HostEvent::Wake => {
                self.request_redraws();
                // The title follows the player even while the window is minimised.
                let now = Instant::now();
                for win in self.wins.values_mut() {
                    win.refresh_title(self.player, now);
                }
                if let Some(tray) = &mut self.tray {
                    tray.wake(self.player);
                }
                self.poll_front(event_loop);
                self.set_flow(event_loop);
            }
            HostEvent::Tray(click) => {
                if let Some(tray) = &mut self.tray
                    && tray
                        .click(
                            event_loop,
                            &mut self.shared,
                            self.opts.system_fonts,
                            click,
                            self.player,
                            self.start.elapsed(),
                        )
                        .is_err()
                {
                    // The menu could not open: the app goes on without it.
                    log::warn!("the tray menu could not be opened");
                }
                self.set_flow(event_loop);
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // Apply the published power mode before this wake's events are handled, so the first frame
        // after a restore does not run marked (I5).
        crate::power::sync_current_thread();
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            let now = Instant::now();
            for win in self.wins.values_mut() {
                // A `Repaint::At` request whose time has come: redraw that window only (the
                // marquee's tick must not redraw the others).
                if matches!(win.flow, ControlFlow::WaitUntil(t) if t <= now + DEADLINE_SLACK) {
                    win.flow = ControlFlow::Wait;
                    if !win.minimized() && !win.hidden {
                        win.window.request_redraw();
                    }
                }
                // The marquee's tick: no redraw.
                win.apply_title(now);
            }
            // The skin poll (P1): no window is redrawn for it.
            if self.polling() && self.next_poll <= now + DEADLINE_SLACK {
                self.poll_front(event_loop);
            }
            let due = self.set.as_ref().is_some_and(|s| {
                s.ticks(platform::OS_BUTTONS) && s.next_tick <= now + DEADLINE_SLACK
            });
            if due {
                // A release the OS saw but the window never got first, so a panel freed by it
                // follows the pointer again on this same tick.
                self.os_buttons_up();
                if let Some(set) = self.set.as_mut() {
                    if set.follows() {
                        set.follow_tick();
                    }
                    set.next_tick = Instant::now() + FOLLOW_TICK;
                }
            }
            if let Some(tray) = &mut self.tray {
                tray.due(now);
            }
            self.set_flow(event_loop);
        }
        self.check_smoke_deadline(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The main thread follows the power mode like the runtime's workers do (I5).
        crate::power::sync_current_thread();
        if event_loop.exiting() {
            return;
        }
        let tray_open = self.tray.as_ref().is_some_and(TrayControl::is_visible);
        let shown = any_shown(self.wins.values().map(Win::visibility), tray_open);
        if shown != self.shown_sent {
            self.shown_sent = shown;
            self.player.dispatch(PlayerAction::SetShown(shown));
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, mut event: WindowEvent) {
        // After `exit()` winit still delivers queued events; ignore them so no frame is acquired.
        if event_loop.exiting() {
            return;
        }
        if self.tray.as_ref().and_then(TrayControl::window_id) == Some(id) {
            self.tray_window_event(event_loop, &event);
            return;
        }
        let panel = self.wins.get(&id).and_then(|w| w.slot.panel());
        match &mut event {
            WindowEvent::CloseRequested => {
                match panel {
                    Some(i) => self.panel_close(event_loop, i),
                    None => self.close_window(event_loop, id),
                }
                return;
            }
            WindowEvent::MouseInput { state, button, .. } if panel.is_some() => {
                let (i, pressed) = (panel.unwrap_or_default(), *state == ElementState::Pressed);
                let all_up = self
                    .set
                    .as_mut()
                    .is_some_and(|set| set.buttons.set(i, *button, pressed));
                // A drag or resize the panel holds ends here even when the toolkit's release
                // never comes (the skin then ignores it).
                if all_up && let Some(skin) = self.skin.as_deref_mut() {
                    skin.buttons_up(i);
                }
                if trace::enabled()
                    && let Some(win) = self.wins.get(&id)
                {
                    trace::line(format_args!(
                        "button panel={i} pressed={pressed} factor={}",
                        win.window.scale_factor()
                    ));
                }
            }
            WindowEvent::Focused(focused) if panel.is_some() => {
                let (i, focused) = (panel.unwrap_or_default(), *focused);
                if !focused
                    && self.set.as_mut().is_some_and(|set| set.buttons.clear(i))
                    && let Some(skin) = self.skin.as_deref_mut()
                {
                    skin.buttons_up(i);
                }
                if let Some(skin) = self.skin.as_deref_mut() {
                    skin.focused(i, focused);
                }
                if focused && let Some(set) = self.set.as_mut() {
                    set.focused(id);
                }
                if focused {
                    self.player.dispatch(PlayerAction::CheckLibrary);
                }
            }
            // Any other Whipspot window (main, diagnostics): a library look, as the panels do.
            WindowEvent::Focused(true) => {
                self.player.dispatch(PlayerAction::CheckLibrary);
            }
            WindowEvent::Occluded(occluded) => {
                if let Some(win) = self.wins.get_mut(&id) {
                    win.occluded = *occluded;
                }
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer,
            } if panel.is_some() => {
                let scale = *scale_factor;
                self.panel_scale_changed(panel.unwrap_or_default(), scale, inner_size_writer);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                // The media keys: luneui has no key for them, and they need no widget.
                match event.logical_key {
                    Key::Named(NamedKey::MediaPlayPause) => {
                        self.player.dispatch(PlayerAction::Toggle);
                    }
                    Key::Named(NamedKey::MediaTrackNext) => {
                        self.player.dispatch(PlayerAction::Next);
                    }
                    Key::Named(NamedKey::MediaTrackPrevious) => {
                        self.player.dispatch(PlayerAction::Previous);
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render(event_loop, id) {
                    self.fail(event_loop, e);
                    return;
                }
                self.poll_front(event_loop);
                self.set_flow(event_loop);
                return;
            }
            _ => {}
        }
        let (Some(win), Some(shared)) = (self.wins.get_mut(&id), &self.shared) else {
            return;
        };
        if let WindowEvent::Resized(size) = &event {
            win.surface.resize(&shared.gpu, size.width, size.height);
            if win.slot.panel().is_none() {
                win.maximized = win.window.is_maximized();
            }
        }
        win.translator.window_event(&event);
        if !win.hidden {
            win.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // The tray's window and icon go first, on this thread.
        self.tray = None;
        // What to save is read while the panel windows still exist; then they close in order.
        if let (Some(skin), Some(set)) = (self.skin.as_deref_mut(), self.set.as_mut())
            && skin.plan().map(|p| p.generation) == Some(set.generation)
        {
            skin.keep_report(&HostPanelWindows::new(set));
        }
        self.close_set();
    }
}

/// A mouse button as a number for the skin trace: 0 left, 1 right, 2 middle, 3 back, 4 forward,
/// then the other buttons by their own number.
fn trace_button(button: winit::event::MouseButton) -> u16 {
    use winit::event::MouseButton;
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(n) => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis() -> WinVis {
        WinVis {
            front_hidden: false,
            minimised: false,
            os_visible: Some(true),
            occluded: false,
        }
    }

    #[test]
    fn all_minimised_is_hidden() {
        let min = WinVis {
            minimised: true,
            ..vis()
        };
        assert!(!any_shown([min, min], false));
    }

    #[test]
    fn a_shown_panel_counts_with_the_native_window_front_hidden() {
        let native = WinVis {
            front_hidden: true,
            ..vis()
        };
        assert!(any_shown([native, vis()], false));
    }

    #[test]
    fn only_occluded_windows_are_hidden() {
        let occ = WinVis {
            occluded: true,
            ..vis()
        };
        assert!(!any_shown([occ, occ], false));
        assert!(any_shown([occ, vis()], false));
    }

    #[test]
    fn the_open_tray_menu_counts() {
        let min = WinVis {
            minimised: true,
            ..vis()
        };
        assert!(any_shown([min], true));
        assert!(!any_shown([min], false));
    }

    #[test]
    fn no_windows_is_hidden() {
        assert!(!any_shown([], false));
        assert!(any_shown([], true));
    }

    #[test]
    fn the_os_visible_answer() {
        let off = WinVis {
            os_visible: Some(false),
            ..vis()
        };
        let unknown = WinVis {
            os_visible: None,
            ..vis()
        };
        assert!(!any_shown([off], false));
        assert!(any_shown([unknown], false));
    }

    #[test]
    fn the_earlier_flow_wins() {
        let (soon, later) = (Instant::now(), Instant::now() + Duration::from_secs(1));
        let until = ControlFlow::WaitUntil;
        assert_eq!(
            merge_flow(ControlFlow::Wait, ControlFlow::Wait),
            ControlFlow::Wait
        );
        assert_eq!(merge_flow(until(later), ControlFlow::Wait), until(later));
        assert_eq!(merge_flow(until(later), until(soon)), until(soon));
        assert_eq!(
            merge_flow(until(soon), ControlFlow::Poll),
            ControlFlow::Poll
        );
    }
}
