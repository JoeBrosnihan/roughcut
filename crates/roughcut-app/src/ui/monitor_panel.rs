//! The source monitor: the video frame, and the scrub bar beneath it.
//!
//! The monitor shows the selected bin clip, or — when the timeline has focus —
//! the timeline at the playhead.

use crate::app::{Focus, RoughcutApp};
use crate::theme;
use crate::video::PixelRect;
use egui::{CornerRadius, Pos2, Rect, Sense, Stroke};
use roughcut_core::time::format_timecode;
use roughcut_core::timeline;
use std::sync::Arc;

const SCRUB_HEIGHT: f32 = 44.0;

pub fn show(app: &mut RoughcutApp, ctx: &egui::Context) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ctx, |ui| {
            let full = ui.max_rect();
            if full.height() < SCRUB_HEIGHT + 20.0 {
                return;
            }
            let split = full.max.y - SCRUB_HEIGHT;
            let video_rect = Rect::from_min_max(full.min, egui::pos2(full.max.x, split));
            let scrub_rect = Rect::from_min_max(egui::pos2(full.min.x, split), full.max);

            video(app, ui, video_rect);
            scrub_bar(app, ui, scrub_rect);
            crate::ui::focus_border(ui, full, app.focus == Focus::Source);
        });
}

fn video(app: &mut RoughcutApp, ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::VIDEO_LETTERBOX);

    let ready = app
        .video
        .lock()
        .map(|v| v.is_ready())
        .unwrap_or(false);

    if ready && app.current_media().is_some() {
        let video = app.video.clone();
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let vp = info.viewport_in_pixels();
                if let Ok(mut v) = video.lock() {
                    v.paint(
                        painter.gl(),
                        PixelRect {
                            left: vp.left_px,
                            bottom: vp.from_bottom_px,
                            width: vp.width_px,
                            height: vp.height_px,
                        },
                    );
                }
            })),
        });
    } else {
        let message = placeholder_text(app);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(13.0),
            theme::TEXT_DIM,
        );
    }

    // A thin caption strip so it is always obvious what the monitor is showing.
    let label = match app.focus {
        Focus::Source => app
            .selected_source()
            .map(|c| c.file_name())
            .unwrap_or_else(|| "—".into()),
        Focus::Timeline => timeline::item_at(&app.project.timeline, app.playhead)
            .and_then(|(i, _)| app.project.timeline.get(i))
            .and_then(|item| app.project.clip(item.clip_id))
            .map(|c| format!("TIMELINE · {}", c.file_name()))
            .unwrap_or_else(|| "TIMELINE".into()),
    };
    painter.text(
        rect.left_top() + egui::vec2(8.0, 6.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );

    // Shuttle feedback, so a J or L press is visibly acknowledged. Absent
    // while paused, which is most of the time.
    use crate::monitor::Transport;
    let shuttle = match app.monitor.transport {
        Transport::Paused => None,
        Transport::Forward(s) => Some(format!("▶ {s}x")),
        Transport::Reverse(s) => Some(format!("◀ {s}x")),
    };
    if let Some(text) = shuttle {
        painter.text(
            rect.right_top() + egui::vec2(-8.0, 6.0),
            egui::Align2::RIGHT_TOP,
            text,
            egui::FontId::monospace(12.0),
            theme::PLAYHEAD,
        );
    }
}

fn placeholder_text(app: &RoughcutApp) -> String {
    if let Some(e) = &app.monitor.load_error {
        return format!("Video unavailable — {e}");
    }
    if let Some(e) = app.video.lock().ok().and_then(|v| v.init_error.clone()) {
        return format!("OpenGL render unavailable — {e}");
    }
    if app.project.clips.is_empty() {
        return "Import media with Ctrl+I, or drop files on the window".to_string();
    }
    match app.focus {
        Focus::Source => "Select a clip in the bin".to_string(),
        Focus::Timeline => "The timeline is empty".to_string(),
    }
}

