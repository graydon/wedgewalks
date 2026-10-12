use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;
use std::path::Path;

/// All tunable parameters. Every field has a default, so the config file
/// only needs to mention what it changes. Defaults are guesses (see plan).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How you get about: on foot or by bicycle.
    pub travel: Travel,
    /// How you reach the days' routes: by transit, or travelling from the base.
    pub access: Access,
    /// The trip, in each combination of the two: how many days, how far each
    /// (when travelling from the base the day includes the way out and home).
    pub trip: Trip,
    pub direct_trip: Trip,
    pub bike_trip: Trip,
    pub bike_direct_trip: Trip,

    // Limits a plan is checked against.
    pub max_road_gap_m: f64,
    pub max_road_fraction: f64,
    /// Plans that run back alongside themselves (out along a spit and
    /// back) for more than this are outside the limits.
    pub max_double_back_km: f64,
    /// Most climbing in one day (m); days over it are marked steep.
    pub max_climb_per_day_m: f64,

    // Transit.
    /// Longest walk between a transit stop and an arc endpoint, and between
    /// the base (city center) and a stop.
    pub access_walk_m: f64,
    /// Longest walk between stops when changing routes.
    pub transfer_walk_m: f64,
    /// A transfer counts as this many metres of extra walking when
    /// choosing between routes.
    pub transfer_penalty_m: f64,
    /// Each metre ridden counts as this many metres of walking when
    /// choosing between routes (so 0.1: 10 km of riding = 1 km walk).
    pub ride_cost: f64,
    /// Each minute of expected waiting at a stop counts as this many
    /// metres of walking. The wait is half the time between departures:
    /// from the route's OSM `interval` tag, else a typical value per mode.
    pub wait_cost_m_per_min: f64,
    /// Bus and trolleybus legs count as this much extra walking, to
    /// prefer trains, metro, trams and ferries.
    pub bus_penalty_m: f64,
    /// Most changes of route on one trip (0: direct routes only). The nth
    /// change costs n times `transfer_penalty_m`, so fewer is strongly
    /// preferred and more is used only where it saves a lot.
    pub max_changes: usize,

    // Way preferences.
    /// How much more a metre of quiet street costs than a metre of path
    /// (a busy road costs 1.8 times that again).
    pub road_avoidance: f64,
    /// Prefer paths away from traffic over paths beside busy roads.
    pub prefer_away_from_traffic: bool,
    /// Prefer paths and streets beside water (rivers, canals, lakes, sea).
    pub prefer_waterside: bool,
    /// A way within this distance of water for most of its length counts
    /// as waterside.
    pub waterside_m: f64,
    /// Cost multiplier for edges on hiking/foot/bicycle route relations.
    pub route_bonus: f64,
    /// A car-free path within this distance of a busy road (tertiary and
    /// above) for most of its length counts as beside traffic.
    pub beside_traffic_m: f64,
    /// Each metre climbed counts as this many metres of walking
    /// (Naismith's rule: about 8). 0 ignores hills.
    pub climb_cost_m: f64,
    /// The same on a bicycle (hills cost more there).
    pub bike_climb_cost_m: f64,
    /// On a bicycle: how much less a cycleway, cycle lane or cycle track
    /// costs, and a way on a signed bicycle route.
    pub cycle_bonus: f64,
    pub bike_route_bonus: f64,
    /// A sharp turn counts as this many metres of walking (a switch to
    /// another street a bit more), so routes stay straight.
    pub turn_cost_m: f64,
    /// The most a lively street (shops, cafés, venues) can be discounted,
    /// as a share of its cost (0 = ignore street life).
    pub life_weight: f64,
    /// The most a fully shaded way can be discounted.
    pub shade_weight: f64,
    /// Ending a day away from any trailhead (station, interchange,
    /// neighbourhood, landmark, cafés, a path's end, a park entrance)
    /// counts as this much extra walking. 0 turns it off.
    pub trailhead_weight_m: f64,
    /// A ring point counts as shaded within this distance of trees, tree
    /// rows, or park/wood edges (or inside a park/wood).
    pub shade_near_m: f64,
    /// Whether flights of steps may be part of a ring.
    pub allow_steps: bool,

    // Planning.
    /// How far from the base (where you sleep) the centre of the ring may
    /// be, km. The ring still goes round the base.
    pub centroid_offset_km: f64,
    /// Area cut around each city center (km); plans stay inside 3/4 of it.
    pub extract_radius_km: f64,
    /// Number of plans reported per city.
    pub top_n: usize,
}

