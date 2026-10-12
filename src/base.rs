//! Routes from the base to everywhere, for travelling out from the base: one
//! shortest-path tree over the graph (for how you travel) that prefers
//! car-free ways, or for a bicycle, cycle ways.

use crate::config::Travel;
use crate::graph::{Csr, Graph, Xy, norm};
use crate::osm::Kind;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Cost per metre when walking to and from the ring: paths are preferred,
/// roads allowed (these are the walks out and home, not the ring itself).
fn weight(g: &Graph, e: usize, travel: Travel) -> f32 {
    let e = &g.edges[e];
    if travel == Travel::Bike {
        return match (e.cycle || e.bike_route, e.kind, e.calm) {
            (true, _, _) => 0.7,
            (false, Kind::CarFree, _) => 1.1,
            (false, Kind::Road, true) => 1.2,
            (false, Kind::Road, false) => 2.0,
        };
    }
    match (e.kind, e.beside, e.calm) {
        (Kind::CarFree, false, _) => 1.0,
        (Kind::CarFree, true, _) => 1.2,
        (Kind::Road, _, true) => 1.5,
        (Kind::Road, _, false) => 2.0,
    }
}

pub struct BaseTree {
    /// Walking metres from the base to each node (infinite if unreachable).
    dist: Vec<f32>,
    /// Edge used to reach each node (u32::MAX for start nodes / unreached).
    via: Vec<u32>,
    /// Metres climbed and descended walking from the base to each node.
    climb: Vec<(f32, f32)>,
}

/// How much more walking along the ring costs on the way out or home.
const RING_AVOID: f32 = 5.0;

/// Nodes within this distance of the base are all starting points (the
/// single nearest node might be a disconnected scrap of path).
const START_RADIUS_M: f64 = 300.0;

impl BaseTree {
    /// `climb_cost`: extra metres per metre climbed or descended, so walks
    /// out and home go round hills where that's not much further.
    pub fn new(g: &Graph, travel: Travel, climb_cost: f64) -> BaseTree {
        BaseTree::build(g, travel, climb_cost, &|_| 1.0, f64::INFINITY)
    }

    /// Walks out to a ring and home that keep off the ring itself: walking
    /// along it would re-walk an arc (yesterday's, or today's twice). Its
    /// edges cost `RING_AVOID` times more, so they're used only where
    /// there's no other way. Only ways within `radius_m` are searched.
    pub fn around_ring(
        g: &Graph,
        travel: Travel,
        climb_cost: f64,
        ring_edges: &[(usize, bool)],
        radius_m: f64,
    ) -> BaseTree {
        let mut on_ring = vec![false; g.edges.len()];
        for &(ei, _) in ring_edges {
            on_ring[ei] = true;
        }
        BaseTree::build(g, travel, climb_cost, &|ei| if on_ring[ei] { RING_AVOID } else { 1.0 }, radius_m)
    }

    /// `factor`: a cost multiplier per edge.
    fn build(g: &Graph, travel: Travel, climb_cost: f64, factor: &dyn Fn(usize) -> f32, radius_m: f64) -> BaseTree {
        let n = g.nodes.len();
        let csr = Csr::new(g, |_, e| {
            (if travel == Travel::Bike { e.bike } else { e.foot }) && e.pts.iter().all(|&p| norm(p) <= radius_m)
        });
        let mut starts: Vec<usize> =
            (0..n).filter(|&i| csr.degree(i) > 0 && norm(g.nodes[i]) <= START_RADIUS_M).collect();
        if starts.is_empty()
            && let Some(nearest) =
                (0..n).filter(|&i| csr.degree(i) > 0).min_by(|&a, &b| norm(g.nodes[a]).total_cmp(&norm(g.nodes[b])))
        {
            starts.push(nearest);
        }
        let mut cost = vec![f32::INFINITY; n];
        let mut dist = vec![f32::INFINITY; n];
        let mut via = vec![u32::MAX; n];
        let mut climb = vec![(0.0f32, 0.0f32); n];
        let mut heap = BinaryHeap::new();
        for &s in &starts {
            let d = norm(g.nodes[s]) as f32;
            cost[s] = d;
            dist[s] = d;
            heap.push(Reverse((d.to_bits(), s as u32)));
        }
        while let Some(Reverse((bits, u))) = heap.pop() {
            let (cu, u) = (f32::from_bits(bits), u as usize);
            if cu > cost[u] {
                continue;
            }
            for &(_, ei) in csr.nbrs(u) {
                let ei = ei as usize;
                let w = weight(g, ei, travel);
                let e = &g.edges[ei];
                let v = if e.a == u { e.b } else { e.a };
                let rise = e.rise(e.a == u) as f32;
                let cv = cu + factor(ei) * w * e.len as f32 + climb_cost as f32 * rise.abs() / 2.0;
                if cv < cost[v] {
                    cost[v] = cv;
                    dist[v] = dist[u] + e.len as f32;
                    via[v] = ei as u32;
                    climb[v] = (climb[u].0 + rise.max(0.0), climb[u].1 + (-rise).max(0.0));
                    heap.push(Reverse((cv.to_bits(), v as u32)));
                }
            }
        }
        BaseTree { dist, via, climb }
    }

