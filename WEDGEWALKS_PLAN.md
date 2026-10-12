# WEDGEWALKS: Ring-route travel planner — plan

## Purpose

Find cities worldwide suited to a specific kind of travel, and plan that travel.

The traveller has very limited mobility and energy. The trip style is **hub-and-spoke ring completion**:

- Stay in one base near a city center.
- On a good day: travel from the center out to a point on a roughly circular route around the city (by transit, or on foot if short), walk or bike one arc of the ring, then travel back to the center.
- Rest days (one or several) happen between outings, at no fixed rhythm.
- Next outing does the next arc. Repeat until the whole ring is done.

The ring completion is the point. Generic unstructured day trips from a hub do not satisfy the goal. Output must be an ordered set of arcs that together cover one closed ring.

## Requirements

### Traveller constraints
- Cannot drive.
- Does not want to walk or bike along roads with car traffic. Car-free paths strongly preferred; roads acceptable only as short gaps, and penalized.
- Limited energy per outing: arcs must be short (see parameters).
- Needs regular rest days; plan must not assume consecutive outings.
- Base must be near a city center with good access to celiac-safe (gluten-free) food.

### City constraints
- Temperate climate: decent weather, cool rather than hot, ideally shady. Not hot and sunny.
- Reachable on a Canadian passport (visa-free or easy entry).
- Relatively safe for strangers doing long walks.
- Has a transit network that reaches points on the ring from the center.

### Technical constraints
- Language: Rust.
- Runs offline on local data. Network use only for downloading data up front.
- Primary data source: OpenStreetMap (.osm.pbf extracts, e.g. from Geofabrik).
- Keep the design simple. Prefer the smallest thing that works. Do not add abstractions, plugin systems, databases, or services unless clearly required.

## Parameters (config file, defaults are guesses to be confirmed by the user)

| Parameter | Default | Notes |
|---|---|---|
| `arc_length_km` | 4–8 | Per outing. Mode: walk. Bike mode optional later. |
| `ring_radius_km` | 3–15 | Distance band from center the ring must stay within. |
| `ring_total_km` | 15–60 | Whole ring. |
| `max_road_gap_m` | 500 | Longest single road stretch tolerated in a ring. |
| `max_road_fraction` | 0.15 | Total road length / ring length. |
| `center_access` | transit or walk ≤ 1 km | How the traveller reaches an arc endpoint. Taxi: open question. |
| `extract_radius_km` | 25 | Area cut around each city center. |

Open questions for the user (do not invent answers; keep as parameters):
1. Realistic arc length, and walk vs. bike.
2. Is a taxi acceptable to reach an arc endpoint when transit doesn't?
3. Acceptable total ring length.
4. How much road is tolerable in a "completed" ring.

## Data

- **OSM extracts**: per-country or per-region .osm.pbf from Geofabrik. Never load the planet. Cut a radius around each candidate city (`osmium extract` CLI, or filter in Rust while parsing).
- **Candidate cities**: a hand-maintained CSV. Columns: `name, country, lat, lon, population, climate_note, summer_high_c, passport_ok, safety_ok, notes`. Climate, passport, and safety are NOT in OSM; the user fills these in. Start with ~10–100 cities.
- **Transit**: OSM `route=bus|tram|subway|train|light_rail|ferry` relations and `public_transport=*` stops. Gives stop order, not schedules. GTFS (schedules/frequency) is out of scope for v1.
- **Food**: OSM `diet:gluten_free=yes|only` near the center. Coverage is spotty; treat as a weak signal, report counts, user checks manually.

### OSM tags

Car-free walk/bike edges (low cost):
- `highway=footway|path|cycleway|pedestrian|track|bridleway|steps`
- `highway=service` only with `access=no|private` for cars, or `motor_vehicle=no`
- members of `route=hiking|foot|bicycle` relations get a bonus

Road edges (high cost, allowed as gaps):
- `highway=residential|living_street|unclassified|tertiary|service`
- Exclude: `motorway`, `trunk`, `primary`, `secondary` and their `_link`s, and anything with `foot=no`.

Treat `sidewalk=*` on roads as still "road" (traffic adjacent).

