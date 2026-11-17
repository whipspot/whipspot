//! The skin's spectrum: the player's 32 bands, smoothed by the native view's [`Spectrum`] and
//! [`Peaks`], cut into bars 3 skin pixels wide with a 1 pixel gap, and turned into the coloured
//! cells a drawer fills. Pure apart from [`VisState::update`], so the cells are tested without a
//! window.

use std::time::Duration;

use whipspot_core::player::PlayerHandle;
use whipspot_core::skin::{Rgb, VisPalette};

use crate::spectrum::{Peaks, Spectrum};

/// A level this small counts as down.
const REST: f32 = 0.001;
/// A bar's width in skin pixels and the gap after it.
const BAR_W: u32 = 3;
const BAR_PITCH: u32 = BAR_W + 1;

/// The smoothed spectrum and its peak caps of one panel.
#[derive(Default)]
pub(super) struct VisState {
    spectrum: Spectrum,
    peaks: Peaks,
}

impl VisState {
    /// Reads the player's frame at app time `now` and moves the levels and the caps.
    pub fn update(&mut self, player: &PlayerHandle, now: Duration) {
        let dt = self.spectrum.update(player, now);
        self.peaks.update(self.spectrum.levels(), dt);
    }

    /// Whether anything still moves: the player feeds the bars, or a level or a cap is up.
    pub fn moving(&self) -> bool {
        self.spectrum.active()
            || !self.peaks.at_rest()
            || self.spectrum.levels().iter().any(|&l| l > REST)
    }

    /// The bar levels and caps for a visualiser `width` skin pixels wide.
    pub fn bars(&self, width: u32) -> (Vec<f32>, Vec<f32>) {
        let n = bar_count(width);
        (
            bar_levels(self.spectrum.levels(), n),
            bar_levels(self.peaks.caps(), n),
        )
    }
}

/// How many bars fit `width` skin pixels: 3 wide with 1 between, none after the last.
pub(super) fn bar_count(width: u32) -> usize {
    ((width + 1) / BAR_PITCH) as usize
}

/// `n` bar levels from the bands: each the highest of its share of the bands (at least one).
pub(super) fn bar_levels(bands: &[f32], n: usize) -> Vec<f32> {
    let len = bands.len();
    (0..n)
        .map(|i| {
            if len == 0 {
                return 0.0;
            }
            let lo = (i * len / n).min(len - 1);
            let hi = ((i + 1) * len / n).max(lo + 1).min(len);
            bands[lo..hi]
                .iter()
                .map(|l| {
                    if l.is_finite() {
                        l.clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                })
                .fold(0.0, f32::max)
        })
        .collect()
}

/// The colours a visualiser is drawn with.
pub(super) struct Colours {
    pub background: Rgb,
    /// From the top row down; one colour fills every row.
    pub bars: Vec<Rgb>,
    pub peak: Rgb,
}

impl Colours {
    /// The skin's palette.
    pub fn palette(p: &VisPalette) -> Colours {
        Colours {
            background: p.background,
            bars: p.bars.clone(),
            peak: p.peak,
        }
    }

    /// The toolkit theme's colours when the skin has no palette.
    pub fn theme(background: Rgb, bar: Rgb, peak: Rgb) -> Colours {
        Colours {
            background,
            bars: vec![bar],
            peak,
        }
    }

    /// The colour of row `row` (0 is the top) of a `height` rows tall visualiser: absolute, so a
    /// short bar shows the lower colours only.
    fn bar(&self, row: u32, height: u32) -> Rgb {
        let len = self.bars.len();
        if len == 0 || height == 0 {
            return self.background;
        }
        self.bars[(row as usize * len / height as usize).min(len - 1)]
    }
}

/// A filled block of the visualiser, in skin pixels from its top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cell {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub colour: Rgb,
}

/// The lit rows of a bar of `level` in `height` rows.
fn lit_rows(level: f32, height: u32) -> u32 {
    let l = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((l * height as f32).round() as u32).min(height)
}

