//! A simple base map under the walk graph: water and parks filled in,
//! rivers, canals and shores, and busy roads. Fills are worked out on a
//! raster and handed to the map as triangles, once per graph.
//!
//! Water is filled from three kinds of evidence:
//! - closed water outlines (lakes, riverbanks, basins): the inside is water;
//! - coastline: OSM draws it with the sea on its right;
//! - pieces of water outlines cut off by the edge of the extract (big
//!   lakes): the water side is the one without walkable ways beside it.
//!
//! The outlines are walls; the area on a water side, up to the next wall,
//! is water. If an outline has a gap, that area runs out over the land, so
//! any area with more than a few cells of walkable ways is left unfilled.

use crate::graph::{Graph, Proj, Xy, dist, lerp, norm};
use crate::osm::{OsmData, WaterKind, WaterShape};
use crate::shade::assemble;

/// Raster cell size: at least this, and coarse enough for at most
/// `MAX_CELLS` cells across.
const CELL_M: f64 = 15.0;
const MAX_CELLS: usize = 2500;
/// A cell with a walkable way within this distance is land.
const LAND_M: f64 = 15.0;
/// A water side is judged by whether there are walkable ways (land cells)
/// this far out from a piece of outline.
const SIDE_M: f64 = 30.0;
/// Water reaches cells up to this many times further from the water side
/// of an outline than from the nearest land.
const LAND_WEIGHT: u64 = 4;

/// Base map geometry in local metres, as flat lists of points: triangles
/// for fills, pairs for line segments.
#[derive(Default)]
pub struct Basemap {
    pub water: Vec<Xy>,
    pub parks: Vec<Xy>,
    pub water_lines: Vec<Xy>,
    pub roads: Vec<Xy>,
}

impl Basemap {
    pub fn build(osm: &OsmData, proj: &Proj, g: &Graph, parks: &[Vec<Xy>], radius_m: f64) -> Basemap {
        let xy = |refs: &[i64]| -> Vec<Xy> {
            refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect()
        };
        let near = |pts: &[Xy]| pts.iter().any(|&p| norm(p) <= radius_m + 2000.0);
        // The loaded data's extent: city extracts are rectangles, and
        // outlines stop at their edge.
        let (lo, hi) = osm
            .coords
            .values()
            .map(|&(la, lo)| proj.xy(la, lo))
            .fold(([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]), |(lo, hi), p| {
                ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])])
            });
        let extent = Extent { lo, hi };
        let mut grid = Grid::new(radius_m, Some(&extent));

        // Land: cells along walkable ways (not bridges, which are over water).
        for e in g.edges.iter().filter(|e| !g.ways[e.way as usize].bridge) {
            for w in e.pts.windows(2) {
                grid.mark(w[0], w[1], LAND_M, LAND);
            }
        }

        // Water outlines: closed ones are filled; coastline and open pieces
        // are walls with a known (or guessed) water side.
        let mut water_lines = vec![];
        let mut sided: Vec<(Vec<Xy>, bool)> = vec![]; // (line, water on the right?)
        let mut area_pieces = vec![];
        for w in &osm.water {
            let pts = xy(&w.refs);
            if pts.len() < 2 || !near(&pts) {
                continue;
            }
            push_segments(&mut water_lines, &pts);
            match (w.shape, w.kind) {
                (WaterShape::Coast, _) => sided.push((pts, true)),
                (WaterShape::Line, _) | (_, WaterKind::Minor) => {}
                (WaterShape::Area, _) => area_pieces.push(w.refs.clone()),
            }
        }
        let (closed, open) = assemble(area_pieces);
        let has_land = |p: Xy| grid.index(p).is_some_and(|i| grid.cells[i] == LAND);
        let mut filled = vec![];
        for ring in &closed {
            let pts = xy(ring);
            if pts.len() >= 4 && near(&pts) {
                filled.push(pts);
            }
        }
        for piece in &open {
            let pts = xy(piece);
            if pts.len() >= 2
                && near(&pts)
                && let Some(right) = water_side(&pts, &has_land)
            {
                sided.push((pts, right));
            }
        }
        for (pts, _) in &sided {
            for w in pts.windows(2) {
                grid.wall(w[0], w[1]);
            }
            // Close the gap between an outline cut off by the edge of the
            // data and that edge.
            for end in [pts[0], pts[pts.len() - 1]] {
                if let Some(edge) = extent.nearest_edge(end, EDGE_SNAP_M) {
                    grid.wall(end, edge);
                }
            }
        }
        let mut seeds = vec![];
        for (pts, right) in &sided {
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                let len = dist(a, b);
                if len == 0.0 {
                    continue;
                }
                let side = if *right { 1.0 } else { -1.0 };
                let normal = [side * (b[1] - a[1]) / len, -side * (b[0] - a[0]) / len];
                let mid = lerp(a, b, 0.5);
                let off = 1.5 * grid.cell;
                seeds.push((grid.index([mid[0] + normal[0] * off, mid[1] + normal[1] * off]), true));
                seeds.push((grid.index([mid[0] - normal[0] * off, mid[1] - normal[1] * off]), false));
            }
        }
        grid.flood(&seeds);
        for pts in &filled {
            grid.fill_polygon(pts, WATER);
        }
        let water = grid.triangles(WATER);

        let mut park_grid = Grid::new(radius_m, None);
        for a in parks {
            park_grid.fill_polygon(a, WATER);
        }
        let parks = park_grid.triangles(WATER);

        let mut roads = vec![];
        for refs in &osm.traffic {
            let pts = xy(refs);
            if near(&pts) {
                push_segments(&mut roads, &pts);
            }
        }
        Basemap { water, parks, water_lines, roads }
    }
}

