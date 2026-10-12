//! The side panels: parameters (left) and rings and outings (right).

use super::{App, map};
use crate::arcs::{Arc as Outing, Stretch};
use crate::config::{Access, Config, DAYS_MAX};
use crate::pipeline::{Built, Plan};
use crate::rings::Ring;
use egui::{Color32, RichText, Ui};

/// Text colours for stretches on roads, and on paths beside traffic.
const ROAD_TEXT: Color32 = Color32::from_rgb(200, 110, 50);
const BESIDE_TEXT: Color32 = Color32::from_rgb(160, 130, 50);
pub(super) const WARN_TEXT: Color32 = Color32::from_rgb(220, 120, 40);

impl App {
    pub(super) fn params(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.selected.is_some() {
                self.base_section(ui);
            }
            if ui.button("Reset to defaults").on_hover_text(TIP_RESET).clicked() {
                self.reset_params();
            }
            self.trip_section(ui);
            outings_section(ui, &mut self.cfg);
            limits_section(ui, &mut self.cfg);
            preferences_section(ui, &mut self.cfg);
            planning_section(ui, &mut self.cfg);
        });
    }

    /// The selected city: base and base controls.
    fn base_section(&mut self, ui: &mut Ui) {
        let Some(i) = self.selected else { return };
        ui.heading(&self.cities[i].name);
        ui.label(format!("Base {:.5}, {:.5}", self.base.0, self.base.1));
        ui.label(RichText::new("Right-click the map to move the base.").weak());
        let c = &self.cities[i];
        let moved = (c.lat, c.lon) != self.base;
        let saved = (c.lat, c.lon);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(moved, egui::Button::new("Reset base"))
                .on_hover_text("Go back to the base in cities.csv")
                .clicked()
            {
                self.base = saved;
            }
            if ui
                .add_enabled(moved, egui::Button::new("Save base"))
                .on_hover_text("Store this base in cities.csv")
                .clicked()
            {
                (self.cities[i].lat, self.cities[i].lon) = self.base;
                self.save_cities();
            }
        });
        ui.separator();
    }

    /// The trip in plain terms: how many days, and how far each.
    fn trip_section(&mut self, ui: &mut Ui) {
        let c = &mut self.cfg;
        let (direct, bike) = (c.is_direct(), c.is_bike());
        egui::CollapsingHeader::new("Trip").default_open(true).show(ui, |ui| {
            let trip = c.trip_mut();
            ui.add(egui::Slider::new(&mut trip.days, 1.0..=DAYS_MAX).step_by(1.0).text("days")).on_hover_text(TIP_DAYS);
            let day_range = if bike {
                5.0..=120.0
            } else if direct {
                2.0..=25.0
            } else {
                2.0..=20.0
            };
            let day_tip = if direct { TIP_DAY_WALKING } else { TIP_DAY_TRANSIT };
            slider(ui, &mut trip.day_km, day_range, "km per day", day_tip);
            ui.label(RichText::new(format!("a ring about {:.1} km out", c.typical_radius_m() / 1000.0)).weak().small())
                .on_hover_text(TIP_OUT);
            if let Some(plan) = &self.plan
                && let (Some(r), Some(arcs)) = (plan.rings.get(self.ring), plan.arcs.get(self.ring))
            {
                ui.label(format!(
                    "Selected plan: {} days, {:.0} km, {:.1} km out",
                    arcs.len(),
                    r.stats.length / 1000.0,
                    r.stats.r_mean / 1000.0
                ));
            }
            slider(ui, &mut c.max_climb_per_day_m, 50.0..=1500.0, "max climb per day m", TIP_MAX_CLIMB);
            if bike {
                ui.label(RichText::new("(a bicycle's day may climb twice that)").weak().small());
            }
        });
    }

    /// Built-in default parameters, and the selected city's saved settings.
    pub fn reset_params(&mut self) {
        self.cfg = Config::default();
        self.status = "parameters reset to defaults".into();
    }

    pub(super) fn results(&mut self, ui: &mut Ui) {
        let (Some(plan), Some(built)) = (self.plan.clone(), self.built.clone()) else {
            ui.label(RichText::new("No rings yet.").weak());
            return;
        };
        ui.heading(format!("Rings ({} within limits)", plan.valid));
        if plan.rings.is_empty() {
            ui.label(TIP_NO_RINGS);
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.ring_list(ui, &plan);
            ui.separator();
            let Some(r) = plan.rings.get(self.ring) else { return };
            ring_details(ui, r, built.has_elevation);
            self.manual_controls(ui, r.manual);
            ui.separator();
            ui.strong("Outings");
            self.outing_list(ui, &plan.arcs[self.ring], &built);
        });
    }

    /// For your own plans, a name and a delete button; for the others, how to start editing one.
    fn manual_controls(&mut self, ui: &mut Ui, manual: Option<usize>) {
        let Some(i) = manual.filter(|&i| i < self.manuals.len()) else {
            ui.label(
                RichText::new(
                    "To edit this plan, drag one of its numbered anchors on the map, or right-click the map to add \
                     routing points: you get a copy of your own.",
                )
                .weak()
                .small(),
            );
            return;
        };
        ui.horizontal(|ui| {
            ui.label("Your plan");
            let before = self.manuals[i].name.clone();
            ui.add(egui::TextEdit::singleline(&mut self.manuals[i].name).desired_width(120.0));
            if self.manuals[i].name != before
                && let Some(c) = self.city().cloned()
            {
                self.save_manuals(&c);
            }
            if ui.button("Delete").on_hover_text("Throw this plan away.").clicked() {
                self.delete_manual(i);
            }
        });
        ui.label(
            RichText::new(
                "Drag anchors to move where days start and end; right-click for routing points, to split a day, \
                 or to remove an anchor.",
            )
            .weak()
            .small(),
        );
    }

    fn ring_list(&mut self, ui: &mut Ui, plan: &Plan) {
        for (i, r) in plan.rings.iter().enumerate() {
            let s = &r.stats;
            let mark = if r.manual.is_some() {
                "★"
            } else if r.ok() {
                "✔"
            } else {
                "✖"
            };
            let text = format!(
                "{mark} {}: {:.1} km, road {:.0}%, shade {:.0}%, {} arcs",
                i + 1,
                s.length / 1000.0,
                s.road_frac * 100.0,
                s.shade_frac * 100.0,
                plan.arcs[i].len()
            );
            let resp = ui.selectable_label(self.ring == i, text);
            if resp.clicked() {
                self.ring = i;
                self.arc = None;
            }
            if !r.ok() {
                resp.on_hover_text(r.problems.join("; "));
            }
        }
    }

    fn outing_list(&mut self, ui: &mut Ui, arcs: &[Outing], built: &Built) {
        for (ai, a) in arcs.iter().enumerate() {
            let ok = a.reachable(&self.cfg);
            let mut note = match (a.day_m(), ok) {
                (Some(d), true) => format!(", {:.1} km day", d / 1000.0),
                (Some(d), false) => format!(", {:.1} km day ⚠ over limit", d / 1000.0),
                (None, true) => String::new(),
                (None, false) => "  ⚠ no transit".into(),
            };
            if built.has_elevation {
                note.push_str(&format!(", climb {:.0} m", a.day_climb()));
                if a.too_steep(&self.cfg) {
                    note.push_str(" ⚠ steep");
                }
            }
            let text = RichText::new(format!(
                "{} {:.1} km, {:.0} m road, {:.0}% shade{note}",
                ai + 1,
                a.length / 1000.0,
                a.road_len,
                a.shade * 100.0
            ))
            .color(map::arc_color(ai));
            let hovered = self.hover == Some((self.ring, ai));
            if ui.selectable_label(self.arc == Some(ai) || hovered, text).clicked() {
                self.arc = if self.arc == Some(ai) { None } else { Some(ai) };
            }
            if self.arc == Some(ai) {
                ui.indent(ai, |ui| outing_details(ui, a, built));
            }
        }
    }
}

