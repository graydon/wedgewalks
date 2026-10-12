//! The planner: D days of walking round the base, as a closed ring or a
//! chain from shore to shore. The algorithm is written out in ALGORITHM.md;
//! the steps are numbered the same here.

use crate::base::BaseTree;
use crate::config::{Access, Config, Travel};
use crate::graph::{Csr, Edge, Graph, Xy, dist, norm};
use crate::manual::Manual;
use crate::pipeline::Built;
use crate::rings::{Ring, make_ring};
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::f64::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

const NONE: u32 = u32::MAX;

// ---- sizes -------------------------------------------------------------

/// Rings run this much longer than a circle through the same points (streets wander).
const WINDING: f64 = 1.35;
/// If the rings found wind more or less than assumed by more than this share, the size is worked
/// out again with what was measured.
const WINDING_TOLERANCE: f64 = 0.06;
const WINDING_RANGE: (f64, f64) = (1.1, 1.8);
/// Transit mode: a day's arc is this share of the day's length (the rest is slack).
const ARC_FILL: f64 = 0.85;
/// Walking mode: the walk out and home runs this much longer than the straight line.
const APPROACH_WINDING: f64 = 1.2;
/// Smallest R tried (m), and the share of the map radius the ring may use.
const R_MIN_M: f64 = 600.0;
const MAP_SHARE: f64 = 0.75;

// ---- profile -----------------------------------------------------------

const BINS: usize = 72;
const BIN: f64 = TAU / BINS as f64;
/// Radial cells per R in the land histogram, and points along the network needed for a cell to be land.
const CELLS: usize = 10;
const MIN_NODES: usize = 3;
/// Spacing of the points along the network counted as land (m).
const SAMPLE_M: f64 = 120.0;
/// Land closer in than this share of R (or this many metres) doesn't make a bearing worth a leg.
const MIN_RHO: f64 = 0.1;
const MIN_RHO_M: f64 = 600.0;
/// A run of bearings pulled in to under this share of R must be at least this share of a day long
/// to count as land.
const PULLED_IN: f64 = 0.5;
const MIN_PULLED_IN: f64 = 0.75;
/// Gaps of fewer bins than this are bridged; this many make the chain open.
const MIN_GAP_BINS: usize = 3;

// ---- corridor, anchors, legs ------------------------------------------

/// Legs and anchors stay between these shares of the profile radius.
const CORRIDOR: (f64, f64) = (0.6, 1.5);
const ANCHOR_BAND: (f64, f64) = (0.6, 1.4);
/// Shore anchors may be closer in (share of R): chains end at the water.
const SHORE_IN: f64 = 0.25;
const ANCHOR_CELL_M: f64 = 400.0;
const MAX_ANCHORS: usize = 320;
/// A leg's bearing step round the centre (degrees).
const STEP_DEG: (f64, f64) = (5.0, 150.0);
/// Legs and routes never go back round the centre by more than this (radians) along any way.
const BACK_TOL: f64 = 15.0 * PI / 180.0;
/// A leg is at most this many times the straight line between its ends, plus a little.
const WIND_MAX: f64 = 2.2;
const WIND_SLACK_M: f64 = 300.0;
/// A day's leg is at least this share of what its day allows (first and last of an open chain: less).
const MIN_FILL: f64 = 0.7;
const EDGE_FILL: f64 = 0.4;
const MIN_LEG_M: f64 = 800.0;

// ---- costs (metres of walking) ----------------------------------------

/// Per metre short of the day's allowance.
const SHORT_W: f64 = 0.3;
/// Per squared share of the profile radius an anchor is off it.
const RADIAL_W: f64 = 12000.0;
const LIFE_BONUS_M: f64 = 150.0;
const SHORE_BONUS_M: f64 = 100.0;
/// Walks out and home are budgeted this much longer than the shortest way, since they keep off the ring.
const WALK_MARGIN: f64 = 1.15;
/// A day end with no transit costs this (m), when days without are allowed at all.
const NO_ACCESS_M: f64 = 5000.0;
/// Per metre of climb over the day's limit.
const CLIMB_OVER_W: f64 = 40.0;
/// Starting or ending a day on a path rather than a street.
const PATH_BONUS_M: f64 = 200.0;
/// Walking to a stop from the base counts this much against walking from a stop to the anchor.
const CENTER_WALK_WEIGHT: f64 = 0.3;
/// Turns: none below 20 degrees, full at 100; changing street costs half as much again.
const TURN_FREE: f64 = 20.0 * PI / 180.0;
const TURN_FULL: f64 = 100.0 * PI / 180.0;
const SWITCH_SHARE: f64 = 0.5;

// ---- chain -------------------------------------------------------------

/// Closed chains are started from this many anchors, and this many kept.
const STARTS: usize = 24;
const KEEP: usize = 4;
/// An open chain starts and ends at shore anchors this close (degrees) to the ends of the land...
const END_ZONE_DEG: f64 = 60.0;
/// ...and ending away from a shore costs this much more (m).
const INLAND_END_M: f64 = 4000.0;
/// ...and uncovered land costs this per day-length of chain.
const END_W: f64 = 8.0;
/// A ring that doesn't enclose the base must come this close to it (share of R).
const NEAR_BASE_SHARE: f64 = 0.35;
/// A chain passes this close to the base (share of R).
const NEAR_BASE_CHAIN: f64 = 0.7;
/// Ways already walked by earlier days of a plan, for routing the next. Walking one of them again, or
/// walking back along one in the other direction (out and back, or the pavement on the other side of the
/// street), is what a day should not do.
/// Points (x, y, direction x, direction y).
type Points = Vec<(f32, f32, f32, f32)>;

#[derive(Default)]
struct Used {
    edges: std::collections::HashSet<usize>,
    /// Points along used ways, by cell, with the direction walked there.
    grid: std::collections::HashMap<(i32, i32), Points>,
}

/// The cells of `Used::grid`, the spacing of its points (m), and how close (m) a way going back must be to
/// one to count as walking back along it. Edges shorter than `SHORT_EDGE_M` are never judged by this.
const USED_CELL_M: f64 = 60.0;
const USED_STEP_M: f64 = 20.0;
const BACK_ALONG_M: f64 = 60.0;
const SHORT_EDGE_M: f64 = 15.0;

impl Used {
    fn cell(p: Xy) -> (i32, i32) {
        ((p[0] / USED_CELL_M).floor() as i32, (p[1] / USED_CELL_M).floor() as i32)
    }

    /// Records a day's route.
    fn add(&mut self, g: &Graph, path: &[(usize, bool)]) {
        for &(ei, fwd) in path {
            self.edges.insert(ei);
            let mut pts = g.edges[ei].pts.clone();
            if !fwd {
                pts.reverse();
            }
            for w in pts.windows(2) {
                let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
                let l = dx.hypot(dy);
                if l <= 0.0 {
                    continue;
                }
                let k = (l / USED_STEP_M).ceil() as usize;
                for i in 0..k {
                    let t = i as f64 / k as f64;
                    let p = [w[0][0] + dx * t, w[0][1] + dy * t];
                    self.grid.entry(Self::cell(p)).or_default().push((
                        p[0] as f32,
                        p[1] as f32,
                        (dx / l) as f32,
                        (dy / l) as f32,
                    ));
                }
            }
        }
    }