fn push_segments(out: &mut Vec<Xy>, pts: &[Xy]) {
    for w in pts.windows(2) {
        out.extend_from_slice(&[w[0], w[1]]);
    }
}

/// Which side of an outline piece the water is on: true for the right,
/// from where walkable ways are (water has none). None if it can't tell.
fn water_side(pts: &[Xy], has_land: &impl Fn(Xy) -> bool) -> Option<bool> {
    let (mut left, mut right) = (0, 0);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = dist(a, b);
        if len == 0.0 {
            continue;
        }
        let n = [(b[1] - a[1]) / len * SIDE_M, -(b[0] - a[0]) / len * SIDE_M];
        let mid = lerp(a, b, 0.5);
        right += has_land([mid[0] + n[0], mid[1] + n[1]]) as usize;
        left += has_land([mid[0] - n[0], mid[1] - n[1]]) as usize;
    }
    match left.cmp(&right) {
        std::cmp::Ordering::Greater => Some(true),
        std::cmp::Ordering::Less => Some(false),
        std::cmp::Ordering::Equal => None,
    }
}

/// Outlines ending this close to the edge of the data are extended to it.
const EDGE_SNAP_M: f64 = 2000.0;

/// The rectangle the loaded data covers.
struct Extent {
    lo: Xy,
    hi: Xy,
}

impl Extent {
    fn contains(&self, p: Xy) -> bool {
        (self.lo[0]..=self.hi[0]).contains(&p[0]) && (self.lo[1]..=self.hi[1]).contains(&p[1])
    }

    /// The nearest point on the edge, if within `within`.
    fn nearest_edge(&self, p: Xy, within: f64) -> Option<Xy> {
        let candidates = [[self.lo[0], p[1]], [self.hi[0], p[1]], [p[0], self.lo[1]], [p[0], self.hi[1]]];
        candidates.into_iter().filter(|&q| dist(p, q) <= within).min_by(|&a, &b| dist(p, a).total_cmp(&dist(p, b)))
    }
}

const EMPTY: u8 = 0;
const LAND: u8 = 1;
const WALL: u8 = 2;
const WATER: u8 = 3;

/// A raster centred on the base. Cells outside the circle of the given
/// radius are walls: outlines are only complete within the loaded data, and
/// beyond it the sea would run round the ends of the coastline.
struct Grid {
    cell: f64,
    half: f64,
    n: usize,
    cells: Vec<u8>,
}

impl Grid {
    fn new(radius_m: f64, extent: Option<&Extent>) -> Grid {
        let half = radius_m;
        let cell = CELL_M.max(2.0 * half / MAX_CELLS as f64);
        let n = (2.0 * half / cell).ceil() as usize + 1;
        let mut cells = vec![EMPTY; n * n];
        for (i, c) in cells.iter_mut().enumerate() {
            let centre = [((i % n) as f64 + 0.5) * cell - half, ((i / n) as f64 + 0.5) * cell - half];
            if norm(centre) > radius_m || extent.is_some_and(|e| !e.contains(centre)) {
                *c = WALL;
            }
        }
        Grid { cell, half, n, cells }
    }