/// How outings reach the ring: transit settings, or in walking mode just
/// the trailhead weight.
fn outings_section(ui: &mut Ui, c: &mut Config) {
    egui::CollapsingHeader::new("Outings").default_open(true).show(ui, |ui| {
        if c.access == Access::Transit {
            slider(ui, &mut c.access_walk_m, 100.0..=2000.0, "walk to stop m", "Longest walk between a transit stop and the ring, and between the base and a stop.");
            slider(ui, &mut c.max_changes, 0..=3, "max changes", TIP_MAX_CHANGES);
            slider(ui, &mut c.transfer_walk_m, 0.0..=500.0, "transfer walk m", "Longest walk between two stops when changing routes.");
            slider(ui, &mut c.transfer_penalty_m, 0.0..=3000.0, "transfer penalty m", "The first change of route counts as this much extra walking when choosing how to get to an arc, the second twice that, and so on. Higher prefers fewer changes.");
            slider(ui, &mut c.wait_cost_m_per_min, 0.0..=150.0, "wait cost m/min", TIP_WAIT);
            slider(ui, &mut c.bus_penalty_m, 0.0..=2000.0, "bus penalty m", "Each bus or trolleybus leg counts as this much extra walking, to prefer metro, trains, trams and ferries.");
            slider(ui, &mut c.ride_cost, 0.0..=0.5, "ride cost", TIP_RIDE);
        }
        slider(ui, &mut c.trailhead_weight_m, 0.0..=2000.0, "trailhead weight m", TIP_TRAILHEADS);
    });
}

