//! Drawing helpers for map overlays, in screen coordinates.

use egui::{Color32, Mesh, Pos2, Shape, Stroke, Vec2, pos2};

/// Arrowheads on directional lines: spacing along the line, and size.
const ARROW_SPACING: f32 = 70.0;
const ARROW_LEN: f32 = 9.0;
const ARROW_HALF_WIDTH: f32 = 5.0;

/// Distance from `p` to the nearest point on a polyline.
pub fn polyline_dist(pts: &[Pos2], p: Pos2) -> f32 {
    pts.windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
            p.distance(a + ab * t)
        })
        .fold(f32::INFINITY, f32::min)
}

/// The polyline shifted sideways by `d` (positive = to the right of the
/// direction of travel), so out and back trips on one street don't overlap.
pub fn offset(pts: &[Pos2], d: f32) -> Vec<Pos2> {
    let normal = |a: Pos2, b: Pos2| {
        let v = b - a;
        let n = Vec2::new(-v.y, v.x);
        if n.length_sq() > 0.0 { n.normalized() } else { Vec2::ZERO }
    };
    (0..pts.len())
        .map(|i| {
            let before = (i > 0).then(|| normal(pts[i - 1], pts[i]));
            let after = (i + 1 < pts.len()).then(|| normal(pts[i], pts[i + 1]));
            let n = match (before, after) {
                (Some(a), Some(b)) => (a + b).normalized(),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => Vec2::ZERO,
            };
            // Screen y points down, so this normal points to the right.
            pts[i] - n * d
        })
        .collect()
}

/// A solid line with filled arrowheads every `ARROW_SPACING` showing its
/// direction (from the first point to the last).
pub fn arrow_line(pts: &[Pos2], stroke: Stroke) -> Vec<Shape> {
    let mut shapes = vec![Shape::line(pts.to_vec(), stroke)];
    let mut next = ARROW_SPACING / 2.0;
    let mut along = 0.0;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        while len > 0.0 && next <= along + len {
            let dir = (b - a) / len;
            let tip = a + dir * (next - along + ARROW_LEN / 2.0);
            let back = tip - dir * ARROW_LEN;
            let side = Vec2::new(-dir.y, dir.x) * ARROW_HALF_WIDTH;
            shapes.push(Shape::convex_polygon(vec![tip, back + side, back - side], stroke.color, Stroke::NONE));
            next += ARROW_SPACING;
        }
        along += len;
    }
    shapes
}

/// A filled polygon with no overlapping triangles (ear clipping), so a
/// translucent fill is even. Falls back to a fan if the outline crosses
/// itself.
pub fn fill_polygon(pts: &[Pos2], color: Color32) -> Shape {
    let mut mesh = Mesh::default();
    for &p in pts {
        mesh.colored_vertex(p, color);
    }
    match triangulate(pts) {
        Some(tris) => {
            for [a, b, c] in tris {
                mesh.add_triangle(a as u32, b as u32, c as u32);
            }
        }
        None => {
            for i in 1..pts.len().saturating_sub(1) as u32 {
                mesh.add_triangle(0, i, i + 1);
            }
        }
    }
    Shape::mesh(mesh)
}

fn cross(o: Pos2, a: Pos2, b: Pos2) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn inside_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// Ear-clipping triangulation of a simple polygon; None if it gets stuck
/// (self-intersecting outline).
fn triangulate(pts: &[Pos2]) -> Option<Vec<[usize; 3]>> {
    let n = pts.len();
    if n < 3 {
        return Some(vec![]);
    }
    let area: f32 = (0..n).map(|i| cross(pos2(0.0, 0.0), pts[i], pts[(i + 1) % n])).sum();
    let mut idx: Vec<usize> = (0..n).collect();
    if area < 0.0 {
        idx.reverse();
    }
    let mut tris = Vec::with_capacity(n - 2);
    let mut guard = 0;
    while idx.len() > 3 {
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            if cross(a, b, c) <= 0.0 {
                continue; // reflex or flat corner
            }
            let blocked = idx.iter().any(|&j| j != ia && j != ib && j != ic && inside_triangle(pts[j], a, b, c));
            if !blocked {
                tris.push([ia, ib, ic]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            // Drop a flat (collinear) corner if there is one, else give up.
            let flat =
                (0..m).find(|&i| cross(pts[idx[(i + m - 1) % m]], pts[idx[i]], pts[idx[(i + 1) % m]]).abs() < 1e-3);
            idx.remove(flat?);
        }
        guard += 1;
        if guard > 4 * n {
            return None;
        }
    }
    tris.push([idx[0], idx[1], idx[2]]);
    Some(tris)
}

/// Thin a polyline to points at least `min_px` apart (for fills, where
/// detail below a few pixels is invisible but costs time).
pub fn thin(pts: &[Pos2], min_px: f32) -> Vec<Pos2> {
    let mut out: Vec<Pos2> = vec![];
    for &p in pts {
        if out.last().is_none_or(|l| l.distance(p) >= min_px) {
            out.push(p);
        }
    }
    out
}
