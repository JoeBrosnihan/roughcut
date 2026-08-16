//! The timeline strip: clip blocks proportional to duration, plus a playhead.

use crate::app::{Focus, RoughcutApp};
use crate::theme;
use crate::ui::truncate_middle;
use crate::workers;
use egui::{CornerRadius, Rect, Sense, Stroke, StrokeKind};
use roughcut_core::model::ClipId;
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
            wheel_input(app, ui, rect, total, usable);
            let px_per_frame = app.zoom;

            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);

            if app.project.timeline.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if dragging_clip(ui.ctx()).is_some() {
                        "Drop to start the timeline"
                    } else {
                        "Timeline empty — mark a range in the source and press A to append, \
                         or drag a clip here"
                    },
                    egui::FontId::proportional(12.0),
                    theme::TEXT_DIM,
                );
                // An empty timeline has only one possible drop position.
                accept_drop(app, ui, rect, 0);
                crate::ui::focus_border(ui, rect, app.focus == Focus::Timeline);
                return;
            }

            // Horizontal scroll keeps the playhead in view when zoomed in.
            let content_width = (total as f32 * px_per_frame).max(usable);
            let mut area = egui::ScrollArea::horizontal().auto_shrink([false, false]);
            if let Some(x) = app.timeline_scroll_to.take() {
                area = area.horizontal_scroll_offset(x.max(0.0));
            }
            let output = area
                .show(ui, |ui| {
                    let (canvas, response) = ui.allocate_exact_size(
                        egui::vec2(content_width, TIMELINE_HEIGHT - 4.0),
                        Sense::click_and_drag(),
                    );
                    draw(app, ui, canvas, px_per_frame);
                    handle_drag(app, ui, &response, canvas, px_per_frame);
                    handle_click(app, &response, canvas, px_per_frame);
                    canvas
                });
            // Remembered so the next wheel zoom knows where it is starting.
            app.timeline_offset = output.state.offset.x;
            let canvas = output.inner;

            // Dropping snaps to the nearest cut, because dropping between two
            // clips is what you almost always mean; a drop mid-clip splits it,
            // exactly as `V` does.
            if dragging_clip(ui.ctx()).is_some() {
                if let Some(pos) = ui.ctx().pointer_latest_pos() {
                    if rect.contains(pos) {
                        let frame = snap_to_cut(
                            app,
                            frame_at_x(pos.x, canvas, px_per_frame, app),
                            px_per_frame,
                        );
                        let x = canvas.left() + frame as f32 * px_per_frame;
                        painter.line_segment(
                            [
                                egui::pos2(x, rect.top() + RULER_HEIGHT),
                                egui::pos2(x, rect.top() + BLOCK_TOP + BLOCK_HEIGHT),
                            ],
                            egui::Stroke::new(2.0, theme::MARK_IN),
                        );
                        accept_drop(app, ui, rect, frame);
                    }
                }
            }

            crate::ui::focus_border(ui, rect, app.focus == Focus::Timeline);
        });
}

/// The wheel over the timeline: Ctrl to zoom, otherwise to scroll sideways.
///
/// Both are driven explicitly rather than left to the scroll area. The canvas
/// senses drags so clips can be reordered, which takes drag-to-pan away, and
/// the timeline only ever scrolls on one axis — so a plain wheel meaning
/// "sideways" is the useful mapping rather than a surprising one.
fn wheel_input(app: &mut RoughcutApp, ui: &egui::Ui, rect: Rect, total: i64, usable: f32) {
    let (delta, ctrl, pointer) = ui.input(|i| {
        let d = i.raw_scroll_delta;
        (
            // A horizontal wheel or trackpad gesture wins if there is one.
            if d.x != 0.0 { d.x } else { d.y },
            i.modifiers.command || i.modifiers.ctrl,
            i.pointer.latest_pos(),
        )
    });
    if delta == 0.0 {
        return;
    }
    let Some(pointer) = pointer.filter(|p| rect.contains(*p)) else {
        return;
    };

    if ctrl {
        // Zoom about the frame under the pointer. Zooming about the left edge
        // is disorienting: the thing you are looking at slides away from you.
        let canvas_left = rect.left() - app.timeline_offset;
        let frame_under_pointer = (pointer.x - canvas_left) / app.zoom.max(1e-6);

        let factor = if delta > 0.0 { 1.25 } else { 1.0 / 1.25 };
        let zoomed = (app.zoom * factor).clamp(0.0005, 40.0);
        if (zoomed - app.zoom).abs() < f32::EPSILON {
            return;
        }
        app.zoom = zoomed;
        app.zoom_fit = false;
        app.timeline_scroll_to =
            Some(frame_under_pointer * app.zoom - (pointer.x - rect.left()));
        return;
    }

    // Scroll sideways, bounded by how much timeline there is to see.
    let content = (total as f32 * app.zoom).max(usable);
    let max_offset = (content - usable).max(0.0);
    if max_offset <= 0.0 {
        return;
    }
    let target = (app.timeline_offset - delta).clamp(0.0, max_offset);
    app.timeline_scroll_to = Some(target);
}