/// What makes a plan outside the limits (re-ranks instantly).
fn limits_section(ui: &mut Ui, c: &mut Config) {
    egui::CollapsingHeader::new("Limits").default_open(true).show(ui, |ui| {
        slider(ui, &mut c.max_road_gap_m, 50.0..=2000.0, "max road gap m", "Longest single stretch of road (with traffic) allowed in a plan.");
        slider(ui, &mut c.max_road_fraction, 0.0..=0.5, "max road share", "Largest share of the whole plan that may be on roads with traffic.");
        slider(ui, &mut c.max_double_back_km, 0.0..=5.0, "max doubling back km", "Plans that run back alongside themselves (out along a spit or pier and back) for more than this are outside the limits.");
        ui.checkbox(&mut c.allow_steps, "allow steps")
            .on_hover_text("When off, flights of steps are removed from the walk graph entirely.");
    });
}

fn preferences_section(ui: &mut Ui, c: &mut Config) {
    egui::CollapsingHeader::new("Way preferences").default_open(true).show(ui, |ui| {
        slider(ui, &mut c.road_avoidance, 1.0..=40.0, "road avoidance", TIP_ROAD_AVOIDANCE);
        ui.checkbox(&mut c.prefer_away_from_traffic, "prefer paths away from traffic").on_hover_text("Car-free paths that run right beside a busy road (e.g. a cycle track along an avenue) cost a little more (1.5x), so quieter paths win when the detour is small.");
        ui.checkbox(&mut c.prefer_waterside, "prefer walking by water").on_hover_text("Paths and streets beside rivers, canals, lakes or the sea count as 25% shorter, so plans follow waterfronts when they can.");
        slider(ui, &mut c.waterside_m, 0.0..=80.0, "waterside m", "A way within this distance of mapped water for most of its length counts as waterside. 0 turns detection off.");
        slider(ui, &mut c.beside_traffic_m, 0.0..=40.0, "beside traffic m", "A car-free path within this distance of a busy road (tertiary or bigger) for most of its length counts as beside traffic. 0 turns detection off.");
        if !c.is_bike() {
            slider(ui, &mut c.route_bonus, 0.3..=1.0, "walking-route bonus", "Paths on signed walking or cycling routes cost this fraction of their length, so plans follow them. 1 = no bonus.");
        }
        if c.is_bike() {
            slider(ui, &mut c.bike_climb_cost_m, 0.0..=60.0, "climb cost m/m (bike)", TIP_CLIMB_COST);
            slider(ui, &mut c.cycle_bonus, 0.2..=1.0, "cycle way bonus", "On a bicycle, cycleways, cycle lanes and cycle tracks cost this fraction of their length. 1 = no bonus.");
            slider(ui, &mut c.bike_route_bonus, 0.4..=1.0, "bicycle route bonus", "Ways on a signed bicycle route cost this fraction more off. 1 = no bonus.");
        } else {
            slider(ui, &mut c.climb_cost_m, 0.0..=30.0, "climb cost m/m", TIP_CLIMB_COST);
        }
        slider(ui, &mut c.turn_cost_m, 0.0..=300.0, "turn cost m", "A sharp turn counts as this many metres of walking (a switch to another street a bit more), so routes stay straight and keep to one street when several are about as good.");
        slider(ui, &mut c.life_weight, 0.0..=0.6, "street life bonus", "The most a lively street (shops, cafés, venues beside it or around it) can be discounted. Prefers a street with things to see over a stroad, without forcing detours.");
        slider(ui, &mut c.shade_weight, 0.0..=0.4, "shade bonus", "The most a fully shaded way (trees, parks, woods beside it) can be discounted.");
    });
}

