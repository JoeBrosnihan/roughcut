//! The four regions of the single window, plus two overlays.
//!
//! Panels are added bottom-up so egui stacks them correctly: status bar first
//! (outermost bottom), then the timeline above it, then the bin on the left,
//! and the source monitor fills whatever is left.

pub mod bin;
pub mod dialogs;
pub mod help;
pub mod monitor_panel;
pub mod status;
pub mod timeline;

use crate::theme;
use egui::{CornerRadius, Rect, StrokeKind};

/// Draw the accent border that marks the region with keyboard focus.
pub fn focus_border(ui: &egui::Ui, rect: Rect, focused: bool) {
    if !focused {
        return;
    }
    ui.painter().rect_stroke(
        rect.shrink(theme::FOCUS_BORDER / 2.0),
        CornerRadius::ZERO,
        egui::Stroke::new(theme::FOCUS_BORDER, theme::ACCENT),
        StrokeKind::Inside,
    );
}

/// Shorten a filename from the middle so both the name and extension stay
/// readable in a narrow column.
pub fn truncate_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars || max_chars < 5 {
        return text.to_string();
    }
    let keep = max_chars - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::truncate_middle;

    #[test]
    fn short_names_are_left_alone() {
        assert_eq!(truncate_middle("a.mp4", 20), "a.mp4");
    }

    #[test]
    fn long_names_keep_head_and_extension() {
        let out = truncate_middle("a_very_long_clip_name_indeed.mp4", 16);
        assert_eq!(out.chars().count(), 16);
        assert!(out.starts_with("a_very_l"));
        assert!(out.ends_with(".mp4"));
    }
}
