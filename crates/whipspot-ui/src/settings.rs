//! The settings page (MD82, MD87): look, playback, sound, output, accounts, keyboard shortcuts and
//! about, as one scrolling column of glass cards. The page keeps no setting itself: a card press is
//! reported in [`SettingsOut`] and the shell applies it (theme, quality) or navigates (`open`).
//!
//! The card form (glass, title row, hairline, body) is shared with the accounts page.

use std::time::Duration;

use lune_ui::color::Rgba;
use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::{Sense, Ui};
use lune_ui::widgets::button::{ButtonKind, button_width};
use lune_ui::widgets::radio_group::RadioOption;
use lune_ui::widgets::scroll_area::{Bars, Extent, ScrollOptions};
use whipspot_core::engine::ReplayGainMode;
use whipspot_core::folders::ScanStatus;
use whipspot_core::player::{EqState, PlayerAction, PlayerHandle, PlayerState, ProviderEntry};
use whipspot_core::provider::model::LoginState;
use whipspot_core::text::display_text;

use crate::StreamQuality;
use crate::draw::{
    P, alpha, caps, caps_width, disc, glass, hgradient, led, outline, shadow, text, width,
};
use crate::equalizer::preset_of;
use crate::grid;
use crate::history::Route;
use crate::layout::{CONTENT_PAD, HEADING_H, heading_slot};
use crate::output::{self, OutputModel};
use crate::palette::CoverAccent;
use crate::skin::SkinStatus;
use crate::theme::{Labels, Look, ThemeId, theme};

/// The sentence under the Lyrics switch: what the lookup tells whom (MD78). It names no
/// music service.
pub(crate) const LYRICS_NOTE: &str = "Lyrics come from LRCLIB (lrclib.net), which learns the artist, title, album and length of what plays.";

/// The sentence under the Efficiency mode switch (D7): what it does, and that music is not slowed.
pub(crate) const EFFICIENCY_NOTE: &str =
    "While Whipspot is hidden, it lets the system save power. Playing music is never slowed down.";

/// The sentence under the quality choice while it differs from the running one (MD88).
const QUALITY_NOTE: &str = "Applies the next time Whipspot starts.";

/// The widest the column of cards gets.
pub(crate) const COLUMN_MAX: f32 = 760.0;
/// The gap between two cards, and the page's bottom margin.
pub(crate) const CARD_GAP: f32 = 16.0;
const BOTTOM_PAD: f32 = 36.0;
/// A card's inner padding at the sides and the bottom.
pub(crate) const PAD_X: f32 = 22.0;
pub(crate) const PAD_BOTTOM: f32 = 20.0;
/// Where a card's hairline and its body start, from the card's top.
const RULE_Y: f32 = 54.0;
pub(crate) const BODY_Y: f32 = 66.0;
/// A label/value row's pitch, and where the value column starts from the label's x.
const ROW_PITCH: f32 = 30.0;
pub(crate) const VALUE_DX: f32 = 160.0;
/// A theme tile's height and the gap between two.
const TILE_H: f32 = 112.0;
const TILE_GAP: f32 = 12.0;
/// The lyrics switch's track and its row.
const TOGGLE_W: f32 = 40.0;
const TOGGLE_H: f32 = 20.0;
const SWITCH_ROW_H: f32 = 28.0;
/// A radio row's height (the toolkit's).
const RADIO_H: f32 = 28.0;
/// A shortcut row's pitch and the gap between a keycap and its action.
const KEY_PITCH: f32 = 28.0;
const KEY_GAP: f32 = 14.0;
/// What the output card is assumed to need until it has been drawn once.
const OUTPUT_H_GUESS: f32 = 128.0;
/// The streaming qualities in the order of the radio group (the key is the index).
const QUALITIES: [StreamQuality; 3] = [
    StreamQuality::High,
    StreamQuality::Normal,
    StreamQuality::Low,
];

/// The longest skin name or folder path the Skins card shows, in characters.
const SKIN_TEXT_MAX: usize = 96;
/// The Skins card's status while a skin loads.
pub(crate) const SKIN_LOADING: &str = "Loading\u{2026}";

/// What the Skins card shows: the skins folder's names, the loader's status and the folder.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SkinsView<'a> {
    pub(crate) names: &'a [String],
    pub(crate) status: &'a SkinStatus,
    /// The skins folder as text; `None` when there is none.
    pub(crate) folder: Option<&'a str>,
}

/// A press on the Skins card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkinPick {
    /// Back to the native look.
    Native,
    /// Use the skin of this file name.
    Use(String),
    /// List the skins folder again.
    Refresh,
}

/// Which row of the Skins card is the current choice: `None` for the native look.
pub(crate) fn current_skin(status: &SkinStatus) -> Option<&str> {
    match status {
        SkinStatus::Loading { name } | SkinStatus::Active { name } => Some(name),
        SkinStatus::Native | SkinStatus::Failed { .. } => None,
    }
}

/// The Skins card's status line: loading, a fixed failure text, or nothing.
fn skin_status_line(status: &SkinStatus) -> Option<&'static str> {
    match status {
        SkinStatus::Loading { .. } => Some(SKIN_LOADING),
        SkinStatus::Failed { text, .. } => Some(text),
        SkinStatus::Native | SkinStatus::Active { .. } => None,
    }
}

/// What the Music folders card shows: the folders as text and where the scan stands.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FoldersView<'a> {
    pub(crate) list: &'a [String],
    pub(crate) status: ScanStatus,
}

/// A press on the Music folders card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FolderPick {
    /// Choose a folder to add.
    Add,
    /// Forget this folder.
    Remove(String),
    /// Scan every folder again.
    Rescan,
}

