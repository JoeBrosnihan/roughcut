//! The source monitor: the video frame, and the scrub bar beneath it.
//!
//! The monitor shows the selected bin clip, or — when the timeline has focus —
//! the timeline at the playhead.

use crate::app::{Focus, MarkEdge, RoughcutApp};
use crate::theme;
use crate::video::PixelRect;
use egui::{CornerRadius, Pos2, Rect, Sense, Stroke};
use roughcut_core::time::format_timecode;
use roughcut_core::timeline;
use std::sync::Arc;

/// Tall enough for the audio to be worth looking at. Every pixel here is a
/// pixel not showing video, so this is the smallest height at which a spike
/// in a waveform is legibly a spike.
const SCRUB_HEIGHT: f32 = 58.0;
const TRACK_TOP: f32 = 12.0;
const TRACK_HEIGHT: f32 = 28.0;

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

            // The document replaces the picture, never shares the space
            // with it: half a monitor of each is worse at both.
            if app.show_transcript {
                crate::ui::transcript_panel::show(app, ui, video_rect);
            } else {
                video(app, ui, video_rect);
            }
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

/// How close the pointer has to be to a mark to take hold of it.
const HANDLE_GRAB: f32 = 6.0;

/// Full-width bar spanning the current source's duration, with the clip's
/// loudness under it, in/out markers you can drag, and the playhead.
fn scrub_bar(app: &mut RoughcutApp, ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, theme::LINE),
    );

    // What span is the bar showing?
    let (total, marks, clip_id, highlights) = match app.focus {
        Focus::Source => match app.selected_source() {
            Some(c) => (
                c.duration_frames,
                Some((c.mark_in, c.mark_out, c.last_frame())),
                Some(c.id),
                c.highlights.clone(),
            ),
            None => (0, None, None, Vec::new()),
        },
        Focus::Timeline => (
            timeline::total_frames(&app.project.timeline),
            None,
            None,
            Vec::new(),
        ),
    };
    if total <= 0 {
        return;
    }
    if let Some(id) = clip_id {
        app.request_waveform(id);
    }

    let track = Rect::from_min_max(
        egui::pos2(rect.left() + 10.0, rect.top() + TRACK_TOP),
        egui::pos2(rect.right() - 10.0, rect.top() + TRACK_TOP + TRACK_HEIGHT),
    );
    painter.rect_filled(track, CornerRadius::ZERO, theme::PANEL_ALT);

    let x_of = |frame: i64| -> f32 {
        let t = (frame as f32 / (total - 1).max(1) as f32).clamp(0.0, 1.0);
        track.left() + t * track.width()
    };

    // --- gestures -----------------------------------------------------------
    //
    // Handled before anything is drawn so a drag shows this pass, not the
    // next one.
    let hit = Rect::from_min_max(
        egui::pos2(track.left(), rect.top()),
        egui::pos2(track.right(), rect.bottom()),
    );
    let response = ui.interact(hit, ui.id().with("scrub"), Sense::click_and_drag());
    let shift = ui.input(|i| i.modifiers.shift);
    let marking = app.focus == Focus::Source && app.selected_clip.is_some();
    let handle_at = |x: f32| -> Option<MarkEdge> {
        let (mark_in, mark_out, _) = marks?;
        [(mark_in, MarkEdge::In), (mark_out, MarkEdge::Out)]
            .into_iter()
            .filter_map(|(mark, edge)| mark.map(|m| (edge, (x_of(m) - x).abs())))
            .filter(|(_, d)| *d <= HANDLE_GRAB)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(edge, _)| edge)
    };

    // Advertise the handles: without this the only way to discover them is to
    // try dragging one and see what happens.
    if marking && app.mark_grab.is_none() {
        if let Some(p) = response.hover_pos() {
            if handle_at(p.x).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
    }

    if response.drag_started() {
        // Where the button actually went down, not where the pointer had got
        // to by the time egui decided this was a drag. Those differ by the
        // drag threshold, which is why a shift-drag used to start a few pixels
        // to the right of the pointer.
        let start = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        if let Some(pos) = start {
            let f = frame_at(pos, track, total);
            if marking && shift {
                app.mark_drag = Some((f, f));
            } else if let Some(edge) = marking.then(|| handle_at(pos.x)).flatten() {
                app.grab_mark(edge, f);
            }
        }
    }

    if app.mark_grab.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                app.drag_mark(frame_at(pos, track, total));
            }
        }
        if response.drag_stopped() {
            app.commit_mark_grab();
        }
    } else if app.mark_drag.is_some() {
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let f = frame_at(pos, track, total);
                if let Some((_, cur)) = app.mark_drag.as_mut() {
                    *cur = f;
                }
                // Follow the pointer while marking. Choosing where a range
                // ends without seeing the frame it ends on is guesswork, and
                // the whole reason for marking on the bar rather than by
                // stepping is to be able to see it.
                app.scrub_to(f);
            }
        }
        if response.drag_stopped() {
            app.commit_mark_drag();
        }
    } else if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            app.scrub_to(frame_at(pos, track, total));
        }
        if response.drag_stopped() || response.clicked() {
            app.scrub_settled();
        }
    }

    // --- drawing ------------------------------------------------------------
    //
    // A mark being dragged is shown where the pointer has it, not where the
    // project still says it is; nothing is written until the button comes up.
    // Re-read: a scrub or a handle drag just moved it.
    let position = app.position();
    let marks = marks.map(|(mut mark_in, mut mark_out, last)| {
        match app.mark_grab {
            Some((MarkEdge::In, f)) => mark_in = Some(f),
            Some((MarkEdge::Out, f)) => mark_out = Some(f),
            None => {}
        }
        (mark_in, mark_out, last)
    });

    if let Some(peaks) = clip_id.and_then(|id| app.waveforms.get(&id)) {
        waveform(&painter, track, peaks);
    }

    // The stretches already decided about, under everything else: a mark
    // being placed now has to stay readable on top of them.
    for h in &highlights {
        let span = Rect::from_min_max(
            egui::pos2(x_of(h.in_frame), track.top()),
            // A short keep in a long clip is a sliver; never less than a
            // pixel, or a real decision would draw as nothing.
            egui::pos2(
                (x_of(h.out_frame)).max(x_of(h.in_frame) + 1.0),
                track.bottom(),
            ),
        );
        painter.rect_filled(span, CornerRadius::ZERO, theme::KEEP.linear_multiply(0.55));
    }

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

    // The pending range of a shift-drag, which is not in the project yet.
    if let Some((a, b)) = app.mark_drag {
        let span = Rect::from_min_max(
            egui::pos2(x_of(a.min(b)), track.top() - 3.0),
            egui::pos2(x_of(a.max(b)), track.bottom() + 3.0),
        );
        painter.rect_filled(
            span,
            CornerRadius::ZERO,
            theme::MARK_IN.linear_multiply(0.45),
        );
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
                [egui::pos2(x, track.top()), egui::pos2(x, track.bottom())],
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

    // A picture of whatever is under the pointer, so a stretch of clip can be
    // found by moving across it rather than by seeking to each guess and
    // waiting for a decode. Costs a textured quad from the sheet already in
    // memory, and only while the pointer is actually over the bar.
    if let Some(pos) = response.hover_pos() {
        if !response.dragged() {
            hover_thumbnail(app, &painter, rect, track, pos, total, clip_id);
        }
    }

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
}

