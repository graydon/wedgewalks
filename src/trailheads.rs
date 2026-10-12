//! Trailheads: natural places for an outing to begin or end. Stations and
//! interchanges, named neighbourhoods, landmarks, clusters of cafés, and
//! along each ring, where it joins or leaves a named path or walking route
//! and where it enters or leaves a park. Arc cut points are drawn towards
//! them (`arcs::split`).

use crate::graph::{Graph, Proj, Xy, norm};
use crate::osm::{Kind, OsmData, SpotKind};
use crate::places::{Places, midpoint};
use crate::rings::Ring;
use crate::transit::Transit;
use rstar::RTree;
use rstar::primitives::GeomWithData;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

/// A place worth starting or ending at, with how good it is (0..1).
#[derive(Clone, Debug)]
pub struct Trailhead {
    pub label: String,
    pub score: f64,
    pub kind: HeadKind,
}

/// At one spot, only the best trailhead of each kind counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HeadKind {
    /// A station, ferry terminal or interchange.
    Transit,
    Path,
    Park,
    Locality,
    Landmark,
    Cafes,
}

/// Scores for each kind of trailhead.
const STATION_SCORE: f64 = 1.0;
const FERRY_SCORE: f64 = 0.8;
const INTERCHANGE_SCORE: f64 = 0.8;
const PATH_END_SCORE: f64 = 0.7;
const PARK_GATE_SCORE: f64 = 0.6;
const LOCALITY_SCORE: f64 = 0.6;
const LANDMARK_SCORE: f64 = 0.6;
const CAFES_SCORE: f64 = 0.5;
/// Further trailheads at the same spot add this share of their score.
const EXTRA_SHARE: f64 = 0.3;

/// Stations, landmarks and cafés count within this distance of a cut point...
const NEAR_M: f64 = 150.0;
/// ...and a neighbourhood's centre node, which marks it only loosely, within this.
const LOCALITY_NEAR_M: f64 = 400.0;
/// Stops with the same name this close together form one station.
const STATION_GROUP_M: f64 = 150.0;
/// A stop served by at least this many routes is an interchange. OSM maps
/// each direction as its own route, so 4 is about two lines.
const INTERCHANGE_ROUTES: usize = 4;
/// At least this many cafés, restaurants, pubs or bakeries near a point
/// make it a lively place to start or finish.
const CAFES_MIN: usize = 4;
/// Route modes whose stops are stations.
const RAIL: &[&str] = &["train", "subway", "light_rail", "monorail"];

/// Named paths shorter than this along the ring, and parks the ring spends
/// less than this in, don't make trailheads.
const MIN_PATH_M: f64 = 400.0;
const MIN_PARK_M: f64 = 300.0;
/// Interruptions shorter than this (a road crossing, a bridge) don't end a
/// path or park.
const SHORT_RUN_M: f64 = 60.0;
/// Trailheads on the ring count within this distance along it.
const ALONG_M: f64 = 75.0;

/// Point trailheads around the base.
pub struct Trailheads {
    heads: Vec<(Trailhead, f64)>,
    /// Index into `heads`, at each one's location.
    points: RTree<GeomWithData<Xy, usize>>,
    venues: RTree<Xy>,
}

