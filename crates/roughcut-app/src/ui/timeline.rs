//! The timeline strip: clip blocks proportional to duration, plus a playhead.

use crate::app::{Focus, RoughcutApp};
use crate::theme;
use crate::ui::truncate_middle;
use egui::{CornerRadius, Rect, Sense, Stroke, StrokeKind};
use roughcut_core::time::format_timecode;
use roughcut_core::timeline as tl;

pub const TIMELINE_HEIGHT: f32 = 160.0;
const RULER_HEIGHT: f32 = 18.0;
const BLOCK_TOP: f32 = 26.0;
const BLOCK_HEIGHT: f32 = 92.0;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("timeline")
        .exact_height(TIMELINE_HEIGHT)
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let total = tl::total_frames(&app.project.timeline);

            // Fit-to-window is the default; explicit zoom pins it.
            let usable = (rect.width() - 20.0).max(50.0);
            if app.zoom_fit {
                app.zoom = if total > 0 {
                    (usable / total as f32).clamp(0.0005, 40.0)
                } else {
                    0.5
                };
            }
            let px_per_frame = app.zoom;

            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);

            if app.project.timeline.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Timeline empty — mark a range in the source and press A to append",
                    egui::FontId::proportional(12.0),
                    theme::TEXT_DIM,
                );
                crate::ui::focus_border(ui, rect, app.focus == Focus::Timeline);
                return;
            }

            // Horizontal scroll keeps the playhead in view when zoomed in.
            let content_width = (total as f32 * px_per_frame).max(usable);
            let scroll = egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (canvas, response) = ui.allocate_exact_size(
                        egui::vec2(content_width, TIMELINE_HEIGHT - 4.0),
                        Sense::click(),
                    );
                    draw(app, ui, canvas, px_per_frame);
                    handle_click(app, &response, canvas, px_per_frame);
                });
            let _ = scroll;

            crate::ui::focus_border(ui, rect, app.focus == Focus::Timeline);
        });
}

