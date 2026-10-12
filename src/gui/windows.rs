//! The "Add cities" window.

use super::App;
use super::panels::WARN_TEXT;
use crate::cities::City;
use crate::fetch::{self, Place, Source};
use egui::Color32;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// Result slot filled by a background thread.
type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

fn spawn_slot<T: Send + 'static>(
    ctx: &egui::Context,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Slot<T> {
    let slot: Slot<T> = Arc::default();
    let (s, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        let r = f();
        *s.lock().unwrap() = Some(r);
        ctx.request_repaint();
    });
    slot
}

/// A slot's result once it is ready; shows a spinner and `waiting` until
/// then, and the error if it failed.
fn poll_slot<T: Clone>(ui: &mut egui::Ui, slot: Option<&Slot<T>>, waiting: &str) -> Option<T> {
    match slot?.lock().unwrap().clone() {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(waiting);
            });
            None
        }
        Some(Err(e)) => {
            ui.colored_label(Color32::RED, e);
            None
        }
        Some(Ok(v)) => Some(v),
    }
}

/// A city looked up in the background for a bulk add.
enum Found {
    Ready { name: String, place: Place, source: Source, size: Option<u64> },
    Failed(String),
}

/// Looks cities up (their place, then where their map data comes from) one after another on a
/// background thread, a second apart as Nominatim asks, and hands them back to be added.
pub(super) struct Adder {
    tx: Option<Sender<(String, String)>>,
    rx: Receiver<Found>,
    found_tx: Sender<Found>,
    submitted: usize,
    finished: usize,
    /// Cities that could not be found, and ones whose map data is a big download (so not started).
    failed: Vec<String>,
    big: Vec<(String, u64)>,
}

impl Default for Adder {
    fn default() -> Adder {
        let (found_tx, rx) = channel();
        Adder { tx: None, rx, found_tx, submitted: 0, finished: 0, failed: vec![], big: vec![] }
    }
}

/// Nominatim's usage policy: at most one request a second.
const NOMINATIM_GAP: std::time::Duration = std::time::Duration::from_millis(1100);

impl Adder {
    pub(super) fn pending(&self) -> usize {
        self.submitted - self.finished
    }

    fn submit(&mut self, ctx: &egui::Context, data: std::path::PathBuf, margin_km: f64, name: &str, country: &str) {
        if self.tx.is_none() {
            let (tx, jobs) = channel::<(String, String)>();
            let (out, ctx) = (self.found_tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                while let Ok((name, country)) = jobs.recv() {
                    let found = match fetch::resolve(&name, &country, &data, margin_km) {
                        Ok((place, source)) => {
                            let size = fetch::content_length(&source.url());
                            Found::Ready { name: name.clone(), place, source, size }
                        }
                        Err(e) => Found::Failed(format!("{name}: {e}")),
                    };
                    let _ = out.send(found);
                    ctx.request_repaint();
                    std::thread::sleep(NOMINATIM_GAP);
                }
            });
            self.tx = Some(tx);
        }
        if let Some(tx) = &self.tx
            && tx.send((name.to_string(), country.to_string())).is_ok()
        {
            self.submitted += 1;
        }
    }
}

/// How often to check whether a download's size has arrived.
const SIZE_POLL: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Default, Clone, Copy, PartialEq)]
enum Tab {
    #[default]
    Search,
    Popular,
}

#[derive(Default)]
pub(super) struct AddCity {
    pub open: bool,
    tab: Tab,
    /// Popular cities ticked (indices into `gawc::CITIES`), and the filter on the list.
    ticked: HashSet<usize>,
    filter: String,
    query: String,
    results: Option<Slot<Vec<Place>>>,
    chosen: Option<Place>,
    source: Option<Slot<Source>>,
    name: String,
}