    /// Whether walking edge `ei` (forward: a to b) would repeat a used way or go back along one.
    fn blocks(&self, g: &Graph, ei: usize, fwd: bool) -> bool {
        let e = &g.edges[ei];
        if e.len < SHORT_EDGE_M {
            return false;
        }
        if self.edges.contains(&ei) {
            return true;
        }
        // The middle of the edge, and the direction walked there.
        let (a, b) = (e.pts[e.pts.len() / 2 - usize::from(e.pts.len() > 1)], e.pts[e.pts.len() / 2]);
        let (dx, dy) = if fwd { (b[0] - a[0], b[1] - a[1]) } else { (a[0] - b[0], a[1] - b[1]) };
        let l = dx.hypot(dy);
        if l <= 0.0 {
            return false;
        }
        let (hx, hy) = (dx / l, dy / l);
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let (cx, cy) = Self::cell(mid);
        for i in cx - 1..=cx + 1 {
            for j in cy - 1..=cy + 1 {
                for &(x, y, ux, uy) in self.grid.get(&(i, j)).into_iter().flatten() {
                    let near = (x as f64 - mid[0]).hypot(y as f64 - mid[1]) <= BACK_ALONG_M;
                    if near && (hx * ux as f64 + hy * uy as f64) < -0.6 {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// A routed leg may cost this many times what its search thought, plus this much (m).
const ROUTE_SLACK: f32 = 2.0;
const ROUTE_SLACK_M: f32 = 3000.0;
/// Chains running back alongside themselves for more than this (m) are repaired as above.
const DOUBLE_BACK_REPAIR_M: f64 = 60.0;
/// Fewest rings for a centre before chains that still double back are let in.
const MIN_RINGS: usize = 2;
/// Enough rings for a centre: no more repair rounds once there are this many.
const ENOUGH_RINGS: usize = 4;
/// How many times chains are chosen again after dropping anchors that can't be routed to.
const REPAIR_ROUNDS: usize = 3;
/// A way already walked by an earlier day costs this many times as much.
const REUSE: f32 = 8.0;

fn wrap(a: f64) -> f64 {
    let a = (a + PI).rem_euclid(TAU) - PI;
    if a == -PI { PI } else { a }
}

/// Angle from `a` to `b` going counter-clockwise, in [0, TAU).
fn ccw(a: f64, b: f64) -> f64 {
    (b - a).rem_euclid(TAU)
}

fn bearing_from(p: Xy, c: Xy) -> f64 {
    (p[1] - c[1]).atan2(p[0] - c[0])
}

fn bin_of(theta: f64) -> usize {
    (((theta + PI) / BIN) as usize).min(BINS - 1)
}

// ======================================================================
// 1. Network and 2. costs
// ======================================================================

/// What every plan shares: the usable network and the cost of each way.
struct Net<'a> {
    b: &'a Built,
    cfg: &'a Config,
    walking: bool,
    csr: Csr,
    /// Cost of each edge, in metres of walking.
    cost: Vec<f32>,
    /// Heading leaving each end of an edge (a->b at a; b->a at b).
    head_a: Vec<f32>,
    head_b: Vec<f32>,
    /// The least cost per metre of any way (a bound for the route search).
    min_per_m: f32,
    /// Walking mode: distance from the base.
    base: Option<BaseTree>,
}

impl<'a> Net<'a> {
    fn new(b: &'a Built, cfg: &'a Config) -> Net<'a> {
        let g = &b.graph;
        // Only ways this way of travelling can use.
        let usable = |e: &Edge| if cfg.travel == Travel::Bike { e.bike } else { e.foot };
        let all = Csr::new(g, |_, e| usable(e));
        let live = live_nodes(g, &all);
        let csr = Csr::new(g, |_, e| usable(e) && live[e.a] && live[e.b]);
        let weights = cfg.class_weights();
        let cost: Vec<f32> =
            g.edges.iter().enumerate().map(|(i, e)| edge_cost(b, cfg, i, e, &weights) as f32).collect();
        let head = |p: Xy, q: Xy| (q[1] - p[1]).atan2(q[0] - p[0]) as f32;
        let n = |e: &Edge| e.pts.len();
        let head_a = g.edges.iter().map(|e| head(e.pts[0], e.pts[1])).collect();
        let head_b = g.edges.iter().map(|e| head(e.pts[n(e) - 1], e.pts[n(e) - 2])).collect();
        let walking = cfg.is_direct();
        let base = walking.then(|| BaseTree::new(g, cfg.travel, cfg.climb_cost()));
        let min_per_m = cost
            .iter()
            .zip(&g.edges)
            .filter(|(_, e)| e.len > 1.0)
            .map(|(&c, e)| c / e.len as f32)
            .fold(1.0f32, f32::min)
            .max(0.05)
            * 0.95;
        Net { b, cfg, walking, csr, cost, head_a, head_b, min_per_m, base }
    }

    fn g(&self) -> &Graph {
        &self.b.graph
    }
}

/// The piece of the network round the base, without dead ends: the part
/// beyond any single way in, away from the base, if it is a small share of
/// the network. A day that ends out there has no way home but the way it came.
fn live_nodes(g: &Graph, csr: &Csr) -> Vec<bool> {
    const NEAR_BASE_M: f64 = 3000.0;
    // A part beyond a single way, away from the base, and smaller than this share of the network.
    const PENDANT_SHARE: f64 = 0.35;
    let n = g.nodes.len();
    // The connected piece with the most nodes near the base.
    let mut label = vec![u32::MAX; n];
    let (mut best, mut best_label) = (0usize, u32::MAX);
    let mut next = 0u32;
    for s in 0..n {
        if label[s] != u32::MAX || csr.degree(s) == 0 {
            continue;
        }
        label[s] = next;
        let (mut stack, mut near) = (vec![s], 0usize);
        while let Some(u) = stack.pop() {
            near += (norm(g.nodes[u]) <= NEAR_BASE_M) as usize;
            for &(v, _) in csr.nbrs(u) {
                if label[v as usize] == u32::MAX {
                    label[v as usize] = next;
                    stack.push(v as usize);
                }
            }
        }
        if near > best {
            (best, best_label) = (near, next);
        }
        next += 1;
    }
    let mut live: Vec<bool> = label.iter().map(|&l| l == best_label).collect();
    let pendant_max = (PENDANT_SHARE * live.iter().filter(|&&l| l).count() as f64) as usize;
    // Bridges, by an iterative depth-first search from the node nearest the base.
    let Some(root) = (0..n)
        .filter(|&u| live[u] && csr.degree(u) >= 2)
        .min_by(|&a, &b| norm(g.nodes[a]).total_cmp(&norm(g.nodes[b])))
    else {
        return live;
    };
    let mut tin = vec![u32::MAX; n];
    let mut low = vec![0u32; n];
    let mut via = vec![u32::MAX; n];
    let mut order: Vec<usize> = vec![root];
    let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
    tin[root] = 0;
    while let Some(&(u, i)) = stack.last() {
        let nbrs = csr.nbrs(u);
        if i < nbrs.len() {
            stack.last_mut().unwrap().1 += 1;
            let (v, ei) = (nbrs[i].0 as usize, nbrs[i].1);
            if ei == via[u] {
                continue;
            }
            if tin[v] == u32::MAX {
                tin[v] = order.len() as u32;
                low[v] = tin[v];
                via[v] = ei;
                order.push(v);
                stack.push((v, 0));
            } else {
                low[u] = low[u].min(tin[v]);
            }
        } else {
            stack.pop();
            if let Some(&(p, _)) = stack.last() {
                low[p] = low[p].min(low[u]);
                // u's side of the bridge p-u is order[tin[u]..now].
                let size = order.len() - tin[u] as usize;
                if low[u] > tin[p] && size < pendant_max {
                    for &w in &order[tin[u] as usize..] {
                        live[w] = false;
                    }
                }
            }
        }
    }
    live
}

/// Cost of walking an edge: metres times the way's preferences, plus climb.
fn edge_cost(b: &Built, cfg: &Config, ei: usize, e: &Edge, weights: &[f32; 4]) -> f64 {
    let mut m = weights[e.class()] as f64;
    if cfg.is_bike() {
        // A cyclist wants cycle ways and signed bicycle routes, not walking routes.
        if e.cycle {
            m *= cfg.cycle_bonus;
        }
        if e.bike_route {
            m *= cfg.bike_route_bonus;
        }
    } else if e.on_route {
        m *= cfg.route_bonus;
    }
    if e.waterside && cfg.prefer_waterside {
        m *= crate::config::WATERSIDE_BONUS;
    }
    m *= 1.0 - cfg.shade_weight * b.feel.shade[ei] as f64;
    m *= 1.0 - cfg.life_weight * b.feel.life[ei] as f64;
    e.len * m + cfg.climb_cost() * e.rise.abs() as f64 / 2.0
}

// ======================================================================
// 3. Centres, 4. profile, 5. size
// ======================================================================

pub fn search(b: &Built, cfg: &Config, cancel: &AtomicBool) -> Vec<Ring> {
    let net = Net::new(b, cfg);
    centres(&net).par_iter().flat_map(|&c| if cancel.load(Relaxed) { vec![] } else { plan_round(&net, c) }).collect()
}

/// Plans round one centre. If the rings found wind differently from what the size was worked out for,
/// the size is worked out again with what was measured, so the days come out as asked.
fn plan_round(net: &Net, c: Xy) -> Vec<Ring> {
    let (mut rings, measured) = Plan::new(net, c, WINDING, true).rings();
    if let Some(m) = measured.filter(|m| (m / WINDING - 1.0).abs() > WINDING_TOLERANCE) {
        let w = m.clamp(WINDING_RANGE.0, WINDING_RANGE.1);
        rings.extend(Plan::new(net, c, w, true).rings().0);
    }
    if rings.is_empty() && net.cfg.access == Access::Transit {
        rings = Plan::new(net, c, WINDING, false).rings().0;
    }
    rings
}

/// The base, and the middle of the land if that is within the offset limit.
fn centres(net: &Net) -> Vec<Xy> {
    let g = net.g();
    let limit = net.cfg.centroid_offset_km * 1000.0;
    let mut out = vec![[0.0, 0.0]];
    if limit <= 0.0 {
        return out;
    }
    let reach = net.cfg.day_m() * net.cfg.days().max(1.0) / TAU;
    let (mut sum, mut n) = ([0.0, 0.0], 0.0);
    for u in 0..g.nodes.len() {
        if net.csr.degree(u) > 0 && norm(g.nodes[u]) <= reach.clamp(3000.0, 12000.0) {
            sum = [sum[0] + g.nodes[u][0], sum[1] + g.nodes[u][1]];
            n += 1.0;
        }
    }
    if n > 0.0 {
        let m = [sum[0] / n, sum[1] / n];
        let d = norm(m);
        let c = if d > limit { [m[0] * limit / d, m[1] * limit / d] } else { m };
        if norm(c) > 500.0 {
            out.push(c);
        }
    }
    out
}

/// The radius at which the ring should run, by bearing.
struct Profile {
    /// Radius per bin (m), or 0 for a gap.
    rho: [f64; BINS],
    /// Perimeter (m) of the whole ring if gaps were land at R, and of the part that is land: the arc
    /// along the profile plus the radial steps between bins.
    perimeter: f64,
    land_perimeter: f64,
}

impl Profile {
    fn is_gap(&self, bin: usize) -> bool {
        self.rho[bin] == 0.0
    }
}

struct Plan<'a> {
    net: &'a Net<'a>,
    c: Xy,
    /// How much longer than a circle through the same points a ring is assumed to be.
    winding: f64,
    /// Transit mode: days must start and end where transit goes. Dropped as a last resort, so that a
    /// town with little transit still gets a plan (whose days say they are unreachable).
    need_transit: bool,
    /// Per node: distance and bearing from the centre, and bin.
    rc: Vec<f32>,
    th: Vec<f32>,
    bin: Vec<u8>,
    /// Points along the network, about every `SAMPLE_M`: (bin, distance from the centre). What the
    /// profile counts as land.
    samples: Vec<(u8, f32)>,
}

impl<'a> Plan<'a> {
    fn new(net: &'a Net<'a>, c: Xy, winding: f64, need_transit: bool) -> Plan<'a> {
        let g = net.g();
        let rc = g.nodes.iter().map(|&p| dist(p, c) as f32).collect();
        let th: Vec<f32> = g.nodes.iter().map(|&p| bearing_from(p, c) as f32).collect();
        let bin = th.iter().map(|&t| bin_of(t as f64) as u8).collect();
        // Land is where there is way, not only where there are nodes: a long straight path has few.
        let mut samples = vec![];
        for e in g.edges.iter().filter(|e| net.csr.degree(e.a) > 0 && net.csr.degree(e.b) > 0) {
            for w in e.pts.windows(2) {
                let k = (dist(w[0], w[1]) / SAMPLE_M).ceil().max(1.0) as usize;
                for i in 0..k {
                    let t = i as f64 / k as f64;
                    let p = [w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t];
                    samples.push((bin_of(bearing_from(p, c)) as u8, dist(p, c) as f32));
                }
            }
        }
        Plan { net, c, winding, need_transit, rc, th, bin, samples }
    }

    fn g(&self) -> &Graph {
        self.net.g()
    }

    /// Step 4: the profile for a given R.
    fn profile(&self, r: f64) -> Profile {
        let cell = r / CELLS as f64;
        let mut count = vec![[0u16; CELLS + 3]; BINS];
        let mut far = vec![[0f32; CELLS + 3]; BINS];
        for &(bin, rc) in &self.samples {
            let k = (rc as f64 / cell) as usize;
            if k < CELLS + 3 {
                let c = &mut count[bin as usize][k];
                *c = c.saturating_add(1);
                let f = &mut far[bin as usize][k];
                *f = f.max(rc);
            }
        }
        let mut rho = [0.0; BINS];
        for (i, cells) in count.iter().enumerate() {
            let land = |k: usize| cells[k] as usize >= MIN_NODES;
            // Land out to R: run at R. Else at the farthest land (the shore, or the edge of the map).
            if (CELLS - 1..CELLS + 3).any(land) {
                rho[i] = r;
            } else if let Some(top) = (0..CELLS - 1).rev().find(|&k| land(k)) {
                // The farthest way in the farthest cell with land.
                let at = far[i][top] as f64;
                if at >= (MIN_RHO * r).max(MIN_RHO_M) {
                    rho[i] = at;
                }
            }
        }
        // A stretch pulled in to the shore that is too short to make a day is no stretch at all: a gap.
        let short = |rho: &[f64; BINS], i: usize| rho[i] > 0.0 && rho[i] < PULLED_IN * r;
        if let Some(start) = (0..BINS).find(|&i| !short(&rho, i)) {
            let (mut run, mut len) = (vec![], 0.0);
            for k in 1..=BINS {
                let i = (start + k) % BINS;
                if short(&rho, i) {
                    run.push(i);
                    len += self.winding * rho[i] * BIN;
                } else {
                    if !run.is_empty() && len < MIN_PULLED_IN * self.arc(r) {
                        run.iter().for_each(|&j| rho[j] = 0.0);
                    }
                    run.clear();
                    len = 0.0;
                }
            }
        }
        // Bridge short gaps with their neighbours' radius, so only real stretches of water are gaps.
        let mut filled = rho;
        for i in 0..BINS {
            if rho[i] > 0.0 {
                continue;
            }
            let run = (0..BINS).take_while(|&k| rho[(i + k) % BINS] == 0.0).count();
            let before = (0..BINS).take_while(|&k| rho[(i + BINS - 1 - k) % BINS] == 0.0).count();
            if run + before < MIN_GAP_BINS {
                let (a, z) = (rho[(i + BINS - 1 - before) % BINS], rho[(i + run) % BINS]);
                if a > 0.0 && z > 0.0 {
                    filled[i] = (a + z) / 2.0;
                }
            }
        }
        // Smooth: the median of the land bins within two either side, so one odd bin doesn't make a spike.
        let rho: [f64; BINS] = std::array::from_fn(|i| {
            if filled[i] == 0.0 {
                return 0.0;
            }
            let mut near: Vec<f64> = (0..5).map(|k| filled[(i + BINS + k - 2) % BINS]).filter(|&x| x > 0.0).collect();
            near.sort_by(|a, b| a.total_cmp(b));
            // Never beyond the land that is really there.
            near[near.len() / 2].min(filled[i])
        });
        let (mut all, mut land) = (0.0, 0.0);
        for i in 0..BINS {
            let j = (i + 1) % BINS;
            all += if rho[i] > 0.0 { rho[i] * BIN } else { r * BIN };
            if rho[i] > 0.0 {
                land += rho[i] * BIN;
                if rho[j] > 0.0 {
                    land += (rho[i] - rho[j]).abs();
                    all += (rho[i] - rho[j]).abs();
                }
            }
        }
        Profile { rho, perimeter: all, land_perimeter: land }
    }

    /// A day's arc (m) if the ring is at distance `r`.
    fn arc(&self, r: f64) -> f64 {
        let day = self.net.cfg.day_m();
        if self.net.walking { (day - 2.0 * APPROACH_WINDING * r).max(0.25 * day) * 0.95 } else { ARC_FILL * day }
    }

    /// Step 5: R at which the profile's length is the days asked for; the map's limit if it can't be.
    fn size(&self, days: f64) -> (f64, Profile) {
        let cfg = self.net.cfg;
        let mut hi = (cfg.extract_radius_km * 1000.0) * MAP_SHARE;
        if self.net.walking {
            hi = hi.min(cfg.day_m() * 0.4);
        }
        let short = |r: f64| self.winding * self.profile(r).perimeter - days * self.arc(r);
        let mut lo = R_MIN_M.min(hi);
        if short(hi) <= 0.0 {
            return (hi, self.profile(hi));
        }
        for _ in 0..18 {
            let mid = (lo + hi) / 2.0;
            if short(mid) < 0.0 { lo = mid } else { hi = mid }
        }
        let r = (lo + hi) / 2.0;
        (r, self.profile(r))
    }
}

// ======================================================================
// 6. Anchors
// ======================================================================

struct Anchor {
    node: usize,
    /// Bearing from the centre.
    theta: f64,
    /// Cost of ending a day here, without (`cost`) and with (`radial`) the pull towards the profile.
    cost: f64,
    radial: f64,
    /// Transit mode: cost of getting there from the base, and back.
    go: f64,
    back: f64,
    /// Walking mode: walk from the base (m), and metres climbed walking out and home.
    walk: f64,
    climb_out: f64,
    climb_home: f64,
    shore: bool,
}

/// A candidate day between two anchors.
#[derive(Clone, Copy)]
struct Leg {
    to: u32,
    /// Cost per metre (1 = plain walking) and length (m).
    mult: f32,
    len: f32,
    /// Metres climbed along it.
    up: f32,
}

impl Plan<'_> {
    /// The ways at a node: their mean street life, whether any is on a shore, and the share that are
    /// car-free paths.
    fn around(&self, u: usize) -> (f64, bool, f64) {
        let g = self.g();
        let (mut sum, mut n, mut shore, mut paths) = (0.0, 0, false, 0);
        for &(_, ei) in self.net.csr.nbrs(u) {
            let e = &g.edges[ei as usize];
            sum += self.net.b.feel.life[ei as usize] as f64;
            n += 1;
            shore |= e.shore;
            // On foot a path is a pleasant place to end a day; on a bicycle, a cycle way.
            paths += if self.net.cfg.is_bike() { e.cycle || e.bike_route } else { e.kind == crate::osm::Kind::CarFree }
                as usize;
        }
        let n = n.max(1) as f64;
        (sum / n, shore, paths as f64 / n)
    }

    /// One node per cell near the profile (and the best shore node), thinned until there are few enough.
    fn thinned(&self, prof: &Profile, r: f64) -> Vec<usize> {
        type Best = std::collections::HashMap<(i64, i64), (f64, usize)>;
        let g = self.g();
        // Candidates near the profile, with how good each is as an end of a day.
        let mut cands: Vec<(usize, f64, bool)> = vec![];
        for u in 0..g.nodes.len() {
            let deg = self.net.csr.degree(u);
            let rho = prof.rho[self.bin[u] as usize];
            if deg == 0 || rho == 0.0 {
                continue;
            }
            let rc = self.rc[u] as f64;
            // Cheap test first: the widest band any node could be in.
            if rc < SHORE_IN * r || rc > ANCHOR_BAND.1 * rho {
                continue;
            }
            let (life, shore, paths) = self.around(u);
            let ok = shore || (ANCHOR_BAND.0 * rho..=ANCHOR_BAND.1 * rho).contains(&rc);
            if !ok {
                continue;
            }
            let score = self.net.b.heads.score_at(g.nodes[u])
                + 0.3 * life
                + 0.15 * shore as u8 as f64
                + 0.25 * paths
                + 0.02 * deg.min(4) as f64;
            cands.push((u, score, shore));
        }
        let mut cell_m = ANCHOR_CELL_M;
        loop {
            let (mut best, mut best_shore): (Best, Best) = Default::default();
            for &(u, score, shore) in &cands {
                let p = g.nodes[u];
                let cell = ((p[0] / cell_m).floor() as i64, (p[1] / cell_m).floor() as i64);
                if best.get(&cell).is_none_or(|x| score > x.0) {
                    best.insert(cell, (score, u));
                }
                if shore && best_shore.get(&cell).is_none_or(|x| score > x.0) {
                    best_shore.insert(cell, (score, u));
                }
            }
            let mut nodes: Vec<usize> = best.values().chain(best_shore.values()).map(|x| x.1).collect();
            nodes.sort_unstable();
            nodes.dedup();
            if nodes.len() <= MAX_ANCHORS {
                return nodes;
            }
            cell_m *= 1.25;
        }
    }

    fn anchors(&self, prof: &Profile, r: f64, day: f64) -> Vec<Anchor> {
        let (net, g) = (self.net, self.g());
        let cfg = net.cfg;
        let access = |a: Option<crate::transit::Access>| {
            a.map_or(NO_ACCESS_M, |a| {
                a.walk_m + CENTER_WALK_WEIGHT * a.center_walk_m + 500.0 * a.legs.len().saturating_sub(1) as f64
            })
        };
        self.thinned(prof, r)
            .into_iter()
            .filter_map(|u| {
                let p = g.nodes[u];
                let rho = prof.rho[self.bin[u] as usize];
                let (life, shore, paths) = self.around(u);
                let walk = net.base.as_ref().and_then(|b| b.walk_m(u));
                // Out climbs what the walk from the base climbs; home climbs what it descends.
                let (climb_out, climb_home) = net.base.as_ref().map_or((0.0, 0.0), |b| b.climb(u));
                if net.walking && walk.is_none_or(|w| w >= day * 0.8) {
                    return None;
                }
                let (go, back) = if net.walking {
                    (0.0, 0.0)
                } else {
                    // A day must start and end where transit gets you there and home.
                    let (go, back) = (
                        net.b.transit.access(p, cfg.access_walk_m, true),
                        net.b.transit.access(p, cfg.access_walk_m, false),
                    );
                    if self.need_transit && (go.is_none() || back.is_none()) {
                        return None;
                    }
                    (access(go), access(back))
                };
                let head = net.b.heads.score_at(p).min(1.0);
                let cost = cfg.trailhead_weight_m * (1.0 - head)
                    - LIFE_BONUS_M * life
                    - SHORE_BONUS_M * shore as u8 as f64
                    - PATH_BONUS_M * paths;
                let radial = RADIAL_W * ((self.rc[u] as f64 - rho).abs() / rho).min(1.5).powi(2);
                Some(Anchor {
                    node: u,
                    theta: self.th[u] as f64,
                    cost,
                    radial,
                    go,
                    back,
                    walk: walk.unwrap_or(0.0),
                    climb_out,
                    climb_home,
                    shore,
                })
            })
            .collect()
    }
}

// ======================================================================
// 7. Legs
// ======================================================================

/// Search state for `legs_from`: distance, length, metres climbed, and the nodes touched.
type LegBuf = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<u32>);