/// The longest folder path the card takes in, in characters (a longer one is cut at the end).
const FOLDER_TEXT_MAX: usize = 400;
/// The Music folders card's text for no folders.
pub(crate) const NO_FOLDERS: &str = "No music folders";
/// The ReplayGain choices in the order of the radio group (the key is the index).
const REPLAY_GAINS: [(ReplayGainMode, &str); 3] = [
    (ReplayGainMode::Off, "Off"),
    (ReplayGainMode::Track, "Track"),
    (ReplayGainMode::Album, "Album"),
];

/// The scan status line: "Scanning\u{2026} N files" while a scan runs, else "N tracks".
pub(crate) fn scan_line(status: ScanStatus) -> String {
    if status.scanning {
        let n = status.files_seen;
        format!(
            "Scanning\u{2026} {n} {}",
            if n == 1 { "file" } else { "files" }
        )
    } else {
        let n = status.tracks;
        format!("{n} {}", if n == 1 { "track" } else { "tracks" })
    }
}

/// `s` cut in the middle with an ellipsis so that `fits` accepts it: both ends stay readable. A
/// text that fits is returned as given.
pub(crate) fn elide_middle(s: &str, mut fits: impl FnMut(&str) -> bool) -> String {
    if fits(s) {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let build = |keep: usize| -> String {
        let head = keep.div_ceil(2);
        let tail = keep / 2;
        let mut out: String = chars[..head].iter().collect();
        out.push('\u{2026}');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    // The widest cut that fits (the width grows with `keep`).
    let (mut lo, mut hi) = (0, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&build(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

/// What the page shows of the shell's state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsView<'a> {
    /// The theme in use.
    pub(crate) theme: ThemeId,
    /// The user's latest streaming quality choice.
    pub(crate) quality: StreamQuality,
    /// The quality the running session started with (a different `quality` applies at the next
    /// start).
    pub(crate) quality_at_start: StreamQuality,
    /// The shortcut table's labels: (keys, what it does).
    pub(crate) shortcuts: &'static [(&'static str, &'static str)],
    /// The accents of the cover playing now, whatever the theme in use: the adaptive tile shows
    /// them even under A and B.
    pub(crate) cover: Option<CoverAccent>,
    /// The user's auto-hide switch (the top bar's icon shows it).
    pub(crate) auto_hide: bool,
    /// The Skins card; `None` hides it (no skin library, as in the demo).
    pub(crate) skins: Option<SkinsView<'a>>,
    /// The Music folders card; `None` hides it (no folders seam).
    pub(crate) folders: Option<FoldersView<'a>>,
    /// The app's version text for the About card.
    pub(crate) app_version: &'static str,
}

/// What the page asks of the shell in a frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SettingsOut {
    /// A theme card was pressed.
    pub(crate) theme: Option<ThemeId>,
    /// A streaming quality was chosen.
    pub(crate) quality: Option<StreamQuality>,
    /// A button that opens another page was pressed.
    pub(crate) open: Option<Route>,
    /// The top bar's auto-hide icon was pressed.
    pub(crate) toggle_auto_hide: bool,
    /// A press on the Skins card.
    pub(crate) skin: Option<SkinPick>,
    /// A press on the Music folders card.
    pub(crate) folder: Option<FolderPick>,
    /// The Music folders card shows a running scan: the shell asks for frames meanwhile.
    pub(crate) poll: bool,
    /// "Show tips again" was pressed: the shell starts the tour and goes back to the home view.
    pub(crate) restart_tour: bool,
}

/// The page's retained state.
#[derive(Debug)]
pub(crate) struct SettingsPane {
    /// The output card's state (M4's view, placed unchanged).
    output: OutputModel,
    /// The height the output view used last frame: its card is drawn before the view, so the card
    /// follows the view by a frame.
    output_h: f32,
}

impl Default for SettingsPane {
    fn default() -> Self {
        SettingsPane {
            output: OutputModel::default(),
            output_h: OUTPUT_H_GUESS,
        }
    }
}

/// The report of a press on theme card `index` (none for an index past the themes).
fn theme_out(index: usize) -> SettingsOut {
    SettingsOut {
        theme: ThemeId::ALL.get(index).copied(),
        ..SettingsOut::default()
    }
}

/// The note under the quality choice: shown only while the choice differs from the running one.
fn quality_note(choice: StreamQuality, at_start: StreamQuality) -> Option<&'static str> {
    (choice != at_start).then_some(QUALITY_NOTE)
}

/// The label of a quality in the radio group.
fn quality_label(q: StreamQuality) -> String {
    let word = match q {
        StreamQuality::High => "High",
        StreamQuality::Normal => "Normal",
        StreamQuality::Low => "Low",
    };
    format!("{word} ({} kbps)", q.kbps())
}

/// What a theme tile calls the theme, and says under it.
fn theme_text(id: ThemeId) -> (&'static str, &'static str) {
    match id {
        ThemeId::A => ("Teal", "Smoked glass"),
        ThemeId::B => ("Copper", "Brushed hi-fi"),
        ThemeId::C => ("Adaptive", "Colours from the cover"),
    }
}

/// The look theme `id` would have: the live cover accent drives the adaptive theme, the others
/// ignore it.
fn look_of(id: ThemeId, cover: Option<CoverAccent>) -> Look {
    Look::new(theme(id), cover)
}

/// The preset the equalizer's bands match exactly, else "Custom" (the equalizer page's rule).
fn preset_name(eq: &EqState) -> &'static str {
    preset_of(eq).map_or("Custom", |p| p.name())
}

/// The word for one login step's state.
fn state_word(state: Option<&LoginState>) -> &'static str {
    match state {
        None => "checking",
        Some(LoginState::LoggedIn) => "logged in",
        Some(LoginState::LoggedOut) => "logged out",
        Some(LoginState::Error { .. }) => "error",
    }
}