/// Full-width bar spanning the current source's duration, with in/out markers
/// and a draggable playhead.
fn scrub_bar(app: &mut RoughcutApp, ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, theme::LINE),
    );

    // What span is the bar showing?
    let position = app.position();
    let (total, marks) = match app.focus {
        Focus::Source => match app.selected_source() {
            Some(c) => (
                c.duration_frames,
                Some((c.mark_in, c.mark_out, c.last_frame())),
            ),
            None => (0, None),
        },
        Focus::Timeline => (timeline::total_frames(&app.project.timeline), None),
    };
    if total <= 0 {
        return;
    }

    let track = Rect::from_min_max(
        egui::pos2(rect.left() + 10.0, rect.top() + 14.0),
        egui::pos2(rect.right() - 10.0, rect.top() + 26.0),
    );
    painter.rect_filled(track, CornerRadius::ZERO, theme::PANEL_ALT);

    let x_of = |frame: i64| -> f32 {
        let t = (frame as f32 / (total - 1).max(1) as f32).clamp(0.0, 1.0);
        track.left() + t * track.width()
    };

    // Marked range is shaded.
    if let Some((mark_in, mark_out, last)) = marks {
        let a = mark_in.unwrap_or(0);
        let b = mark_out.unwrap_or(last);
        if b >= a && (mark_in.is_some() || mark_out.is_some()) {
            let span = Rect::from_min_max(
                egui::pos2(x_of(a), track.top()),
                egui::pos2(x_of(b), track.bottom()),
            );
            painter.rect_filled(
                span,
                CornerRadius::ZERO,
                theme::ACCENT.linear_multiply(0.30),
            );
        }
        if let Some(i) = mark_in {
            marker(&painter, x_of(i), track, theme::MARK_IN, true);
        }
        if let Some(o) = mark_out {
            marker(&painter, x_of(o), track, theme::MARK_OUT, false);
        }
    }

    // Cut points, when scrubbing the timeline.
    if app.focus == Focus::Timeline {
        let mut acc = 0i64;
        for item in &app.project.timeline {
            acc += item.len();
            if acc >= total {
                break;
            }
            let x = x_of(acc);
            painter.line_segment(
                [
                    egui::pos2(x, track.top()),
                    egui::pos2(x, track.bottom()),
                ],
                Stroke::new(1.0, theme::LINE),
            );
        }
    }

    // Playhead
    let px = x_of(position);
    painter.line_segment(
        [
            egui::pos2(px, track.top() - 4.0),
            egui::pos2(px, track.bottom() + 4.0),
        ],
        Stroke::new(2.0, theme::PLAYHEAD),
    );

    // The one position readout in the application: where the playhead is, and
    // how long the thing under it runs for.
    let fps = app.fps();
    painter.text(
        egui::pos2(rect.left() + 10.0, rect.bottom() - 4.0),
        egui::Align2::LEFT_BOTTOM,
        format_timecode(position, fps),
        egui::FontId::monospace(11.0),
        theme::TEXT,
    );
    painter.text(
        egui::pos2(rect.right() - 10.0, rect.bottom() - 4.0),
        egui::Align2::RIGHT_BOTTOM,
        format_timecode(total, fps),
        egui::FontId::monospace(10.0),
        theme::TEXT_DIM,
    );

    // Dragging the bar scrubs. Mouse support is limited to this and to
    // selection, as §9 specifies.
    let hit = Rect::from_min_max(
        egui::pos2(track.left(), rect.top()),
        egui::pos2(track.right(), rect.bottom()),
    );
    let response = ui.interact(
        hit,
        ui.id().with("scrub"),
        Sense::click_and_drag(),
    );
    // Shift-drag marks a range instead of scrubbing. It sets the same in and
    // out points `I` and `O` do, so the mouse and the keyboard agree, and the
    // marks are committed once on release rather than on every pixel.
    let shift = ui.input(|i| i.modifiers.shift);
    let marking = app.focus == Focus::Source && app.selected_clip.is_some();

    if response.drag_started() && shift && marking {
        if let Some(pos) = response.interact_pointer_pos() {
            let f = frame_at(pos, track, total);
            app.mark_drag = Some((f, f));
        }
    }

    if app.mark_drag.is_some() {
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let f = frame_at(pos, track, total);
                if let Some((_, cur)) = app.mark_drag.as_mut() {
                    *cur = f;
                }
            }
        }
        // Preview the pending range; nothing is written to the project yet.
        if let Some((a, b)) = app.mark_drag {
            let span = Rect::from_min_max(
                egui::pos2(x_of(a.min(b)), track.top() - 3.0),
                egui::pos2(x_of(a.max(b)), track.bottom() + 3.0),
            );
            painter.rect_filled(span, CornerRadius::ZERO, theme::MARK_IN.linear_multiply(0.45));
        }
        if response.drag_stopped() {
            app.commit_mark_drag();
        }
    } else if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            app.monitor.pause();
            app.set_position(frame_at(pos, track, total));
        }
    }
}

fn frame_at(pos: Pos2, track: Rect, total: i64) -> i64 {
    let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    ((t * (total - 1).max(1) as f32).round() as i64).clamp(0, (total - 1).max(0))
}

fn marker(painter: &egui::Painter, x: f32, track: Rect, color: egui::Color32, is_in: bool) {
    let w = 5.0;
    let (a, b) = if is_in { (x, x + w) } else { (x - w, x) };
    painter.rect_filled(
        Rect::from_min_max(
            egui::pos2(a, track.top() - 5.0),
            egui::pos2(b, track.bottom() + 5.0),
        ),
        CornerRadius::ZERO,
        color,
    );
}
