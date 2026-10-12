//! Ground elevation from SRTM tiles (the public "skadi" set on AWS: one
//! degree square, 1 arc-second, about 30 m). Tiles are downloaded once into
//! `data/elevation/`, cropped to the area around the base, and smoothed:
//! SRTM measures the tops of buildings and trees, and those bumps would
//! otherwise count as climbs.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

const TILES_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/skadi";
/// Box blur radius in cells, applied twice (close to a Gaussian over about
/// 150 m): wider than buildings, narrower than hills.
const BLUR_CELLS: usize = 2;
/// SRTM's marker for no data.
const VOID: i16 = -32768;

/// Elevations (m) on a regular lat/lon grid, rows from south to north.
#[derive(Clone, Serialize, Deserialize)]
pub struct Dem {
    /// South-west corner (degrees) and cell size (degrees).
    lat0: f64,
    lon0: f64,
    step: f64,
    width: usize,
    height: usize,
    z: Vec<f32>,
}

impl Dem {
    /// Elevation at a point, interpolated; None outside the grid.
    pub fn at(&self, lat: f64, lon: f64) -> Option<f32> {
        let x = (lon - self.lon0) / self.step;
        let y = (lat - self.lat0) / self.step;
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (c, r) = (x.floor() as usize, y.floor() as usize);
        if c + 1 >= self.width || r + 1 >= self.height {
            return None;
        }
        let (fx, fy) = ((x - c as f64) as f32, (y - r as f64) as f32);
        let z = |c: usize, r: usize| self.z[r * self.width + c];
        let south = z(c, r) * (1.0 - fx) + z(c + 1, r) * fx;
        let north = z(c, r + 1) * (1.0 - fx) + z(c + 1, r + 1) * fx;
        Some(south * (1.0 - fy) + north * fy)
    }

    /// A grid from a function (tests, synthetic cities).
    pub fn from_fn(lat0: f64, lon0: f64, step: f64, size: usize, f: impl Fn(f64, f64) -> f32) -> Dem {
        let z = (0..size * size).map(|i| f(lat0 + (i / size) as f64 * step, lon0 + (i % size) as f64 * step)).collect();
        Dem { lat0, lon0, step, width: size, height: size, z }
    }

    /// The elevation around (lat, lon), within `radius_m`, downloading
    /// tiles as needed. Tiles the server doesn't have are open sea (0 m).
    pub fn load(data: &Path, lat: f64, lon: f64, radius_m: f64, progress: &dyn Fn(&str)) -> Result<Dem, String> {
        let dlat = radius_m / crate::graph::M_PER_DEG;
        let dlon = dlat / lat.to_radians().cos();
        let (south, north, west, east) = (lat - dlat, lat + dlat, lon - dlon, lon + dlon);
        let mut tiles = vec![];
        for tlat in south.floor() as i32..=north.floor() as i32 {
            for tlon in west.floor() as i32..=east.floor() as i32 {
                progress("elevation");
                tiles.push(((tlat, tlon), Tile::load(data, tlat, tlon)?));
            }
        }
        // All tiles share one resolution (1 arc-second for skadi tiles; a
        // 3 arc-second tile would be resampled by `Tile::at`).
        let step = 1.0 / 3600.0;
        let (width, height) =
            (((east - west) / step).ceil() as usize + 1, ((north - south) / step).ceil() as usize + 1);
        let mut z = vec![0.0f32; width * height];
        for (r, row) in z.chunks_mut(width).enumerate() {
            let la = south + r as f64 * step;
            for (c, cell) in row.iter_mut().enumerate() {
                let lo = west + c as f64 * step;
                let key = (la.floor() as i32, lo.floor() as i32);
                *cell = tiles.iter().find(|t| t.0 == key).and_then(|t| t.1.as_ref()).map_or(0.0, |t| t.at(la, lo));
            }
        }
        let mut dem = Dem { lat0: south, lon0: west, step, width, height, z };
        dem.blur();
        dem.blur();
        Ok(dem)
    }

    /// Box blur, rows then columns.
    fn blur(&mut self) {
        let (w, h, k) = (self.width, self.height, BLUR_CELLS);
        let pass = |get: &dyn Fn(usize) -> f32, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    let (a, b) = (i.saturating_sub(k), (i + k).min(n - 1));
                    (a..=b).map(get).sum::<f32>() / (b - a + 1) as f32
                })
                .collect()
        };
        for r in 0..h {
            let row = pass(&|c| self.z[r * w + c], w);
            self.z[r * w..(r + 1) * w].copy_from_slice(&row);
        }
        for c in 0..w {
            let col = pass(&|r| self.z[r * w + c], h);
            for (r, v) in col.into_iter().enumerate() {
                self.z[r * w + c] = v;
            }
        }
    }
}

/// One SRTM tile: `size` x `size` samples, rows from north to south.
struct Tile {
    lat: i32,
    lon: i32,
    size: usize,
    z: Vec<i16>,
}

impl Tile {
    fn name(lat: i32, lon: i32) -> String {
        let ns = if lat >= 0 { 'N' } else { 'S' };
        let ew = if lon >= 0 { 'E' } else { 'W' };
        format!("{ns}{:02}{ew}{:03}", lat.abs(), lon.abs())
    }

    /// The tile, downloaded if not already here; None where there is none
    /// (open sea).
    fn load(data: &Path, lat: i32, lon: i32) -> Result<Option<Tile>, String> {
        let name = Tile::name(lat, lon);
        let dir = data.join("elevation");
        let path = dir.join(format!("{name}.hgt.gz"));
        let missing = dir.join(format!("{name}.none"));
        if missing.exists() {
            return Ok(None);
        }
        if !path.exists() {
            let url = format!("{TILES_URL}/{}/{name}.hgt.gz", &name[..3]);
            if !crate::fetch::get_file(&url, &path)? {
                let _ = std::fs::write(&missing, "");
                return Ok(None);
            }
        }
        let mut bytes = vec![];
        flate2::read::GzDecoder::new(std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let size = ((bytes.len() / 2) as f64).sqrt() as usize;
        if size * size * 2 != bytes.len() || size < 2 {
            return Err(format!("{}: not an SRTM tile", path.display()));
        }
        let z = bytes.as_chunks::<2>().0.iter().map(|&b| i16::from_be_bytes(b)).collect();
        Ok(Some(Tile { lat, lon, size, z }))
    }

    /// The sample nearest a point in the tile (voids as 0 m).
    fn at(&self, lat: f64, lon: f64) -> f32 {
        let n = self.size - 1;
        let c = (((lon - self.lon as f64) * n as f64).round() as usize).min(n);
        let r = (((self.lat as f64 + 1.0 - lat) * n as f64).round() as usize).min(n);
        match self.z[r * self.size + c] {
            VOID => 0.0,
            v => v as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dem, Tile};

    #[test]
    fn tile_names() {
        assert_eq!(Tile::name(49, -124), "N49W124");
        assert_eq!(Tile::name(-34, 18), "S34E018");
    }

    #[test]
    fn interpolates_and_blurs() {
        // A plane rising 1 m per cell eastwards: blurring keeps it (away
        // from the edges) and interpolation is exact.
        let mut d = Dem::from_fn(0.0, 0.0, 0.001, 20, |_, lon| (lon / 0.001) as f32);
        d.blur();
        let z = d.at(0.0105, 0.0105).unwrap();
        assert!((z - 10.5).abs() < 1e-3, "{z}");
        assert!(d.at(-0.1, 0.0).is_none());
    }
}
