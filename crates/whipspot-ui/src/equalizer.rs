//! The equalizer pane (MD82, MD83, MD84, MD85): ten band faders and a preamp over the core's EQ
//! actions, the preset table, and the EQ state, headroom, limiter and clip indicators.
//!
//! The faders are drawn on `ui.interact` with a drag sense (the toolkit's slider is horizontal
//! only), like the media control's volume wedge. A value is dispatched only when its snapped value
//! (0.5 dB steps, so exactly 0.0 is reachable and the bypass stays exact) differs from the one
//! shown; a value just set is held until the player's state shows it too.
//!
//! The header also carries the balance control: a horizontal track drawn like a fader turned on
//! its side, -100 (left) to 100 (right) with a detent at the centre. It is not part of the EQ, so
//! it stays live and undimmed whatever the EQ switch says (balance acts after the tap, MD29).

use std::time::Duration;

use lune_ui::color::{Rgba, mix};
use lune_ui::input::CursorIcon;
use lune_ui::paint::Rect;
use lune_ui::ui::{Sense, Ui};
use whipspot_core::diagnostics::Stage;
use whipspot_core::engine::BAND_COUNT;
use whipspot_core::player::{EqState, PlayerAction, PlayerHandle, PlayerState};

use crate::draw::{
    P, alpha, caps, caps_width, disc, hgradient, led, mono, mono_width, outline, shadow, text,
    text_c, text_r, vgradient, well, width,
};
use crate::grid;
use crate::history::Route;
use crate::layout::{BODY_TOP, heading_slot};
use crate::signal::warn;
use crate::theme::{Indicator, Look, Ring};

/// The indicator states the demo forces (the dumps only): whether the Limiter and Clip LEDs are lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EqMeters {
    pub limiter: bool,
    pub clip: bool,
}

// ---------------------------------------------------------------------------------------------
// Constants.

/// The faders' range, dB.
const RANGE_DB: f32 = 12.0;
/// The faders' step, dB (so 0.0 is reached exactly).
const STEP_DB: f32 = 0.5;
/// One scroll notch moves a fader by one step; a notch is this many scroll pixels.
const SCROLL_PIXELS_PER_NOTCH: f32 = 40.0;
/// A value just set is shown until the state shows it too, at most this long.
const HOLD: Duration = Duration::from_millis(600);
/// An indicator stays lit this long after its count last rose (MD85).
const LIT_FOR: Duration = Duration::from_secs(2);
/// The pane asks for a repaint this often while it is visible (MD85).
const REPAINT: Duration = Duration::from_millis(250);
/// A frame gap longer than this means the pane was hidden: the hold logic starts over.
const HIDDEN_GAP: Duration = Duration::from_secs(1);

/// The band labels (`BAND_HZ`, shortened).
const BAND_LABELS: [&str; BAND_COUNT] = [
    "60", "170", "310", "600", "1K", "3K", "6K", "12K", "14K", "16K",
];

/// The smallest and largest spacing between two faders, px.
const MIN_SPACING: f32 = 18.0;
const MAX_SPACING: f32 = 80.0;
/// The widest the fader track may be (at 1440 x 900), px.
const MAX_TRACK: f32 = 300.0;
/// The shortest the track is drawn when the page leaves less.
const MIN_TRACK: f32 = 40.0;
/// The track the card keeps by rising before it gives up height.
const COMFY_TRACK: f32 = 60.0;
/// The card's preferred top under the page area's top (the albums grid's first row).
const CARDS_TOP: f32 = 72.0;
/// Inside the card: above the track, and under it down to the card's bottom (the label row, the
/// readout row and the padding).
const CARD_TOP_PAD: f32 = 14.0;
const CARD_BOTTOM: f32 = 52.0;
/// The dB scale column's width and the card's side padding.
const SCALE_W: f32 = 34.0;
const CARD_SIDE: f32 = 16.0;
/// The indicator row's height and its distance under the card.
const IND_H: f32 = 16.0;
const IND_GAP: f32 = 8.0;
/// Under this spacing the value readouts under the labels are dropped.
const READOUT_MIN_SPACING: f32 = 26.0;
/// The groove's width, px.
const GROOVE_W: f32 = 4.0;
/// Header controls.
const TOGGLE_W: f32 = 40.0;
const TOGGLE_H: f32 = 20.0;
const COMBO_W: f32 = 128.0;
const COMBO_H: f32 = 30.0;
/// Where the heading slot's title is centred, from the slot's top.
const TITLE_CY: f32 = 20.0;
/// The balance range either way, the pointer's snap to the centre, and one scroll notch.
const BALANCE_MAX: i32 = 100;
const BALANCE_SNAP: i32 = 4;
const BALANCE_NOTCH: i32 = 5;
/// The balance track's lengths, longest first (the header takes the first that leaves room), the
/// gap between the group and the Preset label, and the gaps inside the group.
const BALANCE_TRACKS: [f32; 3] = [112.0, 88.0, 64.0];
const BALANCE_GROUP_GAP: f32 = 20.0;
const BALANCE_LABEL_GAP: f32 = 10.0;
const BALANCE_READOUT_GAP: f32 = 8.0;
/// The least room between the page title and the balance group, px.
const BALANCE_TITLE_GAP: f32 = 24.0;
/// The balance hit area's height and the handle's reach beyond the track's ends.
const BALANCE_HIT_H: f32 = 24.0;
const BALANCE_REACH: f32 = 7.0;

// ---------------------------------------------------------------------------------------------
// Presets (MD84).

/// A named band set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    Flat,
    Bass,
    Treble,
    Vocal,
    Loudness,
    Soft,
}

