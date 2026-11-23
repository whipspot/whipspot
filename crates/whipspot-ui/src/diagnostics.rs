//! The diagnostics window (D26, MD45): the output view, the path verdict and the five sections of
//! the audio diagnostics snapshot, with "Copy as text". A skin-independent window of plain luneui
//! widgets; it repaints at 4 Hz while it is drawn.

use std::time::Duration;

use lune_ui::chrome::CARD_RADIUS;
use lune_ui::input::WindowCommand;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::shortcuts::{CommandId, Shortcuts};
use lune_ui::text::{Spec, Style};
use lune_ui::ui::Ui;
use lune_ui::widgets::button::{ButtonKind, ButtonOptions, button_width_with};
use lune_ui::widgets::kv_list::{KvMode, KvOptions};
use lune_ui::widgets::scroll_area::{Extent, ScrollOptions};
use lune_ui::widgets::verdict_row::Verdict as Mark;
use whipspot_core::diagnostics::{
    AudioDiagnostics, Dither, Resampler, Stage, Verdict as PathVerdict,
};
use whipspot_core::player::{OutputStatus, PlayerHandle, PlayerState};
use whipspot_core::power::{PowerMode, PowerStatus};

use crate::layout::{CONTENT_PAD, HEADING_TOP, MARGIN};
use crate::output::{self, OutputModel};
use crate::shortcuts::{Command, diagnostics_table};
use crate::{Chrome, TITLE_BAND};

/// What a value reads when the snapshot does not know it.
const UNKNOWN: &str = "unknown";
/// The window repaints this often while it is drawn.
const REPAINT: Duration = Duration::from_millis(250);
/// The gap between the output view and the path block, and between a block and the next heading.
const BLOCK_GAP: f32 = 24.0;
const SECTION_GAP: f32 = 20.0;
/// The gap under a heading.
const HEADING_GAP: f32 = 4.0;
/// The path heading's line: it holds the small button.
const PATH_LINE: f32 = 24.0;
/// The room kept under the last section.
const BOTTOM_PAD: f32 = 16.0;

