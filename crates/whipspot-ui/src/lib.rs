//! Native views over `lune_ui::Ui` (D31). No winit, wgpu or OS `cfg` here: the host (`whipspot-app`)
//! owns the window and hands per-OS chrome in as data.
//!
//! The main window (M5, D46) is one layout in three themes: a glass background with a full-width
//! visualiser, the browse sheet or the idle now-playing view (one at a time, cross-faded by the
//! idle machine), the media control along the bottom and a centred caption, with toasts over all
//! of it. [`NativeUi`] is the shell: it owns the layers and their order; the pages and the media
//! control are modules it calls.

mod account;
mod anim;
mod art;
mod artist;
mod background;
mod browse;
mod caption;
pub mod diagnostics;
mod draw;
mod equalizer;
mod grid;
mod history;
mod icons;
mod idle;
mod layout;
pub mod library;
mod likes;
mod link;
mod lyrics;
mod media;
mod notices;
mod now_playing;
pub mod output;
mod pages;
pub mod palette;
mod queue;
mod scroll_memory;
mod search;
mod settings;
mod shortcuts;
mod signal;
pub mod skin;
mod spectrum;
pub mod theme;
pub mod title;
mod topbar;
mod tour;
mod tracks;
pub mod tray;

use std::sync::Arc;
use std::time::Duration;

use lune_ui::image::ImageStore;
use lune_ui::input::{CursorIcon, Event, PointerButton, WindowCommand};
use lune_ui::paint::{Rect, Xform};
use lune_ui::scene::LayerId;
use lune_ui::shortcuts::{CommandId, Shortcuts};
use lune_ui::text::{Spec, Style};
use lune_ui::theme::Theme as LuneTheme;
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::caption::{CaptionButton, CaptionOptions};
use lune_ui::widgets::toast::ToastKind;
use lune_ui::window::{ResizeEdge, edge_at};
use whipspot_core::folders::MusicFolders;
use whipspot_core::player::{PlayState, PlayerAction, PlayerHandle, PlayerState};

use art::ArtCache;
use background::CoverBg;
use browse::Browse;
use draw::{P, glass, shadow};
use idle::{Activity, IdleMachine};
use layout::{CAPTION_H, Layout};
use likes::{LIKE_HINT, LikeBook};
use media::MediaControl;
use notices::Notices;
use palette::CoverAccent;
use settings::{FolderPick, FoldersView, SettingsOut, SettingsView, SkinPick, SkinsView};
use shortcuts::Command;
use skin::SkinLibrary;
use spectrum::Spectrum;
use theme::{Look, ThemeId};
use tour::Tour;

pub use diagnostics::DiagnosticsUi;
pub use equalizer::EqMeters;
pub use history::{Route, Tab};
pub use idle::Mode;
pub use library::ProviderChoice;
pub use link::LinkGlyph;
pub use queue::QueueDrag;

/// The key the host knows the diagnostics window by (`WindowSpec::key`, `WindowView::take_toggles`).
pub const DIAGNOSTICS_KEY: &str = "diagnostics";

/// The diagnostics window's title band height, logical px (the main window's is
/// [`layout::CAPTION_H`]).
pub const TITLE_BAND: f32 = 34.0;
/// The title text's left edge in the diagnostics band when the caption is drawn.
const TITLE_LEFT: f32 = 16.0;
/// The playing marks' clock in a forced (demo) view, seconds: bars caught mid-bounce.
const EQ_DEMO_CLOCK: f64 = 0.37;
/// A cross-fade side below this share of its full alpha is not drawn (and takes no input).
const FADE_EPS: f32 = 0.02;
/// The id scope the browse sheet draws in, kept alive while the idle view hides the sheet.
const SHEET_SCOPE: &str = "sheet";
/// Draws the sheet in [`SHEET_SCOPE`]: a keep-alive scope, so its state carries the scope on its
/// chain, ended again before the frame finishes, so on a drawn frame state the sheet did not use
/// is dropped as anywhere else. Only [`hold_sheet`] keeps it.
fn draw_sheet<R>(ui: &mut Ui<'_>, f: impl FnOnce(&mut Ui<'_>) -> R) -> R {
    let r = ui.keep_alive(SHEET_SCOPE, f);
    ui.end_keep_alive(SHEET_SCOPE);
    r
}

/// A frame the idle view skips the sheet: what the sheet used when it was last drawn (its scroll
/// offsets) survives this frame.
fn hold_sheet(ui: &mut Ui<'_>) {
    ui.mark_alive(SHEET_SCOPE);
}

/// The frame interval while a cross-fade runs (about 60 Hz).
const FADE_FRAME: Duration = Duration::from_millis(16);
/// How often the Skins card's status line is redrawn while a skin loads (D5).
const SKIN_POLL: Duration = Duration::from_millis(50);

/// The skin library the Settings card chooses from, and the skins folder as text.
struct SkinsLink {
    library: SkinLibrary,
    folder: Option<String>,
}

impl std::fmt::Debug for SkinsLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SkinsLink")
    }
}

/// The music folders the Settings page lists, and the host's folder picker (it opens the OS
/// dialog on the UI thread and answers the chosen folder, if any).
struct FoldersLink {
    folders: Arc<dyn MusicFolders>,
    pick: Box<dyn FnMut() -> Option<String>>,
}

impl std::fmt::Debug for FoldersLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FoldersLink")
    }
}

/// What differs per OS in the chrome, chosen by the host and handed in as data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chrome {
    /// The window has no native frame: draw caption buttons, drag by the band, resize by the edges.
    pub drawn_caption: bool,
    /// The title is clipped this far from both sides of the window.
    pub title_inset: f32,
}

