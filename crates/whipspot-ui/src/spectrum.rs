//! The visualiser layer (MD51): the player's 32 bands with display smoothing (MD17) and peak caps,
//! interpolated to one bar per 15 px of window width and drawn full width along the window's
//! bottom, behind the sheet and the media control. The core stays raw and deterministic; falling
//! slowly and holding peaks are display matters.

use std::time::Duration;

use lune_ui::color::{Rgba, mix, premul};
use lune_ui::paint::Rect;
use whipspot_core::player::{PlayerHandle, SPECTRUM_BANDS, SpectrumFrame};

use crate::draw::{P, alpha, strip_edge};
use crate::theme::{Bars, Look};

/// How fast a bar falls, in level units (0..1) per second. A bar rises at once.
const FALL_PER_SECOND: f32 = 1.5;
/// The frame interval while the bars move (about 30 Hz).
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// A level this small counts as down (nothing to animate).
const REST: f32 = 0.001;
/// How long a peak cap holds before it falls, seconds.
const CAP_HOLD: f32 = 0.35;
/// How fast a peak cap falls once the hold is over, in level units per second.
const CAP_FALL_PER_SECOND: f32 = 1.2;
/// Window width per bar, logical px.
const BAR_PITCH: f32 = 15.0;
/// The visualiser's share of the window's height.
pub const HEIGHT_SHARE: f32 = 0.30;
/// The most strips a full-height solid or gradient bar is drawn in (MD51).
const MAX_STRIPS: usize = 24;
/// The segmented style's LED height and the gap between two (Q14: an exception to the strip cap,
/// at most `max_h / 5` fills per bar, none at rest).
const LED_SEGMENT: f32 = 4.0;
const LED_GAP: f32 = 1.0;
/// The space between a bar's top and its cap, logical px.
const CAP_GAP: f32 = 3.0;

/// The spectrum's retained state: the last raw frame and the smoothed levels.
#[derive(Debug, Default)]
pub struct Spectrum {
    frame: SpectrumFrame,
    levels: [f32; SPECTRUM_BANDS],
    last: Option<Duration>,
}

impl Spectrum {
    /// Reads the player's frame at app time `now`, moves the smoothed levels and returns the
    /// time step in seconds (0 on the first call, at most 0.25).
    pub fn update(&mut self, player: &PlayerHandle, now: Duration) -> f32 {
        player.spectrum(&mut self.frame);
        let dt = self
            .last
            .map_or(0.0, |t| now.saturating_sub(t).as_secs_f32().min(0.25));
        self.last = Some(now);
        for (i, level) in self.levels.iter_mut().enumerate() {
            let raw = self.frame.bands.get(i).copied().unwrap_or(0.0);
            *level = smooth(*level, raw, dt);
        }
        dt
    }

    /// The smoothed levels, one per band.
    pub fn levels(&self) -> &[f32] {
        &self.levels
    }

    /// Whether the player feeds the bars now.
    pub fn active(&self) -> bool {
        self.frame.active
    }
}

/// One smoothing step: up at once, down by at most `FALL_PER_SECOND * dt`.
fn smooth(level: f32, raw: f32, dt: f32) -> f32 {
    raw_level(raw).max(level - FALL_PER_SECOND * dt).max(0.0)
}

