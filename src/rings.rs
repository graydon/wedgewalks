//! A plan as a ring: the walk (edges, points), how good it is, and where
//! the days end. Closed, or open and closed by an unwalked chord across the
//! water (`seg_water`), which is how the arc splitter sees a shore-to-shore
//! chain.

use crate::config::Config;
use crate::graph::{Graph, Xy, dist, lerp, norm};
use crate::osm::Kind;
use std::collections::HashSet;

#[derive(Clone)]
pub struct Ring {
    /// Edges in order, with traversal direction (true = a->b).
    pub edges: Vec<(usize, bool)>,
    pub pts: Vec<Xy>,
    /// Road flag of each segment pts[i]..pts[i+1].
    pub seg_road: Vec<bool>,
    /// Flag of the closing chord (open chains), which is not walked.
    pub seg_water: Vec<bool>,
    /// Length and rise (m) of each edge, in the order walked.
    pub profile: Vec<(f64, f64)>,
    pub stats: Stats,
    pub problems: Vec<String>,
    /// How it was found, for diagnostics.
    pub search: String,
    /// Share of a full circle round the centre it covers: 1 for a closed
    /// ring, less for a chain from shore to shore.
    pub span: f64,
    /// Where the days end, as distances along the walked ring (first 0,
    /// last the walked length).
    pub cuts: Vec<f64>,
    /// The days asked for, for the part of the circle that has land.
    pub days_wanted: f64,
    /// Whether it goes round the base (or, for a base on the shore, passes close to it).
    pub around_base: bool,
    /// Where each day starts and ends (the anchors), and for a manual plan the routing points of each
    /// day. Positions in the graph's coordinates.
    pub marks: Vec<Xy>,
    pub via_marks: Vec<Vec<Xy>>,
    /// For a plan edited by hand, which one (an index into the city's manual plans).
    pub manual: Option<usize>,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    /// Walked length.
    pub length: f64,
    /// Length of the unwalked chord closing an open chain (0 if closed).
    pub chord_m: f64,
    /// Share of the walked length beside water.
    pub waterside_frac: f64,
    /// Length of ring that runs back alongside another part of itself
    /// (out along a spit and back): metres.
    pub double_back: f64,
    pub road_len: f64,
    pub road_frac: f64,
    pub max_gap: f64,
    /// Midpoint of the longest road gap.
    pub gap_at: Xy,
    pub r_min: f64,
    pub r_max: f64,
    pub r_mean: f64,
    pub route_frac: f64,
    /// Share of the length on car-free paths beside busy roads.
    pub beside_frac: f64,
    /// Number of flights of steps (step edges).
    pub steps: usize,
    /// Metres climbed going round (as much as descended).
    pub climb: f64,
    pub score: f64,
    /// Filled in after the search (see shade.rs).
    pub shade_frac: f64,
    /// Sharp turns (over 45 degrees, between ways) per km.
    pub turns_per_km: f64,
    /// Length-weighted street life, 0..1 (see feel.rs); filled in with shade.
    pub life_frac: f64,
}

impl Ring {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }

    /// Whether it is an open chain (closed by a chord).
    pub fn is_open(&self) -> bool {
        self.stats.chord_m > 0.0
    }
}

/// `bike`: by bicycle, quiet streets and cycle lanes are not "road" (the traffic the limits are about).
pub(crate) fn make_ring(g: &Graph, edges: Vec<(usize, bool)>, search: String, bike: bool) -> Ring {
    let mut pts: Vec<Xy> = vec![];
    let mut seg_road = vec![];
    let (mut length, mut road_len, mut route_len, mut beside_len, mut waterside_len) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(ei, fwd) in &edges {
        let e = &g.edges[ei];
        let mut ep = e.pts.clone();
        if !fwd {
            ep.reverse();
        }
        if pts.is_empty() {
            pts.push(ep[0]);
        }
        pts.extend_from_slice(&ep[1..]);
        let road = e.kind == Kind::Road && !(bike && (e.calm || e.cycle));
        seg_road.extend(std::iter::repeat_n(road, ep.len() - 1));
        length += e.len;
        if e.waterside {
            waterside_len += e.len;
        }
        if road {
            road_len += e.len;
        }
        if e.on_route {
            route_len += e.len;
        }
        if e.beside && e.kind == Kind::CarFree {
            beside_len += e.len;
        }
    }
    let seg_water = vec![false; seg_road.len()];
    let seg_len: Vec<f64> = pts.windows(2).map(|s| dist(s[0], s[1])).collect();
    let (max_gap, gap_at) = longest_road_gap(&pts, &seg_road, &seg_len);
    let (r_min, r_max, r_mean) = radius_stats(&pts, &seg_len);
    let stats = Stats {
        double_back: doubling_back(&pts, &[]),
        length,
        waterside_frac: waterside_len / length.max(1.0),
        road_len,
        road_frac: road_len / length.max(1.0),
        max_gap,
        gap_at,
        r_min,
        r_max,
        r_mean,
        route_frac: route_len / length.max(1.0),
        beside_frac: beside_len / length.max(1.0),
        steps: edges.iter().filter(|e| g.edges[e.0].steps).count(),
        climb: edges.iter().map(|&(ei, fwd)| g.edges[ei].climb(fwd)).sum(),
        turns_per_km: sharp_turns(g, &edges) as f64 / (length / 1000.0).max(0.1),
        ..Stats::default()
    };
    let profile = edges.iter().map(|&(ei, fwd)| (g.edges[ei].len, g.edges[ei].rise(fwd))).collect();
    Ring {
        edges,
        pts,
        seg_road,
        seg_water,
        profile,
        stats,
        problems: vec![],
        search,
        span: 1.0,
        cuts: vec![],
        days_wanted: 0.0,
        around_base: true,
        marks: vec![],
        via_marks: vec![],
        manual: None,
    }
}

