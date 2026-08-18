//! The two modal dialogs: the missing-`ffprobe` block on first import, and
//! the relink list shown when a project's media has moved.

use crate::app::{ExportFormat, RoughcutApp, StatusKind};
use crate::theme;
use roughcut_core::model::ClipId;
use roughcut_core::Rational;
use std::path::PathBuf;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    // Recovery comes first: it decides which project the others apply to.
    recovery(app, ctx);
    missing_tool(app, ctx);
    relink(app, ctx);
    export(app, ctx);
    rendering(app, ctx);
}

/// Common frame heights, offered at whatever shape the export already is.
const HEIGHTS: [u32; 5] = [2160, 1440, 1080, 720, 480];
/// Rates worth offering beyond whatever the footage suggested.
const RATES: [(i64, i64); 6] = [(24, 1), (25, 1), (30000, 1001), (30, 1), (50, 1), (60, 1)];

/// What to export, and at what size and rate.
///
/// The suggestion from the clips actually used is already filled in; this
/// exists so it can be overridden, not so it has to be confirmed. Everything
/// here is two clicks from done.
fn export(app: &mut RoughcutApp, ctx: &egui::Context) {
    let Some(mut plan) = app.export_plan.clone() else {
        return;
    };
    let mut go = false;
    let mut cancel = false;
    let has_melt = app.tools.melt.is_some();

    egui::Modal::new(egui::Id::new("export")).show(ctx, |ui| {
        ui.set_max_width(430.0);
        ui.label(egui::RichText::new("Export").strong());
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Format");
            ui.add_space(18.0);
            ui.selectable_value(&mut plan.format, ExportFormat::Mlt, "Shotcut project");
            ui.add_enabled_ui(has_melt, |ui| {
                ui.selectable_value(&mut plan.format, ExportFormat::Mp4, "MP4 video")
                    .on_disabled_hover_text(
                        "Encoding needs melt, which comes with Shotcut, and it was not found",
                    );
            });
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(match plan.format {
                ExportFormat::Mlt => {
                    "A project file pointing at your original media. Opens in Shotcut                      with every cut where you put it. Instant."
                }
                ExportFormat::Mp4 => {
                    "A finished video, encoded from your originals. Takes minutes, and                      the size and rate below are what it will be."
                }
            })
            .small()
            .color(theme::TEXT_DIM),
        );

        ui.add_space(12.0);
        let aspect = plan.width as f32 / plan.height.max(1) as f32;
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add_space(35.0);
            egui::ComboBox::from_id_salt("export-size")
                .selected_text(format!("{} x {}", plan.width, plan.height))
                .show_ui(ui, |ui| {
                    let s = &plan.suggested;
                    let mut pick = |ui: &mut egui::Ui, w: u32, h: u32, note: &str| {
                        let label = if note.is_empty() {
                            format!("{w} x {h}")
                        } else {
                            format!("{w} x {h}   {note}")
                        };
                        if ui
                            .selectable_label(plan.width == w && plan.height == h, label)
                            .clicked()
                        {
                            plan.width = w;
                            plan.height = h;
                        }
                    };
                    pick(ui, s.width, s.height, "(from your clips)");
                    ui.separator();
                    for h in HEIGHTS {
                        // Keep the shape; only the height is being chosen.
                        let w = even((h as f32 * aspect).round() as u32);
                        if (w, h) != (s.width, s.height) {
                            pick(ui, w, h, "");
                        }
                    }
                });
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Frame rate");
            egui::ComboBox::from_id_salt("export-rate")
                .selected_text(format!("{}", plan.fps))
                .show_ui(ui, |ui| {
                    let suggested = plan.suggested.fps();
                    if ui
                        .selectable_label(plan.fps == suggested, format!("{suggested}   (from your clips)"))
                        .clicked()
                    {
                        plan.fps = suggested;
                    }
                    ui.separator();
                    for (num, den) in RATES {
                        let r = Rational::new(num, den);
                        if r == suggested {
                            continue;
                        }
                        if ui.selectable_label(plan.fps == r, format!("{r}")).clicked() {
                            plan.fps = r;
                        }
                    }
                });
        });

        if plan.fps != plan.suggested.fps() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Changing the rate rescales every cut to keep it at the same moment.                      Frame-exactness is not guaranteed across the conversion.",
                )
                .small()
                .color(theme::WARN),
            );
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if ui
                .button(match plan.format {
                    ExportFormat::Mlt => "Export…",
                    ExportFormat::Mp4 => "Render…",
                })
                .clicked()
            {
                go = true;
            }
        });
    });

    if cancel {
        app.export_plan = None;
    } else if go {
        app.run_export(plan);
    } else {
        app.export_plan = Some(plan);
    }
}

fn even(n: u32) -> u32 {
    let n = n.max(2);
    n + (n & 1)
}

/// While melt encodes. Modal on purpose: the cut must not change underneath a
/// render that is reading it.
fn rendering(app: &mut RoughcutApp, ctx: &egui::Context) {
    let Some(state) = &app.render else {
        return;
    };
    let (frame, total, out) = (state.frame, state.total.max(1), state.out.clone());
    let done = (frame as f32 / total as f32).clamp(0.0, 1.0);
    let cancelling = state.cancel.load(std::sync::atomic::Ordering::Relaxed);
    let mut stop = false;

    egui::Modal::new(egui::Id::new("rendering")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.label(egui::RichText::new("Rendering").strong());
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(out.display().to_string())
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.add_space(8.0);
        ui.add(egui::ProgressBar::new(done).show_percentage().desired_width(420.0));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(if cancelling {
                "stopping…".to_string()
            } else {
                format!("frame {frame} of {total}")
            })
            .small()
            .color(theme::TEXT_DIM),
        );
        ui.add_space(12.0);
        ui.add_enabled_ui(!cancelling, |ui| {
            if ui.button("Cancel").clicked() {
                stop = true;
            }
        });
    });

    if stop {
        app.cancel_render();
    }
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
