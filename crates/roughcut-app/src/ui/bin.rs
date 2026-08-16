//! The bin: a scrollable grid of clip thumbnails.
//!
//! A grid rather than a list because the picture is what identifies a clip;
//! the filename rarely does. Hovering a tile scrubs it, which makes the grid a
//! contact sheet you can skim rather than a table you have to read.

use crate::app::{Focus, ProxyState, RoughcutApp};
use crate::theme;
use crate::ui::truncate_middle;
use crate::workers;
use egui::{CornerRadius, Rect, Sense, StrokeKind};
use roughcut_core::model::ClipId;
use roughcut_core::time::format_timecode;

pub const BIN_WIDTH: f32 = 250.0;
const HEADER_H: f32 = 22.0;
const GAP: f32 = 6.0;
/// Tile width. Two columns at the default panel width; the panel is resizable,
/// so widening it simply fits more.
const TILE_W: f32 = 112.0;
const THUMB_H: f32 = TILE_W * 9.0 / 16.0;
const LABEL_H: f32 = 26.0;
const TILE_H: f32 = THUMB_H + LABEL_H;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    egui::SidePanel::left("bin")
        .default_width(BIN_WIDTH)
        .min_width(140.0)
        .resizable(true)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                header(app, ui);
                ui.add_space(2.0);

                if app.project.clips.is_empty() {
                    empty_state(app, ui);
                    return;
                }

                let ids: Vec<ClipId> = app.project.clips.iter().map(|c| c.id).collect();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| grid(app, ui, &ids));
            });
        });
}

fn grid(app: &mut RoughcutApp, ui: &mut egui::Ui, ids: &[ClipId]) {
    let avail = ui.available_width().max(TILE_W);
    let cols = (((avail - GAP) / (TILE_W + GAP)).floor() as usize).max(1);
    let rows = ids.len().div_ceil(cols);

    let (area, _) = ui.allocate_exact_size(
        egui::vec2(avail, rows as f32 * (TILE_H + GAP) + GAP),
        Sense::hover(),
    );

    for (i, id) in ids.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let rect = Rect::from_min_size(
            area.min
                + egui::vec2(
                    GAP + col as f32 * (TILE_W + GAP),
                    GAP + row as f32 * (TILE_H + GAP),
                ),
            egui::vec2(TILE_W, TILE_H),
        );
        if ui.is_rect_visible(rect) {
            tile(app, ui, *id, rect);
        }
    }
}

/// The bin's header doubles as the application's only menu.
///
/// A dedicated menu bar would be a permanent row of chrome for something used
/// a few times a session, and every pixel of chrome is a pixel not showing
/// video. This row already existed, so the dropdown costs nothing.
fn header(app: &mut RoughcutApp, ui: &mut egui::Ui) {
    use crate::actions::Action;

    let rect = ui.max_rect();
    let rect = Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::PANEL_ALT);

    let mut action: Option<Action> = None;
    let mut recover = false;

    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.add_space(4.0);
            ui.menu_button("File", |ui| {
                ui.set_min_width(190.0);
                if menu_item(ui, "Import media…", "Ctrl+I") {
                    action = Some(Action::Import);
                }
                ui.separator();
                if menu_item(ui, "Open project…", "Ctrl+O") {
                    action = Some(Action::OpenProject);
                }
                if menu_item(ui, "Save", "Ctrl+S") {
                    action = Some(Action::SaveProject);
                }
                if menu_item(ui, "Save as…", "Ctrl+Shift+S") {
                    action = Some(Action::SaveProjectAs);
                }
                ui.separator();
                if menu_item(ui, "Export MLT XML…", "Ctrl+E") {
                    action = Some(Action::ExportMlt);
                }
                ui.separator();
                // Always reachable, so a dismissed startup prompt is never the
                // last word on a session's work.
                ui.add_enabled_ui(app.has_recoverable_session(), |ui| {
                    if menu_item(ui, "Recover last session…", "") {
                        recover = true;
                    }
                });
                ui.separator();
                if menu_item(ui, "Keyboard map", "?") {
                    action = Some(Action::ToggleHelp);
                }
            });

            ui.label(
                egui::RichText::new(format!("BIN ({})", app.project.clips.len()))
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );
        });
    });

    if recover {
        app.recover_last_session();
    }
    if let Some(a) = action {
        app.dispatch(a);
    }
}