/// A provider's status: each login step titled with its state ("Web API: logged in"), joined by
/// " · ". Never an error's own message.
fn account_status(entry: &ProviderEntry) -> String {
    if entry.info.auth_steps.is_empty() {
        return "no login needed".to_owned();
    }
    entry
        .info
        .auth_steps
        .iter()
        .map(|step| {
            let state = entry
                .auth
                .as_ref()
                .map(|a| a.steps.iter().find(|s| s.id == step.id).map(|s| &s.state));
            let word = match state {
                None => state_word(None),
                Some(s) => state_word(s.or(Some(&LoginState::LoggedOut))),
            };
            format!("{}: {word}", step.label)
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// The column of cards of a page `rect`: its x, its width and the y of the first card (16 under
/// the heading slot).
pub(crate) fn column(rect: Rect) -> (f32, f32, f32) {
    let slot = heading_slot(rect);
    let w = (rect.w - 2.0 * CONTENT_PAD).clamp(0.0, COLUMN_MAX);
    (slot.x, w, slot.y + HEADING_H + CARD_GAP)
}

/// The page's scroll area options: the vertical bar only, the look's thumb.
pub(crate) fn scroll_options(identity: u64) -> ScrollOptions {
    ScrollOptions {
        bars: (Bars::Hidden, Bars::Auto),
        identity: Some(identity),
        ..ScrollOptions::default()
    }
}

/// Draws a card over `r`: the glass (B with its shadow), the title row and the hairline. The
/// body starts at `r.y + BODY_Y`.
pub(crate) fn card(p: &mut P<'_>, k: &Look, r: Rect, title: &str) {
    let radius = k.radii().card;
    let tracked = k.decor().labels == Labels::Tracked;
    if tracked {
        shadow(p, r, radius, 14.0, 0.8);
    }
    glass(p, k, r, radius, k.surface().panel_alpha, false);
    let x0 = r.x + PAD_X;
    if tracked {
        caps(
            p,
            &title.to_uppercase(),
            10.0,
            k.ink.text2,
            x0,
            r.y + 22.0,
            2.4,
        );
    } else {
        text(p, title, 15.0, k.ink.text, true, x0, r.y + 20.0, 0.0);
    }
    p.fill(
        Rect::new(r.x + 1.0, r.y + RULE_Y, r.w - 2.0, 1.0),
        0.0,
        k.ink.line_soft,
    );
}

/// A primary button right-aligned in the title row of card `r`; whether it was pressed.
pub(crate) fn header_button(ui: &mut Ui<'_>, r: Rect, id: &'static str, label: &str) -> bool {
    let bw = button_width(ui.painter(), label);
    let h = 32.0;
    let rect = Rect::new(r.right() - PAD_X - bw, r.y + 29.0 - h / 2.0, bw, h);
    ui.button(id, rect, label, ButtonKind::Primary).clicked
}

/// A row's label at `(x, y)` (the row's top): tracked caps on the look that sets labels so.
fn row_label(p: &mut P<'_>, k: &Look, s: &str, x: f32, y: f32) {
    if k.decor().labels == Labels::Tracked {
        caps(p, &s.to_uppercase(), 8.5, k.ink.muted, x, y + 8.0, 1.4);
    } else {
        text(p, s, 12.5, k.ink.muted, false, x, y + 6.0, 0.0);
    }
}

/// A row's value at `(x, y)` (the row's top), cut to `max_w`.
#[allow(clippy::too_many_arguments)]
fn row_value(
    p: &mut P<'_>,
    s: &str,
    color: Rgba,
    medium: bool,
    size: f32,
    x: f32,
    y: f32,
    max_w: f32,
) {
    text(p, s, size, color, medium, x, y + 5.0, max_w);
}

impl SettingsPane {
    /// Draws the page in `rect` (the page area: its heading slot is [`heading_slot`]).
    pub(crate) fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        k: &Look,
        view: &SettingsView<'_>,
        state: &PlayerState,
        player: &PlayerHandle,
    ) -> SettingsOut {
        let (x, w, top) = column(rect);
        let mut out = SettingsOut::default();
        let output_h = &mut self.output_h;
        let output_model = &mut self.output;
        ui.scroll_area("settings-page", rect, &scroll_options(20), |ui, _| {
            ui.painter().on(LayerId::Popup);
            grid::header(
                ui.painter(),
                k,
                heading_slot(rect),
                Route::Settings.title(),
                None,
            );
            let at = |y: f32| Rect::new(x, y, w, 0.0);
            let mut y = top;
            y = look_card(ui, k, view, at(y), &mut out) + CARD_GAP;
            if let Some(skins) = &view.skins {
                y = skins_card(ui, k, skins, at(y), &mut out) + CARD_GAP;
            }
            y = playback_card(ui, k, view, state, player, at(y), &mut out) + CARD_GAP;
            if let Some(folders) = &view.folders {
                y = folders_card(ui, k, folders, at(y), &mut out) + CARD_GAP;
            }
            y = sound_card(ui, k, state, player, at(y), &mut out) + CARD_GAP;
            let (bottom, used) = output_card(ui, k, output_model, *output_h, player, at(y));
            if (used - *output_h).abs() > 0.5 {
                *output_h = used;
                let next = ui.time() + Duration::from_millis(16);
                ui.request_repaint_at(next);
            }
            y = bottom + CARD_GAP;
            y = accounts_card(ui, k, state, at(y), &mut out) + CARD_GAP;
            y = shortcuts_card(ui, k, view.shortcuts, at(y)) + CARD_GAP;
            y = about_card(ui, k, view.app_version, at(y), &mut out);
            (
                (),
                Extent::new(f64::from(rect.w), f64::from(y + BOTTOM_PAD - rect.y)),
            )
        });
        out
    }
}

/// The Look card: the three themes as tiles. `at` gives the card's x, y and width. Returns the
/// card's bottom.
fn look_card(
    ui: &mut Ui<'_>,
    k: &Look,
    view: &SettingsView<'_>,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let r = Rect::new(at.x, at.y, at.w, BODY_Y + TILE_H + PAD_BOTTOM);
    card(ui.painter(), k, r, "Look");
    let n = ThemeId::ALL.len();
    let inner_w = r.w - 2.0 * PAD_X;
    let tw = ((inner_w - (n as f32 - 1.0) * TILE_GAP) / n as f32).max(0.0);
    for (i, id) in ThemeId::ALL.into_iter().enumerate() {
        let tile = Rect::new(
            r.x + PAD_X + i as f32 * (tw + TILE_GAP),
            r.y + BODY_Y,
            tw,
            TILE_H,
        );
        let resp = ui.interact(ui.id(("theme-card", i)), tile, Sense::CLICK_FOCUS);
        if resp.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        let selected = id == view.theme;
        let radius = k.radii().card * 0.75;
        let swatch_look = look_of(id, view.cover);
        let (name, blurb) = theme_text(id);
        let p = ui.painter();
        let fill = if resp.hovered {
            alpha(k.ink.wash, k.ink.wash[3].saturating_mul(2))
        } else {
            k.ink.wash
        };
        outline(p, tile, radius, fill, k.ink.line_soft);
        if selected {
            p.ring(tile, radius, 1.5, k.a, false);
        }
        if resp.focus_visible {
            p.ring(tile, radius, 1.5, k.hi, true);
        }
        // The swatch: the theme's own ramp left to right.
        let sw = Rect::new(tile.x + 10.0, tile.y + 10.0, (tile.w - 20.0).max(0.0), 48.0);
        let thumb = k.radii().thumb;
        p.push_clip_rounded(sw, thumb);
        let seg = sw.w / 3.0;
        for s in 0..3 {
            let t = s as f32 / 3.0;
            hgradient(
                p,
                Rect::new(sw.x + s as f32 * seg, sw.y, seg + 1.0, sw.h),
                swatch_look.at(t),
                swatch_look.at(t + 1.0 / 3.0),
            );
        }
        p.pop_clip();
        p.ring(sw, thumb, 1.0, alpha(k.ink.light, 0x28), false);
        // The name, the mark of the one in use, and what it is.
        let name_y = tile.y + 66.0;
        text(
            p,
            name,
            13.5,
            k.ink.text,
            true,
            tile.x + 12.0,
            name_y,
            tile.w - 36.0,
        );
        text(
            p,
            blurb,
            12.0,
            k.ink.muted,
            false,
            tile.x + 12.0,
            name_y + 20.0,
            tile.w - 24.0,
        );
        if selected {
            let c = (tile.right() - 16.0, name_y + 9.0);
            if k.decor().labels == Labels::Tracked {
                led(p, c, 7.0, k.a, k.hi, true);
            } else {
                disc(p, c, 7.0, k.a);
            }
        }
        if resp.clicked {
            out.theme = theme_out(i).theme;
        }
    }
    r.bottom()
}

/// The Skins card, beside the look: the native look and one row per skin in the skins folder,
/// each with Use and the current one marked, Refresh, a status line and the folder. Its texts
/// name no skin format. Returns the card's bottom.
fn skins_card(
    ui: &mut Ui<'_>,
    k: &Look,
    view: &SkinsView<'_>,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let status = skin_status_line(view.status);
    let rows = 1 + view.names.len();
    let extra = usize::from(status.is_some()) + usize::from(view.folder.is_some());
    let r = Rect::new(
        at.x,
        at.y,
        at.w,
        BODY_Y + (rows + extra) as f32 * ROW_PITCH + 12.0,
    );
    card(ui.painter(), k, r, "Skins");
    if header_button(ui, r, "skins-refresh", "Refresh") {
        out.skin = Some(SkinPick::Refresh);
    }
    let x0 = r.x + PAD_X;
    let right = r.right() - PAD_X;
    let current = current_skin(view.status);
    let mut y = r.y + BODY_Y;
    let entries = std::iter::once(None).chain(view.names.iter().map(|n| Some(n.as_str())));
    for (i, name) in entries.enumerate() {
        let selected = name == current;
        let label = match name {
            None => "Native look".to_owned(),
            Some(n) => display_text(n, SKIN_TEXT_MAX),
        };
        let bw = button_width(ui.painter(), "Use");
        let button = Rect::new(right - bw, y - 2.0, bw, 28.0);
        let p = ui.painter();
        if selected {
            let c = (x0 + 5.0, y + 13.0);
            if k.decor().labels == Labels::Tracked {
                led(p, c, 5.0, k.a, k.hi, true);
            } else {
                disc(p, c, 5.0, k.a);
            }
        }
        let label_w = (button.x - 12.0 - x0 - 18.0).max(0.0);
        let color = if selected { k.ink.text } else { k.ink.text2 };
        row_value(p, &label, color, selected, 13.0, x0 + 18.0, y, label_w);
        if selected {
            row_value(p, "In use", k.ink.muted, false, 12.5, button.x, y, 0.0);
        } else if ui
            .button(("skin-use", i), button, "Use", ButtonKind::Secondary)
            .clicked
        {
            out.skin = Some(match name {
                None => SkinPick::Native,
                Some(n) => SkinPick::Use(n.to_owned()),
            });
        }
        y += ROW_PITCH;
    }
    let p = ui.painter();
    if let Some(line) = status {
        row_value(p, line, k.ink.text2, false, 12.5, x0, y, right - x0);
        y += ROW_PITCH;
    }
    if let Some(folder) = view.folder {
        row_label(p, k, "Folder", x0, y);
        let folder = display_text(folder, SKIN_TEXT_MAX);
        let max = (right - x0 - VALUE_DX).max(0.0);
        row_value(p, &folder, k.ink.text2, false, 12.5, x0 + VALUE_DX, y, max);
    }
    r.bottom()
}

/// The Playback card: streaming quality, the Lyrics switch and the Efficiency mode switch.
/// Returns the card's bottom.
fn playback_card(
    ui: &mut Ui<'_>,
    k: &Look,
    view: &SettingsView<'_>,
    state: &PlayerState,
    player: &PlayerHandle,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let x0 = at.x + PAD_X;
    let inner_w = (at.w - 2.0 * PAD_X).max(0.0);
    let note = quality_note(view.quality, view.quality_at_start);
    let lyrics_note = Spec::plain(LYRICS_NOTE, Style::ui(12.5, k.ink.muted)).wrapped(inner_w);
    let lyrics_note_h = ui.painter().measure(&lyrics_note).1;
    let efficiency_note =
        Spec::plain(EFFICIENCY_NOTE, Style::ui(12.5, k.ink.muted)).wrapped(inner_w);
    let efficiency_note_h = ui.painter().measure(&efficiency_note).1;
    let radios_h = QUALITIES.len() as f32 * RADIO_H;
    let h = BODY_Y
        + radios_h
        + if note.is_some() { 26.0 } else { 0.0 }
        + 14.0
        + 1.0
        + 14.0
        + SWITCH_ROW_H
        + 6.0
        + lyrics_note_h
        + 14.0
        + 1.0
        + 14.0
        + SWITCH_ROW_H
        + 6.0
        + efficiency_note_h
        + PAD_BOTTOM;
    let r = Rect::new(at.x, at.y, at.w, h);
    card(ui.painter(), k, r, "Playback");

    let mut y = r.y + BODY_Y;
    row_label(ui.painter(), k, "Streaming quality", x0, y);
    let labels: Vec<String> = QUALITIES.iter().map(|q| quality_label(*q)).collect();
    let options: Vec<RadioOption<'_>> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| RadioOption {
            key: i as u64,
            label,
            enabled: true,
        })
        .collect();
    let mut selected = QUALITIES
        .iter()
        .position(|q| *q == view.quality)
        .unwrap_or(0) as u64;
    let vx = x0 + VALUE_DX;
    let group = ui.radio_group(
        "quality",
        // The rows pad their circles by 8 px, so the circles start at the value column.
        Rect::new(vx - 8.0, y, (x0 + inner_w - vx + 8.0).max(0.0), 0.0),
        &options,
        &mut selected,
        "Streaming quality",
    );
    if group.changed {
        out.quality = QUALITIES.get(selected as usize).copied();
    }
    y += radios_h;
    if let Some(note) = note {
        text(
            ui.painter(),
            note,
            12.5,
            k.ink.muted,
            false,
            vx,
            y + 4.0,
            0.0,
        );
        y += 26.0;
    }

    // The hairline, then the Lyrics switch.
    y += 14.0;
    ui.painter()
        .fill(Rect::new(x0, y, inner_w, 1.0), 0.0, k.ink.line_soft);
    y += 15.0;
    text(
        ui.painter(),
        "Lyrics",
        13.5,
        k.ink.text,
        true,
        x0,
        y + (SWITCH_ROW_H - 13.5 * 1.3) / 2.0,
        0.0,
    );
    let mut on = state.lyrics_enabled;
    let track = Rect::new(
        x0 + inner_w - TOGGLE_W,
        y + (SWITCH_ROW_H - TOGGLE_H) / 2.0,
        TOGGLE_W,
        TOGGLE_H,
    );
    if ui.toggle("lyrics-toggle", track, &mut on, "Lyrics").changed {
        player.dispatch(PlayerAction::SetLyrics(on));
    }
    ui.painter().text(&lyrics_note, x0, y + SWITCH_ROW_H + 6.0);

    // The hairline, then the Efficiency mode switch (D7).
    y += SWITCH_ROW_H + 6.0 + lyrics_note_h + 14.0;
    ui.painter()
        .fill(Rect::new(x0, y, inner_w, 1.0), 0.0, k.ink.line_soft);
    y += 15.0;
    text(
        ui.painter(),
        "Efficiency mode",
        13.5,
        k.ink.text,
        true,
        x0,
        y + (SWITCH_ROW_H - 13.5 * 1.3) / 2.0,
        0.0,
    );
    let mut on = state.power.enabled;
    let track = Rect::new(
        x0 + inner_w - TOGGLE_W,
        y + (SWITCH_ROW_H - TOGGLE_H) / 2.0,
        TOGGLE_W,
        TOGGLE_H,
    );
    if ui
        .toggle("efficiency-toggle", track, &mut on, "Efficiency mode")
        .changed
    {
        player.dispatch(PlayerAction::SetEfficiency(on));
    }
    ui.painter()
        .text(&efficiency_note, x0, y + SWITCH_ROW_H + 6.0);
    r.bottom()
}

