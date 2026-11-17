//! Synthetic cover art for the demo and the tests: a deterministic square picture per seed (a
//! gradient field with one geometric motif), a port of the round-3 mockup's `render`. Pure
//! pixel math; no real artwork (hard rule 4).

type Rgb = [f32; 3];

fn to8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9e37_79b9_7f4a_7c15)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

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

fn mix3(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

struct Palette {
    base: Rgb,
    deep: Rgb,
    light: Rgb,
    pop: Rgb,
}

impl Palette {
    fn of(rng: &mut Rng) -> Palette {
        let h = rng.range(0.0, 360.0);
        let offsets = [28.0, 150.0, 180.0, 210.0, -40.0];
        let off = offsets[(rng.next() % offsets.len() as u64) as usize];
        Palette {
            base: hsl(h, rng.range(0.5, 0.75), rng.range(0.42, 0.55)),
            deep: hsl(h + off * 0.25, rng.range(0.45, 0.7), rng.range(0.08, 0.16)),
            light: hsl(h + 12.0, rng.range(0.55, 0.8), rng.range(0.74, 0.84)),
            pop: hsl(h + off, rng.range(0.65, 0.85), rng.range(0.52, 0.62)),
        }
    }
}

/// One cover for `seed` at `size` x `size` px: straight RGBA, opaque. The same input gives the
/// same output.
pub fn render(seed: &str, size: u32) -> Vec<u8> {
    let n = size;
    let mut rng = Rng::new(hash(seed));
    let pal = Palette::of(&mut rng);
    let style = rng.next() % 6;
    let aa = 1.6 / n as f32;
    // Per-style parameters drawn up front so every pixel sees the same ones.
    let params: [f32; 12] = std::array::from_fn(|_| rng.f());
    let mut out = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32;
            let v = (y as f32 + 0.5) / n as f32;
            let c = match style {
                0 => bloom(&pal, &params, u, v),
                1 => rings(&pal, &params, u, v, aa),
                2 => sun(&pal, &params, u, v, aa),
                3 => bauhaus(&pal, &params, u, v, aa),
                4 => hills(&pal, &params, u, v, aa),
                _ => prism(&pal, &params, u, v, aa),
            };
            // Grain and a soft vignette.
            let g = (grain(x, y) - 0.5) * 0.04;
            let d = (u - 0.5).powi(2) + (v - 0.5).powi(2);
            let k = 1.0 - 0.28 * d;
            let i = ((y * n + x) * 4) as usize;
            out[i] = to8(c[0] * k + g);
            out[i + 1] = to8(c[1] * k + g);
            out[i + 2] = to8(c[2] * k + g);
            out[i + 3] = 255;
        }
    }
    out
}

/// A near-white "paper" cover for `seed` (a light cover for the legibility checks): an off-white
/// sheet with a soft orange blob and a dark ring, as a pale illustration would have. Straight RGBA,
/// opaque; the mean luminance is about 0.85.
pub fn render_paper(seed: &str, size: u32) -> Vec<u8> {
    let n = size;
    let mut rng = Rng::new(hash(seed));
    let (cx, cy) = (rng.range(0.4, 0.6), rng.range(0.4, 0.6));
    let r = rng.range(0.18, 0.26);
    let mut out = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32;
            let v = (y as f32 + 0.5) / n as f32;
            let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
            let mut c = [0.95, 0.94, 0.91];
            c = mix3(c, [0.96, 0.42, 0.18], 1.0 - smooth(r - 0.01, r + 0.01, d));
            let ring = 1.0 - smooth(0.0, 0.012, (d - (r + 0.04)).abs());
            c = mix3(c, [0.08, 0.07, 0.07], ring * 0.9);
            let g = (grain(x, y) - 0.5) * 0.03;
            let i = ((y * n + x) * 4) as usize;
            out[i] = to8(c[0] + g);
            out[i + 1] = to8(c[1] + g);
            out[i + 2] = to8(c[2] + g);
            out[i + 3] = 255;
        }
    }
    out
}

