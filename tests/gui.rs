//! GUI tests with egui_kittest, against a synthetic city (no network, no
//! downloaded data). The map's OpenGL layer is absent here, so the map
//! uses its plain egui fallback.

use egui::{Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use wedgewalks::gui::worker::Update;
use wedgewalks::gui::{App, Options};
use wedgewalks::{pipeline, synth};

const LAT: f64 = 50.0;
const LON: f64 = 4.0;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wedgewalks-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness(name: &str) -> Harness<'static, App> {
    let dir = temp_dir(name);
    std::fs::write(
        dir.join("cities.csv"),
        format!(
            "name,country,lat,lon,population,climate_note,summer_high_c,passport_ok,safety_ok,notes,source\n\
             Synthville,XX,{LAT},{LON},,,,,,synthetic,\n\
             Nodata,XX,10.0,10.0,,,,,,no data file,bbbike:Nodata\n"
        ),
    )
    .unwrap();
    let opts = Options {
        data: dir.join("data"),
        cities: dir.join("cities.csv"),
        config: dir.join("config.toml"),
        out: dir.join("out"),
    };
    Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |cc| {
        let mut app = App::new(cc, opts);
        // The synthetic city's ring makes about five days.
        app.cfg = synth::cfg();
        app.preload("synthville", pipeline::Loaded::from_osm(synth::city(LAT, LON), LAT, LON, 40_000.0));
        app
    })
}