thread_local! {
    /// Per-thread search state for `legs_from`.
    static LEG_BUF: std::cell::RefCell<LegBuf> = const { std::cell::RefCell::new((vec![], vec![], vec![], vec![])) };
}

impl Plan<'_> {
    /// Per node: may a leg pass here? Inside the corridor round the profile (wide enough, near a jump in
    /// the profile, to get from one radius to the other), or on a shore.
    fn corridor(&self, prof: &Profile) -> Vec<bool> {
        let g = self.g();
        let span: Vec<(f64, f64)> = (0..BINS)
            .map(|i| {
                let near: Vec<f64> = (0..5).map(|k| prof.rho[(i + BINS + k - 2) % BINS]).filter(|&x| x > 0.0).collect();
                let lo = near.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = near.iter().cloned().fold(0.0, f64::max);
                (CORRIDOR.0 * lo, CORRIDOR.1 * hi)
            })
            .collect();
        (0..g.nodes.len())
            .map(|u| {
                if self.net.csr.degree(u) == 0 {
                    return false;
                }
                let rc = self.rc[u] as f64;
                let (lo, hi) = span[self.bin[u] as usize];
                (prof.rho[self.bin[u] as usize] > 0.0 && (lo..=hi).contains(&rc))
                    || self.net.csr.nbrs(u).iter().any(|&(_, ei)| g.edges[ei as usize].shore)
            })
            .collect()
    }

    fn legs(&self, anchors: &[Anchor], ok: &[bool], day: f64) -> Vec<Vec<Leg>> {
        let mut at = vec![NONE; self.g().nodes.len()];
        for (i, a) in anchors.iter().enumerate() {
            at[a.node] = i as u32;
        }
        anchors.par_iter().enumerate().map(|(i, a)| self.legs_from(i, a, anchors, &at, ok, day)).collect()
    }

    /// Cheapest routes from one anchor to the anchors 5-150 degrees further round, up to a day long.
    fn legs_from(&self, i: usize, a: &Anchor, anchors: &[Anchor], at: &[u32], ok: &[bool], day: f64) -> Vec<Leg> {
        let (net, g) = (self.net, self.g());
        let n = g.nodes.len();
        let max_len = day as f32;
        let mut out = vec![];
        LEG_BUF.with_borrow_mut(|(d, len, up, touched)| {
            if d.len() < n {
                *d = vec![f32::INFINITY; n];
                *len = vec![0f32; n];
                *up = vec![0f32; n];
            }
            for &t in touched.iter() {
                d[t as usize] = f32::INFINITY;
            }
            touched.clear();
            let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
            d[a.node] = 0.0;
            len[a.node] = 0.0;
            up[a.node] = 0.0;
            touched.push(a.node as u32);
            heap.push(Reverse((0f32.to_bits(), a.node as u32)));
            while let Some(Reverse((bits, u))) = heap.pop() {
                let (du, u) = (f32::from_bits(bits), u as usize);
                if du > d[u] {
                    continue;
                }
                let k = at[u];
                if k != NONE && k as usize != i && len[u] >= MIN_LEG_M as f32 {
                    let step = ccw(a.theta, anchors[k as usize].theta).to_degrees();
                    if step > STEP_DEG.0 && step < STEP_DEG.1 {
                        out.push(Leg { to: k, mult: du / len[u], len: len[u], up: up[u] });
                    }
                }
                for &(v, ei) in net.csr.nbrs(u) {
                    let (v, ei) = (v as usize, ei as usize);
                    let nl = len[u] + g.edges[ei].len as f32;
                    if nl > max_len || !ok[v] {
                        continue;
                    }
                    // Never back the way round: a leg keeps going the same way about the centre.
                    if wrap(self.th[v] as f64 - self.th[u] as f64) < -BACK_TOL {
                        continue;
                    }
                    // Not far behind the start, or beyond the furthest leg.
                    let ahead = ccw(a.theta, self.th[v] as f64);
                    if ahead > STEP_DEG.1.to_radians() + 0.3 && ahead < TAU - 0.2 {
                        continue;
                    }
                    let nd = du + net.cost[ei];
                    if nd < d[v] {
                        if d[v] == f32::INFINITY {
                            touched.push(v as u32);
                        }
                        d[v] = nd;
                        len[v] = nl;
                        up[v] = up[u] + g.edges[ei].climb(g.edges[ei].a == u) as f32;
                        heap.push(Reverse((nd.to_bits(), v as u32)));
                    }
                }
            }
        });
        out
    }
}

