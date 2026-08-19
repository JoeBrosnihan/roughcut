//! The notification tray.
//!
//! There is deliberately no permanent status readout. §8 specified one
//! (timecode, frame, fps, clip count) but every field in it is either already
//! on screen — the position readout lives under the scrub bar, the duration on
//! each timeline block, the focused region behind an accent border — or static
//! for the life of the project. A row of chrome that never changes is a row of
//! pixels not showing video.
//!
//! What is left is exceptional information only: standing warnings that must
//! stay visible until the condition behind them clears, and the result of the
//! last few actions, which fade.
//!
//! They float in the top right corner rather than occupying a panel. A panel
//! that takes height only when it has something to say reflows everything
//! above it, so a message lasting five seconds moved the video, the bin and
//! the timeline twice — and an application that jiggles is worse than the
//! information that made it jiggle. Overlaying costs nothing but a corner of
//! the picture, and only while there is something to read.

use crate::app::{RoughcutApp, StatusKind};
use crate::theme;

/// Wide enough for a path, narrow enough to leave the picture visible.
const WIDTH: f32 = 320.0;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    let warnings = standing_warnings(app);
    if warnings.is_empty() && app.toasts.is_empty() {
        return;
    }

    // Newest at the top, so the thing that just happened is where the eye
    // already is; standing warnings sit below them, because they are context
    // rather than news.
    let mut dismiss: Option<usize> = None;
    egui::Area::new(egui::Id::new("toasts"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_max_width(WIDTH);
            for (i, toast) in app.toasts.iter().enumerate().rev() {
                if toast_card(ui, &toast.text, colour(toast.kind), true) {
                    dismiss = Some(i);
                }
            }
            for warning in &warnings {
                toast_card(ui, warning, theme::WARN, false);
            }
        });

    if let Some(i) = dismiss {
        app.toasts.remove(i);
    }
}

fn colour(kind: StatusKind) -> egui::Color32 {
    match kind {
        StatusKind::Info => theme::TEXT,
        StatusKind::Warn => theme::WARN,
        StatusKind::Error => theme::ERROR,
    }
}

/// One card. Returns true if it was clicked, which dismisses it.
///
/// The whole card is the dismiss target rather than a small ×: it is a
/// notification, not a dialog, and there is nothing else it could mean.
fn toast_card(ui: &mut egui::Ui, text: &str, colour: egui::Color32, dismissable: bool) -> bool {
    let mut clicked = false;
    egui::Frame::new()
        .fill(theme::PANEL_ALT)
        // A left edge in the message's own colour, so severity is readable
        // before the words are.
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .inner_margin(egui::Margin::symmetric(9, 6))
        .outer_margin(egui::Margin {
            bottom: 6,
            ..Default::default()
        })
        .show(ui, |ui| {
            ui.set_width(WIDTH - 18.0);
            let response = ui.add(
                egui::Label::new(egui::RichText::new(text).size(11.5).color(colour))
                    .wrap()
                    .sense(if dismissable {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    }),
            );
            if dismissable {
                if response.clicked() {
                    clicked = true;
                }
                response.on_hover_text("dismiss");
            }
        });
    clicked
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
    // time. It is logged instead, where it can be read once by whoever is
    // diagnosing slow playback. See docs/deviations.md.
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