/// The Music folders card, under Playback: one row per folder (the path cut in the middle) with
/// Remove, the scan status with Rescan, and Add folder in the title row. Its texts are fixed
/// except the paths and the numbers. Returns the card's bottom.
fn folders_card(
    ui: &mut Ui<'_>,
    k: &Look,
    view: &FoldersView<'_>,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let rows = view.list.len().max(1) + 1;
    let r = Rect::new(at.x, at.y, at.w, BODY_Y + rows as f32 * ROW_PITCH + 12.0);
    card(ui.painter(), k, r, "Music folders");
    if header_button(ui, r, "folders-add", "Add folder") {
        out.folder = Some(FolderPick::Add);
    }
    let x0 = r.x + PAD_X;
    let right = r.right() - PAD_X;
    let mut y = r.y + BODY_Y;
    if view.list.is_empty() {
        row_value(
            ui.painter(),
            NO_FOLDERS,
            k.ink.text2,
            false,
            12.5,
            x0,
            y,
            0.0,
        );
        y += ROW_PITCH;
    }
    for (i, folder) in view.list.iter().enumerate() {
        let bw = button_width(ui.painter(), "Remove");
        let button = Rect::new(right - bw, y - 2.0, bw, 28.0);
        let room = (button.x - 12.0 - x0).max(0.0);
        let shown = display_text(folder, FOLDER_TEXT_MAX);
        let cut = elide_middle(&shown, |t| width(ui.painter(), t, 13.0, false) <= room);
        row_value(ui.painter(), &cut, k.ink.text, false, 13.0, x0, y, 0.0);
        if ui
            .button(
                ("folder-remove", i),
                button,
                "Remove",
                ButtonKind::Secondary,
            )
            .clicked
        {
            out.folder = Some(FolderPick::Remove(folder.clone()));
        }
        y += ROW_PITCH;
    }
    let bw = button_width(ui.painter(), "Rescan");
    let button = Rect::new(right - bw, y - 2.0, bw, 28.0);
    row_value(
        ui.painter(),
        &scan_line(view.status),
        k.ink.text2,
        false,
        12.5,
        x0,
        y,
        (button.x - 12.0 - x0).max(0.0),
    );
    if ui
        .button("folders-rescan", button, "Rescan", ButtonKind::Secondary)
        .clicked
    {
        out.folder = Some(FolderPick::Rescan);
    }
    out.poll |= view.status.scanning;
    r.bottom()
}

