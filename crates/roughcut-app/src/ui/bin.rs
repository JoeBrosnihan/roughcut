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
/// Height of the green strip showing where a clip's good stretches are.
const KEEP_STRIP_H: f32 = 3.0;
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

                // Archived clips are hidden unless asked for. They are still
                // in the project — this is a view, not a filter on what
                // exists.
                let show_archived = app.settings.show_archived;
                let ids: Vec<ClipId> = app
                    .project
                    .clips
                    .iter()
                    .filter(|c| show_archived || !c.archived)
                    .map(|c| c.id)
                    .collect();
                if ids.is_empty() {
                    all_archived_state(app, ui);
                    return;
                }

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
    let mut toggle_proxies: Option<bool> = None;
    let mut toggle_ripple: Option<bool> = None;
    let mut toggle_archived: Option<bool> = None;
    let mut add_audio = false;
    let mut paste_image = false;

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
                // A screenshot is the one source that arrives without being
                // a file first. Here as well as on Ctrl+V because the key is
                // the sort of thing that gets intercepted, and a feature
                // reachable only by a shortcut somebody has to be told about
                // is not reachable.
                if menu_item(ui, "Paste image", "Ctrl+V") {
                    paste_image = true;
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
                if menu_item(ui, "Export…", "Ctrl+E") {
                    action = Some(Action::ExportMlt);
                }
                ui.separator();
                if menu_item(ui, "Add audio track", "") {
                    add_audio = true;
                }
                // Off by default, matching Shotcut. The one mode worth
                // keeping, because there is no right answer: an effect pinned
                // to a moment should follow a cut, a music bed should not.
                let mut ripple = app.settings.ripple_all_tracks;
                if ui
                    .checkbox(&mut ripple, "Ripple all tracks")
                    .on_hover_text(
                        "When the picture is cut, move the sound under it too. \
                         Off, sound stays where it was laid.",
                    )
                    .changed()
                {
                    toggle_ripple = Some(ripple);
                }
                ui.separator();
                // The one setting worth a menu entry: it is the difference
                // between seeking in 26 ms and seeking in 180 ms, and until
                // now it could only be reached by editing settings.json.
                let mut proxies = app.settings.proxies_enabled;
                if ui
                    .checkbox(&mut proxies, "Low-res proxies")
                    .on_hover_text(
                        "Transcode each clip to 540p in the background and play that                          instead. Seeking becomes roughly seven times faster; the                          cut is always made against the original.",
                    )
                    .changed()
                {
                    toggle_proxies = Some(proxies);
                }
                ui.separator();
                // The way back to footage that has been set aside: to restore
                // it, or to remove it for good. Archived clips draw faded and
                // no background work is spent on them.
                let archived_count = app.project.clips.iter().filter(|c| c.archived).count();
                let mut show_archived = app.settings.show_archived;
                ui.add_enabled_ui(archived_count > 0 || show_archived, |ui| {
                    if ui
                        .checkbox(
                            &mut show_archived,
                            format!("Show archived ({archived_count})"),
                        )
                        .on_hover_text(
                            "Delete on a bin clip archives it rather than removing it. \
                             Shown here faded, where Delete removes it for good.",
                        )
                        .changed()
                    {
                        toggle_archived = Some(show_archived);
                    }
                });
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

            let shown = app
                .project
                .clips
                .iter()
                .filter(|c| app.settings.show_archived || !c.archived)
                .count();
            ui.label(
                egui::RichText::new(format!("BIN ({shown})"))
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );
        });
    });

    if let Some(on) = toggle_proxies {
        app.set_proxies_enabled(on);
    }
    if let Some(on) = toggle_ripple {
        app.set_ripple_all_tracks(on);
    }
    if let Some(on) = toggle_archived {
        app.set_show_archived(on);
    }
    if add_audio {
        app.add_audio_track();
    }
    if paste_image {
        app.paste_image();
    }
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
        ui.menu_button("Recent", |ui| {
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

/// Every clip in the bin has been set aside. Says so, and offers the way back
/// rather than looking like an empty project.
fn all_archived_state(app: &mut RoughcutApp, ui: &mut egui::Ui) {
    let n = app.project.clips.len();
    ui.add_space(16.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{n} clip{} archived",
                if n == 1 { "" } else { "s" }
            ))
            .color(theme::TEXT_DIM),
        );
        ui.add_space(6.0);
        if ui.button("Show archived").clicked() {
            app.set_show_archived(true);
        }
    });
}