impl Preset {
    /// Every preset, in list order.
    pub(crate) const ALL: [Preset; 6] = [
        Preset::Flat,
        Preset::Bass,
        Preset::Treble,
        Preset::Vocal,
        Preset::Loudness,
        Preset::Soft,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Preset::Flat => "Flat",
            Preset::Bass => "Bass",
            Preset::Treble => "Treble",
            Preset::Vocal => "Vocal",
            Preset::Loudness => "Loudness",
            Preset::Soft => "Soft",
        }
    }

    /// The ten band gains, dB.
    pub(crate) fn bands(self) -> [f32; BAND_COUNT] {
        match self {
            Preset::Flat => [0.0; BAND_COUNT],
            Preset::Bass => [6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            Preset::Treble => [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 5.5, 6.0],
            Preset::Vocal => [-2.0, -1.0, 0.0, 2.0, 3.5, 3.0, 1.5, 0.0, 0.0, -1.0],
            Preset::Loudness => [5.0, 3.5, 1.0, 0.0, -1.0, 0.0, 1.0, 3.0, 3.5, 4.0],
            Preset::Soft => [0.0, 0.0, 0.0, 0.0, 0.0, -1.0, -2.0, -3.0, -3.5, -4.0],
        }
    }
}

/// The preset whose ten bands equal `eq`'s exactly, `None` for a custom curve.
pub(crate) fn preset_of(eq: &EqState) -> Option<Preset> {
    Preset::ALL.into_iter().find(|p| p.bands() == eq.bands_db)
}

/// The bands (index, gain) that applying `target` changes from `current`.
fn preset_diff(current: &[f32; BAND_COUNT], target: Preset) -> Vec<(usize, f32)> {
    target
        .bands()
        .into_iter()
        .enumerate()
        .filter(|(i, g)| current[*i] != *g)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Pure helpers.

/// `v` snapped to the fader step and clamped to the range.
pub(crate) fn snap(v: f32) -> f32 {
    ((v / STEP_DB).round() * STEP_DB).clamp(-RANGE_DB, RANGE_DB)
}

/// The value (dB, unsnapped) at pointer height `y` on a track from `top` and `h` tall.
pub(crate) fn value_at(y: f32, top: f32, h: f32) -> f32 {
    if h <= 0.0 {
        return 0.0;
    }
    let f = ((y - top) / h).clamp(0.0, 1.0);
    RANGE_DB - 2.0 * RANGE_DB * f
}

/// The height of value `v` on a track from `top` and `h` tall (the inverse of [`value_at`]).
fn y_of(v: f32, top: f32, h: f32) -> f32 {
    top + h * (RANGE_DB - v) / (2.0 * RANGE_DB)
}

/// The balance (-100..=100) at pointer `x` on a track from `left` and `w` wide: linear, with the
/// centre's detent (within [`BALANCE_SNAP`] of 0 it is exactly 0, so the gain stage stays bypassed).
pub(crate) fn balance_at(x: f32, left: f32, w: f32) -> i8 {
    if w <= 0.0 {
        return 0;
    }
    let f = ((x - left) / w).clamp(0.0, 1.0);
    let v = (-BALANCE_MAX as f32 + 2.0 * BALANCE_MAX as f32 * f).round() as i32;
    if v.abs() <= BALANCE_SNAP { 0 } else { v as i8 }
}

/// The x of balance `v` on a track from `left` and `w` wide (the inverse of [`balance_at`]).
fn x_of_balance(v: i8, left: f32, w: f32) -> f32 {
    left + w * (f32::from(v) + BALANCE_MAX as f32) / (2.0 * BALANCE_MAX as f32)
}

/// A balance as a readout: "C", "L 40", "R 25".
pub(crate) fn fmt_balance(v: i8) -> String {
    match v {
        0 => "C".to_owned(),
        v if v < 0 => format!("L {}", -i32::from(v)),
        v => format!("R {v}"),
    }
}

/// A gain as a readout: "0", "+6", "+5.5", "\u{2212}1" (one decimal only on a half).
fn fmt_db(v: f32) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    let sign = if v > 0.0 { '+' } else { '\u{2212}' };
    let a = v.abs();
    if a.fract() == 0.0 {
        format!("{sign}{a:.0}")
    } else {
        format!("{sign}{a:.1}")
    }
}

/// An indicator that stays lit for [`LIT_FOR`] after its count last rose.
#[derive(Debug, Default, Clone, Copy)]
struct Hold {
    last: Option<u64>,
    lit_until: Option<Duration>,
}

impl Hold {
    /// Feeds the count seen at `now`; whether the indicator is lit. The first sample never lights
    /// (there is nothing to compare with), `None` (unknown) clears everything.
    fn update(&mut self, count: Option<u64>, now: Duration) -> bool {
        match count {
            None => {
                *self = Hold::default();
                false
            }
            Some(c) => {
                if self.last.is_some_and(|prev| c > prev) {
                    self.lit_until = Some(now + LIT_FOR);
                }
                self.last = Some(c);
                self.lit_until.is_some_and(|until| now < until)
            }
        }
    }
}

/// A value the user just set (see [`HOLD`]).
#[derive(Debug, Clone, Copy)]
struct Held {
    value: f32,
    until: Duration,
}

impl Held {
    /// The value to show: the held one while live and the state's differs from it.
    fn show(held: Option<Held>, actual: f32, now: Duration) -> f32 {
        match held {
            Some(h) if now < h.until && (actual - h.value).abs() > 0.01 => h.value,
            _ => actual,
        }
    }
}

/// `c` with its alpha scaled by `k`.
fn fade(c: Rgba, k: f32) -> Rgba {
    alpha(c, (f32::from(c[3]) * k).round().clamp(0.0, 255.0) as u8)
}

// ---------------------------------------------------------------------------------------------
// Geometry.