fn planning_section(ui: &mut Ui, c: &mut Config) {
    egui::CollapsingHeader::new("Planning").default_open(false).show(ui, |ui| {
        slider(ui, &mut c.centroid_offset_km, 0.0..=8.0, "centre offset km", "How far from the base (where you sleep) the centre of the ring may be. The ring still goes round the base; this lets it follow the town's own shape when the base is off-centre. 0 centres it on the base.");
        slider(ui, &mut c.top_n, 1..=12, "plans shown", "How many distinct plans to list.");
        slider(ui, &mut c.extract_radius_km, 10.0..=40.0, "extract radius km", "How much map around the base is loaded; plans use three quarters of it. Bigger is slower.");
        slider(ui, &mut c.shade_near_m, 0.0..=50.0, "shade distance m", "A point counts as shaded within this distance of mapped trees, tree rows or park/wood edges, or inside a park or wood.");
    });
}

fn ring_details(ui: &mut Ui, r: &Ring, has_elevation: bool) {
    let s = &r.stats;
    if !r.ok() {
        ui.colored_label(WARN_TEXT, format!("Outside limits: {}", r.problems.join("; ")));
    }
    let shape = if r.is_open() {
        format!("chain from shore to shore, {:.0}° round", r.span * 360.0)
    } else {
        "closed ring".to_string()
    };
    let rows = [
        ("length", format!("{:.1} km", s.length / 1000.0)),
        ("road", format!("{:.0}% ({:.1} km)", s.road_frac * 100.0, s.road_len / 1000.0)),
        ("longest road gap", format!("{:.0} m (red ✖ on map)", s.max_gap)),
        ("radius", format!("{:.1}–{:.1} km", s.r_min / 1000.0, s.r_max / 1000.0)),
        ("beside traffic", format!("{:.0}%", s.beside_frac * 100.0)),
        ("by water", format!("{:.0}%", s.waterside_frac * 100.0)),
        ("doubles back", format!("{:.1} km", s.double_back / 1000.0)),
        ("shape", shape),
        ("on walking routes", format!("{:.0}%", s.route_frac * 100.0)),
        ("shaded", format!("{:.0}%", s.shade_frac * 100.0)),
        ("street life", format!("{:.0}%", s.life_frac * 100.0)),
        ("sharp turns", format!("{:.1} per km", s.turns_per_km)),
        ("flights of steps", format!("{}", s.steps)),
        ("climb", if has_elevation { format!("{:.0} m", s.climb) } else { "no elevation data".into() }),
    ];
    egui::Grid::new("ring_stats").num_columns(2).show(ui, |ui| {
        for (k, v) in rows {
            ui.label(k);
            ui.label(v);
            ui.end_row();
        }
    });
    ui.label(RichText::new(format!("found by {}", r.search)).weak().small());
}

/// The selected outing: its walk (and walks out and home, in walking mode)
/// and transit directions.
fn outing_details(ui: &mut Ui, a: &Outing, built: &Built) {
    if let Some(ends) = a.ends_text() {
        ui.label(RichText::new(ends).italics()).on_hover_text(TIP_ENDS);
    }
    let section = |ui: &mut Ui, title: String, walk: &[Stretch]| {
        ui.label(RichText::new(title).strong());
        stretch_list(ui, walk);
    };
    if let Some(out) = &a.walk_out {
        section(ui, format!("Walk out ({:.1} km)", out.metres / 1000.0), &out.walk);
    }
    section(ui, format!("Ring ({:.1} km)", a.length / 1000.0), &a.walk);
    if let Some(back) = &a.walk_back {
        section(ui, format!("Walk home ({:.1} km)", back.metres / 1000.0), &back.walk);
    }
    if built.has_elevation {
        ui.label(format!("Climb {:.0} m, descent {:.0} m along the ring", a.up, a.down));
    }
    match a.day_m() {
        Some(d) => {
            let climb = if built.has_elevation { format!(", climbing {:.0} m", a.day_climb()) } else { String::new() };
            ui.label(RichText::new(format!("{:.1} km for the day{climb}", d / 1000.0)).strong());
        }
        None => {
            ui.label(RichText::new("Go").strong());
            ui.label(built.transit.trip_text(a.start.as_ref(), true));
            ui.label(RichText::new("Return").strong());
            ui.label(built.transit.trip_text(a.end.as_ref(), false));
        }
    }
}

