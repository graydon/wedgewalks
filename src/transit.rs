//! Transit access from the base (city center) to ring points and back,
//! using OSM route relations: stop order only, no schedules. At most one
//! transfer, with a short walk allowed between the two stops.

use crate::config::Config;
use crate::graph::{Proj, Xy, dist, norm};
use crate::osm::OsmData;
use rstar::RTree;
use rstar::primitives::GeomWithData;
use rustc_hash::FxHashMap as HashMap;

/// Typical minutes between daytime departures when OSM doesn't say.
fn typical_headway(mode: &str) -> f32 {
    match mode {
        "subway" | "light_rail" | "monorail" => 5.0,
        "tram" => 8.0,
        "bus" | "trolleybus" => 12.0,
        _ => 15.0, // train, ferry
    }
}

/// Ranking penalty for night-only routes, in metres of walking.
const NIGHT_PENALTY: f64 = 10_000.0;

pub struct Stop {
    pub xy: Xy,
    /// The stop's own name, else "near" a named stop close by, else
    /// `UNNAMED_STOP`.
    pub name: String,
    /// Whether `name` is the stop's own.
    pub named: bool,
    /// (route, position of this stop in the route's stop order)
    pub routes: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Leg {
    pub route: usize,
    pub board: usize,
    pub alight: usize,
    /// Positions of the boarding and alighting stops in the route's stop
    /// order (`from` < `to`).
    pub from: usize,
    pub to: usize,
}

/// How to get between the base and one stop: legs in travel order.
#[derive(Clone)]
struct Reach {
    cost: f64,
    center_stop: usize,
    center_walk: f64,
    legs: Vec<Leg>,
}

#[derive(Clone)]
pub struct Access {
    /// Ring-side stop and the walk between it and the ring point.
    pub stop: usize,
    pub walk_m: f64,
    pub center_stop: usize,
    pub center_walk_m: f64,
    pub legs: Vec<Leg>,
}

pub struct Transit {
    pub stops: Vec<Stop>,
    pub labels: Vec<String>,
    pub night: Vec<bool>,
    /// Each route's OSM mode (bus, tram, subway, train, ferry, ...).
    pub modes: Vec<String>,
    /// Minutes between departures per route, and whether OSM says so (else
    /// a typical value for the mode).
    pub headway: Vec<(f32, bool)>,
    tree: RTree<GeomWithData<Xy, usize>>,
    /// Best way from the base to each stop, and from each stop to the base.
    out: Vec<Option<Reach>>,
    back: Vec<Option<Reach>>,
    /// Number of routes with a stop near the center.
    pub center_routes: usize,
    /// Each route's stops in order (None where outside the extract).
    route_stops: Vec<Vec<Option<usize>>>,
}

fn improve(slot: &mut Option<Reach>, r: Reach) {
    if slot.as_ref().is_none_or(|s| r.cost < s.cost) {
        *slot = Some(r);
    }
}

/// Unnamed stops borrow the name of a named stop at most this far away.
const NAME_BORROW_M: f64 = 150.0;
/// What a stop with no name, and no named stop close by, is called.
const UNNAMED_STOP: &str = "an unnamed stop";

/// A possible boarding of a second route after a change.
#[derive(Clone, Copy)]
struct Boarding {
    /// Position along the second route.
    pos: usize,
    /// Cost so far, relative to the route's start (outbound) or end
    /// (return), so the ride cost can be added during the sweep.
    cost: f64,
    /// Stop reached by the first ride, and the stop boarded after walking.
    from_stop: usize,
    stop: usize,
}

/// The stop network, while working out the best ways to and from the base.
struct Net<'a> {
    cfg: &'a Config,
    stops: Vec<Stop>,
    /// Each route's stops in order (None where outside the extract).
    route_stops: Vec<Vec<Option<usize>>>,
    tree: RTree<GeomWithData<Xy, usize>>,
    /// Cost of boarding each route (expected wait, bus and night penalties).
    board: Vec<f64>,
    /// Ride cost from each route's first stop to each position.
    ride: Vec<Vec<f64>>,
}