/// The Sound card: the equalizer's state and preset, the ReplayGain choice and the button to the
/// page. Returns the card's bottom.
fn sound_card(
    ui: &mut Ui<'_>,
    k: &Look,
    state: &PlayerState,
    player: &PlayerHandle,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let rg_h = REPLAY_GAINS.len() as f32 * RADIO_H;
    let r = Rect::new(
        at.x,
        at.y,
        at.w,
        BODY_Y + 2.0 * ROW_PITCH + 2.0 + 1.0 + 12.0 + rg_h + 12.0,
    );
    card(ui.painter(), k, r, "Sound");
    if header_button(ui, r, "to-equalizer", "Equalizer") {
        out.open = Some(Route::Equalizer);
    }
    let (x0, y) = (r.x + PAD_X, r.y + BODY_Y);
    let p = ui.painter();
    row_label(p, k, "Equalizer", x0, y);
    let on = if state.eq.enabled { "On" } else { "Off" };
    row_value(p, on, k.ink.text, false, 13.0, x0 + VALUE_DX, y, 0.0);
    let y = y + ROW_PITCH;
    row_label(p, k, "Preset", x0, y);
    let name = preset_name(&state.eq);
    row_value(p, name, k.ink.text, false, 13.0, x0 + VALUE_DX, y, 0.0);

    // The hairline, then the ReplayGain choice (Off keeps the samples untouched).
    let inner_w = (r.w - 2.0 * PAD_X).max(0.0);
    let y = y + ROW_PITCH + 2.0;
    ui.painter()
        .fill(Rect::new(x0, y, inner_w, 1.0), 0.0, k.ink.line_soft);
    let y = y + 13.0;
    row_label(ui.painter(), k, "ReplayGain", x0, y);
    let options: Vec<RadioOption<'_>> = REPLAY_GAINS
        .iter()
        .enumerate()
        .map(|(i, (_, label))| RadioOption {
            key: i as u64,
            label,
            enabled: true,
        })
        .collect();
    let mut selected = REPLAY_GAINS
        .iter()
        .position(|(m, _)| *m == state.replay_gain)
        .unwrap_or(0) as u64;
    let vx = x0 + VALUE_DX;
    let group = ui.radio_group(
        "replay-gain",
        Rect::new(vx - 8.0, y, (x0 + inner_w - vx + 8.0).max(0.0), 0.0),
        &options,
        &mut selected,
        "ReplayGain",
    );
    if group.changed
        && let Some((mode, _)) = REPLAY_GAINS.get(selected as usize)
    {
        player.dispatch(PlayerAction::SetReplayGain(*mode));
    }
    r.bottom()
}