/// The cells above the background: per bar the lit rows (runs of one colour as one block), then
/// its peak as one row at the cap, 1 pixel tall; nothing for a cap at rest.
pub(super) fn cells(height: u32, levels: &[f32], caps: &[f32], colours: &Colours) -> Vec<Cell> {
    let mut out = Vec::new();
    for (i, &level) in levels.iter().enumerate() {
        let x = i as u32 * BAR_PITCH;
        let lit = lit_rows(level, height);
        let mut row = height - lit;
        while row < height {
            let colour = colours.bar(row, height);
            let mut end = row + 1;
            while end < height && colours.bar(end, height) == colour {
                end += 1;
            }
            out.push(Cell {
                x,
                y: row,
                w: BAR_W,
                h: end - row,
                colour,
            });
            row = end;
        }
        let cap = caps.get(i).copied().unwrap_or(0.0);
        if cap > REST {
            let rows = lit_rows(cap, height).max(1);
            out.push(Cell {
                x,
                y: height - rows,
                w: BAR_W,
                h: 1,
                colour: colours.peak,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    fn colours() -> Colours {
        // Four colours over 16 rows: four rows each.
        Colours {
            background: rgb(0, 0, 0),
            bars: vec![
                rgb(250, 0, 0),
                rgb(250, 250, 0),
                rgb(0, 250, 0),
                rgb(0, 100, 0),
            ],
            peak: rgb(255, 255, 255),
        }
    }

    #[test]
    fn a_76_pixel_visualiser_has_19_bars() {
        assert_eq!(bar_count(76), 19);
        assert_eq!(bar_count(3), 1);
        assert_eq!(bar_count(2), 0);
        assert_eq!(bar_count(0), 0);
    }

    #[test]
    fn every_bar_takes_the_highest_band_of_its_share() {
        let mut bands = [0.0f32; 32];
        bands[0] = 0.2;
        bands[1] = 0.9;
        bands[31] = 0.5;
        let bars = bar_levels(&bands, 19);
        assert_eq!(bars.len(), 19);
        // Bar 0 covers band 0, bar 1 bands 1 and 2, the last bar bands 30 and 31.
        assert_eq!(bars[0], 0.2);
        assert_eq!(bars[1], 0.9);
        assert_eq!(bars[18], 0.5);
        // More bars than bands: every bar still has a band.
        assert_eq!(bar_levels(&[0.4, 0.8], 5).len(), 5);
        assert!(bar_levels(&[], 3).iter().all(|&l| l == 0.0));
        assert!(bar_levels(&[f32::NAN, 2.0], 2) == vec![0.0, 1.0]);
    }

    #[test]
    fn lit_rows_are_coloured_by_their_absolute_row() {
        // Half height: rows 8..16 are lit, so only the lower two colours show.
        let cells = cells(16, &[0.5], &[0.0], &colours());
        assert_eq!(
            cells,
            [
                Cell {
                    x: 0,
                    y: 8,
                    w: 3,
                    h: 4,
                    colour: rgb(0, 250, 0)
                },
                Cell {
                    x: 0,
                    y: 12,
                    w: 3,
                    h: 4,
                    colour: rgb(0, 100, 0)
                },
            ]
        );
    }

    #[test]
    fn a_full_bar_shows_every_colour_from_the_top() {
        let cells = cells(16, &[1.0], &[0.0], &colours());
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].y, 0);
        assert_eq!(cells[0].colour, rgb(250, 0, 0));
        assert_eq!(cells[3].colour, rgb(0, 100, 0));
    }

    #[test]
    fn bars_sit_four_pixels_apart_and_silence_draws_nothing() {
        let c = colours();
        assert!(cells(16, &[0.0, 0.0], &[0.0, 0.0], &c).is_empty());
        let two = cells(16, &[1.0, 1.0], &[0.0, 0.0], &c);
        assert_eq!(two.iter().map(|c| c.x).max(), Some(4));
        assert!(two.iter().all(|c| c.w == 3));
    }

    #[test]
    fn a_peak_is_one_row_at_its_cap() {
        let c = colours();
        let cells = cells(16, &[0.25], &[0.75], &c);
        let peak = cells.last().unwrap();
        assert_eq!(peak.colour, c.peak);
        assert_eq!((peak.y, peak.h, peak.w), (4, 1, 3));
        // The bar below it is untouched: 4 lit rows from row 12.
        assert_eq!(cells[0].y, 12);
    }

    #[test]
    fn a_theme_palette_fills_every_row_with_one_colour_in_one_block() {
        let c = Colours::theme(rgb(0, 0, 0), rgb(1, 2, 3), rgb(9, 9, 9));
        let cells = cells(16, &[0.5], &[0.0], &c);
        assert_eq!(
            cells,
            [Cell {
                x: 0,
                y: 8,
                w: 3,
                h: 8,
                colour: rgb(1, 2, 3)
            }]
        );
    }
}
