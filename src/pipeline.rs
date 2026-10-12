//! The planning pipeline in stages, so callers (CLI, GUI) can cache and
//! re-run each part independently:
//! load (parse OSM, cached on disk) -> build (graph, transit, shade for a
//! base location) -> plan (rings and arcs for a set of parameters).

use crate::arcs::{self, Arc};
use crate::config::Config;
use crate::graph::{Graph, Proj, norm};
use crate::osm::{self, OsmData};
use crate::rings::{self, Ring};
use crate::shade::Shade;
use crate::trailheads::{RingMarks, Trailheads};
use crate::transit::Transit;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Extra radius parsed beyond `extract_radius_km`, so the base can move a
/// little without re-reading the file.
pub const LOAD_MARGIN_M: f64 = 5000.0;

/// Bumped whenever `OsmData` changes shape, so old caches are re-parsed.
const CACHE_FORMAT: u32 = 7;

/// Walking mode: walks out and home are searched this far beyond the ring.
const APPROACH_MARGIN_M: f64 = 1500.0;

/// Source files smaller than this are re-parsed rather than cached.
const CACHE_MIN_BYTES: u64 = 400 << 20;

pub fn city_key(name: &str) -> String {
    name.to_lowercase().replace([' ', '/', '\\'], "_")
}

pub fn pbf_path(data: &Path, key: &str) -> PathBuf {
    data.join(format!("{key}.osm.pbf"))
}

/// Parsed OSM data around a center.
#[derive(Serialize, Deserialize)]
pub struct Loaded {
    /// `CACHE_FORMAT` when written: older caches are re-parsed.
    format: u32,
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
    source_mtime: u64,
    pub osm: OsmData,
    /// Ground elevation, if it could be had (not cached with the parse:
    /// the tiles are kept on disk).
    #[serde(skip)]
    pub dem: Option<crate::elevation::Dem>,
}

impl Loaded {
    /// Wraps data that didn't come from a file (tests, synthetic cities).
    pub fn from_osm(osm: OsmData, lat: f64, lon: f64, radius_m: f64) -> Loaded {
        Loaded { format: CACHE_FORMAT, lat, lon, radius_m, source_mtime: 0, osm, dem: None }
    }

    /// Whether a circle of `radius_m` around (lat, lon) lies inside what
    /// was loaded.
    pub fn covers(&self, lat: f64, lon: f64, radius_m: f64) -> bool {
        norm(Proj::new(self.lat, self.lon).xy(lat, lon)) + radius_m <= self.radius_m + 1.0
    }
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Loads OSM data covering `radius_m` around (lat, lon), from the on-disk
/// cache when it is fresh and large enough, else by parsing `pbf`.
pub fn load(
    pbf: &Path,
    cache: &Path,
    lat: f64,
    lon: f64,
    radius_m: f64,
    progress: &dyn Fn(&str),
) -> Result<Loaded, String> {
    let source_mtime = mtime(pbf);
    if source_mtime == 0 {
        return Err(format!("{} not found", pbf.display()));
    }
    if let Ok(bytes) = std::fs::read(cache) {
        progress("reading cache");
        if let Ok(l) = bincode::deserialize::<Loaded>(&bytes)
            && l.format == CACHE_FORMAT
            && l.source_mtime == source_mtime
            && l.covers(lat, lon, radius_m)
        {
            return Ok(l);
        }
    }
    let osm = osm::read(pbf, lat, lon, radius_m, progress)?;
    let l = Loaded { format: CACHE_FORMAT, lat, lon, radius_m, source_mtime, osm, dem: None };
    // City extracts parse in a couple of seconds; only big region files
    // are worth caching.
    if std::fs::metadata(pbf).map_or(0, |m| m.len()) < CACHE_MIN_BYTES {
        let _ = std::fs::remove_file(cache);
        return Ok(l);
    }
    progress("writing cache");
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match bincode::serialize(&l) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(cache, bytes) {
                eprintln!("warning: cache {}: {e}", cache.display());
            }
        }
        Err(e) => eprintln!("warning: cache: {e}"),
    }
    Ok(l)
}