/// The band the clip blocks occupy. Above it is the ruler, below it is empty.
fn is_on_blocks(y: f32, canvas: Rect) -> bool {
    y >= canvas.top() + BLOCK_TOP && y <= canvas.top() + BLOCK_TOP + BLOCK_HEIGHT
}

/// Dragging the timeline: on a clip it moves the clip, anywhere else it
/// scrubs.
///
/// Splitting by where the drag *starts* means the two gestures never compete —
/// the ruler is a scrub strip and the blocks are objects you can pick up, and
/// neither has to guess at the other's intent.
///
/// Handled on the canvas rather than with a widget per block: one interactive
/// region cannot fight with itself over which of two overlapping widgets got
/// the click, and clicking to seek keeps working unchanged.
fn handle_drag(
    app: &mut RoughcutApp,
    ui: &egui::Ui,
    response: &egui::Response,
    canvas: Rect,
    px_per_frame: f32,
) {
    let index_at = |app: &RoughcutApp, x: f32| -> Option<usize> {
        let frame = frame_at_x(x, canvas, px_per_frame, app);
        tl::item_at(&app.project.timeline, frame).map(|(i, _)| i)
    };

    // A horizontal-resize cursor over the ruler advertises that it scrubs.
    if let Some(p) = response.hover_pos() {
        if !is_on_blocks(p.y, canvas) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }

    if response.drag_started() {
        match response.interact_pointer_pos() {
            Some(p) if is_on_blocks(p.y, canvas) => {
                app.dragging_item = index_at(app, p.x);
            }
            Some(_) => app.scrubbing = true,
            None => {}
        }
    }

    // Scrubbing wins outright once started; the playhead follows the pointer
    // for as long as the button is held.
    if app.scrubbing {
        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                app.monitor.pause();
                app.focus = Focus::Timeline;
                app.set_position(frame_at_x(p.x, canvas, px_per_frame, app));
            }
        }
        if response.drag_stopped() {
            app.scrubbing = false;
        }
        return;
    }

    let Some(from) = app.dragging_item else {
        return;
    };

    if response.dragged() {
        // Show where it would land: the moved block ghosted, and a bar on the
        // boundary it would come to rest against.
        let painter = ui.painter_at(canvas);
        let start = tl::item_start(&app.project.timeline, from);
        if let Some(item) = app.project.timeline.get(from) {
            let ghost = Rect::from_min_max(
                egui::pos2(
                    canvas.left() + start as f32 * px_per_frame,
                    canvas.top() + BLOCK_TOP,
                ),
                egui::pos2(
                    canvas.left() + (start + item.len()) as f32 * px_per_frame,
                    canvas.top() + BLOCK_TOP + BLOCK_HEIGHT,
                ),
            );
            painter.rect_filled(
                ghost,
                CornerRadius::ZERO,
                theme::ACCENT.linear_multiply(0.25),
            );
        }
        if let Some(to) = response.interact_pointer_pos().and_then(|p| index_at(app, p.x)) {
            let boundary = if to > from { to + 1 } else { to };
            let x = canvas.left()
                + tl::item_start(&app.project.timeline, boundary) as f32 * px_per_frame;
            painter.line_segment(
                [
                    egui::pos2(x, canvas.top() + RULER_HEIGHT),
                    egui::pos2(x, canvas.top() + BLOCK_TOP + BLOCK_HEIGHT),
                ],
                Stroke::new(2.0, theme::MARK_IN),
            );
        }
    }

    if response.drag_stopped() {
        let to = response
            .interact_pointer_pos()
            .and_then(|p| index_at(app, p.x));
        app.dragging_item = None;
        if let Some(to) = to {
            app.reorder_item(from, to);
        }
    }
}

/// The bin clip currently being dragged, if any.
fn dragging_clip(ctx: &egui::Context) -> Option<ClipId> {
    egui::DragAndDrop::payload::<ClipId>(ctx).map(|id| *id)
}