/// One section of the snapshot: a heading and its key and value rows.
struct Section {
    title: &'static str,
    rows: Vec<(&'static str, String)>,
}

pub(crate) fn or_unknown<T>(value: Option<T>, format: impl FnOnce(T) -> String) -> String {
    value.map_or_else(|| UNKNOWN.to_owned(), format)
}

pub(crate) fn text_or_unknown(value: &Option<String>) -> String {
    or_unknown(value.as_deref(), str::to_owned)
}

pub(crate) fn khz(hz: u32) -> String {
    format!("{} kHz", f64::from(hz) / 1000.0)
}

fn db(value: f32, unit: &str) -> String {
    format!("{value:+.1} {unit}")
}

fn float(value: Option<f32>, format: impl FnOnce(f32) -> String) -> String {
    or_unknown(value.filter(|v| v.is_finite()), format)
}

/// The resampler's line: "none", or the algorithm, quality and ratio.
pub(crate) fn resampler_text(r: &Resampler) -> String {
    match r {
        Resampler::None => "none".to_owned(),
        Resampler::Active {
            algorithm,
            ratio,
            quality,
        } => format!("{algorithm}, {quality}, ratio {ratio:.4}"),
    }
}

fn stage(value: Option<Stage>) -> String {
    or_unknown(value, |s| {
        match s {
            Stage::Active => "active",
            Stage::Bypassed => "bypassed",
        }
        .to_owned()
    })
}

fn yes_no(value: Option<bool>) -> String {
    or_unknown(value, |b| if b { "yes" } else { "no" }.to_owned())
}

fn count(value: Option<u64>) -> String {
    or_unknown(value, |n| n.to_string())
}

/// The efficiency mode's value: the adapter's name and what the player did about power (D8).
/// The texts are fixed except the name, which the adapter gives as a fixed string.
pub(crate) fn power_text(power: &PowerStatus) -> String {
    let Some(name) = power.name else {
        return "Not managed".to_owned();
    };
    if !power.enabled {
        return format!("{name}, off in Settings");
    }
    if power.error.is_some() {
        return format!("{name}, the system refused");
    }
    match (power.mode, power.shown) {
        (PowerMode::Eco, _) => format!("{name}, saving power (hidden, music at full speed)"),
        (PowerMode::EcoIdle, _) => format!("{name}, saving power (hidden, nothing playing)"),
        (PowerMode::Normal, false) => format!("{name}, normal"),
        (PowerMode::Normal, true) => format!("{name}, normal (window shown)"),
    }
}

/// The six sections of `diag` and `power`, in the window's order. Power comes first, so its
/// row sits in the window's first screenful.
fn sections(diag: &AudioDiagnostics, power: &PowerStatus) -> [Section; 6] {
    let (s, dc, dsp, o, l) = (
        &diag.source,
        &diag.decode,
        &diag.dsp,
        &diag.output,
        &diag.live,
    );
    [
        Section {
            title: "Power",
            rows: vec![("Efficiency mode", power_text(power))],
        },
        Section {
            title: "Source",
            rows: vec![
                ("Provider", text_or_unknown(&s.provider)),
                ("Codec", text_or_unknown(&s.codec)),
                ("Container", text_or_unknown(&s.container)),
                (
                    "Nominal bitrate",
                    or_unknown(s.bitrate_nominal_kbps, |k| format!("{k} kbps")),
                ),
                (
                    "Actual bitrate",
                    or_unknown(s.bitrate_actual_kbps, |k| format!("{k} kbps")),
                ),
                ("Sample rate", or_unknown(s.sample_rate_hz, khz)),
                ("Bit depth", or_unknown(s.bit_depth, |b| format!("{b}-bit"))),
                ("Channels", or_unknown(s.channels, |c| c.to_string())),
                ("Lossless", yes_no(s.lossless)),
                ("Quality cap", text_or_unknown(&s.quality_cap)),
            ],
        },
        Section {
            title: "Decode",
            rows: vec![
                ("Decoder", text_or_unknown(&dc.decoder)),
                ("Frames decoded", count(dc.frames_decoded)),
                ("Decode errors", count(dc.decode_errors)),
            ],
        },
        Section {
            title: "DSP",
            rows: vec![
                ("Equaliser", stage(dsp.eq)),
                ("Preamp", stage(dsp.preamp)),
                ("ReplayGain", stage(dsp.replay_gain)),
                (
                    "ReplayGain applied",
                    float(dsp.replay_gain_db, |v| db(v, "dB")),
                ),
                ("Volume", stage(dsp.volume)),
                ("Balance", stage(dsp.balance)),
                ("Headroom", float(dsp.headroom_db, |v| db(v, "dB"))),
                ("Peak", float(dsp.peak_dbfs, |v| db(v, "dBFS"))),
                ("Clips", count(dsp.clip_count)),
                (
                    "Resampler",
                    or_unknown(dsp.resampler.as_ref(), resampler_text),
                ),
                (
                    "Dither",
                    or_unknown(dsp.dither.as_ref(), |d| match d {
                        Dither::None => "none".to_owned(),
                        Dither::Active(name) => name.clone(),
                    }),
                ),
                ("Limiter", stage(dsp.limiter)),
                ("Limiter frames", count(dsp.limiter_frames)),
            ],
        },
        Section {
            title: "Output",
            rows: vec![
                ("Host API", text_or_unknown(&o.host_api)),
                ("Mode", text_or_unknown(&o.mode)),
                ("Device", text_or_unknown(&o.device_name)),
                ("Sample rate", or_unknown(o.sample_rate_hz, khz)),
                ("Format", text_or_unknown(&o.format)),
                ("Bit depth", or_unknown(o.bit_depth, |b| format!("{b}-bit"))),
                (
                    "Buffer",
                    or_unknown(o.buffer_frames, |n| format!("{n} frames")),
                ),
                ("Latency", float(o.latency_ms, |v| format!("{v:.1} ms"))),
                ("Underruns", count(o.underruns)),
                ("Tap drops", count(o.tap_drops)),
            ],
        },
        Section {
            title: "Live",
            rows: vec![
                (
                    "Buffer fill",
                    float(l.buffer_fill, |v| {
                        format!("{:.0} %", v.clamp(0.0, 1.0) * 100.0)
                    }),
                ),
                (
                    "Callback CPU avg",
                    or_unknown(l.callback_cpu_avg_us, |u| format!("{u} \u{b5}s")),
                ),
                (
                    "Callback CPU max",
                    or_unknown(l.callback_cpu_max_us, |u| format!("{u} \u{b5}s")),
                ),
                (
                    "Clock drift",
                    float(l.clock_drift_ppm, |v| format!("{v:.1} ppm")),
                ),
            ],
        },
    ]
}

/// The verdict's word, its reasons and the mark its row gets.
pub(crate) fn verdict_parts(verdict: Option<&PathVerdict>) -> (&'static str, &[String], Mark) {
    match verdict {
        Some(PathVerdict::BitPerfect) => ("BIT-PERFECT", &[], Mark::Ok),
        Some(PathVerdict::LosslessProcessed { reasons }) => {
            ("LOSSLESS-PROCESSED", reasons, Mark::Warn)
        }
        Some(PathVerdict::Resampled { reasons }) => ("RESAMPLED", reasons, Mark::Warn),
        None => ("path unknown", &[], Mark::Warn),
    }
}

/// The verdict row's text: the word, then the reasons.
fn verdict_text(verdict: Option<&PathVerdict>) -> String {
    let (word, reasons, _) = verdict_parts(verdict);
    if reasons.is_empty() {
        word.to_owned()
    } else {
        format!("{word} \u{2014} {}", reasons.join("; "))
    }
}

/// The diagnostics as plain text, for the clipboard: the output, the verdict with its reasons,
/// the lossy flag, then each section's rows as `key: value` lines (the window's keys and order).
pub fn text(diag: &AudioDiagnostics, state: &PlayerState) -> String {
    render_text(diag, &state.output_summary, &state.output, &state.power)
}

fn render_text(
    diag: &AudioDiagnostics,
    summary: &str,
    out: &OutputStatus,
    power: &PowerStatus,
) -> String {
    let mut s = String::from("Audio diagnostics\n");
    s.push_str(&format!(
        "Chosen output: {}, {}\n",
        output::device_name(out),
        output::mode_name(out.mode)
    ));
    s.push_str(&format!(
        "Output summary: {}\n",
        output::summary_text(summary, out)
    ));
    if let Some(reason) = out.fallback {
        s.push_str(&format!("Fallback: {reason}\n"));
    }
    let (word, reasons, _) = verdict_parts(diag.verdict.as_ref());
    s.push_str(&format!("Verdict: {word}\n"));
    for reason in reasons {
        s.push_str(&format!("  - {reason}\n"));
    }
    s.push_str(&format!("Lossy source: {}\n", yes_no(diag.lossy_source)));
    for section in sections(diag, power) {
        s.push_str(&format!("\n{}\n", section.title));
        for (key, value) in &section.rows {
            s.push_str(&format!("{key}: {value}\n"));
        }
    }
    s
}

/// The diagnostics window's view.
#[derive(Debug)]
pub struct DiagnosticsUi {
    chrome: Chrome,
    maximized: bool,
    commands: Vec<WindowCommand>,
    output: OutputModel,
    toggle: bool,
}

impl DiagnosticsUi {
    pub fn new(chrome: Chrome) -> Self {
        Self {
            chrome,
            maximized: false,
            commands: Vec::new(),
            output: OutputModel::default(),
            toggle: false,
        }
    }