fn raw_level(raw: f32) -> f32 {
    if raw.is_finite() {
        raw.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The peak caps: each holds the highest level seen for [`CAP_HOLD`], then falls.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    caps: [f32; SPECTRUM_BANDS],
    hold: [f32; SPECTRUM_BANDS],
}

impl Default for Peaks {
    fn default() -> Self {
        Peaks {
            caps: [0.0; SPECTRUM_BANDS],
            hold: [0.0; SPECTRUM_BANDS],
        }
    }
}

impl Peaks {
    /// Moves every cap by `dt` seconds over the bars' `levels` (a short slice counts as silence
    /// for the missing bands).
    pub fn update(&mut self, levels: &[f32], dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        for i in 0..SPECTRUM_BANDS {
            let level = raw_level(levels.get(i).copied().unwrap_or(0.0));
            if level >= self.caps[i] {
                self.caps[i] = level;
                self.hold[i] = CAP_HOLD;
            } else if self.hold[i] > 0.0 {
                self.hold[i] -= dt;
            } else {
                self.caps[i] = level.max(self.caps[i] - CAP_FALL_PER_SECOND * dt);
            }
        }
    }

    /// The caps, one per band.
    pub fn caps(&self) -> &[f32] {
        &self.caps
    }

    /// Whether every cap is down (nothing to animate).
    pub fn at_rest(&self) -> bool {
        self.caps.iter().all(|&c| c <= REST)
    }
}

/// The bars to draw: `bands` interpolated linearly in band index onto `n` bars, the first bar
/// being band 0 and the last the last band.
pub fn interpolate(bands: &[f32], n: usize) -> Vec<f32> {
    let Some(last) = bands.len().checked_sub(1) else {
        return vec![0.0; n];
    };
    (0..n)
        .map(|i| {
            let pos = if n > 1 {
                i as f32 * last as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let lo = (pos.floor() as usize).min(last);
            let hi = (lo + 1).min(last);
            let t = pos - lo as f32;
            bands[lo] + (bands[hi] - bands[lo]) * t
        })
        .collect()
}

/// The number of bars of a window `width` px wide.
pub fn bar_count(width: f32) -> usize {
    ((width / BAR_PITCH).floor() as usize).max(1)
}

/// A fill above the layer's images and text.
fn over(p: &mut P<'_>, r: Rect, radius: f32, c: Rgba) {
    p.fill_over(r, radius, premul(c));
}

/// Draws the visualiser in `rect` (full width, bottom-anchored; its height is the tallest bar):
/// `levels` and `caps` are per band (0..1), in the look's bar style.
pub fn draw(p: &mut P<'_>, k: &Look, rect: Rect, levels: &[f32], caps: &[f32]) {
    if rect.w < 1.0 || rect.h < 1.0 {
        return;
    }
    let n = bar_count(rect.w);
    let bars = interpolate(levels, n);
    let tops = interpolate(caps, n);
    let pitch = rect.w / n as f32;
    let max_h = rect.h;
    let bottom = rect.bottom();
    let slice = max_h / MAX_STRIPS as f32;
    // Strip `j` counted up from the bottom; inner edges share one device-pixel-snapped position.
    let edge = |p: &P<'_>, j: usize| strip_edge(p, bottom, -max_h, j, MAX_STRIPS);
    let style = k.decor().bars;
    for (i, (&l, &c)) in bars.iter().zip(&tops).enumerate() {
        let h = l.clamp(0.0, 1.0) * max_h;
        let cap_h = c.clamp(0.0, 1.0) * max_h;
        let (bw, cap_t, cap_r, cap_c) = match style {
            Bars::Solid => (
                (pitch - 4.0).floor().max(1.0),
                3.0,
                1.5,
                alpha(k.ink.light, 0x8C),
            ),
            Bars::Segmented => ((pitch * 0.62).round().max(1.0), 2.0, 0.5, alpha(k.hi, 0xC8)),
            Bars::Gradient => ((pitch - 5.0).floor().max(1.0), 2.0, 1.0, alpha(k.hi, 0xB0)),
        };
        let x = (i as f32 * pitch + (pitch - bw) / 2.0).round();
        if h >= 1.0 {
            match style {
                Bars::Solid | Bars::Gradient => {
                    let strips = ((h / slice).ceil() as usize).clamp(1, MAX_STRIPS);
                    for j in 0..strips {
                        let y1 = edge(p, j);
                        let y0 = if j + 1 == strips {
                            bottom - h
                        } else {
                            edge(p, j + 1)
                        };
                        if y1 <= y0 {
                            continue;
                        }
                        let f = ((bottom - (y0 + y1) / 2.0) / max_h).clamp(0.0, 1.0);
                        let (colour, radius) = if style == Bars::Solid {
                            let top = j + 1 == strips;
                            (
                                mix(alpha(k.a, 0x14), alpha(k.hi, 0xB4), f),
                                if top { 2.0 } else { 0.0 },
                            )
                        } else {
                            (mix(alpha(k.b, 0x20), alpha(k.a, 0xB0), f.powf(0.9)), 0.0)
                        };
                        over(p, Rect::new(x, y0, bw, y1 - y0), radius, colour);
                    }
                }
                Bars::Segmented => {
                    // The LED ladder of round 3 (Q14): finer than the strip cap, for this style only.
                    let step = LED_SEGMENT + LED_GAP;
                    let segs = (h / step).floor() as usize;
                    for j in 0..segs {
                        let y = bottom - (j + 1) as f32 * step + LED_GAP;
                        let own = j as f32 / segs.max(1) as f32;
                        let reach = 0.55 + 0.45 * l;
                        let a = (f32::from(0x38_u8) + own * f32::from(0xA8_u8)) as u8;
                        over(
                            p,
                            Rect::new(x, y, bw, LED_SEGMENT),
                            0.5,
                            alpha(k.at(own * reach), a),
                        );
                    }
                }
            }
        }
        if cap_h >= 1.0 {
            over(
                p,
                Rect::new(x, bottom - cap_h - CAP_GAP - cap_t, bw, cap_t),
                cap_r,
                cap_c,
            );
        }
    }
    if style == Bars::Segmented {
        // A low warm haze where the bars stand.
        let haze = Rect::new(rect.x, bottom - 140.0, rect.w, 140.0);
        let steps = MAX_STRIPS;
        for s in 0..steps {
            let t = (s as f32 + 0.5) / steps as f32;
            let (y0, y1) = (
                strip_edge(p, haze.y, haze.h, s, steps),
                strip_edge(p, haze.y, haze.h, s + 1, steps),
            );
            if y1 > y0 {
                over(
                    p,
                    Rect::new(haze.x, y0, haze.w, y1 - y0),
                    0.0,
                    mix(alpha(k.a, 0x00), alpha(k.a, 0x14), t),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_a_cap_holds_then_falls_and_reaches_rest() {
        let mut peaks = Peaks::default();
        assert!(peaks.at_rest());
        let mut loud = [0.0; SPECTRUM_BANDS];
        loud[3] = 0.9;
        peaks.update(&loud, 0.033);
        assert_eq!(peaks.caps()[3], 0.9);
        assert!(!peaks.at_rest());
        let silence = [0.0; SPECTRUM_BANDS];
        // Held: 0.30 s of silence leaves the cap where it was.
        for _ in 0..9 {
            peaks.update(&silence, 0.033);
        }
        assert_eq!(peaks.caps()[3], 0.9);
        // Then it falls, never below the level, and settles within 2 s of silence.
        peaks.update(&silence, 0.1);
        peaks.update(&silence, 0.1);
        assert!(peaks.caps()[3] < 0.9);
        let mut t = 0.0;
        while !peaks.at_rest() && t < 2.0 {
            peaks.update(&silence, 0.033);
            t += 0.033;
        }
        assert!(peaks.at_rest(), "still falling after 2 s");
    }

    #[test]
    fn p1_a_cap_never_falls_below_a_bar() {
        let mut peaks = Peaks::default();
        let mut levels = [0.0; SPECTRUM_BANDS];
        levels[0] = 1.0;
        peaks.update(&levels, 0.033);
        levels[0] = 0.4;
        for _ in 0..100 {
            peaks.update(&levels, 0.033);
            assert!(peaks.caps()[0] >= 0.4);
        }
        assert_eq!(peaks.caps()[0], 0.4);
    }

    #[test]
    fn p2_bands_interpolate_onto_bars_monotonically_at_the_ends() {
        let bands: Vec<f32> = (0..SPECTRUM_BANDS).map(|i| i as f32 / 31.0).collect();
        let bars = interpolate(&bands, 96);
        assert_eq!(bars.len(), 96);
        assert_eq!(bars[0], bands[0]);
        assert!((bars[95] - bands[31]).abs() < 1e-6);
        assert!(bars.windows(2).all(|w| w[1] >= w[0]));
        // A single bar is band 0; an empty input is silence.
        assert_eq!(interpolate(&bands, 1), vec![0.0]);
        assert_eq!(interpolate(&[], 3), vec![0.0; 3]);
    }

    #[test]
    fn bars_are_one_per_15_px() {
        assert_eq!(bar_count(1440.0), 96);
        assert_eq!(bar_count(760.0), 50);
        assert_eq!(bar_count(3.0), 1);
    }

    #[test]
    fn levels_fall_by_the_set_rate() {
        assert_eq!(smooth(1.0, 0.0, 0.2), 0.7);
        assert_eq!(smooth(0.2, 0.9, 0.2), 0.9);
        assert_eq!(smooth(0.0, f32::NAN, 0.2), 0.0);
    }
}