/// Steps frames until `done` holds (the planner runs on its own thread).
fn wait_for(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(h.state()) {
        assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.step();
}

fn pick_city(h: &mut Harness<'static, App>, name: &str) {
    h.step();
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    h.step();
    h.get_by_label(name).click();
    h.step();
}

#[test]
fn starts_with_toolbar() {
    let mut h = harness("start");
    h.step();
    h.get_by_label("Add city…");
    h.get_by_label("Save config");
    assert!(h.state().selected.is_none());
}

#[test]
fn picking_a_city_plans_rings() {
    let mut h = harness("plan");
    pick_city(&mut h, "Synthville");
    assert_eq!(h.state().city().unwrap().name, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let plan = h.state().plan.clone().unwrap();
    assert!(plan.valid >= 1, "expected a valid ring");
    h.get_by_label_contains("✔ 1:");
    assert!(h.query_all_by_label_contains("within limits").count() >= 1);
}

#[test]
fn clicking_an_outing_shows_transit() {
    let mut h = harness("outing");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let arcs = h.state().plan.as_ref().unwrap().arcs[0].len();
    assert!(arcs >= 2);
    h.get_all_by_label_contains("% shade").next().expect("an outing row").click();
    h.step();
    assert_eq!(h.state().arc, Some(0));
    h.get_all_by_label_contains("Ring (").next().expect("the ring walk header");
    // The synthetic ring path is a named walking route.
    h.get_all_by_label_contains("Synth Ring Walk").next().expect("walk stretches name the route");
    h.get_by_label("Go");
    h.get_by_label("Return");
    h.get_all_by_label_contains("take bus").next().expect("transit directions");
}

#[test]
fn changing_limits_reranks_without_searching() {
    let mut h = harness("limits");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let before = h.state().plan.clone().unwrap();
    assert!(before.rings.len() > 1, "test needs several rings");
    // As if the "plans shown" slider were dragged down to one: a limit, not a search setting.
    h.state_mut().cfg.top_n = 1;
    let t = Instant::now();
    wait_for(&mut h, "a new plan", |a| {
        a.plan.as_ref().is_some_and(|p| !std::sync::Arc::ptr_eq(p, &before)) && !a.worker.busy()
    });
    assert!(t.elapsed() < Duration::from_secs(5), "re-ranking should not need a new search");
    let after = h.state().plan.clone().unwrap();
    assert_eq!(after.rings.len(), 1);
    assert_eq!(after.candidates, before.candidates);
}

#[test]
fn moving_the_base_replans_from_the_map() {
    let mut h = harness("base");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let base = h.state().base;
    // Right-click in the middle of the map, then choose "Set base here".
    let map_center = Pos2::new(700.0, 470.0);
    h.hover_at(map_center + vec2(60.0, 0.0));
    h.event(egui::Event::PointerButton {
        pos: map_center + vec2(60.0, 0.0),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.event(egui::Event::PointerButton {
        pos: map_center + vec2(60.0, 0.0),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.step();
    h.step();
    h.get_by_label("Set base here").click();
    h.step();
    let moved = h.state().base;
    assert!(moved.1 > base.1, "base should move east: {base:?} -> {moved:?}");
    wait_for(&mut h, "a plan for the new base", |a| {
        a.built.as_ref().is_some_and(|b| (b.lon - moved.1).abs() < 1e-9) && a.plan.is_some() && !a.worker.busy()
    });
}

#[test]
fn city_without_data_offers_download() {
    let mut h = harness("nodata");
    pick_city(&mut h, "Nodata  (no data)");
    h.get_by_label_contains("Download map data");
    assert!(h.state().plan.is_none());
}

#[test]
fn add_city_dialog_opens() {
    let mut h = harness("add");
    h.step();
    h.get_by_label("Add city…").click();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Search");
}

#[test]
fn theme_toggle_switches_light_and_dark() {
    let mut h = harness("theme");
    h.step();
    h.get_by_label("Light").click();
    h.step();
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    h.get_by_label("Dark").click();
    h.step();
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
}

#[test]
fn reset_to_defaults_restores_parameters() {
    let mut h = harness("reset");
    pick_city(&mut h, "Synthville");
    // Let the first plan land so the panel layout is settled before clicking.
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    h.state_mut().cfg.max_road_fraction = 0.3;
    h.state_mut().cfg.road_avoidance = 99.0;
    wait_for(&mut h, "the changed settings' plan", |a| !a.worker.busy());
    // Through accessibility rather than the pointer, which once in a few
    // dozen runs missed while the planner thread was busy.
    h.get_by_label("Reset to defaults").click_accesskit();
    h.step();
    assert_eq!(h.state().cfg, wedgewalks::config::Config::default());
    wait_for(&mut h, "a plan with defaults", |a| a.plan.is_some() && !a.worker.busy());
}

#[test]
fn walking_mode_plans_walks_from_the_base() {
    let mut h = harness("walking");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a transit plan", |a| a.plan.is_some() && !a.worker.busy());
    h.get_by_label("from base").click();
    h.step();
    assert_eq!(h.state().cfg.access, wedgewalks::config::Access::Direct);
    wait_for(&mut h, "a walking plan", |a| {
        a.plan.as_ref().is_some_and(|p| p.arcs.first().is_some_and(|arcs| arcs.iter().all(|x| x.walk_out.is_some())))
            && !a.worker.busy()
    });
    let plan = h.state().plan.clone().unwrap();
    let day_max = h.state().cfg.day_m();
    assert!(plan.rings[0].stats.r_max <= 3500.0, "walking rings stay close to the base");
    assert!(plan.arcs[0].iter().all(|a| a.day_m().unwrap() <= day_max + 1.0), "every day fits the limit");
    h.get_all_by_label_contains("km day").next().expect("outing rows show the day's walking");
}

/// A rebuilt graph comes with new transit stops and node numbers, so the
/// plan made on the old graph must not be drawn with it (it used to index
/// out of bounds in the map's transit trips). Real cities search for
/// seconds between the new graph and the new plan; here the new graph is
/// applied by hand so that gap can't close before a frame is drawn.
#[test]
fn rebuilding_drops_the_stale_plan() {
    let mut h = harness("rebuild");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    h.state_mut().arc = Some(0);
    h.step();
    let old_stops = h.state().built.as_ref().unwrap().transit.stops.len();
    // Fewer stops: the old plan's stop numbers point past the end.
    let mut cfg = h.state().cfg.clone();
    cfg.extract_radius_km = 1.0;
    let loaded = pipeline::Loaded::from_osm(synth::city(LAT, LON), LAT, LON, 40_000.0);
    let built = pipeline::build(&loaded, LAT, LON, &cfg);
    assert!(built.transit.stops.len() < old_stops, "test needs fewer stops after the rebuild");
    h.state_mut().apply(Update::Built(std::sync::Arc::new(built)));
    assert!(h.state().plan.is_none(), "old plan kept with the rebuilt graph");
    h.step();
}

/// The plan follows the trip: fewer days make a smaller ring with fewer
/// outings, and the number of outings is the number of days asked for.
#[test]
fn trip_controls_the_days() {
    let mut h = harness("trip");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let before = h.state().plan.clone().unwrap();
    let days = h.state().cfg.days().round() as usize;
    assert!(before.arcs[0].len().abs_diff(days) <= 1, "{} outings for {days} days", before.arcs[0].len());
    h.state_mut().cfg.trip.days = (days - 2) as f64;
    wait_for(&mut h, "a plan with fewer days", |a| {
        a.plan.as_ref().is_some_and(|p| !std::sync::Arc::ptr_eq(p, &before)) && !a.worker.busy()
    });
    let after = h.state().plan.clone().unwrap();
    assert!(after.arcs[0].len().abs_diff(days - 2) <= 1, "{} outings for {} days", after.arcs[0].len(), days - 2);
    assert!(after.rings[0].stats.r_mean < before.rings[0].stats.r_mean, "fewer days, a smaller ring");
}

/// Dragging an anchor or adding a routing point makes a plan of your own: it comes first, is routed through
/// what you set, and is saved with the city.
#[test]
fn editing_a_plan_makes_your_own() {
    let mut h = harness("edit");
    pick_city(&mut h, "Synthville");
    wait_for(&mut h, "a plan", |a| a.plan.is_some() && !a.worker.busy());
    let before = h.state().plan.clone().unwrap();
    let ring = &before.rings[0];
    assert!(ring.manual.is_none());
    assert!(ring.marks.len() >= 3, "a plan knows its anchors");

    // A routing point on the second day, well off the route, on a grid road of the synthetic city.
    let leg = wedgewalks::manual::leg_pts(ring, 1);
    let mid = leg[leg.len() / 2];
    let off = [mid[0] * 0.8, mid[1] * 0.8];
    h.state_mut().edit(0, wedgewalks::gui::map::Edit::AddVia(1, off));
    wait_for(&mut h, "your plan", |a| {
        a.plan.as_ref().is_some_and(|p| p.rings.first().is_some_and(|r| r.manual == Some(0))) && !a.worker.busy()
    });
    let plan = h.state().plan.clone().unwrap();
    let mine = &plan.rings[0];
    assert_eq!(mine.search, "your plan 1");
    assert_eq!(mine.via_marks[1].len(), 1);
    let via = mine.via_marks[1][0];
    assert!(
        mine.pts.iter().any(|&p| (p[0] - via[0]).hypot(p[1] - via[1]) < 1.0),
        "the route passes through the routing point"
    );
    assert_eq!(plan.arcs[0].len(), ring.cuts.len() - 1, "same number of days");
    h.get_by_label_contains("★ 1:");
    assert!(h.state().plan.as_ref().unwrap().rings.len() > 1, "the other plans are still listed");

    // Saved: next to the cities file, and loaded again with the city.
    let saved = h.state().manuals.clone();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].vias[1].len(), 1);

    // Delete it again.
    h.state_mut().delete_manual(0);
    wait_for(&mut h, "plans without it", |a| {
        a.plan.as_ref().is_some_and(|p| p.rings.iter().all(|r| r.manual.is_none())) && !a.worker.busy()
    });
    assert!(h.state().manuals.is_empty());
}

/// The Add dialog has a list of popular cities to tick; ones already added show as such.
#[test]
fn popular_cities_can_be_ticked() {
    let mut h = harness("popular");
    h.step();
    h.get_by_label("Add city…").click();
    h.step();
    h.get_by_label("Popular cities").click();
    h.step();
    h.get_by_label_contains("Alpha++");
    h.get_by_label("London, GB").click();
    h.step();
    h.get_by_label("Add 1 selected");
}