/// Where everything sits inside the page area.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geo {
    card: Rect,
    track_top: f32,
    track_h: f32,
    /// Column spacing between faders.
    spacing: f32,
    /// The right edge of the dB scale's text.
    scale_right: f32,
    /// The groove centres: the preamp first, then the ten bands.
    xs: [f32; BAND_COUNT + 1],
    /// The divider between the preamp and the bands.
    divider_x: f32,
    /// The top of the indicator row.
    ind_y: f32,
}

impl Geo {
    fn new(rect: Rect) -> Geo {
        let slot = heading_slot(rect);
        let bottom = rect.bottom() - IND_H - IND_GAP;
        // The card starts where the albums grid's first row does; it rises toward the body's top
        // only as far as needed to leave a comfortable track.
        let pad = CARD_TOP_PAD + CARD_BOTTOM;
        let top = if bottom - (rect.y + CARDS_TOP) - pad >= COMFY_TRACK {
            rect.y + CARDS_TOP
        } else {
            (bottom - COMFY_TRACK - pad).max(rect.y + BODY_TOP)
        };
        let track_h = (bottom - top - pad).clamp(MIN_TRACK, MAX_TRACK);
        let card = Rect::new(slot.x, top, slot.w, track_h + CARD_TOP_PAD + CARD_BOTTOM);
        let avail = (card.w - 2.0 * CARD_SIDE - SCALE_W).max(0.0);
        let spacing = (avail / 12.5).clamp(MIN_SPACING, MAX_SPACING);
        let group_w = SCALE_W + 12.5 * spacing;
        let gx = card.x + (card.w - group_w) / 2.0;
        let first = gx + SCALE_W;
        let mut xs = [0.0; BAND_COUNT + 1];
        xs[0] = first + spacing / 2.0;
        for i in 0..BAND_COUNT {
            xs[i + 1] = first + 2.5 * spacing + spacing * (i as f32 + 0.5);
        }
        Geo {
            card,
            track_top: card.y + CARD_TOP_PAD,
            track_h,
            spacing,
            scale_right: gx + SCALE_W - 8.0,
            xs,
            divider_x: first + 1.75 * spacing,
            ind_y: card.bottom() + IND_GAP,
        }
    }

    fn y(&self, v: f32) -> f32 {
        y_of(v, self.track_top, self.track_h)
    }

    /// The labels' top.
    fn label_y(&self) -> f32 {
        self.track_top + self.track_h + 9.0
    }
}

// ---------------------------------------------------------------------------------------------

/// The pane's retained state.
#[derive(Debug, Default)]
pub(crate) struct EqualizerPane {
    preset_open: bool,
    /// The values just set: the preamp first, then the bands.
    held: [Option<Held>; BAND_COUNT + 1],
    /// The balance just set.
    balance_held: Option<Held>,
    limiter: Hold,
    clip: Hold,
    /// When the pane last drew.
    seen: Option<Duration>,
}

