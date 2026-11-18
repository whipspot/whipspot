//! The two accent colours taken from a cover picture (MD49): a pure function over decoded RGBA,
//! so theme C can follow the playing cover once a provider supplies one (M6).

use lune_ui::color::Rgba;

/// The accents a cover gives: its dominant colour and a secondary one, both lifted to vivid,
/// legible values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverAccent {
    pub a: Rgba,
    pub b: Rgba,
}

/// Most pixels looked at; a larger picture is sampled on a regular grid.
const MAX_SAMPLES: u64 = 4096;
/// Hue histogram bins.
const BINS: usize = 18;
/// Below this mean saturation weight per sample the picture counts as greyscale.
const GREY_WEIGHT: f32 = 0.02;

type Rgb = [f32; 3];

/// The cover's mean luminance (Rec. 709 weights on the encoded values, 0 black to 1 white) over at
/// most 4096 sampled pixels; `None` for a length mismatch or an empty picture. Opaque pixels are
/// assumed (a cover is).
pub fn cover_luma(rgba: &[u8], width: u32, height: u32) -> Option<f32> {
    let (w, h) = (u64::from(width), u64::from(height));
    if w == 0 || h == 0 || (w * h).checked_mul(4)? != rgba.len() as u64 {
        return None;
    }
    let step = sample_step(w, h);
    let (mut sum, mut n) = (0.0f64, 0u32);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let i = ((y * w + x) * 4) as usize;
            let c = &rgba[i..i + 3];
            sum += 0.2126 * f64::from(c[0]) + 0.7152 * f64::from(c[1]) + 0.0722 * f64::from(c[2]);
            n += 1;
            x += step;
        }
        y += step;
    }
    Some((sum / f64::from(n.max(1)) / 255.0) as f32)
}

/// The cover's dominant hue and a secondary one at least 3 bins (60 degrees) away, or a hue 55
/// degrees on when the cover has no second colour. `rgba` is `width * height` straight-alpha RGBA
/// pixels. `None` for a length mismatch, an empty picture or a near-greyscale one. Looks at no
/// more than 4096 pixels and allocates nothing proportional to the picture.
pub fn cover_accent(rgba: &[u8], width: u32, height: u32) -> Option<CoverAccent> {
    let (w, h) = (u64::from(width), u64::from(height));
    if w == 0 || h == 0 || (w * h).checked_mul(4)? != rgba.len() as u64 {
        return None;
    }
    let step = sample_step(w, h);
    let mut weight = [0.0f32; BINS];
    let mut sum = [[0.0f32; 3]; BINS];
    let mut samples = 0u32;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let i = ((y * w + x) * 4) as usize;
            let c = &rgba[i..i + 4];
            let rgb = [
                f32::from(c[0]) / 255.0,
                f32::from(c[1]) / 255.0,
                f32::from(c[2]) / 255.0,
            ];
            let (hue, s, l) = to_hsl(rgb);
            // Colourful, mid-light pixels speak for the cover; greys and near-blacks barely count.
            let wt = s * (1.0 - ((l - 0.5).abs() * 1.7)).max(0.05);
            let b = ((hue / 360.0) * BINS as f32) as usize % BINS;
            weight[b] += wt;
            for (acc, v) in sum[b].iter_mut().zip(rgb) {
                *acc += v * wt;
            }
            samples += 1;
            x += step;
        }
        y += step;
    }
    let total: f32 = weight.iter().sum();
    if samples == 0 || total < GREY_WEIGHT * samples as f32 {
        return None;
    }
    let first = (0..BINS).max_by(|&a, &b| weight[a].total_cmp(&weight[b]))?;
    let dist = |a: usize, b: usize| {
        let d = a.abs_diff(b);
        d.min(BINS - d)
    };
    let second = (0..BINS)
        .filter(|&b| dist(b, first) >= 3)
        .max_by(|&a, &b| weight[a].total_cmp(&weight[b]))
        .filter(|&b| weight[b] > weight[first] * 0.06);
    let mean = |b: usize| -> Rgb {
        let w = weight[b].max(1e-6);
        [sum[b][0] / w, sum[b][1] / w, sum[b][2] / w]
    };
    let vivid = |rgb: Rgb, l: f32| -> Rgba {
        let (h, s, _) = to_hsl(rgb);
        let c = hsl(h, s.clamp(0.62, 0.9), l);
        [to8(c[0]), to8(c[1]), to8(c[2]), 255]
    };
    let a = mean(first);
    let b = match second {
        Some(b) => mean(b),
        None => {
            let (h, s, l) = to_hsl(a);
            hsl(h + 55.0, s, l)
        }
    };
    Some(CoverAccent {
        a: vivid(a, 0.64),
        b: vivid(b, 0.6),
    })
}

/// The grid step (in pixels, both ways) that keeps the sample count within [`MAX_SAMPLES`].
fn sample_step(w: u64, h: u64) -> u64 {
    let count = |s: u64| w.div_ceil(s) * h.div_ceil(s);
    let mut step = (((w * h) as f64 / MAX_SAMPLES as f64).sqrt().ceil() as u64).max(1);
    while count(step) > MAX_SAMPLES {
        step += step.div_ceil(8);
    }
    step
}

