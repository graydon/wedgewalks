//! The map view: the walk graph (drawn with OpenGL when available, since
//! it can have a million edges) with rings, outings and stops on top.
//!
//! Line styles each mean one thing: solid coloured lines are the selected
//! ring's arcs; faded solid lines are other rings; solid lines with
//! arrowheads are trips out to an arc and back; dotted lines are short
//! walks to and from transit stops.

use super::draw;
use super::gl::{self as layers, GlGraph, vertices};
use super::style::{MapColors, map_colors, rgba};
use crate::arcs::Arc as Outing;
use crate::graph::{Graph, Xy};
use crate::osm::Kind;
use crate::pipeline::{Built, Plan};
use crate::transit::{Access, Transit};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2, pos2, vec2};
use std::sync::{Arc, Mutex};

/// Arc colours, cycled.
pub const ARC_COLORS: [Color32; 6] = [
    Color32::from_rgb(214, 39, 40),
    Color32::from_rgb(31, 119, 180),
    Color32::from_rgb(255, 127, 14),
    Color32::from_rgb(148, 103, 189),
    Color32::from_rgb(23, 190, 207),
    Color32::from_rgb(227, 119, 194),
];

pub fn arc_color(i: usize) -> Color32 {
    ARC_COLORS[i % ARC_COLORS.len()]
}

const BASE_COLOR: Color32 = Color32::from_rgb(255, 215, 0);
const STOP_COLOR: Color32 = Color32::from_rgb(20, 60, 200);
const GAP_COLOR: Color32 = Color32::RED;
const SELECTED_HALO: Color32 = Color32::from_rgba_premultiplied(120, 120, 0, 120);

/// Line widths in points.
const ARC_WIDTH: f32 = 4.0;
const SELECTED_ARC_WIDTH: f32 = 7.0;
const OTHER_RING_WIDTH: f32 = 2.0;
const TRIP_WIDTH: f32 = 2.5;
/// Return trips are drawn this far to the side of outbound ones.
const TRIP_OFFSET: f32 = 4.0;
/// Translucency of wedge fills.
const WEDGE_ALPHA: f32 = 0.16;
/// Points closer than this are merged when filling wedges.
const WEDGE_THIN_PX: f32 = 3.0;
/// Clicks within this many points of a line select it; other rings need
/// to be this much closer than the selected ring's arcs.
const PICK_PX: f32 = 12.0;
const OTHER_RING_PICK_HANDICAP: f32 = 6.0;
/// Scroll-wheel zoom speed and the zoom range, in points per metre.
const ZOOM_PER_SCROLL: f32 = 0.003;
const SCALE_RANGE: (f32, f32) = (1e-4, 20.0);
const DEFAULT_FIT_M: f64 = 15_000.0;

pub struct MapView {
    /// World point (metres from the base) at the middle of the view.
    pub center: Xy,
    /// Screen points per metre.
    pub scale: f32,
    /// Fit the view to this radius (metres) on the next frame.
    pub fit: Option<f64>,
    /// The radius "Fit view" goes back to.
    pub home: f64,
    gl: Arc<Mutex<GlGraph>>,
    has_gl: bool,
    /// Graph identity currently uploaded (pointer of the Built).
    uploaded: usize,
    /// A handle being dragged, and where the pointer has taken it.
    drag: Option<(Handle, Xy)>,
}

/// Which outings to draw as wedges: the fill from the base out to the arc,
/// plus the trip there and back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Wedges {
    Off,
    #[default]
    Selected,
    All,
}

/// What to draw, besides the data.
pub struct MapOptions {
    pub ring: usize,
    pub arc: Option<usize>,
    pub busy: bool,
    pub wedges: Wedges,
    pub high_contrast: bool,
    /// Draw water, parks and busy roads under the walk graph.
    pub basemap: bool,
}

/// Something on the selected plan that can be dragged: where a day starts or ends (an anchor), or a
/// point a day's route must pass through (day, index).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Anchor(usize),
    Via(usize, usize),
}

/// A change to a plan made on the map.
#[derive(Clone, Copy, Debug)]
pub enum Edit {
    MoveAnchor(usize, Xy),
    MoveVia(usize, usize, Xy),
    /// Add a routing point to a day.
    AddVia(usize, Xy),
    RemoveVia(usize, usize),
    /// Split a day with a new anchor.
    SplitDay(usize, Xy),
    RemoveAnchor(usize),
}