// ======================================================================
// 8. Chain
// ======================================================================

/// A chain found: anchors in order (n + 1 of them) and its cost.
struct Picked {
    anchors: Vec<usize>,
    cost: f64,
    /// What the search thought each leg's way cost, to bound routing it.
    leg_costs: Vec<f32>,
}

#[derive(Clone, Copy, PartialEq)]
enum Order {
    Closed,
    Open,
}

impl Plan<'_> {
    /// What a leg from anchor `a` to `b` costs, if it fits the day.
    fn leg_cost(&self, a: &Anchor, b: &Anchor, l: &Leg, day: f64, edge: bool) -> Option<f64> {
        let allow = if self.net.walking { day - WALK_MARGIN * (a.walk + b.walk) } else { day };
        if allow < MIN_LEG_M {
            return None;
        }
        let len = l.len as f64;
        let fill = if edge { EDGE_FILL } else { MIN_FILL };
        if len > allow || len < (fill * allow).max(MIN_LEG_M) {
            return None;
        }
        // A leg far longer than the straight line has to double back to fill the day.
        let g = self.g();
        if len > WIND_MAX * dist(g.nodes[a.node], g.nodes[b.node]) + WIND_SLACK_M {
            return None;
        }
        let access = if self.net.walking { 0.0 } else { a.go + b.back };
        // The ends of an open chain sit on the shore, wherever that is.
        let radial = if edge && b.shore { 0.0 } else { b.radial };
        // A day climbing more than the limit costs a lot more, so days are cut to stay within it where they can.
        let climb = l.up as f64 + if self.net.walking { a.climb_out + b.climb_home } else { 0.0 };
        let over = (climb - self.net.cfg.max_climb_m()).max(0.0);
        Some(l.mult as f64 * day + SHORT_W * (allow - len) + b.cost + radial + access + CLIMB_OVER_W * over)
    }

    /// Closed chain of `n` legs, started from anchors spread round the circle.
    fn closed(&self, anchors: &[Anchor], legs: &[Vec<Leg>], n: usize, day: f64, banned: &[bool]) -> Vec<Picked> {
        let mut by_sector: Vec<Option<usize>> = vec![None; STARTS];
        for (i, a) in anchors.iter().enumerate().filter(|&(i, _)| !banned[i]) {
            let s = ((a.theta + PI) / TAU * STARTS as f64) as usize % STARTS;
            if by_sector[s].is_none_or(|j| anchors[j].cost > a.cost) {
                by_sector[s] = Some(i);
            }
        }
        let mut found: Vec<Picked> = by_sector
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let u = |i: usize| if i == s { 0.0 } else { ccw(anchors[s].theta, anchors[i].theta) };
                let cost = |a: usize, k: &Leg, _layer: usize| {
                    self.leg_cost(&anchors[a], &anchors[k.to as usize], k, day, false)
                };
                let valid = |a: usize, k: &Leg, last: bool| {
                    let b = k.to as usize;
                    !banned[b] && if last { b == s } else { b != s && u(b) > u(a) }
                };
                run_dp(anchors.len(), legs, n, &[(s, anchors[s].cost + anchors[s].radial)], &cost, &valid, |i| {
                    (i == s).then_some(0.0)
                })
            })
            .collect();
        found.sort_by(|x, y| x.cost.total_cmp(&y.cost));
        found.truncate(KEEP);
        found
    }

    /// Open chain of `n` legs over the land from one end of it to the other.
    #[allow(clippy::too_many_arguments)]
    fn open(
        &self,
        anchors: &[Anchor],
        legs: &[Vec<Leg>],
        n: usize,
        day: f64,
        cut: f64,
        lo: f64,
        hi: f64,
        banned: &[bool],
    ) -> Option<Picked> {
        let u = |i: usize| ccw(cut, anchors[i].theta);
        let total = n as f64 * day;
        let zone = END_ZONE_DEG.to_radians();
        let starts: Vec<(usize, f64)> = (0..anchors.len())
            .filter(|&i| u(i) <= lo + zone && !banned[i])
            .map(|i| {
                (
                    i,
                    anchors[i].cost
                        + END_W * total * ((u(i) - lo).max(0.0) / TAU)
                        + if anchors[i].shore { 0.0 } else { INLAND_END_M },
                )
            })
            .collect();
        if starts.is_empty() {
            return None;
        }
        let cost = |a: usize, k: &Leg, layer: usize| {
            self.leg_cost(&anchors[a], &anchors[k.to as usize], k, day, layer == 1 || layer == n)
        };
        let valid = |a: usize, k: &Leg, _last: bool| !banned[k.to as usize] && u(k.to as usize) > u(a);
        run_dp(anchors.len(), legs, n, &starts, &cost, &valid, |i| {
            (u(i) >= hi - zone).then(|| {
                END_W * total * ((hi - u(i)).max(0.0) / TAU) + if anchors[i].shore { 0.0 } else { INLAND_END_M }
            })
        })
    }
}

