//! Water outlines near the extract, as segments: what makes a way waterside
//! or a shore.

use crate::graph::{Proj, Xy, project_lines};
use crate::osm::{OsmData, WaterKind};
use rstar::primitives::Line;

/// Water areas drawn as one closed way count as shores from this size.
const MAJOR_AREA_M2: f64 = 200_000.0;

/// Water outlines and lines near the extract, as segments. With
/// `shores_only`, just real shores: coast, rivers and big water bodies,
/// not ponds, canals or streams.
pub fn segments(osm: &OsmData, proj: &Proj, radius_m: f64, shores_only: bool) -> Vec<Line<Xy>> {
    let is_shore = |w: &crate::osm::Water| match w.kind {
        WaterKind::Major => true,
        WaterKind::Minor => false,
        WaterKind::Area => {
            let pts: Vec<Xy> =
                w.refs.iter().filter_map(|id| osm.coords.get(id)).map(|&(la, lo)| proj.xy(la, lo)).collect();
            polygon_area(&pts) >= MAJOR_AREA_M2
        }
    };
    let kept = osm.water.iter().filter(|w| !shores_only || is_shore(w)).map(|w| &w.refs);
    project_lines(osm, proj, kept, radius_m + 2000.0)
}

/// Area enclosed by a closed outline (shoelace formula).
pub(crate) fn polygon_area(pts: &[Xy]) -> f64 {
    pts.windows(2).map(|w| w[0][0] * w[1][1] - w[1][0] * w[0][1]).sum::<f64>().abs() / 2.0
}