impl Trailheads {
    pub fn build(osm: &OsmData, proj: &Proj, transit: &Transit, radius_m: f64) -> Trailheads {
        let mut heads: Vec<(Trailhead, f64, Xy)> = stations(transit, radius_m);
        let mut venues = vec![];
        for s in &osm.spots {
            let p = proj.xy(s.lat, s.lon);
            if norm(p) > radius_m {
                continue;
            }
            let head = |label: String, score, kind| (Trailhead { label, score, kind }, p);
            match s.kind {
                SpotKind::Venue => venues.push(p),
                SpotKind::Locality => {
                    let (h, p) = head(s.name.clone(), LOCALITY_SCORE, HeadKind::Locality);
                    heads.push((h, LOCALITY_NEAR_M, p));
                }
                SpotKind::Landmark => {
                    let name = if s.name.is_empty() { "a viewpoint".to_string() } else { s.name.clone() };
                    let (h, p) = head(name, LANDMARK_SCORE, HeadKind::Landmark);
                    heads.push((h, NEAR_M, p));
                }
            }
        }
        let points = RTree::bulk_load(heads.iter().enumerate().map(|(i, h)| GeomWithData::new(h.2, i)).collect());
        Trailheads {
            heads: heads.into_iter().map(|(h, reach, _)| (h, reach)).collect(),
            points,
            venues: RTree::bulk_load(venues),
        }
    }

    /// No trailheads (tests, benchmarks).
    pub fn none() -> Trailheads {
        Trailheads { heads: vec![], points: RTree::new(), venues: RTree::new() }
    }

    /// How good a place to start or end a day `p` is, 0..1.
    pub fn score_at(&self, p: Xy) -> f64 {
        combine(self.near(p)).map_or(0.0, |h| h.score)
    }

    /// The point trailheads near `p`.
    fn near(&self, p: Xy) -> Vec<Trailhead> {
        let mut out: Vec<Trailhead> = self
            .points
            .locate_within_distance(p, LOCALITY_NEAR_M * LOCALITY_NEAR_M)
            .filter(|g| {
                let (_, reach) = &self.heads[g.data];
                crate::graph::dist(*g.geom(), p) <= *reach
            })
            .map(|g| self.heads[g.data].0.clone())
            .collect();
        let venues = self.venues.locate_within_distance(p, NEAR_M * NEAR_M).count();
        if venues >= CAFES_MIN {
            out.push(Trailhead { label: "cafés".into(), score: CAFES_SCORE, kind: HeadKind::Cafes });
        }
        out
    }
}

/// Stations, ferry terminals and interchanges: (trailhead, reach, location).
fn stations(transit: &Transit, radius_m: f64) -> Vec<(Trailhead, f64, Xy)> {
    // Grouped by name without platform or direction, so platforms and
    // stops on both sides of a street make one station.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::default();
    for (i, s) in transit.stops.iter().enumerate() {
        if s.named && norm(s.xy) <= radius_m {
            by_name.entry(station_name(&s.name)).or_default().push(i);
        }
    }
    let mut out = vec![];
    for (name, group) in &by_name {
        for &i in group {
            let xy = transit.stops[i].xy;
            // Routes at this stop and at same-named stops close by.
            let routes: HashSet<usize> = group
                .iter()
                .filter(|&&j| crate::graph::dist(transit.stops[j].xy, xy) <= STATION_GROUP_M)
                .flat_map(|&j| transit.stops[j].routes.iter().map(|r| r.0))
                .collect();
            let has = |modes: &[&str]| routes.iter().any(|&r| modes.contains(&transit.modes[r].as_str()));
            let lower = name.to_lowercase();
            let head = if has(RAIL) {
                let label = if lower.contains("station") { name.to_string() } else { format!("{name} station") };
                Trailhead { label, score: STATION_SCORE, kind: HeadKind::Transit }
            } else if has(&["ferry"]) {
                let label = if lower.contains("ferry") || lower.contains("terminal") {
                    name.to_string()
                } else {
                    format!("{name} ferry terminal")
                };
                Trailhead { label, score: FERRY_SCORE, kind: HeadKind::Transit }
            } else if routes.len() >= INTERCHANGE_ROUTES {
                Trailhead { label: format!("{name} (interchange)"), score: INTERCHANGE_SCORE, kind: HeadKind::Transit }
            } else {
                continue;
            };
            out.push((head, NEAR_M, xy));
        }
    }
    out.sort_by(|a, b| (&a.0.label, a.2[0], a.2[1]).partial_cmp(&(&b.0.label, b.2[0], b.2[1])).unwrap());
    out
}