/// Layered shortest path: `n` legs from a start (with its cost) to an end
/// (`finish` gives its extra cost, or None if the chain can't end there).
/// `cost` prices a leg in layer 1..=n; `valid` says whether it may be used
/// (`last`: it is the final leg).
fn run_dp(
    n_anchors: usize,
    legs: &[Vec<Leg>],
    n: usize,
    starts: &[(usize, f64)],
    cost: &dyn Fn(usize, &Leg, usize) -> Option<f64>,
    valid: &dyn Fn(usize, &Leg, bool) -> bool,
    finish: impl Fn(usize) -> Option<f64>,
) -> Option<Picked> {
    let mut dp = vec![f64::INFINITY; n_anchors];
    for &(s, c) in starts {
        dp[s] = dp[s].min(c);
    }
    let mut back: Vec<Vec<u32>> = vec![];
    let mut back_cost: Vec<Vec<f32>> = vec![];
    for k in 1..=n {
        let mut next = vec![f64::INFINITY; n_anchors];
        let mut from = vec![NONE; n_anchors];
        let mut from_cost = vec![0f32; n_anchors];
        for a in (0..n_anchors).filter(|&a| dp[a].is_finite()) {
            for l in legs[a].iter().filter(|l| valid(a, l, k == n)) {
                let Some(c) = cost(a, l, k) else { continue };
                let b = l.to as usize;
                if dp[a] + c < next[b] {
                    next[b] = dp[a] + c;
                    from[b] = a as u32;
                    from_cost[b] = l.mult * l.len;
                }
            }
        }
        dp = next;
        back.push(from);
        back_cost.push(from_cost);
    }
    let (end, total) = (0..n_anchors)
        .filter(|&i| dp[i].is_finite())
        .filter_map(|i| finish(i).map(|f| (i, dp[i] + f)))
        .min_by(|x, y| x.1.total_cmp(&y.1))?;
    let mut chain = vec![end];
    let mut leg_costs = vec![];
    for k in (0..n).rev() {
        let at = *chain.last().unwrap();
        let prev = back[k][at];
        if prev == NONE {
            return None;
        }
        leg_costs.push(back_cost[k][at]);
        chain.push(prev as usize);
    }
    chain.reverse();
    leg_costs.reverse();
    Some(Picked { anchors: chain, cost: total, leg_costs })
}

