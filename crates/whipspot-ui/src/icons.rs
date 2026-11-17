//! Thin line icons, stroked as polylines (no font glyphs), so every symbol shares one hairline
//! weight with the glass outlines, plus the filled heart, play, pause and skip.

use std::f32::consts::{PI, TAU};

use lune_ui::color::Rgba;
use lune_ui::paint::{Rect, StrokeStyle};

use crate::draw::{P, disc};

/// The icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Shuffle,
    Repeat,
    Heart,
    /// The output device; its button is hidden for now (kept for a later use).
    #[allow(dead_code)]
    Speaker,
    Queue,
    Gear,
    Search,
    ChevL,
    ChevR,
    Check,
    Note,
    Clock,
    /// An open eye: the browse view hides by itself when the pointer rests.
    Eye,
    /// An eye struck through: the browse view stays.
    EyeOff,
    /// Waves either side of a dot: a streaming provider.
    Stream,
    /// A folder: a provider of files on this computer.
    Folder,
    /// A disc: any other provider.
    Disc,
}

/// Points in the icon's unit box (-1..1 both ways) mapped onto the box of `size` about `c`.
struct Frame {
    c: (f32, f32),
    h: f32,
}

impl Frame {
    fn pt(&self, x: f32, y: f32) -> [f32; 2] {
        [self.c.0 + x * self.h, self.c.1 + y * self.h]
    }
    fn pts(&self, v: &[(f32, f32)]) -> Vec<[f32; 2]> {
        v.iter().map(|&(x, y)| self.pt(x, y)).collect()
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// The heart's outline in the unit box: two round lobes meeting in a notch, tapering to a tip.
fn heart_pts(b: &Frame) -> Vec<[f32; 2]> {
    let (r, cy, cx) = (0.44, -0.26, 0.42);
    let mut v = vec![b.pt(0.0, 0.84)];
    let lobe = |v: &mut Vec<[f32; 2]>, x: f32, from: f32, to: f32| {
        for i in 0..=20 {
            let a = (from + (to - from) * i as f32 / 20.0).to_radians();
            v.push(b.pt(x + r * a.cos(), cy + r * a.sin()));
        }
    };
    lobe(&mut v, -cx, 140.0, 355.0);
    lobe(&mut v, cx, 185.0, 400.0);
    v
}

/// Draws `icon` in a `size` box centred on `c`, stroked `w` wide in `color`.
pub(crate) fn icon(p: &mut P<'_>, icon: Icon, c: (f32, f32), size: f32, color: Rgba, w: f32) {
    let b = Frame { c, h: size / 2.0 };
    let st = StrokeStyle::solid(color, w);
    let line = |p: &mut P<'_>, v: &[(f32, f32)]| p.stroke(&b.pts(v), false, st);
    match icon {
        Icon::Play => p.stroke(
            &b.pts(&[(-0.5, -0.78), (0.82, 0.0), (-0.5, 0.78)]),
            true,
            st,
        ),
        Icon::Pause => {
            line(p, &[(-0.36, -0.72), (-0.36, 0.72)]);
            line(p, &[(0.36, -0.72), (0.36, 0.72)]);
        }
        Icon::Next | Icon::Prev => {
            let k = if icon == Icon::Next { 1.0 } else { -1.0 };
            p.stroke(
                &b.pts(&[(-0.7 * k, -0.66), (0.38 * k, 0.0), (-0.7 * k, 0.66)]),
                true,
                st,
            );
            line(p, &[(0.72 * k, -0.66), (0.72 * k, 0.66)]);
        }
        Icon::Shuffle => {
            for k in [-1.0f32, 1.0] {
                let mut v = vec![(-0.92, -0.52 * k)];
                for i in 0..=14 {
                    let t = i as f32 / 14.0;
                    v.push((-0.42 + 0.84 * t, (-0.52 + 1.04 * smooth(t)) * k));
                }
                v.push((0.86, 0.52 * k));
                line(p, &v);
                line(
                    p,
                    &[
                        (0.6, 0.52 * k - 0.24),
                        (0.86, 0.52 * k),
                        (0.6, 0.52 * k + 0.24),
                    ],
                );
            }
        }
        Icon::Repeat => {
            let corner = |cx: f32, cy: f32, a0: f32| -> Vec<(f32, f32)> {
                (0..=6)
                    .map(|i| {
                        let a = a0 + i as f32 / 6.0 * PI / 2.0;
                        (cx + 0.3 * a.cos(), cy + 0.3 * a.sin())
                    })
                    .collect()
            };
            let mut top = vec![(-0.86, 0.12)];
            top.extend(corner(-0.56, -0.2, PI));
            top.push((0.8, -0.5));
            line(p, &top);
            line(p, &[(0.56, -0.74), (0.8, -0.5), (0.56, -0.26)]);
            let mut bot = vec![(0.86, -0.12)];
            bot.extend(corner(0.56, 0.2, 0.0));
            bot.push((-0.8, 0.5));
            line(p, &bot);
            line(p, &[(-0.56, 0.26), (-0.8, 0.5), (-0.56, 0.74)]);
        }
        Icon::Heart => p.stroke(&heart_pts(&b), true, st),
        Icon::Speaker => {
            let r = Rect::new(c.0 - 0.56 * b.h, c.1 - 0.86 * b.h, 1.12 * b.h, 1.72 * b.h);
            p.ring(r, 0.22 * b.h, w, color, false);
            p.arc(b.pt(0.0, 0.26), 0.3 * b.h, 0.0, TAU, st);
            disc(p, (c.0, c.1 - 0.5 * b.h), w * 1.6, color);
        }
        Icon::Queue => {
            line(p, &[(-0.86, -0.62), (0.86, -0.62)]);
            line(p, &[(-0.86, -0.12), (0.86, -0.12)]);
            line(p, &[(-0.86, 0.38), (0.06, 0.38)]);
            p.stroke(
                &b.pts(&[(0.36, 0.12), (0.86, 0.44), (0.36, 0.76)]),
                true,
                st,
            );
        }
        Icon::Gear => {
            let teeth = 8;
            let mut v = Vec::new();
            for i in 0..teeth {
                let a = i as f32 / teeth as f32 * TAU;
                let s = TAU / teeth as f32;
                for (da, r) in [(-0.3, 0.6), (-0.18, 0.88), (0.18, 0.88), (0.3, 0.6)] {
                    let aa = a + da * s;
                    v.push((r * aa.cos(), r * aa.sin()));
                }
            }
            p.stroke(&b.pts(&v), true, st);
            p.arc(b.pt(0.0, 0.0), 0.26 * b.h, 0.0, TAU, st);
        }
        Icon::Search => {
            p.arc(b.pt(-0.14, -0.14), 0.6 * b.h, 0.0, TAU, st);
            line(p, &[(0.3, 0.3), (0.86, 0.86)]);
        }
        Icon::ChevL => line(p, &[(0.28, -0.66), (-0.36, 0.0), (0.28, 0.66)]),
        Icon::ChevR => line(p, &[(-0.28, -0.66), (0.36, 0.0), (-0.28, 0.66)]),
        Icon::Check => line(p, &[(-0.7, 0.02), (-0.22, 0.5), (0.74, -0.54)]),
        Icon::Note => {
            // Centred on its ink: the head's left edge (-0.57) to the flag's tip (0.57).
            line(p, &[(0.07, 0.5), (0.07, -0.84), (0.57, -0.6)]);
            p.arc(b.pt(-0.25, 0.52), 0.32 * b.h, 0.0, TAU, st);
        }
        Icon::Clock => {
            p.arc(b.pt(0.0, 0.0), 0.84 * b.h, 0.0, TAU, st);
            line(p, &[(0.0, -0.5), (0.0, 0.0), (0.36, 0.22)]);
        }
        Icon::Eye | Icon::EyeOff => {
            // The lid: two arcs of a parabola meeting at the corners, then the iris.
            for k in [-1.0f32, 1.0] {
                let v: Vec<(f32, f32)> = (0..=16)
                    .map(|i| {
                        let t = -1.0 + i as f32 / 8.0;
                        (0.96 * t, k * 0.58 * (1.0 - t * t))
                    })
                    .collect();
                line(p, &v);
            }
            p.arc(b.pt(0.0, 0.0), 0.24 * b.h, 0.0, TAU, st);
            if icon == Icon::EyeOff {
                line(p, &[(-0.8, -0.86), (0.8, 0.86)]);
            }
        }
        Icon::Stream => {
            disc(p, c, w * 1.4, color);
            for r in [0.42, 0.82] {
                // From `start` over a sweep of 1.5 rad: the right and the left wave.
                p.arc(b.pt(0.0, 0.0), r * b.h, -0.75, 1.5, st);
                p.arc(b.pt(0.0, 0.0), r * b.h, PI - 0.75, 1.5, st);
            }
        }
        Icon::Folder => {
            p.stroke(
                &b.pts(&[
                    (-0.86, -0.62),
                    (-0.28, -0.62),
                    (-0.12, -0.42),
                    (0.86, -0.42),
                    (0.86, 0.66),
                    (-0.86, 0.66),
                ]),
                true,
                st,
            );
            line(p, &[(-0.86, -0.2), (0.86, -0.2)]);
        }
        Icon::Disc => {
            p.arc(b.pt(0.0, 0.0), 0.84 * b.h, 0.0, TAU, st);
            p.arc(b.pt(0.0, 0.0), 0.24 * b.h, 0.0, TAU, st);
        }
    }
}

/// A filled heart (liked) in `color`.
pub(crate) fn heart_filled(p: &mut P<'_>, c: (f32, f32), size: f32, color: Rgba) {
    let b = Frame { c, h: size / 2.0 };
    p.fill_path(&heart_pts(&b), color);
}

/// A filled play triangle, for buttons on art.
pub(crate) fn play_filled(p: &mut P<'_>, c: (f32, f32), size: f32, color: Rgba) {
    let b = Frame { c, h: size / 2.0 };
    p.fill_path(&b.pts(&[(-0.5, -0.78), (0.82, 0.0), (-0.5, 0.78)]), color);
}

/// A filled pause: two rounded bars.
pub(crate) fn pause_filled(p: &mut P<'_>, c: (f32, f32), size: f32, color: Rgba) {
    let h = size * 0.78;
    let w = size * 0.24;
    for dx in [-0.21, 0.21] {
        let x = c.0 + dx * size - w / 2.0;
        p.fill(Rect::new(x, c.1 - h / 2.0, w, h), w * 0.3, color);
    }
}

/// A filled skip (next, or previous when `!next`): a triangle against a bar.
pub(crate) fn skip_filled(p: &mut P<'_>, c: (f32, f32), size: f32, color: Rgba, next: bool) {
    let b = Frame { c, h: size / 2.0 };
    let k = if next { 1.0 } else { -1.0 };
    p.fill_path(
        &b.pts(&[(-0.72 * k, -0.7), (0.42 * k, 0.0), (-0.72 * k, 0.7)]),
        color,
    );
    let bw = 0.26 * b.h;
    let bx = c.0 + 0.62 * k * b.h - bw / 2.0;
    p.fill(
        Rect::new(bx, c.1 - 0.7 * b.h, bw, 1.4 * b.h),
        bw * 0.3,
        color,
    );
}