/// Turns of more than 45 degrees where one edge of a ring meets the next.
fn sharp_turns(g: &Graph, edges: &[(usize, bool)]) -> usize {
    let heading = |&(ei, fwd): &(usize, bool), leaving: bool| {
        let p = &g.edges[ei].pts;
        let n = p.len();
        // Direction of travel at the start (leaving) or end of the edge.
        let (a, b) = match (fwd, leaving) {
            (true, true) => (p[0], p[1]),
            (true, false) => (p[n - 2], p[n - 1]),
            (false, true) => (p[n - 1], p[n - 2]),
            (false, false) => (p[1], p[0]),
        };
        (b[1] - a[1]).atan2(b[0] - a[0])
    };
    edges
        .windows(2)
        .filter(|w| {
            let d = (heading(&w[1], true) - heading(&w[0], false)).rem_euclid(std::f64::consts::TAU);
            d.min(std::f64::consts::TAU - d) > std::f64::consts::FRAC_PI_4
        })
        .count()
}

/// The longest run of consecutive road segments (wrapping around the ring)
/// and the point in its middle.
fn longest_road_gap(pts: &[Xy], seg_road: &[bool], seg_len: &[f64]) -> (f64, Xy) {
    let Some(start) = seg_road.iter().position(|r| !r) else {
        return (seg_len.iter().sum(), pts[0]); // all road
    };
    let n = seg_road.len();
    let (mut best, mut at) = (0.0, pts[0]);
    let (mut run, mut run_start) = (0.0, 0);
    for k in 0..n {
        let i = (start + k) % n;
        if !seg_road[i] {
            run = 0.0;
            continue;
        }
        if run == 0.0 {
            run_start = k;
        }
        run += seg_len[i];
        if run > best {
            best = run;
            at = pts[(start + (run_start + k) / 2) % n];
        }
    }
    (best, at)
}

/// Radius statistics of a ring about the base: (min, max, length-weighted mean).
fn radius_stats(pts: &[Xy], seg_len: &[f64]) -> (f64, f64, f64) {
    let (mut sum, mut lo, mut hi) = (0.0, f64::INFINITY, 0.0f64);
    for (s, l) in pts.windows(2).zip(seg_len) {
        let r = norm(lerp(s[0], s[1], 0.5));
        sum += r * l;
        lo = lo.min(norm(s[0]));
        hi = hi.max(norm(s[0]));
    }
    let total = seg_len.iter().sum::<f64>().max(1.0);
    (lo, hi, sum / total)
}

/// How much of a ring runs back alongside itself: samples every 50 m that
/// have another sample within 60 m which is more than 600 m away along the
/// ring. Half the doubled length (each stretch is seen from both sides).
/// Within `CUT_MARGIN_M` of a cut point (where one day ends and the next
/// begins, perhaps on the other side of the street) it doesn't count.
pub(crate) fn doubling_back(pts: &[Xy], cuts: &[f64]) -> f64 {
    doubled_at(pts, cuts).len() as f64 * DOUBLING_STEP_M / 2.0
}

const DOUBLING_STEP_M: f64 = 50.0;

