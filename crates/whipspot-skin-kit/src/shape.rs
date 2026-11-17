//! A panel outline from an image's alpha channel (D12): the exact pixel-edge contour of the
//! opaque pixels (alpha != 0) as the core's [`Shape::Polygons`].
//!
//! The core treats the polygons of a shape as a union, and each polygon as even-odd. So rings
//! that are all outer rings become one polygon each, and as soon as a ring is a hole every ring
//! goes into one polygon, joined by out-and-back bridges (each bridge is crossed twice, so it
//! changes no parity), which keeps holes holes. Anything over the core's caps, and the empty and
//! the full mask, is [`Shape::Rect`]: the window simply loses its shape.
//!
//! The input is untrusted pixels: the vertex count is bounded before anything is traced, the walk
//! is bounded by the edge count, and no step can panic.

use whipspot_core::skin::{MAX_POINTS, MAX_POLYGONS, Point, Shape, SkinImage};

/// Most rings that stay separate polygons (the core's polygon cap).
const MAX_SEPARATE_RINGS: usize = MAX_POLYGONS;

/// Directions of a boundary edge, with the opaque side on the right (screen coordinates, y down):
/// 0 east, 1 south, 2 west, 3 north.
const STEP: [(i64, i64); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

struct Mask<'a> {
    w: i64,
    h: i64,
    rgba: &'a [u8],
}

impl Mask<'_> {
    fn opaque(&self, x: i64, y: i64) -> bool {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return false;
        }
        self.rgba
            .get(((y * self.w + x) * 4 + 3) as usize)
            .is_some_and(|a| *a != 0)
    }

    /// Whether the boundary edge leaving grid vertex (x, y) in direction `d` exists.
    fn edge(&self, x: i64, y: i64, d: usize) -> bool {
        match d {
            0 => self.opaque(x, y) && !self.opaque(x, y - 1),
            1 => self.opaque(x - 1, y) && !self.opaque(x, y),
            2 => self.opaque(x - 1, y - 1) && !self.opaque(x - 1, y),
            _ => self.opaque(x, y - 1) && !self.opaque(x - 1, y - 1),
        }
    }

    /// An upper bound on the corner count of all rings: a vertex touching one or three opaque
    /// pixels is a corner, one touching two diagonal ones is two.
    fn corners(&self) -> usize {
        let mut n = 0usize;
        for y in 0..=self.h {
            for x in 0..=self.w {
                let tl = self.opaque(x - 1, y - 1);
                let tr = self.opaque(x, y - 1);
                let bl = self.opaque(x - 1, y);
                let br = self.opaque(x, y);
                let count = [tl, tr, bl, br].iter().filter(|b| **b).count();
                n += match count {
                    1 | 3 => 1,
                    2 if tl == br => 2,
                    _ => 0,
                };
            }
        }
        n
    }
}

/// The outline of the opaque (alpha != 0) pixels of `image`, as a [`Shape`].
pub fn from_alpha(image: &SkinImage) -> Shape {
    trace(image).unwrap_or(Shape::Rect)
}

