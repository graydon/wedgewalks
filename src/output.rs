//! GeoJSON and text output.

use crate::arcs::Arc;
use crate::config::Config;
use crate::graph::{Graph, Proj, Xy, norm};
use crate::osm::Kind;
use crate::pipeline::{Built, Plan};
use crate::rings::Ring;
use crate::transit::{Access, Transit};
use geojson::{Feature, FeatureCollection, Geometry, JsonObject, Value};
use serde_json::json;
use std::fmt::Write as _;
use std::path::Path;

fn pos(proj: &Proj, p: Xy) -> Vec<f64> {
    let (lon, lat) = proj.lonlat(p);
    vec![(lon * 1e6).round() / 1e6, (lat * 1e6).round() / 1e6]
}

fn feature(value: Value, props: serde_json::Value) -> Feature {
    let properties: Option<JsonObject> = match props {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    };
    Feature { geometry: Some(Geometry::new(value)), properties, ..Default::default() }
}

fn line(proj: &Proj, pts: &[Xy], props: serde_json::Value) -> Feature {
    feature(Value::LineString(pts.iter().map(|&p| pos(proj, p)).collect()), props)
}

fn point(proj: &Proj, p: Xy, props: serde_json::Value) -> Feature {
    feature(Value::Point(pos(proj, p)), props)
}

fn write(path: &Path, features: Vec<Feature>) -> Result<(), String> {
    let fc = FeatureCollection { bbox: None, features, foreign_members: None };
    std::fs::write(path, fc.to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::CarFree => "car_free",
        Kind::Road => "road",
    }
}

pub fn write_graph(path: &Path, g: &Graph, proj: &Proj, radius_m: f64) -> Result<(), String> {
    let features = g
        .edges
        .iter()
        .filter(|e| norm(e.pts[0]) <= radius_m)
        .map(|e| {
            line(
                proj,
                &e.pts,
                json!({
                    "kind": kind_str(e.kind),
                    "on_route": e.on_route,
                    "steps": e.steps,
                    "way_id": e.way_id,
                    "length_m": e.len.round(),
                }),
            )
        })
        .collect();
    write(path, features)
}

pub fn write_rings(path: &Path, rings: &[&Ring], proj: &Proj) -> Result<(), String> {
    let mut features = vec![point(proj, [0.0, 0.0], json!({"name": "center"}))];
    for (i, r) in rings.iter().enumerate() {
        let s = &r.stats;
        features.push(line(
            proj,
            &r.pts,
            json!({
                "rank": i + 1,
                "ok": r.ok(),
                "problems": r.problems.join("; "),
                "length_km": (s.length / 100.0).round() / 10.0,
                "road_fraction": (s.road_frac * 1000.0).round() / 1000.0,
                "max_road_gap_m": s.max_gap.round(),
                "radius_mean_km": (s.r_mean / 100.0).round() / 10.0,
                "score": (s.score * 1000.0).round() / 1000.0,
                "shade_fraction": (s.shade_frac * 100.0).round() / 100.0,
                "steps": s.steps,
                "search": r.search,
            }),
        ));
    }
    write(path, features)
}

fn access_props(a: &Option<Access>, t: &Transit) -> serde_json::Value {
    match a {
        Some(a) => json!({
            "stop": t.stops[a.stop].name,
            "route": t.route_names(&a.legs),
            "transfers": a.legs.len() - 1,
            "walk_m": a.walk_m.round(),
            "center_stop": t.stops[a.center_stop].name,
        }),
        None => json!(null),
    }
}