    fn col_row(&self, p: Xy) -> Option<(usize, usize)> {
        let (c, r) = (((p[0] + self.half) / self.cell).floor(), ((p[1] + self.half) / self.cell).floor());
        (c >= 0.0 && r >= 0.0 && (c as usize) < self.n && (r as usize) < self.n).then_some((c as usize, r as usize))
    }

    fn index(&self, p: Xy) -> Option<usize> {
        self.col_row(p).map(|(c, r)| r * self.n + c)
    }

    /// Marks cells within `width` of the segment a-b (not walls).
    fn mark(&mut self, a: Xy, b: Xy, width: f64, value: u8) {
        let steps = (dist(a, b) / (self.cell / 2.0)).ceil().max(1.0) as usize;
        let k = (width / self.cell).ceil() as isize;
        for s in 0..=steps {
            let Some((c, r)) = self.col_row(lerp(a, b, s as f64 / steps as f64)) else { continue };
            for dr in -k..=k {
                for dc in -k..=k {
                    let (cc, rr) = (c as isize + dc, r as isize + dr);
                    if cc >= 0 && rr >= 0 && (cc as usize) < self.n && (rr as usize) < self.n {
                        let i = rr as usize * self.n + cc as usize;
                        if self.cells[i] == EMPTY {
                            self.cells[i] = value;
                        }
                    }
                }
            }
        }
    }

    /// Marks the cells a segment passes through as wall, as an unbroken
    /// chain (so nothing floods across it).
    fn wall(&mut self, a: Xy, b: Xy) {
        let steps = (dist(a, b) / (self.cell / 3.0)).ceil().max(1.0) as usize;
        let mut last: Option<(usize, usize)> = None;
        for s in 0..=steps {
            let Some((c, r)) = self.col_row(lerp(a, b, s as f64 / steps as f64)) else {
                last = None;
                continue;
            };
            // A diagonal step leaves a gap a 4-connected flood can't pass,
            // but fill the corner anyway to be safe.
            if let Some((lc, lr)) = last
                && lc != c
                && lr != r
            {
                self.cells[lr * self.n + c] = WALL;
            }
            self.cells[r * self.n + c] = WALL;
            last = Some((c, r));
        }
    }

    /// Fills water from the seeds (true: water side of an outline, false:
    /// land side). Walls aside, a cell is water when the water side is
    /// nearer than `LAND_WEIGHT` times the nearest land (walkable ways, land
    /// sides), both measured around walls: behind a shore the water is a
    /// long way round, and where an outline has a gap, water spreads only a
    /// little way between the streets.
    fn flood(&mut self, seeds: &[(Option<usize>, bool)]) {
        let n = self.n;
        let pick = |water: bool| seeds.iter().filter(move |s| s.1 == water).filter_map(|s| s.0);
        let to_water = self.distances(pick(true));
        let land_cells = (0..n * n).filter(|&i| self.cells[i] == LAND);
        let to_land = self.distances(pick(false).chain(land_cells));
        for i in 0..n * n {
            let wet = self.cells[i] == EMPTY
                && to_water[i] != u32::MAX
                && (to_water[i] as u64) < LAND_WEIGHT * to_land[i].min(u32::MAX / 2) as u64;
            if wet {
                self.cells[i] = WATER;
            }
        }
        // Walls beside water are water too (no seam along the shore).
        for i in 0..n * n {
            if self.cells[i] == WALL {
                let (c, r) = (i % n, i / n);
                let wet_next = (c > 0 && self.cells[i - 1] == WATER)
                    || (c + 1 < n && self.cells[i + 1] == WATER)
                    || (r > 0 && self.cells[i - n] == WATER)
                    || (r + 1 < n && self.cells[i + n] == WATER);
                if wet_next {
                    self.cells[i] = WATER;
                }
            }
        }
    }

    /// Steps (4-connected, around walls) from the nearest of `from`.
    fn distances(&self, from: impl Iterator<Item = usize>) -> Vec<u32> {
        let n = self.n;
        let mut d = vec![u32::MAX; n * n];
        let mut queue = std::collections::VecDeque::new();
        for i in from {
            if self.cells[i] != WALL && d[i] == u32::MAX {
                d[i] = 0;
                queue.push_back(i);
            }
        }
        while let Some(i) = queue.pop_front() {
            let (c, r) = (i % n, i / n);
            let next = [
                (c > 0).then(|| i - 1),
                (c + 1 < n).then(|| i + 1),
                (r > 0).then(|| i - n),
                (r + 1 < n).then(|| i + n),
            ];
            for j in next.into_iter().flatten() {
                if d[j] == u32::MAX && self.cells[j] != WALL {
                    d[j] = d[i] + 1;
                    queue.push_back(j);
                }
            }
        }
        d
    }

