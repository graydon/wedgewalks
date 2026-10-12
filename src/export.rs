//! Exports for taking a plan out on the walk: GPX tracks (for Garmin,
//! OsmAnd, Komoot, Gaia, Google My Maps, ...) and a printable itinerary
//! page with Google / Apple Maps links that plan the transit live.

use crate::arcs::{self, Arc};
use crate::config::Travel;
use crate::graph::{Proj, Xy};
use crate::pipeline::{Built, Plan};
use std::fmt::Write as _;
use std::path::Path;

/// Google Maps directions take at most this many waypoints.
const MAX_WAYPOINTS: usize = 8;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn latlon(proj: &Proj, p: Xy) -> (f64, f64) {
    let (lon, lat) = proj.lonlat(p);
    (lat, lon)
}

fn ll(proj: &Proj, p: Xy) -> String {
    let (lat, lon) = latlon(proj, p);
    format!("{lat:.6},{lon:.6}")
}

/// Google Maps directions; `mode` is "transit" or "walking".
fn google_url(proj: &Proj, from: Xy, to: Xy, via: &[Xy], mode: &str) -> String {
    let mut url = format!(
        "https://www.google.com/maps/dir/?api=1&origin={}&destination={}&travelmode={mode}",
        ll(proj, from),
        ll(proj, to)
    );
    if !via.is_empty() {
        url.push_str("&waypoints=");
        url.push_str(&via.iter().map(|&p| ll(proj, p)).collect::<Vec<_>>().join("%7C"));
    }
    url
}

/// Apple Maps directions; `flag` is "r" (transit) or "w" (walking).
fn apple_url(proj: &Proj, from: Xy, to: Xy, flag: &str) -> String {
    format!("https://maps.apple.com/?saddr={}&daddr={}&dirflg={flag}", ll(proj, from), ll(proj, to))
}

/// Up to `MAX_WAYPOINTS` points spread evenly along the inside of a line.
fn waypoints(pts: &[Xy]) -> Vec<Xy> {
    if pts.len() < 3 {
        return vec![];
    }
    let n = MAX_WAYPOINTS.min(pts.len() - 2);
    (1..=n).map(|k| pts[k * (pts.len() - 1) / (n + 1)]).collect()
}

fn gpx_wpt(o: &mut String, proj: &Proj, p: Xy, name: &str) {
    let (lat, lon) = latlon(proj, p);
    let _ = writeln!(o, "  <wpt lat=\"{lat:.6}\" lon=\"{lon:.6}\"><name>{}</name></wpt>", esc(name));
}

fn gpx_segment(o: &mut String, proj: &Proj, pts: &[Xy]) {
    let _ = writeln!(o, "    <trkseg>");
    for &p in pts {
        let (lat, lon) = latlon(proj, p);
        let _ = writeln!(o, "      <trkpt lat=\"{lat:.6}\" lon=\"{lon:.6}\"/>");
    }
    let _ = writeln!(o, "    </trkseg>");
}

/// One outing's track (walk out, arc, walk home) and waypoints.
fn gpx_day(o: &mut String, b: &Built, a: &Arc, day: usize) {
    let p = &b.proj;
    let start = a.pts[0];
    let end = *a.pts.last().unwrap();
    gpx_wpt(o, p, start, &format!("Day {day} start"));
    gpx_wpt(o, p, end, &format!("Day {day} end"));
    for (acc, what) in [(&a.start, "get off"), (&a.end, "get on")] {
        if let Some(acc) = acc {
            gpx_wpt(
                o,
                p,
                b.transit.stops[acc.stop].xy,
                &format!("Day {day}: {what} at {}", b.transit.stops[acc.stop].name),
            );
        }
    }
    let _ = writeln!(o, "  <trk><name>Day {day}: {:.1} km</name>", a.length / 1000.0);
    if let Some(w) = &a.walk_out {
        gpx_segment(o, p, &w.pts);
    }
    gpx_segment(o, p, &a.pts);
    if let Some(w) = &a.walk_back {
        let back: Vec<Xy> = w.pts.iter().rev().copied().collect();
        gpx_segment(o, p, &back);
    }
    let _ = writeln!(o, "  </trk>");
}

fn gpx(name: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"wedgewalks\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n  <metadata><name>{}</name></metadata>\n{body}</gpx>\n",
        esc(name)
    )
}

/// Writes `ring.gpx` (every day), `day-N.gpx` (one each) and
/// `itinerary.html` for ring number `ring` (0-based) into `dir`.
pub fn write(dir: &Path, city: &str, b: &Built, plan: &Plan, ring: usize, travel: Travel) -> Result<(), String> {
    // Google's name for how you travel; Apple Maps has no cycling flag, so it shows walking directions.
    let own = if travel == Travel::Bike { "bicycling" } else { "walking" };
    let (Some(r), Some(arcs)) = (plan.rings.get(ring), plan.arcs.get(ring)) else { return Ok(()) };
    let save = |name: &str, text: String| std::fs::write(dir.join(name), text).map_err(|e| format!("{name}: {e}"));
    let mut all = String::new();
    gpx_wpt(&mut all, &b.proj, [0.0, 0.0], "Base");
    for (i, a) in arcs.iter().enumerate() {
        gpx_day(&mut all, b, a, i + 1);
        let mut one = String::new();
        gpx_wpt(&mut one, &b.proj, [0.0, 0.0], "Base");
        gpx_day(&mut one, b, a, i + 1);
        save(&format!("day-{}.gpx", i + 1), gpx(&format!("{city} day {}", i + 1), &one))?;
    }
    save("ring.gpx", gpx(&format!("{city} ring, {:.1} km", r.stats.length / 1000.0), &all))?;
    save("itinerary.html", itinerary(city, b, r, arcs, own))
}

