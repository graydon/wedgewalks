//! Background planning. One worker thread owns the cached pipeline stages
//! (load → build → search → select) and recomputes only what a request
//! changes. A new request cancels the one in progress.

use crate::cities::City;
use crate::config::Config;
use crate::pipeline::{self, Built, Loaded, Plan};
use crate::rings::Ring;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// Data handed to the worker for a city, bypassing the file.
type Preload = Arc<Mutex<Option<(String, Arc<Loaded>)>>>;

pub struct Request {
    pub city: City,
    /// Base location (may differ from the city's CSV entry).
    pub lat: f64,
    pub lon: f64,
    pub cfg: Config,
    /// Plans edited by hand, for this city.
    pub manual: Vec<crate::manual::Manual>,
}

pub enum Update {
    Status(String),
    Built(Arc<Built>),
    Plan(Arc<Plan>),
    Error(String),
}

/// What the worker is doing, readable from the UI at any time.
#[derive(Default)]
pub struct Progress {
    pub busy: AtomicBool,
    /// In the ring search stage (when `fraction` means something).
    searching: AtomicBool,
    /// Ring search progress, 0..1 (f32 bits).
    fraction: AtomicU32,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        f32::from_bits(self.fraction.load(Relaxed))
    }
    pub fn searching(&self) -> bool {
        self.searching.load(Relaxed)
    }
    fn set(&self, f: f32) {
        self.fraction.store(f.to_bits(), Relaxed);
    }
}

pub struct Worker {
    tx: Sender<Request>,
    pub rx: Receiver<Update>,
    cancel: Arc<Mutex<Arc<AtomicBool>>>,
    pub progress: Arc<Progress>,
    preload: Preload,
}

/// Stage caches, owned by the worker thread.
#[derive(Default)]
struct Cache {
    loaded: Option<(String, Arc<Loaded>)>,
    built: Option<(String, Arc<Built>)>,
    raw: Option<(String, Arc<Vec<Ring>>)>,
    manual: Option<(String, Arc<Vec<Ring>>, Vec<String>)>,
}

impl Worker {
    /// `wake` is called whenever there is news for the UI.
    pub fn new(data: PathBuf, wake: impl Fn() + Send + 'static) -> Worker {
        let (tx, req_rx) = channel::<Request>();
        let (up_tx, rx) = channel();
        let cancel = Arc::new(Mutex::new(Arc::new(AtomicBool::new(false))));
        let progress = Arc::new(Progress::default());
        let preload: Preload = Arc::default();
        let (c, p, pre) = (cancel.clone(), progress.clone(), preload.clone());
        std::thread::Builder::new()
            .name("planner".into())
            .spawn(move || {
                let mut cache = Cache::default();
                while let Ok(mut req) = req_rx.recv() {
                    // Only the latest request matters.
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let flag = Arc::new(AtomicBool::new(false));
                    *c.lock().unwrap() = flag.clone();
                    p.busy.store(true, Relaxed);
                    if let Some(l) = pre.lock().unwrap().take() {
                        cache.loaded = Some(l);
                    }
                    let send = |u: Update| {
                        let _ = up_tx.send(u);
                        wake();
                    };
                    if let Err(e) = run(&req, &data, &mut cache, &flag, &p, &send) {
                        send(Update::Error(e));
                    }
                    p.busy.store(false, Relaxed);
                    wake();
                }
            })
            .expect("spawn planner thread");
        Worker { tx, rx, cancel, progress, preload }
    }

    /// Starts planning, cancelling whatever is in progress.
    pub fn request(&self, req: Request) {
        self.cancel.lock().unwrap().store(true, Relaxed);
        self.progress.busy.store(true, Relaxed);
        let _ = self.tx.send(req);
    }

    /// Supplies already-loaded data for a city (tests, synthetic cities).
    pub fn preload(&self, city_key: &str, loaded: Loaded) {
        *self.preload.lock().unwrap() = Some((city_key.to_string(), Arc::new(loaded)));
    }

    pub fn busy(&self) -> bool {
        self.progress.busy.load(Relaxed)
    }
}

fn run(
    req: &Request,
    data: &std::path::Path,
    cache: &mut Cache,
    cancel: &AtomicBool,
    progress: &Progress,
    send: &dyn Fn(Update),
) -> Result<(), String> {
    let cfg = &req.cfg;
    let city = &req.city;
    let key = city.key();
    let radius = cfg.extract_radius_km * 1000.0;

    let fresh = cache.loaded.as_ref().is_some_and(|(k, l)| *k == key && l.covers(req.lat, req.lon, radius));
    if !fresh {
        cache.built = None;
        cache.raw = None;
        let status = |s: &str| send(Update::Status(format!("{}: {s}…", city.name)));
        let l = pipeline::load_city(city, data, req.lat, req.lon, cfg, &status)?;
        cache.loaded = Some((key.clone(), Arc::new(l)));
    }
    let loaded = cache.loaded.as_ref().unwrap().1.clone();
    if cancel.load(Relaxed) {
        return Ok(());
    }

    let build_key = format!("{key} {} {} {}", req.lat, req.lon, pipeline::build_key(cfg));
    if cache.built.as_ref().is_none_or(|(k, _)| *k != build_key) {
        send(Update::Status(format!("{}: building walk graph…", city.name)));
        cache.raw = None;
        let b = Arc::new(pipeline::build(&loaded, req.lat, req.lon, cfg));
        cache.built = Some((build_key.clone(), b.clone()));
        send(Update::Built(b));
    }
    let built = cache.built.as_ref().unwrap().1.clone();
    if cancel.load(Relaxed) {
        return Ok(());
    }

    let search_key = format!("{build_key} {}", pipeline::search_key(cfg));
    if cache.raw.as_ref().is_none_or(|(k, _)| *k != search_key) {
        send(Update::Status(format!("{}: searching for rings…", city.name)));
        progress.set(0.0);
        progress.searching.store(true, Relaxed);
        let raw = pipeline::search(&built, cfg, cancel, &|f| progress.set(f));
        progress.searching.store(false, Relaxed);
        let Some(raw) = raw else {
            return Ok(()); // cancelled
        };
        cache.raw = Some((search_key.clone(), Arc::new(raw)));
    }
    let raw = cache.raw.as_ref().unwrap().1.clone();
    // Plans edited by hand are routed afresh when they, or what they are routed on, change.
    let manual_key = format!("{search_key} {}", serde_json::to_string(&req.manual).unwrap_or_default());
    if cache.manual.as_ref().is_none_or(|(k, ..)| *k != manual_key) {
        let (rings, errors) = crate::plan::manual_rings(&built, cfg, &req.manual);
        cache.manual = Some((manual_key, Arc::new(rings), errors));
    }
    let (_, manual, errors) = cache.manual.as_ref().unwrap();
    let mut all = (*raw).clone();
    all.extend(manual.iter().cloned());
    let plan = pipeline::select(&built, &all, cfg);
    let mut status = format!("{}: {} rings, {} within limits", city.name, plan.candidates, plan.valid);
    for e in errors {
        status.push_str(&format!("; your plan {e}"));
    }
    send(Update::Status(status));
    send(Update::Plan(Arc::new(plan)));
    Ok(())
}