    /// Sets every cell whose centre is inside the polygon (even-odd rule).
    fn fill_polygon(&mut self, pts: &[Xy], value: u8) {
        let (lo, hi) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
        let r0 = (((lo + self.half) / self.cell).floor().max(0.0)) as usize;
        let r1 = (((hi + self.half) / self.cell).ceil().max(0.0) as usize).min(self.n);
        let mut xs = vec![];
        for r in r0..r1 {
            let y = (r as f64 + 0.5) * self.cell - self.half;
            xs.clear();
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                if (a[1] <= y) != (b[1] <= y) {
                    xs.push(a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                }
            }
            xs.sort_by(f64::total_cmp);
            for pair in xs.as_chunks::<2>().0 {
                let c0 = ((pair[0] + self.half) / self.cell - 0.5).ceil().max(0.0) as usize;
                let c1 = (((pair[1] + self.half) / self.cell - 0.5).floor() + 1.0).max(0.0) as usize;
                for c in c0..c1.min(self.n) {
                    self.cells[r * self.n + c] = value;
                }
            }
        }
    }

    /// Runs of cells with `value` as rectangles, as triangles; runs that
    /// repeat on the next row are merged into one taller rectangle.
    fn triangles(&self, value: u8) -> Vec<Xy> {
        let n = self.n;
        let runs = |r: usize| {
            let row = &self.cells[r * n..(r + 1) * n];
            let mut out = vec![];
            let mut c = 0;
            while c < n {
                if row[c] == value {
                    let start = c;
                    while c < n && row[c] == value {
                        c += 1;
                    }
                    out.push((start, c));
                } else {
                    c += 1;
                }
            }
            out
        };
        let mut tris = vec![];
        // Open rectangles: (c0, c1) -> first row.
        let mut open: Vec<((usize, usize), usize)> = vec![];
        let emit = |(c0, c1): (usize, usize), r0: usize, r1: usize, tris: &mut Vec<Xy>| {
            let x = |c: usize| c as f64 * self.cell - self.half;
            let y = |r: usize| r as f64 * self.cell - self.half;
            let (a, b, c, d) = ([x(c0), y(r0)], [x(c1), y(r0)], [x(c1), y(r1)], [x(c0), y(r1)]);
            tris.extend_from_slice(&[a, b, c, a, c, d]);
        };
        for r in 0..=n {
            let row = if r < n { runs(r) } else { vec![] };
            let mut next = vec![];
            for (span, r0) in open.drain(..) {
                if row.contains(&span) {
                    next.push((span, r0));
                } else {
                    emit(span, r0, r, &mut tris);
                }
            }
            for span in row {
                if !next.iter().any(|(s, _)| *s == span) {
                    next.push((span, r));
                }
            }
            open = next;
        }
        tris
    }
}

#[cfg(test)]
mod tests {
    use super::{Grid, WALL, WATER};

    #[test]
    fn fills_and_merges_rectangles() {
        let mut g = Grid::new(100.0, None);
        // A 60 x 60 m square.
        g.fill_polygon(&[[0.0, 0.0], [60.0, 0.0], [60.0, 60.0], [0.0, 60.0], [0.0, 0.0]], WATER);
        let tris = g.triangles(WATER);
        assert_eq!(tris.len(), 6, "one rectangle");
        let area: f64 = tris
            .chunks(3)
            .map(|t| {
                ((t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[2][0] - t[0][0]) * (t[1][1] - t[0][1])).abs() / 2.0
            })
            .sum();
        assert!((area - 3600.0).abs() < 1.0, "{area}");
    }

    #[test]
    fn floods_the_water_side_only() {
        let mut g = Grid::new(300.0, None);
        // A wall straight across; water seeds above, land seeds below.
        g.wall([-300.0, 0.0], [300.0, 0.0]);
        let above = g.index([0.0, 50.0]);
        let below = g.index([0.0, -50.0]);
        g.flood(&[(above, true), (above, true), (below, false)]);
        assert_eq!(g.cells[above.unwrap()], WATER);
        assert_ne!(g.cells[below.unwrap()], WATER);
        assert_eq!(g.cells[g.index([0.0, 0.0]).unwrap()], WATER, "walls beside water count as water");
        assert_eq!(g.cells[g.index([-290.0, -290.0]).unwrap()], WALL, "outside the circle, beside land");
    }
}