impl App {
    pub(super) fn add_city_window(&mut self, ctx: &egui::Context) {
        let mut open = self.add.open;
        let mut added = None;
        egui::Window::new("Add cities").open(&mut open).default_width(460.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.add.tab, Tab::Search, "By name");
                ui.selectable_value(&mut self.add.tab, Tab::Popular, "Popular cities");
            });
            ui.separator();
            match self.add.tab {
                Tab::Search => {
                    self.place_search(ui, ctx);
                    self.place_results(ui, ctx);
                    added = self.chosen_place(ui, ctx);
                }
                Tab::Popular => self.popular_cities(ui, ctx),
            }
        });
        self.add.open = open;
        if let Some(c) = added {
            self.cities.push(c);
            self.save_cities();
            let i = self.cities.len() - 1;
            self.select_city(i);
            if !self.has_data(&self.cities[i]) {
                self.request_download(i);
            }
            let tab = self.add.tab;
            self.add = AddCity { tab, ..Default::default() };
        }
    }

    /// The seed list of world cities to tick off, and the button that adds them all: each is looked up and its
    /// map data fetched in the background, a few at a time.
    fn popular_cities(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::gawc::{CITIES, TIERS};
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut self.add.filter);
        });
        let have = |name: &str| self.cities.iter().any(|c| c.name.eq_ignore_ascii_case(name));
        let filter = self.add.filter.trim().to_lowercase();
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (t, tier) in TIERS.iter().enumerate() {
                let rows: Vec<usize> = (0..CITIES.len())
                    .filter(|&i| CITIES[i].0 == *tier)
                    .filter(|&i| filter.is_empty() || CITIES[i].1.to_lowercase().contains(&filter))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                let ticked = rows.iter().filter(|i| self.add.ticked.contains(i)).count();
                let title = format!(
                    "{tier}  ({} cities{})",
                    rows.len(),
                    if ticked > 0 { format!(", {ticked} ticked") } else { String::new() }
                );
                egui::CollapsingHeader::new(title).id_salt(("tier", t)).default_open(t < 2 || !filter.is_empty()).show(
                    ui,
                    |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("tick all").clicked() {
                                self.add.ticked.extend(rows.iter().copied().filter(|&i| !have(CITIES[i].1)));
                            }
                            if ui.small_button("none").clicked() {
                                for i in &rows {
                                    self.add.ticked.remove(i);
                                }
                            }
                        });
                        for &i in &rows {
                            let (_, name, country) = CITIES[i];
                            if have(name) {
                                ui.add_enabled(
                                    false,
                                    egui::Checkbox::new(&mut true, format!("{name}, {country}  (added)")),
                                );
                            } else {
                                let mut on = self.add.ticked.contains(&i);
                                if ui.checkbox(&mut on, format!("{name}, {country}")).changed() {
                                    if on {
                                        self.add.ticked.insert(i);
                                    } else {
                                        self.add.ticked.remove(&i);
                                    }
                                }
                            }
                        }
                    },
                );
            }
        });
        ui.separator();
        let n = self.add.ticked.len();
        if ui.add_enabled(n > 0, egui::Button::new(format!("Add {n} selected"))).clicked() {
            let mut order: Vec<usize> = self.add.ticked.drain().collect();
            order.sort();
            for i in order {
                let (_, name, country) = CITIES[i];
                let (data, margin) = (self.opts.data.clone(), self.cfg.extract_radius_km);
                self.adder.submit(ctx, data, margin, name, country);
            }
        }
        if self.adder.pending() > 0 {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("{} cities to look up (about a second each)", self.adder.pending()));
            });
        }
        for f in &self.adder.failed {
            ui.colored_label(Color32::RED, f);
        }
        for (name, bytes) in &self.adder.big {
            ui.colored_label(
                WARN_TEXT,
                format!("{name}: its map data is a big download ({:.1} GB), so it was not started: pick the city and press its Download button.", *bytes as f64 / 1e9),
            );
        }
        ui.label(
            egui::RichText::new("The GaWC 2020 Alpha, Beta and Gamma world cities (from the Wikipedia list).")
                .weak()
                .small(),
        );
    }

    /// Takes in cities the adder has looked up: adds them to the list, and starts their downloads unless
    /// they are big.
    pub(super) fn poll_adds(&mut self, _ctx: &egui::Context) {
        let mut any = false;
        while let Ok(f) = self.adder.rx.try_recv() {
            self.adder.finished += 1;
            any = true;
            match f {
                Found::Failed(e) => self.adder.failed.push(e),
                Found::Ready { name, place, source, size } => {
                    if self.cities.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
                        continue;
                    }
                    self.cities.push(City {
                        name: name.clone(),
                        country: place.country.clone(),
                        lat: place.lat,
                        lon: place.lon,
                        notes: place.display.clone(),
                        source: source.to_field(),
                        ..Default::default()
                    });
                    let i = self.cities.len() - 1;
                    match size {
                        Some(b) if b > fetch::BIG_DOWNLOAD && !self.has_data(&self.cities[i]) => {
                            self.adder.big.push((name, b));
                        }
                        _ => {
                            if !self.has_data(&self.cities[i]) {
                                self.request_download(i);
                            }
                        }
                    }
                }
            }
        }
        if any {
            self.save_cities();
        }
    }

    /// The search box; starts a place search in the background.
    fn place_search(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let edit = ui.text_edit_singleline(&mut self.add.query);
            let go =
                ui.button("Search").clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && !self.add.query.trim().is_empty() {
                let q = self.add.query.clone();
                self.add.results = Some(spawn_slot(ctx, move || fetch::search_places(&q)));
                self.add.chosen = None;
                self.add.source = None;
            }
        });
    }

    /// The matching places; choosing one starts looking for its map data.
    fn place_results(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(places) = poll_slot(ui, self.add.results.as_ref(), "searching…") else { return };
        if places.is_empty() {
            ui.label("No matches.");
        }
        for p in places {
            let chosen = self.add.chosen.as_ref().is_some_and(|c| c.display == p.display);
            let label = match &p.native {
                Some(n) => format!("{}  ({n})", p.display),
                None => p.display.clone(),
            };
            if ui.selectable_label(chosen, label).clicked() {
                self.add.name = p.name.clone();
                let (place, data) = (p.clone(), self.opts.data.clone());
                let margin = self.cfg.extract_radius_km;
                self.add.source = Some(spawn_slot(ctx, move || fetch::choose_source(&place, &data, margin)));
                self.add.chosen = Some(p);
            }
        }
    }

    /// The chosen place's name and map data, and the button that adds it.
    fn chosen_place(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> Option<City> {
        let p = self.add.chosen.clone()?;
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.add.name);
        });
        ui.label(format!("{:.4}, {:.4} ({})", p.lat, p.lon, p.country));
        let src = poll_slot(ui, self.add.source.as_ref(), "finding map data…")?;
        let size = fetch::content_length_cached(&src.url());
        if size.is_none() {
            ctx.request_repaint_after(SIZE_POLL);
        }
        ui.label(format!(
            "Map data: {}{}",
            src.describe(),
            size.map_or(String::new(), |s| format!(", {:.0} MB", s as f64 / 1e6))
        ));
        if size.is_some_and(|s| s > fetch::BIG_DOWNLOAD) {
            ui.colored_label(
                WARN_TEXT,
                "That is a big download. Cities inside this region share it, but it may take a while and a lot of disk.",
            );
        }
        let name = self.add.name.trim();
        let taken = self.cities.iter().any(|c| c.name.eq_ignore_ascii_case(name));
        if taken {
            ui.colored_label(WARN_TEXT, "A city with that name exists.");
        }
        let add = ui.add_enabled(!taken && !name.is_empty(), egui::Button::new("Add and download")).clicked();
        add.then(|| City {
            name: name.to_string(),
            country: p.country.clone(),
            lat: p.lat,
            lon: p.lon,
            notes: p.display.clone(),
            source: src.to_field(),
            ..Default::default()
        })
    }
}
