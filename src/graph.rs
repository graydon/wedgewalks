//! The walk graph: nodes at way junctions and ends, edges are way pieces
//! between them. Coordinates are projected to local metres around the city
//! center (equirectangular; fine within a few tens of km).

use crate::osm::{Kind, OsmData, WAY_TYPES};
use rayon::prelude::*;
use rstar::RTree;
use rstar::primitives::Line;
use rustc_hash::FxHashMap as HashMap;

pub type Xy = [f64; 2];

const EARTH_R: f64 = 6_371_000.0;

/// Metres per degree of latitude (and of longitude at the equator).
pub const M_PER_DEG: f64 = EARTH_R * std::f64::consts::PI / 180.0;

#[derive(Clone, Copy)]
pub struct Proj {
    lat0: f64,
    lon0: f64,
    kx: f64,
}

impl Proj {
    pub fn new(lat0: f64, lon0: f64) -> Proj {
        Proj { lat0, lon0, kx: lat0.to_radians().cos() }
    }
    pub fn xy(&self, lat: f64, lon: f64) -> Xy {
        [(lon - self.lon0).to_radians() * self.kx * EARTH_R, (lat - self.lat0).to_radians() * EARTH_R]
    }
    /// Back to (lon, lat), GeoJSON order.
    pub fn lonlat(&self, p: Xy) -> (f64, f64) {
        (self.lon0 + (p[0] / (self.kx * EARTH_R)).to_degrees(), self.lat0 + (p[1] / EARTH_R).to_degrees())
    }
}

pub fn dist(a: Xy, b: Xy) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

pub fn norm(a: Xy) -> f64 {
    a[0].hypot(a[1])
}

/// The point a fraction `t` of the way from `a` to `b`.
pub fn lerp(a: Xy, b: Xy, t: f64) -> Xy {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Projected line segments of OSM node lists, keeping those starting
/// within `radius_m` of the base.
pub fn project_lines<'a>(
    osm: &OsmData,
    proj: &Proj,
    lists: impl Iterator<Item = &'a Vec<i64>>,
    radius_m: f64,
) -> Vec<Line<Xy>> {
    lists
        .flat_map(|refs| {
            let pts: Vec<Xy> =
                refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect();
            pts.windows(2).filter(|w| norm(w[0]) <= radius_m).map(|w| Line::new(w[0], w[1])).collect::<Vec<_>>()
        })
        .collect()
}

/// How far apart `near_fraction` samples a line.
const NEAR_SAMPLE_M: f64 = 10.0;