/// Hue (degrees), saturation and lightness of `c` (0..1 each channel).
fn to_hsl(c: Rgb) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-5 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-5);
    let h = if max == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    (h, s.clamp(0.0, 1.0), l)
}

/// The colour (0..1 each channel) of hue `h` degrees, saturation `s` and lightness `l`.
fn hsl(h: f32, s: f32, l: f32) -> Rgb {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r + m, g + m, b + m]
}

/// The hue of `c`, degrees (0..360).
pub(crate) fn hue_of(c: Rgba) -> f32 {
    to_hsl([
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
    ])
    .0
}

/// `c` with its hue turned by `degrees` (saturation and lightness kept): the rule for a second
/// colour when a cover gives none, also the third stop of theme C's links.
pub(crate) fn turn_hue(c: Rgba, degrees: f32) -> Rgba {
    let rgb = [
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
    ];
    let (h, s, l) = to_hsl(rgb);
    let t = hsl(h + degrees, s, l);
    [to8(t[0]), to8(t[1]), to8(t[2]), c[3]]
}

fn to8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn hue(c: Rgba) -> f32 {
        to_hsl([
            f32::from(c[0]) / 255.0,
            f32::from(c[1]) / 255.0,
            f32::from(c[2]) / 255.0,
        ])
        .0
    }

    /// The smaller angle between two hues, degrees.
    fn apart(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.repeat((w * h) as usize)
    }

    #[test]
    fn t1_solid_red_gives_a_red_accent() {
        let acc = cover_accent(&solid(32, 32, [0xE0, 0x10, 0x10, 0xFF]), 32, 32).expect("colour");
        assert!(apart(hue(acc.a), 0.0) < 20.0, "hue {}", hue(acc.a));
        assert_eq!(acc.a[3], 255);
    }

    #[test]
    fn t2_greyscale_gradient_is_none() {
        let mut px = Vec::new();
        for i in 0..32 * 32u32 {
            let v = (i % 256) as u8;
            px.extend([v, v, v, 0xFF]);
        }
        assert_eq!(cover_accent(&px, 32, 32), None);
    }

    #[test]
    fn luma_of_black_white_and_a_half_and_half_picture() {
        assert_eq!(cover_luma(&solid(8, 8, [0, 0, 0, 0xFF]), 8, 8), Some(0.0));
        let white = cover_luma(&solid(8, 8, [0xFF; 4]), 8, 8).unwrap();
        assert!((white - 1.0).abs() < 1e-4, "{white}");
        let mut px = solid(4, 4, [0, 0, 0, 0xFF]);
        for p in px.chunks_exact_mut(4).take(8) {
            p.copy_from_slice(&[0xFF; 4]);
        }
        let half = cover_luma(&px, 4, 4).unwrap();
        assert!((half - 0.5).abs() < 1e-4, "{half}");
        assert_eq!(cover_luma(&px, 3, 4), None);
        assert_eq!(cover_luma(&[], 0, 0), None);
    }

    #[test]
    fn t3_wrong_length_and_empty_are_none() {
        let px = solid(4, 4, [200, 30, 30, 255]);
        assert_eq!(cover_accent(&px, 4, 5), None);
        assert_eq!(cover_accent(&px[1..], 4, 4), None);
        assert_eq!(cover_accent(&[], 0, 0), None);
        assert_eq!(cover_accent(&px, 0, 4), None);
        assert_eq!(cover_accent(&px, 4, 0), None);
        assert_eq!(cover_accent(&px, u32::MAX, u32::MAX), None);
    }

    #[test]
    fn t4_blue_and_orange_halves_give_two_distinct_hues() {
        let mut px = Vec::new();
        for _ in 0..32 {
            for x in 0..32 {
                px.extend(if x < 16 {
                    [0x10, 0x20, 0xE0, 0xFF]
                } else {
                    [0xF0, 0x80, 0x10, 0xFF]
                });
            }
        }
        let acc = cover_accent(&px, 32, 32).expect("colour");
        assert!(apart(hue(acc.a), hue(acc.b)) >= 60.0);
    }

    #[test]
    fn t5_a_huge_picture_is_sampled() {
        let px = solid(4000, 4000, [0x30, 0xC0, 0x50, 0xFF]);
        let t = Instant::now();
        let acc = cover_accent(&px, 4000, 4000).expect("colour");
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(apart(hue(acc.a), 130.0) < 20.0);
    }

    #[test]
    fn sample_grid_stays_within_the_cap() {
        for (w, h) in [
            (1, 1),
            (64, 64),
            (65, 64),
            (4000, 4000),
            (100_000, 1),
            (1, 100_000),
        ] {
            let s = sample_step(w, h);
            assert!(
                w.div_ceil(s) * h.div_ceil(s) <= MAX_SAMPLES,
                "{w}x{h} step {s}"
            );
        }
    }
}
