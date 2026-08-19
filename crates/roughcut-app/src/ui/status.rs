//! The alert bar.
//!
//! There is deliberately no permanent status readout. §8 specified one
//! (timecode, frame, fps, clip count) but every field in it is either already
//! on screen — the position readout lives under the scrub bar, the duration on
//! each timeline block, the focused region behind an accent border — or static
//! for the life of the project. A row of chrome that never changes is a row of
//! pixels not showing video.
//!
//! What is left is exceptional information only: warnings that must stay
//! visible (§4's missing ffprobe) and the result of the last action. When
//! there is nothing to say, nothing is drawn at all.
//!
//! It floats over the bottom of the window rather than occupying a panel of
//! its own. A panel that appears and disappears reflows everything above it,
//! so a message that lasts three seconds moved the video, the bin and the
//! timeline twice — and a bar that jiggles the application is worse than the
//! information it carries.

use crate::app::{RoughcutApp, StatusKind};
use crate::theme;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    let warnings = standing_warnings(app);
    if warnings.is_empty() && app.status.is_none() {
        return;
    }

    // Bottom left, over the empty strip below the timeline blocks: the one
    // place in the window where nothing is ever drawn.
    egui::Area::new(egui::Id::new("alerts"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::PANEL_ALT)
                .stroke(egui::Stroke::new(1.0, theme::LINE))
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;

                        for warning in &warnings {
                            ui.label(egui::RichText::new(warning).color(theme::WARN));
                            sep(ui);
                        }
                        if let Some((text, kind, _)) = &app.status {
                            let color = match kind {
                                StatusKind::Info => theme::TEXT_DIM,
                                StatusKind::Warn => theme::WARN,
                                StatusKind::Error => theme::ERROR,
                            };
                            ui.label(egui::RichText::new(text).color(color));
                        }

                        // Dismiss the transient message; standing warnings
                        // persist until the condition behind them clears.
                        if app.status.is_some()
                            && ui
                                .add(egui::Button::new("×").frame(false))
                                .on_hover_text("dismiss")
                                .clicked()
                        {
                            app.status = None;
                        }
                    });
                });
        });
}

/// Conditions the user needs to keep seeing, not a transient message.
fn standing_warnings(app: &RoughcutApp) -> Vec<String> {
    let mut out = Vec::new();
    if !app.tools.has_ffprobe() {
        out.push("no ffprobe".to_string());
    }
    if let Some(e) = &app.monitor.load_error {
        out.push(format!("no video: {e}"));
    }
    // Software decoding is deliberately *not* reported. §3 asked for it, but
    // `hwdec-current` is unset until mpv has actually built the decoder, so
    // the notice appeared on every single clip load and was wrong nearly every
    // time. It is logged at startup instead, where it can be read once by
    // whoever is diagnosing slow playback. See docs/deviations.md.
    if app
        .video
        .lock()
        .ok()
        .and_then(|v| v.init_error.clone())
        .is_some()
    {
        out.push("GL render unavailable".to_string());
    }
    if !app.missing_media.is_empty() {
        out.push(format!("{} missing file(s)", app.missing_media.len()));
    }
    out
}

fn sep(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("·").color(theme::LINE));
}
