//! Splitting a ring into arcs.

use crate::config::Config;
use crate::graph::{Graph, Xy, dist, lerp};
use crate::osm::WAY_TYPES;
use crate::places::Places;
use crate::rings::Ring;
use crate::trailheads::{RingMarks, Trailhead};
use crate::transit::{Access, Transit};

#[derive(Clone)]
pub struct Arc {
    pub pts: Vec<Xy>,
    pub length: f64,
    pub road_len: f64,
    /// Filled in by the caller (see shade.rs).
    pub shade: f64,
    pub start: Option<Access>,
    pub end: Option<Access>,
    /// Walking mode: the walk from the base to the arc's start, and from
    /// its end back to the base.
    pub walk_out: Option<Approach>,
    pub walk_back: Option<Approach>,
    /// Where the arc starts, as distance along the ring.
    pub from: f64,
    /// The walk, as named or typed stretches. Filled in by the caller.
    pub walk: Vec<Stretch>,
    /// The trailheads it starts and ends at, if any.
    pub start_at: Option<Trailhead>,
    pub end_at: Option<Trailhead>,
    /// Metres climbed and descended along the arc.
    pub up: f64,
    pub down: f64,
}

/// A walk between the base and an arc end (walking mode).
#[derive(Clone, Debug)]
pub struct Approach {
    pub metres: f64,
    /// From the base to the arc end (for the walk back, reverse it).
    pub pts: Vec<Xy>,
    /// The ring node walked to.
    pub node: usize,
    /// The walk from the base, as named stretches (filled in by the caller).
    pub walk: Vec<Stretch>,
    /// Metres climbed and descended on this walk, in the direction walked.
    pub up: f64,
    pub down: f64,
}

impl Arc {
    /// Where it starts and ends, when those are trailheads: "starts at
    /// Union station; ends where it leaves Lower Don Trail".
    pub fn ends_text(&self) -> Option<String> {
        let at = |h: &Trailhead| {
            if h.label.starts_with("where ") { h.label.clone() } else { format!("at {}", h.label) }
        };
        let parts: Vec<String> = [("starts", &self.start_at), ("ends", &self.end_at)]
            .into_iter()
            .filter_map(|(verb, h)| h.as_ref().map(|h| format!("{verb} {}", at(h))))
            .collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// Whether the outing can be done as planned: transit both ways, or
    /// in walking mode, walks both ways within the day limit.
    pub fn reachable(&self, cfg: &Config) -> bool {
        match (&self.walk_out, &self.walk_back) {
            (Some(o), Some(b)) => o.metres + self.length + b.metres <= cfg.day_m() + ROUNDING_M,
            _ => self.start.is_some() && self.end.is_some(),
        }
    }

    /// Metres climbed in the day: the arc, plus the walks out and home in
    /// walking mode.
    pub fn day_climb(&self) -> f64 {
        self.up + self.walk_out.as_ref().map_or(0.0, |w| w.up) + self.walk_back.as_ref().map_or(0.0, |w| w.up)
    }

    /// Whether the day climbs more than the limit.
    pub fn too_steep(&self, cfg: &Config) -> bool {
        self.day_climb() > cfg.max_climb_per_day_m + ROUNDING_M
    }

    /// Total walking for the day in walking mode.
    pub fn day_m(&self) -> Option<f64> {
        Some(self.walk_out.as_ref()?.metres + self.length + self.walk_back.as_ref()?.metres)
    }

    /// Walking mode: "walk 1.0 km from the base to the ring, 4.3 km along
    /// it, 1.4 km home (6.6 km)".
    pub fn day_summary(&self) -> Option<String> {
        let (out, back) = (self.walk_out.as_ref()?, self.walk_back.as_ref()?);
        Some(format!(
            "walk {:.1} km from the base to the ring, {:.1} km along it, {:.1} km home ({:.1} km)",
            out.metres / 1000.0,
            self.length / 1000.0,
            back.metres / 1000.0,
            self.day_m()? / 1000.0
        ))
    }
}

/// How arc ends connect to the base.
pub enum Ends<'a> {
    Transit(&'a Transit),
    Walking(&'a crate::graph::Graph, &'a crate::base::BaseTree),
}

/// A stretch of a walk along one named way or walking route, or, for
/// unnamed ways, one type of way in the same surroundings.
#[derive(Clone, Debug)]
pub struct Stretch {
    /// The way's (or walking route's) name if `named`; else where it goes
    /// ("beside Queen Street"), possibly empty.
    pub label: String,
    pub named: bool,
    /// Way type with the most length in the stretch (see `osm::WAY_TYPES`).
    pub wtype: &'static str,
    pub road: bool,
    /// Mostly on car-free paths beside busy roads.
    pub beside: bool,
    pub length: f64,
}

impl Stretch {
    pub fn describe(&self) -> String {
        let len = if self.length >= 1000.0 {
            format!("{:.1} km", self.length / 1000.0)
        } else {
            format!("{:.0} m", self.length)
        };
        // "beside traffic" is redundant next to "beside <street>".
        let says_beside = !self.named && self.label.starts_with("beside ");
        let kind =
            if self.beside && !says_beside { format!("{} beside traffic", self.wtype) } else { self.wtype.to_string() };
        match (self.named, self.label.is_empty()) {
            (true, _) => format!("{} ({kind}, {len})", self.label),
            (false, false) => format!("{len} of {kind} {}", self.label),
            (false, true) => format!("{len} of {kind}"),
        }
    }