/// A printable page: one section per day with directions and map links.
fn itinerary(city: &str, b: &Built, r: &crate::rings::Ring, arcs: &[Arc], own: &str) -> String {
    let p = &b.proj;
    let base = [0.0, 0.0];
    let mut o = String::new();
    let city = esc(city);
    let _ = write!(
        o,
        "<!doctype html><meta charset=\"utf-8\"><title>{city} walks</title>
<style>
body {{ font: 14px/1.45 Georgia, serif; max-width: 46em; margin: 2em auto; padding: 0 1em; color: #222; }}
h1 {{ font-size: 1.6em; }} h2 {{ font-size: 1.2em; margin-top: 1.6em; border-bottom: 1px solid #ccc; }} .ends {{ font-style: italic; }}
.day {{ break-inside: avoid; }} .road {{ color: #b0562a; }} .muted {{ color: #777; }}
a {{ color: #2a5d8f; }} ol {{ padding-left: 1.4em; }} li {{ margin: .15em 0; }}
@media print {{ a {{ color: #222; text-decoration: none; }} .links {{ display: none; }} }}
</style>
<h1>{city}: {:.1} km ring in {} days</h1>
<p class=\"muted\">Base {}. Road {:.0}%, shaded {:.0}%, by water {:.0}%. Each day has its own GPX file (day-N.gpx); ring.gpx has them all.</p>",
        r.stats.length / 1000.0,
        arcs.len(),
        ll(p, base),
        r.stats.road_frac * 100.0,
        r.stats.shade_frac * 100.0,
        r.stats.waterside_frac * 100.0,
    );
    for (i, a) in arcs.iter().enumerate() {
        let (start, end) = (a.pts[0], *a.pts.last().unwrap());
        let _ = write!(o, "<div class=\"day\"><h2>Day {}: {:.1} km walk", i + 1, a.length / 1000.0);
        if let Some(d) = a.day_m() {
            let _ = write!(o, " ({:.1} km for the day)", d / 1000.0);
        }
        let _ = write!(o, "</h2>");
        if let Some(ends) = a.ends_text() {
            let _ = write!(o, "<p class=\"ends\">{}</p>", esc(&upper_first(&ends)));
        }
        if b.has_elevation {
            let _ = write!(o, "<p>Climb {:.0} m, descent {:.0} m for the day.</p>", a.day_climb(), {
                a.down + a.walk_out.as_ref().map_or(0.0, |w| w.down) + a.walk_back.as_ref().map_or(0.0, |w| w.down)
            });
        }
        let stretch_list = |o: &mut String, title: &str, walk: &[arcs::Stretch]| {
            let _ = write!(o, "<p><b>{title}</b></p><ol>");
            for s in walk {
                let class = if s.road { " class=\"road\"" } else { "" };
                let _ = write!(o, "<li{class}>{}</li>", esc(&s.describe()));
            }
            let _ = write!(o, "</ol>");
        };
        match (&a.walk_out, &a.walk_back) {
            (Some(out), Some(back)) => {
                stretch_list(&mut o, &format!("Walk out ({:.1} km)", out.metres / 1000.0), &out.walk);
                stretch_list(&mut o, &format!("Ring ({:.1} km)", a.length / 1000.0), &a.walk);
                stretch_list(&mut o, &format!("Walk home ({:.1} km)", back.metres / 1000.0), &back.walk);
                let _ = write!(
                    o,
                    "<p class=\"links\">Walk out: <a href=\"{}\">Google Maps</a> · <a href=\"{}\">Apple Maps</a>. Home: <a href=\"{}\">Google Maps</a> · <a href=\"{}\">Apple Maps</a>.</p>",
                    esc(&google_url(p, base, start, &waypoints(&out.pts), own)),
                    esc(&apple_url(p, base, start, "w")),
                    esc(&google_url(
                        p,
                        end,
                        base,
                        &waypoints(&back.pts.iter().rev().copied().collect::<Vec<_>>()),
                        own
                    )),
                    esc(&apple_url(p, end, base, "w")),
                );
            }
            _ => {
                let _ = write!(o, "<p><b>Getting there:</b> {}</p>", esc(&b.transit.trip_text(a.start.as_ref(), true)));
                stretch_list(&mut o, &format!("The walk ({:.1} km)", a.length / 1000.0), &a.walk);
                let _ = write!(o, "<p><b>Getting back:</b> {}</p>", esc(&b.transit.trip_text(a.end.as_ref(), false)));
                let _ = write!(
                    o,
                    "<p class=\"links\">Transit there: <a href=\"{}\">Google Maps</a> · <a href=\"{}\">Apple Maps</a>. Back: <a href=\"{}\">Google Maps</a> · <a href=\"{}\">Apple Maps</a>.</p>",
                    esc(&google_url(p, base, start, &[], "transit")),
                    esc(&apple_url(p, base, start, "r")),
                    esc(&google_url(p, end, base, &[], "transit")),
                    esc(&apple_url(p, end, base, "r")),
                );
            }
        }
        let _ = write!(
            o,
            "<p class=\"links\">The walk itself in <a href=\"{}\">Google Maps</a> (approximate: follows the arc through {} points). GPX: day-{}.gpx</p></div>",
            esc(&google_url(p, start, end, &waypoints(&a.pts), own)),
            waypoints(&a.pts).len(),
            i + 1
        );
    }
    o
}

/// The text with its first letter capitalised.
fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}
