//! The bin: a scrollable list of imported clips.

use crate::app::{ProxyState, RoughcutApp};
use crate::theme;
use crate::ui::truncate_middle;
use egui::{CornerRadius, Sense, StrokeKind};
use roughcut_core::model::ClipId;
use roughcut_core::time::format_timecode;

pub const BIN_WIDTH: f32 = 240.0;
const ROW_HEIGHT: f32 = 62.0;
const THUMB_W: f32 = 88.0;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    egui::SidePanel::left("bin")
        .exact_width(BIN_WIDTH)
        .resizable(false)
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
                    .show(ui, |ui| {
                        for id in ids {
                            row(app, ui, id);
                        }
                    });
            });
        });
}

fn header(app: &RoughcutApp, ui: &mut egui::Ui) {
    let rect = ui
        .allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::hover())
        .0;
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::PANEL_ALT);
    ui.painter().text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("BIN ({})", app.project.clips.len()),
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
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

fn row(app: &mut RoughcutApp, ui: &mut egui::Ui, id: ClipId) {
    let Some(clip) = app.project.clip(id) else {
        return;
    };
    let selected = app.selected_clip == Some(id);
    let name = clip.file_name();
    let duration = format_timecode(clip.duration_frames, app.project.fps());
    let rate_mismatch = clip.rate_mismatch;
    let missing = !clip.path.exists();
    let has_proxy = clip.proxy_path.as_ref().is_some_and(|p| p.exists());
    let marked = clip.mark_in.is_some() || clip.mark_out.is_some();
    let mark_text = match (clip.mark_in, clip.mark_out) {
        (None, None) => String::new(),
        (i, o) => format!(
            "[{} – {}]",
            i.map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
            o.map(|v| v.to_string())
                .unwrap_or_else(|| clip.last_frame().to_string())
        ),
    };
    let proxy_state = app.proxy_state.get(&id).copied();

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }

    let bg = if selected {
        theme::CLIP_SELECTED
    } else if response.hovered() {
        theme::PANEL_ALT
    } else {
        theme::PANEL
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, theme::LINE),
    );

    // Thumbnail
    let thumb_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(6.0, 6.0),
        egui::vec2(THUMB_W, ROW_HEIGHT - 12.0),
    );
    painter.rect_filled(thumb_rect, CornerRadius::ZERO, theme::VIDEO_LETTERBOX);
    if let Some(tex) = app.thumbnails.get(&id) {
        let size = tex.size_vec2();
        let scale = (thumb_rect.width() / size.x).min(thumb_rect.height() / size.y);
        let draw = egui::Rect::from_center_size(thumb_rect.center(), size * scale);
        painter.image(
            tex.id(),
            draw,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    // Text block
    let text_x = thumb_rect.right() + 8.0;
    let max_chars = 20;
    painter.text(
        egui::pos2(text_x, rect.top() + 10.0),
        egui::Align2::LEFT_TOP,
        truncate_middle(&name, max_chars),
        egui::FontId::proportional(12.0),
        if missing { theme::ERROR } else { theme::TEXT },
    );
    painter.text(
        egui::pos2(text_x, rect.top() + 27.0),
        egui::Align2::LEFT_TOP,
        duration,
        egui::FontId::monospace(11.0),
        theme::TEXT_DIM,
    );
    if marked {
        painter.text(
            egui::pos2(text_x, rect.top() + 42.0),
            egui::Align2::LEFT_TOP,
            mark_text,
            egui::FontId::monospace(10.0),
            theme::MARK_IN,
        );
    }

    // Badges, right-aligned along the bottom edge.
    let mut badge_x = rect.right() - 6.0;
    let badge = |painter: &egui::Painter, x: &mut f32, text: &str, color: egui::Color32| {
        let width = text.len() as f32 * 6.0 + 8.0;
        let r = egui::Rect::from_min_size(
            egui::pos2(*x - width, rect.bottom() - 18.0),
            egui::vec2(width, 13.0),
        );
        painter.rect_filled(r, CornerRadius::ZERO, color.linear_multiply(0.25));
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
        *x -= width + 4.0;
    };

    if missing {
        badge(painter, &mut badge_x, "MISSING", theme::ERROR);
    }
    if rate_mismatch {
        // §5 rule 5: a persistent warning that frame-exactness is not
        // guaranteed for this clip.
        badge(painter, &mut badge_x, "FPS", theme::WARN);
    }
    match proxy_state {
        Some(ProxyState::Queued) => badge(painter, &mut badge_x, "PXY…", theme::TEXT_DIM),
        Some(ProxyState::Running) => badge(painter, &mut badge_x, "PXY▶", theme::ACCENT),
        Some(ProxyState::Failed) => badge(painter, &mut badge_x, "PXY!", theme::ERROR),
        None if has_proxy => badge(painter, &mut badge_x, "PXY", theme::MARK_IN),
        None => {}
    }

    if response.clicked() {
        app.select_bin_clip(id);
    }
    if missing && response.double_clicked() {
        app.relink_dialog(id);
    }
    let response = response.on_hover_text(format!(
        "{}\n{}\n{} frames{}",
        name,
        app.project
            .clip(id)
            .map(|c| c.path.display().to_string())
            .unwrap_or_default(),
        app.project.clip(id).map(|c| c.duration_frames).unwrap_or(0),
        if rate_mismatch {
            "\nframe rate differs from the project — MLT will resample"
        } else {
            ""
        }
    ));
    let _ = response;
}