/// The Output card: M4's view placed unchanged. Returns the card's bottom and the height the
/// view used.
fn output_card(
    ui: &mut Ui<'_>,
    k: &Look,
    model: &mut OutputModel,
    assumed_h: f32,
    player: &PlayerHandle,
    at: Rect,
) -> (f32, f32) {
    let r = Rect::new(at.x, at.y, at.w, BODY_Y + assumed_h + PAD_BOTTOM);
    card(ui.painter(), k, r, "Output");
    let used = output::view(
        ui,
        Rect::new(r.x + PAD_X, r.y + BODY_Y, (r.w - 2.0 * PAD_X).max(0.0), 0.0),
        model,
        player,
    );
    (r.bottom(), used)
}

/// The Accounts card: one status row per provider and the button to the Accounts page. Returns
/// the card's bottom.
fn accounts_card(
    ui: &mut Ui<'_>,
    k: &Look,
    state: &PlayerState,
    at: Rect,
    out: &mut SettingsOut,
) -> f32 {
    let rows = state.providers.len().max(1) as f32;
    let r = Rect::new(at.x, at.y, at.w, BODY_Y + rows * ROW_PITCH + 12.0);
    card(ui.painter(), k, r, "Accounts");
    if header_button(ui, r, "to-accounts", "Accounts") {
        out.open = Some(Route::Account);
    }
    let x0 = r.x + PAD_X;
    let mut y = r.y + BODY_Y;
    if state.providers.is_empty() {
        row_value(
            ui.painter(),
            "No accounts to log in to.",
            k.ink.text2,
            false,
            12.5,
            x0,
            y,
            0.0,
        );
    }
    let max = (r.right() - PAD_X - x0 - VALUE_DX).max(0.0);
    for entry in &state.providers {
        let p = ui.painter();
        row_value(
            p,
            entry.info.name,
            k.ink.text,
            true,
            13.0,
            x0,
            y,
            VALUE_DX - 12.0,
        );
        row_value(
            p,
            &account_status(entry),
            k.ink.text2,
            false,
            12.5,
            x0 + VALUE_DX,
            y,
            max,
        );
        y += ROW_PITCH;
    }
    r.bottom()
}