// ======================================================================
// 9. Route
// ======================================================================

thread_local! {
    /// Per-thread state for `route`: distance and previous directed edge, and the states touched.
    static ROUTE_BUF: std::cell::RefCell<(Vec<f32>, Vec<u32>, Vec<u32>)> = const { std::cell::RefCell::new((vec![], vec![], vec![])) };
}

impl Net<'_> {
    /// The cheapest route between two nodes that charges for turns and for
    /// changing street. State: directed edge (2e is a->b, 2e+1 is b->a).
    /// `th`: with each node's bearing round the ring's centre, ways never go back round it by more than
    /// `BACK_TOL`; without, they are free to.
    #[allow(clippy::too_many_arguments)]
    fn route(
        &self,
        from: usize,
        to: usize,
        ok: &[bool],
        used: &Used,
        strict: bool,
        limit: f32,
        th: Option<&[f32]>,
    ) -> Option<Vec<(usize, bool)>> {
        let (net, g) = (self, self.g());
        let back = |u: usize, v: usize| th.is_some_and(|th| wrap(th[v] as f64 - th[u] as f64) < -BACK_TOL);
        let n = g.edges.len() * 2;
        let end_of = |s: usize| {
            let e = &g.edges[s / 2];
            if s.is_multiple_of(2) { e.b } else { e.a }
        };
        let leave = |s: usize| if s.is_multiple_of(2) { net.head_a[s / 2] } else { net.head_b[s / 2] } as f64;
        // Heading on arriving is the reverse of leaving at that end.
        let arrive = |s: usize| (if s.is_multiple_of(2) { net.head_b[s / 2] } else { net.head_a[s / 2] }) as f64 + PI;
        let turn_cost = |p: usize, s: usize| {
            let phi = wrap(leave(s) - arrive(p)).abs();
            if phi <= TURN_FREE {
                return 0.0;
            }
            let t = ((phi - TURN_FREE) / (TURN_FULL - TURN_FREE)).min(1.0);
            let same = g.edges[p / 2].way_id == g.edges[s / 2].way_id;
            net.cfg.turn_cost_m * t * if same { 1.0 } else { 1.0 + SWITCH_SHARE }
        };
        ROUTE_BUF.with_borrow_mut(|(d, prev, touched)| {
            if d.len() < n {
                *d = vec![f32::INFINITY; n];
                *prev = vec![NONE; n];
            }
            for &t in touched.iter() {
                d[t as usize] = f32::INFINITY;
            }
            touched.clear();
            let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
            // A*: the straight line to the goal at the cheapest cost per metre.
            let goal = g.nodes[to];
            let h = |v: usize| net.min_per_m * dist(g.nodes[v], goal) as f32;
            let relax = |heap: &mut BinaryHeap<Reverse<(u32, u32)>>,
                         d: &mut Vec<f32>,
                         prev: &mut Vec<u32>,
                         touched: &mut Vec<u32>,
                         p: Option<usize>,
                         u: usize| {
                let base = p.map_or(0.0, |p| d[p]);
                for &(_, ei) in net.csr.nbrs(u) {
                    let ei = ei as usize;
                    let e = &g.edges[ei];
                    let v = if e.a == u { e.b } else { e.a };
                    let s = 2 * ei + (e.a != u) as usize;
                    if !ok[v] || p == Some(s ^ 1) || back(u, v) {
                        continue;
                    }
                    // Walking a way again, or back along one, is not allowed (strict) or costs a lot.
                    let blocked = used.blocks(g, ei, e.a == u);
                    if blocked && strict {
                        continue;
                    }
                    let reuse = if blocked { REUSE } else { 1.0 };
                    let c = base + net.cost[ei] * reuse + p.map_or(0.0, |p| turn_cost(p, s) as f32);
                    if c < d[s] && c <= limit {
                        if d[s] == f32::INFINITY {
                            touched.push(s as u32);
                        }
                        d[s] = c;
                        prev[s] = p.map_or(NONE, |p| p as u32);
                        heap.push(Reverse(((c + h(v)).to_bits(), s as u32)));
                    }
                }
            };
            relax(&mut heap, d, prev, touched, None, from);
            let mut goal = None;
            while let Some(Reverse((bits, s))) = heap.pop() {
                let s = s as usize;
                let u = end_of(s);
                if f32::from_bits(bits) > d[s] + h(u) + 1e-2 {
                    continue;
                }
                if u == to {
                    goal = Some(s);
                    break;
                }
                relax(&mut heap, d, prev, touched, Some(s), u);
            }
            let mut s = goal?;
            let mut out = vec![(s / 2, s % 2 == 0)];
            while prev[s] != NONE {
                s = prev[s] as usize;
                out.push((s / 2, s % 2 == 0));
            }
            out.reverse();
            Some(out)
        })
    }
}