/// Timeline frame under a screen x, in canvas coordinates.
fn frame_at_x(x: f32, canvas: Rect, px_per_frame: f32, app: &RoughcutApp) -> i64 {
    (((x - canvas.left()) / px_per_frame.max(1e-6)).round() as i64)
        .clamp(0, tl::total_frames(&app.project.timeline))
}

/// Pull a drop position onto a nearby cut. Dropping between two clips is what
/// is almost always meant, and hitting an exact boundary by hand is fiddly.
fn snap_to_cut(app: &RoughcutApp, frame: i64, px_per_frame: f32) -> i64 {
    let tolerance = (10.0 / px_per_frame.max(1e-6)) as i64;
    tl::cut_points(&app.project.timeline)
        .into_iter()
        .map(|p| (p, (p - frame).abs()))
        .filter(|(_, d)| *d <= tolerance)
        .min_by_key(|(_, d)| *d)
        .map(|(p, _)| p)
        .unwrap_or(frame)
}

/// Complete a drag if the pointer was released over `zone`.
fn accept_drop(app: &mut RoughcutApp, ui: &egui::Ui, zone: Rect, frame: i64) {
    let Some(clip_id) = dragging_clip(ui.ctx()) else {
        return;
    };
    let released = ui.input(|i| i.pointer.any_released());
    let over = ui
        .ctx()
        .pointer_latest_pos()
        .is_some_and(|p| zone.contains(p));
    if released && over {
        egui::DragAndDrop::clear_payload(ui.ctx());
        app.drop_clip_at(clip_id, frame);
    }
}

/// Tile a timeline block with the clip's own frames, so a block shows what is
/// actually in it rather than just a coloured rectangle.
///
/// The pictures come from the filmstrip sheet already built for the bin, so
/// this costs no decoding — only a handful of textured quads per block, and
/// only for the part of the block currently on screen.
fn paint_block_filmstrip(
    app: &RoughcutApp,
    painter: &egui::Painter,
    block: Rect,
    canvas: Rect,
    item: &roughcut_core::TimelineItem,
) {
    let Some(tex) = app.thumbnails.get(&item.clip_id) else {
        return;
    };
    let Some(clip) = app.project.clip(item.clip_id) else {
        return;
    };
    let tiles = workers::FILMSTRIP_FRAMES;
    let sheet = tex.size_vec2();
    if sheet.x <= 0.0 || sheet.y <= 0.0 || block.width() < 2.0 {
        return;
    }

    // One picture, drawn at the block's height, keeping the tile's aspect.
    let tile_aspect = (sheet.x / tiles as f32) / sheet.y;
    let img_w = (block.height() * tile_aspect).max(8.0);
    let clipped = painter.with_clip_rect(block.intersect(canvas));

    // Only walk the visible span. Zoomed in, a block can be tens of thousands
    // of pixels wide, and stepping across all of it would be pure waste.
    let from = block.left().max(canvas.left());
    let to = block.right().min(canvas.right());
    let first = ((from - block.left()) / img_w).floor().max(0.0);
    let mut x = block.left() + first * img_w;

    let len = item.len().max(1);
    while x < to {
        // Which source frame sits at this x, and therefore which tile.
        let t = ((x - block.left()) / block.width()).clamp(0.0, 1.0);
        let source = item.in_frame + (t * (len - 1) as f32) as i64;
        let tile = workers::tile_for_frame(source, clip.duration_frames);
        let u0 = tile as f32 / tiles as f32;
        let u1 = (tile + 1) as f32 / tiles as f32;

        clipped.image(
            tex.id(),
            Rect::from_min_size(egui::pos2(x, block.top()), egui::vec2(img_w, block.height())),
            Rect::from_min_max(egui::pos2(u0, 0.0), egui::pos2(u1, 1.0)),
            egui::Color32::WHITE,
        );
        x += img_w;
    }

    // Darken slightly so the labels drawn on top stay readable.
    clipped.rect_filled(
        block,
        CornerRadius::same(theme::CLIP_RADIUS),
        egui::Color32::from_black_alpha(90),
    );
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
        let radius = CornerRadius::same(theme::CLIP_RADIUS);
        painter.rect_filled(
            block,
            radius,
            if selected {
                theme::CLIP_SELECTED
            } else {
                theme::CLIP
            },
        );
        paint_block_filmstrip(app, &painter, block, canvas, item);

        painter.rect_stroke(
            block,
            radius,
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
