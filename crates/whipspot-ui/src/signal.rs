//! The signal path (MD53): the quality pill's text, the rows of its hover tooltip and the glass
//! card that shows them. The values come from the same formatting the Audio window uses
//! (`diagnostics.rs`); nothing here guesses: what the snapshot does not know reads "unknown".

use lune_ui::color::Rgba;
use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use whipspot_core::diagnostics::{AudioDiagnostics, Resampler, Stage, Verdict};
use whipspot_core::player::PlayerState;

use crate::diagnostics::{khz, or_unknown, resampler_text, text_or_unknown, verdict_parts};
use crate::draw::{
    P, alpha, caps, caps_width, disc, glass, glow, led, outline, shadow, text, vgradient, well,
};
use crate::icons::{Icon, icon};
use crate::theme::{Indicator, Labels, Look, lune_theme};

/// One row of the tooltip: its label, its value and the ok mark where one applies.
pub(crate) type Row = (&'static str, String, Option<bool>);

/// Which verdict the path has (colours the pill, the card's verdict box).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    BitPerfect,
    Processed,
    Resampled,
    Unknown,
}

/// The verdict as the card shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VerdictLine {
    /// The verdict's word ("BIT-PERFECT", "RESAMPLED", "path unknown", ...).
    pub word: &'static str,
    pub kind: Kind,
    /// The line inside the verdict box, right-aligned: the first reason (or a fixed phrase when
    /// there is none), then "lossy source" when the snapshot flags it.
    pub note: String,
    /// Further reasons and the output's fallback reason: one muted line each under the box.
    pub extra: Vec<String>,
}

/// The kind of `verdict`.
pub(crate) fn kind_of(verdict: Option<&Verdict>) -> Kind {
    match verdict {
        Some(Verdict::BitPerfect) => Kind::BitPerfect,
        Some(Verdict::LosslessProcessed { .. }) => Kind::Processed,
        Some(Verdict::Resampled { .. }) => Kind::Resampled,
        None => Kind::Unknown,
    }
}

/// The warm tone a resampled path is shown in: the toolkit theme's warning colour derived for
/// `look`.
pub(crate) fn warn(look: &Look) -> Rgba {
    lune_theme(look).status_warning
}

/// The colour the verdict wears (indicator, word, box): the accent when bit-perfect, light ink
/// when only processed, the warning tone when resampled, faint when unknown.
pub(crate) fn kind_color(look: &Look, kind: Kind) -> Rgba {
    match kind {
        Kind::BitPerfect => look.hi,
        Kind::Processed => look.ink.text2,
        Kind::Resampled => warn(look),
        Kind::Unknown => look.ink.faint,
    }
}