impl Plan<'_> {
    fn route(&self, from: usize, to: usize, ok: &[bool], used: &Used, limit: f32) -> Option<Vec<(usize, bool)>> {
        self.net.route(from, to, ok, used, true, limit, Some(&self.th))
    }
}

// ======================================================================
// Plans edited by hand
// ======================================================================

/// The rings for plans edited by hand (those for how you are travelling), and a message for each that
/// could not be routed.
pub fn manual_rings(b: &Built, cfg: &Config, manuals: &[Manual]) -> (Vec<Ring>, Vec<String>) {
    let net = Net::new(b, cfg);
    let (mut rings, mut errors) = (vec![], vec![]);
    for (i, m) in manuals.iter().enumerate().filter(|(_, m)| m.travel == cfg.travel) {
        match net.manual_ring(i, m) {
            Ok(r) => rings.push(r),
            Err(e) => errors.push(format!("{}: {e}", m.name)),
        }
    }
    (rings, errors)
}

impl Net<'_> {
    /// The nodes nearest to each of `points` (lat, lon) that a day can start from, end at or pass through.
    fn snap(&self, points: &[[f64; 2]]) -> Vec<usize> {
        let g = self.g();
        let xy: Vec<Xy> = points.iter().map(|p| self.b.proj.xy(p[0], p[1])).collect();
        let mut best = vec![(f64::INFINITY, usize::MAX); xy.len()];
        for u in (0..g.nodes.len()).filter(|&u| self.csr.degree(u) > 0) {
            for (b, &p) in best.iter_mut().zip(&xy) {
                let d = (g.nodes[u][0] - p[0]).powi(2) + (g.nodes[u][1] - p[1]).powi(2);
                if d < b.0 {
                    *b = (d, u);
                }
            }
        }
        best.into_iter().map(|b| b.1).collect()
    }

    fn manual_ring(&self, index: usize, m: &Manual) -> Result<Ring, String> {
        let g = self.g();
        if m.anchors.len() < 2 {
            return Err("needs at least two anchors".into());
        }
        // Every point to snap, in order: anchors, then each day's routing points.
        let all: Vec<[f64; 2]> = m.anchors.iter().copied().chain(m.vias.iter().flatten().copied()).collect();
        let nodes = self.snap(&all);
        if nodes.contains(&usize::MAX) {
            return Err("no ways here".into());
        }
        let (anchor_nodes, via_nodes) = nodes.split_at(m.anchors.len());
        let mut at = 0;
        let free = vec![true; g.nodes.len()];
        let mut used = Used::default();
        let (mut edges, mut cuts) = (vec![], vec![0.0]);
        let mut via_marks = vec![];
        for (day, w) in anchor_nodes.windows(2).enumerate() {
            let n = m.vias.get(day).map_or(0, Vec::len);
            let vias = &via_nodes[at..at + n];
            at += n;
            via_marks.push(vias.iter().map(|&v| g.nodes[v]).collect());
            let stops: Vec<usize> =
                std::iter::once(w[0]).chain(vias.iter().copied()).chain(std::iter::once(w[1])).collect();
            for pair in stops.windows(2).filter(|p| p[0] != p[1]) {
                let route = self
                    .route(pair[0], pair[1], &free, &used, false, f32::INFINITY, None)
                    .ok_or_else(|| format!("no way between two points of day {}", day + 1))?;
                used.add(g, &route);
                edges.extend(route);
            }
            cuts.push(edges.iter().map(|&(e, _): &(usize, bool)| g.edges[e].len).sum());
        }
        if edges.is_empty() {
            return Err("no route".into());
        }
        let mut ring = make_ring(g, edges, format!("your plan {}", m.name), self.cfg.is_bike());
        ring.cuts = cuts;
        ring.stats.double_back = crate::rings::doubling_back(&ring.pts, &ring.cuts);
        ring.marks = anchor_nodes.iter().map(|&a| g.nodes[a]).collect();
        ring.via_marks = via_marks;
        ring.manual = Some(index);
        ring.days_wanted = m.days() as f64;
        if !m.closed {
            let (a, z) = (ring.pts[0], *ring.pts.last().unwrap());
            ring.stats.chord_m = dist(a, z).max(1.0);
            ring.pts.push(a);
            ring.seg_road.push(false);
            ring.seg_water.push(true);
        }
        Ok(ring)
    }
}

// ======================================================================
// Putting it together
// ======================================================================