impl Net<'_> {
    /// Direct rides: every stop after (outbound) or before (return) a stop
    /// near the center on the same route. Also counts the routes serving
    /// the center.
    fn direct(&self) -> (Vec<Option<Reach>>, Vec<Option<Reach>>, usize) {
        let mut out: Vec<Option<Reach>> = vec![None; self.stops.len()];
        let mut back: Vec<Option<Reach>> = vec![None; self.stops.len()];
        let mut center_routes = rustc_hash::FxHashSet::default();
        for c in self.tree.locate_within_distance([0.0, 0.0], self.cfg.access_walk_m.powi(2)) {
            let (cs, cw) = (c.data, norm(*c.geom()));
            for &(r, pc) in &self.stops[cs].routes {
                center_routes.insert(r);
                for (pos, s) in self.route_stops[r].iter().enumerate() {
                    let Some(s) = *s else { continue };
                    let cost = cw + self.board[r] + (self.ride[r][pos] - self.ride[r][pc]).abs();
                    let reach = |legs| Reach { cost, center_stop: cs, center_walk: cw, legs };
                    if pos > pc {
                        improve(&mut out[s], reach(vec![Leg { route: r, board: cs, alight: s, from: pc, to: pos }]));
                    } else if pos < pc {
                        improve(&mut back[s], reach(vec![Leg { route: r, board: s, alight: cs, from: pos, to: pc }]));
                    }
                }
            }
        }
        (out, back, center_routes.len())
    }

    /// Adds a change: from a stop already reached, walk to a nearby stop
    /// and ride another route. For each such route, sweep its stops in
    /// travel order keeping the cheapest boarding so far. `nth`: this is
    /// the nth change (1 for the first), which costs `nth` penalties.
    fn with_transfer(&self, direct: &[Option<Reach>], outbound: bool, nth: usize) -> Vec<Option<Reach>> {
        let cfg = self.cfg;
        let mut boardings: HashMap<usize, Vec<Boarding>> = HashMap::default();
        for (t1, reach) in direct.iter().enumerate() {
            let Some(reach) = reach else { continue };
            for t2 in self.tree.locate_within_distance(self.stops[t1].xy, cfg.transfer_walk_m.powi(2)) {
                let w = dist(self.stops[t1].xy, *t2.geom());
                for &(r, pos) in &self.stops[t2.data].routes {
                    let ride_here = if outbound { -self.ride[r][pos] } else { self.ride[r][pos] };
                    let penalty = cfg.transfer_penalty_m * nth as f64;
                    let cost = reach.cost + w + penalty + self.board[r] + ride_here;
                    boardings.entry(r).or_default().push(Boarding { pos, cost, from_stop: t1, stop: t2.data });
                }
            }
        }
        let mut best: Vec<Option<Reach>> = direct.to_vec();
        for (r, mut bs) in boardings {
            let seq = &self.route_stops[r];
            bs.sort_by_key(|b| b.pos);
            let order: Vec<usize> = if outbound { (0..seq.len()).collect() } else { (0..seq.len()).rev().collect() };
            if !outbound {
                bs.reverse();
            }
            let mut next = 0;
            let mut cheapest: Option<Boarding> = None;
            for pos in order {
                if let (Some(s), Some(b)) = (seq[pos], cheapest) {
                    let cost = b.cost + if outbound { self.ride[r][pos] } else { -self.ride[r][pos] };
                    let first = direct[b.from_stop].as_ref().unwrap();
                    let mut legs = first.legs.clone();
                    if outbound {
                        legs.push(Leg { route: r, board: b.stop, alight: s, from: b.pos, to: pos });
                    } else {
                        legs.insert(0, Leg { route: r, board: s, alight: b.stop, from: pos, to: b.pos });
                    }
                    improve(
                        &mut best[s],
                        Reach { cost, center_stop: first.center_stop, center_walk: first.center_walk, legs },
                    );
                }
                // Boardings at this position count for later stops only.
                while next < bs.len() && bs[next].pos == pos {
                    if cheapest.is_none_or(|c| bs[next].cost < c.cost) {
                        cheapest = Some(bs[next]);
                    }
                    next += 1;
                }
            }
        }
        best
    }
}

