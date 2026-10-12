# The planner (`src/plan.rs`)

Finds D days of K km round the base: a closed ring, or where water leaves nothing
to walk on, a chain from shore to shore. Step numbers match the code.

**Inputs:** base, walk graph, transit, trailheads, shade, street life; days D,
day km K, travel (foot/bike), access (transit/from base), way preferences,
centre offset.

1. **Network.** Walkable graph (water never crossed), restricted to the part
   connected to the base, minus dead ends: anything beyond a graph bridge, away
   from the base, under a third of the network. Edges over 500 m are split.
2. **Way cost** (metres): path 1, path beside traffic 1.5, quiet street f, busy
   road 1.8f (f = road avoidance); walking-route and waterside bonuses; minus shade
   and street life; plus climb. By bike: only bikeable ways; cycleways/lanes/tracks
   and bike routes much cheaper, quiet streets 1/5, climb dearer.
3. **Centre.** Plan round the base itself and round the middle of the land if
   within `centre offset` of the base; all results compete.
4. **Profile.** For each 5° bearing, the radius ρ(θ) to run at: R if there is way
   out to R, else the farthest way in that bearing (shore or data edge).
   Bearings with no land (< 0.1R or 600 m), or a pulled-in stretch too short for a
   day, are gaps; short gaps are bridged. Smooth ρ (median of 5, never beyond land).
5. **Size.** Bisect R so winding × perimeter = D × one day's arc. A gap counts as
   land at R (omitting costs a day; retracting doesn't). Arc: transit 0.85K;
   from base K minus walks out and home. If the map is too small, R stops at the
   maximum and the plan has fewer days (stated). Winding starts at 1.35; if found
   rings differ by > 6%, re-size with measured winding.
6. **Anchors.** One per ~400 m at 0.6–1.4ρ (shore nodes may be nearer in), chosen
   by trailhead score, street life, shore, path over street. Transit mode: need
   transit both ways (relaxed for sparse towns). From base: must be reachable in a day.
7. **Legs.** From each anchor, cheapest route to each anchor 5°–150° further round,
   ≤ K long, inside a corridor 0.6–1.5ρ (wider at jumps in ρ; shore ways may
   stray), never going back round the centre by > 15°.
8. **Chain.** DP over bearing choosing exactly N legs (N = days for the profile ±
   a few; plans ranked by closeness to D). Leg length near its day's allowance
   (transit K; from base K minus walks), ≤ 2.2× its end-to-end distance. Cost: leg
   quality, trailhead quality, transit access, distance from ρ, climb over limit.
   Closed: back to start, from 24 starting points. Open: from one end of the land to
   the other, near shore anchors (inland ends cost more); end days may be short.
9. **Route.** Re-route each leg with A* charging turns (90 m sharp, more for
   changing street) and ways already used by an earlier day (never reused, never
   walked back along), so routes are simple and days share no streets.
10. **Check and rank.** Road share, road gap, doubling back, climb, days found vs
    wanted, goes round the base, outings reachable. Valid first, then score.

## Lessons

- Radius belongs to the anchors, not the route between them.
- The day count is the number of legs, not a window.
- Retract to a shore where there is one; omit only what has no land.
- Beyond a single way in is a dead end; out-and-back days come from there.
- Land is where there is way, not nodes. Winding: ~1.25 dense, ~1.5 hilly/watery.

## Hand edits (`src/manual.rs`)

A list of anchors and per-day routing points, each snapped to the nearest way;
stretches routed as step 9 without the corridor or centre rule. The result is an
ordinary plan (checked, cut into days), always ranked first, never merged, saved per city.