impl Plan<'_> {
    /// The plans for this centre, and how much longer than the profile they are (the winding of this map).
    fn rings(&self) -> (Vec<Ring>, Option<f64>) {
        let cfg = self.net.cfg;
        let day = cfg.day_m();
        let days = cfg.days();
        let (r, prof) = self.size(days);
        let anchors = self.anchors(&prof, r, day);
        if anchors.len() < 8 {
            return (vec![], None);
        }
        let ok = self.corridor(&prof);
        let legs = self.legs(&anchors, &ok, day);
        // The days asked for, for the share of the pie that has land.
        let wanted = days * prof.land_perimeter / prof.perimeter;
        let gap = self.gap(&prof);
        let order = if gap.is_some() { Order::Open } else { Order::Closed };
        let n_wanted = {
            let share = if gap.is_some() { prof.land_perimeter } else { prof.perimeter };
            (self.winding * share / self.arc(r)).round().max(2.0) as usize
        };
        // A ring that winds less than assumed, or a map with few anchors, fits fewer days; an open chain may
        // need a day more or fewer to reach the water at both ends.
        let mut tries =
            vec![n_wanted, n_wanted + 1, n_wanted.saturating_sub(1).max(2), n_wanted.saturating_sub(2).max(2)];
        tries.sort_unstable();
        tries.dedup();
        // Chains are routed one day after another, never walking a way again or back along one. Where a
        // chain can't be (an anchor out on a spur, past a bridge with no other way back), that anchor is
        // dropped and the chains are chosen again.
        let mut banned = vec![false; anchors.len()];
        let mut seen: std::collections::HashSet<Vec<usize>> = Default::default();
        let mut rings: Vec<Ring> = vec![];
        let mut doubling: Vec<Ring> = vec![];
        for _ in 0..REPAIR_ROUNDS {
            let picked: Vec<Picked> = match gap {
                None => tries.iter().flat_map(|&n| self.closed(&anchors, &legs, n, day, &banned)).collect(),
                Some((cut, lo, hi)) => {
                    tries.iter().filter_map(|&n| self.open(&anchors, &legs, n, day, cut, lo, hi, &banned)).collect()
                }
            };
            let fresh: Vec<Picked> = picked.into_iter().filter(|p| seen.insert(p.anchors.clone())).collect();
            // The chains are routed in parallel: each is a handful of searches.
            let built: Vec<Result<Ring, (usize, Option<Box<Ring>>)>> =
                fresh.into_par_iter().map(|p| self.build(&anchors, p, order, &ok, r)).collect();
            let mut failed = false;
            for b in built {
                match b {
                    Ok(mut ring) => {
                        ring.days_wanted = wanted;
                        rings.push(ring);
                    }
                    Err((a, ring)) => {
                        banned[a] = true;
                        failed = true;
                        doubling.extend(ring.map(|r| {
                            let mut r = *r;
                            r.days_wanted = wanted;
                            r
                        }));
                    }
                }
            }
            if !failed || rings.len() >= ENOUGH_RINGS {
                break;
            }
        }
        // With nothing better, the chains that still double back a little are better than none.
        if rings.len() < MIN_RINGS {
            doubling.sort_by(|a, b| a.stats.double_back.total_cmp(&b.stats.double_back));
            rings.extend(doubling.into_iter().take(MIN_RINGS - rings.len()));
        }
        // How much longer than the profile the rings are: the median, over rings that go the whole way round.
        let share = if order == Order::Open { prof.land_perimeter } else { prof.perimeter };
        let mut measured: Vec<f64> = rings.iter().map(|r| r.stats.length / share).collect();
        measured.sort_by(|a, b| a.total_cmp(b));
        (rings, measured.get(measured.len() / 2).copied())
    }

    /// The biggest gap in the profile: (bearing of its middle, land from `lo` to `hi` counter-clockwise from there).
    fn gap(&self, prof: &Profile) -> Option<(f64, f64, f64)> {
        let start = (0..BINS).find(|&i| !prof.is_gap(i))?;
        let (mut best, mut run, mut run_start) = ((0usize, 0usize), 0usize, 0usize);
        for k in 1..=BINS {
            let i = (start + k) % BINS;
            if prof.is_gap(i) {
                if run == 0 {
                    run_start = i;
                }
                run += 1;
                if run > best.1 {
                    best = (run_start, run);
                }
            } else {
                run = 0;
            }
        }
        if best.1 < MIN_GAP_BINS {
            return None;
        }
        let mid = -PI + (best.0 as f64 + best.1 as f64 / 2.0) * BIN;
        let gap = best.1 as f64 * BIN;
        Some((mid, gap / 2.0, TAU - gap / 2.0))
    }

    /// Step 9: route each leg, join them, and set the cut points.
    /// Err: the anchor to drop, and the ring if there is one (a chain that still doubles back).
    fn build(
        &self,
        anchors: &[Anchor],
        picked: Picked,
        order: Order,
        ok: &[bool],
        r: f64,
    ) -> Result<Ring, (usize, Option<Box<Ring>>)> {
        let g = self.g();
        let mut edges: Vec<(usize, bool)> = vec![];
        let mut cuts = vec![0.0];
        let mut used = Used::default();
        for (w, &est) in picked.anchors.windows(2).zip(&picked.leg_costs) {
            let (a, b) = (&anchors[w[0]], &anchors[w[1]]);
            // The way found for the leg may cost more once turns and earlier days are charged for, but not
            // without limit: past this it is a different leg.
            let limit = ROUTE_SLACK * est + ROUTE_SLACK_M;
            let path = self.route(a.node, b.node, ok, &used, limit).ok_or((w[1], None))?;
            // A leg that starts or ends by wandering off and coming back to its anchor (a spur) is no good
            // either: the anchor is on the wrong side of something.
            if let Some(end) = tail_at_anchor(g, &path) {
                return Err((if end { w[1] } else { w[0] }, None));
            }
            used.add(g, &path);
            edges.extend(path);
            cuts.push(edges.iter().map(|&(ei, _)| g.edges[ei].len).sum());
        }
        let label = format!(
            "{} days round a centre {:.1} km from the base, R {:.1} km",
            picked.anchors.len() - 1,
            norm(self.c) / 1000.0,
            r / 1000.0
        );
        let mut ring = make_ring(g, edges, label, self.net.cfg.is_bike());
        ring.cuts = cuts;
        ring.marks = picked.anchors.iter().map(|&a| g.nodes[anchors[a].node]).collect();
        // A chain that still runs back alongside itself, even after all that: the anchor nearest where it
        // does is dropped, and the chains chosen again.
        let mut doubles = None;
        let doubled = crate::rings::doubled_at(&ring.pts, &ring.cuts);
        ring.stats.double_back = doubled.len() as f64 * 25.0;
        if ring.stats.double_back > DOUBLE_BACK_REPAIR_M
            && let Some(&first) = doubled.first()
        {
            let k = (0..ring.cuts.len())
                .min_by(|&i, &j| (ring.cuts[i] - first).abs().total_cmp(&(ring.cuts[j] - first).abs()))
                .unwrap_or(0);
            // A ring's last anchor is its first.
            // Kept as a last resort.
            doubles = Some(picked.anchors[k.min(picked.anchors.len() - 1)]);
        }
        if order == Order::Open {
            let (a, z) = (ring.pts[0], *ring.pts.last().unwrap());
            ring.stats.chord_m = dist(a, z);
            ring.pts.push(a);
            ring.seg_road.push(false);
            ring.seg_water.push(true);
            ring.span = (ccw(bearing_from(a, self.c), bearing_from(z, self.c)) / TAU).clamp(0.05, 1.0);
        }
        // The ring must go round the base (or, for a base on the shore, pass close to it).
        let near = ring.pts.iter().map(|&p| norm(p)).fold(f64::INFINITY, f64::min);
        // A closed ring goes round the base; a chain from shore to shore leaves the base on its inside, which
        // is the water side.
        ring.around_base = if order == Order::Open {
            near < NEAR_BASE_CHAIN * r
        } else {
            encloses(&ring.pts, [0.0, 0.0]) || near < NEAR_BASE_SHARE * r
        };
        match doubles {
            Some(anchor) => Err((anchor, Some(Box::new(ring)))),
            None => Ok(ring),
        }
    }
}

/// Whether a leg starts (`false`) or ends (`true`) with a spur: within `TAIL_M` of walking from its anchor,
/// after at least `TAIL_MIN_M`, it is back beside the anchor.
fn tail_at_anchor(g: &Graph, path: &[(usize, bool)]) -> Option<bool> {
    let pts: Vec<Xy> = path
        .iter()
        .flat_map(|&(ei, fwd)| {
            let mut p = g.edges[ei].pts.clone();
            if !fwd {
                p.reverse();
            }
            p
        })
        .collect();
    let back_beside = |pts: &[Xy]| {
        let mut walked = 0.0;
        for w in pts.windows(2) {
            walked += dist(w[0], w[1]);
            if walked > TAIL_M {
                return false;
            }
            if walked >= TAIL_MIN_M && dist(w[1], pts[0]) < TAIL_NEAR_M {
                return true;
            }
        }
        false
    };
    if back_beside(&pts) {
        return Some(false);
    }
    let rev: Vec<Xy> = pts.iter().rev().copied().collect();
    back_beside(&rev).then_some(true)
}

const TAIL_M: f64 = 1500.0;
const TAIL_MIN_M: f64 = 250.0;
const TAIL_NEAR_M: f64 = 80.0;

/// Whether a closed polyline goes round a point (even-odd rule).
fn encloses(pts: &[Xy], p: Xy) -> bool {
    let mut inside = false;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_wrap() {
        assert!((wrap(3.0 * PI) - PI).abs() < 1e-9);
        assert!((wrap(-PI - 0.1) - (PI - 0.1)).abs() < 1e-9);
        assert!((ccw(0.1, -0.1) - (TAU - 0.2)).abs() < 1e-9);
        assert_eq!(bin_of(-PI), 0);
        assert_eq!(bin_of(PI), BINS - 1);
    }

    #[test]
    fn a_ring_goes_round_a_point() {
        let square = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0], [-1.0, -1.0]];
        assert!(encloses(&square, [0.0, 0.0]));
        assert!(!encloses(&square, [2.0, 0.0]));
    }

    /// Anchors 0..4 in a row, legs between neighbours and skipping one: two legs from 0 reach 2 or 4 by
    /// the cheapest way, and a chain needs exactly the number of legs asked for.
    #[test]
    fn the_chain_has_exactly_n_legs() {
        let leg = |to: u32, mult: f32| Leg { to, mult, len: 1000.0, up: 0.0 };
        let legs = vec![
            vec![leg(1, 1.0), leg(2, 1.0)],
            vec![leg(2, 1.0), leg(3, 1.0)],
            vec![leg(3, 1.0), leg(4, 1.0)],
            vec![leg(4, 1.0)],
            vec![],
        ];
        let cost = |_: usize, l: &Leg, _: usize| Some(l.mult as f64);
        let forward = |a: usize, l: &Leg, _: bool| l.to as usize > a;
        let two = run_dp(5, &legs, 2, &[(0, 0.0)], &cost, &forward, |i| (i == 4).then_some(0.0)).unwrap();
        assert_eq!(two.anchors, vec![0, 2, 4]);
        let four = run_dp(5, &legs, 4, &[(0, 0.0)], &cost, &forward, |i| (i == 4).then_some(0.0)).unwrap();
        assert_eq!(four.anchors, vec![0, 1, 2, 3, 4]);
        assert!(run_dp(5, &legs, 5, &[(0, 0.0)], &cost, &forward, |i| (i == 4).then_some(0.0)).is_none());
    }
}