/// A menu row with its keyboard equivalent shown on the right, so the menu
/// teaches the shortcuts rather than replacing them.
fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
    let clicked = ui
        .horizontal(|ui| {
            let clicked = ui.selectable_label(false, label).clicked();
            if !shortcut.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(shortcut)
                            .monospace()
                            .small()
                            .color(theme::TEXT_DIM),
                    );
                });
            }
            clicked
        })
        .inner;
    if clicked {
        ui.close_kind(egui::UiKind::Menu);
    }
    clicked
}

fn empty_state(app: &mut RoughcutApp, ui: &mut egui::Ui) {
    ui.add_space(16.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("No clips").color(theme::TEXT_DIM));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Ctrl+I to import,\nor drop files here")
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.add_space(10.0);
        if ui.button("Import…").clicked() {
            app.dispatch(crate::actions::Action::Import);
        }
    });
}

fn tile(app: &mut RoughcutApp, ui: &mut egui::Ui, id: ClipId, rect: Rect) {
    let Some(clip) = app.project.clip(id) else {
        return;
    };
    let selected = app.selected_clip == Some(id);
    let name = clip.file_name();
    let duration_frames = clip.duration_frames;
    let duration = format_timecode(duration_frames, app.project.fps());
    let rate_mismatch = clip.rate_mismatch;
    let missing = !clip.path.exists();
    let has_proxy = clip.proxy_path.as_ref().is_some_and(|p| p.exists());
    let marked = clip.mark_in.is_some() || clip.mark_out.is_some();
    let path = clip.path.display().to_string();
    let proxy_state = app.proxy_state.get(&id).copied();
    let uses = app.project.timeline_uses(id);

    let response = ui.interact(
        rect,
        egui::Id::new(("bin-tile", id)),
        Sense::click_and_drag(),
    );
    let thumb = Rect::from_min_size(rect.min, egui::vec2(rect.width(), THUMB_H));
    let painter = ui.painter_at(rect);

    if selected || response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            if selected {
                theme::CLIP_SELECTED
            } else {
                theme::PANEL_ALT
            },
        );
    }
    painter.rect_filled(thumb, CornerRadius::ZERO, theme::VIDEO_LETTERBOX);

    // Hover-scrub: the pointer's horizontal position within the thumbnail
    // picks which of the filmstrip's tiles to show. The tiles were baked into
    // one texture when the clip was imported, so this costs a UV offset and
    // nothing else — no decode, and no work at all once the pointer stops.
    let hover_x = response
        .hover_pos()
        .filter(|p| thumb.contains(*p))
        .map(|p| ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0));
    let tiles = workers::FILMSTRIP_FRAMES;
    let frame_tile = match hover_x {
        Some(t) => ((t * tiles as f32) as usize).min(tiles - 1),
        None => 0,
    };

    if let Some(tex) = app.thumbnails.get(&id) {
        let sheet = tex.size_vec2();
        let tile_size = egui::vec2(sheet.x / tiles as f32, sheet.y);
        let scale = (thumb.width() / tile_size.x).min(thumb.height() / tile_size.y);
        let draw = Rect::from_center_size(thumb.center(), tile_size * scale);
        let u0 = frame_tile as f32 / tiles as f32;
        let u1 = (frame_tile + 1) as f32 / tiles as f32;
        painter.image(
            tex.id(),
            draw,
            Rect::from_min_max(egui::pos2(u0, 0.0), egui::pos2(u1, 1.0)),
            egui::Color32::WHITE,
        );
    }

    // A scrubber line under the pointer, so it is obvious that the picture is
    // tracking the mouse rather than flickering.
    if let Some(t) = hover_x {
        let x = thumb.left() + t * thumb.width();
        painter.line_segment(
            [egui::pos2(x, thumb.top()), egui::pos2(x, thumb.bottom())],
            egui::Stroke::new(1.0, theme::PLAYHEAD),
        );
    }

    // Duration on the picture, bottom right, the way every browser does it.
    painter.text(
        thumb.right_bottom() + egui::vec2(-3.0, -2.0),
        egui::Align2::RIGHT_BOTTOM,
        duration,
        egui::FontId::monospace(10.0),
        theme::TEXT,
    );
    if marked {
        painter.text(
            thumb.left_bottom() + egui::vec2(3.0, -2.0),
            egui::Align2::LEFT_BOTTOM,
            "▮",
            egui::FontId::proportional(10.0),
            theme::MARK_IN,
        );
    }

    // Badges along the top of the picture.
    let mut badge_x = thumb.left() + 3.0;
    let mut badge = |text: &str, color: egui::Color32| {
        let w = text.chars().count() as f32 * 5.5 + 6.0;
        let r = Rect::from_min_size(egui::pos2(badge_x, thumb.top() + 3.0), egui::vec2(w, 12.0));
        painter.rect_filled(r, CornerRadius::ZERO, egui::Color32::from_black_alpha(180));
        painter.rect_stroke(
            r,
            CornerRadius::ZERO,
            egui::Stroke::new(1.0, color),
            StrokeKind::Inside,
        );
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(9.0),
            color,
        );
        badge_x += w + 3.0;
    };
    if missing {
        badge("MISSING", theme::ERROR);
    }
    if rate_mismatch {
        // §5 rule 5: frame-exactness is not guaranteed for this clip.
        badge("FPS", theme::WARN);
    }
    match proxy_state {
        Some(ProxyState::Queued) => badge("PXY…", theme::TEXT_DIM),
        Some(ProxyState::Running) => badge("PXY▶", theme::ACCENT),
        Some(ProxyState::Failed) => badge("PXY!", theme::ERROR),
        None if has_proxy => badge("PXY", theme::MARK_IN),
        None => {}
    }

    // Name below the picture.
    let max_chars = ((rect.width() - 6.0) / 5.6).max(4.0) as usize;
    painter.text(
        egui::pos2(rect.left() + 3.0, thumb.bottom() + 4.0),
        egui::Align2::LEFT_TOP,
        truncate_middle(&name, max_chars),
        egui::FontId::proportional(11.0),
        if missing { theme::ERROR } else { theme::TEXT },
    );

    if selected {
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            egui::Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
    }

    // --- interaction --------------------------------------------------------

    if response.drag_started() {
        app.select_bin_clip(id);
        egui::DragAndDrop::set_payload(ui.ctx(), id);
    }
    if response.dragged() {
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            egui::Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
    }
    if response.clicked() {
        app.select_bin_clip(id);
        // Clicking the picture opens the clip *at the frame under the
        // pointer*, so skimming to a moment and landing on it is one gesture.
        if let Some(t) = response
            .interact_pointer_pos()
            .filter(|p| thumb.contains(*p))
            .map(|p| ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0))
        {
            app.focus = Focus::Source;
            let last = (duration_frames - 1).max(0);
            app.set_position((t * last as f32).round() as i64);
        }
    }
    if response.double_clicked() {
        if missing {
            // A clip that cannot be found has nothing to append yet.
            app.relink_dialog(id);
        } else {
            app.select_bin_clip(id);
            app.append_clip(id);
        }
    }

    let mut remove = false;
    let mut relink = false;
    response.context_menu(|ui| {
        ui.set_min_width(170.0);
        if missing && ui.button("Relink…").clicked() {
            relink = true;
            ui.close_kind(egui::UiKind::Menu);
        }
        ui.add_enabled_ui(uses == 0, |ui| {
            if ui.button("Remove from bin").clicked() {
                remove = true;
                ui.close_kind(egui::UiKind::Menu);
            }
        });
        if uses > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "used by {uses} cut{} on the timeline",
                    if uses == 1 { "" } else { "s" }
                ))
                .small()
                .color(theme::TEXT_DIM),
            );
        }
    });
    if relink {
        app.relink_dialog(id);
    }
    if remove {
        app.select_bin_clip(id);
        app.remove_selected_clip();
    }

    response.on_hover_text(format!(
        "{name}\n{path}\n{duration_frames} frames{}",
        if rate_mismatch {
            "\nframe rate differs from the project — MLT will resample"
        } else {
            ""
        }
    ));
}