/// Everything derived from loaded data for one base location.
pub struct Built {
    pub lat: f64,
    pub lon: f64,
    pub proj: Proj,
    pub graph: Graph,
    pub transit: Transit,
    pub shade: Shade,
    /// Shade and street life of each way.
    pub feel: crate::feel::Feel,
    /// Named surroundings, for describing unnamed paths.
    pub places: crate::places::Places,
    /// Stations, neighbourhoods, landmarks and cafés, for where arcs end.
    pub heads: Trailheads,
    /// Water, parks and busy roads, for drawing.
    pub basemap: crate::basemap::Basemap,
    /// Whether ground elevation was available (else every climb is 0).
    pub has_elevation: bool,
}

/// A key for the settings `build` depends on: the whole config except
/// settings known to matter only to the later stages. A setting missing
/// from this exclusion list just causes an unneeded rebuild, never a stale
/// result.
pub fn build_key(cfg: &Config) -> String {
    let d = Config::default();
    let c = Config {
        road_avoidance: d.road_avoidance,
        prefer_away_from_traffic: d.prefer_away_from_traffic,
        prefer_waterside: d.prefer_waterside,
        route_bonus: d.route_bonus,
        climb_cost_m: d.climb_cost_m,
        bike_climb_cost_m: d.bike_climb_cost_m,
        cycle_bonus: d.cycle_bonus,
        bike_route_bonus: d.bike_route_bonus,
        turn_cost_m: d.turn_cost_m,
        life_weight: d.life_weight,
        shade_weight: d.shade_weight,
        trailhead_weight_m: d.trailhead_weight_m,
        centroid_offset_km: d.centroid_offset_km,
        trip: d.trip,
        travel: d.travel,
        access: d.access,
        direct_trip: d.direct_trip,
        bike_trip: d.bike_trip,
        bike_direct_trip: d.bike_direct_trip,
        ..search_independent(cfg)
    };
    format!("{c:?}")
}

/// The config with settings that only affect `select` (limits, ranking)
/// reset to defaults: the key for `search`, and the base of the key for
/// `build`.
pub fn search_key(cfg: &Config) -> String {
    format!("{:?}", search_independent(cfg))
}

fn search_independent(cfg: &Config) -> Config {
    let d = Config::default();
    Config {
        max_road_gap_m: d.max_road_gap_m,
        max_road_fraction: d.max_road_fraction,
        max_double_back_km: d.max_double_back_km,
        max_climb_per_day_m: d.max_climb_per_day_m,
        top_n: d.top_n,
        ..cfg.clone()
    }
}

pub fn build(l: &Loaded, lat: f64, lon: f64, cfg: &Config) -> Built {
    let proj = Proj::new(lat, lon);
    let around = cfg.surroundings_m();
    let ((graph, places), ((transit, heads), (shade, parks))) = rayon::join(
        || {
            rayon::join(
                || {
                    let mut g = Graph::build(&l.osm, &proj, &crate::graph::BuildOpts::from(cfg));
                    if let Some(dem) = &l.dem {
                        g.set_elevation(dem, &proj);
                    }
                    g
                },
                || crate::places::Places::build(&l.osm, &proj, around),
            )
        },
        || {
            rayon::join(
                || {
                    let transit = Transit::build(&l.osm, &proj, cfg);
                    let heads = Trailheads::build(&l.osm, &proj, &transit, around);
                    (transit, heads)
                },
                || {
                    rayon::join(
                        || Shade::build(&l.osm, &proj, around, cfg.shade_near_m),
                        // The base map shows parks over the whole extract.
                        || crate::shade::park_outlines(&l.osm, &proj, cfg.extract_radius_km * 1000.0),
                    )
                },
            )
        },
    );
    let basemap = crate::basemap::Basemap::build(&l.osm, &proj, &graph, &parks, cfg.extract_radius_km * 1000.0);
    let has_elevation = l.dem.is_some();
    let feel = crate::feel::Feel::build(&graph, &l.osm, &proj, &shade, around);
    Built { lat, lon, proj, graph, transit, shade, feel, places, heads, basemap, has_elevation }
}