/// One keycap with `keys` at `(x, y)` (the row's top, `KEY_PITCH` high): a pill, or on a look
/// with tracked labels the small outlined caps form of the top bar. Returns its width; nothing
/// is drawn when `draw` is false (a measure).
fn keycap(p: &mut P<'_>, k: &Look, keys: &str, x: f32, y: f32, draw: bool) -> f32 {
    if k.decor().labels == Labels::Tracked {
        let label = keys.to_uppercase();
        let w = caps_width(p, &label, 8.0, 1.0) + 12.0;
        if draw {
            let r = Rect::new(x, y + (KEY_PITCH - 18.0) / 2.0, w, 18.0);
            p.ring(r, 5.0, 1.0, k.ink.line, false);
            caps(p, &label, 8.0, k.ink.muted, r.x + 6.0, r.y + 4.5, 1.0);
        }
        w
    } else {
        let w = width(p, keys, 11.5, true) + 14.0;
        if draw {
            let r = Rect::new(x, y + (KEY_PITCH - 22.0) / 2.0, w, 22.0);
            outline(p, r, 5.0, k.ink.wash, k.ink.line_soft);
            text(p, keys, 11.5, k.ink.text2, true, r.x + 7.0, r.y + 3.5, 0.0);
        }
        w
    }
}

/// The Keyboard shortcuts card: two column pairs of keycap and action, the actions of a pair on
/// one vertical line. Returns the card's bottom.
fn shortcuts_card(ui: &mut Ui<'_>, k: &Look, shortcuts: &[(&str, &str)], at: Rect) -> f32 {
    let rows = shortcuts.len().div_ceil(2).max(1);
    let r = Rect::new(at.x, at.y, at.w, BODY_Y + rows as f32 * KEY_PITCH + 12.0);
    card(ui.painter(), k, r, "Keyboard shortcuts");
    let inner_w = (r.w - 2.0 * PAD_X).max(0.0);
    let pair_w = inner_w / 2.0;
    let p = ui.painter();
    for (pair, entries) in shortcuts.chunks(rows).enumerate() {
        let px = r.x + PAD_X + pair as f32 * pair_w;
        let widest = entries
            .iter()
            .map(|(keys, _)| keycap(p, k, keys, px, 0.0, false))
            .fold(0.0, f32::max);
        for (i, (keys, what)) in entries.iter().enumerate() {
            let y = r.y + BODY_Y + i as f32 * KEY_PITCH;
            keycap(p, k, keys, px, y, true);
            let ax = px + widest + KEY_GAP;
            text(
                p,
                what,
                12.5,
                k.ink.text,
                false,
                ax,
                y + (KEY_PITCH - 12.5 * 1.3) / 2.0,
                (px + pair_w - 12.0 - ax).max(0.0),
            );
        }
    }
    r.bottom()
}