fn trace(image: &SkinImage) -> Option<Shape> {
    let (w, h) = (i64::from(image.width()), i64::from(image.height()));
    let mask = Mask {
        w,
        h,
        rgba: image.rgba(),
    };
    let opaque = mask.rgba.chunks_exact(4).filter(|p| p[3] != 0).count();
    if opaque == 0 || opaque as i64 == w * h {
        return None;
    }
    if mask.corners() > MAX_POINTS {
        return None;
    }

    let stride = (w + 1) as usize;
    let mut used = vec![0u8; stride * (h + 1) as usize];
    let mut rings: Vec<Vec<(i64, i64)>> = Vec::new();
    let mut total = 0usize;
    // Every step of every ring uses one distinct edge, so a walk longer than this has looped.
    let walk_cap = 4usize.checked_mul(used.len())?;

    for y in 0..=h {
        for x in 0..=w {
            for d in 0..4 {
                let at = y as usize * stride + x as usize;
                if used[at] & (1 << d) != 0 || !mask.edge(x, y, d) {
                    continue;
                }
                let ring = walk(&mask, &mut used, stride, (x, y, d), walk_cap)?;
                total += ring.len();
                if total > MAX_POINTS || rings.len() >= MAX_POINTS {
                    return None;
                }
                rings.push(ring);
            }
        }
    }
    if rings.is_empty() {
        return None;
    }

    let all_outer = rings.iter().all(|r| area2(r) > 0);
    let polygons: Vec<Vec<(i64, i64)>> = if all_outer && rings.len() <= MAX_SEPARATE_RINGS {
        rings
    } else {
        vec![chain(&rings)]
    };
    let points: usize = polygons.iter().map(Vec::len).sum();
    if points > MAX_POINTS || polygons.len() > MAX_POLYGONS {
        return None;
    }
    let mut out = Vec::with_capacity(polygons.len());
    for poly in polygons {
        let mut p = Vec::with_capacity(poly.len());
        for (x, y) in poly {
            p.push(Point {
                x: u32::try_from(x).ok()?,
                y: u32::try_from(y).ok()?,
            });
        }
        out.push(p);
    }
    Some(Shape::Polygons(out))
}

/// Walks one ring from the edge `(x, y, d)`, marking its edges used, and returns its corners.
/// `None` when the state is inconsistent or the walk is too long.
fn walk(
    mask: &Mask,
    used: &mut [u8],
    stride: usize,
    start: (i64, i64, usize),
    walk_cap: usize,
) -> Option<Vec<(i64, i64)>> {
    let (sx, sy, sd) = start;
    let (mut x, mut y, mut d) = start;
    let mut ring = Vec::new();
    for _ in 0..walk_cap {
        *used.get_mut(y as usize * stride + x as usize)? |= 1 << d;
        x += STEP[d].0;
        y += STEP[d].1;
        // Right turn first, then straight, then left: at a vertex where two diagonal opaque
        // pixels touch, this keeps them in separate rings.
        let next = [(d + 1) % 4, d, (d + 3) % 4].into_iter().find(|nd| {
            let free = used
                .get(y as usize * stride + x as usize)
                .is_some_and(|u| u & (1 << nd) == 0);
            let is_start = (x, y, *nd) == (sx, sy, sd);
            mask.edge(x, y, *nd) && (free || is_start)
        })?;
        if next != d {
            ring.push((x, y));
        }
        if (x, y, next) == (sx, sy, sd) {
            return Some(ring);
        }
        d = next;
    }
    None
}

/// Twice the signed area (shoelace): positive for an outer ring in this tracing order.
fn area2(ring: &[(i64, i64)]) -> i64 {
    let mut sum = 0i64;
    for (i, (x0, y0)) in ring.iter().enumerate() {
        let (x1, y1) = ring[(i + 1) % ring.len()];
        sum += x0 * y1 - x1 * y0;
    }
    sum
}

