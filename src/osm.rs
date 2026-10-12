//! Reads the bits of an .osm.pbf file we care about, within a radius of a
//! point, in up to three parallel passes: nodes inside the radius, then the
//! ways touching them plus relations, then multipolygon outlines (known
//! only once relations have been read).

use crate::graph::{Proj, norm};
use osmpbf::{BlobDecode, BlobReader, Element, PrimitiveBlock, RelMemberType, Relation};
use rayon::prelude::*;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    CarFree,
    Road,
}

#[derive(Serialize, Deserialize)]
pub struct WayRec {
    pub id: i64,
    pub kind: Kind,
    /// Whether it can be walked, and whether it can be cycled.
    pub foot: bool,
    pub bike: bool,
    /// Built for bicycles: a cycleway, a way signed for them, or a road with a cycle lane or track.
    pub cycle: bool,
    pub steps: bool,
    pub refs: Vec<i64>,
    /// `name`, else `ref`.
    pub name: Option<String>,
    /// Index into `WAY_TYPES`.
    pub wtype: u8,
    pub bridge: bool,
}

/// What kind of way an edge is, for describing walks.
pub const WAY_TYPES: &[&str] = &[
    "footway",
    "path",
    "cycleway",
    "pedestrian street",
    "track",
    "bridleway",
    "steps",
    "residential street",
    "living street",
    "minor road",
    "tertiary road",
    "service road",
    "sidewalk",
    "crossing",
    "bridge sidewalk",
    "towpath",
    "way",
];

/// Car-free highway types.
const PATHS: &[&str] = &["footway", "path", "cycleway", "pedestrian", "track", "bridleway", "steps"];
/// Walkable roads with traffic.
const MINOR_ROADS: &[&str] = &["residential", "living_street", "unclassified", "tertiary", "tertiary_link", "service"];
/// Roads too busy to walk along, except on bridges (see `classify`).
const MAJOR_ROADS: &[&str] = &["primary", "secondary", "trunk", "primary_link", "secondary_link"];
/// Roads whose traffic makes a path beside them "beside traffic".
const BUSY_ROADS: &[&str] = &[
    "motorway",
    "trunk",
    "primary",
    "secondary",
    "tertiary",
    "motorway_link",
    "trunk_link",
    "primary_link",
    "secondary_link",
    "tertiary_link",
];

const TRANSIT_MODES: &[&str] = &["bus", "trolleybus", "tram", "subway", "train", "light_rail", "monorail", "ferry"];
const WALKING_ROUTES: &[&str] = &["hiking", "foot", "walking", "bicycle"];

/// The `WAY_TYPES` index for a walkable way's tags.
pub fn way_type(tags: &[(&str, &str)]) -> u8 {
    let side = tag(tags, "footway").or(tag(tags, "path"));
    let hw = tag(tags, "highway").unwrap_or("");
    let name = match (hw, side) {
        (_, Some("sidewalk")) => "sidewalk",
        (_, Some("crossing")) => "crossing",
        (_, _) if tag(tags, "towpath") == Some("yes") => "towpath",
        ("pedestrian", _) => "pedestrian street",
        ("residential", _) => "residential street",
        ("living_street", _) => "living street",
        ("unclassified", _) => "minor road",
        ("tertiary" | "tertiary_link", _) => "tertiary road",
        ("service", _) => "service road",
        (h, _) if MAJOR_ROADS.contains(&h) => "bridge sidewalk",
        (h, _) if PATHS.contains(&h) => h,
        _ => "way",
    };
    WAY_TYPES.iter().position(|t| *t == name).unwrap_or(WAY_TYPES.len() - 1) as u8
}

#[derive(Serialize, Deserialize)]
pub struct Route {
    pub mode: String,
    pub label: String,
    /// Night-only service; a poor choice for daytime outings.
    pub night: bool,
    /// Stops in travel order (stop positions and platforms).
    pub stops: Vec<i64>,
    /// Minutes between departures, from the `interval` tag if mapped.
    pub interval_min: Option<f32>,
}