/// A hover state the demo forces without a pointer (the dumps only).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hover {
    /// The quality pill's signal-path tooltip.
    Pill,
    /// The progress ring's seek tooltip with the pointer at this fraction of the turn (0..1).
    Ring(f32),
    /// The like heart of the media control, as when the pointer is over it.
    Like,
    /// The queue pane's row at this list index, as when the pointer is over it (its grip and
    /// remove button show).
    QueueRow(usize),
    /// The top bar's provider menu, open.
    ProviderMenu,
    /// The `nth` artist or album link drawn in the frame (counted in drawing order), as when the
    /// pointer is over it, or, with `focus`, as when it has keyboard focus; its gradient is at
    /// `phase` (0..1 of a loop).
    Link { nth: usize, focus: bool, phase: f32 },
}

/// What the demo forces: the view, a hover state and the panes' forced states
/// (`NativeUi::force`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Force {
    pub mode: Option<Mode>,
    pub hover: Option<Hover>,
    /// The equalizer's limiter and clip indicators, lit whatever the engine says.
    pub eq_meters: Option<EqMeters>,
    /// A queue drag in progress, drawn without a pointer.
    pub queue_drag: Option<QueueDrag>,
    /// The first-run tour's card at this index of its list, shown whatever the saved flag says.
    pub tour: Option<usize>,
    /// The artist page's selected release chip, whatever was clicked.
    pub group: Option<whipspot_core::provider::ReleaseGroup>,
}

/// How much of a music service's streaming bitrate the user allows (MD88). The view shows the
/// choice provider-neutrally; the composition root maps it to an adapter's setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamQuality {
    /// Up to 320 kbps.
    #[default]
    High,
    /// Up to 160 kbps.
    Normal,
    /// Up to 96 kbps.
    Low,
}

impl StreamQuality {
    /// The stable key the settings file stores.
    pub fn key(self) -> &'static str {
        match self {
            StreamQuality::High => "high",
            StreamQuality::Normal => "normal",
            StreamQuality::Low => "low",
        }
    }

    /// The quality stored under `key`; an unknown key gives [`StreamQuality::High`].
    pub fn from_key(key: &str) -> StreamQuality {
        [
            StreamQuality::High,
            StreamQuality::Normal,
            StreamQuality::Low,
        ]
        .into_iter()
        .find(|q| q.key() == key)
        .unwrap_or_default()
    }

    /// The bitrate cap in kbps.
    pub fn kbps(self) -> u32 {
        match self {
            StreamQuality::High => 320,
            StreamQuality::Normal => 160,
            StreamQuality::Low => 96,
        }
    }
}

/// Where the controls the first-run tour points at were drawn in the last frame, in logical px;
/// `None` for a control that was not drawn (the browse sheet hidden in the idle view, no switch
/// to show, a pill that does not fit).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct Anchors {
    pub provider: Option<Rect>,
    pub gear: Option<Rect>,
    pub eye: Option<Rect>,
    pub pill: Option<Rect>,
    pub queue: Option<Rect>,
}

/// What the view starts with (read from the settings file by the composition root).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPrefs {
    pub theme: ThemeId,
    pub quality: StreamQuality,
    /// Hide the browse view when the pointer rests (MD50); on by default.
    pub auto_hide: bool,
    /// The first-run tour was finished or skipped; it shows once while this is false.
    pub tour_seen: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: ThemeId::default(),
            quality: StreamQuality::default(),
            auto_hide: true,
            tour_seen: false,
        }
    }
}

/// The main window's view: the shell of the round-3 design.
#[derive(Debug)]
pub struct NativeUi {
    chrome: Chrome,
    maximized: bool,
    commands: Vec<WindowCommand>,
    refreshed: bool,
    toggle_diagnostics: bool,
    theme_id: ThemeId,
    /// The streaming quality the user chose last, and the one the running session started with.
    quality: StreamQuality,
    quality_at_start: StreamQuality,
    /// The user's auto-hide switch (the top bar's icon): when off, the browse view stays.
    auto_hide: bool,
    /// The first-run tour was finished or skipped (saved on exit with the other prefs).
    tour_seen: bool,
    lune_theme: LuneTheme,
    idle: IdleMachine,
    force: Force,
    /// The tour's anchors, assembled at the end of each frame.
    anchors: Anchors,
    /// The first-run tour (Browse mode only).
    tour: Tour,
    /// The mode drawn in the last frame.
    last_mode: Mode,
    /// A pointer button is held (from the events of earlier frames).
    held: bool,
    /// Something outside the frame's events asked for the browse view (a shortcut, the queue
    /// icon, a demo hook).
    wake: bool,
    spectrum: Spectrum,
    peaks: spectrum::Peaks,
    notices: Notices,
    browse: Browse,
    media: MediaControl,
    art: ArtCache,
    likes: LikeBook,
    /// Which track is current and since when it changed (a new track fades in).
    track: anim::TrackWatch,
    /// The idle view's body, which changes one at a time with a fade (MD204).
    idle_swap: anim::Swap<now_playing::IdleModel>,
    /// The idle view waits for its lyrics or cover, or a body is fading (the demo's dumps wait).
    idle_busy: bool,
    /// The cover behind everything, its Ken Burns clock and cross-fade.
    cover_bg: CoverBg,
    /// The accents of the cover drawn now (theme C's; A and B ignore them).
    accent: Option<CoverAccent>,
    /// The Settings page's Skins card; `None` hides it.
    skins: Option<SkinsLink>,
    /// The Settings page's Music folders card; `None` hides it.
    folders: Option<FoldersLink>,
}