/// A walk's stretches, one per line, road in orange and paths beside
/// traffic in ochre.
fn stretch_list(ui: &mut Ui, walk: &[Stretch]) {
    for st in walk {
        let text = RichText::new(st.describe());
        let text = match (st.road, st.beside) {
            (true, _) => text.color(ROAD_TEXT),
            (false, true) => text.color(BESIDE_TEXT),
            _ => text,
        };
        ui.label(text);
    }
}

/// A slider with a tooltip; true if it changed.
fn slider<N: egui::emath::Numeric>(
    ui: &mut Ui,
    v: &mut N,
    range: std::ops::RangeInclusive<N>,
    label: &str,
    tip: &str,
) -> bool {
    ui.add(egui::Slider::new(v, range).text(label)).on_hover_text(tip).changed()
}

const TIP_ENDS: &str = "Trailheads where this outing begins and ends: stations, interchanges, neighbourhoods, \
    landmarks, cafés, where the ring joins or leaves a named path, park entrances. The \"trailhead weight\" setting \
    draws cut points towards them.";
const TIP_TRAILHEADS: &str = "Cutting the ring away from a trailhead counts as this much extra walking, so outings \
    tend to start and end at stations and interchanges, named neighbourhoods, landmarks and viewpoints, clusters \
    of cafés, where the ring joins or leaves a named path or walking route, and park entrances. 0 turns it off.";
const TIP_MAX_CLIMB: &str = "Most climbing in one day, from ground elevation (SRTM, smoothed). Arcs are cut to \
    keep each day within it where possible (in walking mode, counting the walks out and home); days over it are \
    marked \"steep\". Rings are steered away from hills by \"climb cost\" under Way preferences.";
const TIP_CLIMB_COST: &str = "Each metre climbed counts as this many metres of walking when looking for rings, so \
    they go round hills where that's not much longer. Naismith's rule for walkers is about 8. 0 ignores hills.";
const TIP_MAX_CHANGES: &str = "Most changes of route on one trip to or from an arc. 0: direct routes only (arcs whose ends \
    have none show as unreachable). The nth change costs n times the transfer penalty, so fewer is strongly preferred \
    and extra changes are used only where they save a lot or are the only way. More changes make the search a little \
    slower.";
const TIP_WAIT: &str = "Each minute of expected waiting at a stop counts as this many metres of walking. The wait is half \
    the time between departures: from the route's OSM interval tag if mapped (shown as \"every ~N min\"), else typical: \
    metro 5 min, tram 8, bus 12, train/ferry 15. Higher favours frequent lines.";
const TIP_RIDE: &str = "Each metre on transit counts as this many metres of walking when choosing routes, so trips don't \
    ride far out of the way to save a short walk. 0.1 = 10 km riding counts like 1 km walking.";
const TIP_DAYS: &str = "How many days of walking, round the base. The ring is sized to make this many days of the length \
    below: more days means a bigger ring, as far as the map allows. If the map is too small, you get fewer days, and \
    the plan says so.";
const TIP_DAY_TRANSIT: &str = "How far to walk on each outing (the arc; the short walks to and from stops are \
    extra). Longer days mean fewer of them for the same ring.";
const TIP_DAY_WALKING: &str = "How far to walk on each outing, including the walk out to the ring and home. Longer \
    days allow a ring further out.";
const TIP_OUT: &str = "How far from the base the ring runs, typically, for this many days of this length. Where \
    there is water, the ring comes in to the shore instead.";
const TIP_RESET: &str = "Put every parameter back to its built-in default. Use \"Save config\" afterwards to keep \
    the defaults in config.toml.";
const TIP_NO_RINGS: &str = "No plan found. Water or motorways may leave too little to walk on round the base: try \
    right-clicking the map to move the base, fewer days, or a bigger extract radius.";
const TIP_ROAD_AVOIDANCE: &str = "How many metres of path one metre of quiet street is worth (a busy road costs 1.8 \
    times that again). Low values allow some road for a shorter walk; high values keep plans car-free even if longer.";