pub struct Plan {
    /// Best rings first (valid before invalid), at most `top_n`.
    pub rings: Vec<Ring>,
    /// Arcs for each ring in `rings`.
    pub arcs: Vec<Vec<Arc>>,
    pub candidates: usize,
    pub valid: usize,
}

/// Loads a city's data around (lat, lon), enough for `cfg.extract_radius_km`
/// plus `LOAD_MARGIN_M`.
pub fn load_city(
    city: &crate::cities::City,
    data: &Path,
    lat: f64,
    lon: f64,
    cfg: &Config,
    progress: &dyn Fn(&str),
) -> Result<Loaded, String> {
    let pbf = city.pbf_path(data);
    if !pbf.exists() {
        return Err(format!("No map data for {} yet: download it first.", city.name));
    }
    let radius = cfg.extract_radius_km * 1000.0 + LOAD_MARGIN_M;
    let mut l = load(&pbf, &city.cache_path(data), lat, lon, radius, progress)?;
    // Planning works without elevation, just without climbs.
    match crate::elevation::Dem::load(data, lat, lon, radius, progress) {
        Ok(d) => l.dem = Some(d),
        Err(e) => eprintln!("note: no elevation for {}: {e}", city.name),
    }
    Ok(l)
}

/// Everything for one city at its CSV base: load, build and plan, with
/// Err if cancelled.
pub fn plan_city(
    city: &crate::cities::City,
    cfg: &Config,
    data: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(&str),
) -> Result<(Built, Plan), String> {
    let loaded = load_city(city, data, city.lat, city.lon, cfg, progress)?;
    let built = build(&loaded, city.lat, city.lon, cfg);
    progress(&format!("graph: {} nodes, {} edges", built.graph.nodes.len(), built.graph.edges.len()));
    let plan = plan(&built, cfg, cancel).ok_or("stopped")?;
    progress(&format!("rings: {} candidates, {} valid", plan.candidates, plan.valid));
    Ok((built, plan))
}

/// Finds rings and splits them into arcs. None if cancelled.
pub fn plan(b: &Built, cfg: &Config, cancel: &AtomicBool) -> Option<Plan> {
    let raw = search(b, cfg, cancel, &|_| {})?;
    Some(select(b, &raw, cfg))
}

/// The expensive part of planning: raw candidate rings. None if cancelled.
pub fn search(b: &Built, cfg: &Config, cancel: &AtomicBool, progress: &(dyn Fn(f32) + Sync)) -> Option<Vec<Ring>> {
    progress(0.0);
    let raw = crate::plan::search(b, cfg, cancel);
    progress(1.0);
    (!cancel.load(std::sync::atomic::Ordering::Relaxed)).then_some(raw)
}

