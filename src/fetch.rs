//! Network access, all of it for fetching data up front: place search
//! (Nominatim), choosing an OSM extract (BBBike city extracts, else the
//! smallest Geofabrik region), and downloading with progress, resuming a
//! partial file. Blocking HTTP (`ureq`), always on background threads.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const USER_AGENT: &str = "wedgewalks/0.1 (ring-walk travel planner)";
const BBBIKE: &str = "https://download.bbbike.org/osm/bbbike";
const GEOFABRIK_INDEX: &str = "https://download.geofabrik.de/index-v1.json";
/// Time limits for small downloads and HEAD requests, and for files fetched while planning (elevation).
const TEXT_TIMEOUT: Duration = Duration::from_secs(60);
const HEAD_TIMEOUT: Duration = Duration::from_secs(20);
const FILE_TIMEOUT: Duration = Duration::from_secs(300);
/// Connecting must not take longer than this, and a big download may stall for this long before it is
/// given up on.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the interface is told about a download's progress.
const DOWNLOAD_POLL: Duration = Duration::from_millis(250);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().user_agent(USER_AGENT).timeout_connect(CONNECT_TIMEOUT).build()
}

/// A readable message for a failed request.
fn describe(url: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("{url}: HTTP {code}"),
        // The transport error names the address itself.
        ureq::Error::Transport(t) => t.to_string(),
    }
}

pub fn get_text(url: &str) -> Result<String, String> {
    let resp = agent().get(url).timeout(TEXT_TIMEOUT).call().map_err(|e| describe(url, e))?;
    resp.into_string().map_err(|e| format!("{url}: {e}"))
}

/// Downloads `url` to `dest` (via a temporary file). Ok(false) if the
/// server has no such file (404 or 403).
pub fn get_file(url: &str, dest: &Path) -> Result<bool, String> {
    let part = dest.with_extension("part");
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let resp = match agent().get(url).timeout(FILE_TIMEOUT).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404 | 403, _)) => return Ok(false),
        Err(e) => return Err(describe(url, e)),
    };
    let mut file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let copied = std::io::copy(&mut resp.into_reader(), &mut file);
    if let Err(e) = copied.and_then(|_| file.flush()) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{url}: {e}"));
    }
    std::fs::rename(&part, dest).map(|_| true).map_err(|e| format!("{}: {e}", dest.display()))
}

/// Size in bytes from a HEAD request, if the server says.
pub fn content_length(url: &str) -> Option<u64> {
    let resp = agent().head(url).timeout(HEAD_TIMEOUT).call().ok()?;
    resp.header("content-length")?.trim().parse().ok()
}

/// Like `content_length` but never blocks: the first call starts a
/// background lookup and returns None; later calls return the answer.
pub fn content_length_cached(url: &str) -> Option<u64> {
    static CACHE: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Option<u64>>>> =
        std::sync::LazyLock::new(Default::default);
    let mut cache = CACHE.lock().unwrap();
    if let Some(v) = cache.get(url) {
        return *v;
    }
    cache.insert(url.to_string(), None);
    let url = url.to_string();
    std::thread::spawn(move || {
        let len = content_length(&url);
        CACHE.lock().unwrap().insert(url, len);
    });
    None
}

#[derive(Clone, Debug)]
pub struct Place {
    /// What the user searched for.
    pub query: String,
    /// The name in English (as Nominatim has it; the local name where it has no other).
    pub name: String,
    /// The local name, when it differs from `name` (東京都 for Tokyo).
    pub native: Option<String>,
    pub display: String,
    pub country: String,
    pub lat: f64,
    pub lon: f64,
}

/// Searches for a place by name.
pub fn search_places(query: &str) -> Result<Vec<Place>, String> {
    let q: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    // In English, with the local names too: so a search for Tokyo shows Tokyo (東京都), and the city is
    // named and matched to map extracts by its English name.
    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=jsonv2&addressdetails=1&namedetails=1&accept-language=en&limit=8&q={q}"
    );
    places_from(&get_text(&url)?, query)
}