/// Width of the picture that follows the pointer along the scrub bar.
const HOVER_W: f32 = 172.0;

/// Show the frame under the pointer, above the bar.
///
/// Deliberately from the filmstrip sheet rather than from mpv. A sheet holds
/// 112 frames of the clip and is already resident, so this follows the pointer
/// exactly; asking mpv would mean a decode per pixel and a picture that lags
/// by most of a second. It is a coarse preview and reads as one — the sharp
/// frame is the one in the monitor, which the playhead is still driving.
fn hover_thumbnail(
    app: &mut RoughcutApp,
    painter: &egui::Painter,
    rect: Rect,
    track: Rect,
    pos: Pos2,
    total: i64,
    clip_id: Option<roughcut_core::ClipId>,
) {
    let Some(id) = clip_id else { return };
    app.request_scrub_sheet(id);
    let Some(thumb) = app.sheets.get(&id).filter(|t| t.tiles > 1) else {
        return;
    };
    let Some(duration) = app.project.clip(id).map(|c| c.duration_frames) else {
        return;
    };
    let frame = frame_at(pos, track, total);
    let tile = crate::workers::tile_for_frame(frame, duration, thumb.tiles);

    let h = HOVER_W / thumb.tile_aspect().max(0.05);
    // Above the bar, and kept inside the window at either end.
    let x = pos.x.clamp(rect.left() + HOVER_W / 2.0, rect.right() - HOVER_W / 2.0);
    let frame_rect = Rect::from_center_size(
        egui::pos2(x, rect.top() - h / 2.0 - 8.0),
        egui::vec2(HOVER_W, h),
    );
    painter.rect_filled(
        frame_rect.expand(2.0),
        egui::CornerRadius::same(2),
        theme::VIDEO_LETTERBOX,
    );
    painter.image(
        thumb.tex.id(),
        frame_rect,
        thumb.uv(tile),
        egui::Color32::WHITE,
    );
    painter.rect_stroke(
        frame_rect.expand(2.0),
        egui::CornerRadius::same(2),
        Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    // The time it is showing, so the picture is not the only thing to go on.
    painter.text(
        frame_rect.center_bottom() + egui::vec2(0.0, -2.0),
        egui::Align2::CENTER_BOTTOM,
        format_timecode(frame, app.fps()),
        egui::FontId::monospace(10.0),
        theme::TEXT,
    );
}

/// The clip's loudness, mirrored about the middle of the track.
///
/// One column per pixel, each taking the loudest bucket it covers — the
/// opposite of averaging, so a single sharp sound stays a visible spike at
/// every zoom level rather than being smoothed out of existence.
fn waveform(painter: &egui::Painter, track: Rect, peaks: &[u8]) {
    if peaks.is_empty() || track.width() < 1.0 {
        return;
    }
    let mid = track.center().y;
    let half = track.height() / 2.0 - 1.0;
    let columns = track.width().floor().max(1.0) as usize;
    let shapes: Vec<egui::Shape> = (0..columns)
        .filter_map(|c| {
            let from = c * peaks.len() / columns;
            let to = ((c + 1) * peaks.len() / columns).max(from + 1).min(peaks.len());
            let peak = peaks[from..to].iter().copied().max().unwrap_or(0);
            if peak == 0 {
                return None;
            }
            let h = (peak as f32 / 255.0) * half;
            let x = track.left() + c as f32;
            Some(egui::Shape::rect_filled(
                Rect::from_min_max(egui::pos2(x, mid - h), egui::pos2(x + 1.0, mid + h)),
                CornerRadius::ZERO,
                theme::WAVEFORM,
            ))
        })
        .collect();
    painter.extend(shapes);
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