impl NativeUi {
    /// A view that opens on `start`'s tab, in the default theme.
    pub fn new(chrome: Chrome, start: Tab) -> Self {
        let theme_id = ThemeId::default();
        Self {
            chrome,
            maximized: false,
            commands: Vec::new(),
            refreshed: false,
            toggle_diagnostics: false,
            theme_id,
            quality: StreamQuality::default(),
            quality_at_start: StreamQuality::default(),
            auto_hide: UiPrefs::default().auto_hide,
            tour_seen: UiPrefs::default().tour_seen,
            lune_theme: theme::lune_theme(&Look::new(theme::theme(theme_id), None)),
            idle: IdleMachine::new(),
            force: Force::default(),
            anchors: Anchors::default(),
            tour: Tour::default(),
            last_mode: Mode::Browse,
            held: false,
            wake: false,
            spectrum: Spectrum::default(),
            peaks: spectrum::Peaks::default(),
            notices: Notices::default(),
            browse: Browse::new(start),
            media: MediaControl::default(),
            art: ArtCache::new(),
            likes: LikeBook::default(),
            track: anim::TrackWatch::default(),
            idle_swap: anim::Swap::default(),
            idle_busy: false,
            cover_bg: CoverBg::default(),
            accent: None,
            skins: None,
            folders: None,
        }
    }

    /// Shares the library provider choice (the switch in the top bar sets it; the skinned
    /// front-end and the host's settings read it).
    pub fn with_provider_choice(mut self, choice: ProviderChoice) -> Self {
        self.browse = self.browse.with_choice(choice);
        self
    }

    /// Shows the Music folders card in Settings over `folders`; `pick_folder` opens the OS folder
    /// dialog and answers the chosen folder (it blocks the UI thread while it is open).
    pub fn with_music_folders(
        mut self,
        folders: Arc<dyn MusicFolders>,
        pick_folder: impl FnMut() -> Option<String> + 'static,
    ) -> Self {
        self.folders = Some(FoldersLink {
            folders,
            pick: Box::new(pick_folder),
        });
        self
    }

    /// Shows the Skins card in Settings over `library`, with `folder` (the skins folder as text,
    /// `None` when there is none) under it; the folder is listed once now.
    pub fn with_skins(mut self, library: SkinLibrary, folder: Option<String>) -> Self {
        library.refresh();
        self.skins = Some(SkinsLink { library, folder });
        self
    }

    /// Starts with `prefs`: the theme applies before the first frame; the quality is both the
    /// user's choice and what the running session started with.
    pub fn with_prefs(mut self, prefs: UiPrefs) -> Self {
        self.set_theme(prefs.theme);
        self.quality = prefs.quality;
        self.quality_at_start = prefs.quality;
        self.auto_hide = prefs.auto_hide;
        self.tour_seen = prefs.tour_seen;
        if !prefs.tour_seen {
            self.tour.start();
        }
        self
    }

    /// The tab the top bar highlights now (the host reads it on exit to save it).
    pub fn tab(&self) -> Tab {
        self.browse.tab()
    }

    /// The toolkit theme the view's widgets read (the host hands it to the toolkit before each
    /// frame).
    pub fn look_theme(&self) -> LuneTheme {
        self.lune_theme
    }

    /// Follows the cover's accents: when they change, the toolkit theme the host reads is
    /// recomputed and another frame asked for.
    fn set_accent(&mut self, accent: Option<CoverAccent>) {
        if accent == self.accent {
            return;
        }
        self.accent = accent;
        let next = theme::lune_theme(&Look::new(theme::theme(self.theme_id), accent));
        if next.accent != self.lune_theme.accent {
            self.lune_theme = next;
            self.commands.push(WindowCommand::Redraw);
        }
    }

    /// Switches the theme (the host saves [`NativeUi::theme_id`] on exit).
    pub fn set_theme(&mut self, id: ThemeId) {
        self.theme_id = id;
        self.lune_theme = theme::lune_theme(&Look::new(theme::theme(id), self.accent));
        self.commands.push(WindowCommand::Redraw);
    }

    /// The theme in use.
    pub fn theme_id(&self) -> ThemeId {
        self.theme_id
    }

    /// The streaming quality the user chose last (it applies at the next start of the app).
    pub fn stream_quality(&self) -> StreamQuality {
        self.quality
    }

    /// Whether the browse view hides when the pointer rests (the host saves it on exit).
    pub fn auto_hide(&self) -> bool {
        self.auto_hide
    }

    /// Whether the first-run tour was finished or skipped (the host saves it on exit).
    pub fn tour_seen(&self) -> bool {
        self.tour_seen
    }

    /// Opens `route` in the browse sheet and brings the sheet up.
    pub fn navigate(&mut self, route: Route) {
        self.browse.navigate(route);
        self.wake = true;
    }

    /// Opens the Search route and runs `query` there.
    pub fn search(&mut self, query: &str) {
        self.browse.search(query);
        self.wake = true;
    }

    /// The mode drawn in the last frame.
    pub fn mode(&self) -> Mode {
        self.last_mode
    }

    /// Starts or stops recording the glyphs of lit artist and album links (the demo prints them).
    pub fn trace_links(&mut self, on: bool) {
        link::trace(on);
    }

    /// The boxes (x, y, w, h in logical px) of the artist and album links drawn in the last frame.
    pub fn link_boxes(&self) -> Vec<[f32; 4]> {
        link::boxes()
    }

    /// Starts or stops recording the boxes the artist and album pages draw (the demo's layout
    /// check reads them with [`Self::page_boxes`]).
    pub fn trace_page_boxes(&mut self, on: bool) {
        draw::page_boxes_enable(on);
    }

    /// The boxes (x, y, w, h in logical px, page coordinates) the artist or album page drew in the
    /// last frame, whether each is text, and the inner bounds of the sheet they must stay within.
    pub fn page_boxes(&self, window: (f32, f32)) -> (Vec<(bool, [f32; 4])>, [f32; 4]) {
        let sheet = Layout::new(Rect::new(0.0, 0.0, window.0, window.1)).sheet;
        let vp = grid::viewport(sheet);
        let boxes = draw::page_boxes()
            .into_iter()
            .map(|(k, r)| (k == draw::BoxKind::Text, [r.x, r.y, r.w, r.h]))
            .collect();
        (boxes, [vp.x, vp.y, vp.w, vp.h])
    }

