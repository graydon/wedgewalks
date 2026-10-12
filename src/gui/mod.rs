//! Interactive planner: pick or add a city, adjust parameters, see rings
//! and arcs on the map, click arcs for transit details.

mod draw;
mod gl;
pub mod map;
mod panels;
pub mod style;
mod windows;
pub mod worker;

use crate::cities::{self, City};
use crate::config::Config;
use crate::fetch::{self, Download};
use crate::graph::{Proj, Xy};
use crate::pipeline::{self, Built, Plan};
use egui::Color32;
use map::MapView;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use worker::{Request, Update, Worker};

/// How many map downloads run at once (the rest wait their turn).
const MAX_DOWNLOADS: usize = 3;

pub struct Options {
    pub data: PathBuf,
    pub cities: PathBuf,
    pub config: PathBuf,
    pub out: PathBuf,
}

pub fn run(opts: Options) -> eframe::Result {
    let native = |multisampling| eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 950.0]).with_title("wedgewalks"),
        multisampling,
        ..Default::default()
    };
    // Multisampling smooths the map's lines but isn't available everywhere.
    let opts = Arc::new(Mutex::new(Some(opts)));
    let app = |o: Arc<Mutex<Option<Options>>>| -> eframe::AppCreator<'static> {
        Box::new(move |cc| Ok(Box::new(App::new(cc, o.lock().unwrap().take().expect("options")))))
    };
    match eframe::run_native("wedgewalks", native(4), app(opts.clone())) {
        Err(e) if opts.lock().unwrap().is_some() => {
            eprintln!("note: retrying without multisampling ({e})");
            eframe::run_native("wedgewalks", native(0), app(opts))
        }
        r => r,
    }
}

