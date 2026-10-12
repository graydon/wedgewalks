//! Fonts and colours.

use egui::{Color32, FontData, FontDefinitions, FontFamily, Visuals};

/// System fonts with wide Unicode coverage, tried in order and added as
/// fallbacks after egui's built-in fonts, so place and route names in any
/// script (and symbols like arrows) render instead of showing boxes.
const FALLBACK_FONTS: &[&str] = &[
    // macOS
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial Unicode.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    // Linux
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    // Windows
    "C:\\Windows\\Fonts\\seguisym.ttf",
    "C:\\Windows\\Fonts\\arialuni.ttf",
];

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (i, path) in FALLBACK_FONTS.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = format!("fallback-{i}");
        fonts.font_data.insert(name.clone(), FontData::from_owned(bytes).into());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
    }
    ctx.set_fonts(fonts);
    ctx.set_visuals_of(egui::Theme::Light, soft_light());
}

/// A gentle light theme: warm paper background, soft greys, muted accents.
fn soft_light() -> Visuals {
    let mut v = Visuals::light();
    let paper = Color32::from_rgb(244, 241, 234);
    let panel = Color32::from_rgb(238, 234, 225);
    let ink = Color32::from_rgb(70, 66, 60);
    v.panel_fill = panel;
    v.window_fill = paper;
    v.extreme_bg_color = Color32::from_rgb(250, 248, 243);
    v.faint_bg_color = Color32::from_rgb(232, 228, 218);
    v.code_bg_color = Color32::from_rgb(232, 228, 218);
    v.override_text_color = Some(ink);
    v.hyperlink_color = Color32::from_rgb(70, 110, 150);
    v.selection.bg_fill = Color32::from_rgb(203, 220, 232);
    v.selection.stroke.color = Color32::from_rgb(60, 90, 120);
    v.window_stroke.color = Color32::from_rgb(214, 208, 196);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke.color = ink;
    }
    v.widgets.noninteractive.bg_fill = panel;
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(218, 212, 200);
    v.widgets.inactive.bg_fill = Color32::from_rgb(226, 221, 210);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(226, 221, 210);
    v.widgets.hovered.bg_fill = Color32::from_rgb(216, 210, 197);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(216, 210, 197);
    v.widgets.active.bg_fill = Color32::from_rgb(205, 198, 184);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(205, 198, 184);
    v
}

/// Map colours for one theme.
pub struct MapColors {
    pub background: Color32,
    pub road: Color32,
    pub path: Color32,
    pub beside: Color32,
    pub text: Color32,
    pub legend_bg: Color32,
    pub other_ring: Color32,
    pub other_ring_ok: Color32,
    /// The base map under the walk graph.
    pub water_fill: Color32,
    pub park_fill: Color32,
    pub water_line: Color32,
    pub busy_road: Color32,
}

/// Map colours for a theme. The normal palettes keep the street network
/// clearly visible; `high` pushes it further from the background.
pub fn map_colors(dark: bool, high: bool) -> MapColors {
    match (dark, high) {
        (true, false) => MapColors {
            background: Color32::from_gray(16),
            road: Color32::from_gray(78),
            path: Color32::from_rgb(70, 160, 80),
            beside: Color32::from_rgb(170, 140, 55),
            text: Color32::from_gray(225),
            legend_bg: Color32::from_black_alpha(190),
            other_ring: Color32::from_gray(150).gamma_multiply(0.8),
            other_ring_ok: Color32::from_rgb(140, 160, 240).gamma_multiply(0.8),
            water_fill: Color32::from_rgb(22, 40, 62),
            park_fill: Color32::from_rgb(25, 44, 29),
            water_line: Color32::from_rgb(52, 92, 140),
            busy_road: Color32::from_rgb(92, 84, 96),
        },
        (true, true) => MapColors {
            background: Color32::BLACK,
            road: Color32::from_gray(120),
            path: Color32::from_rgb(90, 220, 100),
            beside: Color32::from_rgb(235, 190, 60),
            text: Color32::WHITE,
            legend_bg: Color32::from_black_alpha(220),
            other_ring: Color32::from_gray(190),
            other_ring_ok: Color32::from_rgb(160, 180, 255),
            water_fill: Color32::from_rgb(14, 48, 92),
            park_fill: Color32::from_rgb(14, 58, 24),
            water_line: Color32::from_rgb(70, 130, 210),
            busy_road: Color32::from_rgb(150, 140, 165),
        },
        (false, false) => MapColors {
            background: Color32::from_rgb(244, 241, 234),
            road: Color32::from_rgb(178, 170, 158),
            path: Color32::from_rgb(70, 145, 75),
            beside: Color32::from_rgb(196, 140, 40),
            text: Color32::from_rgb(55, 52, 48),
            legend_bg: Color32::from_rgba_unmultiplied(250, 248, 243, 225),
            other_ring: Color32::from_rgb(110, 104, 96).gamma_multiply(0.75),
            other_ring_ok: Color32::from_rgb(70, 100, 160).gamma_multiply(0.75),
            water_fill: Color32::from_rgb(178, 208, 232),
            park_fill: Color32::from_rgb(208, 228, 192),
            water_line: Color32::from_rgb(118, 158, 204),
            busy_road: Color32::from_rgb(214, 206, 222),
        },
        (false, true) => MapColors {
            background: Color32::WHITE,
            road: Color32::from_gray(120),
            path: Color32::from_rgb(20, 125, 35),
            beside: Color32::from_rgb(190, 115, 0),
            text: Color32::BLACK,
            legend_bg: Color32::from_white_alpha(235),
            other_ring: Color32::from_gray(70),
            other_ring_ok: Color32::from_rgb(30, 70, 160),
            water_fill: Color32::from_rgb(140, 188, 238),
            park_fill: Color32::from_rgb(176, 218, 156),
            water_line: Color32::from_rgb(40, 100, 190),
            busy_road: Color32::from_rgb(172, 160, 196),
        },
    }
}

pub fn rgba(c: Color32) -> [f32; 4] {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}