    /// The glyphs of the lit links of the last frame; empty unless [`Self::trace_links`] is on.
    pub fn take_link_trace(&mut self) -> Vec<LinkGlyph> {
        link::take_trace()
    }

    /// Forces the view and a hover state, whatever the idle machine and the pointer say (the
    /// demo's dumps only; an all-`None` value hands control back).
    #[doc(hidden)]
    pub fn force(&mut self, force: Force) {
        self.force = force;
    }

    /// Registers the covers that arrived with the host's image store; the host calls it before
    /// each frame of this window (`WindowView::images`).
    pub fn images(&mut self, store: &mut dyn ImageStore) {
        self.art.sync(store);
    }

    /// Whether the idle view is waiting for its lyrics or cover, or fading one body for another
    /// (the demo's dumps wait for it to be over).
    #[doc(hidden)]
    pub fn idle_busy(&self) -> bool {
        self.idle_busy
    }

    /// The idle view (MD204): the body the track should show, which waits for the lyrics and the
    /// cover to settle, changes one at a time (the old fades out, then the new fades in) and
    /// holds the old snapshot to draw while it leaves. `fade` is the view's own cross-fade with
    /// the sheet.
    fn idle_frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        look: &Look,
        state: &Arc<PlayerState>,
        (fade, track_since): (f32, Option<Duration>),
        player: &PlayerHandle,
    ) {
        let now = ui.time();
        // A track not seen to change has been settled for as long as the app has run.
        let held = track_since.map_or(Duration::MAX, |s| now.saturating_sub(s));
        let want = now_playing::idle_want(ui, rect, state, &mut self.art, player, held);
        let pending = matches!(want, now_playing::IdleWant::Pending);
        let model = match want {
            now_playing::IdleWant::Show(m) => Some(m),
            _ => None,
        };
        self.idle_swap
            .update(model, now, anim::pace(), now_playing::IdleModel::same);
        self.idle_busy = pending || self.idle_swap.running();
        if self.idle_busy {
            ui.request_repaint_at(now + FADE_FRAME);
        }
        let (out, shown) = self.idle_swap.alphas(now);
        if out * fade > FADE_EPS
            && let Some(model) = self.idle_swap.leaving().cloned()
        {
            with_alpha(ui, out * fade, |ui| {
                now_playing::idle(
                    ui,
                    rect,
                    look,
                    &model,
                    &mut self.art,
                    &mut self.likes,
                    player,
                );
            });
        }
        if shown * fade > FADE_EPS
            && let Some(model) = self.idle_swap.shown().cloned()
        {
            with_alpha(ui, shown * fade, |ui| {
                now_playing::idle(
                    ui,
                    rect,
                    look,
                    &model,
                    &mut self.art,
                    &mut self.likes,
                    player,
                );
            });
        }
    }

    /// Whether every cover the last frame drew is registered or given up on (the demo's dumps
    /// wait for it).
    #[doc(hidden)]
    pub fn art_settled(&self) -> bool {
        self.art.settled()
    }

    /// Shows a fixed text as a toast at the next frame.
    pub fn push_notice(&mut self, text: &'static str) {
        self.notices.push(ToastKind::Info, text);
    }

    /// Whether the window is maximised now (the host tells before each frame).
    pub fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    /// The window operations the last frame asked for, in order (the `Ui` has no channel for
    /// them: the host applies them after the frame).
    pub fn take_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Whether the user asked to open or close the diagnostics window (the shortcut or the media
    /// control's device icon) since the last call.
    pub fn take_toggle_diagnostics(&mut self) -> bool {
        std::mem::take(&mut self.toggle_diagnostics)
    }

    /// The keyboard shortcut table the host gives the toolkit (see [`NativeUi::fired`]).
    pub fn shortcuts() -> Shortcuts {
        shortcuts::table()
    }

    /// Acts on the shortcut commands the last frame reported. `text_focused` tells that a text
    /// field had focus in that frame: nothing is acted on then. The host redraws when the view
    /// asked for another frame (`take_commands`).
    pub fn fired(&mut self, fired: &[CommandId], text_focused: bool, player: &PlayerHandle) {
        if text_focused {
            return;
        }
        for command in fired.iter().filter_map(Command::from_id) {
            if let Some(action) = command.action(&player.state()) {
                player.dispatch(action);
                continue;
            }
            match command {
                Command::FocusSearch => {
                    self.browse.navigate(Route::Search);
                    self.wake = true;
                }
                Command::OpenSettings => self.navigate(Route::Settings),
                Command::OpenEqualizer => self.navigate(Route::Equalizer),
                Command::OpenQueue => self.navigate(Route::Queue),
                Command::ToggleDiagnostics => self.toggle_diagnostics = true,
                Command::CycleTheme => {
                    let all = ThemeId::ALL;
                    let at = all.iter().position(|&t| t == self.theme_id).unwrap_or(0);
                    self.set_theme(all[(at + 1) % all.len()]);
                }
                Command::Back => {
                    self.browse.back();
                    self.wake = true;
                }
                Command::Forward => {
                    self.browse.forward();
                    self.wake = true;
                }
                _ => {}
            }
            self.commands.push(WindowCommand::Redraw);
        }
    }

    /// What this frame's events say to the idle machine; the mouse's side buttons step the
    /// history here.
    fn activity(&mut self, ui: &Ui<'_>, layout: &Layout, has_current: bool) -> Activity {
        let mut a = Activity {
            has_current,
            text_focus: self.browse.text_focused(),
            input: std::mem::take(&mut self.wake),
            auto_hide: self.tour.auto_hide(self.auto_hide),
            ..Activity::default()
        };
        // The media control is only a neutral zone while it is shown (not in the idle view).
        let media_shown = self.last_mode != Mode::Idle;
        let neutral = |pos: &(f32, f32)| idle::is_neutral(*pos, layout, media_shown);
        for event in &ui.input().events {
            match event {
                Event::PointerMoved { pos } => {
                    if neutral(pos) {
                        a.neutral = true;
                    } else {
                        a.moved = true;
                    }
                }
                Event::PointerLeft => a.left = true,
                Event::PointerButton {
                    button: PointerButton::Back,
                    pressed: true,
                    ..
                } => {
                    self.browse.back();
                    a.input = true;
                }
                Event::PointerButton {
                    button: PointerButton::Forward,
                    pressed: true,
                    ..
                } => {
                    self.browse.forward();
                    a.input = true;
                }
                Event::PointerButton { pos, pressed, .. } => {
                    self.held = *pressed;
                    if neutral(pos) {
                        a.neutral = true;
                    } else if *pressed {
                        a.input = true;
                    }
                }
                Event::Scroll { pos, .. } => {
                    if neutral(pos) {
                        a.neutral = true;
                    } else {
                        a.input = true;
                    }
                }
                Event::Key { pressed: true, .. } | Event::Text(_) => a.input = true,
                _ => {}
            }
        }
        a.button_held = self.held;
        a
    }

    /// Draws one frame: background, visualiser, the browse sheet or the idle view, the media
    /// control, the caption, toasts.
    pub fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        if !self.refreshed {
            self.refreshed = true;
            player.dispatch(PlayerAction::RefreshAuth);
        }
        for notice in player.drain_notices() {
            if self.likes.on_notice(&notice) {
                continue;
            }
            if let Some(rest) = self.browse.route_notice(notice) {
                self.notices.route(rest);
            }
        }
        let state = player.state();
        let diag = player.diagnostics();
        let win = ui.painter().window;
        let layout = Layout::new(win);
        let now = ui.time();
        // How much motion this frame plays: the toolkit's tier, none in the demo's forced views.
        anim::set_pace(anim::pace_of(ui.tier(), self.force.mode.is_some()));
        draw::page_boxes_frame();
        link::begin_frame(match self.force.hover {
            Some(Hover::Link { nth, focus, phase }) => Some(link::ForcedLink { nth, focus, phase }),
            _ => None,
        });
        self.art.note_time(now);
        self.track
            .update(state.current.as_ref().map(|n| n.track.uri.as_str()), now);
        let track_since = self.track.since();
        if anim::running(track_since, now, anim::TRACK) {
            ui.request_repaint_at(now + FADE_FRAME);
        }

        // The cover behind everything, and theme C's accent from it.
        let playing = state.play_state == PlayState::Playing;
        self.cover_bg.tick(now, playing);
        let cover = self.cover_bg.update(
            &mut self.art,
            player,
            state.current.as_ref().map(|n| n.track.images.as_slice()),
            now,
        );
        let accent = self
            .cover_bg
            .shown_url()
            .and_then(|url| self.art.accent(url));
        self.set_accent(accent);
        let look = Look::new(theme::theme(self.theme_id), accent);

        // Whether an Escape press is the tour's, read before the sheet's frame closes a menu or
        // cancels a drag with that same press.
        let esc_free = self.browse.escape_free();
        // The idle machine and the cross-fade (fade 0 is the sheet, 1 the idle view).
        let activity = self.activity(ui, &layout, state.current.is_some());
        let mode = self.idle.update(now, activity);
        let (mode, fade) = match self.force.mode {
            Some(forced) => (forced, if forced == Mode::Idle { 1.0 } else { 0.0 }),
            None => {
                if let Some(at) = self.idle.deadline() {
                    ui.request_repaint_at(at);
                }
                if self.idle.fading(now) {
                    ui.request_repaint_at(now + FADE_FRAME);
                }
                (mode, self.idle.fade(now))
            }
        };
        self.last_mode = mode;

        // Layers, back to front.
        let previous = self.cover_bg.previous(&mut self.art, now);
        // A bright cover darkens the background as the idle view comes in; the veil follows the
        // cover cross-fade, so a track change does not make it jump.
        let dim = self
            .cover_bg
            .dim(&self.art, previous.as_ref().map(|(_, f)| *f))
            * fade;
        background::background(
            ui.painter(),
            &look,
            win,
            cover,
            previous,
            self.cover_bg.clock(),
            dim,
        );
        if cover.is_some() && playing {
            ui.request_repaint_at(now + spectrum::FRAME_INTERVAL);
        }
        if self.cover_bg.fading(now) {
            ui.request_repaint_at(now + FADE_FRAME);
        }
        let dt = self.spectrum.update(player, now);
        self.peaks.update(self.spectrum.levels(), dt);
        spectrum::draw(
            ui.painter(),
            &look,
            layout.visualiser,
            self.spectrum.levels(),
            self.peaks.caps(),
        );
        if self.spectrum.active() || !self.peaks.at_rest() {
            ui.request_repaint_at(now + spectrum::FRAME_INTERVAL);
        }

        // The playing marks' clock: running while a song plays, a fixed one when the demo forces
        // the view (a dump is the same every time).
        grid::set_eq_clock((playing).then(|| {
            if self.force.mode.is_some() {
                EQ_DEMO_CLOCK
            } else {
                now.as_secs_f64()
            }
        }));
        let mut settings_out = SettingsOut::default();
        let mut anchors = Anchors::default();
        let skin_names = self.skins.as_ref().map(|l| l.library.names());
        let skin_status = self.skins.as_ref().map(|l| l.library.status());
        if self.skins.as_ref().is_some_and(|l| l.library.pending()) {
            ui.request_repaint_at(now + SKIN_POLL);
        }
        let folder_list = self.folders.as_ref().map(|l| l.folders.folders());
        let folder_status = self.folders.as_ref().map(|l| l.folders.status());
        if fade < 1.0 - FADE_EPS {
            let folders = match (&folder_list, folder_status) {
                (Some(list), Some(status)) => Some(FoldersView { list, status }),
                _ => None,
            };
            let skins = match (&self.skins, &skin_names, &skin_status) {
                (Some(link), Some(names), Some(status)) => Some(SkinsView {
                    names,
                    status,
                    folder: link.folder.as_deref(),
                }),
                _ => None,
            };
            let view = SettingsView {
                theme: self.theme_id,
                quality: self.quality,
                quality_at_start: self.quality_at_start,
                shortcuts: shortcuts::labels(),
                cover: accent,
                auto_hide: self.auto_hide,
                skins,
                folders,
            };
            let force = self.force;
            // The sheet's per-widget state (the scroll offsets of the grids, the home page and the
            // track lists) survives the frames the idle view skips it (`hold_sheet`), and only
            // those.
            draw_sheet(ui, |ui| {
                with_alpha(ui, 1.0 - fade, |ui| {
                    sheet_surface(ui.painter(), &look, layout.sheet);
                    settings_out = self.browse.frame(
                        ui,
                        layout.sheet,
                        &look,
                        &state,
                        player,
                        &mut self.art,
                        &mut self.likes,
                        (&view, &force),
                    );
                });
            });
            anchors = self.browse.anchors();
        } else {
            hold_sheet(ui);
        }
        if let Some(theme) = settings_out.theme {
            self.set_theme(theme);
        }
        if let Some(quality) = settings_out.quality {
            self.quality = quality;
        }
        if settings_out.toggle_auto_hide {
            self.auto_hide = !self.auto_hide;
        }
        if let (Some(pick), Some(link)) = (settings_out.skin.take(), &self.skins) {
            match pick {
                SkinPick::Native => link.library.choose_native(),
                SkinPick::Use(name) => link.library.choose(&name),
                SkinPick::Refresh => link.library.refresh(),
            }
            ui.request_repaint_at(now + SKIN_POLL);
        }
        if let (Some(pick), Some(link)) = (settings_out.folder.take(), &mut self.folders) {
            match pick {
                FolderPick::Add => {
                    if let Some(path) = (link.pick)()
                        && let Err(e) = link.folders.add(path)
                    {
                        self.notices.push(ToastKind::Info, e.text());
                    }
                }
                FolderPick::Remove(path) => link.folders.remove(&path),
                FolderPick::Rescan => link.folders.rescan(),
            }
            ui.request_repaint_at(now + SKIN_POLL);
        }
        if settings_out.poll {
            ui.request_repaint_at(now + SKIN_POLL);
        }
        if let Some(route) = settings_out.open {
            self.navigate(route);
        }
        if settings_out.restart_tour {
            self.tour.start();
            self.navigate(Tab::Home.root());
        }
        if fade > FADE_EPS {
            self.idle_frame(ui, layout.idle, &look, &state, (fade, track_since), player);
        } else {
            self.idle_swap.reset();
            self.idle_busy = false;
        }

        // The playing marks bounce while a song plays; ask for frames only while some were drawn.
        if grid::take_eq_moved() {
            ui.request_repaint_at(now + spectrum::FRAME_INTERVAL);
        }
        // The media control fades with the sheet and is gone in the idle view (v1 round 2); a
        // forced hover (the demo) keeps it up.
        let media_alpha = media_alpha(
            fade,
            self.force
                .hover
                .is_some_and(|h| !matches!(h, Hover::Link { .. })),
        );
        if media_alpha > FADE_EPS {
            let out = with_alpha(ui, media_alpha, |ui| {
                self.media.frame(
                    ui,
                    layout.media,
                    &look,
                    &state,
                    &diag,
                    self.force.hover,
                    player,
                    &mut self.art,
                    &mut self.likes,
                    track_since,
                )
            });
            if out.toggle_audio {
                self.toggle_diagnostics = true;
            }
            if out.open_queue {
                self.browse.navigate(Route::Queue);
                self.wake = true;
            }
            anchors.pill = out.pill_rect;
            anchors.queue = out.queue_rect;
        }
        self.anchors = anchors;

        // A forced card is held every frame (its control may come up a few frames late).
        if let Some(i) = self.force.tour {
            self.tour.show(i);
        }
        // The first-run tour's card, last: above the media control, below the caption and toasts.
        // It is left out while the provider menu is open: the menu opens right under the card's
        // control, and the card (on the Modal layer) would cover it and take its clicks.
        let menu_open =
            self.browse.provider_menu_open() || self.force.hover == Some(Hover::ProviderMenu);
        if !menu_open
            && let Some(view) = self.tour.view(&anchors, mode == Mode::Browse)
            && self.tour.draw(ui, &look, &view, &anchors, esc_free)
        {
            ui.request_repaint_at(now + FADE_FRAME);
        }
        if self.tour.take_finished() {
            self.tour_seen = true;
        }

        // Covers that arrived wait for the host's next `images` call: draw again soon; one still
        // fading in from the placeholder asks for frames until it is there.
        if self.art.has_queued() || self.art.fading() {
            ui.request_repaint_at(now + FADE_FRAME);
        }
        if self.likes.take_hint() {
            self.notices.push(ToastKind::Info, LIKE_HINT);
            self.browse.navigate(Route::Account);
            self.wake = true;
        }

        caption::draw(ui, layout.caption, &look, &title::window_title(&state).text);
        window_controls(
            ui,
            win,
            CAPTION_H,
            self.chrome,
            self.maximized,
            &mut self.commands,
        );

        self.notices.draw(ui);
        // A clicked artist or album link opens its page (the library switches to its provider
        // first when another one shows).
        for clicked in link::take_clicks() {
            self.browse.open_ref(&clicked, &state.providers);
            self.wake = true;
            ui.request_repaint_at(now + FADE_FRAME);
        }
    }
}

