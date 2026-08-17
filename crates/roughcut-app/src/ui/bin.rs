//! The bin: a scrollable grid of clip thumbnails.
//!
//! A grid rather than a list because the picture is what identifies a clip;
//! the filename rarely does. Hovering a tile scrubs it, which makes the grid a
//! contact sheet you can skim rather than a table you have to read.

use crate::app::{ProxyState, RoughcutApp};
use crate::theme;
use crate::ui::truncate_middle;
use egui::{CornerRadius, Rect, Sense, StrokeKind};
use roughcut_core::model::ClipId;
use roughcut_core::rotate::Turn;
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
/// Vertical distance from one row of tiles to the next.
const ROW_PITCH: f32 = TILE_H + GAP;
/// Points one wheel notch produces before any multiplier — egui's
/// `line_scroll_speed` on native, which winit feeds one line per notch.
const POINTS_PER_NOTCH: f32 = 40.0;

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

                // A wheel event is applied over several frames, and nothing in
                // egui asks for the repaints that would finish the job. The
                // event loop only wakes on input, so the tail of every flick
                // was being stranded until something else happened to wake it
                // — which is why a burst of notches never added up to what it
                // should have. This costs frames only while the wheel is
                // actually turning: the residual decays to nothing within
                // about a tenth of a second, and then the repaints stop.
                if ui.input(|i| i.smooth_scroll_delta) != egui::Vec2::ZERO {
                    ui.ctx().request_repaint();
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    // One notch, one row. egui's default of 40 points is a
                    // text-oriented figure that moves less than half a tile
                    // here, so the wheel had to be hammered to get anywhere.
                    // Deriving this from the row pitch rather than picking a
                    // number keeps a notch landing on a row boundary whatever
                    // the tiles are sized at.
                    .wheel_scroll_multiplier(egui::Vec2::splat(ROW_PITCH / POINTS_PER_NOTCH))
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
            // Scrub data is built for what you can see, not for the whole bin.
            app.request_scrub_sheet(*id);
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
    let mut open_recent: Option<std::path::PathBuf> = None;
    let mut clear_recents = false;

    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.add_space(4.0);
            ui.menu_button("File", |ui| {
                ui.set_min_width(190.0);
                if menu_item(ui, "New project", "Ctrl+N") {
                    action = Some(Action::NewProject);
                }
                if menu_item(ui, "Import media…", "Ctrl+I") {
                    action = Some(Action::Import);
                }
                ui.separator();
                if menu_item(ui, "Open project…", "Ctrl+O") {
                    action = Some(Action::OpenProject);
                }
                recents_menu(app, ui, &mut open_recent, &mut clear_recents);
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
    if let Some(path) = open_recent {
        app.open_recent(&path);
    }
    if clear_recents {
        app.clear_recents();
    }
    if let Some(a) = action {
        app.dispatch(a);
    }
}

/// The recents submenu.
///
/// Shows the file name, since that is what anyone recognises, with the folder
/// underneath only where two entries would otherwise read identically — the
/// full path is on hover either way. A project whose file has since gone is
/// left visible but disabled rather than quietly dropped, because vanishing
/// entries are more confusing than dead ones.
fn recents_menu(
    app: &RoughcutApp,
    ui: &mut egui::Ui,
    open: &mut Option<std::path::PathBuf>,
    clear: &mut bool,
) {
    let recents = app.settings.recent_projects.clone();
    ui.add_enabled_ui(!recents.is_empty(), |ui| {
        ui.menu_button("Open recent", |ui| {
            ui.set_min_width(240.0);
            let names: Vec<String> = recents
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .collect();
            for (i, path) in recents.iter().enumerate() {
                let ambiguous = names.iter().enumerate().any(|(j, n)| j != i && *n == names[i]);
                let label = if ambiguous {
                    let parent = path
                        .parent()
                        .and_then(|d| d.file_name())
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    format!("{}  —  {parent}", names[i])
                } else {
                    names[i].clone()
                };
                let exists = path.exists();
                let response = ui.add_enabled(
                    exists,
                    egui::Button::new(truncate_middle(&label, 40)).frame(false),
                );
                if response.on_hover_text(path.display().to_string()).clicked() {
                    *open = Some(path.clone());
                    ui.close_kind(egui::UiKind::Menu);
                }
            }
            ui.separator();
            if ui.button("Clear list").clicked() {
                *clear = true;
                ui.close_kind(egui::UiKind::Menu);
            }
        });
    });
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
    if let Some(t) = app.thumb(id) {
        // The tile under the pointer. A sheet has one per pixel of the
        // thumbnail's width, so a single pixel of movement lands on a
        // different frame; a clip still showing only its poster has one tile
        // and simply shows it.
        let frame_tile = match hover_x {
            Some(x) => ((x * t.tiles as f32) as usize).min(t.tiles - 1),
            None => t.tiles / 2,
        };
        let aspect = t.tile_aspect();
        let mut size = egui::vec2(thumb.width(), thumb.width() / aspect);
        if size.y > thumb.height() {
            size = egui::vec2(thumb.height() * aspect, thumb.height());
        }
        painter.image(
            t.tex.id(),
            Rect::from_center_size(thumb.center(), size),
            t.uv(frame_tile),
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
    // Whether this clip is in the cut, and how many times. It is the question
    // the bin gets asked most while assembling — "have I used this one yet?" —
    // and it was previously only answerable by opening the context menu.
    if uses > 0 {
        badge(&format!("×{uses}"), theme::ACCENT);
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
        // Opens at the start, not at the frame under the pointer. Skimming is
        // for finding the clip you want; once you have picked it you want to
        // watch it, and landing at whatever moment the pointer happened to be
        // over means scrubbing backwards first every time.
        app.select_bin_clip(id);
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
    let mut turn = None;
    let rotating = app.rotating.contains(&id);
    response.context_menu(|ui| {
        ui.set_min_width(170.0);
        if missing && ui.button("Relink…").clicked() {
            relink = true;
            ui.close_kind(egui::UiKind::Menu);
        }
        // Rotation rewrites the file on disk, so it stays available whatever
        // the timeline is doing — a clip that arrived on its side is wrong
        // everywhere, and the fix cannot wait until it is unused.
        ui.add_enabled_ui(!missing && !rotating, |ui| {
            if ui.button("Rotate right").clicked() {
                turn = Some(Turn::Clockwise);
                ui.close_kind(egui::UiKind::Menu);
            }
            if ui.button("Rotate left").clicked() {
                turn = Some(Turn::CounterClockwise);
                ui.close_kind(egui::UiKind::Menu);
            }
        });
        if rotating {
            ui.label(
                egui::RichText::new("rotating…")
                    .small()
                    .color(theme::TEXT_DIM),
            );
        }
        ui.separator();
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
    if let Some(turn) = turn {
        app.rotate_clip(id, turn);
    }
    if remove {
        app.select_bin_clip(id);
        app.remove_selected_clip();
    }

    response.on_hover_text(format!(
        "{name}\n{path}\n{duration_frames} frames{}{}",
        if rate_mismatch {
            "\nframe rate differs from the project — MLT will resample"
        } else {
            ""
        },
        match uses {
            0 => String::new(),
            1 => "\nused once on the timeline".to_string(),
            n => format!("\nused {n} times on the timeline"),
        }
    ));
}