/// The quality pill's text: bit depth (or bitrate) and sample rate of the source, `None` when
/// the snapshot knows neither.
pub(crate) fn pill_text(diag: &AudioDiagnostics) -> Option<String> {
    let s = &diag.source;
    let lead = match s.lossless {
        Some(true) => Some(
            s.bit_depth
                .map_or_else(|| "Lossless".to_owned(), |b| format!("{b}-bit")),
        ),
        Some(false) | None => s
            .bitrate_nominal_kbps
            .or(s.bitrate_actual_kbps)
            .map(|k| format!("{k} kbps"))
            .or_else(|| s.bit_depth.map(|b| format!("{b}-bit"))),
    };
    let parts: Vec<String> = lead.into_iter().chain(s.sample_rate_hz.map(khz)).collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// `parts` joined with the card's separator, or "unknown" when there are none.
fn join(parts: Vec<String>) -> String {
    if parts.is_empty() {
        "unknown".to_owned()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// The six rows of the signal path: Source, Decoder, Resampling, DSP, Output mode, Device.
pub(crate) fn rows(diag: &AudioDiagnostics, state: &PlayerState) -> Vec<Row> {
    let (s, dc, dsp, o) = (&diag.source, &diag.decode, &diag.dsp, &diag.output);

    let mut source = Vec::new();
    if let Some(codec) = s.codec.as_ref() {
        source.push(codec.clone());
    }
    let figures: Vec<String> = [
        s.bit_depth.map(|b| format!("{b}-bit")).or_else(|| {
            (s.lossless != Some(true))
                .then(|| s.bitrate_nominal_kbps.or(s.bitrate_actual_kbps))
                .flatten()
                .map(|k| format!("{k} kbps"))
        }),
        s.sample_rate_hz.map(khz),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !figures.is_empty() {
        source.push(figures.join(" / "));
    }

    let mut decoder = Vec::new();
    if let Some(d) = dc.decoder.as_ref() {
        decoder.push(d.clone());
        if let Some(ch) = s.channels {
            decoder.push(format!("{ch} ch"));
        }
    }

    let resampling = or_unknown(dsp.resampler.as_ref(), resampler_text);
    let resampling_ok = dsp.resampler.as_ref().map(|r| matches!(r, Resampler::None));

    let stages = [
        ("EQ", dsp.eq),
        ("preamp", dsp.preamp),
        ("ReplayGain", dsp.replay_gain),
        ("volume", dsp.volume),
        ("balance", dsp.balance),
    ];
    let active: Vec<&str> = stages
        .iter()
        .filter(|(_, st)| *st == Some(Stage::Active))
        .map(|(name, _)| *name)
        .collect();
    let (dsp_text, dsp_ok) = if !active.is_empty() {
        (format!("Active: {}", active.join(", ")), Some(false))
    } else if stages.iter().any(|(_, st)| st.is_some()) {
        ("None active".to_owned(), Some(true))
    } else {
        ("unknown".to_owned(), None)
    };

    let mut mode = Vec::new();
    match (&o.host_api, &o.mode) {
        (Some(api), Some(m)) => mode.push(format!("{api} {m}")),
        (Some(x), None) | (None, Some(x)) => mode.push(x.clone()),
        (None, None) => {}
    }
    if let Some(b) = o.bit_depth {
        mode.push(format!("{b}-bit"));
    }
    let mode_ok = if state.output.fallback.is_some() {
        Some(false)
    } else {
        o.mode
            .as_deref()
            .filter(|m| matches!(*m, "exclusive" | "hog"))
            .map(|_| true)
    };

    let mut device = vec![text_or_unknown(&o.device_name)];
    if let Some(hz) = o.sample_rate_hz {
        device.push(khz(hz));
    }

    vec![
        ("Source", join(source), None),
        ("Decoder", join(decoder), None),
        ("Resampling", resampling, resampling_ok),
        ("DSP", dsp_text, dsp_ok),
        ("Output mode", join(mode), mode_ok),
        ("Device", join(device), None),
    ]
}

/// The verdict line and the extra lines for `diag` and the output's fallback in `state`.
pub(crate) fn verdict_line(diag: &AudioDiagnostics, state: &PlayerState) -> VerdictLine {
    let (word, reasons, _) = verdict_parts(diag.verdict.as_ref());
    let kind = kind_of(diag.verdict.as_ref());
    let mut note = match (reasons.first(), kind) {
        (Some(first), _) => first.clone(),
        (None, Kind::BitPerfect) => "samples untouched".to_owned(),
        (None, Kind::Unknown) => "no verdict yet".to_owned(),
        (None, _) => String::new(),
    };
    if diag.lossy_source == Some(true) {
        if !note.is_empty() {
            note.push_str(" \u{b7} ");
        }
        note.push_str("lossy source");
    }
    let mut extra: Vec<String> = reasons.iter().skip(1).cloned().collect();
    if let Some(reason) = state.output.fallback {
        extra.push(reason.to_owned());
    }
    VerdictLine {
        word,
        kind,
        note,
        extra,
    }
}

/// The output device's name for the card's header, when known.
pub(crate) fn header_device(diag: &AudioDiagnostics) -> Option<&str> {
    diag.output
        .device_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("none"))
}

/// The verdict word as the card's untracked themes set it: sentence case ("Bit-perfect"); the
/// tracked theme keeps the caps of [`VerdictLine::word`].
pub(crate) fn display_word(line: &VerdictLine) -> &'static str {
    match line.kind {
        Kind::BitPerfect => "Bit-perfect",
        Kind::Processed => "Lossless-processed",
        Kind::Resampled => "Resampled",
        Kind::Unknown => "Path unknown",
    }
}

/// The card's width.
const CARD_W: f32 = 392.0;
const ROW_H: f32 = 30.0;
/// The room kept between the card and the window's right edge.
const EDGE: f32 = 24.0;
/// The gap between the card and the panel under it.
const GAP: f32 = 14.0;
/// One extra muted line under the verdict box.
const EXTRA_H: f32 = 20.0;

/// The card's rectangle: above `panel`, centred on `pill` and kept inside the window of width
/// `win_w`; `rows` rows and `extra` extra lines tall.
pub(crate) fn card_rect(win_w: f32, panel: Rect, pill: Rect, rows: usize, extra: usize) -> Rect {
    let h = 64.0 + rows as f32 * ROW_H + 70.0 + extra as f32 * EXTRA_H;
    let pc = pill.x + pill.w / 2.0;
    let x = (pc - CARD_W / 2.0).min(win_w - EDGE - CARD_W).max(12.0);
    Rect::new(x, panel.y - GAP - h, CARD_W, h)
}

/// The ok mark of a row (`Some(true)`: the accent's mark; `Some(false)`: a small warning dot).
fn mark(p: &mut P<'_>, k: &Look, ok: bool, c: (f32, f32)) {
    if !ok {
        disc(p, c, 5.0, warn(k));
        return;
    }
    match k.decor().indicator {
        Indicator::Check => icon(p, Icon::Check, c, 11.0, k.a, 1.6),
        Indicator::Led => led(p, c, 5.0, k.a, k.hi, true),
    }
}

/// Draws the signal-path card above `panel` and returns its rectangle.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_card(
    p: &mut P<'_>,
    k: &Look,
    win_w: f32,
    panel: Rect,
    pill: Rect,
    rows: &[Row],
    line: &VerdictLine,
    device: Option<&str>,
) -> Rect {
    p.on(LayerId::Modal);
    let ink = k.ink;
    let tracked = k.decor().labels == Labels::Tracked;
    let r = card_rect(win_w, panel, pill, rows.len(), line.extra.len());

    let rad = k.radii().popup;
    shadow(p, r, rad, 26.0, if k.surface().bevel { 1.5 } else { 0.9 });
    glass(p, k, r, rad, k.surface().popup_alpha, false);

    let x0 = r.x + 22.0;
    if tracked {
        caps(p, "SIGNAL PATH", 10.0, ink.text2, x0, r.y + 22.0, 2.4);
        if let Some(d) = device {
            let d = d.to_uppercase();
            let dw = caps_width(p, &d, 9.0, 1.6).min(180.0);
            caps(
                p,
                &d,
                9.0,
                ink.faint,
                r.right() - 22.0 - dw,
                r.y + 23.0,
                1.6,
            );
        }
    } else {
        text(p, "Signal path", 15.0, ink.text, true, x0, r.y + 20.0, 0.0);
        if let Some(d) = device {
            let w = crate::draw::width(p, d, 12.5, false).min(180.0);
            text(
                p,
                d,
                12.5,
                ink.muted,
                false,
                r.right() - 22.0 - w,
                r.y + 22.0,
                180.0,
            );
        }
    }
    p.fill(
        Rect::new(r.x + 1.0, r.y + 54.0, r.w - 2.0, 1.0),
        0.0,
        ink.line_soft,
    );

    let vx = x0 + 116.0;
    let mut y = r.y + 66.0;
    for (key, value, ok) in rows {
        if tracked {
            caps(p, &key.to_uppercase(), 8.5, ink.muted, x0, y + 8.0, 1.4);
        } else {
            text(p, key, 12.5, ink.muted, false, x0, y + 6.0, 0.0);
        }
        let vw = text(
            p,
            value,
            13.0,
            ink.text,
            false,
            vx,
            y + 5.0,
            r.right() - 40.0 - vx,
        );
        if let Some(ok) = ok {
            mark(p, k, *ok, (vx + vw + 12.0, y + 14.0));
        }
        y += ROW_H;
    }

    // The verdict.
    let col = kind_color(k, line.kind);
    let v = Rect::new(x0, y + 12.0, CARD_W - 44.0, 44.0);
    let vr = (rad - 6.0).max(8.0);
    let word_end = if tracked {
        glow(
            p,
            v,
            vr,
            10.0,
            col,
            if line.kind == Kind::BitPerfect {
                0.4
            } else {
                0.2
            },
        );
        well(p, v, vr, true);
        p.push_clip_rounded(v, vr);
        vgradient(p, v, alpha(col, 0x08), alpha(col, 0x1E));
        p.pop_clip();
        led(
            p,
            (v.x + 20.0, v.y + 22.0),
            7.0,
            col,
            col,
            line.kind != Kind::Unknown,
        );
        v.x + 38.0 + caps(p, line.word, 11.5, col, v.x + 38.0, v.y + 14.0, 2.4)
    } else {
        outline(p, v, vr, alpha(col, 0x1A), alpha(col, 0x66));
        if line.kind == Kind::BitPerfect {
            icon(p, Icon::Check, (v.x + 22.0, v.y + 22.0), 14.0, col, 1.8);
        }
        let tx = if line.kind == Kind::BitPerfect {
            v.x + 40.0
        } else {
            v.x + 16.0
        };
        tx + text(p, display_word(line), 14.0, col, true, tx, v.y + 12.0, 0.0)
    };
    if !line.note.is_empty() {
        let room = (v.right() - 14.0 - word_end - 12.0).max(0.0);
        if room > 20.0 {
            let w = crate::draw::width(p, &line.note, 11.5, false).min(room);
            text(
                p,
                &line.note,
                11.5,
                ink.muted,
                false,
                v.right() - 14.0 - w,
                v.y + 14.5,
                room,
            );
        }
    }
    let mut ey = v.bottom() + 8.0;
    for extra in &line.extra {
        text(p, extra, 11.5, ink.muted, false, x0, ey, v.w);
        ey += EXTRA_H;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;
    use whipspot_core::diagnostics::Resampler;
    use whipspot_core::player::{
        EqState, LyricsState, OutputStatus, PlayState, QueueState, RepeatMode,
    };

    fn state() -> PlayerState {
        PlayerState {
            revision: 0,
            play_state: PlayState::Stopped,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume: 100,
            balance: 0,
            replay_gain: Default::default(),
            eq: EqState {
                enabled: false,
                preamp_db: 0.0,
                bands_db: [0.0; whipspot_core::engine::BAND_COUNT],
            },
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: QueueState {
                items: Arc::new(Vec::new()),
                current: None,
                next: None,
            },
            providers: Vec::new(),
            output_summary: String::new(),
            output: OutputStatus::default(),
            lyrics: LyricsState::Off,
            lyrics_enabled: false,
            remote: None,
        }
    }

    #[test]
    fn q1_lossy_pill() {
        let mut d = AudioDiagnostics::default();
        d.source.lossless = Some(false);
        d.source.bitrate_nominal_kbps = Some(320);
        d.source.sample_rate_hz = Some(44_100);
        assert_eq!(pill_text(&d).as_deref(), Some("320 kbps \u{b7} 44.1 kHz"));
    }

    #[test]
    fn q2_lossless_pill_with_depth() {
        let mut d = AudioDiagnostics::default();
        d.source.lossless = Some(true);
        d.source.bit_depth = Some(24);
        d.source.sample_rate_hz = Some(96_000);
        assert_eq!(pill_text(&d).as_deref(), Some("24-bit \u{b7} 96 kHz"));
    }

    #[test]
    fn q3_lossless_without_depth() {
        let mut d = AudioDiagnostics::default();
        d.source.lossless = Some(true);
        d.source.sample_rate_hz = Some(44_100);
        assert_eq!(pill_text(&d).as_deref(), Some("Lossless \u{b7} 44.1 kHz"));
    }

    #[test]
    fn q4_nothing_known_is_no_pill() {
        assert_eq!(pill_text(&AudioDiagnostics::default()), None);
    }

    #[test]
    fn s1_resampled_snapshot_gives_six_rows_and_the_reason() {
        let mut d = AudioDiagnostics::default();
        d.source.codec = Some("FLAC".into());
        d.source.bit_depth = Some(24);
        d.source.sample_rate_hz = Some(96_000);
        d.dsp.resampler = Some(Resampler::Active {
            algorithm: "sinc".into(),
            ratio: 0.5,
            quality: "high".into(),
        });
        d.verdict = Some(Verdict::Resampled {
            reasons: vec!["96 kHz to 48 kHz".into(), "second reason".into()],
        });
        d.lossy_source = Some(true);
        let mut s = state();
        s.output.fallback = Some("exclusive mode unavailable");

        let r = rows(&d, &s);
        let labels: Vec<_> = r.iter().map(|(l, _, _)| *l).collect();
        assert_eq!(
            labels,
            [
                "Source",
                "Decoder",
                "Resampling",
                "DSP",
                "Output mode",
                "Device"
            ]
        );
        assert_eq!(r[0].1, "FLAC \u{b7} 24-bit / 96 kHz");
        assert_eq!(r[1].1, "unknown");
        assert_eq!(r[2].1, "sinc, high, ratio 0.5000");
        assert_eq!(r[2].2, Some(false));
        assert_eq!(r[3].1, "unknown");
        assert_eq!(r[4].1, "unknown");
        assert_eq!(r[4].2, Some(false));
        assert_eq!(r[5].1, "unknown");

        let line = verdict_line(&d, &s);
        assert_eq!(line.word, "RESAMPLED");
        assert_eq!(line.kind, Kind::Resampled);
        assert_eq!(line.note, "96 kHz to 48 kHz \u{b7} lossy source");
        assert_eq!(line.extra, ["second reason", "exclusive mode unavailable"]);
    }

    #[test]
    fn bit_perfect_line_has_a_fixed_phrase() {
        let mut d = AudioDiagnostics::default();
        d.verdict = Some(Verdict::BitPerfect);
        let line = verdict_line(&d, &state());
        assert_eq!(
            (line.word, line.note.as_str()),
            ("BIT-PERFECT", "samples untouched")
        );
        assert!(line.extra.is_empty());
        let none = verdict_line(&AudioDiagnostics::default(), &state());
        assert_eq!(none.kind, Kind::Unknown);
    }

    #[test]
    fn header_leaves_out_an_unnamed_device_and_words_are_sentence_case() {
        let mut d = AudioDiagnostics::default();
        assert_eq!(header_device(&d), None);
        d.output.device_name = Some("none".into());
        assert_eq!(header_device(&d), None);
        d.output.device_name = Some(" ".into());
        assert_eq!(header_device(&d), None);
        d.output.device_name = Some("Studio DAC (USB)".into());
        assert_eq!(header_device(&d), Some("Studio DAC (USB)"));
        d.verdict = Some(Verdict::LosslessProcessed { reasons: vec![] });
        assert_eq!(
            display_word(&verdict_line(&d, &state())),
            "Lossless-processed"
        );
        assert_eq!(
            display_word(&verdict_line(&AudioDiagnostics::default(), &state())),
            "Path unknown"
        );
    }

    #[test]
    fn card_stays_inside_the_window_above_the_panel() {
        let panel = Rect::new(32.0, 812.0, 1376.0, 76.0);
        let pill = Rect::new(1100.0, 835.0, 150.0, 30.0);
        let r = card_rect(1440.0, panel, pill, 6, 0);
        assert_eq!(r.w, 392.0);
        assert!(r.right() <= 1440.0 - 24.0 + 0.01);
        assert!((r.bottom() - (panel.y - 14.0)).abs() < 0.01);
        let narrow = card_rect(760.0, Rect::new(32.0, 392.0, 696.0, 76.0), pill, 6, 2);
        assert!(narrow.x >= 12.0 - 0.01);
    }
}