/// What a water feature is, for deciding what counts as a shore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaterKind {
    /// Canals, streams: nice to walk beside, but not a shore.
    Minor,
    /// Coastline, rivers, riverbanks, docks, multipolygon water bodies.
    Major,
    /// A water area drawn as one closed way: major if big enough.
    Area,
}

/// A water feature: a shoreline-like line or the outline (or a piece of
/// it) of a water area.
#[derive(Serialize, Deserialize)]
pub struct Water {
    pub refs: Vec<i64>,
    pub kind: WaterKind,
    pub shape: WaterShape,
}

/// How a water way relates to the water: for filling it in on the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaterShape {
    /// Coastline: the sea is on its right.
    Coast,
    /// A river, canal or stream drawn as its centre line.
    Line,
    /// The outline (or a piece of it) of a water area.
    Area,
}

fn water_shape(tags: &[(&str, &str)]) -> WaterShape {
    match (tag(tags, "natural"), tag(tags, "waterway")) {
        (Some("coastline"), _) => WaterShape::Coast,
        (_, Some("river" | "canal" | "stream")) => WaterShape::Line,
        _ => WaterShape::Area,
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct OsmData {
    pub ways: Vec<WayRec>,
    /// Ways on hiking/foot/bicycle route relations -> the route's name
    /// (empty if unnamed).
    pub route_ways: HashMap<i64, String>,
    /// Ways on signed bicycle routes.
    pub bike_route_ways: std::collections::HashSet<i64>,
    pub routes: Vec<Route>,
    /// node id -> (lat, lon)
    pub coords: HashMap<i64, (f64, f64)>,
    pub stop_names: HashMap<i64, String>,
    /// Gluten-free places: (lat, lon, is_dedicated "only").
    /// Shade features: node refs, and whether it is a line (tree row)
    /// rather than an area outline. Multipolygon outers come as pieces.
    pub shade_ways: Vec<(Vec<i64>, bool)>,
    /// Individually mapped trees (lat, lon).
    pub trees: Vec<(f64, f64)>,
    /// Busy roads, as node lists: paths running alongside them count as
    /// beside traffic.
    pub traffic: Vec<Vec<i64>>,
    pub water: Vec<Water>,
    /// Named streets, water and parks, for describing where unnamed paths
    /// go ("beside Queen Street", "through Riverdale Park").
    pub places: Vec<Place>,
    /// Points that make natural places to start or end a walk.
    pub spots: Vec<Spot>,
    /// Shops, food and drink, and cultural venues (lat, lon): what makes a
    /// street lively.
    pub lively: Vec<(f64, f64)>,
}

/// A point of interest for where outings begin and end.
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Spot {
    pub lat: f64,
    pub lon: f64,
    pub kind: SpotKind,
    pub name: String,
}

/// Whether a node is the kind of place that gives a street life: a shop, a
/// place to eat or drink, a market or a cultural venue.
fn is_lively(tags: &[(&str, &str)]) -> bool {
    match (tag(tags, "shop"), tag(tags, "amenity"), tag(tags, "tourism")) {
        (Some(s), ..) if s != "no" && s != "vacant" => true,
        (
            _,
            Some("cafe" | "restaurant" | "pub" | "bar" | "fast_food" | "ice_cream" | "biergarten" | "food_court"),
            _,
        )
        | (_, Some("marketplace" | "theatre" | "cinema" | "arts_centre" | "library" | "nightclub"), _)
        | (.., Some("museum" | "gallery" | "attraction")) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Serialize, Deserialize)]
pub enum SpotKind {
    /// A named neighbourhood, quarter, suburb or village.
    Locality,
    /// A viewpoint, attraction or museum.
    Landmark,
    /// A café, restaurant, pub or bakery (unnamed: only counted).
    Venue,
}

/// The kind of spot a node is, if any.
fn spot_kind(tags: &[(&str, &str)]) -> Option<SpotKind> {
    match (tag(tags, "place"), tag(tags, "tourism"), tag(tags, "amenity"), tag(tags, "shop")) {
        (Some("neighbourhood" | "quarter" | "suburb" | "village"), ..) => Some(SpotKind::Locality),
        (_, Some("viewpoint" | "attraction" | "museum"), ..) => Some(SpotKind::Landmark),
        (_, _, Some("cafe" | "restaurant" | "pub" | "bar" | "ice_cream" | "biergarten"), _) | (.., Some("bakery")) => {
            Some(SpotKind::Venue)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaceKind {
    Street,
    Water,
    Park,
}

/// A named feature: a street or water line, or a park or water outline
/// (multipolygon outlines come as pieces, each carrying the name).
#[derive(Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    pub kind: PlaceKind,
    pub refs: Vec<i64>,
}

/// The kind of named place a way or relation is, if any.
fn place_kind(tags: &[(&str, &str)]) -> Option<PlaceKind> {
    let underground = tag(tags, "tunnel").is_some_and(|t| t != "no")
        || tag(tags, "layer").and_then(|l| l.parse::<i32>().ok()).is_some_and(|l| l < 0)
        || tag(tags, "covered") == Some("yes");
    if tag(tags, "highway").is_some() {
        (!underground).then_some(PlaceKind::Street)
    } else if water_kind(tags).is_some() {
        Some(PlaceKind::Water)
    } else if shade_kind(tags) == Some(false)
        || matches!(tag(tags, "leisure"), Some("garden" | "nature_reserve" | "common"))
    {
        Some(PlaceKind::Park)
    } else {
        None
    }
}

fn tag<'a>(tags: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The water kind of a way or multipolygon, if it is water at all.
fn water_kind(tags: &[(&str, &str)]) -> Option<WaterKind> {
    match (tag(tags, "natural"), tag(tags, "waterway"), tag(tags, "landuse")) {
        (Some("coastline" | "bay" | "strait"), _, _) | (_, Some("river" | "riverbank" | "dock"), _) => {
            Some(WaterKind::Major)
        }
        (_, Some("canal" | "stream"), _) => Some(WaterKind::Minor),
        (Some("water"), _, _) | (_, _, Some("reservoir" | "basin")) => Some(WaterKind::Area),
        _ => None,
    }
}

/// Shade-giving features: Some(true) for lines (tree rows), Some(false)
/// for areas (woods, forests, parks).
fn shade_kind(tags: &[(&str, &str)]) -> Option<bool> {
    match (tag(tags, "natural"), tag(tags, "landuse"), tag(tags, "leisure")) {
        (Some("tree_row"), _, _) => Some(true),
        (Some("wood"), _, _) | (_, Some("forest"), _) | (_, _, Some("park")) => Some(false),
        _ => None,
    }
}

/// How a way can be used: what kind it is, and by whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WayUse {
    pub kind: Kind,
    pub foot: bool,
    pub bike: bool,
    pub cycle: bool,
}

/// Decides whether a way can be walked or cycled, and if so whether it is car-free.
pub fn classify(tags: &[(&str, &str)]) -> Option<WayUse> {
    let hw = tag(tags, "highway")?;
    let private = matches!(tag(tags, "access"), Some("no" | "private"));
    let foot = tag(tags, "foot");
    let foot_ok = matches!(foot, Some("yes" | "designated" | "permissive"));
    let on_foot = foot != Some("no") && !(private && !foot_ok);
    let bicycle = tag(tags, "bicycle");
    let bike_ok = matches!(bicycle, Some("yes" | "designated" | "permissive"));
    let sidewalk_like = matches!(tag(tags, "footway").or(tag(tags, "path")), Some("sidewalk" | "crossing"))
        || tag(tags, "cycleway") == Some("crossing");
    let kind_and_bike = if PATHS.contains(&hw) {
        // Separately mapped sidewalks and crossings sit next to traffic.
        let by_default = match hw {
            "path" | "track" | "cycleway" | "bridleway" => !sidewalk_like || tag(tags, "cycleway") == Some("crossing"),
            _ => false, // footways, pedestrian streets and steps: on foot unless signed otherwise
        };
        let bike = hw != "steps" && by_default;
        Some((if sidewalk_like { Kind::Road } else { Kind::CarFree }, bike))
    } else if MINOR_ROADS.contains(&hw) {
        let no_cars = matches!(tag(tags, "motor_vehicle").or(tag(tags, "motorcar")), Some("no" | "private"))
            || (private && foot_ok);
        Some((if no_cars { Kind::CarFree } else { Kind::Road }, true))
    } else {
        // Major roads are excluded, except bridges: often the only way across
        // a river. Primary/secondary bridges almost always have a sidewalk even
        // when it isn't tagged, so allow them unless tagged otherwise; trunk
        // roads only with a tagged sidewalk or foot access.
        let bridge = tag(tags, "bridge").is_some_and(|b| b != "no");
        if MAJOR_ROADS.contains(&hw) && bridge {
            let sides = ["sidewalk", "sidewalk:both", "sidewalk:left", "sidewalk:right"].map(|k| tag(tags, k));
            let tagged_yes = sides.iter().any(|v| matches!(v, Some("both" | "left" | "right" | "yes" | "separate")));
            let tagged_no =
                sides.iter().any(|v| v.is_some()) && sides.iter().all(|v| matches!(v, Some("no" | "none") | None));
            let trunk = hw.starts_with("trunk");
            (foot_ok || tagged_yes || (!trunk && !tagged_no)).then_some((Kind::Road, !trunk))
        } else {
            None
        }
    };
    let (kind, bike_default) = kind_and_bike?;
    // Signed bicycle access beats the default either way; a private way is for bikes only if signed so.
    let bike = match bicycle {
        Some("no" | "dismount") => false,
        _ if bike_ok => hw != "steps",
        _ => bike_default && !private,
    };
    let foot = on_foot;
    let lane =
        |k: &str| matches!(tag(tags, k), Some("lane" | "track" | "opposite_lane" | "opposite_track" | "separate"));
    let cycle = bike
        && (hw == "cycleway"
            || bicycle == Some("designated")
            || tag(tags, "bicycle_road") == Some("yes")
            || ["cycleway", "cycleway:left", "cycleway:right", "cycleway:both"].iter().any(|k| lane(k)));
    (foot || bike).then_some(WayUse { kind, foot, bike, cycle })
}

/// Route names often use arrows ("29 → Wood Green"); spell them out so
/// any font can show them.
fn plain_arrows(s: &str) -> String {
    let mut out = s.to_string();
    for a in ["<->", "-->", "=>", "->", "→", "⇒", "⟶", "➔", "↔", "⇄"] {
        let to = if matches!(a, "↔" | "<->" | "⇄") { " and " } else { " to " };
        out = out.replace(a, to);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Minutes from an OSM `interval` value: "10", "00:10", "0:10:00", or a
/// range like "5-10" (the larger is used, to be safe).
pub fn parse_interval(v: &str) -> Option<f32> {
    let one = |s: &str| -> Option<f32> {
        let parts: Vec<f32> = s.trim().split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        match parts[..] {
            [m] => Some(m),
            [h, m] => Some(h * 60.0 + m),
            [h, m, s] => Some(h * 60.0 + m + s / 60.0),
            _ => None,
        }
    };
    v.split(['-', ';', ','])
        .filter_map(one)
        .filter(|m| *m > 0.0 && *m < 24.0 * 60.0)
        .fold(None, |a: Option<f32>, m| Some(a.map_or(m, |a| a.max(m))))
}

/// Night-only routes by tag or by common naming ("N12", "Noctilien").
fn is_night_route(tags: &[(&str, &str)], label: &str) -> bool {
    tag(tags, "by_night") == Some("only")
        || label.contains("Noctilien")
        || label.to_lowercase().contains("night")
        || tag(tags, "ref")
            .is_some_and(|r| r.len() > 1 && r.starts_with('N') && r[1..].bytes().all(|b| b.is_ascii_digit()))
}

/// Runs `f` over every primitive block in parallel, folding into `T`.
fn par_blocks<T, F, M>(path: &Path, f: F, merge: M) -> Result<T, String>
where
    T: Default + Send,
    F: Fn(&PrimitiveBlock, &mut T) + Sync,
    M: Fn(T, T) -> T + Sync,
{
    let reader = BlobReader::from_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
    reader
        .par_bridge()
        .try_fold(T::default, |mut acc, blob| {
            let blob = blob.map_err(|e| e.to_string())?;
            if let BlobDecode::OsmData(block) = blob.decode().map_err(|e| e.to_string())? {
                f(&block, &mut acc);
            }
            Ok(acc)
        })
        .try_reduce(T::default, |a, b| Ok(merge(a, b)))
}

/// A transit route relation before platform ways are resolved to nodes.
struct RawRoute {
    mode: String,
    label: String,
    night: bool,
    /// Stops in order, as (is a platform way, id).
    members: Vec<(bool, i64)>,
    interval_min: Option<f32>,
}

/// A walking or cycling route's name; a route with only a number gets its
/// kind ("cycle route 6"), since a bare "6" means nothing on its own.
fn route_name(tags: &[(&str, &str)], mode: &str) -> String {
    match (tag(tags, "name"), tag(tags, "ref")) {
        (Some(name), _) => name.to_string(),
        (None, Some(r)) => {
            let kind = if mode == "bicycle" { "cycle" } else { "walking" };
            format!("{kind} route {r}")
        }
        (None, None) => String::new(),
    }
}

fn raw_route(r: &Relation, tags: &[(&str, &str)], mode: &str) -> RawRoute {
    let label = match (tag(tags, "ref"), tag(tags, "name")) {
        (Some(re), Some(n)) if !n.contains(re) => format!("{re} {n}"),
        (_, Some(n)) => n.to_string(),
        (Some(re), None) => re.to_string(),
        (None, None) => format!("relation {}", r.id()),
    };
    let label = plain_arrows(&label);
    let members = r
        .members()
        .filter_map(|m| {
            let role = m.role().unwrap_or("");
            let stopish = role.is_empty() || role.starts_with("stop") || role.starts_with("platform");
            match m.member_type {
                RelMemberType::Node if stopish => Some((false, m.member_id)),
                RelMemberType::Way if role.starts_with("platform") => Some((true, m.member_id)),
                _ => None,
            }
        })
        .collect();
    RawRoute {
        mode: mode.to_string(),
        night: is_night_route(tags, &label),
        label,
        members,
        interval_min: tag(tags, "interval").and_then(parse_interval),
    }
}

#[derive(Default)]
struct Nodes {
    coords: Vec<(i64, f64, f64)>,
    names: Vec<(i64, String)>,
    trees: Vec<(f64, f64)>,
    spots: Vec<Spot>,
    lively: Vec<(f64, f64)>,
}

/// Pass 1: nodes inside the circle, with the tags we use.
fn read_nodes(path: &Path, inside: &(dyn Fn(f64, f64) -> bool + Sync)) -> Result<Nodes, String> {
    let stop_like = |tags: &[(&str, &str)]| {
        tags.iter().any(|(k, v)| {
            matches!(*k, "public_transport" | "railway")
                || (*k == "highway" && *v == "bus_stop")
                || (*k == "amenity" && matches!(*v, "bus_station" | "ferry_terminal"))
        })
    };
    par_blocks(
        path,
        |block, acc: &mut Nodes| {
            let mut visit = |id: i64, la: f64, lo: f64, tags: &[(&str, &str)]| {
                if !inside(la, lo) {
                    return;
                }
                acc.coords.push((id, la, lo));
                if tag(tags, "natural") == Some("tree") {
                    acc.trees.push((la, lo));
                }
                if is_lively(tags) {
                    acc.lively.push((la, lo));
                }
                if let Some(kind) = spot_kind(tags) {
                    let name = tag(tags, "name").unwrap_or_default().to_string();
                    if kind == SpotKind::Venue || !name.is_empty() || tag(tags, "tourism") == Some("viewpoint") {
                        acc.spots.push(Spot { lat: la, lon: lo, kind, name });
                    }
                }
                if let Some(name) = tag(tags, "name").filter(|_| stop_like(tags)) {
                    acc.names.push((id, name.to_string()));
                }
            };
            for el in block.elements() {
                match el {
                    Element::DenseNode(n) => visit(n.id(), n.lat(), n.lon(), &n.tags().collect::<Vec<_>>()),
                    Element::Node(n) => visit(n.id(), n.lat(), n.lon(), &n.tags().collect::<Vec<_>>()),
                    _ => {}
                }
            }
        },
        |mut a, b| {
            a.coords.extend(b.coords);
            a.names.extend(b.names);
            a.trees.extend(b.trees);
            a.spots.extend(b.spots);
            a.lively.extend(b.lively);
            a
        },
    )
}

#[derive(Default)]
struct WaysAndRelations {
    ways: Vec<WayRec>,
    route_ways: Vec<(i64, String)>,
    bike_route_ways: Vec<i64>,
    routes: Vec<RawRoute>,
    platform_way_node: HashMap<i64, i64>,
    shade_ways: Vec<(Vec<i64>, bool)>,
    shade_mp_ways: Vec<i64>,
    traffic: Vec<Vec<i64>>,
    water: Vec<Water>,
    water_mp_ways: Vec<i64>,
    places: Vec<Place>,
    /// Outer ways of named multipolygons -> (name, kind).
    place_mp_ways: Vec<(i64, String, PlaceKind)>,
}

impl WaysAndRelations {
    fn add_way(&mut self, id: i64, refs: Vec<i64>, tags: &[(&str, &str)]) {
        let is_platform = tag(tags, "public_transport") == Some("platform")
            || matches!(tag(tags, "railway").or(tag(tags, "highway")), Some("platform"));
        if is_platform {
            self.platform_way_node.insert(id, refs[0]);
        }
        if let Some(line) = shade_kind(tags) {
            self.shade_ways.push((refs.clone(), line));
        }
        if tag(tags, "highway").is_some_and(|h| BUSY_ROADS.contains(&h)) {
            self.traffic.push(refs.clone());
        }
        if let Some(kind) = water_kind(tags) {
            self.water.push(Water { refs: refs.clone(), kind, shape: water_shape(tags) });
        }
        if let (Some(kind), Some(name)) = (place_kind(tags), tag(tags, "name")) {
            self.places.push(Place { name: name.to_string(), kind, refs: refs.clone() });
        }
        if let Some(WayUse { kind, foot, bike, cycle }) = classify(tags) {
            let steps = tag(tags, "highway") == Some("steps");
            let name = tag(tags, "name").or(tag(tags, "ref")).map(str::to_string);
            let bridge = tag(tags, "bridge").is_some_and(|b| b != "no");
            self.ways.push(WayRec { id, kind, foot, bike, cycle, steps, refs, name, wtype: way_type(tags), bridge });
        }
    }

    fn add_relation(&mut self, r: &Relation, tags: &[(&str, &str)]) {
        let outers = || {
            r.members()
                .filter(|m| m.member_type == RelMemberType::Way)
                .filter(|m| matches!(m.role(), Ok("outer" | "")))
                .map(|m| m.member_id)
        };
        match tag(tags, "type") {
            Some("multipolygon") => {
                if shade_kind(tags) == Some(false) {
                    self.shade_mp_ways.extend(outers());
                }
                if water_kind(tags).is_some() {
                    self.water_mp_ways.extend(outers());
                }
                if let (Some(kind), Some(name)) = (place_kind(tags), tag(tags, "name")) {
                    self.place_mp_ways.extend(outers().map(|w| (w, name.to_string(), kind)));
                }
            }
            Some("route") => match tag(tags, "route") {
                Some(mode) if WALKING_ROUTES.contains(&mode) => {
                    let name = route_name(tags, mode);
                    let ways = r.members().filter(|m| m.member_type == RelMemberType::Way);
                    if mode == "bicycle" {
                        self.bike_route_ways
                            .extend(r.members().filter(|m| m.member_type == RelMemberType::Way).map(|m| m.member_id));
                    }
                    self.route_ways.extend(ways.map(|m| (m.member_id, name.clone())));
                }
                Some(mode) if TRANSIT_MODES.contains(&mode) => self.routes.push(raw_route(r, tags, mode)),
                _ => {}
            },
            _ => {}
        }
    }

    fn merge(mut self, b: WaysAndRelations) -> WaysAndRelations {
        self.ways.extend(b.ways);
        self.route_ways.extend(b.route_ways);
        self.bike_route_ways.extend(b.bike_route_ways);
        self.routes.extend(b.routes);
        self.platform_way_node.extend(b.platform_way_node);
        self.shade_ways.extend(b.shade_ways);
        self.shade_mp_ways.extend(b.shade_mp_ways);
        self.traffic.extend(b.traffic);
        self.water.extend(b.water);
        self.water_mp_ways.extend(b.water_mp_ways);
        self.places.extend(b.places);
        self.place_mp_ways.extend(b.place_mp_ways);
        self
    }
}

/// Pass 2: ways touching the loaded nodes, and relations.
fn read_ways_and_relations(path: &Path, touches: &(dyn Fn(&[i64]) -> bool + Sync)) -> Result<WaysAndRelations, String> {
    par_blocks(
        path,
        |block, acc: &mut WaysAndRelations| {
            for el in block.elements() {
                match el {
                    Element::Way(w) => {
                        let refs: Vec<i64> = w.refs().collect();
                        if touches(&refs) {
                            acc.add_way(w.id(), refs, &w.tags().collect::<Vec<_>>());
                        }
                    }
                    Element::Relation(r) => acc.add_relation(&r, &r.tags().collect::<Vec<_>>()),
                    _ => {}
                }
            }
        },
        WaysAndRelations::merge,
    )
}

/// Pass 3: node lists of the given ways (multipolygon outlines), by id.
fn read_outlines(
    path: &Path,
    wanted: &HashSet<i64>,
    touches: &(dyn Fn(&[i64]) -> bool + Sync),
) -> Result<HashMap<i64, Vec<i64>>, String> {
    par_blocks(
        path,
        |block, acc: &mut HashMap<i64, Vec<i64>>| {
            for w in block.groups().flat_map(|g| g.ways()) {
                if wanted.contains(&w.id()) {
                    let refs: Vec<i64> = w.refs().collect();
                    if touches(&refs) {
                        acc.insert(w.id(), refs);
                    }
                }
            }
        },
        |mut x, y| {
            x.extend(y);
            x
        },
    )
}

/// Reads everything within `radius_m` of (lat, lon). Works on city extracts
/// and on large region files alike. `progress` gets a short status per pass.
pub fn read(path: &Path, lat: f64, lon: f64, radius_m: f64, progress: &dyn Fn(&str)) -> Result<OsmData, String> {
    let proj = Proj::new(lat, lon);
    let inside = |la: f64, lo: f64| norm(proj.xy(la, lo)) <= radius_m;

    progress("reading nodes");
    let nodes = read_nodes(path, &inside)?;
    let coords: HashMap<i64, (f64, f64)> = nodes.coords.into_iter().map(|(id, la, lo)| (id, (la, lo))).collect();
    let mut trees = nodes.trees;
    trees.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut spots = nodes.spots;
    spots.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut lively = nodes.lively;
    lively.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let touches = |refs: &[i64]| refs.iter().any(|r| coords.contains_key(r));

    progress("reading ways and relations");
    let mut p = read_ways_and_relations(path, &touches)?;
    // Blocks are read in parallel and merged in whatever order threads
    // finish: sort, so the same file always gives the same plan (ties in
    // routing and ring search otherwise break differently run to run).
    p.ways.sort_unstable_by_key(|w| w.id);
    p.routes.sort_by(|a, b| (&a.label, &a.mode, &a.members).cmp(&(&b.label, &b.mode, &b.members)));
    p.route_ways.sort();
    p.shade_ways.sort();
    p.traffic.sort();
    p.water.sort_by(|a, b| a.refs.cmp(&b.refs));
    p.places.sort_by(|a, b| (&a.name, &a.refs).cmp(&(&b.name, &b.refs)));
    p.shade_mp_ways.sort_unstable();
    p.water_mp_ways.sort_unstable();
    p.place_mp_ways.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));

    let mut shade_ways = p.shade_ways;
    let mut water = p.water;
    let mut places = p.places;
    let wanted: HashSet<i64> =
        p.shade_mp_ways.iter().chain(&p.water_mp_ways).copied().chain(p.place_mp_ways.iter().map(|w| w.0)).collect();
    if !wanted.is_empty() {
        progress("reading park and water outlines");
        let outlines = read_outlines(path, &wanted, &touches)?;
        let get = |id: &i64| outlines.get(id).cloned();
        water.extend(p.water_mp_ways.iter().filter_map(get).map(|refs| Water {
            refs,
            kind: WaterKind::Major,
            shape: WaterShape::Area,
        }));
        shade_ways.extend(p.shade_mp_ways.iter().filter_map(get).map(|refs| (refs, false)));
        places.extend(
            p.place_mp_ways.into_iter().filter_map(|(id, name, kind)| Some(Place { name, kind, refs: get(&id)? })),
        );
    }

    let routes: Vec<Route> = p
        .routes
        .into_iter()
        .map(|r| {
            let stops = r
                .members
                .iter()
                .filter_map(|&(is_way, id)| if is_way { p.platform_way_node.get(&id).copied() } else { Some(id) })
                .collect();
            Route { mode: r.mode, label: r.label, night: r.night, stops, interval_min: r.interval_min }
        })
        .filter(|r| r.stops.iter().any(|s| coords.contains_key(s)))
        .collect();

    // A way on several routes keeps the first walking route's name.
    let mut route_ways: HashMap<i64, String> = HashMap::default();
    for (w, n) in p.route_ways {
        route_ways.entry(w).or_insert(n);
    }

    Ok(OsmData {
        ways: p.ways,
        route_ways,
        bike_route_ways: p.bike_route_ways.into_iter().collect(),
        routes,
        coords,
        stop_names: nodes.names.into_iter().collect(),
        shade_ways,
        trees,
        traffic: p.traffic,
        water,
        places,
        spots,
        lively,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn intervals() {
        assert_eq!(super::parse_interval("10"), Some(10.0));
        assert_eq!(super::parse_interval("00:15"), Some(15.0));
        assert_eq!(super::parse_interval("0:07:30"), Some(7.5));
        assert_eq!(super::parse_interval("5-10"), Some(10.0));
        assert_eq!(super::parse_interval("often"), None);
    }

    #[test]
    fn route_names() {
        use super::route_name;
        assert_eq!(route_name(&[("name", "Lower Don Trail"), ("ref", "6")], "bicycle"), "Lower Don Trail");
        assert_eq!(route_name(&[("ref", "6")], "bicycle"), "cycle route 6");
        assert_eq!(route_name(&[("ref", "GR 2")], "hiking"), "walking route GR 2");
        assert_eq!(route_name(&[], "foot"), "");
    }

    #[test]
    fn classifies_ways() {
        use super::{Kind, classify};
        assert_eq!(classify(&[("highway", "footway")]).map(|w| w.kind), Some(Kind::CarFree));
        assert_eq!(classify(&[("highway", "footway"), ("footway", "sidewalk")]).map(|w| w.kind), Some(Kind::Road));
        assert_eq!(classify(&[("highway", "residential")]).map(|w| w.kind), Some(Kind::Road));
        assert_eq!(classify(&[("highway", "primary")]), None);
        assert_eq!(classify(&[("highway", "primary"), ("bridge", "yes")]).map(|w| w.kind), Some(Kind::Road));
        assert_eq!(classify(&[("highway", "trunk"), ("bridge", "yes")]), None);
        assert_eq!(classify(&[("highway", "path"), ("foot", "no")]).map(|w| (w.foot, w.bike)), Some((false, true)));
        assert_eq!(classify(&[("highway", "path"), ("foot", "no"), ("bicycle", "no")]), None);
        assert_eq!(classify(&[("highway", "footway")]).map(|w| (w.foot, w.bike)), Some((true, false)));
        assert!(classify(&[("highway", "cycleway")]).unwrap().cycle);
        assert!(classify(&[("highway", "residential"), ("cycleway:right", "lane")]).unwrap().cycle);
        assert_eq!(classify(&[("highway", "steps"), ("bicycle", "yes")]).map(|w| w.bike), Some(false));
    }
}