    /// The shortcut table of this window: the toggle that closes it.
    pub fn shortcuts() -> Shortcuts {
        diagnostics_table()
    }

    /// Acts on the shortcut commands the last frame reported.
    pub fn fired(&mut self, fired: &[CommandId]) {
        if fired
            .iter()
            .filter_map(Command::from_id)
            .any(|c| c == Command::ToggleDiagnostics)
        {
            self.toggle = true;
        }
    }

    /// Whether the window asked to be closed since the last call.
    pub fn take_toggle(&mut self) -> bool {
        std::mem::take(&mut self.toggle)
    }

    /// Whether the window is maximised now (the host tells before each frame).
    pub fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    /// The window operations the last frame asked for, in order.
    pub fn take_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Draws one frame: the glass, the title band and one card with a scrolling column.
    pub fn frame(&mut self, ui: &mut Ui<'_>, player: &PlayerHandle) {
        let state = player.state();
        let diag = player.diagnostics();
        let theme = *ui.theme();
        let win = ui.painter().window;
        let card = Rect::new(
            MARGIN,
            TITLE_BAND,
            (win.w - 2.0 * MARGIN).max(0.0),
            (win.h - TITLE_BAND - MARGIN).max(0.0),
        );
        {
            let p = ui.painter();
            p.on(LayerId::Base);
            let panel = theme.glass.panel.premultiplied();
            p.acrylic(win, 0.0, panel, [0.0; 4], true);
            p.acrylic(
                card,
                CARD_RADIUS,
                theme.glass.card.premultiplied(),
                panel,
                true,
            );
            p.on(LayerId::Page);
        }
        crate::draw_title(ui, win, self.chrome, "Audio");

        let column = Rect::new(
            card.x + CONTENT_PAD,
            card.y + HEADING_TOP,
            (card.w - 2.0 * CONTENT_PAD).max(0.0),
            0.0,
        );
        let output = &mut self.output;
        let copy = ui
            .scroll_area("column", card, &ScrollOptions::default(), |ui, _| {
                let (copy, bottom) = column_body(ui, column, output, &diag, &state.power, player);
                (
                    copy,
                    Extent::new(f64::from(card.w), f64::from(bottom + BOTTOM_PAD - card.y)),
                )
            })
            .value;
        if copy {
            ui.clipboard_write(text(&diag, &state));
        }

        crate::window_controls(
            ui,
            win,
            TITLE_BAND,
            self.chrome,
            self.maximized,
            &mut self.commands,
        );
        let at = ui.time() + REPAINT;
        ui.request_repaint_at(at);
    }
}

/// A small section heading at (`x`, `y`); returns its height.
fn heading(ui: &mut Ui<'_>, title: &str, x: f32, y: f32) -> f32 {
    let theme = *ui.theme();
    let spec = Spec::plain(
        title.to_uppercase(),
        Style::ui(theme.font_s, theme.fg_tertiary).medium(),
    );
    ui.painter().text(&spec, x, y).1
}

/// The column: the output view, the path block, the five sections. Returns whether "Copy as
/// text" was clicked, and the column's bottom.
fn column_body(
    ui: &mut Ui<'_>,
    column: Rect,
    model: &mut OutputModel,
    diag: &AudioDiagnostics,
    power: &PowerStatus,
    player: &PlayerHandle,
) -> (bool, f32) {
    let mut y = column.y;
    y += output::view(ui, Rect::new(column.x, y, column.w, 0.0), model, player);

    // The path block.
    y += BLOCK_GAP;
    let heading_h = heading_in(ui, column.x, y, PATH_LINE, "Path");
    let options = ButtonOptions {
        kind: ButtonKind::Secondary,
        small: true,
        icon: None,
    };
    let w = button_width_with(ui.painter(), "Copy as text", &options);
    let copy = ui
        .button_with(
            "copy-as-text",
            Rect::new(column.right() - w, y, w, PATH_LINE),
            "Copy as text",
            &options,
        )
        .clicked;
    y += heading_h.max(PATH_LINE) + HEADING_GAP;
    let (_, _, mark) = verdict_parts(diag.verdict.as_ref());
    y += ui.verdict_row(
        "verdict",
        Rect::new(column.x, y, column.w, 0.0),
        mark,
        "Verdict",
        &verdict_text(diag.verdict.as_ref()),
        false,
    );
    if diag.lossy_source == Some(true) {
        y += ui.verdict_row(
            "lossy",
            Rect::new(column.x, y, column.w, 0.0),
            Mark::Warn,
            "Source",
            "lossy source",
            false,
        );
    }

    // The sections.
    for section in sections(diag, power) {
        y += SECTION_GAP;
        y += heading(ui, section.title, column.x, y) + HEADING_GAP;
        let pairs: Vec<(&str, &str)> = section.rows.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let opts = KvOptions {
            mode: KvMode::Wrap,
            mono: section.title == "Live",
            copy: false,
        };
        y += ui.kv_list(
            section.title,
            Rect::new(column.x, y, column.w, 0.0),
            &pairs,
            &opts,
        );
    }
    (copy, y)
}

/// A heading vertically centred on a line of `line` px at `y`; returns the line's height.
fn heading_in(ui: &mut Ui<'_>, x: f32, y: f32, line: f32, title: &str) -> f32 {
    let theme = *ui.theme();
    let spec = Spec::plain(
        title.to_uppercase(),
        Style::ui(theme.font_s, theme.fg_tertiary).medium(),
    );
    ui.painter()
        .text_in(&spec, Rect::new(x, y, 200.0, line), 0.0);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_has_every_section_and_reads_unknown() {
        let mut diag = AudioDiagnostics::default();
        diag.source.codec = Some("Ogg Vorbis".to_owned());
        diag.output.sample_rate_hz = Some(48_000);
        diag.dsp.eq = Some(Stage::Bypassed);
        diag.verdict = Some(PathVerdict::Resampled {
            reasons: vec!["44.1 kHz source on a 48 kHz device".to_owned()],
        });
        diag.lossy_source = Some(true);
        let out = OutputStatus::default();
        let text = render_text(
            &diag,
            "Shared mode, Speakers, 48 kHz, 32-bit float",
            &out,
            &PowerStatus::default(),
        );
        for header in [
            "\nSource\n",
            "\nDecode\n",
            "\nDSP\n",
            "\nOutput\n",
            "\nLive\n",
        ] {
            assert!(text.contains(header), "{header:?} missing in\n{text}");
        }
        assert!(text.contains("Codec: Ogg Vorbis\n"));
        assert!(text.contains("Container: unknown\n"));
        assert!(text.contains("Sample rate: 48 kHz\n"));
        assert!(text.contains("Equaliser: bypassed\n"));
        assert!(text.contains("Verdict: RESAMPLED\n"));
        assert!(text.contains("  - 44.1 kHz source on a 48 kHz device\n"));
        assert!(text.contains("Lossy source: yes\n"));
        assert!(text.contains("Output summary: Shared mode, Speakers, 48 kHz, 32-bit float\n"));
    }

    #[test]
    fn an_empty_snapshot_reads_unknown_everywhere() {
        let text = render_text(
            &AudioDiagnostics::default(),
            "",
            &OutputStatus::default(),
            &PowerStatus::default(),
        );
        assert!(text.contains("Verdict: path unknown\n"));
        assert!(text.contains("Lossy source: unknown\n"));
        assert!(text.contains("Output summary: Output not open\n"));
        assert!(text.contains("Resampler: unknown\n"));
        assert_eq!(verdict_text(None), "path unknown");
    }

    #[test]
    fn text_shows_the_live_cpu_max_and_clock_drift_after_the_average() {
        let mut diag = AudioDiagnostics::default();
        diag.live.callback_cpu_avg_us = Some(40);
        diag.live.callback_cpu_max_us = Some(310);
        diag.live.clock_drift_ppm = Some(-12.34);
        let text = render_text(&diag, "", &OutputStatus::default(), &PowerStatus::default());
        let live = text.split("\nLive\n").nth(1).expect("a Live section");
        let lines: Vec<&str> = live.lines().collect();
        assert_eq!(
            lines,
            [
                "Buffer fill: unknown",
                "Callback CPU avg: 40 \u{b5}s",
                "Callback CPU max: 310 \u{b5}s",
                "Clock drift: -12.3 ppm",
            ]
        );
        // Not known yet (under 2 s of rendering): unknown, not zero.
        diag.live.callback_cpu_max_us = None;
        diag.live.clock_drift_ppm = None;
        let text = render_text(&diag, "", &OutputStatus::default(), &PowerStatus::default());
        assert!(text.contains("Callback CPU max: unknown\n"));
        assert!(text.contains("Clock drift: unknown\n"));
    }

    fn power(
        name: Option<&'static str>,
        enabled: bool,
        shown: bool,
        mode: PowerMode,
    ) -> PowerStatus {
        PowerStatus {
            name,
            enabled,
            shown,
            mode,
            error: None,
        }
    }

    #[test]
    fn the_power_row_reads_each_case() {
        use whipspot_core::power::PowerError;
        assert_eq!(power_text(&PowerStatus::default()), "Not managed");
        assert_eq!(
            power_text(&power(Some("Demo power"), false, true, PowerMode::Normal)),
            "Demo power, off in Settings"
        );
        let refused = PowerStatus {
            error: Some(PowerError::Refused),
            ..power(Some("Demo power"), true, false, PowerMode::Normal)
        };
        assert_eq!(power_text(&refused), "Demo power, the system refused");
        assert_eq!(
            power_text(&power(Some("Demo power"), true, false, PowerMode::Eco)),
            "Demo power, saving power (hidden, music at full speed)"
        );
        assert_eq!(
            power_text(&power(Some("Demo power"), true, false, PowerMode::EcoIdle)),
            "Demo power, saving power (hidden, nothing playing)"
        );
        assert_eq!(
            power_text(&power(Some("Demo power"), true, false, PowerMode::Normal)),
            "Demo power, normal"
        );
        assert_eq!(
            power_text(&power(Some("Demo power"), true, true, PowerMode::Normal)),
            "Demo power, normal (window shown)"
        );
    }

    #[test]
    fn the_text_has_a_power_section_with_the_row() {
        let shown = power(Some("Demo power"), true, true, PowerMode::Normal);
        let text = render_text(
            &AudioDiagnostics::default(),
            "",
            &OutputStatus::default(),
            &shown,
        );
        assert!(
            text.contains("\nPower\nEfficiency mode: Demo power, normal (window shown)\n"),
            "{text}"
        );
    }
}