    /// Whether two stretches describe the same thing (so can be merged).
    fn same_as(&self, o: &Stretch) -> bool {
        self.named == o.named && self.label.eq_ignore_ascii_case(&o.label) && (self.named || self.wtype == o.wtype)
    }
}

/// Tolerance when re-checking the day limit: the arc length and walks are
/// summed in a different order than in `cut_points_walking`.
const ROUNDING_M: f64 = 1.0;

/// Stretches shorter than this (crossings, bits of sidewalk) are folded
/// into their neighbours when describing a walk.
const MIN_STRETCH_M: f64 = 120.0;

/// The ring's edges overlapping the part from `from` running `len` metres
/// forward (wrapping), with how many metres of each.
fn ring_parts(ring: &Ring, g: &Graph, from: f64, len: f64) -> Vec<(usize, f64)> {
    let total: f64 = ring.edges.iter().map(|&(ei, _)| g.edges[ei].len).sum();
    let to = from + len;
    let mut parts = vec![];
    for lap in 0..2 {
        let mut s0 = lap as f64 * total;
        for &(ei, _) in &ring.edges {
            let s1 = s0 + g.edges[ei].len;
            let overlap = s1.min(to) - s0.max(from);
            if overlap > 0.0 {
                parts.push((ei, overlap));
            }
            s0 = s1;
        }
    }
    parts
}

/// Describes a walk over these edges (with metres walked on each, in order)
/// as named stretches.
pub fn stretches(g: &Graph, places: &Places, parts: &[(usize, f64)]) -> Vec<Stretch> {
    /// A stretch being built: metres per way type, of road, and beside traffic.
    struct Acc {
        s: Stretch,
        types: Vec<(u8, f64)>,
        road: f64,
        beside: f64,
    }
    let mut acc: Vec<Acc> = vec![];
    for &(ei, m) in parts {
        let e = &g.edges[ei];
        let w = &g.ways[e.way as usize];
        let own = w.name.clone().or_else(|| w.route.clone());
        let named = own.is_some();
        let label = own.or_else(|| places.describe(&e.pts)).unwrap_or_default();
        let road = e.kind == crate::osm::Kind::Road;
        let beside = e.beside && !road;
        let s = Stretch { label, named, wtype: WAY_TYPES[w.wtype as usize], road, beside, length: 0.0 };
        if acc.last().is_none_or(|l| !l.s.same_as(&s)) {
            acc.push(Acc { s, types: vec![], road: 0.0, beside: 0.0 });
        }
        let l = acc.last_mut().unwrap();
        l.s.length += m;
        match l.types.iter_mut().find(|t| t.0 == w.wtype) {
            Some(t) => t.1 += m,
            None => l.types.push((w.wtype, m)),
        }
        l.road += if road { m } else { 0.0 };
        l.beside += if beside { m } else { 0.0 };
    }
    let mut out: Vec<Stretch> = acc
        .into_iter()
        .map(|Acc { mut s, types, road, beside }| {
            s.wtype = WAY_TYPES[types.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0 as usize];
            s.road = road > s.length / 2.0;
            s.beside = beside > s.length / 2.0;
            s
        })
        .collect();
    fold_short(&mut out);
    out
}

/// Repeatedly folds the shortest stretch under `MIN_STRETCH_M` into its
/// longer neighbour, then merges neighbours that now match.
fn fold_short(out: &mut Vec<Stretch>) {
    while let Some(i) = (0..out.len())
        .filter(|&i| out[i].length < MIN_STRETCH_M && out.len() > 1)
        .min_by(|&a, &b| out[a].length.total_cmp(&out[b].length))
    {
        let j = match (i.checked_sub(1), (i + 1 < out.len()).then_some(i + 1)) {
            (Some(p), Some(n)) => {
                if out[p].length >= out[n].length {
                    p
                } else {
                    n
                }
            }
            (Some(p), None) => p,
            (None, Some(n)) => n,
            (None, None) => break,
        };
        let m = out.remove(i).length;
        out[if j > i { j - 1 } else { j }].length += m;
        let mut k = 1;
        while k < out.len() {
            if out[k].same_as(&out[k - 1]) {
                let s = out.remove(k);
                out[k - 1].length += s.length;
            } else {
                k += 1;
            }
        }
    }
}

/// Describes the part of the ring from `from` running `len` metres forward
/// (wrapping) as named stretches.
pub fn walk_stretches(ring: &Ring, g: &Graph, places: &Places, from: f64, len: f64) -> Vec<Stretch> {
    stretches(g, places, &ring_parts(ring, g, from, len))
}

/// One line describing a walk: the longest stretches in order.
pub fn describe_walk(walk: &[Stretch], max: usize) -> String {
    let mut keep: Vec<usize> = (0..walk.len()).collect();
    keep.sort_by(|&a, &b| walk[b].length.total_cmp(&walk[a].length));
    keep.truncate(max);
    keep.sort();
    // Shown stretches that describe the same thing read as one.
    let mut shown: Vec<Stretch> = vec![];
    for &i in &keep {
        match shown.last_mut().filter(|l| l.same_as(&walk[i])) {
            Some(l) => l.length += walk[i].length,
            None => shown.push(walk[i].clone()),
        }
    }
    let parts: Vec<String> = shown.iter().map(Stretch::describe).collect();
    let rest: f64 = (0..walk.len()).filter(|i| !keep.contains(i)).map(|i| walk[i].length).sum();
    let mut text = parts.join(", then ");
    if rest > 0.0 {
        text.push_str(&format!(", plus {:.0} m of short links", rest));
    }
    text
}

/// Cumulative distance along a polyline.
fn cumulative(pts: &[Xy]) -> Vec<f64> {
    let mut cum = vec![0.0];
    for s in pts.windows(2) {
        cum.push(cum.last().unwrap() + dist(s[0], s[1]));
    }
    cum
}

fn point_at(pts: &[Xy], cum: &[f64], s: f64) -> Xy {
    let i = cum.partition_point(|&c| c <= s).clamp(1, pts.len() - 1);
    let (a, b) = (pts[i - 1], pts[i]);
    let seg = cum[i] - cum[i - 1];
    let t = if seg > 0.0 { ((s - cum[i - 1]) / seg).clamp(0.0, 1.0) } else { 0.0 };
    lerp(a, b, t)
}

/// The piece of the ring from position `from` running `len` metres forward
/// (wrapping), and how much of it is road.
fn slice(ring: &Ring, cum: &[f64], from: f64, len: f64) -> (Vec<Xy>, f64) {
    let total = *cum.last().unwrap();
    let mut out = vec![point_at(&ring.pts, cum, from)];
    let mut road = 0.0;
    let to = from + len;
    // Visit segments over two laps so wrapping is just a longer interval.
    for lap in 0..2 {
        let off = lap as f64 * total;
        for i in 0..ring.seg_road.len() {
            let (s0, s1) = (cum[i] + off, cum[i + 1] + off);
            let (lo, hi) = (s0.max(from), s1.min(to));
            if hi <= lo {
                continue;
            }
            if ring.seg_road[i] {
                road += hi - lo;
            }
            if s1 < to {
                out.push(ring.pts[i + 1]);
            }
        }
    }
    out.push(point_at(&ring.pts, cum, to % total));
    (out, road)
}

/// Cuts the ring into the planned days: one arc between each pair of cut
/// points, with how each end connects to the base.
pub fn split(ring: &Ring, ends: &Ends, marks: &RingMarks, cfg: &Config) -> Vec<Arc> {
    let sp = Splitter::new(ring, ends, marks, cfg);
    let walked = sp.cum.last().copied().unwrap_or(0.0) - ring.stats.chord_m;
    let cuts = if ring.cuts.len() >= 2 { ring.cuts.clone() } else { vec![0.0, walked] };
    let at: Vec<End> = cuts.iter().map(|&s| sp.end_at(s)).collect();
    cuts.windows(2)
        .enumerate()
        .map(|(i, w)| {
            let (from, len) = (w[0], w[1] - w[0]);
            let (pts, road_len) = slice(ring, &sp.cum, from, len);
            let (up, down) = sp.climb(from, len);
            Arc {
                pts,
                length: len,
                road_len,
                shade: 0.0,
                start: at[i].go.clone(),
                end: at[i + 1].back.clone(),
                walk_out: sp.approach(&at[i], from, false),
                walk_back: sp.approach(&at[i + 1], w[1], true),
                from,
                walk: vec![],
                start_at: at[i].head.clone(),
                end_at: at[i + 1].head.clone(),
                up,
                down,
            }
        })
        .collect()
}

struct Splitter<'a> {
    ring: &'a Ring,
    ends: &'a Ends<'a>,
    marks: &'a RingMarks<'a>,
    cfg: &'a Config,
    cum: Vec<f64>,
    total: f64,
    /// Walking mode: (position along the ring, node) of each ring node.
    node_pos: Vec<(f64, usize)>,
    /// (position, metres climbed, metres descended) from the ring's start
    /// at each edge boundary.
    climbs: Vec<(f64, f64, f64)>,
}

