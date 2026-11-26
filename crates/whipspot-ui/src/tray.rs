//! The tray / menu-bar icon's content (D7, D8, D9): the menu's rows and what a pick does, the
//! tooltip and the icon's picture. The host owns the OS item and the popover window; nothing here
//! opens a window.
//!
//! The menu is luneui's glass menu with two pages: the main page (what plays, play or pause, next,
//! previous, the skin choice, show, quit) and the skins page (back, the native look, each skin).
//! Rows are plain data ([`Row`]) so the texts, the disabled flags and the picks are testable without
//! a window; [`TrayMenu::frame`] elides them to the width cap and draws them.

use lune_ui::icon;
use lune_ui::paint::{Painter, Rect};
use lune_ui::text::{Spec, Style};
use lune_ui::ui::Ui;
use lune_ui::widgets::menu::{MenuItem, MenuRow};
use lune_ui::widgets::tone::Tone;
use whipspot_core::player::{PlayState, PlayerAction, PlayerHandle, PlayerState};
use whipspot_core::text::display_text;

use crate::settings::current_skin;
use crate::skin::{SkinLibrary, SkinStatus};

/// The product name, the tooltip's first word.
const PRODUCT: &str = "Whipspot";
/// The widest the menu may be, logical px.
pub const MENU_MAX_W: f32 = 280.0;
/// The room the header name, the header status and an item label have inside [`MENU_MAX_W`]:
/// the panel's padding (6 each side), the row's insets and the dot or glyph slot, as `menu_size`
/// counts them.
const NAME_MAX_W: f32 = MENU_MAX_W - 2.0 * 6.0 - 8.0 - (8.0 + 8.0) - 8.0;
const STATUS_MAX_W: f32 = MENU_MAX_W - 2.0 * 6.0 - 16.0;
const LABEL_MAX_W: f32 = MENU_MAX_W - 2.0 * 6.0 - 8.0 - 14.0 - 8.0 - 8.0;
/// The chevron's room (its glyph and the gap before it) taken off a label.
const CHEVRON_ROOM: f32 = 28.0;
/// Characters kept of a title or a skin name before the width cut (a long input is cut early so
/// the measuring stays cheap).
const TEXT_CHARS: usize = 200;

/// The status line of the header when nothing is playing.
const NOTHING_PLAYING_HINT: &str = "Choose something to play";
/// Characters of the track and the artist in the tooltip (Windows cuts the tooltip at 128 UTF-16
/// units).
const TIP_TRACK_CHARS: usize = 60;
const TIP_ARTIST_CHARS: usize = 40;

/// Which page the menu shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Main,
    Skins,
}

/// What a row does when picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Play or pause.
    Toggle,
    Next,
    Previous,
    /// The skins page.
    OpenSkins,
    /// The main page.
    Back,
    /// The native look.
    Native,
    /// The skin of this file name.
    Skin(String),
    Show,
    Quit,
}

/// What the app must do that the menu cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayRequest {
    /// Bring the app's window to the front.
    Show,
    /// Exit as closing the main window does.
    Quit,
}

/// One row of the menu as data.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// The track and its artist, behind a status dot.
    Header {
        name: String,
        status: String,
        tone: Tone,
    },
    Separator,
    Item {
        text: String,
        glyph: Option<&'static str>,
        disabled: bool,
        chevron: bool,
        pick: Pick,
    },
}

impl Row {
    fn item(text: impl Into<String>, pick: Pick) -> Row {
        Row::Item {
            text: text.into(),
            glyph: None,
            disabled: false,
            chevron: false,
            pick,
        }
    }

    fn disabled(mut self, off: bool) -> Row {
        if let Row::Item { disabled, .. } = &mut self {
            *disabled = off;
        }
        self
    }

    /// The row's label, when it has one.
    pub fn text(&self) -> Option<&str> {
        match self {
            Row::Item { text, .. } => Some(text),
            Row::Header { name, .. } => Some(name),
            Row::Separator => None,
        }
    }
}

/// Whether a pick closes the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A page change: the menu stays open.
    Stay,
    Close,
}

