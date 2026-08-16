//! Dark, flat, high contrast. No gradients, no animation beyond a sub-100ms
//! selection highlight. Every pixel of chrome is a pixel not showing video.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x1e);
pub const PANEL_ALT: Color32 = Color32::from_rgb(0x23, 0x23, 0x27);
pub const LINE: Color32 = Color32::from_rgb(0x33, 0x33, 0x38);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xe0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x92);
pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x9a, 0xff);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xb0, 0x3a);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x5c, 0x5c);
pub const MARK_IN: Color32 = Color32::from_rgb(0x4c, 0xd1, 0x7a);
pub const MARK_OUT: Color32 = Color32::from_rgb(0xff, 0x8c, 0x42);
pub const CLIP: Color32 = Color32::from_rgb(0x2e, 0x3a, 0x4c);
pub const CLIP_SELECTED: Color32 = Color32::from_rgb(0x33, 0x50, 0x78);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0xd8, 0x4c);
pub const VIDEO_LETTERBOX: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0b);

/// Width of the accent border marking the focused region.
pub const FOCUS_BORDER: f32 = 2.0;

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = PANEL_ALT;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Flat: no corner radius anywhere, no shadows.
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::ZERO;
        w.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.inactive.bg_fill = PANEL_ALT;
    visuals.widgets.inactive.weak_bg_fill = PANEL_ALT;
    visuals.widgets.hovered.bg_fill = LINE;
    visuals.widgets.hovered.weak_bg_fill = LINE;
    visuals.widgets.active.bg_fill = ACCENT.linear_multiply(0.5);
    visuals.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.5);
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(8);
    style.animation_time = 0.08; // the one permitted animation
    ctx.set_style(style);
}
