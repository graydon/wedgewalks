//! How pleasant each way is to walk, beyond what its tags say: how shaded it
//! is and how much street life it has (shops, cafés, venues beside it).

use crate::graph::{Graph, Proj, Xy, dist, norm};
use crate::osm::OsmData;
use crate::shade::Shade;
use rayon::prelude::*;
use rstar::RTree;

/// A place counts for a way within this distance of it...
const NEAR_M: f64 = 35.0;
/// ...and for the surrounding area within this one.
const AREA_M: f64 = 150.0;
/// Places per 100 m of way that make it fully lively, and per hectare-ish
/// (the area circle) that make its surroundings so.
const LIVELY_PER_100M: f64 = 3.0;
const LIVELY_IN_AREA: f64 = 25.0;
/// How much the surroundings count towards a way's liveliness.
const AREA_SHARE: f64 = 0.3;

/// Per-edge shade and street life, each 0..1.
pub struct Feel {
    pub shade: Vec<f32>,
    pub life: Vec<f32>,
}

impl Feel {
    pub fn build(g: &Graph, osm: &OsmData, proj: &Proj, shade: &Shade, around_m: f64) -> Feel {
        let places: Vec<Xy> =
            osm.lively.iter().map(|&(la, lo)| proj.xy(la, lo)).filter(|&p| norm(p) <= around_m + 1000.0).collect();
        let tree = RTree::bulk_load(places);
        let (shade, life) = g
            .edges
            .par_iter()
            .map(|e| {
                if norm(e.pts[0]) > around_m {
                    return (0.0, 0.0);
                }
                (shade.fraction(&e.pts) as f32, life(&tree, &e.pts, e.len) as f32)
            })
            .unzip();
        Feel { shade, life }
    }
}

/// Street life along a polyline: places near it per length, blended with
/// how many there are around its middle.
fn life(tree: &RTree<Xy>, pts: &[Xy], len: f64) -> f64 {
    let n_along = tree
        .locate_in_envelope(&bbox(pts, NEAR_M))
        .filter(|&&p| pts.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= NEAR_M))
        .count() as f64;
    let mid = pts[pts.len() / 2];
    let n_area = tree.locate_within_distance(mid, AREA_M * AREA_M).count() as f64;
    let along = (n_along / (len.max(60.0) / 100.0) / LIVELY_PER_100M).min(1.0);
    let area = (n_area / LIVELY_IN_AREA).min(1.0);
    (1.0 - AREA_SHARE) * along + AREA_SHARE * area
}

fn bbox(pts: &[Xy], pad: f64) -> rstar::AABB<Xy> {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in pts {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    rstar::AABB::from_corners([lo[0] - pad, lo[1] - pad], [hi[0] + pad, hi[1] + pad])
}

fn seg_dist(p: Xy, a: Xy, b: Xy) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}