/// Runs `draw` with everything it paints faded to `alpha` (0..1).
pub(crate) fn with_alpha<T>(ui: &mut Ui<'_>, alpha: f32, draw: impl FnOnce(&mut Ui<'_>) -> T) -> T {
    ui.painter().push_xform(Xform {
        origin: (0.0, 0.0),
        scale: 1.0,
        dx: 0.0,
        dy: 0.0,
        alpha: alpha.clamp(0.0, 1.0),
    });
    let out = draw(ui);
    ui.painter().pop_xform();
    out
}

/// The media control's alpha at idle fade `fade` (0 browse, 1 idle): it fades with the sheet, and
/// stays fully shown while a hover is forced.
fn media_alpha(fade: f32, forced: bool) -> f32 {
    if forced { 1.0 } else { 1.0 - fade }
}

/// The browse sheet's glass, on the popup layer (the page's own content must not be blurred by
/// its own backdrop); the page content is drawn over it.
fn sheet_surface(p: &mut P<'_>, k: &Look, r: Rect) {
    p.on(LayerId::Popup);
    let radius = k.radii().sheet;
    if k.surface().bevel {
        shadow(p, r, radius, 26.0, 1.0);
    }
    glass(p, k, r, radius, k.surface().sheet_alpha, false);
}

/// Draws the window's title in the band: left-aligned as in the Windows reference when the caption
/// is drawn (the window buttons on the right), centred between the native buttons otherwise.
pub(crate) fn draw_title(ui: &mut Ui<'_>, win: Rect, chrome: Chrome, title: &str) {
    let theme = *ui.theme();
    let p = ui.painter();
    p.on(LayerId::Base);
    let side = chrome.title_inset;
    let title = Spec::plain(title, Style::ui(12.0, theme.fg_secondary).medium());
    let band = Rect::new(0.0, 0.0, win.w, TITLE_BAND - 2.0);
    if chrome.drawn_caption {
        let clip = Rect::new(0.0, 0.0, (win.w - side).max(0.0), TITLE_BAND);
        p.set_clip(Some(clip));
        p.text_in(
            &title,
            Rect::new(TITLE_LEFT, 0.0, (clip.w - TITLE_LEFT).max(0.0), band.h),
            0.0,
        );
    } else {
        p.set_clip(Some(Rect::new(
            side,
            0.0,
            (win.w - 2.0 * side).max(0.0),
            TITLE_BAND,
        )));
        p.text_centered(&title, band);
    }
    p.set_clip(None);
    p.on(LayerId::Page);
}

