//! Plans edited by hand. The planner's plans are good guesses; local knowledge beats them. A manual plan
//! is a list of *anchors* (where each day starts and ends) and, for each day, *routing points* its route
//! must pass through, in order. The routes between them are found as for any plan, so a manual plan is
//! ranked, cut into days and described like the others, and kept (per city) when the base moves.

use crate::config::Travel;
use crate::graph::{Proj, Xy, dist};
use crate::rings::Ring;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manual {
    pub name: String,
    /// How it is travelled: its routes use only ways that can be walked or cycled.
    pub travel: Travel,
    /// A ring comes back to where it started; anything else is a chain.
    pub closed: bool,
    /// Where each day starts and ends, as (lat, lon). A ring repeats its first at the end.
    pub anchors: Vec<[f64; 2]>,
    /// For each day, points its route must pass through, in order, as (lat, lon).
    pub vias: Vec<Vec<[f64; 2]>>,
}

impl Manual {
    /// A manual plan with the same anchors as `r` and no routing points yet.
    pub fn from_ring(r: &Ring, proj: &Proj, name: String, travel: Travel) -> Manual {
        let ll = |p: Xy| {
            let (lon, lat) = proj.lonlat(p);
            [lat, lon]
        };
        let anchors: Vec<[f64; 2]> = r.marks.iter().map(|&p| ll(p)).collect();
        Manual { name, travel, closed: !r.is_open(), vias: vec![vec![]; anchors.len().saturating_sub(1)], anchors }
    }

    pub fn days(&self) -> usize {
        self.anchors.len().saturating_sub(1)
    }

    /// Moves anchor `i` (a ring's first and last are one place).
    pub fn move_anchor(&mut self, i: usize, to: [f64; 2]) {
        let last = self.anchors.len() - 1;
        self.anchors[i] = to;
        if self.closed && (i == 0 || i == last) {
            self.anchors[0] = to;
            self.anchors[last] = to;
        }
    }

    /// Removes anchor `i`, merging the two days either side of it (and their routing points). A plan
    /// keeps at least two days.
    pub fn remove_anchor(&mut self, i: usize) {
        if self.days() <= 2 {
            return;
        }
        let last = self.anchors.len() - 1;
        if self.closed && (i == 0 || i == last) {
            // Drop the shared start: the first day and the last merge into a new one that starts at the next anchor.
            self.anchors.remove(last);
            self.anchors.remove(0);
            let mut first = self.vias.remove(0);
            let end = self.vias.pop().unwrap_or_default();
            // The old last day then the old first day, as one leg round the old start.
            let mut merged = end;
            merged.append(&mut first);
            self.vias.push(merged);
            let start = self.anchors[0];
            self.anchors.push(start);
            return;
        }
        if i == 0 || i == last {
            // A chain's end: that day goes.
            self.anchors.remove(i);
            self.vias.remove(if i == 0 { 0 } else { self.vias.len() - 1 });
            return;
        }
        self.anchors.remove(i);
        let mut after = self.vias.remove(i);
        self.vias[i - 1].append(&mut after);
    }

    /// Adds a routing point to day `leg`, in order along it (`along` is how far along the day's current
    /// route the point is, and `positions` are those of the day's existing points).
    pub fn add_via(&mut self, leg: usize, at: [f64; 2], along: f64, positions: &[f64]) {
        let k = positions.iter().take_while(|&&p| p < along).count().min(self.vias[leg].len());
        self.vias[leg].insert(k, at);
    }

    /// Splits day `leg` at a new anchor; the day's routing points go to whichever half they are in.
    pub fn split_day(&mut self, leg: usize, at: [f64; 2], along: f64, positions: &[f64]) {
        let k = positions.iter().take_while(|&&p| p < along).count().min(self.vias[leg].len());
        let after = self.vias[leg].split_off(k);
        self.vias.insert(leg + 1, after);
        self.anchors.insert(leg + 1, at);
    }
}

/// The route of day `leg` of a ring (the part of its outline between its cut points).
pub fn leg_pts(r: &Ring, leg: usize) -> Vec<Xy> {
    let (Some(&from), Some(&to)) = (r.cuts.get(leg), r.cuts.get(leg + 1)) else { return vec![] };
    let mut out = vec![];
    let mut s = 0.0;
    for w in r.pts.windows(2) {
        let l = dist(w[0], w[1]);
        if s + l >= from && s <= to {
            if out.is_empty() {
                out.push(w[0]);
            }
            out.push(w[1]);
        }
        s += l;
    }
    out
}