fn draw(app: &RoughcutApp, ui: &egui::Ui, canvas: Rect, px_per_frame: f32) {
    let painter = ui.painter_at(canvas);
    let fps = app.fps();
    let x_of = |frame: i64| canvas.left() + frame as f32 * px_per_frame;

    // --- ruler --------------------------------------------------------------
    let ruler = Rect::from_min_size(canvas.min, egui::vec2(canvas.width(), RULER_HEIGHT));
    painter.rect_filled(ruler, CornerRadius::ZERO, theme::PANEL_ALT);

    let total = tl::total_frames(&app.project.timeline);
    let step = tick_step(px_per_frame, fps.nominal_fps());
    if step > 0 {
        let mut f = 0i64;
        while f <= total {
            let x = x_of(f);
            if x > canvas.right() {
                break;
            }
            painter.line_segment(
                [
                    egui::pos2(x, ruler.bottom() - 5.0),
                    egui::pos2(x, ruler.bottom()),
                ],
                Stroke::new(1.0, theme::LINE),
            );
            painter.text(
                egui::pos2(x + 3.0, ruler.top() + 2.0),
                egui::Align2::LEFT_TOP,
                format_timecode(f, fps),
                egui::FontId::monospace(9.0),
                theme::TEXT_DIM,
            );
            f += step;
        }
    }

    // --- clip blocks --------------------------------------------------------
    let mut start = 0i64;
    for (i, item) in app.project.timeline.iter().enumerate() {
        let len = item.len();
        let block = Rect::from_min_max(
            egui::pos2(x_of(start), canvas.top() + BLOCK_TOP),
            egui::pos2(x_of(start + len), canvas.top() + BLOCK_TOP + BLOCK_HEIGHT),
        );
        start += len;

        if block.right() < canvas.left() || block.left() > canvas.right() {
            continue;
        }

        let selected = app.selected_item == Some(i);
        painter.rect_filled(
            block,
            CornerRadius::ZERO,
            if selected {
                theme::CLIP_SELECTED
            } else {
                theme::CLIP
            },
        );
        painter.rect_stroke(
            block,
            CornerRadius::ZERO,
            Stroke::new(
                if selected { 2.0 } else { 1.0 },
                if selected { theme::ACCENT } else { theme::LINE },
            ),
            StrokeKind::Inside,
        );

        // Only label a block that is wide enough to read.
        if block.width() > 44.0 {
            let clip = app.project.clip(item.clip_id);
            let name = clip.map(|c| c.file_name()).unwrap_or_else(|| "?".into());
            let max_chars = ((block.width() - 10.0) / 6.2).max(3.0) as usize;
            painter.text(
                block.left_top() + egui::vec2(5.0, 4.0),
                egui::Align2::LEFT_TOP,
                truncate_middle(&name, max_chars),
                egui::FontId::proportional(11.0),
                theme::TEXT,
            );
            if block.width() > 70.0 {
                painter.text(
                    block.left_bottom() + egui::vec2(5.0, -4.0),
                    egui::Align2::LEFT_BOTTOM,
                    format_timecode(len, fps),
                    egui::FontId::monospace(10.0),
                    theme::TEXT_DIM,
                );
            }
            if clip.is_some_and(|c| c.rate_mismatch) {
                painter.text(
                    block.right_top() + egui::vec2(-5.0, 4.0),
                    egui::Align2::RIGHT_TOP,
                    "FPS",
                    egui::FontId::proportional(9.0),
                    theme::WARN,
                );
            }
        }
    }

    // --- playhead -----------------------------------------------------------
    let px = x_of(app.playhead);
    painter.line_segment(
        [
            egui::pos2(px, canvas.top() + RULER_HEIGHT - 4.0),
            egui::pos2(px, canvas.top() + BLOCK_TOP + BLOCK_HEIGHT + 6.0),
        ],
        Stroke::new(2.0, theme::PLAYHEAD),
    );
    painter.text(
        egui::pos2(px + 4.0, canvas.top() + BLOCK_TOP + BLOCK_HEIGHT + 8.0),
        egui::Align2::LEFT_TOP,
        format_timecode(app.playhead, fps),
        egui::FontId::monospace(10.0),
        theme::PLAYHEAD,
    );
}

fn handle_click(app: &mut RoughcutApp, response: &egui::Response, canvas: Rect, ppf: f32) {
    if !response.clicked() {
        return;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return;
    };
    let frame = ((pos.x - canvas.left()) / ppf.max(1e-6)).round() as i64;
    app.monitor.pause();
    app.focus = Focus::Timeline;
    // Set focus first: `set_position` clamps against whichever region has it.
    app.set_position(frame);
    app.selected_item = tl::item_at(&app.project.timeline, app.playhead).map(|(i, _)| i);
}

/// Pick a ruler tick interval that leaves at least ~90 px between labels.
fn tick_step(px_per_frame: f32, nominal_fps: i64) -> i64 {
    if px_per_frame <= 0.0 {
        return 0;
    }
    let want_frames = (90.0 / px_per_frame).ceil() as i64;
    // Snap to a sensible musical-ish ladder of durations.
    let candidates = [
        1,
        nominal_fps,
        nominal_fps * 2,
        nominal_fps * 5,
        nominal_fps * 10,
        nominal_fps * 30,
        nominal_fps * 60,
        nominal_fps * 300,
        nominal_fps * 600,
        nominal_fps * 1800,
        nominal_fps * 3600,
    ];
    candidates
        .into_iter()
        .find(|&c| c >= want_frames)
        .unwrap_or(nominal_fps * 3600)
}

#[cfg(test)]
mod tests {
    use super::tick_step;

    #[test]
    fn ticks_get_coarser_as_you_zoom_out() {
        let fine = tick_step(10.0, 30);
        let coarse = tick_step(0.05, 30);
        assert!(fine < coarse, "{fine} should be finer than {coarse}");
        assert!(fine >= 1);
    }

    #[test]
    fn zero_zoom_is_handled() {
        assert_eq!(tick_step(0.0, 30), 0);
    }
}
