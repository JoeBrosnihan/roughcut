//! The `?` overlay. Rendered from the same `KEY_MAP` that generates `KEYS.md`.

use crate::actions::{Scope, KEY_MAP};
use crate::app::RoughcutApp;
use crate::theme;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    if !app.show_help {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard map")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 520.0])
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::LINE))
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for section in KEY_MAP {
                        ui.label(
                            egui::RichText::new(section.title)
                                .strong()
                                .color(theme::ACCENT),
                        );
                        ui.add_space(2.0);
                        egui::Grid::new(section.title)
                            .num_columns(3)
                            .spacing([14.0, 3.0])
                            .striped(false)
                            .show(ui, |ui| {
                                for b in section.bindings {
                                    ui.label(
                                        egui::RichText::new(b.keys)
                                            .monospace()
                                            .color(theme::TEXT),
                                    );
                                    ui.label(
                                        egui::RichText::new(b.description)
                                            .color(theme::TEXT_DIM),
                                    );
                                    ui.label(
                                        egui::RichText::new(match b.scope {
                                            Scope::Global => "",
                                            Scope::Source => "source",
                                            Scope::Timeline => "timeline",
                                        })
                                        .small()
                                        .color(theme::LINE),
                                    );
                                    ui.end_row();
                                }
                            });
                        ui.add_space(10.0);
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "Roughcut assembles and trims. Everything else — effects, \
                             transitions, audio, titles — belongs in Shotcut, which is \
                             what the exported .mlt is for.",
                        )
                        .small()
                        .color(theme::TEXT_DIM),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("This build")
                            .strong()
                            .color(theme::ACCENT),
                    );
                    // Which copy am I running, and whose settings is it using?
                    // Worth being able to answer at a glance once there is an
                    // installed build and a development one.
                    let exe = std::env::current_exe()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "?".into());
                    let config = crate::settings::config_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "?".into());
                    // What the background workers hold right now — the first
                    // place to look when the machine feels slow.
                    let working = crate::gauge::live_line()
                        .unwrap_or_else(|| "nothing running".to_string());
                    for line in [
                        format!("version: {}", env!("CARGO_PKG_VERSION")),
                        format!("binary:  {exe}"),
                        format!("config:  {config}"),
                        format!("work:    {working}"),
                    ] {
                        ui.label(
                            egui::RichText::new(line)
                                .small()
                                .monospace()
                                .color(theme::TEXT_DIM),
                        );
                    }

                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("External tools")
                            .strong()
                            .color(theme::ACCENT),
                    );
                    for (name, path) in [
                        ("ffprobe", app.tools.ffprobe.clone()),
                        ("ffmpeg", app.tools.ffmpeg.clone()),
                        ("melt", app.tools.melt.clone()),
                        (
                            "libmpv",
                            app.monitor.library_path().map(|p| p.to_path_buf()),
                        ),
                    ] {
                        let (text, color) = match path {
                            Some(p) => (p.display().to_string(), theme::TEXT_DIM),
                            None if name == "libmpv" && !app.monitor.is_available() => {
                                ("not loaded yet".to_string(), theme::TEXT_DIM)
                            }
                            None => ("not found".to_string(), theme::WARN),
                        };
                        ui.label(
                            egui::RichText::new(format!("{name}: {text}"))
                                .small()
                                .monospace()
                                .color(color),
                        );
                    }
                });
        });
    if !open {
        app.show_help = false;
    }
}