/// Places from a Nominatim answer.
fn places_from(json: &str, query: &str) -> Result<Vec<Place>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let places = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let lat = p["lat"].as_str()?.parse().ok()?;
            let lon = p["lon"].as_str()?.parse().ok()?;
            let display = p["display_name"].as_str()?.to_string();
            let name = p["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| display.split(',').next().unwrap_or("").trim().to_string());
            let country = p["address"]["country_code"].as_str().unwrap_or("").to_uppercase();
            let name = clean_name(&name);
            // The local name, if it is a different name from the English one.
            let native = p["namedetails"]["name"]
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&name) && squash(n) != squash(&name))
                .map(str::to_string);
            Some(Place { query: query.trim().to_string(), name, native, display, country, lat, lon })
        })
        .collect();
    Ok(places)
}

/// Searches for a place in a country (an ISO 3166 code): for a list of cities where the name alone is
/// ambiguous. The best match, if any.
pub fn search_in_country(name: &str, country: &str) -> Result<Option<Place>, String> {
    let q: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=jsonv2&addressdetails=1&namedetails=1&accept-language=en&limit=5&countrycodes={}&q={q}",
        country.to_lowercase()
    );
    Ok(places_from(&get_text(&url)?, name)?.into_iter().next())
}

/// A listed city looked up and matched to map data: its place, and where its map data comes from.
pub fn resolve(name: &str, country: &str, data: &Path, margin_km: f64) -> Result<(Place, Source), String> {
    let place =
        search_in_country(crate::gawc::query_for(name), country)?.ok_or_else(|| format!("{name}: not found"))?;
    let source = choose_source(&place, data, margin_km)?;
    Ok((place, source))
}

/// Where to get OSM data for a city.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A BBBike city extract, by BBBike's city name.
    Bbbike(String),
    /// A Geofabrik region: id (e.g. "ile-de-france") and download URL.
    Geofabrik { id: String, url: String },
}

impl Source {
    /// The form stored in cities.csv.
    pub fn to_field(&self) -> String {
        match self {
            Source::Bbbike(n) => format!("bbbike:{n}"),
            Source::Geofabrik { id, url } => format!("geofabrik:{id} {url}"),
        }
    }

    pub fn from_field(s: &str) -> Option<Source> {
        if let Some(n) = s.strip_prefix("bbbike:") {
            return Some(Source::Bbbike(n.trim().to_string()));
        }
        let rest = s.strip_prefix("geofabrik:")?;
        let (id, url) = rest.trim().split_once(' ')?;
        Some(Source::Geofabrik { id: id.to_string(), url: url.trim().to_string() })
    }

    pub fn url(&self) -> String {
        match self {
            Source::Bbbike(n) => format!("{BBBIKE}/{n}/{n}.osm.pbf"),
            Source::Geofabrik { url, .. } => url.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Source::Bbbike(n) => format!("BBBike city extract \"{n}\""),
            Source::Geofabrik { id, .. } => format!("Geofabrik region \"{id}\""),
        }
    }

    /// Local file for this source. City extracts are per city; regions
    /// are shared by every city inside them.
    pub fn path(&self, data: &Path, city_key: &str) -> PathBuf {
        match self {
            Source::Bbbike(_) => data.join(format!("{city_key}.osm.pbf")),
            Source::Geofabrik { id, .. } => data.join("regions").join(format!("{id}.osm.pbf")),
        }
    }
}

/// BBBike's list of city extracts (fetched once per run).
pub fn bbbike_cities() -> Result<Vec<String>, String> {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    if let Some(l) = LIST.get() {
        return Ok(l.clone());
    }
    let l = fetch_bbbike_cities()?;
    Ok(LIST.get_or_init(|| l).clone())
}

