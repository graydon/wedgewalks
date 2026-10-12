//! Shade signal: how much of a ring runs through or beside parks, woods and
//! trees. A weak heuristic; OSM tree coverage varies a lot by city.

use crate::graph::{Proj, Xy, dist, lerp, norm};
use crate::osm::OsmData;
use rstar::RTree;
use rstar::primitives::{GeomWithData, Line, Rectangle};
use rustc_hash::FxHashMap as HashMap;

/// Spacing of the points checked for shade along a walk.
const SAMPLE_M: f64 = 25.0;

pub struct Shade {
    areas: Vec<Vec<Xy>>,
    area_boxes: RTree<GeomWithData<Rectangle<Xy>, usize>>,
    /// Tree rows and area outlines.
    lines: RTree<Line<Xy>>,
    trees: RTree<Xy>,
    near_m: f64,
}

/// Joins open ways that share end nodes into closed rings where possible.
/// Returns (closed rings, leftover open pieces).
pub(crate) fn assemble(mut pieces: Vec<Vec<i64>>) -> (Vec<Vec<i64>>, Vec<Vec<i64>>) {
    let mut closed = vec![];
    let mut open = vec![];
    pieces.retain(|p| {
        if p.len() >= 4 && p.first() == p.last() {
            closed.push(p.clone());
            false
        } else {
            p.len() >= 2
        }
    });
    let mut by_end: HashMap<i64, Vec<usize>> = HashMap::default();
    for (i, p) in pieces.iter().enumerate() {
        by_end.entry(p[0]).or_default().push(i);
        by_end.entry(*p.last().unwrap()).or_default().push(i);
    }
    let mut used = vec![false; pieces.len()];
    for start in 0..pieces.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut cur = pieces[start].clone();
        while cur.first() != cur.last() {
            let end = *cur.last().unwrap();
            let Some(&next) = by_end.get(&end).and_then(|v| v.iter().find(|&&i| !used[i])) else { break };
            used[next] = true;
            let mut p = pieces[next].clone();
            if p[0] != end {
                p.reverse();
            }
            cur.extend_from_slice(&p[1..]);
        }
        if cur.first() == cur.last() { closed.push(cur) } else { open.push(cur) }
    }
    (closed, open)
}

pub(crate) fn inside(poly: &[Xy], p: Xy) -> bool {
    let mut c = false;
    for w in poly.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            c = !c;
        }
    }
    c
}

/// Park, wood and forest outlines (closed) with a point within `radius_m`,
/// and the pieces that don't close.
fn outlines(osm: &OsmData, proj: &Proj, radius_m: f64) -> (Vec<Vec<Xy>>, Vec<Vec<Xy>>) {
    let xy = |refs: &[i64]| -> Vec<Xy> {
        refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect()
    };
    let pieces = osm.shade_ways.iter().filter(|(_, line)| !line).map(|(refs, _)| refs.clone()).collect();
    let (closed, open) = assemble(pieces);
    let areas = closed.iter().map(|r| xy(r)).filter(|a| a.len() >= 4 && a.iter().any(|&p| norm(p) <= radius_m));
    (areas.collect(), open.iter().map(|r| xy(r)).collect())
}

/// Park, wood and forest outlines with a point within `radius_m`.
pub fn park_outlines(osm: &OsmData, proj: &Proj, radius_m: f64) -> Vec<Vec<Xy>> {
    outlines(osm, proj, radius_m).0
}

impl Shade {
    pub fn build(osm: &OsmData, proj: &Proj, radius_m: f64, near_m: f64) -> Shade {
        let xy = |refs: &[i64]| -> Vec<Xy> {
            refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect()
        };
        let nearby = |pts: &Vec<Xy>| pts.iter().any(|&p| norm(p) <= radius_m);
        let mut lines: Vec<Vec<Xy>> =
            osm.shade_ways.iter().filter(|(_, line)| *line).map(|(refs, _)| xy(refs)).collect();
        let (areas, open) = outlines(osm, proj, radius_m);
        lines.extend(open);
        lines.extend(areas.iter().cloned());
        let segs: Vec<Line<Xy>> = lines
            .iter()
            .filter(|l| nearby(l))
            .flat_map(|l| l.windows(2).map(|w| Line::new(w[0], w[1])).collect::<Vec<_>>())
            .collect();
        let boxes = areas
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                for p in a {
                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                }
                GeomWithData::new(Rectangle::from_corners(lo, hi), i)
            })
            .collect();
        let trees = osm.trees.iter().map(|&(la, lo)| proj.xy(la, lo)).filter(|&p| norm(p) <= radius_m).collect();
        Shade {
            areas,
            area_boxes: RTree::bulk_load(boxes),
            lines: RTree::bulk_load(segs),
            trees: RTree::bulk_load(trees),
            near_m,
        }
    }

    pub fn shaded(&self, p: Xy) -> bool {
        let d2 = self.near_m * self.near_m;
        self.trees.locate_within_distance(p, d2).next().is_some()
            || self.lines.locate_within_distance(p, d2).next().is_some()
            || self.area_boxes.locate_all_at_point(&p).any(|b| inside(&self.areas[b.data], p))
    }

    /// Fraction of a polyline's length that is shaded, sampled every
    /// `SAMPLE_M`.
    pub fn fraction(&self, pts: &[Xy]) -> f64 {
        let (mut total, mut shaded) = (0.0, 0.0);
        for w in pts.windows(2) {
            let len = dist(w[0], w[1]);
            let n = (len / SAMPLE_M).ceil().max(1.0) as usize;
            for k in 0..n {
                let t = (k as f64 + 0.5) / n as f64;
                let p = lerp(w[0], w[1], t);
                total += len / n as f64;
                if self.shaded(p) {
                    shaded += len / n as f64;
                }
            }
        }
        if total > 0.0 { shaded / total } else { 0.0 }
    }
}
