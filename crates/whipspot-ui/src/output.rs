//! The output view: which device and mode the audio goes to, and what plays now. It draws no
//! heading and no card of its own and assumes no window, so the diagnostics window and a settings
//! pane can both place it.

use lune_ui::paint::Rect;
use lune_ui::text::{Spec, Style};
use lune_ui::ui::Ui;
use lune_ui::widgets::button::{ButtonKind, button_width};
use whipspot_core::engine::OutputMode;
use whipspot_core::player::{OutputStatus, PlayerAction, PlayerHandle};

/// A control's height.
const ROW_H: f32 = 32.0;
/// The gap between two rows.
const ROW_GAP: f32 = 8.0;
/// The label column's width.
const LABEL_W: f32 = 64.0;
/// The gap between a combo and the button beside it.
const SIDE_GAP: f32 = 8.0;
/// The mode combo's width.
const MODE_W: f32 = 200.0;
/// The gap above the summary.
const SUMMARY_GAP: f32 = 12.0;
/// A summary line's height, and a note's.
const LINE_H: f32 = 20.0;
const NOTE_H: f32 = 16.0;

/// The device entry that stands for the system default.
pub(crate) const DEFAULT_DEVICE: &str = "System default";
const NO_EXCLUSIVE: &str = "Exclusive mode is not available on this system";

/// What the view keeps between frames.
#[derive(Debug, Default)]
pub struct OutputModel {
    device_open: bool,
    mode_open: bool,
    /// The device list was asked for.
    requested: bool,
}

/// The chosen device's name: the listed device with that id, else the id, else the default's.
pub(crate) fn device_name(out: &OutputStatus) -> String {
    match &out.device {
        None => DEFAULT_DEVICE.to_owned(),
        Some(id) => out
            .devices
            .iter()
            .find(|d| &d.id == id)
            .map_or_else(|| id.clone(), |d| d.name.clone()),
    }
}

/// The mode's word.
pub(crate) fn mode_name(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Shared => "Shared",
        OutputMode::Exclusive => "Exclusive",
    }
}

/// The summary line: what plays, or why nothing does.
pub(crate) fn summary_text(summary: &str, out: &OutputStatus) -> String {
    if !summary.is_empty() {
        summary.to_owned()
    } else if out.idle {
        "Output released while idle".to_owned()
    } else {
        "Output not open".to_owned()
    }
}

/// Draws the view with its top left at that of `rect`, wide as `rect` (its height is ignored).
/// Returns the height used. The first frame of a model asks the player for the device list.
pub fn view(ui: &mut Ui<'_>, rect: Rect, model: &mut OutputModel, player: &PlayerHandle) -> f32 {
    if !model.requested {
        model.requested = true;
        player.dispatch(PlayerAction::RefreshOutputDevices);
    }
    let state = player.state();
    let out = &state.output;
    let theme = *ui.theme();
    let label_style = Style::ui(theme.font_base, theme.fg_tertiary);
    let mut y = rect.y;

    // Device: the combo, and Refresh at the right edge.
    let refresh_w = button_width(ui.painter(), "Refresh");
    let refresh = Rect::new(rect.right() - refresh_w, y, refresh_w, ROW_H);
    ui.painter().text_in(
        &Spec::plain("Device", label_style),
        Rect::new(rect.x, y, LABEL_W, ROW_H),
        0.0,
    );
    let combo = Rect::new(
        rect.x + LABEL_W,
        y,
        (refresh.x - SIDE_GAP - rect.x - LABEL_W).max(0.0),
        ROW_H,
    );
    let mut options: Vec<(String, bool)> = vec![(DEFAULT_DEVICE.to_owned(), true)];
    options.extend(out.devices.iter().map(|d| (d.name.clone(), true)));
    let selected = out
        .device
        .as_ref()
        .and_then(|id| out.devices.iter().position(|d| &d.id == id))
        .map_or(0, |i| i + 1);
    let r = ui.combo(
        "output-device",
        combo,
        &device_name(out),
        &mut model.device_open,
        "Output device",
    );
    if r.clicked && !r.changed {
        model.device_open = false;
    }
    if model.device_open {
        let d = ui.dropdown("output-device-list", r.rect, r.id, &options, selected);
        if let Some(i) = d.picked.filter(|i| *i != selected) {
            let device = i.checked_sub(1).map(|i| out.devices[i].id.clone());
            player.dispatch(PlayerAction::SetOutput {
                device,
                mode: out.mode,
            });
        }
        if d.picked.is_some() || d.dismissed {
            model.device_open = false;
        }
    }
    if ui
        .button("output-refresh", refresh, "Refresh", ButtonKind::Ghost)
        .clicked
    {
        player.dispatch(PlayerAction::RefreshOutputDevices);
    }
    y += ROW_H + ROW_GAP;

    // Mode.
    ui.painter().text_in(
        &Spec::plain("Mode", label_style),
        Rect::new(rect.x, y, LABEL_W, ROW_H),
        0.0,
    );
    let exclusive = out.modes.contains(&OutputMode::Exclusive);
    let modes: &[OutputMode] = if exclusive || out.mode == OutputMode::Exclusive {
        &[OutputMode::Shared, OutputMode::Exclusive]
    } else {
        &[OutputMode::Shared]
    };
    let options: Vec<(&str, bool)> = modes.iter().map(|m| (mode_name(*m), true)).collect();
    let selected = modes.iter().position(|m| *m == out.mode).unwrap_or(0);
    let combo = Rect::new(rect.x + LABEL_W, y, MODE_W.min(rect.w - LABEL_W), ROW_H);
    let r = ui.combo(
        "output-mode",
        combo,
        mode_name(out.mode),
        &mut model.mode_open,
        "Output mode",
    );
    if r.clicked && !r.changed {
        model.mode_open = false;
    }
    if model.mode_open {
        let d = ui.dropdown("output-mode-list", r.rect, r.id, &options, selected);
        if let Some(i) = d.picked.filter(|i| *i != selected) {
            player.dispatch(PlayerAction::SetOutput {
                device: out.device.clone(),
                mode: modes[i],
            });
        }
        if d.picked.is_some() || d.dismissed {
            model.mode_open = false;
        }
    }
    y += ROW_H;
    if !exclusive {
        let note = Spec::plain(NO_EXCLUSIVE, Style::ui(theme.font_s, theme.fg_tertiary));
        let at = Rect::new(rect.x + LABEL_W, y + 4.0, rect.w - LABEL_W, NOTE_H);
        let p = ui.painter();
        let note = p.elide(&note, at.w);
        p.text_in(&note, at, 0.0);
        y += 4.0 + NOTE_H;
    }

    // What plays now, and why it is not what was chosen.
    y += SUMMARY_GAP;
    let summary = Spec::plain(
        summary_text(&state.output_summary, out),
        Style::ui(theme.font_base, theme.fg_secondary),
    );
    let at = Rect::new(rect.x, y, rect.w, LINE_H);
    let p = ui.painter();
    let summary = p.elide(&summary, at.w);
    p.text_in(&summary, at, 0.0);
    y += LINE_H;
    if let Some(reason) = out.fallback {
        let line = Spec::plain(reason, Style::ui(theme.font_s, theme.warn));
        let at = Rect::new(rect.x, y, rect.w, NOTE_H);
        let p = ui.painter();
        let line = p.elide(&line, at.w);
        p.text_in(&line, at, 0.0);
        y += NOTE_H;
    }
    y - rect.y
}