/// The window's own controls, registered last so they sit over the content: the resize edges and
/// the caption band of height `band_h` (drag, double-click maximise) and the caption buttons when drawn. The window
/// operations they ask for go to `commands`.
pub(crate) fn window_controls(
    ui: &mut Ui<'_>,
    win: Rect,
    band_h: f32,
    chrome: Chrome,
    maximized: bool,
    commands: &mut Vec<WindowCommand>,
) {
    let (w, h) = (win.w, win.h);
    let pressed_edge = if chrome.drawn_caption && !maximized {
        if let Some(edge) = ui.pointer_pos().and_then(|p| edge_at(p, w, h)) {
            ui.set_cursor(resize_cursor(edge));
        }
        edge_press(&ui.input().events, (w, h))
    } else {
        None
    };
    if let Some(edge) = pressed_edge {
        commands.push(WindowCommand::DragResize(edge));
    }

    // The band first, so the caption buttons registered after it sit on top.
    let band_id = ui.id("title-band");
    let band = ui.interact(band_id, Rect::new(0.0, 0.0, w, band_h.min(h)), Sense::CLICK);
    if band.press_started && pressed_edge.is_none() {
        commands.push(band_command(band.press_count, chrome));
    }
    if chrome.drawn_caption {
        let opts = CaptionOptions {
            height: band_h,
            maximized,
            active: ui.input().focused,
        };
        if let Some(button) = ui.caption_buttons("caption", win, opts) {
            commands.push(caption_command(button));
        }
    }
}