impl EqualizerPane {
    /// Draws the pane in `rect` (the page area: its heading slot is [`heading_slot`]); `force`
    /// lights the limiter and clip indicators whatever the engine says (the demo's dumps only).
    pub(crate) fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        k: &Look,
        state: &PlayerState,
        player: &PlayerHandle,
        force: Option<EqMeters>,
    ) {
        let now = ui.time();
        if self
            .seen
            .is_some_and(|t| now.saturating_sub(t) > HIDDEN_GAP)
        {
            self.limiter = Hold::default();
            self.clip = Hold::default();
        }
        self.seen = Some(now);
        let g = Geo::new(rect);
        let slot = heading_slot(rect);
        let eq = &state.eq;

        self.header(ui, slot, k, eq, state.balance, player);

        // The faders: what each shows, and the ones being touched.
        let actual = |i: usize| {
            if i == 0 {
                eq.preamp_db
            } else {
                eq.bands_db[i - 1]
            }
        };
        let mut shown = [0.0f32; BAND_COUNT + 1];
        let mut active = [false; BAND_COUNT + 1];
        for i in 0..=BAND_COUNT {
            shown[i] = Held::show(self.held[i], actual(i), now);
            let cx = g.xs[i];
            let hit = Rect::new(
                cx - g.spacing / 2.0,
                g.track_top - 10.0,
                g.spacing,
                g.track_h + 20.0,
            );
            let id = ui.id(("eq-fader", i));
            let r = ui.interact(
                id,
                hit,
                Sense {
                    scroll: true,
                    ..Sense::DRAG
                },
            );
            active[i] = r.hovered || r.pressed;
            if active[i] {
                ui.set_cursor(CursorIcon::Pointer);
            }
            let py = if r.press_started {
                r.press_pos.map(|p| p.1)
            } else if r.pressed {
                ui.pointer_pos().map(|p| p.1)
            } else {
                None
            };
            let mut wanted = py.map(|y| snap(value_at(y, g.track_top, g.track_h)));
            let notches = r.scroll_lines.1 + r.scroll_pixels.1 / SCROLL_PIXELS_PER_NOTCH;
            if notches != 0.0 {
                let step = if notches > 0.0 { STEP_DB } else { -STEP_DB };
                wanted = Some(snap(shown[i] + step));
            }
            if r.double_clicked {
                wanted = Some(0.0);
            }
            if let Some(v) = wanted
                && v != shown[i]
            {
                player.dispatch(if i == 0 {
                    PlayerAction::SetEqPreamp(v)
                } else {
                    PlayerAction::SetEqBand {
                        index: i - 1,
                        gain_db: v,
                    }
                });
                self.held[i] = Some(Held {
                    value: v,
                    until: now + HOLD,
                });
                shown[i] = v;
            }
        }

        // The indicators.
        let diag = player.diagnostics();
        let dsp = &diag.dsp;
        let limiter = self.limiter.update(dsp.limiter_frames, now);
        let clip = self.clip.update(dsp.clip_count, now);
        let (limiter, clip) = match force {
            Some(f) => (f.limiter, f.clip),
            None => (limiter, clip),
        };
        let dim = if eq.enabled { 1.0 } else { 0.4 };

        let p = ui.painter();
        card(p, k, g.card);
        scale(p, k, &g);
        for i in 0..=BAND_COUNT {
            fader(p, k, &g, i, shown[i], active[i], dim);
        }
        let label_y = g.label_y();
        for i in 0..=BAND_COUNT {
            let cx = g.xs[i];
            let col = if active[i] { k.ink.text } else { k.ink.muted };
            let label = if i == 0 { "PRE" } else { BAND_LABELS[i - 1] };
            let tracked = k.decor().labels == crate::theme::Labels::Tracked;
            if i == 0 && !tracked && width(p, "Preamp", 12.0, false) <= g.spacing - 4.0 {
                text_c(p, "Preamp", 12.0, col, false, cx, label_y);
            } else if i == 0 && tracked {
                let w = caps_width(p, label, 10.0, 1.5);
                caps(p, label, 10.0, col, cx - w / 2.0, label_y + 1.0, 1.5);
            } else {
                text_c(p, label, 12.0, col, false, cx, label_y);
            }
            if g.spacing >= READOUT_MIN_SPACING {
                let readout = fmt_db(shown[i]);
                if k.decor().mono_numbers {
                    let w = mono_width(p, &readout, 11.0);
                    mono(p, &readout, 11.0, k.ink.faint, cx - w / 2.0, label_y + 16.0);
                } else {
                    text_c(p, &readout, 11.0, k.ink.faint, false, cx, label_y + 16.0);
                }
            }
        }
        indicators(
            p,
            k,
            &g,
            Indicators {
                eq: dsp.eq,
                headroom: dsp.headroom_db,
                clip_count: dsp.clip_count,
                limiter,
                clip,
            },
        );

        ui.request_repaint_at(now + REPAINT);
    }

    /// The title's row: the preset combo and the On/Off switch at the right end of the slot.
    fn header(
        &mut self,
        ui: &mut Ui<'_>,
        slot: Rect,
        k: &Look,
        eq: &EqState,
        balance: i8,
        player: &PlayerHandle,
    ) {
        grid::header(ui.painter(), k, slot, Route::Equalizer.title(), None);
        let cy = slot.y + TITLE_CY;
        let tracked = k.decor().labels == crate::theme::Labels::Tracked;

        // The switch, far right, with its word to the left.
        let toggle = Rect::new(
            slot.right() - TOGGLE_W,
            cy - TOGGLE_H / 2.0,
            TOGGLE_W,
            TOGGLE_H,
        );
        let word_w = width(ui.painter(), "Off", 13.0, false);
        let word = if eq.enabled { "On" } else { "Off" };
        text_r(
            ui.painter(),
            word,
            13.0,
            k.ink.muted,
            false,
            toggle.x - 8.0,
            cy - 8.0,
        );
        let mut on = eq.enabled;
        if ui
            .toggle("eq-enabled", toggle, &mut on, "Equalizer")
            .changed
        {
            player.dispatch(PlayerAction::SetEqEnabled(on));
        }

        // The preset combo and its small label.
        let combo = Rect::new(
            toggle.x - 8.0 - word_w - 20.0 - COMBO_W,
            cy - COMBO_H / 2.0,
            COMBO_W,
            COMBO_H,
        );
        let preset = preset_of(eq);
        let preset_left = if tracked {
            combo.x - 10.0 - caps_width(ui.painter(), "PRESET", 10.0, 1.5)
        } else {
            combo.x - 10.0 - width(ui.painter(), "Preset", 12.0, false)
        };
        self.balance(
            ui,
            slot,
            k,
            balance,
            preset_left - BALANCE_GROUP_GAP,
            player,
        );
        if tracked {
            let w = caps_width(ui.painter(), "PRESET", 10.0, 1.5);
            caps(
                ui.painter(),
                "PRESET",
                10.0,
                k.ink.muted,
                combo.x - 10.0 - w,
                cy - 6.0,
                1.5,
            );
        } else {
            text_r(
                ui.painter(),
                "Preset",
                12.0,
                k.ink.muted,
                false,
                combo.x - 10.0,
                cy - 8.0,
            );
        }
        let r = ui.combo(
            "eq-preset",
            combo,
            preset.map_or("Custom", Preset::name),
            &mut self.preset_open,
            "Equalizer preset",
        );
        if r.clicked && !r.changed {
            self.preset_open = false;
        }
        if self.preset_open {
            let mut options: Vec<(&str, bool)> = vec![("Custom", false)];
            options.extend(Preset::ALL.iter().map(|p| (p.name(), true)));
            let selected = preset
                .and_then(|p| Preset::ALL.iter().position(|q| *q == p))
                .map_or(0, |i| i + 1);
            let d = ui.dropdown("eq-preset-list", r.rect, r.id, &options, selected);
            if let Some(i) = d.picked.filter(|i| *i != selected && *i >= 1) {
                for (index, gain_db) in preset_diff(&eq.bands_db, Preset::ALL[i - 1]) {
                    player.dispatch(PlayerAction::SetEqBand { index, gain_db });
                }
            }
            if d.picked.is_some() || d.dismissed {
                self.preset_open = false;
            }
        }
    }

    /// The balance group, its right edge at `right`, centred on the title's row: label, track,
    /// readout. Drag the track (the pointer maps linearly, the centre snaps), wheel by
    /// [`BALANCE_NOTCH`], double-click the track or click the readout to centre it. The faders take
    /// no keys, so neither does this. Nothing is drawn when even the shortest track would crowd
    /// the title.
    fn balance(
        &mut self,
        ui: &mut Ui<'_>,
        slot: Rect,
        k: &Look,
        balance: i8,
        right: f32,
        player: &PlayerHandle,
    ) {
        let now = ui.time();
        let cy = slot.y + TITLE_CY;
        let tracked = k.decor().labels == crate::theme::Labels::Tracked;
        let mono_nums = k.decor().mono_numbers;
        let shown = Held::show(self.balance_held, f32::from(balance), now).round() as i8;

        // Sizes: the readout's box is as wide as the widest text, so nothing shifts while dragging.
        let (label, label_w, readout_w, title_right) = {
            let p = ui.painter();
            let (label, label_w) = if tracked {
                ("BALANCE", caps_width(p, "BALANCE", 10.0, 1.5))
            } else {
                ("Balance", width(p, "Balance", 12.0, false))
            };
            let readout_w = if mono_nums {
                mono_width(p, "L 100", 11.0)
            } else {
                width(p, "L 100", 11.0, false)
            };
            let title_w = width(p, Route::Equalizer.title(), 34.0, k.decor().display_medium);
            (label, label_w, readout_w, slot.x - 2.0 + title_w)
        };
        // The longest track with its label that leaves room under the title; then without the label.
        let fits = |track: f32, with_label: bool| {
            let lead = if with_label {
                label_w + BALANCE_LABEL_GAP
            } else {
                0.0
            };
            let left = right - (lead + track + BALANCE_READOUT_GAP + readout_w);
            (left - title_right >= BALANCE_TITLE_GAP).then_some((left, lead, track, with_label))
        };
        let chosen = [true, false]
            .into_iter()
            .flat_map(|l| BALANCE_TRACKS.into_iter().map(move |t| (t, l)))
            .find_map(|(t, l)| fits(t, l));
        let Some((left, lead, track_w, with_label)) = chosen else {
            return;
        };
        let track = Rect::new(left + lead, cy, track_w, 0.0);
        let hit = Rect::new(
            track.x - BALANCE_REACH,
            cy - BALANCE_HIT_H / 2.0,
            track_w + 2.0 * BALANCE_REACH,
            BALANCE_HIT_H,
        );
        let readout = Rect::new(
            track.right() + BALANCE_READOUT_GAP,
            cy - BALANCE_HIT_H / 2.0,
            readout_w,
            BALANCE_HIT_H,
        );

        let id = ui.id("eq-balance");
        let r = ui.interact(
            id,
            hit,
            Sense {
                scroll: true,
                ..Sense::DRAG
            },
        );
        let rid = ui.id("eq-balance-readout");
        let rr = ui.interact(rid, readout, Sense::CLICK);
        let active = r.hovered || r.pressed;
        if active || rr.hovered {
            ui.set_cursor(CursorIcon::Pointer);
        }
        // Without its label the control says what it is on hover.
        if !with_label {
            ui.tooltip(&r, "Balance");
            ui.tooltip(&rr, "Balance");
        }
        let px = if r.press_started {
            r.press_pos.map(|p| p.0)
        } else if r.pressed {
            ui.pointer_pos().map(|p| p.0)
        } else {
            None
        };
        let mut wanted = px.map(|x| balance_at(x, track.x, track_w));
        let notches = r.scroll_lines.1 + r.scroll_pixels.1 / SCROLL_PIXELS_PER_NOTCH;
        if notches != 0.0 {
            let step = if notches > 0.0 {
                BALANCE_NOTCH
            } else {
                -BALANCE_NOTCH
            };
            wanted = Some((i32::from(shown) + step).clamp(-BALANCE_MAX, BALANCE_MAX) as i8);
        }
        if r.double_clicked || rr.clicked {
            wanted = Some(0);
        }
        let shown = match wanted {
            Some(v) if v != shown => {
                player.dispatch(PlayerAction::SetBalance(v));
                self.balance_held = Some(Held {
                    value: f32::from(v),
                    until: now + HOLD,
                });
                v
            }
            _ => shown,
        };

        let p = ui.painter();
        if with_label {
            if tracked {
                caps(p, label, 10.0, k.ink.muted, left, cy - 6.0, 1.5);
            } else {
                text(p, label, 12.0, k.ink.muted, false, left, cy - 8.0, 0.0);
            }
        }
        balance_track(p, k, track, shown, active);
        let s = fmt_balance(shown);
        if mono_nums {
            mono(p, &s, 11.0, k.ink.muted, readout.x, cy - 7.0);
        } else {
            text(p, &s, 11.0, k.ink.muted, false, readout.x, cy - 8.0, 0.0);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing.

/// The fader card.
fn card(p: &mut P<'_>, k: &Look, r: Rect) {
    let radius = k.radii().card;
    if k.decor().indicator == Indicator::Led {
        well(p, r, radius, false);
    } else {
        outline(p, r, radius, k.ink.wash, k.ink.line_soft);
    }
}

/// The dB scale at the left, the 0 line, the divider after the preamp.
fn scale(p: &mut P<'_>, k: &Look, g: &Geo) {
    let mono_nums = k.decor().mono_numbers;
    let top = g.track_top;
    for (v, s) in [(RANGE_DB, "+12"), (0.0, "0"), (-RANGE_DB, "\u{2212}12")] {
        let y = g.y(v);
        if mono_nums {
            let w = mono_width(p, s, 12.0);
            mono(p, s, 12.0, k.ink.muted, g.scale_right - w, y - 8.0);
        } else {
            text_r(p, s, 12.0, k.ink.muted, false, g.scale_right, y - 8.0);
        }
    }
    // Ticks at the scale: +-12 and 0 at the line's strength, +-6 softer.
    for (v, col) in [
        (RANGE_DB, k.ink.line),
        (6.0, k.ink.line_soft),
        (-6.0, k.ink.line_soft),
        (-RANGE_DB, k.ink.line),
    ] {
        p.fill(
            Rect::new(g.scale_right + 3.0, g.y(v) - 0.5, 5.0, 1.0),
            0.0,
            col,
        );
    }
    // The 0 line, from the scale to the last fader.
    let x0 = g.scale_right + 3.0;
    let x1 = g.xs[BAND_COUNT] + g.spacing / 2.0 - 4.0;
    p.fill(Rect::new(x0, g.y(0.0) - 0.5, x1 - x0, 1.0), 0.0, k.ink.line);
    // The divider between the preamp and the bands.
    p.fill(
        Rect::new(g.divider_x - 0.5, top - 4.0, 1.0, g.track_h + 8.0),
        0.0,
        k.ink.line,
    );
}

/// One fader: groove, the fill from the 0 line, the handle.
fn fader(p: &mut P<'_>, k: &Look, g: &Geo, i: usize, v: f32, active: bool, dim: f32) {
    let cx = g.xs[i];
    let (top, h) = (g.track_top, g.track_h);
    let ring = k.decor().ring;
    let b = k.decor().indicator == Indicator::Led;

    // Groove.
    if b {
        well(p, Rect::new(cx - 3.0, top - 2.0, 6.0, h + 4.0), 3.0, false);
        // Ticks every 3 dB left of the groove.
        for j in -4..=4 {
            if j == 0 {
                continue;
            }
            let len = if j % 2 == 0 { 5.0 } else { 3.0 };
            p.fill(
                Rect::new(cx - 6.0 - len, g.y(3.0 * j as f32) - 0.5, len, 1.0),
                0.0,
                k.ink.line_soft,
            );
        }
    } else {
        p.fill(
            Rect::new(cx - GROOVE_W / 2.0, top, GROOVE_W, h),
            GROOVE_W / 2.0,
            k.ink.line_soft,
        );
    }

    // The fill, from the 0 line to the handle.
    let (zy, hy) = (g.y(0.0), g.y(v));
    if (zy - hy).abs() >= 0.5 {
        let boost = v > 0.0;
        let r = if boost {
            Rect::new(cx - GROOVE_W / 2.0, hy, GROOVE_W, zy - hy)
        } else {
            Rect::new(cx - GROOVE_W / 2.0, zy, GROOVE_W, hy - zy)
        };
        if b {
            let dark = mix(k.deep, k.a, 0.6);
            let (t, bt) = if boost { (k.hi, dark) } else { (dark, k.hi) };
            vgradient(p, r, fade(t, dim), fade(bt, dim));
        } else if ring == Ring::Hairline {
            let (t, bt) = if boost { (k.b, k.a) } else { (k.a, k.b) };
            vgradient(p, r, fade(t, dim), fade(bt, dim));
        } else {
            p.fill(r, 0.0, fade(k.a, dim));
        }
    }

    // The handle, its centre on the value.
    handle(p, k, (cx, hy), active, dim, false);
}

/// A fader's or the balance track's handle, centred on `c`; `across` is the cap of the Ticked ring
/// turned on its side (for a horizontal track).
fn handle(p: &mut P<'_>, k: &Look, c: (f32, f32), active: bool, dim: f32, across: bool) {
    let (cx, hy) = c;
    match k.decor().ring {
        Ring::Bold => {
            shadow(
                p,
                Rect::new(cx - 7.0, hy - 7.0, 14.0, 14.0),
                7.0,
                4.0,
                0.8 * dim,
            );
            disc(p, (cx, hy), 14.0, fade(k.ink.light, dim));
        }
        Ring::Ticked => {
            let (w, h) = if across { (10.0, 20.0) } else { (20.0, 10.0) };
            let r = Rect::new(cx - w / 2.0, hy - h / 2.0, w, h);
            shadow(p, r, 2.0, 3.0, 0.8 * dim);
            p.push_clip_rounded(r, 2.0);
            vgradient(
                p,
                r,
                fade([0x4A, 0x40, 0x36, 0xFF], dim),
                fade([0x18, 0x14, 0x11, 0xFF], dim),
            );
            p.pop_clip();
            let line = if across {
                Rect::new(cx - 0.5, hy - 9.0, 1.0, 18.0)
            } else {
                Rect::new(cx - 9.0, hy - 0.5, 18.0, 1.0)
            };
            p.fill(line, 0.0, fade([0x00, 0x00, 0x00, 0xB0], dim));
            p.ring(
                r,
                2.0,
                1.0,
                alpha(k.ink.light, if active { 0x70 } else { 0x30 }),
                false,
            );
        }
        Ring::Hairline => {
            disc(p, (cx, hy), 12.0, fade(k.ink.light, dim));
            p.ring(
                Rect::new(cx - 6.0, hy - 6.0, 12.0, 12.0),
                6.0,
                1.0,
                fade(k.a, if active { 1.0 } else { 0.8 } * dim),
                false,
            );
        }
    }
}

/// The balance track: the fader's groove turned on its side, a centre mark, the fill from the
/// centre to the handle (outward end bright) and the handle. `track.y` is the centre line.
fn balance_track(p: &mut P<'_>, k: &Look, track: Rect, v: i8, active: bool) {
    let cy = track.y;
    let led = k.decor().indicator == Indicator::Led;
    let ring = k.decor().ring;
    if led {
        well(
            p,
            Rect::new(track.x - 2.0, cy - 3.0, track.w + 4.0, 6.0),
            3.0,
            false,
        );
    } else {
        p.fill(
            Rect::new(track.x, cy - GROOVE_W / 2.0, track.w, GROOVE_W),
            GROOVE_W / 2.0,
            k.ink.line_soft,
        );
    }
    let (zx, hx) = (
        x_of_balance(0, track.x, track.w),
        x_of_balance(v, track.x, track.w),
    );
    if (hx - zx).abs() >= 0.5 {
        let right = v > 0;
        let r = if right {
            Rect::new(zx, cy - GROOVE_W / 2.0, hx - zx, GROOVE_W)
        } else {
            Rect::new(hx, cy - GROOVE_W / 2.0, zx - hx, GROOVE_W)
        };
        // Inner (centre) end and outward (handle) end colours.
        let (inner, outer) = if led {
            (mix(k.deep, k.a, 0.6), k.hi)
        } else {
            (k.a, k.b)
        };
        if led || ring == Ring::Hairline {
            let (l, rt) = if right {
                (inner, outer)
            } else {
                (outer, inner)
            };
            hgradient(p, r, l, rt);
        } else {
            p.fill(r, 0.0, k.a);
        }
    }
    p.fill(Rect::new(zx - 0.5, cy - 5.0, 1.0, 10.0), 0.0, k.ink.line);
    handle(p, k, (hx, cy), active, 1.0, true);
}

/// What the indicator row shows.
struct Indicators {
    eq: Option<Stage>,
    headroom: Option<f32>,
    clip_count: Option<u64>,
    limiter: bool,
    clip: bool,
}

/// The indicator row under the card.
fn indicators(p: &mut P<'_>, k: &Look, g: &Geo, ind: Indicators) {
    let tracked = k.decor().labels == crate::theme::Labels::Tracked;
    let y = g.ind_y;
    let right = g.card.right();
    let d = 7.0;
    let cy = y + IND_H / 2.0;

    // The right group, from the right: "n clipped since start", Clip, Limiter.
    let mut x = right;
    if let Some(n) = ind.clip_count.filter(|n| *n > 0) {
        let s = format!("{n} clipped since start");
        let w = width(p, &s, 12.0, false);
        x -= w;
        text(p, &s, 12.0, k.ink.text2, false, x, y + 1.0, 0.0);
        x -= 10.0;
    }
    let item = |p: &mut P<'_>, x: f32, name: &str, lit: bool, colour: Rgba| -> f32 {
        let label = if tracked {
            name.to_uppercase()
        } else {
            name.to_owned()
        };
        let col = if lit { k.ink.text } else { k.ink.muted };
        let w = if tracked {
            caps_width(p, &label, 10.0, 1.5)
        } else {
            width(p, &label, 12.0, false)
        };
        let lx = x - w;
        if tracked {
            caps(p, &label, 10.0, col, lx, y + 3.0, 1.5);
        } else {
            text(p, &label, 12.0, col, false, lx, y + 1.0, 0.0);
        }
        let ledx = lx - 7.0 - d / 2.0;
        led(p, (ledx, cy), d, colour, k.hi, lit);
        ledx - d / 2.0
    };
    x = item(p, x, "Clip", ind.clip, warn(k)) - 24.0;
    x = item(p, x, "Limiter", ind.limiter, k.a);
    let left_max = x - 24.0;

    // The left: the EQ state, then the headroom when it fits.
    let mut lx = g.card.x;
    let state = match ind.eq {
        Some(Stage::Active) => Some("Active"),
        Some(Stage::Bypassed) => Some("Bypassed: the EQ touches no sample"),
        _ => None,
    };
    if let Some(s) = state {
        let w = text(
            p,
            s,
            13.0,
            k.ink.text2,
            false,
            lx,
            y,
            (left_max - lx).max(0.0),
        );
        lx += w + 24.0;
    }
    if let Some(h) = ind.headroom.filter(|h| *h > 0.05) {
        let s = format!("Headroom \u{2212}{h:.1} dB");
        if lx + width(p, &s, 13.0, false) <= left_max {
            text(p, &s, 13.0, k.ink.muted, false, lx, y, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::engine::BAND_HZ;

    #[test]
    fn e1_snap_rounds_to_half_steps_and_clamps() {
        assert_eq!(snap(0.24), 0.0);
        assert_eq!(snap(0.26), 0.5);
        assert_eq!(snap(-0.26), -0.5);
        assert_eq!(snap(13.0), 12.0);
        assert_eq!(snap(-40.0), -12.0);
    }

    #[test]
    fn e1b_balance_at_spans_the_range_and_snaps_to_an_exact_centre() {
        // A 200 px track from x 50: one pixel per unit.
        assert_eq!(balance_at(50.0, 50.0, 200.0), -100);
        assert_eq!(balance_at(250.0, 50.0, 200.0), 100);
        assert_eq!(balance_at(-30.0, 50.0, 200.0), -100);
        assert_eq!(balance_at(900.0, 50.0, 200.0), 100);
        assert_eq!(balance_at(150.0, 50.0, 200.0), 0);
        for off in [-4.0, -3.0, 0.0, 3.0, 4.0] {
            assert_eq!(balance_at(150.0 + off, 50.0, 200.0), 0, "{off}");
        }
        assert_eq!(balance_at(150.0 + 5.0, 50.0, 200.0), 5);
        assert_eq!(balance_at(150.0 - 5.0, 50.0, 200.0), -5);
        assert_eq!(balance_at(110.0, 50.0, 200.0), -40);
        assert_eq!(balance_at(10.0, 50.0, 0.0), 0);
        // The inverse puts the centre in the middle and the ends on the ends.
        assert_eq!(x_of_balance(0, 50.0, 200.0), 150.0);
        assert_eq!(x_of_balance(-100, 50.0, 200.0), 50.0);
        assert_eq!(x_of_balance(100, 50.0, 200.0), 250.0);
    }

    #[test]
    fn e1c_balance_readouts_name_the_side() {
        assert_eq!(fmt_balance(0), "C");
        assert_eq!(fmt_balance(-40), "L 40");
        assert_eq!(fmt_balance(25), "R 25");
        assert_eq!(fmt_balance(-100), "L 100");
        assert_eq!(fmt_balance(100), "R 100");
    }

    #[test]
    fn e2_value_at_spans_the_range_with_an_exact_centre() {
        assert_eq!(value_at(10.0, 10.0, 100.0), 12.0);
        assert_eq!(value_at(110.0, 10.0, 100.0), -12.0);
        assert_eq!(value_at(60.0, 10.0, 100.0), 0.0);
        // Outside the track it clamps.
        assert_eq!(value_at(-50.0, 10.0, 100.0), 12.0);
        assert_eq!(value_at(500.0, 10.0, 100.0), -12.0);
        // The inverse puts +6 at the quarter point.
        assert_eq!(y_of(6.0, 10.0, 100.0), 35.0);
    }

    #[test]
    fn e3_flat_on_a_flat_state_changes_nothing() {
        assert!(preset_diff(&[0.0; BAND_COUNT], Preset::Flat).is_empty());
        let bass = preset_diff(&[0.0; BAND_COUNT], Preset::Bass);
        assert_eq!(bass, vec![(0, 6.0), (1, 5.0), (2, 3.0), (3, 1.0)]);
        // From Bass, applying Bass again changes nothing; Flat undoes it.
        assert!(preset_diff(&Preset::Bass.bands(), Preset::Bass).is_empty());
        assert_eq!(preset_diff(&Preset::Bass.bands(), Preset::Flat).len(), 4);
    }

    fn state(bands: [f32; BAND_COUNT]) -> EqState {
        EqState {
            enabled: true,
            preamp_db: 0.0,
            bands_db: bands,
        }
    }

    #[test]
    fn e4_preset_of_is_exact() {
        for p in Preset::ALL {
            assert_eq!(preset_of(&state(p.bands())), Some(p));
        }
        let mut b = Preset::Bass.bands();
        b[9] += 0.5;
        assert_eq!(preset_of(&state(b)), None);
        // The preamp and the enable flag do not matter.
        let mut s = state(Preset::Bass.bands());
        s.preamp_db = 3.0;
        s.enabled = false;
        assert_eq!(preset_of(&s), Some(Preset::Bass));
        assert_eq!(Preset::Bass.name(), "Bass");
    }

    #[test]
    fn e5_hold_lights_for_two_seconds_after_a_rise() {
        let s = Duration::from_secs;
        let mut h = Hold::default();
        // The first sample never lights, even above zero.
        assert!(!h.update(Some(5), s(0)));
        assert!(!h.update(Some(5), s(1)));
        // A rise at t = 2 lights until t = 4.
        assert!(h.update(Some(6), s(2)));
        assert!(h.update(Some(6), Duration::from_millis(3990)));
        assert!(!h.update(Some(6), s(4)));
        // Unknown clears; the next sample is a first one again.
        assert!(h.update(Some(7), s(5)));
        assert!(!h.update(None, s(5)));
        assert!(!h.update(Some(9), s(6)));
    }

    #[test]
    fn readouts_use_the_minus_sign_and_one_decimal_on_halves() {
        assert_eq!(fmt_db(0.0), "0");
        assert_eq!(fmt_db(6.0), "+6");
        assert_eq!(fmt_db(5.5), "+5.5");
        assert_eq!(fmt_db(-1.0), "\u{2212}1");
        assert_eq!(fmt_db(-3.5), "\u{2212}3.5");
    }

    #[test]
    fn layout_fits_inside_the_page_area_at_both_sizes() {
        // The page areas of 1440 x 900 and 760 x 480 (browse's: sheet inset, heading above).
        for area in [
            Rect::new(68.0, 136.0, 1304.0, 612.0),
            Rect::new(68.0, 136.0, 624.0, 192.0),
        ] {
            let g = Geo::new(area);
            let slot = heading_slot(area);
            assert!(g.card.x >= area.x && g.card.right() <= area.right());
            assert!(g.card.y >= slot.bottom(), "card clears the heading");
            assert!(
                g.ind_y + IND_H <= area.bottom() + 0.01,
                "indicator row inside"
            );
            assert!(g.spacing >= MIN_SPACING && g.spacing <= MAX_SPACING);
            assert!(g.track_h >= MIN_TRACK && g.track_h <= MAX_TRACK);
            // Every fader column sits inside the card, the preamp left of the divider.
            assert!(g.xs[0] - g.spacing / 2.0 > g.scale_right);
            assert!(g.xs[0] < g.divider_x && g.divider_x < g.xs[1]);
            assert!(g.xs[BAND_COUNT] + g.spacing / 2.0 <= g.card.right());
            // The labels and readouts sit inside the card.
            assert!(g.label_y() + 30.0 <= g.card.bottom() + 0.01);
        }
    }

    #[test]
    fn card_sits_at_the_grids_offset_when_the_page_is_tall() {
        let tall = Geo::new(Rect::new(68.0, 136.0, 1304.0, 612.0));
        assert_eq!(tall.card.y, 136.0 + CARDS_TOP);
        // At the minimum it rises, to no higher than the body's top.
        let min = Geo::new(Rect::new(68.0, 136.0, 624.0, 192.0));
        assert!(min.card.y >= 136.0 + BODY_TOP && min.card.y < tall.card.y - 136.0 + 136.0);
        assert!(min.track_h >= MIN_TRACK);
    }

    #[test]
    fn band_labels_follow_the_core_table() {
        let want: Vec<String> = BAND_HZ
            .iter()
            .map(|hz| {
                if *hz >= 1000.0 {
                    format!("{}K", (hz / 1000.0) as u32)
                } else {
                    format!("{}", *hz as u32)
                }
            })
            .collect();
        assert_eq!(BAND_LABELS.to_vec(), want);
    }
}