    /// Walking metres from the base to a node.
    pub fn walk_m(&self, node: usize) -> Option<f64> {
        let d = self.dist[node];
        d.is_finite().then_some(d as f64)
    }

    /// Metres climbed and descended walking from the base to a node (the
    /// walk back climbs what this descends).
    pub fn climb(&self, node: usize) -> (f64, f64) {
        let (up, down) = self.climb[node];
        (up as f64, down as f64)
    }

    /// The edges walked from the base to a node, in order, with their
    /// lengths (for describing the walk).
    pub fn path_parts(&self, g: &Graph, node: usize) -> Vec<(usize, f64)> {
        let mut parts = vec![];
        let mut at = node;
        while self.via[at] != u32::MAX {
            let ei = self.via[at] as usize;
            parts.push((ei, g.edges[ei].len));
            at = g.other(ei, at);
        }
        parts.reverse();
        parts
    }

    /// The walk from the base to a node, as a polyline.
    pub fn path(&self, g: &Graph, node: usize) -> Vec<Xy> {
        let mut pts: Vec<Xy> = vec![];
        let mut at = node;
        while self.via[at] != u32::MAX {
            let e = &g.edges[self.via[at] as usize];
            // Collected from the node back towards the base: geometry
            // running from `at` to the previous node.
            let (from, back): (usize, Vec<Xy>) =
                if e.a == at { (e.b, e.pts.clone()) } else { (e.a, e.pts.iter().rev().copied().collect()) };
            pts.extend(back.into_iter().skip(if pts.is_empty() { 0 } else { 1 }));
            at = from;
        }
        if pts.is_empty() {
            pts.push(g.nodes[node]);
        }
        pts.push([0.0, 0.0]);
        pts.reverse();
        pts
    }
}

#[cfg(test)]
mod tests {
    use super::BaseTree;
    use crate::config::Travel;
    use crate::graph::{Edge, Graph, WayInfo};
    use crate::osm::Kind;

    /// The base, A 1 km east, C 1 km north of A: a car-free path out to A,
    /// the ring from A to C, and a road straight from the base to C. The
    /// cheapest way to C runs along the ring; avoiding the ring takes the
    /// road.
    #[test]
    fn walks_keep_off_the_ring() {
        let edge = |a: usize, b: usize, pts: Vec<[f64; 2]>, kind: Kind| {
            let len = crate::graph::dist(pts[0], pts[1]);
            Edge { pts, kind, ..Edge::new(a, b, len, 0) }
        };
        let (o, a, c) = ([0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0]);
        let g = Graph {
            nodes: vec![o, a, c],
            edges: vec![
                edge(0, 1, vec![o, a], Kind::CarFree),
                edge(1, 2, vec![a, c], Kind::CarFree),
                edge(0, 2, vec![o, c], Kind::Road),
            ],
            ways: vec![WayInfo { name: None, wtype: 0, route: None, bridge: false }],
        };
        let ring = [(1, true)];
        let plain = BaseTree::new(&g, Travel::Foot, 0.0);
        let around = BaseTree::around_ring(&g, Travel::Foot, 0.0, &ring, f64::INFINITY);
        assert_eq!(plain.path_parts(&g, 2).iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(around.path_parts(&g, 2).iter().map(|p| p.0).collect::<Vec<_>>(), vec![2]);
    }
}