pub fn write_arcs(path: &Path, plans: &[(usize, Vec<Arc>)], t: &Transit, proj: &Proj) -> Result<(), String> {
    let mut features = vec![];
    for (rank, arcs) in plans {
        for (i, a) in arcs.iter().enumerate() {
            features.push(line(
                proj,
                &a.pts,
                json!({
                    "ring": rank,
                    "arc": i + 1,
                    "length_km": (a.length / 100.0).round() / 10.0,
                    "road_m": a.road_len.round(),
                    "shade_fraction": (a.shade * 100.0).round() / 100.0,
                    "start": access_props(&a.start, t),
                    "end": access_props(&a.end, t),
                }),
            ));
            if let Some(acc) = &a.start {
                features.push(point(
                    proj,
                    t.stops[acc.stop].xy,
                    json!({
                        "ring": rank, "arc": i + 1, "stop": t.stops[acc.stop].name, "route": t.route_names(&acc.legs),
                    }),
                ));
            }
        }
    }
    write(path, features)
}

fn describe(a: &Option<Access>, t: &Transit) -> String {
    match a {
        Some(a) => format!(
            "{} ({:.0} m walk) via {}{} ({:.0} m from base)",
            t.stops[a.stop].name,
            a.walk_m,
            t.route_names(&a.legs),
            if a.legs.len() > 1 { ", 1 change" } else { "" },
            a.center_walk_m
        ),
        None => "NOT REACHABLE from the center with at most one change".to_string(),
    }
}

/// The text report: graph and transit summary, each ring with its arcs,
/// and an outing-by-outing itinerary for the best ring.
pub fn report(city: &str, b: &Built, plan: &Plan, cfg: &Config) -> String {
    let mut o = header(city, b, plan);
    for (i, (r, arcs)) in plan.rings.iter().zip(&plan.arcs).enumerate() {
        ring_section(&mut o, i + 1, r, arcs, b, cfg);
    }
    if let Some(arcs) = plan.arcs.first() {
        itinerary(&mut o, arcs, &b.transit);
    }
    o
}

fn header(city: &str, b: &Built, plan: &Plan) -> String {
    let mut o = String::new();
    let total = |k: Kind| b.graph.edges.iter().filter(|e| e.kind == k).map(|e| e.len).sum::<f64>() / 1000.0;
    let _ = writeln!(o, "WEDGEWALKS report: {city} (center {:.5}, {:.5})", b.lat, b.lon);
    let _ = writeln!(
        o,
        "graph: {} nodes, {} edges, {:.0} km car-free, {:.0} km road",
        b.graph.nodes.len(),
        b.graph.edges.len(),
        total(Kind::CarFree),
        total(Kind::Road)
    );
    let t = &b.transit;
    let _ = writeln!(
        o,
        "transit: {} stops, {} routes, {} routes stop near the center",
        t.stops.len(),
        t.labels.len(),
        t.center_routes
    );
    let _ = writeln!(o, "rings: {} distinct candidates, {} pass all limits\n", plan.candidates, plan.valid);
    o
}