/// How close (screen points) the pointer must be to pick up or point at a handle.
const HANDLE_PICK_PX: f32 = 12.0;
/// A drag shorter than this (screen points) moves nothing.
const HANDLE_MIN_DRAG_PX: f32 = 4.0;
const VIA_COLOR: Color32 = Color32::from_rgb(230, 120, 20);

/// The handles of a plan with their places: its anchors (a ring's last is its first, so left out), and
/// each day's routing points.
fn handle_points(r: &crate::rings::Ring) -> Vec<(Handle, Xy)> {
    let n = if r.is_open() { r.marks.len() } else { r.marks.len().saturating_sub(1) };
    let mut out: Vec<(Handle, Xy)> = r.marks.iter().take(n).enumerate().map(|(i, &p)| (Handle::Anchor(i), p)).collect();
    for (day, vias) in r.via_marks.iter().enumerate() {
        out.extend(vias.iter().enumerate().map(|(j, &p)| (Handle::Via(day, j), p)));
    }
    out
}

/// What the user did on the map this frame.
#[derive(Default)]
pub struct MapEvents {
    /// A change to a plan: (plan, change).
    pub edit: Option<(usize, Edit)>,
    pub select: Option<(usize, Option<usize>)>,
    pub set_base: Option<Xy>,
    pub hover: Option<(usize, usize)>,
}

/// The closest line under the pointer: (distance, ring, arc).
type Hit = Option<(f32, usize, Option<usize>)>;

impl MapView {
    pub fn new(has_gl: bool) -> MapView {
        MapView {
            center: [0.0, 0.0],
            scale: 0.03,
            fit: Some(DEFAULT_FIT_M),
            home: DEFAULT_FIT_M,
            gl: Arc::new(Mutex::new(GlGraph::default())),
            has_gl,
            uploaded: 0,
            drag: None,
        }
    }

    fn to_screen(&self, rect: Rect, p: Xy) -> Pos2 {
        rect.center()
            + vec2(((p[0] - self.center[0]) as f32) * self.scale, -((p[1] - self.center[1]) as f32) * self.scale)
    }

    fn to_world(&self, rect: Rect, p: Pos2) -> Xy {
        let d = p - rect.center();
        [self.center[0] + (d.x / self.scale) as f64, self.center[1] - (d.y / self.scale) as f64]
    }

    /// Re-centres on the base after the base moves (world coordinates are
    /// relative to it), keeping the same piece of ground in view.
    pub fn shift(&mut self, delta: Xy) {
        self.center = [self.center[0] - delta[0], self.center[1] - delta[1]];
    }

