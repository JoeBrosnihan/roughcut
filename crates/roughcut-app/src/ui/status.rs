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
//! visible (§3's software-decode notice, §4's missing ffprobe) and the result
//! of the last action. When there is nothing to say, this panel occupies zero
//! height.

use crate::app::{RoughcutApp, StatusKind};
use crate::theme;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    let warnings = standing_warnings(app);
    if warnings.is_empty() && app.status.is_none() {
        return;
    }

    egui::TopBottomPanel::bottom("alerts")
        .exact_height(24.0)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL_ALT)
                .inner_margin(egui::Margin::symmetric(8, 3)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;

                for warning in &warnings {
                    ui.label(egui::RichText::new(warning).color(theme::WARN));
                    sep(ui);
                }
                if let Some((text, kind)) = &app.status {
                    let color = match kind {
                        StatusKind::Info => theme::TEXT_DIM,
                        StatusKind::Warn => theme::WARN,
                        StatusKind::Error => theme::ERROR,
                    };
                    ui.label(egui::RichText::new(text).color(color));
                }

                // Dismiss the transient message; standing warnings persist
                // until the condition behind them clears.
                if app.status.is_some() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("×").frame(false))
                            .on_hover_text("dismiss")
                            .clicked()
                        {
                            app.status = None;
                        }
                    });
                }
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
    } else if app.monitor.software_decoding() {
        // §3: hardware decode is mandatory; say so when it is unavailable.
        out.push("software decoding".to_string());
    }
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