/// On foot, or by bicycle (which wants cycle routes, not footpaths, can't
/// use steps, and goes further in a day).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Travel {
    #[default]
    Foot,
    Bike,
}

/// How the days' routes are reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// By transit to the start of each day, and back from its end.
    #[default]
    Transit,
    /// From the base, out to the ring along an arc and home, all in one day.
    Direct,
}

/// A trip as people plan one: how many days, and how far to walk each.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Trip {
    /// Days of walking, round the base.
    pub days: f64,
    /// Walking per day: the arc in transit mode; the arc plus the walks out
    /// and home in walking mode.
    pub day_km: f64,
}

impl Default for Trip {
    fn default() -> Self {
        Trip { days: 7.0, day_km: 8.0 }
    }
}

/// Rings run this much longer than a circle through the same points
/// (streets wander).
pub const WINDING: f64 = 1.35;
/// Transit mode: a day's arc is this share of the day's length.
pub const ARC_FILL: f64 = 0.85;
/// Walking mode: the walk out and home runs this much longer than the
/// straight line.
pub const APPROACH_WINDING: f64 = 1.2;
/// Plans use this share of the map's radius.
pub const MAP_SHARE: f64 = 0.75;
/// A bicycle's day climbs this much more than a walker's.
pub const BIKE_CLIMB_FACTOR: f64 = 2.0;
/// Most days the controls offer.
pub const DAYS_MAX: f64 = 30.0;

/// Cost multiplier for waterside ways when `prefer_waterside` is on.
pub const WATERSIDE_BONUS: f64 = 0.75;
/// A preference ("prefer X over Y") makes Y this much more costly: enough
/// to break ties, not enough to force long detours.
pub const PREFERENCE_PENALTY: f64 = 1.5;
/// On a bicycle quiet streets cost this share of what they cost a walker, and busy roads this share of
/// theirs.
pub const BIKE_QUIET: f64 = 0.2;
pub const BIKE_BUSY: f64 = 0.7;
/// Busy roads cost this much more than quiet streets, always.
pub const BUSY_EXTRA: f64 = 1.8;

impl Default for Config {
    fn default() -> Self {
        Config {
            travel: Travel::Foot,
            access: Access::Transit,
            trip: Trip::default(),
            direct_trip: Trip { days: 4.0, day_km: 8.0 },
            bike_trip: Trip { days: 4.0, day_km: 30.0 },
            bike_direct_trip: Trip { days: 3.0, day_km: 40.0 },
            max_road_gap_m: 500.0,
            max_road_fraction: 0.15,
            max_double_back_km: 0.6,
            max_climb_per_day_m: 300.0,
            access_walk_m: 1000.0,
            transfer_walk_m: 200.0,
            transfer_penalty_m: 500.0,
            ride_cost: 0.1,
            wait_cost_m_per_min: 40.0,
            bus_penalty_m: 0.0,
            max_changes: 1,
            road_avoidance: 10.0,
            prefer_away_from_traffic: true,
            prefer_waterside: true,
            waterside_m: 30.0,
            route_bonus: 0.8,
            beside_traffic_m: 15.0,
            climb_cost_m: 8.0,
            bike_climb_cost_m: 25.0,
            cycle_bonus: 0.55,
            bike_route_bonus: 0.8,
            turn_cost_m: 90.0,
            life_weight: 0.3,
            shade_weight: 0.15,
            trailhead_weight_m: 500.0,
            shade_near_m: 15.0,
            allow_steps: true,
            centroid_offset_km: 3.0,
            extract_radius_km: 25.0,
            top_n: 5,
        }
    }
}