/// A stop's name without platform, bay or direction: "Crossharbour
/// Platform 1" is Crossharbour, "Mountain Hwy (NB) at Crown St" is
/// Mountain Hwy at Crown St.
fn station_name(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    if let Some(k) =
        words.iter().position(|w| matches!(w.to_lowercase().as_str(), "platform" | "bay" | "quai" | "gleis"))
        && k > 0
        && words.len() - k <= 2
    {
        words.truncate(k);
    }
    words.retain(|w| !matches!(*w, "(NB)" | "(SB)" | "(EB)" | "(WB)" | "NB" | "SB" | "EB" | "WB"));
    words.join(" ").trim_end_matches([',', '-', ' ']).to_string()
}

/// One stretch of the ring with the same identity (path name, park).
#[derive(Debug, PartialEq)]
struct Run {
    id: Option<String>,
    start: f64,
    len: f64,
}

/// Consecutive pieces (identity, length) as runs, with short interruptions
/// absorbed into the run before them and the runs either side of the ring's
/// starting point merged.
fn runs(pieces: &[(Option<String>, f64)]) -> Vec<Run> {
    let mut out: Vec<Run> = vec![];
    let mut s = 0.0;
    for (id, len) in pieces {
        match out.last_mut() {
            Some(r) if r.id == *id || *len < SHORT_RUN_M => r.len += len,
            _ => out.push(Run { id: id.clone(), start: s, len: *len }),
        }
        // A run cut short by an interruption continues after it.
        if out.len() >= 3 && out[out.len() - 2].len < SHORT_RUN_M && out[out.len() - 3].id == out[out.len() - 1].id {
            let last = out.pop().unwrap();
            let gap = out.pop().unwrap();
            out.last_mut().unwrap().len += gap.len + last.len;
        }
        s += len;
    }
    if out.len() > 1 && out[0].id == out[out.len() - 1].id {
        let last = out.pop().unwrap();
        out[0].start = last.start;
        out[0].len += last.len;
    }
    out
}

/// Where runs begin, as (position, trailhead), for runs whose identity is
/// worth marking (named and long enough); `label` describes the boundary
/// from the run before (if marked) to the run after (if marked).
fn boundaries(
    runs: &[Run],
    min_len: f64,
    score: f64,
    kind: HeadKind,
    label: impl Fn(Option<&str>, Option<&str>) -> String,
) -> Vec<(f64, Trailhead)> {
    let n = runs.len();
    if n < 2 {
        return vec![];
    }
    let marked = |k: usize| runs[k].id.as_deref().filter(|_| runs[k].len >= min_len);
    (0..n)
        .filter_map(|k| {
            let (prev, next) = (marked((k + n - 1) % n), marked(k));
            (prev.is_some() || next.is_some())
                .then(|| (runs[k].start, Trailhead { label: label(prev, next), score, kind }))
        })
        .collect()
}

/// The trailheads for one ring: its own (path ends, park gates) and the
/// point trailheads around it.
pub struct RingMarks<'a> {
    heads: &'a Trailheads,
    marks: Vec<(f64, Trailhead)>,
    total: f64,
}

