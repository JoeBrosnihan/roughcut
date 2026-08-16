//! The two modal dialogs: the missing-`ffprobe` block on first import, and
//! the relink list shown when a project's media has moved.

use crate::app::{RoughcutApp, StatusKind};
use crate::theme;
use roughcut_core::model::ClipId;
use std::path::PathBuf;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    // Recovery comes first: it decides which project the others apply to.
    recovery(app, ctx);
    missing_tool(app, ctx);
    relink(app, ctx);
}

/// Offered once at startup when a previous session left unsaved work behind.
fn recovery(app: &mut RoughcutApp, ctx: &egui::Context) {
    let Some(snapshot) = &app.recovery else {
        return;
    };
    let age = describe_age(roughcut_core::project_io::autosave_age_secs(snapshot));
    let clips = snapshot.project.clips.len();
    let cuts = snapshot.project.timeline.len();
    let frames = roughcut_core::timeline::total_frames(&snapshot.project.timeline);
    let fps = snapshot.project.fps();
    let origin = snapshot
        .project_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "never saved to a file".to_string());

    let mut accept = false;
    let mut decline = false;

    egui::Modal::new(egui::Id::new("recovery")).show(ctx, |ui| {
        ui.set_max_width(560.0);
        ui.label(
            egui::RichText::new("Unsaved work from a previous session")
                .strong()
                .color(theme::WARN),
        );
        ui.add_space(8.0);
        ui.label(format!(
            "Roughcut closed {age} with changes that were never saved."
        ));
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!(
                "{clips} clip{} in the bin · {cuts} cut{} on the timeline · {}",
                if clips == 1 { "" } else { "s" },
                if cuts == 1 { "" } else { "s" },
                roughcut_core::time::format_timecode(frames, fps),
            ))
            .monospace()
            .color(theme::TEXT_DIM),
        );
        ui.label(
            egui::RichText::new(origin)
                .monospace()
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Recover  ⏎").clicked() {
                accept = true;
            }
            if ui.button("Discard  Esc").clicked() {
                decline = true;
            }
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Recovering does not overwrite anything — the work comes back \
                 unsaved, exactly as it was.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
    });

    // This is the first thing a keyboard-driven tool shows you, so it answers
    // to the keyboard. Normal key dispatch is paused while a modal is up, so
    // these are read directly.
    let (enter, escape) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });

    if accept || enter {
        app.accept_recovery();
    } else if decline || escape {
        app.decline_recovery();
    }
}

fn describe_age(secs: u64) -> String {
    match secs {
        0..=90 => "moments ago".to_string(),
        s if s < 3600 => format!("{} minutes ago", s / 60),
        s if s < 7200 => "an hour ago".to_string(),
        s if s < 86_400 => format!("{} hours ago", s / 3600),
        s if s < 172_800 => "yesterday".to_string(),
        s => format!("{} days ago", s / 86_400),
    }
}

/// §4: a blocking dialog on first import when ffprobe is missing, pointing at
/// Shotcut's bundled copy as the easiest fix.
fn missing_tool(app: &mut RoughcutApp, ctx: &egui::Context) {
    if !app.show_missing_tool {
        return;
    }
    egui::Modal::new(egui::Id::new("missing-ffprobe")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        ui.label(
            egui::RichText::new("ffprobe is required to import media")
                .strong()
                .color(theme::WARN),
        );
        ui.add_space(8.0);
        ui.label(
            "Roughcut reads frame rates and durations with ffprobe, and cannot import \
             anything without it. It was not found on PATH.",
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "If you have Shotcut installed, it ships a copy — point Roughcut at \
                 the ffprobe inside the Shotcut program folder.",
            )
            .color(theme::TEXT_DIM),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Locate ffprobe…").clicked() {
                app.locate_tool_dialog();
            }
            if ui.button("Continue without importing").clicked() {
                app.show_missing_tool = false;
            }
        });
    });
}

/// §11: a relink dialog on load when a stored absolute path no longer exists.
fn relink(app: &mut RoughcutApp, ctx: &egui::Context) {
    if app.missing_media.is_empty() {
        return;
    }
    let entries: Vec<(ClipId, PathBuf)> = app.missing_media.clone();
    let mut dismiss = false;
    let mut relink_id: Option<ClipId> = None;

    egui::Modal::new(egui::Id::new("relink")).show(ctx, |ui| {
        ui.set_max_width(640.0);
        ui.label(
            egui::RichText::new(format!("{} file(s) could not be found", entries.len()))
                .strong()
                .color(theme::WARN),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Projects store absolute paths. Relink each clip, or continue and \
                 relink later by double-clicking a red entry in the bin.",
            )
            .color(theme::TEXT_DIM),
        );
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for (id, path) in &entries {
                    ui.horizontal(|ui| {
                        if ui.button("Locate…").clicked() {
                            relink_id = Some(*id);
                        }
                        ui.label(
                            egui::RichText::new(path.display().to_string())
                                .monospace()
                                .small()
                                .color(theme::ERROR),
                        );
                    });
                }
            });

        ui.add_space(12.0);
        if ui.button("Continue").clicked() {
            dismiss = true;
        }
    });

    if let Some(id) = relink_id {
        app.relink_dialog(id);
    }
    if dismiss {
        let n = app.missing_media.len();
        app.missing_media.clear();
        app.set_status(
            format!("{n} clip(s) still need relinking before export"),
            StatusKind::Warn,
        );
    }
}
