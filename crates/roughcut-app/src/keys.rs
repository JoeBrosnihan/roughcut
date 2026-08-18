//! Translating raw key events into `Action`s.
//!
//! Reading `input.events` rather than `input.key_pressed` matters: it preserves
//! ordering, honours auto-repeat for stepping, and lets a single press produce
//! exactly one action.

use crate::actions::Action;
use egui::{Event, Key, Modifiers};

/// Collect the actions implied by this frame's key events.
///
/// There is deliberately no "is egui using the keyboard?" guard here.
/// `Context::wants_keyboard_input` is `memory.focused().is_some()`, which is
/// true for *any* focused widget, not just a text field — so pressing `Tab`
/// handed focus to the scrub bar and silently killed every binding in the
/// application until something was clicked. Roughcut has no text widgets at
/// all (file names come from native dialogs), so the correct answer is always
/// "no", and `App::update` clears egui's focus each pass to keep it that way.
///
/// If a text field is ever added, the guard must come back — but as a test for
/// a *text* widget specifically, not for focus in general.
pub fn actions_this_frame(ctx: &egui::Context) -> Vec<Action> {
    ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => map_key(*key, *modifiers),
                _ => None,
            })
            .collect()
    })
}

fn map_key(key: Key, m: Modifiers) -> Option<Action> {
    let ctrl = m.command || m.ctrl;
    let shift = m.shift;

    // Ctrl-modified bindings first: Ctrl+I must not also mean "mark in".
    if ctrl {
        return match key {
            Key::N => Some(Action::NewProject),
            Key::O => Some(Action::OpenProject),
            Key::S if shift => Some(Action::SaveProjectAs),
            Key::S => Some(Action::SaveProject),
            Key::I => Some(Action::Import),
            Key::E => Some(Action::ExportMlt),
            Key::Z if shift => Some(Action::Redo),
            Key::Z => Some(Action::Undo),
            Key::Y => Some(Action::Redo),
            Key::C => Some(Action::Copy),
            Key::X => Some(Action::Cut),
            Key::V => Some(Action::Paste),
            Key::ArrowLeft => Some(Action::MoveEarlier),
            Key::ArrowRight => Some(Action::MoveLater),
            _ => None,
        };
    }

    // Shotcut moves between edit points with Alt+Left/Right. The brief put the
    // same function on Up/Down; both are kept, so muscle memory from either
    // tool works.
    if m.alt {
        return match key {
            Key::ArrowLeft => Some(Action::PrevCut),
            Key::ArrowRight => Some(Action::NextCut),
            _ => None,
        };
    }

    match key {
        // Transport
        Key::Space => Some(Action::TogglePlay),
        Key::L => Some(Action::ShuttleForward),
        Key::J => Some(Action::ShuttleReverse),
        Key::K => Some(Action::Pause),
        Key::ArrowRight if shift => Some(Action::StepSeconds(1)),
        Key::ArrowLeft if shift => Some(Action::StepSeconds(-1)),
        Key::ArrowRight => Some(Action::StepFrames(1)),
        Key::ArrowLeft => Some(Action::StepFrames(-1)),
        Key::Home => Some(Action::GoToStart),
        Key::End => Some(Action::GoToEnd),
        Key::ArrowUp => Some(Action::PrevCut),
        Key::ArrowDown => Some(Action::NextCut),

        // Marking
        Key::I if shift => Some(Action::ClearIn),
        Key::I => Some(Action::MarkIn),
        Key::O if shift => Some(Action::ClearOut),
        Key::O => Some(Action::MarkOut),
        Key::X if shift => Some(Action::ClearMarks),

        // Assembly
        Key::A => Some(Action::Append),
        Key::Enter => Some(Action::Append),
        Key::V => Some(Action::Insert),

        // Timeline editing. `S` and `X` match Shotcut, so the two tools agree.
        Key::S => Some(Action::Split),
        Key::X | Key::Delete | Key::Backspace => Some(Action::RippleDelete),
        Key::OpenBracket => Some(Action::TrimHead),
        Key::CloseBracket => Some(Action::TrimTail),

        // View
        Key::Tab => Some(Action::ToggleFocus),
        Key::Minus => Some(Action::ZoomOut),
        Key::Equals | Key::Plus => Some(Action::ZoomIn),
        Key::Num0 => Some(Action::ZoomFit),
        Key::F11 => Some(Action::ToggleFullscreen),
        Key::Questionmark => Some(Action::ToggleHelp),
        Key::Slash if shift => Some(Action::ToggleHelp),
        Key::Escape => Some(Action::ToggleHelp),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::default()
    }
    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Default::default()
        }
    }
    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        }
    }
    fn ctrl_shift() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        }
    }

    #[test]
    fn transport_keys() {
        assert_eq!(map_key(Key::Space, plain()), Some(Action::TogglePlay));
        assert_eq!(map_key(Key::L, plain()), Some(Action::ShuttleForward));
        assert_eq!(map_key(Key::J, plain()), Some(Action::ShuttleReverse));
        assert_eq!(map_key(Key::K, plain()), Some(Action::Pause));
    }

    #[test]
    fn stepping_respects_shift() {
        assert_eq!(map_key(Key::ArrowRight, plain()), Some(Action::StepFrames(1)));
        assert_eq!(map_key(Key::ArrowLeft, plain()), Some(Action::StepFrames(-1)));
        assert_eq!(map_key(Key::ArrowRight, shift()), Some(Action::StepSeconds(1)));
        assert_eq!(map_key(Key::ArrowLeft, shift()), Some(Action::StepSeconds(-1)));
    }

    #[test]
    fn marking_and_clearing_are_distinct() {
        assert_eq!(map_key(Key::I, plain()), Some(Action::MarkIn));
        assert_eq!(map_key(Key::I, shift()), Some(Action::ClearIn));
        assert_eq!(map_key(Key::O, plain()), Some(Action::MarkOut));
        assert_eq!(map_key(Key::O, shift()), Some(Action::ClearOut));
        assert_eq!(map_key(Key::X, shift()), Some(Action::ClearMarks));
        assert_eq!(map_key(Key::X, plain()), Some(Action::RippleDelete));
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Default::default()
        }
    }

    /// `S` and `X` deliberately match Shotcut, and `Ctrl+S` must still save.
    #[test]
    fn shotcut_timeline_keys() {
        assert_eq!(map_key(Key::S, plain()), Some(Action::Split));
        assert_eq!(map_key(Key::X, plain()), Some(Action::RippleDelete));
        assert_eq!(map_key(Key::S, ctrl()), Some(Action::SaveProject));
        assert_eq!(map_key(Key::S, ctrl_shift()), Some(Action::SaveProjectAs));
    }

    /// Shotcut's edit-point navigation, alongside the brief's Up/Down.
    #[test]
    fn alt_arrows_move_between_edit_points() {
        assert_eq!(map_key(Key::ArrowLeft, alt()), Some(Action::PrevCut));
        assert_eq!(map_key(Key::ArrowRight, alt()), Some(Action::NextCut));
        assert_eq!(map_key(Key::ArrowUp, plain()), Some(Action::PrevCut));
        assert_eq!(map_key(Key::ArrowDown, plain()), Some(Action::NextCut));
        // Alt must not swallow stepping or reordering.
        assert_eq!(map_key(Key::ArrowLeft, plain()), Some(Action::StepFrames(-1)));
        assert_eq!(map_key(Key::ArrowLeft, ctrl()), Some(Action::MoveEarlier));
        assert_eq!(map_key(Key::A, alt()), None);
    }

    #[test]
    fn ctrl_i_is_import_not_mark_in() {
        assert_eq!(map_key(Key::I, ctrl()), Some(Action::Import));
    }

    #[test]
    fn file_bindings() {
        assert_eq!(map_key(Key::N, ctrl()), Some(Action::NewProject));
        assert_eq!(map_key(Key::O, ctrl()), Some(Action::OpenProject));
        assert_eq!(map_key(Key::S, ctrl()), Some(Action::SaveProject));
        assert_eq!(map_key(Key::S, ctrl_shift()), Some(Action::SaveProjectAs));
        assert_eq!(map_key(Key::E, ctrl()), Some(Action::ExportMlt));
        assert_eq!(map_key(Key::Z, ctrl()), Some(Action::Undo));
        assert_eq!(map_key(Key::Z, ctrl_shift()), Some(Action::Redo));
    }

    #[test]
    fn ctrl_arrows_reorder_rather_than_step() {
        assert_eq!(map_key(Key::ArrowLeft, ctrl()), Some(Action::MoveEarlier));
        assert_eq!(map_key(Key::ArrowRight, ctrl()), Some(Action::MoveLater));
    }

    #[test]
    fn assembly_and_trim() {
        assert_eq!(map_key(Key::A, plain()), Some(Action::Append));
        assert_eq!(map_key(Key::Enter, plain()), Some(Action::Append));
        assert_eq!(map_key(Key::V, plain()), Some(Action::Insert));
        assert_eq!(map_key(Key::OpenBracket, plain()), Some(Action::TrimHead));
        assert_eq!(map_key(Key::CloseBracket, plain()), Some(Action::TrimTail));
        assert_eq!(map_key(Key::Delete, plain()), Some(Action::RippleDelete));
        assert_eq!(map_key(Key::Backspace, plain()), Some(Action::RippleDelete));
    }

    #[test]
    fn every_spec_binding_is_reachable() {
        // A blunt guard against a binding being dropped in a refactor.
        let expected = [
            Action::TogglePlay,
            Action::ShuttleForward,
            Action::ShuttleReverse,
            Action::Pause,
            Action::StepFrames(1),
            Action::StepFrames(-1),
            Action::StepSeconds(1),
            Action::StepSeconds(-1),
            Action::GoToStart,
            Action::GoToEnd,
            Action::PrevCut,
            Action::NextCut,
            Action::MarkIn,
            Action::MarkOut,
            Action::ClearIn,
            Action::ClearOut,
            Action::ClearMarks,
            Action::Append,
            Action::Insert,
            Action::Split,
            Action::RippleDelete,
            Action::TrimHead,
            Action::TrimTail,
            Action::MoveEarlier,
            Action::MoveLater,
            Action::NewProject,
            Action::OpenProject,
            Action::SaveProject,
            Action::SaveProjectAs,
            Action::Import,
            Action::ExportMlt,
            Action::Undo,
            Action::Redo,
            Action::ToggleFocus,
            Action::ZoomIn,
            Action::ZoomOut,
            Action::ZoomFit,
            Action::ToggleFullscreen,
            Action::ToggleHelp,
        ];
        let all_keys = [
            Key::Space, Key::L, Key::J, Key::K, Key::ArrowLeft, Key::ArrowRight,
            Key::ArrowUp, Key::ArrowDown, Key::Home, Key::End, Key::I, Key::O,
            Key::X, Key::A, Key::Enter, Key::V, Key::Delete, Key::Backspace,
            Key::OpenBracket, Key::CloseBracket, Key::S, Key::E, Key::Z, Key::N,
            Key::Tab, Key::Minus, Key::Equals, Key::Num0, Key::Questionmark, Key::F11,
        ];
        let mut produced = Vec::new();
        for k in all_keys {
            for m in [plain(), shift(), ctrl(), ctrl_shift()] {
                if let Some(a) = map_key(k, m) {
                    produced.push(a);
                }
            }
        }
        for want in expected {
            assert!(produced.contains(&want), "no key produces {want:?}");
        }
    }
}