/// The About card. Returns the card's bottom.
fn about_card(ui: &mut Ui<'_>, k: &Look, version: &str, at: Rect, out: &mut SettingsOut) -> f32 {
    let r = Rect::new(
        at.x,
        at.y,
        at.w,
        BODY_Y + 3.0 * ROW_PITCH + 18.0 + PAD_BOTTOM,
    );
    card(ui.painter(), k, r, "About");
    if header_button(ui, r, "show-tips", "Show tips again") {
        out.restart_tour = true;
    }
    let x0 = r.x + PAD_X;
    let mut y = r.y + BODY_Y;
    let p = ui.painter();
    row_value(p, "Whipspot", k.ink.text, true, 15.0, x0, y - 2.0, 0.0);
    y += ROW_PITCH;
    for (label, value) in [("Version", version), ("Licence", "Apache-2.0")] {
        row_label(p, k, label, x0, y);
        row_value(p, value, k.ink.text, false, 13.0, x0 + VALUE_DX, y, 0.0);
        y += ROW_PITCH;
    }
    text(
        p,
        "Not affiliated with any music service.",
        12.5,
        k.ink.muted,
        false,
        x0,
        y + 2.0,
        0.0,
    );
    r.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_folder_path_is_cut_in_the_middle_and_keeps_both_ends() {
        let path = "C:\\Users\\someone\\Music\\Lossless\\Archive";
        let fits = |n: usize| move |t: &str| t.chars().count() <= n;
        // A path that fits is shown as given.
        assert_eq!(elide_middle(path, fits(path.chars().count())), path);
        let cut = elide_middle(path, fits(24));
        assert!(cut.chars().count() <= 24, "{cut}");
        assert!(cut.starts_with("C:\\Users"), "{cut}");
        assert!(cut.ends_with("Archive"), "{cut}");
        assert!(cut.contains('\u{2026}'));
        // The cut is the widest that fits.
        assert_eq!(cut.chars().count(), 24);
        // Nothing fits: the ellipsis alone, never a panic; wide chars cut on char boundaries.
        assert_eq!(elide_middle(path, |_| false), "\u{2026}");
        let wide = "\u{4e2d}\u{6587}\u{97f3}\u{4e50}\u{76ee}\u{5f55}";
        assert!(elide_middle(wide, fits(4)).chars().count() <= 4);
    }

    #[test]
    fn the_scan_line_counts_files_while_scanning_and_tracks_after() {
        let idle = |tracks| ScanStatus {
            scanning: false,
            files_seen: 99,
            tracks,
        };
        assert_eq!(scan_line(idle(0)), "0 tracks");
        assert_eq!(scan_line(idle(1)), "1 track");
        assert_eq!(scan_line(idle(1234)), "1234 tracks");
        let busy = |files_seen| ScanStatus {
            scanning: true,
            files_seen,
            tracks: 7,
        };
        assert_eq!(scan_line(busy(1)), "Scanning\u{2026} 1 file");
        assert_eq!(scan_line(busy(42)), "Scanning\u{2026} 42 files");
    }

    #[test]
    fn a_theme_card_press_carries_that_theme() {
        for (i, id) in ThemeId::ALL.into_iter().enumerate() {
            assert_eq!(
                theme_out(i),
                SettingsOut {
                    theme: Some(id),
                    ..SettingsOut::default()
                }
            );
        }
        assert_eq!(theme_out(ThemeId::ALL.len()), SettingsOut::default());
    }

    #[test]
    fn the_quality_note_shows_only_when_the_two_differ() {
        for a in QUALITIES {
            for b in QUALITIES {
                assert_eq!(quality_note(a, b).is_some(), a != b, "{a:?} vs {b:?}");
            }
        }
        assert_eq!(
            quality_note(StreamQuality::Low, StreamQuality::High),
            Some("Applies the next time Whipspot starts.")
        );
    }

    #[test]
    fn the_efficiency_note_names_no_os_and_says_music_is_not_slowed() {
        assert!(!EFFICIENCY_NOTE.is_empty());
        for os in ["Windows", "macOS", "Mac", "Linux"] {
            assert!(!EFFICIENCY_NOTE.contains(os), "{EFFICIENCY_NOTE}");
        }
        assert!(EFFICIENCY_NOTE.contains("never slowed down"));
    }

    #[test]
    fn quality_labels_state_the_cap() {
        assert_eq!(quality_label(StreamQuality::High), "High (320 kbps)");
        assert_eq!(quality_label(StreamQuality::Normal), "Normal (160 kbps)");
        assert_eq!(quality_label(StreamQuality::Low), "Low (96 kbps)");
    }

    #[test]
    fn the_sound_card_names_the_matching_preset_or_custom() {
        let mut eq = EqState {
            enabled: true,
            preamp_db: 0.0,
            bands_db: crate::equalizer::Preset::Bass.bands(),
        };
        assert_eq!(preset_name(&eq), "Bass");
        eq.bands_db[3] += 0.5;
        assert_eq!(preset_name(&eq), "Custom");
        eq.bands_db = [0.0; whipspot_core::engine::BAND_COUNT];
        assert_eq!(preset_name(&eq), "Flat");
    }

    #[test]
    fn the_adaptive_tile_follows_the_live_cover_under_any_theme() {
        let cover = CoverAccent {
            a: [200, 40, 40, 255],
            b: [40, 40, 200, 255],
        };
        let with = look_of(ThemeId::C, Some(cover));
        assert_eq!((with.a, with.b), (cover.a, cover.b));
        // The same tile is drawn whichever theme is in use: it depends on the cover alone.
        assert_eq!(look_of(ThemeId::C, Some(cover)).a, with.a);
        let without = look_of(ThemeId::C, None);
        assert_ne!(without.a, with.a);
        // A and B ignore the cover.
        for id in [ThemeId::A, ThemeId::B] {
            assert_eq!(look_of(id, Some(cover)).a, look_of(id, None).a);
        }
    }

    #[test]
    fn state_words_are_fixed_texts() {
        assert_eq!(state_word(None), "checking");
        assert_eq!(state_word(Some(&LoginState::LoggedIn)), "logged in");
        assert_eq!(state_word(Some(&LoginState::LoggedOut)), "logged out");
    }

    #[test]
    fn the_skins_card_marks_the_choice_and_states_loading_or_the_fixed_failure() {
        let name = || "blue.skin".to_owned();
        assert_eq!(current_skin(&SkinStatus::Native), None);
        assert_eq!(
            current_skin(&SkinStatus::Active { name: name() }),
            Some("blue.skin")
        );
        assert_eq!(
            current_skin(&SkinStatus::Loading { name: name() }),
            Some("blue.skin")
        );
        let failed = SkinStatus::Failed {
            name: name(),
            text: crate::skin::OPEN_FAILED,
        };
        // A failure leaves the native look in use.
        assert_eq!(current_skin(&failed), None);
        assert_eq!(skin_status_line(&failed), Some(crate::skin::OPEN_FAILED));
        assert_eq!(
            skin_status_line(&SkinStatus::Loading { name: name() }),
            Some(SKIN_LOADING)
        );
        assert_eq!(skin_status_line(&SkinStatus::Native), None);
        assert_eq!(skin_status_line(&SkinStatus::Active { name: name() }), None);
    }

    #[test]
    fn the_column_starts_sixteen_under_the_heading_slot() {
        let rect = Rect::new(100.0, 200.0, 1000.0, 600.0);
        let (x, w, top) = column(rect);
        let slot = heading_slot(rect);
        assert_eq!(x, slot.x);
        assert_eq!(w, COLUMN_MAX);
        assert_eq!(top, slot.y + slot.h + 16.0);
    }
}