/// What one menu frame reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrayFrame {
    /// Escape, a press outside or a focus loss asked to close the menu.
    pub dismissed: bool,
    /// A pick closed the menu.
    pub closed: bool,
}

/// The tray menu's state: the page, the skin library it chooses from and the request waiting for
/// the host.
#[derive(Clone)]
pub struct TrayMenu {
    library: SkinLibrary,
    folder: bool,
    page: Page,
    request: Option<TrayRequest>,
}

impl TrayMenu {
    /// A menu over `library`; `folder` tells that there is a skins folder (without one the skin
    /// row is disabled).
    pub fn new(library: SkinLibrary, folder: bool) -> Self {
        TrayMenu {
            library,
            folder,
            page: Page::Main,
            request: None,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// The menu opens: always on the main page.
    pub fn open(&mut self) {
        self.page = Page::Main;
    }

    /// The show or quit request a pick made, once.
    pub fn take_request(&mut self) -> Option<TrayRequest> {
        self.request.take()
    }

    /// The rows of the current page for `state`, as the data they are (not yet cut to the width).
    pub fn rows(&self, state: &PlayerState) -> Vec<Row> {
        match self.page {
            Page::Main => self.main_rows(state),
            Page::Skins => self.skin_rows(),
        }
    }

    fn main_rows(&self, state: &PlayerState) -> Vec<Row> {
        let header = match &state.current {
            Some(now) => Row::Header {
                name: display_text(&now.track.name, TEXT_CHARS),
                status: display_text(&now.track.artist, TEXT_CHARS),
                tone: match state.play_state {
                    PlayState::Playing | PlayState::Loading => Tone::Ok,
                    PlayState::Paused => Tone::Paused,
                    PlayState::Stopped => Tone::Neutral,
                },
            },
            None => Row::Header {
                name: "Nothing playing".to_owned(),
                // The toolkit's header always reserves a status line; an empty one shows as a gap.
                status: NOTHING_PLAYING_HINT.to_owned(),
                tone: Tone::Neutral,
            },
        };
        let running = matches!(state.play_state, PlayState::Playing | PlayState::Loading);
        let nothing_queued = state.current.is_none() && state.queue.items.is_empty();
        let nothing_current = state.current.is_none();
        let mut skin = Row::item("Skin", Pick::OpenSkins).disabled(!self.folder);
        if let Row::Item { chevron, .. } = &mut skin {
            *chevron = true;
        }
        vec![
            header,
            Row::Separator,
            Row::item(if running { "Pause" } else { "Play" }, Pick::Toggle)
                .disabled(nothing_queued),
            Row::item("Next", Pick::Next).disabled(nothing_current),
            Row::item("Previous", Pick::Previous).disabled(nothing_current),
            Row::Separator,
            skin,
            Row::Separator,
            Row::item("Show Whipspot", Pick::Show),
            Row::item("Quit Whipspot", Pick::Quit),
        ]
    }

    fn skin_rows(&self) -> Vec<Row> {
        let status = self.library.status();
        let current = current_skin(&status);
        let names = self.library.names();
        let mark = |on: bool, row: Row| match row {
            Row::Item {
                text,
                disabled,
                chevron,
                pick,
                ..
            } => Row::Item {
                text,
                glyph: on.then_some(icon::CHECK),
                disabled,
                chevron,
                pick,
            },
            other => other,
        };
        let mut back = Row::item("Back", Pick::Back);
        if let Row::Item { glyph, .. } = &mut back {
            *glyph = Some(icon::ARROW_LEFT);
        }
        let mut rows = vec![
            back,
            Row::Separator,
            mark(current.is_none(), Row::item("Native look", Pick::Native)),
        ];
        for name in &names {
            let on = current == Some(name.as_str());
            rows.push(mark(
                on,
                Row::item(display_text(name, TEXT_CHARS), Pick::Skin(name.clone())),
            ));
        }
        if names.is_empty() {
            rows.push(
                Row::item("No skins found", Pick::Back).disabled(true), // never picked
            );
        }
        rows
    }

    /// The index of the row that does `pick` on the current page.
    pub fn index_of(&self, state: &PlayerState, pick: &Pick) -> Option<usize> {
        self.rows(state)
            .iter()
            .position(|r| matches!(r, Row::Item { pick: p, .. } if p == pick))
    }

    /// What picking row `index` does: the menu's own change, and the player action to send. A
    /// disabled or non-item row does nothing.
    pub fn pick_at(
        &mut self,
        index: usize,
        state: &PlayerState,
    ) -> (Outcome, Option<PlayerAction>) {
        let rows = self.rows(state);
        match rows.get(index) {
            Some(Row::Item {
                pick,
                disabled: false,
                ..
            }) => self.apply(pick.clone()),
            _ => (Outcome::Stay, None),
        }
    }

    /// Applies `pick`.
    pub fn apply(&mut self, pick: Pick) -> (Outcome, Option<PlayerAction>) {
        match pick {
            Pick::Toggle => (Outcome::Close, Some(PlayerAction::Toggle)),
            Pick::Next => (Outcome::Close, Some(PlayerAction::Next)),
            Pick::Previous => (Outcome::Close, Some(PlayerAction::Previous)),
            Pick::OpenSkins => {
                self.page = Page::Skins;
                self.library.refresh();
                (Outcome::Stay, None)
            }
            Pick::Back => {
                self.page = Page::Main;
                (Outcome::Stay, None)
            }
            Pick::Native => {
                self.library.choose_native();
                (Outcome::Close, None)
            }
            Pick::Skin(name) => {
                self.library.choose(&name);
                (Outcome::Close, None)
            }
            Pick::Show => {
                self.request = Some(TrayRequest::Show);
                (Outcome::Close, None)
            }
            Pick::Quit => {
                self.request = Some(TrayRequest::Quit);
                (Outcome::Close, None)
            }
        }
    }

    /// The rows cut to the width cap, measured with the styles `menu_size` measures with.
    fn fitted<H: Copy + PartialEq>(&self, p: &mut Painter<'_, H>, state: &PlayerState) -> Vec<Row> {
        let t = p.theme;
        let name_style = Style::ui(t.font_base, t.menu_text_primary).medium();
        let status_style = Style::ui(t.font_s, t.menu_text_tertiary);
        let label_style = Style::ui(12.0, t.menu_text_primary);
        let cut = |p: &mut Painter<'_, H>, text: &str, style: Style, max: f32| {
            let spec = p.elide(&Spec::plain(text, style), max);
            lune_ui::text::joined_text(&spec)
        };
        self.rows(state)
            .into_iter()
            .map(|row| match row {
                Row::Header { name, status, tone } => Row::Header {
                    name: cut(p, &name, name_style, NAME_MAX_W),
                    status: cut(p, &status, status_style, STATUS_MAX_W),
                    tone,
                },
                Row::Item {
                    text,
                    glyph,
                    disabled,
                    chevron,
                    pick,
                } => {
                    let room = if chevron {
                        LABEL_MAX_W - CHEVRON_ROOM
                    } else {
                        LABEL_MAX_W
                    };
                    Row::Item {
                        text: cut(p, &text, label_style, room),
                        glyph,
                        disabled,
                        chevron,
                        pick,
                    }
                }
                Row::Separator => Row::Separator,
            })
            .collect()
    }

    /// The size in logical px of the window the current page fills.
    pub fn size<H: Copy + PartialEq>(
        &self,
        p: &mut Painter<'_, H>,
        state: &PlayerState,
    ) -> (f32, f32) {
        let rows = self.fitted(p, state);
        lune_ui::widgets::menu::menu_size(p, &menu_rows(&rows))
    }

    /// Draws the menu filling `area` and acts on a pick: the player action goes to `player`, a
    /// show or quit waits for [`TrayMenu::take_request`].
    pub fn frame(&mut self, ui: &mut Ui<'_>, area: Rect, player: &PlayerHandle) -> TrayFrame {
        let (out, action) = self.frame_of(ui, area, &player.state());
        if let Some(action) = action {
            player.dispatch(action);
        }
        out
    }

    /// [`TrayMenu::frame`] over a given state, handing back the player action instead of sending
    /// it (the demo's dumps draw states no player has).
    pub fn frame_of(
        &mut self,
        ui: &mut Ui<'_>,
        area: Rect,
        state: &PlayerState,
    ) -> (TrayFrame, Option<PlayerAction>) {
        let rows = self.fitted(ui.painter(), state);
        let response = ui.menu_window_in("tray-menu", area, &menu_rows(&rows));
        let mut out = TrayFrame {
            dismissed: response.dismissed,
            ..TrayFrame::default()
        };
        let mut action = None;
        if let Some(index) = response.picked {
            // The rows the pick maps through are the unfitted ones: same order and picks.
            let (outcome, a) = self.pick_at(index, state);
            action = a;
            out.closed = outcome == Outcome::Close;
        }
        (out, action)
    }
}

/// The rows as luneui draws them.
fn menu_rows(rows: &[Row]) -> Vec<MenuRow<'_>> {
    rows.iter()
        .map(|row| match row {
            Row::Header { name, status, tone } => MenuRow::Header {
                name,
                status,
                tone: *tone,
            },
            Row::Separator => MenuRow::Separator,
            Row::Item {
                text,
                glyph,
                disabled,
                chevron,
                ..
            } => MenuRow::Entry(MenuItem {
                glyph: *glyph,
                text,
                disabled: *disabled,
                chevron: *chevron,
                ..MenuItem::default()
            }),
        })
        .collect()
}