/// The window command of a caption button.
fn caption_command(button: CaptionButton) -> WindowCommand {
    match button {
        CaptionButton::Minimize => WindowCommand::Minimize,
        CaptionButton::Maximize => WindowCommand::ToggleMaximize,
        CaptionButton::Close => WindowCommand::Close,
    }
}

/// What a press in the title band does: a double click maximises or restores (borderless windows
/// only: elsewhere the OS owns that), any other press drags the window.
fn band_command(press_count: u8, chrome: Chrome) -> WindowCommand {
    if press_count >= 2 && chrome.drawn_caption {
        WindowCommand::ToggleMaximize
    } else {
        WindowCommand::DragMove
    }
}

fn resize_cursor(edge: ResizeEdge) -> CursorIcon {
    match edge {
        ResizeEdge::North | ResizeEdge::South => CursorIcon::NsResize,
        ResizeEdge::East | ResizeEdge::West => CursorIcon::EwResize,
        ResizeEdge::NorthEast | ResizeEdge::SouthWest => CursorIcon::NeswResize,
        ResizeEdge::NorthWest | ResizeEdge::SouthEast => CursorIcon::NwseResize,
    }
}

/// The edge a primary press of this frame landed on, for a window of `size` (logical).
fn edge_press(events: &[Event], size: (f32, f32)) -> Option<ResizeEdge> {
    events.iter().find_map(|e| match e {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            ..
        } => edge_at(*pos, size.0, size.1),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_quality_keys_round_trip_and_unknown_is_high() {
        for q in [
            StreamQuality::High,
            StreamQuality::Normal,
            StreamQuality::Low,
        ] {
            assert_eq!(StreamQuality::from_key(q.key()), q);
        }
        assert_eq!(StreamQuality::from_key("extreme"), StreamQuality::High);
        assert_eq!(StreamQuality::from_key(""), StreamQuality::High);
        let kbps: Vec<u32> = [
            StreamQuality::High,
            StreamQuality::Normal,
            StreamQuality::Low,
        ]
        .iter()
        .map(|q| q.kbps())
        .collect();
        assert_eq!(kbps, [320, 160, 96]);
    }

    #[test]
    fn prefs_default_to_theme_c_and_high() {
        let prefs = UiPrefs::default();
        assert_eq!(
            (prefs.theme, prefs.quality),
            (ThemeId::C, StreamQuality::High)
        );
        let chrome = Chrome {
            drawn_caption: false,
            title_inset: 0.0,
        };
        let ui = NativeUi::new(chrome, Tab::Home).with_prefs(UiPrefs {
            theme: ThemeId::B,
            quality: StreamQuality::Low,
            auto_hide: false,
            tour_seen: true,
        });
        assert_eq!(ui.theme_id(), ThemeId::B);
        assert_eq!(ui.stream_quality(), StreamQuality::Low);
        assert_eq!(ui.quality_at_start, StreamQuality::Low);
        assert!(!ui.auto_hide());
        assert!(ui.tour_seen());
        assert!(!NativeUi::new(chrome, Tab::Home).tour_seen());
        assert!(!ui.tour.running(), "a seen tour does not start");
        let fresh = NativeUi::new(chrome, Tab::Home).with_prefs(UiPrefs::default());
        assert!(fresh.tour.running(), "an unseen tour starts at launch");
        assert!(UiPrefs::default().auto_hide);
        assert!(NativeUi::new(chrome, Tab::Home).auto_hide());
    }

    /// Draws one frame of a scrolling page the way the shell draws the sheet: inside
    /// [`SHEET_SCOPE`] when `drawn`, and with the scope merely held when the idle view skips it.
    /// Returns the page's scroll offset when it was drawn.
    fn sheet_frame(
        h: &mut lune_ui::testing::UiHarness,
        ms: u64,
        drawn: bool,
        scoped: bool,
    ) -> Option<f64> {
        page_frame(h, ms, drawn, scoped, "page")
    }

    /// [`sheet_frame`] drawing the page `key`.
    fn page_frame(
        h: &mut lune_ui::testing::UiHarness,
        ms: u64,
        drawn: bool,
        scoped: bool,
        key: &str,
    ) -> Option<f64> {
        use lune_ui::widgets::scroll_area::{Extent, ScrollOptions};
        h.at_ms(ms);
        let mut offset = None;
        h.frame(|ui| {
            let mut page = |ui: &mut Ui<'_>| {
                let vp = Rect::new(0.0, 0.0, 200.0, 100.0);
                let area = ui.scroll_area(("grid", key), vp, &ScrollOptions::default(), |_, _| {
                    (
                        (),
                        Extent {
                            width: 200.0,
                            height: 1000.0,
                        },
                    )
                });
                offset = Some(area.offset.1);
            };
            match (drawn, scoped) {
                (true, true) => draw_sheet(ui, page),
                (true, false) => page(ui),
                (false, true) => hold_sheet(ui),
                (false, false) => {}
            }
        });
        offset
    }

    #[test]
    fn a_scrolled_page_keeps_its_offset_across_the_frames_the_idle_view_skips_the_sheet() {
        let run = |scoped: bool| {
            let mut h = lune_ui::testing::UiHarness::new((200.0, 100.0));
            sheet_frame(&mut h, 0, true, scoped);
            h.input.pointer_to(50.0, 50.0);
            h.input.scroll_pixels(0.0, -300.0);
            sheet_frame(&mut h, 16, true, scoped);
            let before = sheet_frame(&mut h, 32, true, scoped).expect("drawn");
            // Idle: the sheet is not drawn for several frames.
            for i in 0..6 {
                assert_eq!(sheet_frame(&mut h, 100 + i * 16, false, scoped), None);
            }
            let after = sheet_frame(&mut h, 300, true, scoped).expect("drawn");
            (before, after)
        };
        let (before, after) = run(true);
        assert!(before > 0.0, "the page scrolled ({before})");
        assert_eq!(after, before, "the offset survives the idle frames");
        // The instrument can fail: without the scope the offset is dropped.
        let (before, after) = run(false);
        assert!(before > 0.0 && after == 0.0, "{before} -> {after}");
    }

    #[test]
    fn a_page_not_drawn_while_the_sheet_is_drawn_loses_its_offset() {
        // As on main: a tab switch (another page drawn in the sheet) drops the page's state;
        // only the idle frames hold it.
        let mut h = lune_ui::testing::UiHarness::new((200.0, 100.0));
        page_frame(&mut h, 0, true, true, "a");
        h.input.pointer_to(50.0, 50.0);
        h.input.scroll_pixels(0.0, -300.0);
        page_frame(&mut h, 16, true, true, "a");
        let before = page_frame(&mut h, 32, true, true, "a").expect("drawn");
        assert!(before > 0.0, "the page scrolled ({before})");
        page_frame(&mut h, 48, true, true, "b");
        page_frame(&mut h, 64, true, true, "b");
        let after = page_frame(&mut h, 80, true, true, "a").expect("drawn");
        assert_eq!(after, 0.0, "the offset was dropped with the page");
    }
}