impl Config {
    /// The current trip: for this way of getting about and reaching the routes.
    pub fn trip(&self) -> Trip {
        match (self.travel, self.access) {
            (Travel::Foot, Access::Transit) => self.trip,
            (Travel::Foot, Access::Direct) => self.direct_trip,
            (Travel::Bike, Access::Transit) => self.bike_trip,
            (Travel::Bike, Access::Direct) => self.bike_direct_trip,
        }
    }

    pub fn trip_mut(&mut self) -> &mut Trip {
        match (self.travel, self.access) {
            (Travel::Foot, Access::Transit) => &mut self.trip,
            (Travel::Foot, Access::Direct) => &mut self.direct_trip,
            (Travel::Bike, Access::Transit) => &mut self.bike_trip,
            (Travel::Bike, Access::Direct) => &mut self.bike_direct_trip,
        }
    }

    /// Whether days start and end at the base (not at transit).
    pub fn is_direct(&self) -> bool {
        self.access == Access::Direct
    }

    pub fn is_bike(&self) -> bool {
        self.travel == Travel::Bike
    }

    /// Metres of walking one metre climbed is worth, for how you travel.
    pub fn climb_cost(&self) -> f64 {
        if self.is_bike() { self.bike_climb_cost_m } else { self.climb_cost_m }
    }

    /// Most climbing in a day (bicycles go further, so climb more).
    pub fn max_climb_m(&self) -> f64 {
        self.max_climb_per_day_m * if self.is_bike() { BIKE_CLIMB_FACTOR } else { 1.0 }
    }

    pub fn days(&self) -> f64 {
        self.trip().days
    }

    /// Metres of walking in a day.
    pub fn day_m(&self) -> f64 {
        self.trip().day_km * 1000.0
    }

    /// A day's arc (m) if the ring is `r` metres out.
    pub fn arc_m(&self, r: f64) -> f64 {
        let day = self.day_m();
        match self.access {
            Access::Transit => ARC_FILL * day,
            Access::Direct => (day - 2.0 * APPROACH_WINDING * r).max(0.25 * day) * 0.95,
        }
    }

    /// The furthest out a ring may run (m): what the map covers, and in
    /// walking mode what a day can reach.
    pub fn max_radius_m(&self) -> f64 {
        let map = self.extract_radius_km * 1000.0 * MAP_SHARE;
        match self.access {
            Access::Transit => map,
            Access::Direct => map.min(self.day_m() * 0.4),
        }
    }

    /// Roughly how far out a full circle of this many days runs (m): what
    /// the planner starts from, and shows as "about N km out".
    pub fn typical_radius_m(&self) -> f64 {
        let days = self.days();
        let r = match self.access {
            Access::Transit => days * self.arc_m(0.0) / (WINDING * TAU),
            // days * (day - 2 w r) = W tau r
            Access::Direct => days * self.day_m() * 0.95 / (WINDING * TAU + days * 2.0 * APPROACH_WINDING * 0.95),
        };
        r.min(self.max_radius_m())
    }

    /// Radius of the area that matters around the base: how far a plan may
    /// go, plus a margin. Places, shade and street life cover it.
    pub fn surroundings_m(&self) -> f64 {
        self.extract_radius_km * 1000.0 * MAP_SHARE + 1000.0
    }

    /// Cost per metre for each of `WAY_CLASSES` (path, path beside traffic,
    /// quiet street, busy road). A cyclist minds quiet streets much less and
    /// rough footpaths a little more.
    pub fn class_weights(&self) -> [f32; 4] {
        let f = self.road_avoidance;
        let away = if self.prefer_away_from_traffic { PREFERENCE_PENALTY } else { 1.0 };
        if self.is_bike() {
            [1.2, (1.2 * away) as f32, (f * BIKE_QUIET) as f32, (f * BUSY_EXTRA * BIKE_BUSY) as f32]
        } else {
            [1.0, away as f32, f as f32, (f * BUSY_EXTRA) as f32]
        }
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        if !path.exists() {
            eprintln!("note: {} not found, using defaults", path.display());
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Kinds of way, as indexed by `Config::class_weights`. Paths are
/// car-free; quiet streets are residential, living streets and service
/// roads; busy roads are everything else walkable with traffic (including
/// separately mapped sidewalks).
pub const WAY_CLASSES: [&str; 4] = ["path", "path beside traffic", "quiet street", "busy road"];