/// The icon's tooltip: "Whipspot", or "Whipspot — <track> · <artist>" with control characters
/// gone. The host sets it only when it changes.
pub fn tooltip(state: &PlayerState) -> String {
    let Some(now) = &state.current else {
        return PRODUCT.to_owned();
    };
    let track = display_text(&now.track.name, TIP_TRACK_CHARS);
    let artist = display_text(&now.track.artist, TIP_ARTIST_CHARS);
    match (track.is_empty(), artist.is_empty()) {
        (true, _) => PRODUCT.to_owned(),
        (false, true) => format!("{PRODUCT} \u{2014} {track}"),
        (false, false) => format!("{PRODUCT} \u{2014} {track} \u{b7} {artist}"),
    }
}

/// Whether a menu pick is the skin of `status`' current choice (the Settings card's rule).
pub fn is_current(status: &SkinStatus, name: &str) -> bool {
    current_skin(status) == Some(name)
}

// ---- the icon ---------------------------------------------------------------------------------

/// The icon's grid, in units; the picture is this scaled to the requested size.
const GRID: f32 = 16.0;
/// The square: inset half a unit, corner radius 3.5.
const SQUARE_INSET: f32 = 0.5;
const SQUARE_RADIUS: f32 = 3.5;
/// The gradient's top and bottom (theme A's ramp: 40 % of the way from its first to its second
/// stop, and its first stop).
const TOP: [f32; 3] = [32.0, 158.0, 134.0];
const BOTTOM: [f32; 3] = [22.0, 122.0, 106.0];
/// The bars: left unit, height in units; all 2 wide, bottoms at unit 13, top corners 0.5.
const BARS: [(f32, f32); 3] = [(3.0, 6.0), (7.0, 9.0), (11.0, 4.0)];
const BAR_W: f32 = 2.0;
const BAR_BOTTOM: f32 = 13.0;
const BAR_RADIUS: f32 = 0.5;
/// Samples per pixel side for the coverage.
const SAMPLES: u32 = 8;