pub struct App {
    opts: Options,
    pub cities: Vec<City>,
    pub selected: Option<usize>,
    /// Base for the selected city.
    pub base: (f64, f64),
    pub cfg: Config,
    /// Plans edited by hand for the selected city (saved as it changes).
    pub manuals: Vec<crate::manual::Manual>,
    /// After the next plan, select this manual plan (an index into `manuals`).
    select_manual: Option<usize>,
    /// Last request sent to the worker, to spot changes.
    requested: Option<(String, f64, f64, Config, Vec<crate::manual::Manual>)>,
    pub worker: Worker,
    pub built: Option<Arc<Built>>,
    pub plan: Option<Arc<Plan>>,
    pub ring: usize,
    pub arc: Option<usize>,
    pub map: MapView,
    pub status: String,
    pub error: Option<String>,
    /// Downloads in progress, by the file they are fetching (cities in one region share it).
    downloads: HashMap<PathBuf, Arc<Download>>,
    /// Downloads waiting for one of the few places to run at once: (file, url).
    download_queue: std::collections::VecDeque<(PathBuf, String)>,
    /// Cities being looked up in the background, to add and download.
    adder: windows::Adder,
    add: windows::AddCity,
    hover: Option<(usize, usize)>,
    /// Which outings to draw as wedges with their transit trips.
    pub wedges: map::Wedges,
    /// Stronger map colours.
    pub high_contrast: bool,
    /// Water, parks and busy roads under the walk graph.
    pub basemap: bool,
    /// Cities whose data was supplied in memory (tests, demos).
    preloaded: std::collections::HashSet<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> App {
        let (cities, mut error) = match cities::load(&opts.cities) {
            Ok(c) => (c, None),
            Err(e) => (vec![], Some(e)),
        };
        let cfg = Config::load(&opts.config).unwrap_or_else(|e| {
            error = Some(e);
            Config::default()
        });
        style::install(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::new(opts.data.clone(), move || ctx.request_repaint());
        App {
            opts,
            cities,
            selected: None,
            base: (0.0, 0.0),
            cfg,
            manuals: vec![],
            select_manual: None,
            requested: None,
            worker,
            built: None,
            plan: None,
            ring: 0,
            arc: None,
            map: MapView::new(cc.gl.is_some()),
            status: String::new(),
            error,
            downloads: HashMap::new(),
            download_queue: Default::default(),
            adder: Default::default(),
            add: windows::AddCity::default(),
            hover: None,
            wedges: map::Wedges::default(),
            high_contrast: false,
            basemap: true,
            preloaded: Default::default(),
        }
    }

    /// Supplies map data for a city directly, instead of from a file.
    pub fn preload(&mut self, city_key: &str, loaded: pipeline::Loaded) {
        self.preloaded.insert(city_key.to_string());
        self.worker.preload(city_key, loaded);
    }

    pub fn select_city(&mut self, i: usize) {
        if self.selected == Some(i) {
            return;
        }
        self.selected = Some(i);
        let c = &self.cities[i];
        self.base = (c.lat, c.lon);
        self.built = None;
        self.plan = None;
        self.ring = 0;
        self.arc = None;
        self.error = None;
        self.manuals = crate::manual::load(&self.edits_dir(), &self.cities[i].key());
        self.select_manual = None;
        self.fit_ring();
    }

    /// Where plans edited by hand are kept: next to the cities file.
    fn edits_dir(&self) -> PathBuf {
        self.opts.cities.parent().unwrap_or(std::path::Path::new(".")).join("edits")
    }

    pub fn city(&self) -> Option<&City> {
        self.selected.map(|i| &self.cities[i])
    }

    /// Fits the view (now, and for "Fit view") to the largest ring allowed.
    fn fit_ring(&mut self) {
        let r = self.cfg.typical_radius_m() * FIT_MARGIN;
        self.map.home = r;
        self.map.fit = Some(r);
    }

    fn has_data(&self, c: &City) -> bool {
        self.preloaded.contains(&c.key()) || c.pbf_path(&self.opts.data).exists()
    }

    /// Starts downloading a city's map data (or queues it: a few go at a time). Nothing if it has the
    /// data, or is already on its way (cities in one region share a file).
    fn request_download(&mut self, i: usize) {
        let c = &self.cities[i];
        let Some(src) = c.source() else {
            self.error = Some(format!("{} has no source; add it via \"Add city\".", c.name));
            return;
        };
        let dest = c.pbf_path(&self.opts.data);
        if dest.exists() || self.downloads.contains_key(&dest) || self.download_queue.iter().any(|(d, _)| *d == dest) {
            return;
        }
        self.download_queue.push_back((dest, src.url()));
    }

    /// Starts queued downloads while there is room.
    fn pump_downloads(&mut self, ctx: &egui::Context) {
        while self.downloads.len() < MAX_DOWNLOADS
            && let Some((dest, url)) = self.download_queue.pop_front()
        {
            let ctx = ctx.clone();
            let d = fetch::download(url, dest.clone(), move || ctx.request_repaint());
            self.downloads.insert(dest, d);
        }
    }

    /// How a city's map data is coming: on its way (with progress), or waiting its turn.
    fn download_of(&self, c: &City) -> Option<Result<Arc<Download>, ()>> {
        let dest = c.pbf_path(&self.opts.data);
        match self.downloads.get(&dest) {
            Some(d) => Some(Ok(d.clone())),
            None => self.download_queue.iter().any(|(d, _)| *d == dest).then_some(Err(())),
        }
    }

    /// Sends a planning request if anything relevant changed.
    fn maybe_request(&mut self, pointer_down: bool) {
        let Some(c) = self.city().cloned() else { return };
        if !self.has_data(&c) || pointer_down {
            return;
        }
        let cfg = self.cfg.clone();
        let key = (c.key(), self.base.0, self.base.1, cfg.clone(), self.manuals.clone());
        if self.requested.as_ref() == Some(&key) {
            return;
        }
        self.requested = Some(key);
        self.worker.request(Request { city: c, lat: self.base.0, lon: self.base.1, cfg, manual: self.manuals.clone() });
    }

    /// Takes in news from the planner.
    pub fn apply(&mut self, u: Update) {
        match u {
            Update::Status(s) => self.status = s,
            Update::Built(b) => {
                // World coordinates are relative to the base: keep the
                // same ground in view when the base moves.
                if let Some(old) = &self.built {
                    let shift = old.proj.xy(b.lat, b.lon);
                    self.map.shift(shift);
                }
                self.built = Some(b);
                // The old plan refers to the old graph's nodes, transit
                // stops and coordinates: drawing it against the new one
                // would index out of bounds. The new plan follows.
                self.plan = None;
                self.hover = None;
                self.error = None;
                self.ring = 0;
                self.arc = None;
            }
            Update::Plan(p) => {
                if let Some(i) = self.select_manual.take()
                    && let Some(k) = p.rings.iter().position(|r| r.manual == Some(i))
                {
                    self.ring = k;
                    self.arc = None;
                }
                if self.ring >= p.rings.len() {
                    self.ring = 0;
                }
                if self.arc.is_some_and(|a| p.arcs.get(self.ring).is_none_or(|arcs| a >= arcs.len())) {
                    self.arc = None;
                }
                self.plan = Some(p);
            }
            Update::Error(e) => {
                self.error = Some(e);
                self.status.clear();
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(u) = self.worker.rx.try_recv() {
            self.apply(u);
        }
        // Finished downloads: forget them, and plan if it was ours.
        let done: Vec<PathBuf> =
            self.downloads.iter().filter(|(_, d)| d.done.load(Relaxed)).map(|(k, _)| k.clone()).collect();
        for k in done {
            let d = self.downloads.remove(&k).unwrap();
            if let Some(e) = d.error.lock().unwrap().take() {
                self.error = Some(e);
            }
            self.requested = None;
        }
        self.poll_adds(ctx);
        self.pump_downloads(ctx);
        if self.worker.busy()
            || !self.downloads.is_empty()
            || !self.download_queue.is_empty()
            || self.adder.pending() > 0
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// "3 downloading, 5 waiting", with each city and how far it has got on hover.
    fn downloads_summary(&mut self, ui: &mut egui::Ui) {
        let (active, queued, adding) = (self.downloads.len(), self.download_queue.len(), self.adder.pending());
        if active + queued + adding == 0 {
            return;
        }
        let mut text = format!("⇩ {active} downloading");
        if queued > 0 {
            text.push_str(&format!(", {queued} waiting"));
        }
        if adding > 0 {
            text.push_str(&format!(", {adding} cities to look up"));
        }
        let names = |dest: &PathBuf| -> String {
            let n: Vec<&str> =
                self.cities.iter().filter(|c| c.pbf_path(&self.opts.data) == *dest).map(|c| c.name.as_str()).collect();
            n.join(", ")
        };
        let tip: Vec<String> = self
            .downloads
            .iter()
            .map(|(d, dl)| {
                format!(
                    "{}: {:.0} MB{}",
                    names(d),
                    dl.bytes.load(Relaxed) as f64 / 1e6,
                    dl.fraction().map_or(String::new(), |f| format!(" ({:.0}%)", f * 100.0))
                )
            })
            .chain(self.download_queue.iter().map(|(d, _)| format!("{}: waiting", names(d))))
            .collect();
        ui.label(text).on_hover_text(tip.join("\n"));
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("City");
            let label = self.city().map_or("choose…".to_string(), |c| c.name.clone());
            let mut pick = None;
            egui::ComboBox::from_id_salt("city").selected_text(label).width(160.0).show_ui(ui, |ui| {
                for (i, c) in self.cities.iter().enumerate() {
                    let mark = if self.preloaded.contains(&c.key()) || c.pbf_path(&self.opts.data).exists() { "" } else { "  (no data)" };
                    if ui.selectable_label(self.selected == Some(i), format!("{}{mark}", c.name)).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if let Some(i) = pick {
                self.select_city(i);
            }
            if ui.button("Add city…").clicked() {
                self.add.open = true;
            }
            if let Some(i) = self.selected {
                let c = self.cities[i].clone();
                match self.download_of(&c) {
                    Some(Ok(d)) => {
                        let mb = d.bytes.load(Relaxed) as f32 / 1e6;
                        ui.add(egui::ProgressBar::new(d.fraction().unwrap_or(0.0)).desired_width(180.0).text(format!("downloading {mb:.0} MB")));
                    }
                    Some(Err(())) => {
                        ui.label("waiting to download…");
                    }
                    None if !self.has_data(&c) => {
                        let what = c.source().map_or("no source".into(), |s| s.describe());
                        if ui.button(format!("Download map data ({what})")).clicked() {
                            self.request_download(i);
                        }
                    }
                    None => {}
                }
                self.downloads_summary(ui);
            }
            ui.separator();
            ui.label("Travel").on_hover_text(TRAVEL_TIP);
            for (t, label) in [(crate::config::Travel::Foot, "on foot"), (crate::config::Travel::Bike, "by bike")] {
                if ui.selectable_label(self.cfg.travel == t, label).on_hover_text(TRAVEL_TIP).clicked() && self.cfg.travel != t {
                    self.cfg.travel = t;
                    self.fit_ring();
                }
            }
            ui.label("Reach").on_hover_text(ACCESS_TIP);
            for (m, label) in [(crate::config::Access::Transit, "by transit"), (crate::config::Access::Direct, "from base")] {
                if ui.selectable_label(self.cfg.access == m, label).on_hover_text(ACCESS_TIP).clicked() && self.cfg.access != m {
                    self.cfg.access = m;
                    self.fit_ring();
                }
            }
            ui.separator();
            let dark = ui.ctx().theme() == egui::Theme::Dark;
            if ui.selectable_label(!dark, "Light").on_hover_text("Soft light colours").clicked() {
                ui.ctx().set_theme(egui::Theme::Light);
            }
            if ui.selectable_label(dark, "Dark").on_hover_text("Dark colours").clicked() {
                ui.ctx().set_theme(egui::Theme::Dark);
            }
            ui.toggle_value(&mut self.basemap, "Base map").on_hover_text(
                "Water and parks filled in, rivers and shores, and busy roads drawn heavier, under the walk network.",
            );
            ui.toggle_value(&mut self.high_contrast, "High contrast").on_hover_text("Stronger map colours: streets and paths stand out more from the background.");
            ui.separator();
            ui.label("Wedges").on_hover_text(WEDGES_TIP);
            for (w, label) in [(map::Wedges::Off, "off"), (map::Wedges::Selected, "selected"), (map::Wedges::All, "all")] {
                if ui.selectable_label(self.wedges == w, label).on_hover_text(WEDGES_TIP).clicked() {
                    self.wedges = w;
                }
            }
            ui.separator();
            if ui.add_enabled(self.plan.is_some(), egui::Button::new("Export")).on_hover_text("Write the selected ring to out/<city>/: a printable itinerary (itinerary.html, opened in your browser) with Google/Apple Maps links for each day's trips, GPX tracks for each day and the whole ring (for Garmin, OsmAnd, Komoot, Gaia, Google My Maps), GeoJSON, and a text report.").clicked() {
                self.export();
            }
            if ui.button("Save config").on_hover_text("Write parameters to config.toml").clicked() {
                self.save_config();
            }
            ui.separator();
            if self.worker.busy() {
                ui.spinner();
                let f = self.worker.progress.fraction();
                if self.worker.progress.searching() && f > 0.0 {
                    ui.add(egui::ProgressBar::new(f).desired_width(120.0).show_percentage());
                }
            }
            ui.label(&self.status);
            if let Some(e) = &self.error {
                ui.colored_label(Color32::from_rgb(220, 60, 60), e);
            }
        });
    }

    fn export(&mut self) {
        let (Some(c), Some(b), Some(p)) = (self.city().cloned(), self.built.clone(), self.plan.clone()) else { return };
        let dir = self.opts.out.join(c.key());
        let cfg = self.cfg.clone();
        match crate::output::write_all(&dir, &c.name, &b, &p, &cfg, self.ring) {
            Ok(_) => {
                self.status = format!("wrote {}", dir.display());
                open_in_browser(&dir.join("itinerary.html"));
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn save_config(&mut self) {
        match toml::to_string_pretty(&self.cfg) {
            Ok(t) => match std::fs::write(&self.opts.config, t) {
                Ok(()) => self.status = format!("saved {}", self.opts.config.display()),
                Err(e) => self.error = Some(e.to_string()),
            },
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn save_cities(&mut self) {
        match cities::save(&self.opts.cities, &self.cities) {
            Ok(()) => self.status = format!("saved {}", self.opts.cities.display()),
            Err(e) => self.error = Some(e),
        }
    }

    /// A change to a plan made on the map. The plan becomes (or already is) one of your own, and is saved.
    pub fn edit(&mut self, ring: usize, edit: map::Edit) {
        use map::Edit;
        let (Some(b), Some(plan), Some(c)) = (self.built.clone(), self.plan.clone(), self.city().cloned()) else {
            return;
        };
        let Some(r) = plan.rings.get(ring) else { return };
        let ll = |p: Xy| {
            let (lon, lat) = b.proj.lonlat(p);
            [lat, lon]
        };
        let idx = match r.manual {
            Some(i) => i,
            None => {
                let name = format!("{}", self.manuals.len() + 1);
                self.manuals.push(crate::manual::Manual::from_ring(r, &b.proj, name, self.cfg.travel));
                self.manuals.len() - 1
            }
        };
        // Where along a day a point is, and where the day's routing points are.
        let along = |day: usize, p: Xy| {
            let pts = crate::manual::leg_pts(r, day);
            let at = crate::manual::along(&pts, p).0;
            let vias: Vec<f64> =
                r.via_marks.get(day).into_iter().flatten().map(|&v| crate::manual::along(&pts, v).0).collect();
            (at, vias)
        };
        let m = &mut self.manuals[idx];
        match edit {
            Edit::MoveAnchor(i, to) => m.move_anchor(i, ll(to)),
            Edit::MoveVia(day, j, to) => m.vias[day][j] = ll(to),
            Edit::AddVia(day, at) => {
                let (a, vias) = along(day, at);
                m.add_via(day, ll(at), a, &vias);
            }
            Edit::RemoveVia(day, j) => {
                m.vias[day].remove(j);
            }
            Edit::SplitDay(day, at) => {
                let (a, vias) = along(day, at);
                m.split_day(day, ll(at), a, &vias);
            }
            Edit::RemoveAnchor(i) => m.remove_anchor(i),
        }
        self.select_manual = Some(idx);
        self.save_manuals(&c);
    }

    fn save_manuals(&mut self, c: &City) {
        if let Err(e) = crate::manual::save(&self.edits_dir(), &c.key(), &self.manuals) {
            self.error = Some(e);
        }
    }

    /// Deletes one of your own plans.
    pub fn delete_manual(&mut self, idx: usize) {
        let Some(c) = self.city().cloned() else { return };
        if idx < self.manuals.len() {
            self.manuals.remove(idx);
            self.save_manuals(&c);
            self.ring = 0;
            self.arc = None;
        }
    }

    pub fn set_base(&mut self, world: Xy) {
        if let Some(b) = &self.built {
            let (lon, lat) = b.proj.lonlat(world);
            self.base = (lat, lon);
        } else {
            let (lon, lat) = Proj::new(self.base.0, self.base.1).lonlat(world);
            self.base = (lat, lon);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("params").resizable(true).default_size(310.0).min_size(260.0).show(ui, |ui| self.params(ui));
        egui::Panel::right("results")
            .resizable(true)
            .default_size(380.0)
            .min_size(320.0)
            .show(ui, |ui| self.results(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let busy = self.worker.busy();
            let opt = map::MapOptions {
                ring: self.ring,
                arc: self.arc,
                busy,
                wedges: self.wedges,
                high_contrast: self.high_contrast,
                basemap: self.basemap,
            };
            let ev = self.map.show(ui, self.built.as_ref(), self.plan.as_deref(), &opt);
            self.hover = ev.hover;
            if let Some((r, a)) = ev.select {
                self.ring = r;
                self.arc = a;
            }
            if let Some(w) = ev.set_base {
                self.set_base(w);
            }
            if let Some((ring, edit)) = ev.edit {
                self.edit(ring, edit);
            }
        });
        self.add_city_window(&ctx);
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.maybe_request(pointer_down);
    }
}

/// Opens a file with the system's default handler (best effort).
fn open_in_browser(path: &std::path::Path) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

/// Room around the largest ring when fitting the view.
const FIT_MARGIN: f64 = 1.3;
const TRAVEL_TIP: &str = "on foot: footpaths and quiet streets, steps allowed, about 8 km a day. by bike: cycle ways and \
    signed bicycle routes preferred over footpaths, no steps, hills cost more, much further in a day. Each combination of \
    travel and reach keeps its own days and day length.";
const ACCESS_TIP: &str = "by transit: ride out to each day's start and back from its end. from base: every day goes from the \
    base out to the ring, along an arc, and home, so rings are smaller and closer.";
const WEDGES_TIP: &str = "Draw each outing as a wedge: a shaded fan from the base to its arc, with the trip there and \
    back (solid lines with arrowheads, the return drawn alongside). Walks to and from stops are dotted; transit is drawn stop \
    to stop. \"selected\" shows only the outing picked in the list.";