fn ring_section(o: &mut String, rank: usize, r: &Ring, arcs: &[Arc], b: &Built, cfg: &Config) {
    let st = &r.stats;
    let rejected = if r.ok() { String::new() } else { format!(" [REJECTED: {}]", r.problems.join("; ")) };
    let gap_at = if st.max_gap > 0.0 {
        let (lon, lat) = b.proj.lonlat(st.gap_at);
        format!(" (at {lat:.5},{lon:.5})")
    } else {
        String::new()
    };
    let _ = writeln!(
        o,
        "Ring {rank}{rejected}: {:.1} km, road {:.0}% ({:.1} km), longest road gap {:.0} m{gap_at}, radius {:.1}-{:.1} km \
         (mean {:.1}), beside traffic {:.0}%, by water {:.0}%, doubles back {:.1} km, on walking routes {:.0}%, \
         shaded {:.0}%, street life {:.0}%, {:.1} sharp turns/km, {} flights of steps, climb {:.0} m",
        st.length / 1000.0,
        st.road_frac * 100.0,
        st.road_len / 1000.0,
        st.max_gap,
        st.r_min / 1000.0,
        st.r_max / 1000.0,
        st.r_mean / 1000.0,
        st.beside_frac * 100.0,
        st.waterside_frac * 100.0,
        st.double_back / 1000.0,
        st.route_frac * 100.0,
        st.shade_frac * 100.0,
        st.life_frac * 100.0,
        st.turns_per_km,
        st.steps,
        st.climb,
    );
    if r.is_open() {
        let _ = writeln!(o, "  a chain from shore to shore, {:.0}° round the centre", r.span * 360.0);
    }
    let _ = writeln!(o, "  found by: {}", r.search);
    let reachable = arcs.iter().filter(|a| a.reachable(cfg)).count();
    let _ = writeln!(o, "  {} arcs, {} reachable at both ends", arcs.len(), reachable);
    for (i, a) in arcs.iter().enumerate() {
        let _ = writeln!(
            o,
            "  arc {}: {:.1} km, {:.0} m road, {:.0}% shaded, climb {:.0} m (day {:.0} m{})",
            i + 1,
            a.length / 1000.0,
            a.road_len,
            a.shade * 100.0,
            a.up,
            a.day_climb(),
            if a.too_steep(cfg) { ", over the limit" } else { "" }
        );
        if let Some(ends) = a.ends_text() {
            let _ = writeln!(o, "    {ends}");
        }
        let _ = writeln!(o, "    walk:  {}", crate::arcs::describe_walk(&a.walk, 8));
        match a.day_summary() {
            Some(day) => {
                let _ = writeln!(o, "    {day}");
            }
            None => {
                let _ = writeln!(o, "    start: {}", describe(&a.start, &b.transit));
                let _ = writeln!(o, "    end:   {}", describe(&a.end, &b.transit));
            }
        }
    }
    let _ = writeln!(o);
}

fn itinerary(o: &mut String, arcs: &[Arc], t: &Transit) {
    let _ = writeln!(o, "Itinerary for ring 1 (one outing per good day; rest as needed between)");
    for (i, a) in arcs.iter().enumerate() {
        if let (Some(day), Some(out), Some(back)) = (a.day_summary(), &a.walk_out, &a.walk_back) {
            let _ = writeln!(o, "  Outing {}: {day}", i + 1);
            if let Some(ends) = a.ends_text() {
                let _ = writeln!(o, "    {ends}");
            }
            let _ = writeln!(o, "    out:  {}", crate::arcs::describe_walk(&out.walk, 8));
            let _ = writeln!(o, "    ring: {}", crate::arcs::describe_walk(&a.walk, 8));
            let _ = writeln!(o, "    home: {}", crate::arcs::describe_walk(&back.walk, 8));
        } else {
            let _ = writeln!(o, "  Outing {}: walk {:.1} km", i + 1, a.length / 1000.0);
            if let Some(ends) = a.ends_text() {
                let _ = writeln!(o, "    {ends}");
            }
            let _ = writeln!(o, "    go:     {}", t.trip_text(a.start.as_ref(), true));
            let _ = writeln!(o, "    return: {}", t.trip_text(a.end.as_ref(), false));
        }
    }
}

/// Writes graph, rings, arcs and report for a plan into `dir`, plus GPX
/// tracks and a printable itinerary for ring `ring` (0-based); returns the
/// report text.
pub fn write_all(dir: &Path, city: &str, b: &Built, plan: &Plan, cfg: &Config, ring: usize) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_graph(&dir.join("graph.geojson"), &b.graph, &b.proj, cfg.surroundings_m())?;
    let rings: Vec<&Ring> = plan.rings.iter().collect();
    let plans: Vec<(usize, Vec<Arc>)> = plan.arcs.iter().cloned().enumerate().map(|(i, a)| (i + 1, a)).collect();
    write_rings(&dir.join("rings.geojson"), &rings, &b.proj)?;
    write_arcs(&dir.join("arcs.geojson"), &plans, &b.transit, &b.proj)?;
    let text = report(city, b, plan, cfg);
    std::fs::write(dir.join("report.txt"), &text).map_err(|e| e.to_string())?;
    crate::export::write(dir, city, b, plan, ring, cfg.travel)?;
    Ok(text)
}