/// The cheap part: score, check and dedupe candidates, then cut arcs.
pub fn select(b: &Built, raw: &[Ring], cfg: &Config) -> Plan {
    let mut rings = rings::select(&b.graph, raw, cfg);
    let candidates = rings.len();
    // Arcs are cut for more rings than are shown: a ring whose outings
    // mostly can't be reached is demoted once that is known.
    // Plans edited by hand are always kept.
    let n_manual = rings.iter().filter(|r| r.manual.is_some()).count();
    rings.truncate(n_manual + cfg.top_n * 3);
    let arcs: Vec<Vec<arcs::Arc>> = rings
        .par_iter()
        .map(|r| {
            // Walking mode: walks out and home that keep off this ring.
            let walks = cfg.is_direct().then(|| {
                let radius = r.stats.r_max + APPROACH_MARGIN_M;
                crate::base::BaseTree::around_ring(&b.graph, cfg.travel, cfg.climb_cost(), &r.edges, radius)
            });
            let ends = match &walks {
                None => arcs::Ends::Transit(&b.transit),
                Some(w) => arcs::Ends::Walking(&b.graph, w),
            };
            let marks = RingMarks::new(r, &b.graph, &b.places, &b.heads);
            let mut arcs = arcs::split(r, &ends, &marks, cfg);
            for a in &mut arcs {
                a.shade = b.shade.fraction(&a.pts);
                a.walk = arcs::walk_stretches(r, &b.graph, &b.places, a.from, a.length);
                let describe = |node| {
                    let parts = walks.as_ref().map_or(vec![], |w| w.path_parts(&b.graph, node));
                    arcs::stretches(&b.graph, &b.places, &parts)
                };
                if let Some(w) = &mut a.walk_out {
                    w.walk = describe(w.node);
                }
                if let Some(w) = &mut a.walk_back {
                    w.walk = describe(w.node);
                    w.walk.reverse(); // walked from the ring home
                }
            }
            arcs
        })
        .collect();
    let (mut rings, mut arcs) = demote_unreachable(rings, arcs, cfg);
    rings.truncate(n_manual + cfg.top_n);
    arcs.truncate(n_manual + cfg.top_n);
    let valid = rings.iter().filter(|r| r.ok()).count();
    // Ring shade over the walked parts only (not water crossings).
    for (r, arcs) in rings.iter_mut().zip(&arcs) {
        let walked: f64 = arcs.iter().map(|a| a.length).sum();
        r.stats.shade_frac = arcs.iter().map(|a| a.shade * a.length).sum::<f64>() / walked.max(1.0);
        let len: f64 = r.edges.iter().map(|&(ei, _)| b.graph.edges[ei].len).sum();
        r.stats.life_frac =
            r.edges.iter().map(|&(ei, _)| b.feel.life[ei] as f64 * b.graph.edges[ei].len).sum::<f64>() / len.max(1.0);
    }
    Plan { rings, arcs, candidates, valid }
}

/// Rings with fewer than this share of their outings reachable are outside the limits.
const MIN_REACHABLE: f64 = 0.7;

/// Checks what is only known once arcs are cut (outings reachable), marks
/// rings that fail as outside the limits, and puts rings back in order:
/// valid first, then by score.
fn demote_unreachable(rings: Vec<Ring>, arcs: Vec<Vec<Arc>>, cfg: &Config) -> (Vec<Ring>, Vec<Vec<Arc>>) {
    let mut both: Vec<(Ring, Vec<Arc>)> = rings.into_iter().zip(arcs).collect();
    for (r, a) in &mut both {
        let share = a.iter().filter(|x| x.reachable(cfg)).count() as f64 / a.len().max(1) as f64;
        if share < MIN_REACHABLE {
            r.problems.push(format!("only {:.0}% of outings reachable", share * 100.0));
        }
        r.stats.score += 1.0 - share;
    }
    both.sort_by(|x, y| {
        // Plans edited by hand first, in the order made; then valid plans, best first.
        x.0.manual
            .is_none()
            .cmp(&y.0.manual.is_none())
            .then(x.0.manual.cmp(&y.0.manual))
            .then(y.0.ok().cmp(&x.0.ok()))
            .then(x.0.stats.score.total_cmp(&y.0.stats.score))
    });
    both.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::{build_key, search_key};
    use crate::config::Config;

    #[test]
    fn cache_keys_track_the_right_settings() {
        let base = Config::default();
        let changed = |f: fn(&mut Config)| {
            let mut c = base.clone();
            f(&mut c);
            (build_key(&c) != build_key(&base), search_key(&c) != search_key(&base))
        };
        // Places, shade and street life are built over the whole map.
        assert_eq!(changed(|c| c.extract_radius_km = 20.0), (true, true));
        assert_eq!(changed(|c| c.access_walk_m = 500.0), (true, true));
        // Planning settings don't rebuild the graph.
        assert_eq!(changed(|c| c.road_avoidance = 5.0), (false, true));
        assert_eq!(changed(|c| c.trip.days = 3.0), (false, true));
        assert_eq!(changed(|c| c.centroid_offset_km = 1.0), (false, true));
        // Limits only re-rank.
        assert_eq!(changed(|c| c.max_road_fraction = 0.3), (false, false));
        assert_eq!(changed(|c| c.top_n = 3), (false, false));
    }
}