/// Stops within `limit` of the base, deduplicated, and each route's stops
/// in order as indices into them.
fn index_stops(osm: &OsmData, proj: &Proj, limit: f64) -> (Vec<Stop>, Vec<Vec<Option<usize>>>) {
    let mut index: HashMap<i64, usize> = HashMap::default();
    let mut stops: Vec<Stop> = vec![];
    let mut unnamed = vec![];
    let route_stops = osm
        .routes
        .iter()
        .enumerate()
        .map(|(ri, r)| {
            r.stops
                .iter()
                .enumerate()
                .map(|(pos, id)| {
                    let xy = osm.coords.get(id).map(|&(la, lo)| proj.xy(la, lo)).filter(|&p| norm(p) <= limit)?;
                    let si = *index.entry(*id).or_insert_with(|| {
                        let own = osm.stop_names.get(id).cloned();
                        if own.is_none() {
                            unnamed.push(stops.len());
                        }
                        let named = own.is_some();
                        let name = own.unwrap_or_else(|| UNNAMED_STOP.to_string());
                        stops.push(Stop { xy, name, named, routes: vec![] });
                        stops.len() - 1
                    });
                    stops[si].routes.push((ri, pos));
                    Some(si)
                })
                .collect()
        })
        .collect();
    // Unnamed stops (often stop positions) borrow the nearest name.
    let named: RTree<GeomWithData<Xy, usize>> = RTree::bulk_load(
        (0..stops.len())
            .filter(|i| unnamed.binary_search(i).is_err())
            .map(|i| GeomWithData::new(stops[i].xy, i))
            .collect(),
    );
    for &i in &unnamed {
        if let Some(n) = named.nearest_neighbor(&stops[i].xy)
            && dist(*n.geom(), stops[i].xy) < NAME_BORROW_M
        {
            stops[i].name = format!("near {}", stops[n.data].name);
        }
    }
    (stops, route_stops)
}

/// Ride cost (distance ridden times `k`) from each route's first stop to
/// each position.
fn ride_costs(route_stops: &[Vec<Option<usize>>], stops: &[Stop], k: f64) -> Vec<Vec<f64>> {
    route_stops
        .iter()
        .map(|seq| {
            let mut acc = 0.0;
            let mut last: Option<Xy> = None;
            seq.iter()
                .map(|s| {
                    if let Some(s) = s {
                        let p = stops[*s].xy;
                        if let Some(q) = last {
                            acc += dist(p, q);
                        }
                        last = Some(p);
                    }
                    acc * k
                })
                .collect()
        })
        .collect()
}

impl Transit {
    pub fn build(osm: &OsmData, proj: &Proj, cfg: &Config) -> Transit {
        let labels: Vec<String> =
            osm.routes.iter().map(|r| format!("{} {}", r.mode, r.label).trim().to_string()).collect();
        let night: Vec<bool> = osm.routes.iter().map(|r| r.night).collect();
        let modes: Vec<String> = osm.routes.iter().map(|r| r.mode.clone()).collect();
        // Minutes between departures: mapped, else typical for the mode.
        let headway: Vec<(f32, bool)> = osm
            .routes
            .iter()
            .map(|r| r.interval_min.map_or((typical_headway(&r.mode), false), |m| (m, true)))
            .collect();
        let board: Vec<f64> = osm
            .routes
            .iter()
            .zip(&headway)
            .map(|(r, &(h, _))| {
                let bus = matches!(r.mode.as_str(), "bus" | "trolleybus");
                (h as f64 / 2.0) * cfg.wait_cost_m_per_min
                    + if bus { cfg.bus_penalty_m } else { 0.0 }
                    + if r.night { NIGHT_PENALTY } else { 0.0 }
            })
            .collect();
        let (stops, route_stops) = index_stops(osm, proj, cfg.extract_radius_km * 1000.0);
        let ride = ride_costs(&route_stops, &stops, cfg.ride_cost);
        let tree = RTree::bulk_load(stops.iter().enumerate().map(|(i, s)| GeomWithData::new(s.xy, i)).collect());
        let net = Net { cfg, stops, route_stops, tree, board, ride };
        let (out, back, center_routes) = net.direct();
        // Each round adds one more change to what's already reachable.
        let (mut out, mut back) = (out, back);
        for nth in 1..=cfg.max_changes {
            (out, back) = (net.with_transfer(&out, true, nth), net.with_transfer(&back, false, nth));
        }
        let Net { stops, route_stops, tree, .. } = net;
        Transit { stops, labels, night, modes, headway, tree, out, back, center_routes, route_stops }
    }