    /// World points to screen, merging points less than a pixel apart.
    fn screen_line(&self, rect: Rect, pts: &[Xy]) -> Vec<Pos2> {
        let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
        for &p in pts {
            let s = self.to_screen(rect, p);
            if out.last().is_none_or(|l: &Pos2| l.distance_sq(s) > 1.0) {
                out.push(s);
            }
        }
        if let (Some(&last), Some(&p)) = (out.last(), pts.last()) {
            let s = self.to_screen(rect, p);
            if last != s {
                out.push(s);
            }
        }
        out
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        built: Option<&Arc<Built>>,
        plan: Option<&Plan>,
        opt: &MapOptions,
    ) -> MapEvents {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let colors = map_colors(ui.visuals().dark_mode, opt.high_contrast);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, colors.background);
        let handles = plan.and_then(|p| p.rings.get(opt.ring)).map(handle_points).unwrap_or_default();
        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let near = |(_, w): &&(Handle, Xy)| self.to_screen(rect, *w).distance(pos);
            self.drag = handles
                .iter()
                .filter(|h| near(h) <= HANDLE_PICK_PX)
                .min_by(|a, b| near(a).total_cmp(&near(b)))
                .map(|(h, _)| (*h, self.to_world(rect, pos)));
        }
        if self.drag.is_some()
            && resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let at = self.to_world(rect, pos);
            if let Some((_, w)) = self.drag.as_mut() {
                *w = at;
            }
        }
        self.handle_view_input(ui, rect, &resp, self.drag.is_none());

        let mut hit: Hit = None;
        if let Some(b) = built {
            self.paint_graph(&painter, rect, b, &colors, opt.basemap);
            if let Some(plan) = plan {
                hit = self.paint_plan(&painter, rect, b, plan, opt, &colors, resp.hover_pos());
            }
            self.paint_handles(&painter, rect, &handles);
            let base = self.to_screen(rect, [0.0, 0.0]);
            painter.circle(base, 7.0, BASE_COLOR, Stroke::new(2.0, Color32::BLACK));
            legend(&painter, rect, &colors, opt.basemap);
        } else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Pick a city (or add one) to start",
                FontId::proportional(18.0),
                Color32::GRAY,
            );
        }
        if opt.busy {
            painter.text(
                rect.right_top() + vec2(-10.0, 10.0),
                Align2::RIGHT_TOP,
                "working…",
                FontId::proportional(14.0),
                Color32::GRAY,
            );
        }
        let mut ev = self.events(ui, rect, &resp, hit, plan, opt.ring);
        if resp.drag_stopped()
            && let Some((h, to)) = self.drag.take()
            && let Some(&(_, from)) = handles.iter().find(|(k, _)| *k == h)
            && self.to_screen(rect, from).distance(self.to_screen(rect, to)) >= HANDLE_MIN_DRAG_PX
        {
            ev.edit = Some((
                opt.ring,
                match h {
                    Handle::Anchor(i) => Edit::MoveAnchor(i, to),
                    Handle::Via(d, j) => Edit::MoveVia(d, j, to),
                },
            ));
        }
        ev
    }

    /// Anchors as numbered discs and routing points as orange dots, drawn over the plan; the one being dragged
    /// follows the pointer.
    fn paint_handles(&self, painter: &egui::Painter, rect: Rect, handles: &[(Handle, Xy)]) {
        for &(h, w) in handles {
            let w = match self.drag {
                Some((d, at)) if d == h => at,
                _ => w,
            };
            let p = self.to_screen(rect, w);
            match h {
                Handle::Anchor(i) => {
                    painter.circle(p, 8.0, Color32::WHITE, Stroke::new(2.0, Color32::from_rgb(40, 40, 40)));
                    painter.text(
                        p,
                        Align2::CENTER_CENTER,
                        (i + 1).to_string(),
                        FontId::proportional(10.0),
                        Color32::BLACK,
                    );
                }
                Handle::Via(..) => {
                    painter.circle(p, 5.0, VIA_COLOR, Stroke::new(1.5, Color32::BLACK));
                }
            }
        }
    }

    /// Pan by dragging; zoom with the wheel or a pinch, about the pointer.
    fn handle_view_input(&mut self, ui: &egui::Ui, rect: Rect, resp: &egui::Response, pan: bool) {
        if let Some(r) = self.fit.take() {
            self.center = [0.0, 0.0];
            self.scale = (rect.width().min(rect.height()) / (2.0 * r as f32)).max(SCALE_RANGE.0);
        }
        if pan && resp.dragged() {
            let d = resp.drag_delta();
            self.center = [self.center[0] - (d.x / self.scale) as f64, self.center[1] + (d.y / self.scale) as f64];
        }
        if let Some(hover) = resp.hover_pos() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = zoom * (scroll * ZOOM_PER_SCROLL).exp();
            if factor != 1.0 {
                let before = self.to_world(rect, hover);
                self.scale = (self.scale * factor).clamp(SCALE_RANGE.0, SCALE_RANGE.1);
                let after = self.to_world(rect, hover);
                self.center = [self.center[0] + before[0] - after[0], self.center[1] + before[1] - after[1]];
            }
        }
    }

    fn paint_graph(&mut self, painter: &egui::Painter, rect: Rect, b: &Arc<Built>, colors: &MapColors, basemap: bool) {
        if !self.has_gl {
            self.paint_graph_cpu(painter, rect, &b.graph, colors);
            return;
        }
        let id = Arc::as_ptr(b) as usize;
        if self.uploaded != id {
            self.gl.lock().unwrap().pending = Some(vertices(&b.graph, &b.basemap));
            self.uploaded = id;
        }
        let gl = self.gl.clone();
        let (center, scale) = (self.center, self.scale);
        let ndc = [2.0 * scale / rect.width(), 2.0 * scale / rect.height()];
        let base = |c: Color32| basemap.then(|| rgba(c));
        let mut colours = [None; layers::LAYERS];
        colours[layers::WATER_FILL] = base(colors.water_fill);
        colours[layers::PARK_FILL] = base(colors.park_fill);
        colours[layers::WATER_LINE] = base(colors.water_line);
        colours[layers::BUSY_ROAD] = base(colors.busy_road);
        colours[layers::ROAD] = Some(rgba(colors.road));
        colours[layers::BESIDE] = Some(rgba(colors.beside));
        colours[layers::PATH] = Some(rgba(colors.path));
        // One pixel in clip space (which spans 2 across the rect).
        let pixel = [2.0 / rect.width(), 2.0 / rect.height()];
        let cb = egui_glow::CallbackFn::new(move |_info, painter| {
            gl.lock().unwrap().paint(painter.gl(), center, ndc, pixel, colours);
        });
        painter.add(egui::PaintCallback { rect, callback: Arc::new(cb) });
    }

    /// Fallback without OpenGL (e.g. tests): visible edges only.
    fn paint_graph_cpu(&self, painter: &egui::Painter, rect: Rect, g: &Graph, colors: &MapColors) {
        let view = rect.expand(20.0);
        let mut shapes = vec![];
        for e in &g.edges {
            let (a, b) = (self.to_screen(rect, e.pts[0]), self.to_screen(rect, *e.pts.last().unwrap()));
            if !view.contains(a) && !view.contains(b) {
                continue;
            }
            let color = match (e.kind, e.beside) {
                (Kind::Road, _) => colors.road,
                (Kind::CarFree, true) => colors.beside,
                (Kind::CarFree, false) => colors.path,
            };
            shapes.push(Shape::line(self.screen_line(rect, &e.pts), Stroke::new(1.0, color)));
        }
        painter.extend(shapes);
    }

    /// Other rings, wedges, the selected ring's arcs and markers. Returns
    /// the line under the pointer, if any.
    #[allow(clippy::too_many_arguments)]
    fn paint_plan(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        b: &Built,
        plan: &Plan,
        opt: &MapOptions,
        colors: &MapColors,
        pointer: Option<Pos2>,
    ) -> Hit {
        let mut hit: Hit = None;
        let mut consider = |pts: &[Pos2], ring: usize, arc: Option<usize>| {
            if let Some(p) = pointer {
                let d = draw::polyline_dist(pts, p) + if arc.is_none() { OTHER_RING_PICK_HANDICAP } else { 0.0 };
                if d < PICK_PX && hit.is_none_or(|h| d < h.0) {
                    hit = Some((d, ring, arc));
                }
            }
        };
        for (ri, r) in plan.rings.iter().enumerate().filter(|(ri, _)| *ri != opt.ring) {
            let pts = self.screen_line(rect, &r.pts);
            consider(&pts, ri, None);
            let c = if r.ok() { colors.other_ring_ok } else { colors.other_ring };
            painter.add(Shape::line(pts, Stroke::new(OTHER_RING_WIDTH, c)));
        }
        let Some(arcs) = plan.arcs.get(opt.ring) else { return hit };
        for (ai, a) in arcs.iter().enumerate() {
            let show = match opt.wedges {
                Wedges::Off => false,
                Wedges::Selected => opt.arc == Some(ai),
                Wedges::All => true,
            };
            if show {
                self.paint_wedge(painter, rect, &b.transit, a, arc_color(ai), colors);
            }
        }
        for (ai, a) in arcs.iter().enumerate() {
            let pts = self.screen_line(rect, &a.pts);
            consider(&pts, opt.ring, Some(ai));
            let selected = opt.arc == Some(ai);
            if selected {
                painter.add(Shape::line(pts.clone(), Stroke::new(SELECTED_ARC_WIDTH + 4.0, SELECTED_HALO)));
            }
            let width = if selected { SELECTED_ARC_WIDTH } else { ARC_WIDTH };
            painter.add(Shape::line(pts.clone(), Stroke::new(width, arc_color(ai))));
            if let Some(&p) = pts.first() {
                painter.circle(p, 4.5, Color32::WHITE, Stroke::new(1.5, Color32::BLACK));
            }
            if let Some(&mid) = pts.get(pts.len() / 2) {
                painter.text(
                    mid + vec2(6.0, -6.0),
                    Align2::LEFT_BOTTOM,
                    format!("{}", ai + 1),
                    FontId::proportional(15.0),
                    colors.text,
                );
            }
            for acc in [&a.start, &a.end].into_iter().flatten() {
                let s = self.to_screen(rect, b.transit.stops[acc.stop].xy);
                painter.rect_filled(Rect::from_center_size(s, Vec2::splat(7.0)), 1.0, STOP_COLOR);
            }
        }
        if let Some(r) = plan.rings.get(opt.ring)
            && r.stats.max_gap > 0.0
        {
            cross_mark(painter, self.to_screen(rect, r.stats.gap_at), GAP_COLOR);
        }
        hit
    }

    /// One outing's wedge: an even translucent fill from the base over the
    /// arc, and the trips out and back (solid with arrowheads; walks to and
    /// from stops dotted).
    fn paint_wedge(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        t: &Transit,
        a: &Outing,
        color: Color32,
        colors: &MapColors,
    ) {
        let base = self.to_screen(rect, [0.0, 0.0]);
        let mut outline = vec![base];
        outline.extend(draw::thin(&self.screen_line(rect, &a.pts), WEDGE_THIN_PX));
        painter.add(draw::fill_polygon(&outline, color.gamma_multiply(WEDGE_ALPHA)));

        let stroke = Stroke::new(TRIP_WIDTH, color);
        // Walking mode: the walks out and home.
        if let Some(w) = &a.walk_out {
            painter.extend(draw::arrow_line(&self.screen_line(rect, &w.pts), stroke));
        }
        if let Some(w) = &a.walk_back {
            let pts: Vec<Pos2> = self.screen_line(rect, &w.pts).into_iter().rev().collect();
            painter.extend(draw::arrow_line(&draw::offset(&pts, TRIP_OFFSET), stroke));
        }
        // Transit mode: walk to the stop, ride, (change), ride, walk.
        if let Some(s) = &a.start {
            painter.extend(self.trip(rect, t, s, [0.0, 0.0], a.pts[0], stroke, colors, 0.0));
        }
        if let Some(e) = &a.end {
            painter.extend(self.trip(rect, t, e, *a.pts.last().unwrap(), [0.0, 0.0], stroke, colors, TRIP_OFFSET));
        }
    }

    /// The shapes for one transit trip between `from` and `to`, with its
    /// legs in travel order; offset sideways by `side`.
    #[allow(clippy::too_many_arguments)]
    fn trip(
        &self,
        rect: Rect,
        t: &Transit,
        acc: &Access,
        from: Xy,
        to: Xy,
        stroke: Stroke,
        colors: &MapColors,
        side: f32,
    ) -> Vec<Shape> {
        let walk = |p: Xy, q: Xy| {
            Shape::dotted_line(&[self.to_screen(rect, p), self.to_screen(rect, q)], colors.text, 6.0, 1.3)
        };
        let first = t.stops[acc.legs[0].board].xy;
        let last = t.stops[acc.legs[acc.legs.len() - 1].alight].xy;
        let mut shapes = walk(from, first);
        shapes.extend(walk(last, to));
        for w in acc.legs.windows(2) {
            shapes.extend(walk(t.stops[w[0].alight].xy, t.stops[w[1].board].xy));
        }
        for l in &acc.legs {
            let pts = draw::offset(&self.screen_line(rect, &t.leg_points(l)), side);
            for p in [pts.first(), pts.last()].into_iter().flatten() {
                shapes.push(Shape::circle_filled(*p, 3.5, stroke.color));
            }
            shapes.extend(draw::arrow_line(&pts, stroke));
        }
        shapes
    }

    /// Hover, click and right-click handling.
    fn events(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        resp: &egui::Response,
        hit: Hit,
        plan: Option<&Plan>,
        ring: usize,
    ) -> MapEvents {
        let mut ev = MapEvents::default();
        if let Some((_, r, a)) = hit {
            ev.hover = a.map(|a| (r, a));
            if resp.clicked() {
                ev.select = Some((r, a));
            }
        }
        let menu_id = egui::Id::new("map_menu_at");
        if resp.secondary_clicked() {
            let p = resp.interact_pointer_pos().unwrap_or(rect.center());
            ui.memory_mut(|m| m.data.insert_temp(menu_id, self.to_world(rect, p)));
        }
        resp.context_menu(|ui| {
            let at: Option<Xy> = ui.memory(|m| m.data.get_temp(menu_id));
            if ui.button("Set base here").clicked() {
                ev.set_base = at;
                ui.close();
            }
            // Editing the selected plan: its handles and days are what the pointer is near.
            if let (Some(w), Some(r)) = (at, plan.and_then(|p| p.rings.get(ring))) {
                let px = |p: Xy| self.to_screen(rect, p).distance(self.to_screen(rect, w));
                let near = handle_points(r)
                    .into_iter()
                    .filter(|&(_, p)| px(p) <= HANDLE_PICK_PX * 1.5)
                    .min_by(|a, b| px(a.1).total_cmp(&px(b.1)));
                match near {
                    Some((Handle::Via(d, j), _)) => {
                        if ui.button("Remove this routing point").clicked() {
                            ev.edit = Some((ring, Edit::RemoveVia(d, j)));
                            ui.close();
                        }
                    }
                    Some((Handle::Anchor(i), _)) => {
                        if ui.button("Remove this anchor (merge its two days)").clicked() {
                            ev.edit = Some((ring, Edit::RemoveAnchor(i)));
                            ui.close();
                        }
                    }
                    None => {
                        if let Some((day, ..)) = crate::manual::nearest_leg(r, w) {
                            if ui.button(format!("Add a routing point to day {} here", day + 1)).clicked() {
                                ev.edit = Some((ring, Edit::AddVia(day, w)));
                                ui.close();
                            }
                            if ui.button(format!("Split day {} here (new anchor)", day + 1)).clicked() {
                                ev.edit = Some((ring, Edit::SplitDay(day, w)));
                                ui.close();
                            }
                        }
                    }
                }
            }
            if ui.button("Fit view").clicked() {
                self.fit = Some(self.home);
                ui.close();
            }
        });
        ev
    }
}

