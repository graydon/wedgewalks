use crate::fetch::Source;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One row of the hand-maintained candidate city list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct City {
    pub name: String,
    pub country: String,
    pub lat: f64,
    pub lon: f64,
    pub population: Option<u64>,
    pub climate_note: String,
    pub summer_high_c: Option<f64>,
    pub passport_ok: Option<bool>,
    pub safety_ok: Option<bool>,
    pub notes: String,
    /// Where the OSM data comes from (see `fetch::Source`); empty means a
    /// file placed by hand at data/<city>.osm.pbf.
    #[serde(default)]
    pub source: String,
}

impl City {
    pub fn key(&self) -> String {
        crate::pipeline::city_key(&self.name)
    }

    pub fn source(&self) -> Option<Source> {
        Source::from_field(&self.source)
    }

    pub fn pbf_path(&self, data: &Path) -> PathBuf {
        match self.source() {
            Some(s) => s.path(data, &self.key()),
            None => crate::pipeline::pbf_path(data, &self.key()),
        }
    }

    pub fn cache_path(&self, data: &Path) -> PathBuf {
        data.join("cache").join(format!("{}.bin", self.key()))
    }
}

const HEADER: &str = "\
# Candidate cities. lat/lon is the base (city center). Climate, passport and
# safety columns are maintained by hand; check them before relying on them.
# source: bbbike:<Name> or geofabrik:<region-id> <url> (blank = data/<city>.osm.pbf).
";

pub fn load(path: &Path) -> Result<Vec<City>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .comment(Some(b'#'))
        .from_path(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    rdr.deserialize().map(|r| r.map_err(|e| format!("{}: {e}", path.display()))).collect()
}

pub fn save(path: &Path, cities: &[City]) -> Result<(), String> {
    let mut w = csv::Writer::from_writer(vec![]);
    for c in cities {
        w.serialize(c).map_err(|e| e.to_string())?;
    }
    let body = String::from_utf8(w.into_inner().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::write(path, format!("{HEADER}{body}")).map_err(|e| format!("{}: {e}", path.display()))
}