fn fetch_bbbike_cities() -> Result<Vec<String>, String> {
    let html = get_text(&format!("{BBBIKE}/"))?;
    let mut names: Vec<String> = html
        .split("href=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .filter_map(|h| h.strip_suffix('/'))
        .filter(|h| h.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !h.contains('/'))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Bounding box (min lon, min lat, max lon, max lat) of a BBBike extract.
pub fn bbbike_bbox(name: &str) -> Result<[f64; 4], String> {
    let poly = get_text(&format!("{BBBIKE}/{name}/{name}.poly"))?;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for line in poly.lines() {
        let nums: Vec<f64> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if let [lon, lat] = nums[..] {
            b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
        }
    }
    if b[0].is_finite() { Ok(b) } else { Err(format!("{name}: no coordinates in .poly")) }
}

/// "City of Edinburgh" -> "Edinburgh", and similar administrative prefixes.
fn clean_name(name: &str) -> String {
    let prefixes = ["City of ", "Ville de ", "Stadt ", "Città di ", "Ciudad de ", "Municipality of ", "Greater "];
    prefixes.iter().find_map(|p| name.strip_prefix(p)).unwrap_or(name).trim().to_string()
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A BBBike extract named like `place` whose box contains it with at
/// least `margin_km` to spare on every side.
pub fn bbbike_for(place: &Place, cities: &[String], margin_km: f64) -> Option<String> {
    let first = place.display.split(',').next().unwrap_or("");
    let wants: Vec<String> = [place.name.as_str(), &clean_name(first), &place.query]
        .iter()
        .map(|s| squash(s))
        .filter(|s| !s.is_empty())
        .collect();
    let name = cities.iter().find(|c| wants.contains(&squash(c)))?;
    let b = bbbike_bbox(name).ok()?;
    let deg_per_km = 1000.0 / crate::graph::M_PER_DEG;
    let (dlat, dlon) = (margin_km * deg_per_km, margin_km * deg_per_km / place.lat.to_radians().cos());
    let fits =
        place.lon - dlon >= b[0] && place.lat - dlat >= b[1] && place.lon + dlon <= b[2] && place.lat + dlat <= b[3];
    fits.then(|| name.clone())
}

fn ring_contains(ring: &[serde_json::Value], lon: f64, lat: f64) -> bool {
    let pts: Vec<(f64, f64)> = ring.iter().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect();
    let mut inside = false;
    for w in pts.windows(2) {
        let ((x1, y1), (x2, y2)) = (w[0], w[1]);
        if (y1 > lat) != (y2 > lat) && lon < x1 + (lat - y1) / (y2 - y1) * (x2 - x1) {
            inside = !inside;
        }
    }
    inside
}

fn ring_area(ring: &[serde_json::Value]) -> f64 {
    let pts: Vec<[f64; 2]> = ring.iter().filter_map(|p| Some([p[0].as_f64()?, p[1].as_f64()?])).collect();
    crate::water::polygon_area(&pts)
}

/// The smallest Geofabrik region containing the point. The index is
/// cached in `cache` after the first download.
pub fn geofabrik_for(lat: f64, lon: f64, cache: &Path) -> Result<Source, String> {
    let text = match std::fs::read_to_string(cache) {
        Ok(t) => t,
        Err(_) => {
            let t = get_text(GEOFABRIK_INDEX)?;
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(cache, &t);
            t
        }
    };
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut best: Option<(f64, Source)> = None;
    for f in v["features"].as_array().into_iter().flatten() {
        let props = &f["properties"];
        let (Some(id), Some(url)) = (props["id"].as_str(), props["urls"]["pbf"].as_str()) else { continue };
        let geom = &f["geometry"];
        let polys: Vec<&serde_json::Value> = match geom["type"].as_str() {
            Some("Polygon") => vec![&geom["coordinates"]],
            Some("MultiPolygon") => geom["coordinates"].as_array().into_iter().flatten().collect(),
            _ => continue,
        };
        for poly in polys {
            let Some(outer) = poly.get(0).and_then(|r| r.as_array()) else { continue };
            if ring_contains(outer, lon, lat) {
                let area = ring_area(outer);
                if best.as_ref().is_none_or(|b| area < b.0) {
                    best = Some((area, Source::Geofabrik { id: id.to_string(), url: url.to_string() }));
                }
            }
        }
    }
    best.map(|b| b.1).ok_or_else(|| format!("no Geofabrik region contains {lat:.4},{lon:.4}"))
}

/// How much of a BBBike extract round the city is enough (km).
const MIN_EXTRACT_MARGIN_KM: f64 = 8.0;

/// Downloads bigger than this (bytes) are not started by themselves when adding many cities.
pub const BIG_DOWNLOAD: u64 = 1_000_000_000;

/// Picks a source for a place: a BBBike city extract that comfortably
/// covers it, else the smallest Geofabrik region.
pub fn choose_source(place: &Place, data: &Path, margin_km: f64) -> Result<Source, String> {
    // A city extract is a box a few tens of km across: it need only leave room for the ring, not for
    // all of the map the settings would load.
    let margin_km = margin_km.min(MIN_EXTRACT_MARGIN_KM);
    if let Ok(cities) = bbbike_cities()
        && let Some(name) = bbbike_for(place, &cities, margin_km)
    {
        return Ok(Source::Bbbike(name));
    }
    geofabrik_for(place.lat, place.lon, &data.join("geofabrik-index.json"))
}

/// Progress of a background download, shared with the UI.
#[derive(Default)]
pub struct Download {
    pub bytes: AtomicU64,
    pub total: AtomicU64,
    pub done: AtomicBool,
    pub error: Mutex<Option<String>>,
}

impl Download {
    pub fn fraction(&self) -> Option<f32> {
        let t = self.total.load(Relaxed);
        (t > 0).then(|| self.bytes.load(Relaxed) as f32 / t as f32)
    }
}

/// Downloads `url` to `dest` on a background thread, via `dest.part` so a
/// partial file never looks complete. A partial file from an earlier try is carried on from where it
/// stopped, if the server allows. `on_change` is called as it goes.
pub fn download(url: String, dest: PathBuf, on_change: impl Fn() + Send + 'static) -> Arc<Download> {
    let state = Arc::new(Download::default());
    let st = state.clone();
    std::thread::spawn(move || {
        if let Err(e) = download_to(&url, &dest, &st, &on_change) {
            *st.error.lock().unwrap() = Some(e);
        }
        st.done.store(true, Relaxed);
        on_change();
    });
    state
}

fn download_to(url: &str, dest: &Path, st: &Download, on_change: &dyn Fn()) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = dest.with_extension("part");
    let have = std::fs::metadata(&part).map_or(0, |m| m.len());
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(STALL_TIMEOUT)
        .build();
    let mut req = agent.get(url);
    if have > 0 {
        req = req.set("Range", &format!("bytes={have}-"));
    }
    let resp = match req.call() {
        Ok(r) => r,
        // The partial file is all of it already, or no longer fits: start again.
        Err(ureq::Error::Status(416, _)) => {
            let _ = std::fs::remove_file(&part);
            return download_to(url, dest, st, on_change);
        }
        Err(e) => return Err(describe(url, e)),
    };
    // 206: carry on from `have`; 200: the whole file, from the start.
    let resumed = resp.status() == 206;
    let length: u64 = resp.header("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let start = if resumed { have } else { 0 };
    st.total.store(if length > 0 { start + length } else { 0 }, Relaxed);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)
        .map_err(|e| format!("{}: {e}", part.display()))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = start;
    let mut last = std::time::Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e} (the download can be resumed)"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", part.display()))?;
        done += n as u64;
        st.bytes.store(done, Relaxed);
        if last.elapsed() >= DOWNLOAD_POLL {
            last = std::time::Instant::now();
            on_change();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    let total = st.total.load(Relaxed);
    if total > 0 && done < total {
        return Err(format!("{url}: stopped at {done} of {total} bytes (the download can be resumed)"));
    }
    std::fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_admin_prefixes() {
        assert_eq!(clean_name("City of Edinburgh"), "Edinburgh");
        assert_eq!(clean_name("Paris"), "Paris");
    }

    /// Nominatim, asked for English, gives Tokyo's English name; the local name is kept to show beside it.
    #[test]
    fn places_are_in_english_with_the_local_name_beside() {
        let json = r#"[{"lat":"35.6768601","lon":"139.7638947","name":"Tokyo","display_name":"Tokyo, Japan",
            "address":{"country_code":"jp"},"namedetails":{"name":"東京都","name:en":"Tokyo"}},
            {"lat":"51.5","lon":"-0.1","name":"London","display_name":"London, England, United Kingdom",
            "address":{"country_code":"gb"},"namedetails":{"name":"London","name:en":"London"}}]"#;
        let places = places_from(json, "tokyo").unwrap();
        assert_eq!((places[0].name.as_str(), places[0].native.as_deref()), ("Tokyo", Some("東京都")));
        assert_eq!(places[0].country, "JP");
        assert_eq!(places[1].native, None, "no local name to show when it is the same");
    }

    #[test]
    fn source_field_round_trip() {
        for s in [
            Source::Bbbike("Paris".into()),
            Source::Bbbike("SanFrancisco".into()),
            Source::Geofabrik { id: "ile-de-france".into(), url: "https://x/y.osm.pbf".into() },
        ] {
            assert_eq!(Source::from_field(&s.to_field()), Some(s));
        }
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// A tiny HTTP server for one body, with HEAD, GET and Range, on a free port, for `requests` requests.
    fn serve(body: Vec<u8>, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut range = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                let head = first.starts_with("HEAD");
                if first.contains("/missing") {
                    stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .unwrap();
                    continue;
                }
                let (status, part) = match range {
                    Some(from) => ("206 Partial Content", &body[from..]),
                    None => ("200 OK", &body[..]),
                };
                let header =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", part.len());
                stream.write_all(header.as_bytes()).unwrap();
                if !head {
                    stream.write_all(part).unwrap();
                }
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wedgewalks-http-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn text_sizes_files_and_missing_files() {
        let base = serve(b"hello world".to_vec(), 4);
        assert_eq!(get_text(&format!("{base}/a")).unwrap(), "hello world");
        assert_eq!(content_length(&format!("{base}/a")), Some(11));
        let dir = temp("file");
        assert_eq!(get_file(&format!("{base}/a"), &dir.join("x.bin")), Ok(true));
        assert_eq!(std::fs::read(dir.join("x.bin")).unwrap(), b"hello world");
        assert_eq!(get_file(&format!("{base}/missing"), &dir.join("y.bin")), Ok(false));
        assert!(!dir.join("y.bin").exists());
    }

    #[test]
    fn a_download_reports_progress_and_resumes_a_partial_file() {
        let body: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let base = serve(body.clone(), 2);
        let dir = temp("resume");
        let dest = dir.join("big.osm.pbf");
        // Half of it is already there from an earlier try.
        std::fs::write(dest.with_extension("part"), &body[..1_200_000]).unwrap();
        let d = download(format!("{base}/big"), dest.clone(), || {});
        let start = std::time::Instant::now();
        while !d.done.load(Relaxed) && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(d.done.load(Relaxed), "the download did not finish");
        assert_eq!(*d.error.lock().unwrap(), None);
        assert_eq!(d.total.load(Relaxed), body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), body, "resumed file is whole and unbroken");
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn a_dead_server_is_an_error_not_a_hang() {
        // Nothing listens on a just-freed port.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(get_text(&format!("http://127.0.0.1:{port}/")).is_err());
        assert_eq!(content_length(&format!("http://127.0.0.1:{port}/")), None);
    }
}

#[cfg(test)]
mod network_tests {
    /// Needs the internet: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn tokyo_is_found_in_english_and_matched_to_a_city_extract() {
        let places = super::search_places("tokyo").unwrap();
        let tokyo = &places[0];
        assert_eq!(tokyo.name, "Tokyo");
        assert!(tokyo.native.is_some(), "the local name is kept: {:?}", tokyo.native);
        let (place, source) = super::resolve("Tokyo", "JP", std::path::Path::new("/tmp"), 25.0).unwrap();
        eprintln!("{} -> {} {:?}", place.display, source.describe(), super::content_length(&source.url()));
        assert!(matches!(source, super::Source::Bbbike(_)), "{source:?}");
    }
}