fn cross_mark(painter: &egui::Painter, at: Pos2, color: Color32) {
    let s = Stroke::new(2.5, color);
    painter.line_segment([at + vec2(-6.0, -6.0), at + vec2(6.0, 6.0)], s);
    painter.line_segment([at + vec2(-6.0, 6.0), at + vec2(6.0, -6.0)], s);
}

/// How a legend entry is drawn.
enum Mark {
    Line(f32),
    Fill,
    Arrow,
    Dotted,
    Base,
    Stop,
    Cross,
    Anchor,
    Via,
}

fn legend(painter: &egui::Painter, rect: Rect, colors: &MapColors, basemap: bool) {
    const ROW: f32 = 18.0;
    let mut items: Vec<(&str, Color32, Mark)> = vec![
        ("car-free path", colors.path, Mark::Line(2.0)),
        ("path beside traffic", colors.beside, Mark::Line(2.0)),
        ("road", colors.road, Mark::Line(2.0)),
        ("other rings", colors.other_ring, Mark::Line(OTHER_RING_WIDTH)),
        ("trip out and back", colors.text, Mark::Arrow),
        ("walk to or from a stop", colors.text, Mark::Dotted),
        ("base", BASE_COLOR, Mark::Base),
        ("transit stop", STOP_COLOR, Mark::Stop),
        ("longest road gap", GAP_COLOR, Mark::Cross),
        ("anchor: drag to move", Color32::WHITE, Mark::Anchor),
        ("routing point", VIA_COLOR, Mark::Via),
    ];
    if basemap {
        items.push(("busy road", colors.busy_road, Mark::Line(3.0)));
        items.push(("water", colors.water_fill, Mark::Fill));
        items.push(("park or wood", colors.park_fill, Mark::Fill));
    }
    let origin = rect.left_bottom() + vec2(10.0, -10.0 - ROW * items.len() as f32);
    painter.rect_filled(
        Rect::from_min_size(origin - vec2(6.0, 6.0), vec2(185.0, ROW * items.len() as f32 + 10.0)),
        4.0,
        colors.legend_bg,
    );
    for (i, (label, c, mark)) in items.into_iter().enumerate() {
        let y = origin.y + ROW * i as f32 + 8.0;
        let (a, b, m) = (pos2(origin.x, y), pos2(origin.x + 22.0, y), pos2(origin.x + 11.0, y));
        match mark {
            Mark::Line(w) => {
                painter.line_segment([a, b], Stroke::new(w, c));
            }
            Mark::Fill => {
                painter.rect_filled(Rect::from_min_max(a - vec2(0.0, 5.0), b + vec2(0.0, 5.0)), 2.0, c);
            }
            Mark::Arrow => painter.extend(draw::arrow_line(&[a, b], Stroke::new(TRIP_WIDTH, c))),
            Mark::Dotted => painter.extend(Shape::dotted_line(&[a, b], c, 6.0, 1.3)),
            Mark::Base => {
                painter.circle(m, 6.0, c, Stroke::new(1.5, Color32::BLACK));
            }
            Mark::Stop => {
                painter.rect_filled(Rect::from_center_size(m, Vec2::splat(7.0)), 1.0, c);
            }
            Mark::Cross => cross_mark(painter, m, c),
            Mark::Anchor => {
                painter.circle(m, 6.0, c, Stroke::new(2.0, Color32::from_rgb(40, 40, 40)));
            }
            Mark::Via => {
                painter.circle(m, 4.0, c, Stroke::new(1.5, Color32::BLACK));
            }
        }
        painter.text(pos2(origin.x + 30.0, y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), colors.text);
    }
}