/// Where along the ring (m) the samples are that run back alongside another part of it.
pub(crate) fn doubled_at(pts: &[Xy], cuts: &[f64]) -> Vec<f64> {
    use rstar::RTree;
    use rstar::primitives::GeomWithData;
    const STEP: f64 = DOUBLING_STEP_M;
    let mut samples: Vec<GeomWithData<Xy, f64>> = vec![];
    let mut along = 0.0;
    let mut next = 0.0;
    for w in pts.windows(2) {
        let len = dist(w[0], w[1]);
        while next <= along + len {
            let t = if len > 0.0 { (next - along) / len } else { 0.0 };
            samples.push(GeomWithData::new(lerp(w[0], w[1], t), next));
            next += STEP;
        }
        along += len;
    }
    let total = along;
    let near_cut = |p: f64| cuts.iter().any(|&c| (p - c).abs() < CUT_MARGIN_M);
    let tree = RTree::bulk_load(samples.clone());
    samples
        .iter()
        .filter(|s| !near_cut(s.data))
        .filter(|s| {
            tree.locate_within_distance(*s.geom(), 60.0 * 60.0).any(|o| {
                let d = (o.data - s.data).abs();
                d.min(total - d) > 600.0 && !near_cut(o.data)
            })
        })
        .map(|s| s.data)
        .collect()
}

/// Score per share of the wanted number of days a ring is off by, and the
/// share (and one day) beyond which it is outside the limits.
const DAYS_WEIGHT: f64 = 2.0;
const DAYS_TOLERANCE: f64 = 0.25;
/// Score per sharp turn per km, and per unit of street life.
const TURN_WEIGHT: f64 = 0.04;
const LIFE_WEIGHT: f64 = 0.15;

/// Days meeting on a street, or on the pavements either side of it, aren't doubling back.
const CUT_MARGIN_M: f64 = 100.0;

/// Scores a ring (lower is better) and checks it against the limits in `cfg`.
fn evaluate(r: &mut Ring, cfg: &Config) {
    let s = &mut r.stats;
    let water_pref = if cfg.prefer_waterside { 0.2 * s.waterside_frac } else { 0.0 };
    s.score = s.road_frac * 2.0 + s.beside_frac + 0.5 * s.max_gap / cfg.max_road_gap_m - 0.2 * s.route_frac
        + 2.0 * s.double_back / s.length.max(1.0)
        + cfg.climb_cost_m * s.climb / s.length.max(1.0)
        + TURN_WEIGHT * s.turns_per_km
        - LIFE_WEIGHT * s.life_frac
        - water_pref;
    // The days are the planned legs: a plan that makes fewer or more than asked for says so.
    let days = r.cuts.len().saturating_sub(1) as f64;
    let want = r.days_wanted.max(1.0);
    let off = (days - want).abs();
    if r.manual.is_none() {
        s.score += DAYS_WEIGHT * off / want;
    }
    r.problems.clear();
    if r.manual.is_none() && off > 1.0 && off > DAYS_TOLERANCE * want {
        r.problems.push(format!("{days:.0} days (wanted {want:.0})"));
    }
    if !r.around_base {
        r.problems.push("doesn't go round the base".into());
    }
    if s.road_frac > cfg.max_road_fraction {
        r.problems.push(format!("road fraction {:.0}%", s.road_frac * 100.0));
    }
    if s.max_gap > cfg.max_road_gap_m {
        r.problems.push(format!("road gap {:.0} m", s.max_gap));
    }
    if s.double_back > cfg.max_double_back_km * 1000.0 {
        r.problems.push(format!("doubles back {:.1} km", s.double_back / 1000.0));
    }
}

/// Rings sharing more than this share of their length with a better ring
/// are dropped as duplicates.
const DUPLICATE_OVERLAP: f64 = 0.5;

/// Scores and checks candidates, drops near-duplicates, best first (valid
/// rings before invalid).
pub fn select(g: &Graph, raw: &[Ring], cfg: &Config) -> Vec<Ring> {
    let mut candidates: Vec<Ring> = raw.to_vec();
    for r in &mut candidates {
        evaluate(r, cfg);
    }
    candidates.sort_by(|x, y| y.ok().cmp(&x.ok()).then(x.stats.score.total_cmp(&y.stats.score)));
    // Plans edited by hand come first, in the order they were made, and are never merged with others.
    let (mut kept, rest): (Vec<Ring>, Vec<Ring>) = candidates.into_iter().partition(|r| r.manual.is_some());
    kept.sort_by_key(|r| r.manual);
    let mut kept_sets: Vec<HashSet<usize>> = kept.iter().map(|r| r.edges.iter().map(|e| e.0).collect()).collect();
    let n_manual = kept.len();
    for r in rest {
        let set: HashSet<usize> = r.edges.iter().map(|e| e.0).collect();
        let dup = kept.iter().zip(&kept_sets).skip(n_manual).any(|(k, ks)| {
            let shared: f64 = set.intersection(ks).map(|&ei| g.edges[ei].len).sum();
            shared > DUPLICATE_OVERLAP * r.stats.length.min(k.stats.length)
        });
        if !dup {
            kept.push(r);
            kept_sets.push(set);
        }
    }
    kept
}
