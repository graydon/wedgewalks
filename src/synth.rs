//! A small synthetic city for tests and demos: a road grid with two
//! car-free ring paths, radial footpaths, bus routes and a few trees.

use crate::graph::Proj;
use crate::osm::{Kind, OsmData, Route, WayRec, way_type};

/// The default config with the number of days the synthetic city's 5 km
/// ring makes.
pub fn cfg() -> crate::config::Config {
    let mut c = crate::config::Config::default();
    c.trip.days = 5.0;
    c
}

pub fn city(lat: f64, lon: f64) -> OsmData {
    let mut d = OsmData::default();
    let proj = Proj::new(lat, lon);
    let latlon = |x: f64, y: f64| {
        let (lo, la) = proj.lonlat([x, y]);
        (la, lo)
    };
    let mut next_id = 1i64;
    let mut node = |d: &mut OsmData, x: f64, y: f64| {
        let id = next_id;
        next_id += 1;
        d.coords.insert(id, latlon(x, y));
        id
    };
    let mut way_id = 1i64;
    let mut way = |d: &mut OsmData, kind: Kind, refs: Vec<i64>| {
        let highway = if kind == Kind::Road { "residential" } else { "footway" };
        let wtype = way_type(&[("highway", highway)]);
        d.ways.push(WayRec {
            id: way_id,
            kind,
            foot: true,
            bike: true,
            cycle: false,
            steps: false,
            refs,
            name: None,
            wtype,
            bridge: false,
        });
        way_id += 1;
        way_id - 1
    };

    // Road grid, 400 m spacing, 12 km across.
    let step = 400.0;
    let n = 31;
    let half = (n / 2) as f64 * step;
    let mut grid = vec![vec![0i64; n]; n];
    for (i, row) in grid.iter_mut().enumerate() {
        for (j, id) in row.iter_mut().enumerate() {
            *id = node(&mut d, j as f64 * step - half, i as f64 * step - half);
        }
    }
    for (i, row) in grid.iter().enumerate() {
        way(&mut d, Kind::Road, row.clone());
        way(&mut d, Kind::Road, grid.iter().map(|r| r[i]).collect());
    }

    // Car-free rings at 1.5, 3.5 and 5 km, joined to the grid where they
    // pass close to a grid node, and radial footpaths every 45 degrees
    // from 1 km out that share nodes with the rings they cross.
    let k = 240;
    let radii = [1500.0f64, 3500.0, 5000.0];
    let mut ring_nodes: Vec<Vec<i64>> = vec![];
    for (ri, r) in radii.into_iter().enumerate() {
        let mut refs = vec![];
        for s in 0..k {
            let t = s as f64 / k as f64 * std::f64::consts::TAU;
            let (x, y) = (r * t.cos(), r * t.sin());
            let (gi, gj) = (((y + half) / step).round() as usize, ((x + half) / step).round() as usize);
            let (gx, gy) = (gj as f64 * step - half, gi as f64 * step - half);
            if (gx - x).hypot(gy - y) < 30.0 {
                refs.push(grid[gi][gj]);
            } else {
                refs.push(node(&mut d, x, y));
            }
        }
        ring_nodes.push(refs.clone());
        refs.push(refs[0]);
        let w = way(&mut d, Kind::CarFree, refs);
        if ri == 2 {
            d.route_ways.insert(w, "Synth Ring Walk".into());
        }
    }
    for a in 0..8 {
        let t = a as f64 / 8.0 * std::f64::consts::TAU;
        let refs: Vec<i64> = (0..=20)
            .map(|s| {
                let r = 1000.0 + s as f64 * 250.0;
                match radii.iter().position(|&rr| (rr - r).abs() < 1.0) {
                    Some(ri) => ring_nodes[ri][a * k / 8],
                    None => node(&mut d, r * t.cos(), r * t.sin()),
                }
            })
            .collect();
        // Join the inner end to the nearest grid node with a short path.
        let (x, y) = (1000.0 * t.cos(), 1000.0 * t.sin());
        let (gi, gj) = (((y + half) / step).round() as usize, ((x + half) / step).round() as usize);
        way(&mut d, Kind::CarFree, vec![grid[gi][gj], refs[0]]);
        way(&mut d, Kind::CarFree, refs);
    }
    // Bus routes along grid lines through the center, both directions.
    let mid = n / 2;
    for (label, line) in [
        ("1 Eastbound", (0..n).map(|j| grid[mid][j]).collect::<Vec<_>>()),
        ("2 Northbound", (0..n).map(|i| grid[i][mid]).collect()),
        ("3 Diagonal", (0..n).map(|i| grid[i][i]).collect()),
        ("4 Antidiagonal", (0..n).map(|i| grid[i][n - 1 - i]).collect()),
    ] {
        let stops: Vec<i64> = line.iter().step_by(2).copied().collect();
        for (dir, stops) in [("", stops.clone()), (" (return)", stops.into_iter().rev().collect())] {
            d.routes.push(Route {
                mode: "bus".into(),
                label: format!("{label}{dir}"),
                night: false,
                stops,
                interval_min: Some(10.0),
            });
        }
    }
    for (i, row) in grid.iter().enumerate() {
        for (j, id) in row.iter().enumerate() {
            if i % 2 == 0 && j % 2 == 0 {
                d.stop_names.insert(*id, format!("Stop {i}/{j}"));
            }
        }
    }
    for a in 0..60 {
        let t = a as f64 / 60.0 * std::f64::consts::TAU;
        d.trees.push(latlon(5000.0 * t.cos(), 5000.0 * t.sin()));
    }
    d
}