fn grain(x: u32, y: u32) -> f32 {
    let mut h = (x.wrapping_mul(374_761_393)) ^ (y.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Soft colour blobs over a diagonal gradient.
fn bloom(p: &Palette, q: &[f32; 12], u: f32, v: f32) -> Rgb {
    let mut c = mix3(p.deep, p.base, (u * 0.4 + (1.0 - v) * 0.6) * 0.8);
    let blobs = [
        (q[0], q[1] * 0.6, 0.30 + q[2] * 0.15, p.pop),
        (q[3], 0.5 + q[4] * 0.5, 0.25 + q[5] * 0.15, p.light),
        (q[6] * 0.6 + 0.2, q[7], 0.18 + q[8] * 0.12, p.base),
    ];
    for (cx, cy, r, col) in blobs {
        let d2 = (u - cx).powi(2) + (v - cy).powi(2);
        let w = (-d2 / (r * r)).exp();
        c = mix3(c, col, w * 0.85);
    }
    c
}

/// Concentric rings from an off-centre point.
fn rings(p: &Palette, q: &[f32; 12], u: f32, v: f32, aa: f32) -> Rgb {
    let (cx, cy) = (0.2 + q[0] * 0.6, 0.2 + q[1] * 0.6);
    let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
    let k = 7.0 + q[2] * 5.0;
    let f = (d * k).fract();
    let edge = aa * k;
    let band = smooth(0.5 - edge, 0.5 + edge, f) * (1.0 - smooth(1.0 - edge, 1.0, f));
    let bg = mix3(p.deep, p.base, (1.0 - d).clamp(0.0, 1.0) * 0.7);
    let ring = mix3(p.pop, p.light, d.clamp(0.0, 1.0));
    let fade = (1.0 - d * 0.9).clamp(0.15, 1.0);
    mix3(bg, ring, band * fade)
}

/// A striped sun over a horizon.
fn sun(p: &Palette, q: &[f32; 12], u: f32, v: f32, aa: f32) -> Rgb {
    let horizon = 0.66 + q[0] * 0.08;
    let sky = mix3(p.deep, p.pop, (v / horizon).clamp(0.0, 1.0).powf(1.6));
    let (cx, cy, r) = (0.5, horizon - 0.05, 0.26 + q[1] * 0.06);
    let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
    let inside = 1.0 - smooth(r - aa, r + aa, d);
    let sunc = mix3(
        p.light,
        p.base,
        ((v - (cy - r)) / (2.0 * r)).clamp(0.0, 1.0),
    );
    // Cuts in the sun's lower half, wider towards the bottom.
    let t = ((v - cy + r * 0.1) / r).clamp(0.0, 1.0);
    let cut = if t > 0.0 {
        let f = (v * 34.0).fract();
        let w = 0.12 + t * 0.45;
        smooth(1.0 - w - 0.04, 1.0 - w, f)
    } else {
        0.0
    };
    let mut c = mix3(sky, sunc, inside * (1.0 - cut));
    if v > horizon {
        let g = (v - horizon) / (1.0 - horizon);
        let ground = mix3(p.deep, [0.0, 0.0, 0.0], g * 0.6);
        // Perspective lines.
        let x = (u - 0.5) / (0.15 + g);
        let line = 1.0 - smooth(0.0, 0.06, ((x * 4.0).fract() - 0.5).abs() * 2.0 - 0.9);
        c = mix3(ground, p.pop, line * 0.35 * g + 0.0);
        let edge = 1.0 - smooth(0.0, 2.0 * aa, v - horizon);
        c = mix3(c, p.light, edge * 0.8);
    }
    c
}

/// A 3x3 grid of circles and quarter circles.
fn bauhaus(p: &Palette, q: &[f32; 12], u: f32, v: f32, aa: f32) -> Rgb {
    let cells = 3.0;
    let (cx, cy) = ((u * cells).floor(), (v * cells).floor());
    let (fx, fy) = ((u * cells).fract(), (v * cells).fract());
    let idx = (cx + cy * cells) as usize;
    let seed = (q[idx % 12] * 1000.0) as u32 + idx as u32 * 7;
    let cols = [p.deep, p.base, p.pop, p.light];
    let bg = cols[(seed % 4) as usize];
    let fg = cols[((seed / 4 + 1 + seed % 4) % 4) as usize];
    let fg = if fg == bg {
        cols[(seed as usize + 2) % 4]
    } else {
        fg
    };
    let aa = aa * cells;
    let shape = match seed % 3 {
        0 => {
            let d = ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt();
            1.0 - smooth(0.36 - aa, 0.36 + aa, d)
        }
        1 => {
            let (ox, oy) = match (seed / 3) % 4 {
                0 => (0.0, 0.0),
                1 => (1.0, 0.0),
                2 => (1.0, 1.0),
                _ => (0.0, 1.0),
            };
            let d = ((fx - ox).powi(2) + (fy - oy).powi(2)).sqrt();
            1.0 - smooth(1.0 - aa, 1.0 + aa, d)
        }
        _ => {
            let d = ((fx - 0.5).powi(2) + (fy - 1.0).powi(2)).sqrt();
            1.0 - smooth(0.5 - aa, 0.5 + aa, d)
        }
    };
    mix3(bg, fg, shape)
}

/// Layered hills under a pale sky with a moon. The 6.28 stays as in the mockup so the pixels
/// match the approved reference.
#[allow(clippy::approx_constant)]
fn hills(p: &Palette, q: &[f32; 12], u: f32, v: f32, aa: f32) -> Rgb {
    let mut c = mix3(p.light, p.base, v * 0.9);
    let (mx, my) = (0.25 + q[0] * 0.5, 0.18 + q[1] * 0.12);
    let md = ((u - mx).powi(2) + (v - my).powi(2)).sqrt();
    c = mix3(c, [1.0, 0.98, 0.93], 1.0 - smooth(0.08 - aa, 0.08 + aa, md));
    for k in 0..4 {
        let kf = k as f32;
        let y = 0.42
            + kf * 0.14
            + 0.05 * (u * (5.0 + kf * 2.3) + q[2 + k] * 6.28).sin()
            + 0.025 * (u * (13.0 + kf) + q[6 + k] * 6.28).sin();
        let inside = smooth(y - aa, y + aa, v);
        let col = mix3(p.pop, p.deep, (kf + 1.0) / 4.0);
        c = mix3(c, col, inside);
    }
    c
}

/// Diagonal bands with a bold triangle.
fn prism(p: &Palette, q: &[f32; 12], u: f32, v: f32, aa: f32) -> Rgb {
    let t = (u + v) * 0.5;
    let bands = 5.0 + (q[0] * 4.0).floor();
    let b = (t * bands).floor() / bands;
    let mut c = mix3(p.deep, p.base, b);
    // A triangle pointing up.
    let (cx, base_y, top_y) = (0.5 + (q[1] - 0.5) * 0.2, 0.82, 0.22);
    let half_w = 0.34 * (v - top_y) / (base_y - top_y);
    let inside_v = smooth(top_y - aa, top_y + aa, v) * (1.0 - smooth(base_y - aa, base_y + aa, v));
    let inside_h = 1.0 - smooth(half_w - aa, half_w + aa, (u - cx).abs());
    let tri = mix3(p.pop, p.light, (v - top_y) / (base_y - top_y));
    c = mix3(c, tri, inside_v * inside_h * 0.92);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_deterministic_and_sized() {
        let a = render("album-1", 64);
        assert_eq!(a.len(), 64 * 64 * 4);
        assert_eq!(a, render("album-1", 64));
        assert_ne!(a, render("album-2", 64));
        assert!(a.chunks(4).all(|p| p[3] == 255));
    }

    #[test]
    fn every_motif_renders() {
        // Different seeds reach all six styles; none may panic or come out flat.
        for i in 0..24 {
            let img = render(&format!("seed-{i}"), 32);
            let first = &img[..4];
            assert!(img.chunks(4).any(|p| p != first), "seed-{i} is flat");
        }
    }
}