fn tile(app: &mut RoughcutApp, ui: &mut egui::Ui, id: ClipId, rect: Rect) {
    let Some(clip) = app.project.clip(id) else {
        return;
    };
    let showing = app.selected_clip == Some(id);
    let selected = showing || app.is_picked(id);
    let name = clip.file_name();
    let duration_frames = clip.duration_frames;
    let duration = format_timecode(duration_frames, app.project.fps());
    let rate_mismatch = clip.rate_mismatch;
    let variable_rate = clip.variable_rate;
    let flagged = clip.flagged;
    let archived = clip.archived;
    let audio_only = clip.audio_only;
    // Where the good stretches are, as fractions of the clip, so the strip
    // can be painted without keeping the clip borrowed.
    let last = clip.last_frame().max(1) as f32;
    let highlights: Vec<(f32, f32)> = clip
        .highlights
        .iter()
        .map(|h| (h.in_frame as f32 / last, h.out_frame as f32 / last))
        .collect();
    let missing = !clip.path.exists();
    let has_proxy = clip.proxy_path.as_ref().is_some_and(|p| p.exists());
    let marked = clip.mark_in.is_some() || clip.mark_out.is_some();
    let path = clip.path.display().to_string();
    let proxy_state = app.proxy_state.get(&id).copied();
    let transcribing = app.transcribing.contains(&id);
    let transcribed = app.transcripts.get(&id).is_some_and(|t| !t.is_empty());
    let uses = app.project.timeline_uses(id);

    let response = ui.interact(
        rect,
        egui::Id::new(("bin-tile", id)),
        Sense::click_and_drag(),
    );
    let thumb = Rect::from_min_size(rect.min, egui::vec2(rect.width(), THUMB_H));
    let painter = ui.painter_at(rect);
    // An archived tile is drawn at half strength: present, legible, and
    // obviously not part of the working bin.
    let fade = |c: egui::Color32| {
        if archived {
            c.linear_multiply(0.45)
        } else {
            c
        }
    };

    if selected || response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            if showing {
                theme::CLIP_SELECTED
            } else if selected {
                // Picked, but not the one being watched: the same colour at
                // half strength, so a run of them reads as one thing.
                theme::CLIP_SELECTED.linear_multiply(0.55)
            } else {
                theme::PANEL_ALT
            },
        );
    }
    painter.rect_filled(thumb, CornerRadius::ZERO, theme::VIDEO_LETTERBOX);

    // A sound file has no frames to show, so the tile says what it is
    // instead of sitting there as an empty black rectangle waiting for a
    // thumbnail that is never coming.
    if audio_only {
        let mid = thumb.center();
        painter.text(
            mid,
            egui::Align2::CENTER_CENTER,
            "\u{266A}",
            egui::FontId::proportional(26.0),
            fade(theme::WAVEFORM),
        );
        painter.text(
            egui::pos2(mid.x, thumb.bottom() - 12.0),
            egui::Align2::CENTER_CENTER,
            "SOUND",
            egui::FontId::proportional(9.0),
            fade(theme::TEXT_DIM),
        );
    }

    // Hover-scrub: the pointer's horizontal position within the thumbnail
    // picks which of the filmstrip's tiles to show. The tiles were baked into
    // one texture when the clip was imported, so this costs a UV offset and
    // nothing else — no decode, and no work at all once the pointer stops.
    let hover_x = response
        .hover_pos()
        .filter(|p| thumb.contains(*p))
        .map(|p| ((p.x - thumb.left()) / thumb.width()).clamp(0.0, 1.0));
    // Pointing at a clip is the strongest possible statement about which sheet
    // is wanted next.
    if hover_x.is_some() && !audio_only {
        app.request_scrub_sheet(id);
    }
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
            fade(egui::Color32::WHITE),
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

    // The good stretches, as a strip along the foot of the picture.
    //
    // A count would say how many there are; this says *where* they are and
    // how much of the clip they cover, which is the question being asked
    // while skimming a bin for material. It is the same shape as the scrub
    // bar directly below it, so the two read as one idea at two sizes.
    if !highlights.is_empty() {
        let strip = Rect::from_min_max(
            egui::pos2(thumb.left(), thumb.bottom() - KEEP_STRIP_H),
            thumb.right_bottom(),
        );
        painter.rect_filled(strip, CornerRadius::ZERO, egui::Color32::from_black_alpha(150));
        for (a, b) in &highlights {
            // At least a pixel wide: a two-second keep in a twenty-minute
            // take is a fraction of a pixel and would vanish, which would
            // say "nothing kept" about a clip that has something.
            let x0 = strip.left() + a * strip.width();
            let x1 = (strip.left() + b * strip.width()).max(x0 + 1.0);
            painter.rect_filled(
                Rect::from_min_max(
                    egui::pos2(x0, strip.top()),
                    egui::pos2(x1.min(strip.right()), strip.bottom()),
                ),
                CornerRadius::ZERO,
                fade(theme::KEEP),
            );
        }
    }

    // The flag, top right: clear of the status badges on the left and the
    // duration below, and the only warm mark on the tile.
    if flagged {
        let at = thumb.right_top() + egui::vec2(-9.0, 9.0);
        painter.circle_filled(at, 7.5, egui::Color32::from_black_alpha(190));
        painter.text(
            at,
            egui::Align2::CENTER_CENTER,
            "\u{2605}",
            egui::FontId::proportional(11.0),
            fade(theme::FLAG),
        );
    }

    // Duration on the picture, bottom right, the way every browser does it.
    painter.text(
        thumb.right_bottom() + egui::vec2(-3.0, -2.0),
        egui::Align2::RIGHT_BOTTOM,
        duration,
        egui::FontId::monospace(10.0),
        fade(theme::TEXT),
    );
    if marked {
        painter.text(
            thumb.left_bottom() + egui::vec2(3.0, -2.0),
            egui::Align2::LEFT_BOTTOM,
            "▮",
            egui::FontId::proportional(10.0),
            fade(theme::MARK_IN),
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
    if variable_rate {
        // The file does not hold a steady rate, so its positions are only as
        // exact as its average. Worth saying, since it is invisible otherwise.
        badge("VFR", theme::WARN);
    }
    match proxy_state {
        Some(ProxyState::Queued) => badge("PXY…", theme::TEXT_DIM),
        Some(ProxyState::Running) => badge("PXY▶", theme::ACCENT),
        Some(ProxyState::Failed) => badge("PXY!", theme::ERROR),
        None if has_proxy => badge("PXY", theme::MARK_IN),
        None => {}
    }
    // Whether this clip can be read as well as watched. Worth showing across a
    // large bin, because transcription runs for minutes and there is otherwise
    // no way to tell how far it has got.
    if transcribing {
        badge("TXT…", theme::TEXT_DIM);
    } else if transcribed {
        badge("TXT", theme::ACCENT);
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
        fade(if missing { theme::ERROR } else { theme::TEXT }),
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
        if !app.is_picked(id) {
            app.select_bin_clip(id);
        }
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
    // Middle-click flags the clip: reachable without leaving the contact
    // sheet, which is the point, since culling is a pass you do at speed.
    // Ctrl and Shift belong to picking clips out — that is what they do in
    // every other list on the machine, and a bin that disagreed would be
    // wrong in a way no label could fix.
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
    if response.clicked_by(egui::PointerButton::Middle) {
        app.toggle_flag(id);
    } else if response.clicked() && ctrl {
        app.toggle_bin_pick(id);
    } else if response.clicked() && shift {
        app.extend_bin_pick(id);
    } else if response.clicked() {
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
    let mut set_archived: Option<bool> = None;
    let mut relink = false;
    let mut reveal = false;
    let mut flag = false;
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
        if ui
            .button(if flagged { "Unflag" } else { "Flag as good" })
            .on_hover_text("Middle-click a clip to toggle this")
            .clicked()
        {
            flag = true;
            ui.close_kind(egui::UiKind::Menu);
        }
        ui.separator();
        if ui.button("Show in folder").clicked() {
            reveal = true;
            ui.close_kind(egui::UiKind::Menu);
        }
        ui.separator();
        if archived {
            if ui.button("Restore to bin").clicked() {
                set_archived = Some(false);
                ui.close_kind(egui::UiKind::Menu);
            }
        } else if ui
            .button("Archive")
            .on_hover_text("Delete does this too. The clip keeps its marks and its good stretches.")
            .clicked()
        {
            set_archived = Some(true);
            ui.close_kind(egui::UiKind::Menu);
        }
        // Removing for good is offered only once a clip has been set aside,
        // so the destructive item is never the one sitting under the pointer
        // during a first pass.
        ui.add_enabled_ui(uses == 0 && archived, |ui| {
            if ui.button("Remove from project").clicked() {
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
    if flag {
        app.toggle_flag(id);
    }
    if let Some(on) = set_archived {
        app.set_archived(id, on);
    }
    if reveal {
        app.reveal_clip(id);
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