/// How far along `pts` the nearest point to `p` is, and how far from the line.
pub fn along(pts: &[Xy], p: Xy) -> (f64, f64) {
    let (mut best, mut s) = ((0.0, f64::INFINITY), 0.0);
    for w in pts.windows(2) {
        let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 { (((p[0] - w[0][0]) * dx + (p[1] - w[0][1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let q = [w[0][0] + t * dx, w[0][1] + t * dy];
        let d = dist(p, q);
        if d < best.1 {
            best = (s + t * l2.sqrt(), d);
        }
        s += l2.sqrt();
    }
    best
}

/// The day of `r` whose route is nearest to `p`, with how far along that route the nearest point is.
pub fn nearest_leg(r: &Ring, p: Xy) -> Option<(usize, f64, f64)> {
    (0..r.cuts.len().saturating_sub(1))
        .map(|i| {
            let (a, d) = along(&leg_pts(r, i), p);
            (i, a, d)
        })
        .min_by(|x, y| x.2.total_cmp(&y.2))
}

fn path(dir: &Path, city_key: &str) -> PathBuf {
    dir.join(format!("{city_key}.json"))
}

/// The manual plans saved for a city (none if there are none, or the file is unreadable).
pub fn load(dir: &Path, city_key: &str) -> Vec<Manual> {
    std::fs::read_to_string(path(dir, city_key)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, city_key: &str, plans: &[Manual]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(plans).map_err(|e| e.to_string())?;
    std::fs::write(path(dir, city_key), text).map_err(|e| format!("{}: {e}", path(dir, city_key).display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(closed: bool, n: usize) -> Manual {
        let anchors: Vec<[f64; 2]> = (0..=n).map(|i| [i as f64, 0.0]).collect();
        let mut anchors = anchors;
        if closed {
            anchors[n] = anchors[0];
        }
        Manual { name: "t".into(), travel: Travel::Foot, closed, anchors, vias: vec![vec![]; n] }
    }

    #[test]
    fn a_rings_start_moves_with_its_end() {
        let mut m = plan(true, 4);
        m.move_anchor(0, [9.0, 9.0]);
        assert_eq!((m.anchors[0], m.anchors[4]), ([9.0, 9.0], [9.0, 9.0]));
        m.move_anchor(2, [7.0, 7.0]);
        assert_eq!(m.anchors[2], [7.0, 7.0]);
    }

    #[test]
    fn splitting_and_merging_days_keeps_routing_points() {
        let mut m = plan(false, 3);
        m.vias[1] = vec![[0.5, 1.0], [0.5, 2.0]];
        m.split_day(1, [1.5, 0.0], 5.0, &[3.0, 8.0]);
        assert_eq!(m.days(), 4);
        assert_eq!((m.vias[1].len(), m.vias[2].len()), (1, 1));
        m.remove_anchor(2);
        assert_eq!(m.days(), 3);
        assert_eq!(m.vias[1].len(), 2);
    }

    #[test]
    fn routing_points_keep_their_order_along_the_day() {
        let mut m = plan(false, 3);
        m.add_via(0, [0.0, 1.0], 10.0, &[]);
        m.add_via(0, [0.0, 2.0], 20.0, &[10.0]);
        m.add_via(0, [0.0, 1.5], 15.0, &[10.0, 20.0]);
        assert_eq!(m.vias[0], vec![[0.0, 1.0], [0.0, 1.5], [0.0, 2.0]]);
    }

    #[test]
    fn plans_are_saved_and_loaded() {
        let dir = std::env::temp_dir().join(format!("wedgewalks-manual-{}", std::process::id()));
        let plans = vec![plan(false, 3), plan(true, 4)];
        save(&dir, "somewhere", &plans).unwrap();
        assert_eq!(load(&dir, "somewhere"), plans);
        assert!(load(&dir, "nowhere").is_empty());
    }

    #[test]
    fn nearest_point_along_a_line() {
        let line = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]];
        let (a, d) = along(&line, [100.0, 40.0]);
        assert!((a - 140.0).abs() < 1e-9 && d < 1e-9);
    }
}
