//! The transcript, as a document you can select from.
//!
//! This is the answer to long footage. Skimming a two-hour recording by
//! pictures means dragging past four and a half seconds per pixel; reading it
//! means finding the moment someone says the thing. Selecting a sentence here
//! is the same act as marking a range on the scrub bar — it sets `in` and
//! `out` — so `A` appends it and every other key already works.
//!
//! It replaces the picture rather than sitting beside it. Half a monitor of
//! video and half a monitor of text is worse at both, and `T` puts it back.

use crate::app::{Focus, RoughcutApp};
use crate::theme;
use roughcut_core::time::format_timecode;
use roughcut_core::transcript::{Selection, Transcript};

/// Gap between words, and the padding that makes a word a comfortable target.
const WORD_GAP: f32 = 4.0;

pub fn show(app: &mut RoughcutApp, ui: &mut egui::Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::BG);

    let Some((clip_id, transcript)) = app.current_transcript().map(|(i, t)| (i, t.clone())) else {
        placeholder(app, ui, rect);
        return;
    };
    if transcript.is_empty() {
        message(&painter, rect, "Nothing was said in this clip.");
        return;
    }

    // Where the playhead is, in the clip's own milliseconds, so the word being
    // spoken can be marked.
    let playing_ms = current_ms(app, clip_id);
    let current_word = playing_ms.and_then(|ms| transcript.word_at(ms));

    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)));
    ui.spacing_mut().item_spacing = egui::vec2(WORD_GAP, 6.0);

    let mut clicked_word: Option<usize> = None;
    let mut hovered_word: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(&mut ui, |ui| {
            let mut index = 0usize;
            for segment in &transcript.segments {
                if segment.words.is_empty() {
                    continue;
                }
                // The timecode a sentence starts at, in the margin, so the
                // document reads as footage rather than as prose.
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format_timecode(
                            ms_to_frame(segment.start_ms(), app.fps()),
                            app.fps(),
                        ))
                        .monospace()
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                    );
                });
                ui.horizontal_wrapped(|ui| {
                    for word in &segment.words {
                        let i = index;
                        index += 1;
                        let selected = app.text_selection.is_some_and(|s| s.contains(i));
                        let colour = if selected {
                            theme::TEXT
                        } else if current_word == Some(i) {
                            theme::PLAYHEAD
                        } else {
                            theme::TEXT
                        };
                        let response = ui.add(
                            egui::Label::new(egui::RichText::new(&word.text).size(13.5).color(colour))
                                .sense(egui::Sense::click_and_drag()),
                        );
                        if selected {
                            ui.painter().rect_filled(
                                response.rect.expand2(egui::vec2(2.0, 1.0)),
                                egui::CornerRadius::same(2),
                                theme::ACCENT.linear_multiply(0.35),
                            );
                            // Painted after the fact, so put the word back on
                            // top of its own highlight.
                            ui.painter().text(
                                response.rect.left_top(),
                                egui::Align2::LEFT_TOP,
                                &word.text,
                                egui::FontId::proportional(13.5),
                                theme::TEXT,
                            );
                        }
                        if current_word == Some(i) && !selected {
                            ui.painter().line_segment(
                                [response.rect.left_bottom(), response.rect.right_bottom()],
                                egui::Stroke::new(1.5, theme::PLAYHEAD),
                            );
                        }
                        if response.hovered() {
                            hovered_word = Some(i);
                        }
                        if response.drag_started() || response.clicked() {
                            clicked_word = Some(i);
                        }
                    }
                });
                ui.add_space(4.0);
            }
        });

    gestures(app, &ui, clicked_word, hovered_word, &transcript, clip_id);
}

/// Click to go there, drag to select, and the selection is the mark.
fn gestures(
    app: &mut RoughcutApp,
    ui: &egui::Ui,
    clicked: Option<usize>,
    hovered: Option<usize>,
    transcript: &Transcript,
    clip_id: roughcut_core::ClipId,
) {
    let (down, released) = ui.input(|i| {
        (
            i.pointer.button_down(egui::PointerButton::Primary),
            i.pointer.any_released(),
        )
    });

    if let Some(i) = clicked {
        // A press starts a selection of one word and seeks there, so a plain
        // click is "show me this moment" and a drag grows from it.
        app.text_selection = Some(Selection::new(i, i));
        app.selecting_text = true;
        seek_to_word(app, transcript, clip_id, i);
    } else if app.selecting_text && down {
        if let Some(i) = hovered {
            if let Some(sel) = app.text_selection {
                app.text_selection = Some(Selection::new(sel.from, i));
            }
        }
    }

    if app.selecting_text && released {
        app.selecting_text = false;
        // Committed the moment the button comes up: from here on the range is
        // an ordinary mark, shown on the scrub bar and appended by `A`.
        app.mark_from_selection();
    }
}

fn seek_to_word(
    app: &mut RoughcutApp,
    transcript: &Transcript,
    clip_id: roughcut_core::ClipId,
    index: usize,
) {
    let Some(word) = transcript.word(index) else {
        return;
    };
    let Some(clip) = app.project.clip(clip_id) else {
        return;
    };
    let frame = ms_to_frame(word.start_ms, app.fps()).clamp(0, clip.last_frame());
    app.selected_clip = Some(clip_id);
    app.focus = Focus::Source;
    app.scrub_to(frame);
    app.scrub_settled();
}

/// The playhead in the clip's own milliseconds.
fn current_ms(app: &RoughcutApp, clip_id: roughcut_core::ClipId) -> Option<i64> {
    let fps = app.fps();
    let frame = match app.focus {
        Focus::Source => app.source_frame,
        // On the timeline the playhead is a timeline position; what the
        // transcript needs is where that lands inside the source file.
        Focus::Timeline => {
            let (i, offset) = roughcut_core::timeline::item_at(&app.project.timeline, app.playhead)?;
            let item = app.project.timeline.get(i)?;
            if item.clip_id != clip_id {
                return None;
            }
            item.in_frame + offset
        }
    };
    Some(frame as i64 * 1000 * fps.den / fps.num.max(1))
}

fn ms_to_frame(ms: i64, fps: roughcut_core::Rational) -> i64 {
    if fps.den == 0 {
        return 0;
    }
    (ms as i128 * fps.num as i128 / (1000_i128 * fps.den as i128)) as i64
}

/// Why there is no document to show.
fn placeholder(app: &RoughcutApp, ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let clip = match app.focus {
        Focus::Source => app.selected_clip,
        Focus::Timeline => roughcut_core::timeline::item_at(&app.project.timeline, app.playhead)
            .and_then(|(i, _)| app.project.timeline.get(i))
            .map(|item| item.clip_id),
    };
    let text = if app.tools.whisper.is_none() {
        "Transcripts need whisper.cpp, which was not found.\n\
         Unzip a release into %LOCALAPPDATA%\\Programs\\whisper with a model in models\\."
            .to_string()
    } else if clip.is_some_and(|id| app.transcribing.contains(&id)) {
        "Listening to this clip…".to_string()
    } else if clip.is_some_and(|id| app.project.clip(id).is_some_and(|c| c.still)) {
        "A photograph has nothing to say.".to_string()
    } else if clip.is_some_and(|id| app.project.clip(id).is_some_and(|c| !c.has_audio)) {
        "This clip has no audio.".to_string()
    } else if clip.is_none() {
        "Select a clip in the bin.".to_string()
    } else {
        "No transcript yet.".to_string()
    };
    message(&painter, rect, &text);
}

fn message(painter: &egui::Painter, rect: egui::Rect, text: &str) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        theme::TEXT_DIM,
    );
}