/// Whether (x, y) is inside the rect `(x0, y0, x1, y1)` whose corners have the radii
/// (top, bottom).
fn in_rounded(x: f32, y: f32, r: (f32, f32, f32, f32), top: f32, bottom: f32) -> bool {
    let (x0, y0, x1, y1) = r;
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let corner = |cx: f32, cy: f32, radius: f32| (x - cx).hypot(y - cy) <= radius;
    if y < y0 + top {
        if x < x0 + top && !corner(x0 + top, y0 + top, top) {
            return false;
        }
        if x > x1 - top && !corner(x1 - top, y0 + top, top) {
            return false;
        }
    }
    if y > y1 - bottom {
        if x < x0 + bottom && !corner(x0 + bottom, y1 - bottom, bottom) {
            return false;
        }
        if x > x1 - bottom && !corner(x1 - bottom, y1 - bottom, bottom) {
            return false;
        }
    }
    true
}

/// The tray icon as `size` x `size` straight-alpha RGBA, drawn by code: a rounded teal square with
/// three white bars (D9). Edges are anti-aliased by coverage (8 x 8 samples a pixel).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let size = size.max(1);
    let unit = GRID / size as f32;
    let square = (
        SQUARE_INSET,
        SQUARE_INSET,
        GRID - SQUARE_INSET,
        GRID - SQUARE_INSET,
    );
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut inside, mut rgb) = (0u32, [0.0f32; 3]);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * unit;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * unit;
                    if !in_rounded(x, y, square, SQUARE_RADIUS, SQUARE_RADIUS) {
                        continue;
                    }
                    inside += 1;
                    let bar = BARS.iter().any(|&(left, height)| {
                        in_rounded(
                            x,
                            y,
                            (left, BAR_BOTTOM - height, left + BAR_W, BAR_BOTTOM),
                            BAR_RADIUS,
                            0.0,
                        )
                    });
                    if bar {
                        for c in &mut rgb {
                            *c += 255.0;
                        }
                    } else {
                        let t = ((y - SQUARE_INSET) / (GRID - 2.0 * SQUARE_INSET)).clamp(0.0, 1.0);
                        for (c, (top, bottom)) in rgb.iter_mut().zip(TOP.iter().zip(&BOTTOM)) {
                            *c += top + (bottom - top) * t;
                        }
                    }
                }
            }
            if inside == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let n = inside as f32;
                let alpha = 255.0 * n / (SAMPLES * SAMPLES) as f32;
                out.extend(rgb.iter().map(|c| (c / n).round() as u8));
                out.push(alpha.round() as u8);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use lune_ui::paint::Rect;
    use lune_ui::testing::FakeText;
    use lune_ui::theme::DARK;
    use whipspot_core::engine::BAND_COUNT;
    use whipspot_core::player::{
        EqState, LyricsState, NowPlaying, OutputStatus, QueueState, RepeatMode,
    };
    use whipspot_core::provider::Track;
    use whipspot_core::skin::SkinRegistry;

    use super::*;

    fn track(name: &str, artist: &str) -> Track {
        Track {
            uri: "demo:1".into(),
            name: name.into(),
            artist: artist.into(),
            album: "Album".into(),
            duration_ms: 1000,
            images: vec![],
            year: None,
            album_ref: None,
            artists: vec![],
        }
    }

    fn state(current: Option<(&str, &str)>, play_state: PlayState, queued: usize) -> PlayerState {
        PlayerState {
            revision: 0,
            play_state,
            current: current.map(|(name, artist)| NowPlaying {
                track: track(name, artist),
                provider: "demo",
                source_format: None,
            }),
            position_ms: 0,
            position_at: Instant::now(),
            volume: 100,
            balance: 0,
            replay_gain: Default::default(),
            eq: EqState {
                enabled: false,
                preamp_db: 0.0,
                bands_db: [0.0; BAND_COUNT],
            },
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: QueueState {
                items: Arc::new((0..queued).map(|_| track("q", "q")).collect()),
                current: None,
                next: None,
            },
            providers: Vec::new(),
            output_summary: String::new(),
            output: OutputStatus::default(),
            lyrics: LyricsState::Off,
            lyrics_enabled: false,
            remote: None,
            power: Default::default(),
            update: Default::default(),
            app_version: String::new(),
            update_check: true,
        }
    }

    /// A library with no source: the names are empty, and `choose` fails into `Failed`.
    fn menu(folder: bool) -> TrayMenu {
        TrayMenu::new(
            SkinLibrary::new(None, Arc::new(SkinRegistry::new())),
            folder,
        )
    }

    fn item(rows: &[Row], text: &str) -> Row {
        rows.iter()
            .find(|r| r.text() == Some(text) && matches!(r, Row::Item { .. }))
            .unwrap_or_else(|| panic!("no row {text}"))
            .clone()
    }

    fn disabled(rows: &[Row], text: &str) -> bool {
        matches!(item(rows, text), Row::Item { disabled: true, .. })
    }

    fn texts(rows: &[Row]) -> Vec<Option<&str>> {
        rows.iter().map(Row::text).collect()
    }

    #[test]
    fn tray_rows_for_nothing_playing() {
        let rows = menu(true).rows(&state(None, PlayState::Stopped, 0));
        assert_eq!(
            rows[0],
            Row::Header {
                name: "Nothing playing".into(),
                status: "Choose something to play".into(),
                tone: Tone::Neutral
            }
        );
        assert_eq!(
            texts(&rows),
            [
                Some("Nothing playing"),
                None,
                Some("Play"),
                Some("Next"),
                Some("Previous"),
                None,
                Some("Skin"),
                None,
                Some("Show Whipspot"),
                Some("Quit Whipspot")
            ]
        );
        assert!(disabled(&rows, "Play"));
        assert!(disabled(&rows, "Next"));
        assert!(disabled(&rows, "Previous"));
        assert!(!disabled(&rows, "Skin"));
        assert!(matches!(
            item(&rows, "Skin"),
            Row::Item { chevron: true, .. }
        ));
    }

    #[test]
    fn tray_play_is_enabled_with_a_queue_and_nothing_current() {
        let rows = menu(true).rows(&state(None, PlayState::Stopped, 3));
        assert!(!disabled(&rows, "Play"));
        assert!(disabled(&rows, "Next"));
    }

    #[test]
    fn tray_rows_while_playing_loading_and_paused() {
        let m = menu(true);
        let playing = m.rows(&state(Some(("Song", "Band")), PlayState::Playing, 0));
        assert_eq!(
            playing[0],
            Row::Header {
                name: "Song".into(),
                status: "Band".into(),
                tone: Tone::Ok
            }
        );
        assert!(!disabled(&playing, "Pause"));
        assert!(!disabled(&playing, "Next"));
        assert!(!disabled(&playing, "Previous"));
        let loading = m.rows(&state(Some(("Song", "Band")), PlayState::Loading, 0));
        assert!(loading.iter().any(|r| r.text() == Some("Pause")));
        let paused = m.rows(&state(Some(("Song", "Band")), PlayState::Paused, 0));
        assert!(paused.iter().any(|r| r.text() == Some("Play")));
        assert!(matches!(
            paused[0],
            Row::Header {
                tone: Tone::Paused,
                ..
            }
        ));
    }

    #[test]
    fn tray_skin_row_is_disabled_without_a_folder() {
        let rows = menu(false).rows(&state(None, PlayState::Stopped, 0));
        assert!(disabled(&rows, "Skin"));
    }

    #[test]
    fn tray_skins_page_with_the_native_look_current_and_no_skins() {
        let mut m = menu(true);
        m.apply(Pick::OpenSkins);
        assert_eq!(m.page(), Page::Skins);
        let rows = m.rows(&state(None, PlayState::Stopped, 0));
        assert_eq!(
            texts(&rows),
            [
                Some("Back"),
                None,
                Some("Native look"),
                Some("No skins found")
            ]
        );
        assert!(matches!(
            item(&rows, "Native look"),
            Row::Item {
                glyph: Some(g),
                ..
            } if g == icon::CHECK
        ));
        assert!(disabled(&rows, "No skins found"));
        assert!(matches!(
            item(&rows, "Back"),
            Row::Item { glyph: Some(g), .. } if g == icon::ARROW_LEFT
        ));
    }

    /// A source that lists fixed names and keeps every read waiting (a skin stays "loading").
    struct Listed(Vec<String>);

    impl whipspot_core::skin::SkinSource for Listed {
        fn list(&self) -> Vec<String> {
            self.0.clone()
        }
        fn read(&self, _name: &str) -> Result<Vec<u8>, whipspot_core::skin::SkinError> {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            Err(whipspot_core::skin::SkinError::NotFound)
        }
    }

    #[test]
    fn tray_skins_page_lists_the_skins_and_marks_the_current_one() {
        let names = vec!["a.tsk".to_owned(), "b.tsk".to_owned()];
        let lib = SkinLibrary::new(Some(Arc::new(Listed(names))), Arc::new(SkinRegistry::new()));
        let mut m = TrayMenu::new(lib.clone(), true);
        let s = state(None, PlayState::Stopped, 0);
        m.apply(Pick::OpenSkins);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while lib.names().len() < 2 {
            assert!(std::time::Instant::now() < until, "no names");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // The native look is current.
        let rows = m.rows(&s);
        assert_eq!(
            texts(&rows),
            [
                Some("Back"),
                None,
                Some("Native look"),
                Some("a.tsk"),
                Some("b.tsk")
            ]
        );
        let marked = |rows: &[Row]| -> Vec<String> {
            rows.iter()
                .filter_map(|r| match r {
                    Row::Item {
                        text,
                        glyph: Some(g),
                        ..
                    } if *g == icon::CHECK => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(marked(&rows), ["Native look"]);
        // Choosing b.tsk: loading counts as current.
        let i = m.index_of(&s, &Pick::Skin("b.tsk".into())).expect("row");
        let (o, _) = m.pick_at(i, &s);
        assert_eq!(o, Outcome::Close);
        assert_eq!(marked(&m.rows(&s)), ["b.tsk"]);
    }

    #[test]
    fn tray_the_current_skin_is_marked_and_the_native_look_is_not() {
        assert!(is_current(
            &SkinStatus::Active {
                name: "a.tsk".into()
            },
            "a.tsk"
        ));
        assert!(is_current(
            &SkinStatus::Loading {
                name: "a.tsk".into()
            },
            "a.tsk"
        ));
        assert!(!is_current(&SkinStatus::Native, "a.tsk"));
        assert!(!is_current(
            &SkinStatus::Failed {
                name: "a.tsk".into(),
                text: "x"
            },
            "a.tsk"
        ));
    }

    #[test]
    fn tray_picks_map_to_actions_and_pages() {
        let mut m = menu(true);
        let s = state(Some(("Song", "Band")), PlayState::Playing, 0);
        let at = |m: &TrayMenu, p: &Pick| m.index_of(&s, p).expect("row");
        let (o, a) = m.pick_at(at(&m, &Pick::Toggle), &s);
        assert_eq!(o, Outcome::Close);
        assert!(matches!(a, Some(PlayerAction::Toggle)));
        let (o, a) = m.pick_at(at(&m, &Pick::Next), &s);
        assert_eq!(o, Outcome::Close);
        assert!(matches!(a, Some(PlayerAction::Next)));
        let (o, a) = m.pick_at(at(&m, &Pick::Previous), &s);
        assert_eq!(o, Outcome::Close);
        assert!(matches!(a, Some(PlayerAction::Previous)));
        // The skin row opens the skins page and the menu stays; Back returns.
        let (o, a) = m.pick_at(at(&m, &Pick::OpenSkins), &s);
        assert_eq!(
            (o, a.is_none(), m.page()),
            (Outcome::Stay, true, Page::Skins)
        );
        let (o, _) = m.pick_at(at(&m, &Pick::Back), &s);
        assert_eq!((o, m.page()), (Outcome::Stay, Page::Main));
        // Show and quit are requests for the host, once.
        let (o, a) = m.pick_at(at(&m, &Pick::Show), &s);
        assert!(a.is_none());
        assert_eq!(
            (o, m.take_request(), m.take_request()),
            (Outcome::Close, Some(TrayRequest::Show), None)
        );
        m.pick_at(at(&m, &Pick::Quit), &s);
        assert_eq!(m.take_request(), Some(TrayRequest::Quit));
    }

    #[test]
    fn tray_a_disabled_row_does_nothing() {
        let mut m = menu(true);
        let s = state(None, PlayState::Stopped, 0);
        let i = m.index_of(&s, &Pick::Next).expect("row");
        let (o, a) = m.pick_at(i, &s);
        assert_eq!(o, Outcome::Stay);
        assert!(a.is_none());
        // A separator or an index past the end too.
        assert!(m.pick_at(1, &s).1.is_none());
        assert!(m.pick_at(99, &s).1.is_none());
    }

    #[test]
    fn tray_choosing_the_native_look_closes() {
        let mut m = menu(true);
        let (o, a) = m.apply(Pick::Native);
        assert_eq!(o, Outcome::Close);
        assert!(a.is_none());
        assert_eq!(m.library.status(), SkinStatus::Native);
    }

    #[test]
    fn tray_opening_returns_to_the_main_page() {
        let mut m = menu(true);
        m.apply(Pick::OpenSkins);
        m.open();
        assert_eq!(m.page(), Page::Main);
    }

    #[test]
    fn tray_tooltip_is_the_product_alone_or_with_the_track() {
        assert_eq!(tooltip(&state(None, PlayState::Stopped, 0)), "Whipspot");
        assert_eq!(
            tooltip(&state(Some(("Song", "Band")), PlayState::Playing, 0)),
            "Whipspot \u{2014} Song \u{b7} Band"
        );
        assert_eq!(
            tooltip(&state(Some(("Song", "")), PlayState::Playing, 0)),
            "Whipspot \u{2014} Song"
        );
    }

    #[test]
    fn tray_tooltip_has_no_control_characters() {
        let tip = tooltip(&state(
            Some(("So\nng\t!", "Ba\r\nnd\u{7}")),
            PlayState::Playing,
            0,
        ));
        assert!(!tip.chars().any(char::is_control), "{tip:?}");
        assert!(tip.contains("Song") || tip.contains("So ng"));
    }

    #[test]
    fn tray_tooltip_is_short_enough_for_windows() {
        let long = "x".repeat(500);
        let tip = tooltip(&state(Some((&long, &long)), PlayState::Playing, 0));
        assert!(tip.encode_utf16().count() < 128, "{}", tip.len());
    }

    fn painter<'a>(text: &'a mut FakeText) -> Painter<'a, ()> {
        Painter::new(1.0, Rect::new(0.0, 0.0, 4096.0, 4096.0), text, None, DARK)
    }

    #[test]
    fn tray_the_long_title_menu_stays_within_the_cap_and_a_short_one_keeps_the_minimum() {
        let mut text = FakeText;
        let mut p = painter(&mut text);
        let m = menu(true);
        let long = state(
            Some((&"N".repeat(80), &"A".repeat(60))),
            PlayState::Playing,
            0,
        );
        let (w, _) = m.size(&mut p, &long);
        assert!(w <= MENU_MAX_W, "{w}");
        let short = state(Some(("Song", "Band")), PlayState::Playing, 0);
        let (w, h) = m.size(&mut p, &short);
        assert!(w >= 190.0, "{w}");
        assert!(h > 100.0);
        // The elided header ends in an ellipsis.
        let rows = m.fitted(&mut p, &long);
        let Row::Header { name, status, .. } = &rows[0] else {
            panic!("no header")
        };
        assert!(name.ends_with('\u{2026}') && status.ends_with('\u{2026}'));
    }

    #[test]
    fn tray_icon_size_and_alpha() {
        for size in [16u32, 32] {
            let px = icon_rgba(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            // The corner is transparent, the centre opaque.
            assert_eq!(px[3], 0);
            let c = ((size / 2 * size + size / 2) * 4) as usize;
            assert_eq!(px[c + 3], 255);
        }
    }

    #[test]
    fn tray_icon_bars_are_white_and_the_gaps_are_the_gradient() {
        let px = icon_rgba(32);
        let at = |x: u32, y: u32| {
            let i = ((y * 32 + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(8, 22), [255, 255, 255, 255]);
        let [r, g, b, a] = at(12, 22);
        assert_eq!(a, 255);
        assert!(g > r && b < g, "{r} {g} {b}");
    }

    #[test]
    fn tray_icon_edges_are_anti_aliased() {
        let px = icon_rgba(16);
        assert!(
            px.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255),
            "no partial coverage"
        );
    }
}