/// Fraction of a polyline's length (sampled every `NEAR_SAMPLE_M`) within
/// `d` of any segment in `tree`.
pub fn near_fraction(tree: &RTree<Line<Xy>>, pts: &[Xy], d: f64) -> f64 {
    // Most lines are nowhere near: rule them out with one box query.
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in pts {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let env = rstar::AABB::from_corners([lo[0] - d, lo[1] - d], [hi[0] + d, hi[1] + d]);
    if tree.locate_in_envelope_intersecting(&env).next().is_none() {
        return 0.0;
    }
    let (mut near, mut total) = (0usize, 0usize);
    for w in pts.windows(2) {
        let n = (dist(w[0], w[1]) / NEAR_SAMPLE_M).ceil().max(1.0) as usize;
        for k in 0..n {
            total += 1;
            if tree.locate_within_distance(lerp(w[0], w[1], (k as f64 + 0.5) / n as f64), d * d).next().is_some() {
                near += 1;
            }
        }
    }
    if total == 0 { 0.0 } else { near as f64 / total as f64 }
}

pub struct Edge {
    pub a: usize,
    pub b: usize,
    /// Geometry from a to b, including both ends.
    pub pts: Vec<Xy>,
    pub len: f64,
    pub kind: Kind,
    pub on_route: bool,
    /// Who can use it: on foot, by bicycle.
    pub foot: bool,
    pub bike: bool,
    /// Built for bicycles (a cycleway, a cycle lane or track), and on a signed bicycle route.
    pub cycle: bool,
    pub bike_route: bool,
    pub steps: bool,
    pub way_id: i64,
    /// Index into `Graph::ways`.
    pub way: u32,
    /// A car-free edge running alongside a busy road for most of its
    /// length (e.g. a separated cycle track beside an avenue).
    pub beside: bool,
    /// A quiet road: residential, living street or service road.
    pub calm: bool,
    /// Beside water (river, canal, lake, sea) for most of its length.
    pub waterside: bool,
    /// Within `SHORE_BAND_M` of a real shore (sea, lake, river) for most of
    /// its length: shore ways are where chains end, and may stray from the
    /// ring's radius.
    pub shore: bool,
    /// Height gained from a to b (m; negative going down, 0 without
    /// elevation data).
    pub rise: f32,
}

/// Ways this close to a real shore count as following it.
pub const SHORE_BAND_M: f64 = 200.0;

/// Options for `Graph::build`.
pub struct BuildOpts {
    pub radius_m: f64,
    pub allow_steps: bool,
    pub beside_m: f64,
    pub waterside_m: f64,
}

impl From<&crate::config::Config> for BuildOpts {
    fn from(c: &crate::config::Config) -> BuildOpts {
        BuildOpts {
            radius_m: c.extract_radius_km * 1000.0,
            allow_steps: c.allow_steps,
            beside_m: c.beside_traffic_m,
            waterside_m: c.waterside_m,
        }
    }
}

/// Descriptive details of a way, shared by its edges.
pub struct WayInfo {
    pub name: Option<String>,
    /// Index into `osm::WAY_TYPES`.
    pub wtype: u8,
    /// Walking or cycling route the way is part of, if any.
    pub route: Option<String>,
    pub bridge: bool,
}

pub struct Graph {
    pub nodes: Vec<Xy>,
    pub edges: Vec<Edge>,
    pub ways: Vec<WayInfo>,
}

/// Compressed adjacency over a subset of the graph's edges: for each node,
/// its (neighbour, edge) pairs. Self-loops are left out.
pub struct Csr {
    off: Vec<u32>,
    adj: Vec<(u32, u32)>,
}

impl Csr {
    pub fn new(g: &Graph, keep: impl Fn(usize, &Edge) -> bool) -> Csr {
        let n = g.nodes.len();
        let kept = |(ei, e): &(usize, &Edge)| e.a != e.b && keep(*ei, e);
        let mut off = vec![0u32; n + 1];
        for (_, e) in g.edges.iter().enumerate().filter(kept) {
            off[e.a + 1] += 1;
            off[e.b + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let mut fill = off.clone();
        let mut adj = vec![(0u32, 0u32); off[n] as usize];
        for (ei, e) in g.edges.iter().enumerate().filter(kept) {
            adj[fill[e.a] as usize] = (e.b as u32, ei as u32);
            fill[e.a] += 1;
            adj[fill[e.b] as usize] = (e.a as u32, ei as u32);
            fill[e.b] += 1;
        }
        Csr { off, adj }
    }

    pub fn nbrs(&self, u: usize) -> &[(u32, u32)] {
        &self.adj[self.off[u] as usize..self.off[u + 1] as usize]
    }

    pub fn degree(&self, u: usize) -> u32 {
        self.off[u + 1] - self.off[u]
    }

    pub fn node_count(&self) -> usize {
        self.off.len() - 1
    }
}

/// One way clipped to the extract: (index into `osm.ways`, node ids and
/// positions).
type Run = (usize, Vec<(i64, Xy)>);

impl Graph {
    pub fn build(osm: &OsmData, proj: &Proj, opts: &BuildOpts) -> Graph {
        let runs = clip_ways(osm, proj, opts);
        let mut g = split_runs(osm, &runs);
        g.renumber_spatially();
        g.mark_beside_traffic(osm, proj, opts);
        g.mark_water(osm, proj, opts);
        g
    }

    /// Sets each edge's rise from the ground elevation at its ends.
    pub fn set_elevation(&mut self, dem: &crate::elevation::Dem, proj: &Proj) {
        let z: Vec<f32> = self
            .nodes
            .iter()
            .map(|&p| {
                let (lon, lat) = proj.lonlat(p);
                dem.at(lat, lon).unwrap_or(0.0)
            })
            .collect();
        for e in &mut self.edges {
            e.rise = z[e.b] - z[e.a];
        }
    }

    pub fn other(&self, ei: usize, n: usize) -> usize {
        let e = &self.edges[ei];
        if e.a == n { e.b } else { e.a }
    }

    /// Renumber nodes in Morton (Z-curve) order of 64 m cells so that
    /// nearby nodes have nearby indices: searches touch far fewer cache
    /// lines.
    fn renumber_spatially(&mut self) {
        let morton = |p: Xy| {
            let q = |v: f64| (((v / 64.0) as i64 + 32768).clamp(0, 65535)) as u64;
            let spread = |mut x: u64| {
                x = (x | (x << 8)) & 0x00ff_00ff;
                x = (x | (x << 4)) & 0x0f0f_0f0f;
                x = (x | (x << 2)) & 0x3333_3333;
                (x | (x << 1)) & 0x5555_5555
            };
            spread(q(p[0])) | (spread(q(p[1])) << 1)
        };
        let mut order: Vec<usize> = (0..self.nodes.len()).collect();
        order.sort_by_cached_key(|&i| morton(self.nodes[i]));
        let mut rank = vec![0; self.nodes.len()];
        for (new, &old) in order.iter().enumerate() {
            rank[old] = new;
        }
        self.nodes = order.iter().map(|&i| self.nodes[i]).collect();
        for e in &mut self.edges {
            e.a = rank[e.a];
            e.b = rank[e.b];
        }
    }

    /// Flags car-free edges that run within `beside_m` of a busy road for at
    /// least half their length.
    fn mark_beside_traffic(&mut self, osm: &OsmData, proj: &Proj, opts: &BuildOpts) {
        if opts.beside_m <= 0.0 {
            return;
        }
        let tree = RTree::bulk_load(project_lines(osm, proj, osm.traffic.iter(), opts.radius_m + 1000.0));
        let d = opts.beside_m;
        self.edges
            .par_iter_mut()
            .filter(|e| e.kind == Kind::CarFree)
            .for_each(|e| e.beside = near_fraction(&tree, &e.pts, d) >= 0.5);
    }

    /// Waterside and shore flags.
    fn mark_water(&mut self, osm: &OsmData, proj: &Proj, opts: &BuildOpts) {
        let water = RTree::bulk_load(crate::water::segments(osm, proj, opts.radius_m, false));
        let shores = RTree::bulk_load(crate::water::segments(osm, proj, opts.radius_m, true));
        let d = opts.waterside_m;
        self.edges.par_iter_mut().for_each(|e| {
            e.waterside = d > 0.0 && near_fraction(&water, &e.pts, d) >= 0.5;
            e.shore = near_fraction(&shores, &e.pts, SHORE_BAND_M) >= 0.5;
        });
    }
}

impl Edge {
    /// A plain car-free edge; callers override what differs.
    pub(crate) fn new(a: usize, b: usize, len: f64, way: u32) -> Edge {
        Edge {
            a,
            b,
            pts: vec![],
            len,
            kind: Kind::CarFree,
            on_route: false,
            foot: true,
            bike: true,
            cycle: false,
            bike_route: false,
            steps: false,
            way_id: 0,
            way,
            beside: false,
            calm: false,
            waterside: false,
            shore: false,
            rise: 0.0,
        }
    }

    /// Which `config::WAY_CLASSES` entry the edge belongs to.
    pub fn class(&self) -> usize {
        match (self.kind, self.beside, self.calm) {
            (Kind::CarFree, false, _) => 0,
            (Kind::CarFree, true, _) => 1,
            (Kind::Road, _, true) => 2,
            (Kind::Road, _, false) => 3,
        }
    }

    /// Height gained walking it forward (true: a to b) or back.
    pub fn rise(&self, forward: bool) -> f64 {
        if forward { self.rise as f64 } else { -self.rise as f64 }
    }

    /// Metres climbed walking it forward or back.
    pub fn climb(&self, forward: bool) -> f64 {
        self.rise(forward).max(0.0)
    }
}

/// Clip each way to the extract radius: split it into runs of nodes that
/// have coordinates and lie inside.
fn clip_ways(osm: &OsmData, proj: &Proj, opts: &BuildOpts) -> Vec<Run> {
    let mut runs: Vec<Run> = vec![];
    for (wi, w) in osm.ways.iter().enumerate() {
        if w.steps && !opts.allow_steps {
            continue;
        }
        let mut run = vec![];
        for id in &w.refs {
            match osm.coords.get(id).map(|&(la, lo)| proj.xy(la, lo)) {
                Some(p) if norm(p) <= opts.radius_m => run.push((*id, p)),
                _ => {
                    if run.len() >= 2 {
                        runs.push((wi, std::mem::take(&mut run)));
                    }
                    run.clear();
                }
            }
        }
        if run.len() >= 2 {
            runs.push((wi, run));
        }
    }
    runs
}

/// Edges are cut at a vertex once they are this long (m).
const MAX_EDGE_M: f64 = 500.0;

/// Cuts runs into edges at junctions (nodes shared by runs, or run ends).
fn split_runs(osm: &OsmData, runs: &[Run]) -> Graph {
    let mut uses: HashMap<i64, u32> = HashMap::default();
    for (_, run) in runs {
        for (i, (id, _)) in run.iter().enumerate() {
            let bump = if i == 0 || i == run.len() - 1 { 2 } else { 1 };
            *uses.entry(*id).or_default() += bump;
        }
    }
    let mut index: HashMap<i64, usize> = HashMap::default();
    let mut g = Graph { nodes: vec![], edges: vec![], ways: vec![] };
    let mut node_of = |id: i64, p: Xy, nodes: &mut Vec<Xy>| {
        *index.entry(id).or_insert_with(|| {
            nodes.push(p);
            nodes.len() - 1
        })
    };
    for (wi, run) in runs {
        let w = &osm.ways[*wi];
        let route = osm.route_ways.get(&w.id);
        let calm = matches!(WAY_TYPES[w.wtype as usize], "residential street" | "living street" | "service road");
        let mut way = None;
        // Cut at junctions; a loop that comes back to the same node (a path
        // round a lake touching the network once) is also cut at its
        // middle, so it becomes two edges rather than a self-loop the ring
        // search can't use.
        let mut cut: Vec<bool> = run.iter().map(|(id, _)| uses[id] >= 2).collect();
        let mut prev = 0;
        for i in 1..run.len() {
            if cut[i] {
                if run[i].0 == run[prev].0 && i - prev >= 2 {
                    cut[(prev + i) / 2] = true;
                }
                prev = i;
            }
        }
        // Long stretches without a junction are cut too, so a day can start or end part-way along a
        // path or a long road.
        let mut since = 0.0;
        for i in 1..run.len() {
            since += dist(run[i - 1].1, run[i].1);
            if cut[i] {
                since = 0.0;
            } else if since >= MAX_EDGE_M && i + 1 < run.len() {
                cut[i] = true;
                since = 0.0;
            }
        }
        let mut start = 0;
        for i in (1..run.len()).filter(|&i| cut[i]) {
            let pts: Vec<Xy> = run[start..=i].iter().map(|(_, p)| *p).collect();
            let len = pts.windows(2).map(|s| dist(s[0], s[1])).sum();
            let a = node_of(run[start].0, run[start].1, &mut g.nodes);
            let b = node_of(run[i].0, run[i].1, &mut g.nodes);
            start = i;
            if a == b && len == 0.0 {
                continue;
            }
            let way = *way.get_or_insert_with(|| {
                let route = route.filter(|n| !n.is_empty()).cloned();
                g.ways.push(WayInfo { name: w.name.clone(), wtype: w.wtype, route, bridge: w.bridge });
                g.ways.len() as u32 - 1
            });
            g.edges.push(Edge {
                pts,
                kind: w.kind,
                on_route: route.is_some(),
                foot: w.foot,
                bike: w.bike,
                cycle: w.cycle,
                bike_route: osm.bike_route_ways.contains(&w.id),
                steps: w.steps,
                way_id: w.id,
                calm,
                ..Edge::new(a, b, len, way)
            });
        }
    }
    g
}
