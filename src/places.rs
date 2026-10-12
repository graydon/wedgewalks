//! Named surroundings, for describing paths that have no name of their
//! own: "beside Queen Street", "through Riverdale Park", "along the Don".

use crate::graph::{Proj, Xy, dist, lerp, norm};
use crate::osm::{OsmData, PlaceKind};
use crate::shade::{assemble, inside};
use rstar::RTree;
use rstar::primitives::{GeomWithData, Line, Rectangle};
use rustc_hash::FxHashMap as HashMap;

/// A path counts as along named water within this distance.
const ALONG_WATER_M: f64 = 60.0;
/// ...and as beside a named street within this distance...
const BESIDE_STREET_M: f64 = 30.0;
/// ...and failing anything closer, near one within this distance.
const NEAR_STREET_M: f64 = 150.0;

pub struct Places {
    names: Vec<String>,
    /// Street and water lines, with (name index, kind).
    lines: RTree<GeomWithData<Line<Xy>, (u32, PlaceKind)>>,
    /// Park outlines and their names.
    parks: Vec<(Vec<Xy>, u32)>,
    park_boxes: RTree<GeomWithData<Rectangle<Xy>, usize>>,
}

impl Places {
    pub fn build(osm: &OsmData, proj: &Proj, radius_m: f64) -> Places {
        let mut names: Vec<String> = vec![];
        let mut name_index: HashMap<&str, u32> = HashMap::default();
        let xy = |refs: &[i64]| -> Vec<Xy> {
            refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect()
        };
        let near = |pts: &[Xy]| pts.iter().any(|&p| norm(p) <= radius_m);

        let mut lines = vec![];
        let mut park_pieces: HashMap<u32, Vec<Vec<i64>>> = HashMap::default();
        for p in &osm.places {
            let n = *name_index.entry(p.name.as_str()).or_insert_with(|| {
                names.push(p.name.clone());
                names.len() as u32 - 1
            });
            match p.kind {
                PlaceKind::Park => park_pieces.entry(n).or_default().push(p.refs.clone()),
                kind => {
                    let pts = xy(&p.refs);
                    if near(&pts) {
                        lines.extend(pts.windows(2).map(|w| GeomWithData::new(Line::new(w[0], w[1]), (n, kind))));
                    }
                }
            }
        }
        let mut parks = vec![];
        for (n, pieces) in park_pieces {
            let (closed, _) = assemble(pieces);
            parks.extend(closed.iter().map(|r| xy(r)).filter(|pts| pts.len() >= 4 && near(pts)).map(|pts| (pts, n)));
        }
        let boxes = parks
            .iter()
            .enumerate()
            .map(|(i, (pts, _))| {
                let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                for p in pts {
                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                }
                GeomWithData::new(Rectangle::from_corners(lo, hi), i)
            })
            .collect();
        Places { names, lines: RTree::bulk_load(lines), parks, park_boxes: RTree::bulk_load(boxes) }
    }

    fn nearest(&self, p: Xy, kind: PlaceKind, within: f64) -> Option<&str> {
        self.lines
            .nearest_neighbor_iter_with_distance_2(&p)
            .take_while(|(_, d2)| *d2 <= within * within)
            .find(|(l, _)| l.data.1 == kind)
            .map(|(l, _)| self.names[l.data.0 as usize].as_str())
    }

    pub(crate) fn park_at(&self, p: Xy) -> Option<&str> {
        self.park_boxes
            .locate_all_at_point(&p)
            .find(|b| inside(&self.parks[b.data].0, p))
            .map(|b| self.names[self.parks[b.data].1 as usize].as_str())
    }

    /// How to describe a polyline without a name of its own, from what it
    /// runs along, beside or through (looked up at its middle).
    pub fn describe(&self, pts: &[Xy]) -> Option<String> {
        let mid = midpoint(pts)?;
        if let Some(w) = self.nearest(mid, PlaceKind::Water, ALONG_WATER_M) {
            return Some(format!("along {w}"));
        }
        if let Some(s) = self.nearest(mid, PlaceKind::Street, BESIDE_STREET_M) {
            return Some(format!("beside {s}"));
        }
        if let Some(p) = self.park_at(mid) {
            return Some(format!("through {p}"));
        }
        self.nearest(mid, PlaceKind::Street, NEAR_STREET_M).map(|s| format!("near {s}"))
    }
}

/// The point halfway along a polyline.
pub(crate) fn midpoint(pts: &[Xy]) -> Option<Xy> {
    let total: f64 = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
    let mut left = total / 2.0;
    for w in pts.windows(2) {
        let l = dist(w[0], w[1]);
        if l >= left {
            return Some(lerp(w[0], w[1], if l > 0.0 { left / l } else { 0.0 }));
        }
        left -= l;
    }
    pts.first().copied()
}