impl<'a> Splitter<'a> {
    fn new(ring: &'a Ring, ends: &'a Ends<'a>, marks: &'a RingMarks<'a>, cfg: &'a Config) -> Splitter<'a> {
        let cum = cumulative(&ring.pts);
        let total = *cum.last().unwrap();
        let mut node_pos = vec![];
        if let Ends::Walking(g, _) = ends {
            let mut s = 0.0;
            for &(ei, fwd) in &ring.edges {
                let e = &g.edges[ei];
                node_pos.push((s, if fwd { e.a } else { e.b }));
                s += e.len;
            }
            if let Some(&(_, n0)) = node_pos.first() {
                node_pos.push((s, n0));
            }
        }
        let mut climbs = vec![(0.0, 0.0, 0.0)];
        for &(len, rise) in &ring.profile {
            let &(s, up, down) = climbs.last().unwrap();
            climbs.push((s + len, up + rise.max(0.0), down + (-rise).max(0.0)));
        }
        Splitter { ring, ends, marks, cfg, cum, total, node_pos, climbs }
    }

    /// Metres climbed and descended from the ring's start to position `s`.
    fn climbed_to(&self, s: f64) -> (f64, f64) {
        let k = self.climbs.partition_point(|c| c.0 <= s).clamp(1, self.climbs.len() - 1);
        let (a, b) = (self.climbs[k - 1], self.climbs[k]);
        let t = if b.0 > a.0 { ((s - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) } else { 0.0 };
        (a.1 + t * (b.1 - a.1), a.2 + t * (b.2 - a.2))
    }

    /// Metres climbed and descended walking `len` forward from `from`
    /// (wrapping round the ring).
    fn climb(&self, from: f64, len: f64) -> (f64, f64) {
        let (u0, d0) = self.climbed_to(from);
        let to = from + len;
        if to <= self.total {
            let (u1, d1) = self.climbed_to(to);
            return ((u1 - u0).max(0.0), (d1 - d0).max(0.0));
        }
        let (ut, dt) = self.climbed_to(self.total);
        let (u1, d1) = self.climbed_to(to - self.total);
        ((ut - u0 + u1).max(0.0), (dt - d0 + d1).max(0.0))
    }

    /// Walking mode: metres from the base to ring position `s`, via the
    /// nearer of the ring nodes either side of it, and that node.
    fn walk_to(&self, s: f64) -> Option<(f64, usize)> {
        let Ends::Walking(_, tree) = self.ends else { return None };
        let k = self.node_pos.partition_point(|&(p, _)| p <= s).clamp(1, self.node_pos.len() - 1);
        let (p0, n0) = self.node_pos[k - 1];
        let (p1, n1) = self.node_pos[k];
        let a = tree.walk_m(n0).map(|d| (d + (s - p0).max(0.0), n0));
        let b = tree.walk_m(n1).map(|d| (d + (p1 - s).max(0.0), n1));
        match (a, b) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    fn end_at(&self, s: f64) -> End {
        let p = point_at(&self.ring.pts, &self.cum, s);
        let head = self.marks.at(s, p);
        match self.ends {
            Ends::Transit(transit) => {
                let walk = self.cfg.access_walk_m;
                let (go, back) = (transit.access(p, walk, true), transit.access(p, walk, false));
                End { go, back, walk: None, head }
            }
            Ends::Walking(..) => End { go: None, back: None, walk: self.walk_to(s), head },
        }
    }

    /// Walking mode: the walk between the base and ring position `s`.
    /// `home`: the walk back from the ring rather than out to it.
    fn approach(&self, e: &End, s: f64, home: bool) -> Option<Approach> {
        let (Ends::Walking(g, tree), Some((metres, node))) = (self.ends, e.walk) else { return None };
        let mut pts = tree.path(g, node);
        pts.push(point_at(&self.ring.pts, &self.cum, s));
        let (up, down) = tree.climb(node);
        let (up, down) = if home { (down, up) } else { (up, down) };
        Some(Approach { metres, pts, node, walk: vec![], up, down })
    }
}
/// How a ring position connects to the base.
#[derive(Clone)]
struct End {
    go: Option<Access>,
    back: Option<Access>,
    /// Walking mode: metres from the base, and the ring node walked to.
    walk: Option<(f64, usize)>,
    head: Option<Trailhead>,
}