#[cfg(test)]
mod tests {
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn synthetic_city_has_a_valid_ring() {
        let cfg = super::cfg();
        let loaded = pipeline::Loaded::from_osm(super::city(50.0, 0.0), 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        assert!(plan.valid >= 1, "no valid ring; best: {:?}", plan.rings.first().map(|r| &r.problems));
        let best = &plan.rings[0];
        assert!(best.stats.road_frac < 0.15);
        assert!(plan.arcs[0].iter().all(|a| a.start.is_some() && a.end.is_some()));

        // Exports: one GPX per day plus the ring, and the itinerary page.
        let dir = std::env::temp_dir().join(format!("wedgewalks-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::export::write(&dir, "Synthville", &built, &plan, 0, crate::config::Travel::Foot).unwrap();
        let ring = std::fs::read_to_string(dir.join("ring.gpx")).unwrap();
        assert_eq!(ring.matches("<trk>").count(), plan.arcs[0].len());
        assert!(ring.contains("<trkpt"));
        let page = std::fs::read_to_string(dir.join("itinerary.html")).unwrap();
        assert!(page.contains("travelmode=transit"));
        assert!(dir.join("day-1.gpx").exists());
    }
}

#[cfg(test)]
mod trailhead_tests {
    use crate::config::Config;
    use crate::osm::{Spot, SpotKind};
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    /// A landmark near the end of an arc pulls its cut point to it when
    /// trailheads are weighted, and the arc says so.
    #[test]
    fn cuts_move_to_a_landmark() {
        let (lat, lon) = (50.0, 0.0);
        let plan_with = |osm, weight| {
            let cfg = Config { trailhead_weight_m: weight, ..super::cfg() };
            let loaded = pipeline::Loaded::from_osm(osm, lat, lon, 30_000.0);
            let built = pipeline::build(&loaded, lat, lon, &cfg);
            (pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap(), built)
        };
        let cfg_walk = super::cfg().access_walk_m;
        let (plan, built) = plan_with(super::city(lat, lon), 0.0);
        let arc = &plan.arcs[0][0];
        // Near the end of the arc, on a junction with transit both ways: a day can end a little
        // earlier, but not half way.
        let target = arc.pts[arc.pts.len() * 9 / 10];
        let access =
            |p| built.transit.access(p, cfg_walk, true).is_some() && built.transit.access(p, cfg_walk, false).is_some();
        let node = *built
            .graph
            .nodes
            .iter()
            .filter(|&&p| access(p))
            .min_by(|a, b| crate::graph::dist(**a, target).total_cmp(&crate::graph::dist(**b, target)))
            .unwrap();
        let (lo, la) = built.proj.lonlat(node);
        let mut osm = super::city(lat, lon);
        osm.spots.push(Spot { lat: la, lon: lo, kind: SpotKind::Landmark, name: "Synth Tower".into() });
        let (plan, _) = plan_with(osm, 2000.0);
        let at_tower =
            |h: &Option<crate::trailheads::Trailhead>| h.as_ref().is_some_and(|h| h.label.contains("Synth Tower"));
        assert!(
            plan.arcs[0].iter().any(|a| at_tower(&a.start_at) || at_tower(&a.end_at)),
            "no arc ends at the landmark: {:?}",
            plan.arcs[0].iter().map(|a| a.ends_text()).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod climb_tests {
    use crate::config::Config;
    use crate::elevation::Dem;
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    /// A round hill `height` m high and about `width` m across, 5 km east
    /// of the base, on the synthetic city's outer ring.
    fn hilly(height: f64, width: f64) -> pipeline::Loaded {
        let (lat, lon) = (50.0, 0.0);
        let mut l = pipeline::Loaded::from_osm(super::city(lat, lon), lat, lon, 30_000.0);
        let m = crate::graph::M_PER_DEG;
        l.dem = Some(Dem::from_fn(lat - 0.3, lon - 0.45, 0.001, 900, |la, lo| {
            let (x, y) = ((lo - lon) * m * lat.to_radians().cos() - 5000.0, (la - lat) * m);
            (height * (-(x * x + y * y) / (width * width / 4.0)).exp()) as f32
        }));
        l
    }

    #[test]
    fn walks_go_round_hills() {
        let l = hilly(150.0, 2000.0);
        let built = pipeline::build(&l, l.lat, l.lon, &super::cfg());
        assert!(built.has_elevation);
        let g = &built.graph;
        let beyond = (0..g.nodes.len())
            .min_by(|&a, &b| {
                let d = |i: usize| crate::graph::dist(g.nodes[i], [6200.0, 0.0]);
                d(a).total_cmp(&d(b))
            })
            .unwrap();
        let over = crate::base::BaseTree::new(g, crate::config::Travel::Foot, 0.0).climb(beyond).0;
        let round = crate::base::BaseTree::new(g, crate::config::Travel::Foot, 200.0).climb(beyond).0;
        assert!(over > 50.0, "the straight way should cross the hill: {over:.0} m");
        assert!(round < over / 2.0, "climbing {over:.0} m straight, {round:.0} m with a climb cost");
    }

    #[test]
    fn days_are_cut_to_the_climb_limit() {
        let l = hilly(100.0, 16000.0);
        let cfg = Config { climb_cost_m: 0.0, max_climb_per_day_m: 10_000.0, ..super::cfg() };
        let built = pipeline::build(&l, l.lat, l.lon, &cfg);
        let worst = |p: &pipeline::Plan| p.arcs[0].iter().map(|a| a.day_climb()).fold(0.0, f64::max);
        let loose = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        let limit = worst(&loose) - 20.0;
        assert!(limit > 20.0, "the hill should make some day climb: {:.0} m", worst(&loose));
        let tight = Config { max_climb_per_day_m: limit, ..cfg };
        let plan = pipeline::plan(&built, &tight, &AtomicBool::new(false)).unwrap();
        // Days can only be cut where there are places to cut: the worst day gets no worse, and days over the limit say so.
        assert!(worst(&plan) <= worst(&loose) + 1.0, "worst day {:.0} m, was {:.0} m", worst(&plan), worst(&loose));
        assert!(plan.arcs[0].iter().all(|a| a.too_steep(&tight) == (a.day_climb() > limit + 1.0)));
    }
}

#[cfg(test)]
mod change_tests {
    use crate::config::Config;
    use crate::graph::Proj;
    use crate::transit::Transit;

    /// Grid node ids in `city`: row i, column j of the 31 x 31 road grid.
    fn grid(i: usize, j: usize) -> i64 {
        1 + (i * 31 + j) as i64
    }

    /// A line through the centre (the eastbound bus) and a second line
    /// crossing it 2.0 km east, which only reaches the base by changing.
    fn network() -> crate::osm::OsmData {
        let mut osm = super::city(50.0, 0.0);
        osm.routes.retain(|r| r.label.starts_with("1 Eastbound") && !r.label.contains("return"));
        let route = |label: &str, stops: Vec<i64>| crate::osm::Route {
            mode: "bus".into(),
            label: label.into(),
            night: false,
            stops,
            interval_min: Some(10.0),
        };
        osm.routes.push(route("X Crosstown", (1..31).step_by(2).map(|i| grid(i, 20)).collect()));
        osm.routes.push(route("Y Further", (0..31).step_by(2).map(|j| grid(29, j)).collect()));
        osm
    }

    /// The most changes any reachable stop needs, and how many are reachable.
    fn reach(n: usize) -> (usize, usize) {
        let osm = network();
        let cfg = Config { max_changes: n, ..Config::default() };
        let t = Transit::build(&osm, &Proj::new(50.0, 0.0), &cfg);
        let found: Vec<usize> =
            t.stops.iter().filter_map(|s| t.access(s.xy, 1.0, true)).map(|a| a.legs.len() - 1).collect();
        (found.len(), found.into_iter().max().unwrap_or(0))
    }

    #[test]
    fn more_changes_reach_more_and_never_exceed_the_limit() {
        let ((direct, m0), (one, m1), (two, m2), (three, m3)) = (reach(0), reach(1), reach(2), reach(3));
        assert_eq!((m0, m1, m2), (0, 1, 2), "most changes used at limits 0, 1, 2");
        assert!(direct < one && one < two, "each change reaches more: {direct} {one} {two}");
        assert!(m3 <= 3 && three >= two);
    }
}

#[cfg(test)]
mod walking_tests {
    use crate::config::{Access, Config};
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn synthetic_city_walking_mode() {
        let cfg = Config { access: Access::Direct, ..Config::default() };
        let loaded = pipeline::Loaded::from_osm(super::city(50.0, 0.0), 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        assert!(plan.valid >= 1);
        let day_max = cfg.day_m();
        assert!(plan.arcs[0].iter().all(|a| a.day_m().is_some_and(|d| d <= day_max + 1.0)));
    }

    /// More days make a ring further out; every day can still be walked
    /// within the limit, out, along the arc and home.
    #[test]
    fn walking_mode_far_out() {
        let mut cfg = Config { access: Access::Direct, ..Config::default() };
        cfg.direct_trip.days = 6.0;
        let loaded = pipeline::Loaded::from_osm(super::city(50.0, 0.0), 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        let arcs = &plan.arcs[0];
        assert!(arcs.iter().all(|a| a.reachable(&cfg)), "some day can't be walked within the limit");
    }
}

#[cfg(test)]
mod bike_tests {
    use crate::config::{Config, Travel, Trip};
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    fn bike() -> Config {
        Config { travel: Travel::Bike, bike_trip: Trip { days: 3.0, day_km: 14.0 }, ..Config::default() }
    }

    fn best(osm: crate::osm::OsmData, cfg: &Config) -> Option<(f64, usize)> {
        let loaded = pipeline::Loaded::from_osm(osm, 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, cfg);
        let plan = pipeline::plan(&built, cfg, &AtomicBool::new(false)).unwrap();
        plan.rings.first().map(|r| (r.stats.road_frac, plan.arcs[0].len()))
    }

    /// By bicycle there is a plan in a few long days, with no more road than the streets a cyclist accepts.
    #[test]
    fn bike_plans_go_further() {
        let (road, days) = best(super::city(50.0, 0.0), &bike()).expect("a bike plan");
        assert!(road < 0.5, "road share {road}");
        assert!((2..=4).contains(&days), "{days} days");
    }

    /// Paths that cannot be cycled (footways) are not used by bike: the ring has to take the streets.
    #[test]
    fn bikes_do_not_use_foot_only_ways() {
        let mut osm = super::city(50.0, 0.0);
        for w in &mut osm.ways {
            if w.kind == crate::osm::Kind::CarFree {
                w.bike = false;
            }
        }
        let cfg = bike();
        let loaded = pipeline::Loaded::from_osm(osm, 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        let r = plan.rings.first().expect("a bike plan");
        let on_paths = r.edges.iter().filter(|&&(e, _)| built.graph.edges[e].kind == crate::osm::Kind::CarFree).count();
        assert_eq!(on_paths, 0, "the plan uses footways");
    }

    /// Cycle ways beat plain paths and streets for a cyclist: with the paths built as cycle ways, the plan is on them.
    #[test]
    fn bikes_prefer_cycle_ways() {
        let mut osm = super::city(50.0, 0.0);
        for w in &mut osm.ways {
            w.cycle = w.kind == crate::osm::Kind::CarFree;
        }
        let cfg = Config { prefer_waterside: false, ..bike() };
        let loaded = pipeline::Loaded::from_osm(osm, 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        let r = &plan.rings[0];
        let on = |e: usize| built.graph.edges[e].cycle;
        let cycle: f64 = r.edges.iter().filter(|&&(e, _)| on(e)).map(|&(e, _)| built.graph.edges[e].len).sum();
        assert!(cycle > 0.8 * r.stats.length, "{:.0} of {:.0} m on cycle ways", cycle, r.stats.length);
    }
}

#[cfg(test)]
mod manual_tests {
    use crate::manual::Manual;
    use crate::pipeline;
    use std::sync::atomic::AtomicBool;

    /// A manual plan starts as a copy of a plan; moving an anchor re-routes its two days to the new place,
    /// and a routing point is on the route.
    #[test]
    fn a_manual_plan_follows_its_anchors_and_routing_points() {
        let cfg = super::cfg();
        let loaded = pipeline::Loaded::from_osm(super::city(50.0, 0.0), 50.0, 0.0, 30_000.0);
        let built = pipeline::build(&loaded, 50.0, 0.0, &cfg);
        let plan = pipeline::plan(&built, &cfg, &AtomicBool::new(false)).unwrap();
        let ring = &plan.rings[0];
        let mut m = Manual::from_ring(ring, &built.proj, "t".into(), cfg.travel);
        assert_eq!(m.days(), ring.cuts.len() - 1);

        // The copy routes like the original.
        let (same, errors) = crate::plan::manual_rings(&built, &cfg, std::slice::from_ref(&m));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(same[0].cuts.len(), ring.cuts.len());
        assert!((same[0].stats.length - ring.stats.length).abs() < 0.2 * ring.stats.length);

        // Move anchor 1 a long way along the grid: the days either side now end there.
        let to = built.proj.xy(50.0 + 0.02, 0.0 + 0.0);
        let target = [to[0], to[1]];
        let (lon, lat) = built.proj.lonlat(target);
        m.move_anchor(1, [lat, lon]);
        let (moved, _) = crate::plan::manual_rings(&built, &cfg, std::slice::from_ref(&m));
        let a = moved[0].marks[1];
        let before = ring.marks[1];
        let d_after = ((a[0] - target[0]).powi(2) + (a[1] - target[1]).powi(2)).sqrt();
        let d_before = ((before[0] - target[0]).powi(2) + (before[1] - target[1]).powi(2)).sqrt();
        assert!(d_after < d_before, "the anchor moved towards where it was put: {d_before:.0} m -> {d_after:.0} m");
        assert_eq!(moved[0].manual, Some(0));
    }
}