## Prototype (v1) — one city, end to end

Goal: prove that ring-finding works on one real city. No UI.

Steps:
1. **Workspace**: single binary crate. Crates: `osmpbf` (parsing), `petgraph` (graph), `geo` (geometry), `rstar` (spatial index), `serde` + `toml` (config), `geojson` (output). Add others only if needed.
2. **Input**: one city (user picks one they know), lat/lon, a pre-cut .osm.pbf.
3. **Parse**: read nodes and ways; keep ways matching the tag lists above. Build an undirected graph: nodes = OSM nodes at way junctions/ends, edges = way segments with `length_m` and `kind: CarFree | Road`.
4. **Dump**: write the graph as GeoJSON (car-free and road edges distinguished by a property). User checks it in QGIS or geojson.io.
5. **Find rings**:
   - Restrict to edges inside the `ring_radius_km` annulus.
   - Planar face extraction on the graph (treat it as planar; OSM bridges/tunnels may break this — ignore for v1, note failures).
   - Keep faces whose polygon contains the center.
   - Score each: length, road fraction, longest road gap, radius variance. Reject anything violating parameters.
   - Output top N rings as GeoJSON.
   - If face extraction gives poor results (too fragmented, rings broken by tiny gaps), fallback: add road edges with high cost and search again. Do not over-engineer before seeing real output.
6. **Split into arcs**: walk around the chosen ring, cut into consecutive arcs of ~`arc_length_km`, preferring cut points near transit stops.
7. **Access**: for each arc start/end, find the nearest transit stop within walking distance, and check there's an OSM route relation that also has a stop near the city center. Report "reachable / not reachable" per arc endpoint. No routing across transfers in v1.
8. **Output**: `rings.geojson`, `arcs.geojson`, and a plain-text report: ring length, arc list (length, start/end stop, route used), road fraction, gluten-free place count near center.

Done when: on the test city, the tool produces at least one ring that the user agrees is real and usable, split into sensible arcs.

## Full version (v2+) — many cities, ranking, optional UI

Do these only after v1 works. Each step is independent; do them in order of usefulness.

1. **Batch over city list**: read the CSV, filter on `passport_ok`, `safety_ok`, climate; for each city cut the extract (shell out to `osmium` or filter during parse) and run the v1 pipeline. Cache parsed graphs per city on disk (e.g. `bincode`) so re-runs are fast.
2. **Ranking**: one score per city from best ring score, number of arcs with transit access at both ends, gluten-free count near center. Output a ranked CSV/text report. Keep the formula simple and in config.
3. **Transit transfers**: allow one transfer between route relations to reach an arc endpoint.
4. **Base location**: pick a center point that minimizes transit access cost to all arc endpoints, rather than using the CSV lat/lon.
5. **Itinerary output**: ordered day list (arc N: go to stop X by route R, walk arc, return from stop Y by route S). Rest days are not scheduled; arcs are just ordered.
6. **Shade signal (optional)**: fraction of the ring within N m of `natural=wood`, `landuse=forest`, `leisure=park`, or `natural=tree_row`. Weak heuristic.
7. **Bike mode (optional)**: separate arc length; bike-legal edges only.
8. **GTFS (optional)**: if a city publishes GTFS, use it for frequency/hours of service at arc endpoints.
9. **egui viewer (optional)**: only if GeoJSON + QGIS gets tedious. Draw own geometry (graph edges, ring, arcs, stops) on a plain canvas with pan/zoom. No basemap tiles. Pick city from list, show ranked rings, click arc to see its access info.

## Non-goals

- No web app, server, or database.
- No live data or online routing APIs.
- No basemap tile rendering.
- No automatic climate/safety/visa data; the user maintains that in the CSV.
- No scheduling of rest days or booking anything.

## Known risks

- **Ring finding is the riskiest part.** No standard algorithm known; planar face extraction is a starting guess. Validate on a city with a known ring early.
- OSM graph is not truly planar (bridges, tunnels, layers). Expect odd faces.
- Gluten-free tagging in OSM is incomplete.
- Transit relations in OSM vary in quality by country.