    /// Best way between the base and a ring point `p`: outbound (base to
    /// `p`) or return. Ranked by walking, with transfers and night-only
    /// routes penalised.
    pub fn access(&self, p: Xy, walk_m: f64, outbound: bool) -> Option<Access> {
        let table = if outbound { &self.out } else { &self.back };
        let mut best: Option<(f64, Access)> = None;
        for s in self.tree.locate_within_distance(p, walk_m.powi(2)) {
            let Some(reach) = &table[s.data] else { continue };
            let w = dist(*s.geom(), p);
            let rank = reach.cost + w;
            if best.as_ref().is_none_or(|b| rank < b.0) {
                let a = Access {
                    stop: s.data,
                    walk_m: w,
                    center_stop: reach.center_stop,
                    center_walk_m: reach.center_walk,
                    legs: reach.legs.clone(),
                };
                best = Some((rank, a));
            }
        }
        best.map(|b| b.1)
    }

    /// Directions for getting to the ring (`outbound`) or back to the base,
    /// e.g. "walk 120 m, take 16 Arbutus from A to B, walk 40 m to the ring".
    pub fn trip_text(&self, a: Option<&Access>, outbound: bool) -> String {
        match a {
            None => "no transit within the allowed changes: taxi or a longer walk needed".into(),
            Some(a) if outbound => {
                format!(
                    "walk {:.0} m, {}, walk {:.0} m to the ring",
                    a.center_walk_m,
                    self.describe_legs(&a.legs),
                    a.walk_m
                )
            }
            Some(a) => format!(
                "walk {:.0} m, {}, walk {:.0} m to base",
                a.walk_m,
                self.describe_legs(&a.legs),
                a.center_walk_m
            ),
        }
    }

    /// "take X from A to B, change (walk 40 m), take Y from C to D".
    pub fn describe_legs(&self, legs: &[Leg]) -> String {
        let mut parts = vec![];
        for (i, l) in legs.iter().enumerate() {
            if i > 0 {
                let w = dist(self.stops[legs[i - 1].alight].xy, self.stops[l.board].xy);
                parts.push(format!("change (walk {w:.0} m)"));
            }
            let (h, known) = self.headway[l.route];
            parts.push(format!(
                "take {}{}{} from {} to {}",
                self.labels[l.route],
                if self.night[l.route] { " [NIGHT ONLY]" } else { "" },
                if known { format!(" (every ~{h:.0} min)") } else { String::new() },
                self.stops[l.board].name,
                self.stops[l.alight].name
            ));
        }
        parts.join(", ")
    }

    /// The stops a leg passes, in travel order, for drawing. (OSM routes
    /// give stop order, not track geometry, so this is stop to stop.)
    pub fn leg_points(&self, l: &Leg) -> Vec<Xy> {
        let seq = &self.route_stops[l.route];
        let pts: Vec<Xy> = seq[l.from.min(seq.len())..=l.to.min(seq.len() - 1)]
            .iter()
            .filter_map(|s| s.map(|s| self.stops[s].xy))
            .collect();
        // Platforms and stop positions alternate in many routes; drop
        // near-duplicates so the line doesn't zigzag.
        let mut out: Vec<Xy> = vec![];
        for p in pts {
            if out.last().is_none_or(|q| dist(*q, p) > 40.0) {
                out.push(p);
            }
        }
        out
    }

    pub fn route_names(&self, legs: &[Leg]) -> String {
        legs.iter().map(|l| self.labels[l.route].as_str()).collect::<Vec<_>>().join(" + ")
    }
}