/// All rings as one polygon: ring 0, then each other ring reached by a bridge from ring 0's first
/// point and left by the same bridge. Even-odd over this polygon equals the mask.
fn chain(rings: &[Vec<(i64, i64)>]) -> Vec<(i64, i64)> {
    let anchor = rings[0][0];
    let mut poly = rings[0].clone();
    if rings.len() > 1 {
        poly.push(anchor);
    }
    for ring in &rings[1..] {
        poly.extend_from_slice(ring);
        poly.push(ring[0]);
        poly.push(anchor);
    }
    poly
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mask from rows of `#` (opaque) and `.`.
    fn mask(rows: &[&str]) -> SkinImage {
        let h = rows.len() as u32;
        let w = rows[0].len() as u32;
        let mut rgba = Vec::new();
        for row in rows {
            for c in row.bytes() {
                rgba.extend_from_slice(&[10, 20, 30, if c == b'#' { 255 } else { 0 }]);
            }
        }
        SkinImage::new(w, h, rgba).unwrap()
    }

    fn polys(shape: Shape) -> Vec<Vec<Point>> {
        match shape {
            Shape::Polygons(p) => p,
            Shape::Rect => panic!("expected polygons"),
        }
    }

    /// Even-odd containment of a point (in pixel units) over all polygons of a shape, as a
    /// union: inside when any polygon contains it.
    fn contains(polys: &[Vec<Point>], px: f64, py: f64) -> bool {
        polys.iter().any(|poly| {
            let mut inside = false;
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let (ax, ay, bx, by) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64);
                if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
                    inside = !inside;
                }
            }
            inside
        })
    }

    fn assert_matches_mask(rows: &[&str], shape: Shape) {
        let p = polys(shape);
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.bytes().enumerate() {
                assert_eq!(
                    contains(&p, x as f64 + 0.5, y as f64 + 0.5),
                    c == b'#',
                    "pixel ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn empty_and_full_masks_are_rects() {
        assert_eq!(from_alpha(&mask(&["...", "..."])), Shape::Rect);
        assert_eq!(from_alpha(&mask(&["###", "###"])), Shape::Rect);
    }

    #[test]
    fn one_pixel_is_a_positive_ring_of_four_points() {
        let p = polys(from_alpha(&mask(&[".#.", "..."])));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), 4);
        let ring: Vec<(i64, i64)> = p[0].iter().map(|q| (q.x.into(), q.y.into())).collect();
        assert!(area2(&ring) > 0);
        assert!(p[0].iter().all(|q| q.x <= 3 && q.y <= 2));
    }

    #[test]
    fn an_l_shape_is_one_polygon_of_six_points() {
        let rows = ["#.", "#.", "##"];
        let p = polys(from_alpha(&mask(&rows)));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), 6);
        assert_matches_mask(&rows, Shape::Polygons(p));
    }

    #[test]
    fn a_ring_with_a_one_pixel_hole_keeps_the_hole() {
        let rows = ["###", "#.#", "###"];
        let shape = from_alpha(&mask(&rows));
        let p = polys(shape.clone());
        assert_eq!(p.len(), 1);
        assert_matches_mask(&rows, shape);
    }

    #[test]
    fn a_hole_and_a_separate_island() {
        let rows = ["#####..#", "#...#...", "#.#.#..#", "#...#...", "#####..#"];
        let shape = from_alpha(&mask(&rows));
        assert_eq!(polys(shape.clone()).len(), 1);
        assert_matches_mask(&rows, shape);
    }

    #[test]
    fn diagonal_pixels_are_separate_rings() {
        let rows = ["#.", ".#"];
        let p = polys(from_alpha(&mask(&rows)));
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|r| r.len() == 4));
        assert_matches_mask(&rows, Shape::Polygons(p));
    }

    #[test]
    fn a_noisy_mask_is_a_rect() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut rgba = Vec::new();
        for _ in 0..128 * 128 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            rgba.extend_from_slice(&[0, 0, 0, if s & 1 == 1 { 255 } else { 0 }]);
        }
        assert_eq!(
            from_alpha(&SkinImage::new(128, 128, rgba).unwrap()),
            Shape::Rect
        );
        // A 64 x 64 checkerboard has far more than MAX_POINTS corners.
        let mut rgba = Vec::new();
        for y in 0..64 {
            for x in 0..64 {
                rgba.extend_from_slice(&[0, 0, 0, if (x + y) % 2 == 0 { 255 } else { 0 }]);
            }
        }
        assert_eq!(
            from_alpha(&SkinImage::new(64, 64, rgba).unwrap()),
            Shape::Rect
        );
    }

    #[test]
    fn many_islands_chain_into_one_polygon_and_stay_in_the_caps() {
        // 20 x 20 islands of one pixel at even coordinates: 400 rings of 4 points, 400 > 128
        // rings, 1600 + bridges points, under 4096.
        let mut rows = Vec::new();
        for y in 0..39 {
            rows.push(
                (0..39)
                    .map(|x| if x % 2 == 0 && y % 2 == 0 { '#' } else { '.' })
                    .collect::<String>(),
            );
        }
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let shape = from_alpha(&mask(&refs));
        let p = polys(shape.clone());
        assert_eq!(p.len(), 1);
        assert!(p[0].len() <= MAX_POINTS);
        assert_matches_mask(&refs, shape);
    }
}