impl<'a> RingMarks<'a> {
    pub fn new(ring: &Ring, g: &Graph, places: &Places, heads: &'a Trailheads) -> RingMarks<'a> {
        let mut paths = vec![];
        let mut parks = vec![];
        for &(ei, _) in &ring.edges {
            let e = &g.edges[ei];
            let w = &g.ways[e.way as usize];
            let path = (e.kind == Kind::CarFree).then(|| w.name.clone().or_else(|| w.route.clone())).flatten();
            paths.push((path, e.len));
            let park = midpoint(&e.pts).and_then(|m| places.park_at(m)).map(str::to_string);
            parks.push((park, e.len));
        }
        let total = paths.iter().map(|p| p.1).sum();
        let mut marks = boundaries(&runs(&paths), MIN_PATH_M, PATH_END_SCORE, HeadKind::Path, |a, b| match (a, b) {
            (Some(a), Some(b)) => format!("where {a} meets {b}"),
            (None, Some(b)) => format!("where it joins {b}"),
            (Some(a), _) => format!("where it leaves {a}"),
            (None, None) => unreachable!(),
        });
        marks.extend(boundaries(&runs(&parks), MIN_PARK_M, PARK_GATE_SCORE, HeadKind::Park, |a, b| {
            format!("{} entrance", b.or(a).unwrap())
        }));
        RingMarks { heads, marks, total }
    }

    /// The trailhead (or a combination) at ring position `s`, point `p`.
    pub fn at(&self, s: f64, p: Xy) -> Option<Trailhead> {
        let mut found = self.heads.near(p);
        for (pos, h) in &self.marks {
            let d = (s - pos).rem_euclid(self.total);
            if d.min(self.total - d) <= ALONG_M {
                found.push(h.clone());
            }
        }
        combine(found)
    }
}

/// Several trailheads at one spot: the best of each kind, scored as the
/// best plus a share of the others, labelled with the best two.
fn combine(mut found: Vec<Trailhead>) -> Option<Trailhead> {
    found.sort_by(|a, b| a.kind.cmp(&b.kind).then(b.score.total_cmp(&a.score)).then_with(|| a.label.cmp(&b.label)));
    found.dedup_by(|a, b| a.kind == b.kind);
    found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.kind.cmp(&b.kind)));
    let best = found.first()?;
    let score = (best.score + EXTRA_SHARE * found[1..].iter().map(|h| h.score).sum::<f64>()).min(1.0);
    let label = found.iter().take(2).map(|h| h.label.as_str()).collect::<Vec<_>>().join(", ");
    Some(Trailhead { label, score, kind: best.kind })
}

#[cfg(test)]
mod tests {
    use super::{Run, runs};

    fn s(name: &str) -> Option<String> {
        (!name.is_empty()).then(|| name.to_string())
    }

    #[test]
    fn runs_absorb_short_interruptions_and_wrap() {
        let pieces = [(s("A"), 300.0), (s(""), 20.0), (s("A"), 200.0), (s("B"), 500.0), (s("A"), 100.0)];
        assert_eq!(
            runs(&pieces),
            vec![Run { id: s("A"), start: 1020.0, len: 620.0 }, Run { id: s("B"), start: 520.0, len: 500.0 }]
        );
    }

    #[test]
    fn station_names_drop_platforms_and_directions() {
        use super::station_name;
        assert_eq!(station_name("Crossharbour Platform 1"), "Crossharbour");
        assert_eq!(station_name("Brentwood Town Centre Station Bay 1"), "Brentwood Town Centre Station");
        assert_eq!(station_name("Mountain Hwy (NB) at Crown St"), "Mountain Hwy at Crown St");
        assert_eq!(station_name("Bay Street"), "Bay Street");
        assert_eq!(station_name("Union"), "Union");
    }

    #[test]
    fn combine_prefers_the_best() {
        use super::HeadKind::{Cafes, Transit};
        let h = |label: &str, score, kind| super::Trailhead { label: label.into(), score, kind };
        let c = super::combine(vec![
            h("cafés", 0.5, Cafes),
            h("Union (interchange)", 0.8, Transit),
            h("Union station", 1.0, Transit),
            h("cafés", 0.5, Cafes),
        ])
        .unwrap();
        assert_eq!(c.label, "Union station, cafés");
        assert_eq!(c.score, 1.0);
        let c = super::combine(vec![h("Bay (interchange)", 0.8, Transit), h("Bay St (interchange)", 0.8, Transit)])
            .unwrap();
        assert_eq!(c.label, "Bay (interchange)");
        assert_eq!(c.score, 0.8);
        assert!(super::combine(vec![]).is_none());
    }
}
