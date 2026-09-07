//! Every operation the editor can perform, and the canonical keyboard map.
//!
//! `KEYS.md` and the in-app `?` overlay are both generated from `KEY_MAP`, so
//! the documentation cannot drift from the implementation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Transport
    TogglePlay,
    ShuttleForward,
    ShuttleReverse,
    Pause,
    StepFrames(i64),
    StepSeconds(i64),
    GoToStart,
    GoToEnd,
    PrevCut,
    NextCut,

    // Marking (source focus)
    MarkIn,
    MarkOut,
    ClearIn,
    ClearOut,
    ClearMarks,
    /// Keep the marked range as a highlight, or drop the one under the
    /// playhead.
    KeepRange,

    // Assembly
    Append,
    Insert,

    // Timeline editing (timeline focus)
    Copy,
    Cut,
    Paste,
    Split,
    RippleDelete,
    TrimHead,
    TrimTail,
    MoveEarlier,
    MoveLater,

    // File
    NewProject,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    Import,
    ExportMlt,
    Undo,
    Redo,

    // View
    ToggleFocus,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ToggleFullscreen,
    ToggleHelp,
    /// Read the clip instead of watching it.
    ToggleTranscript,
}

/// Which region a binding applies to, for the help overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Source,
    Timeline,
}

pub struct Binding {
    pub keys: &'static str,
    pub description: &'static str,
    pub scope: Scope,
}

pub struct Section {
    pub title: &'static str,
    pub bindings: &'static [Binding],
}

/// The keyboard map exactly as §9 specifies it.
pub const KEY_MAP: &[Section] = &[
    Section {
        title: "Transport",
        bindings: &[
            Binding { keys: "Space", description: "Play / pause", scope: Scope::Global },
            Binding { keys: "L", description: "Play forward — repeat to cycle 1x, 2x, 4x, 8x", scope: Scope::Global },
            Binding { keys: "J", description: "Play reverse — repeat to cycle 1x, 2x, 4x, 8x", scope: Scope::Global },
            Binding { keys: "K", description: "Pause", scope: Scope::Global },
            Binding { keys: "Right / Left", description: "Step one frame forward / back", scope: Scope::Global },
            Binding { keys: "Shift+Right / Shift+Left", description: "Step one second forward / back", scope: Scope::Global },
            Binding { keys: "Home / End", description: "Go to start / end", scope: Scope::Global },
            Binding { keys: "Up / Down", description: "Jump to previous / next edit point", scope: Scope::Timeline },
            Binding { keys: "Alt+Left / Alt+Right", description: "Jump to previous / next edit point — Shotcut's binding", scope: Scope::Timeline },
        ],
    },
    Section {
        title: "Marking",
        bindings: &[
            Binding { keys: "I", description: "Mark in at the playhead", scope: Scope::Source },
            Binding { keys: "O", description: "Mark out at the playhead", scope: Scope::Source },
            Binding { keys: "Shift+I", description: "Clear in", scope: Scope::Source },
            Binding { keys: "Shift+O", description: "Clear out", scope: Scope::Source },
            Binding { keys: "Shift+X", description: "Clear both", scope: Scope::Source },
            Binding { keys: "G", description: "Keep the marked range as good material — press again inside one to drop it", scope: Scope::Source },
        ],
    },
    Section {
        title: "Assembly",
        bindings: &[
            Binding { keys: "A  or  Enter", description: "Append the marked source range to the timeline", scope: Scope::Global },
            Binding { keys: "V", description: "Insert the marked range at the playhead, rippling", scope: Scope::Global },
        ],
    },
    Section {
        title: "Timeline editing",
        bindings: &[
            Binding { keys: "Ctrl+C", description: "Copy the clip under the playhead", scope: Scope::Timeline },
            Binding { keys: "Ctrl+X", description: "Cut it: copy, then close the gap", scope: Scope::Timeline },
            Binding { keys: "Ctrl+V", description: "Paste at the playhead, rippling the rest", scope: Scope::Timeline },
            Binding { keys: "S", description: "Split at the playhead", scope: Scope::Timeline },
            Binding { keys: "X  or  Delete", description: "Ripple delete, closing the gap", scope: Scope::Timeline },
            Binding { keys: "[", description: "Trim the head of the clip to the playhead", scope: Scope::Timeline },
            Binding { keys: "]", description: "Trim the tail of the clip to the playhead", scope: Scope::Timeline },
            Binding { keys: "Ctrl+Left / Ctrl+Right", description: "Move the clip one position earlier / later", scope: Scope::Timeline },
        ],
    },
    Section {
        title: "File",
        bindings: &[
            Binding { keys: "Ctrl+N", description: "New project — the first clip imported sets the format", scope: Scope::Global },
            Binding { keys: "Ctrl+O", description: "Open project", scope: Scope::Global },
            Binding { keys: "Ctrl+S", description: "Save project", scope: Scope::Global },
            Binding { keys: "Ctrl+Shift+S", description: "Save project as", scope: Scope::Global },
            Binding { keys: "Ctrl+I", description: "Import media", scope: Scope::Global },
            Binding { keys: "Ctrl+E", description: "Export the cut — a Shotcut project, or a rendered MP4", scope: Scope::Global },
            Binding { keys: "Ctrl+Z / Ctrl+Shift+Z", description: "Undo / redo", scope: Scope::Global },
        ],
    },
    Section {
        title: "View",
        bindings: &[
            Binding { keys: "Tab", description: "Switch focus between source and timeline", scope: Scope::Global },
            Binding { keys: "- / =", description: "Zoom the timeline out / in", scope: Scope::Timeline },
            Binding { keys: "0", description: "Fit the timeline to the window", scope: Scope::Timeline },
            Binding { keys: "Ctrl+Wheel", description: "Zoom the timeline about the pointer", scope: Scope::Timeline },
            Binding { keys: "T", description: "Read the transcript instead of the picture", scope: Scope::Global },
            Binding { keys: "F11", description: "Toggle fullscreen", scope: Scope::Global },
            Binding { keys: "?", description: "Show this help", scope: Scope::Global },
        ],
    },
];

/// Render the key map as Markdown, so `KEYS.md` can be regenerated from the
/// same source of truth the app uses.
pub fn key_map_markdown() -> String {
    let mut out = String::from(
        "# Roughcut keyboard map\n\n\
         Generated from `crates/roughcut-app/src/actions.rs` — run\n\
         `cargo test -p roughcut-app keys_md_is_current` to check it is up to date.\n\n\
         Scope is the region a binding applies to. `Source` and `Timeline` bindings\n\
         act on whichever region currently has focus; press `Tab` to switch.\n",
    );
    for section in KEY_MAP {
        out.push_str(&format!("\n## {}\n\n", section.title));
        out.push_str("| Key | Action | Scope |\n| --- | --- | --- |\n");
        for b in section.bindings {
            let scope = match b.scope {
                Scope::Global => "Any",
                Scope::Source => "Source",
                Scope::Timeline => "Timeline",
            };
            out.push_str(&format!("| `{}` | {} | {} |\n", b.keys, b.description, scope));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_md_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../KEYS.md")
            .canonicalize()
            .expect("KEYS.md should exist at the workspace root")
    }

    /// `KEYS.md` is a deliverable, and a stale one is worse than none. Set
    /// `ROUGHCUT_REGEN_KEYS=1` to rewrite it from the table above.
    #[test]
    fn keys_md_is_current() {
        let path = keys_md_path();
        let generated = key_map_markdown();
        if std::env::var_os("ROUGHCUT_REGEN_KEYS").is_some() {
            std::fs::write(&path, &generated).expect("cannot write KEYS.md");
            return;
        }
        let on_disk = std::fs::read_to_string(&path).expect("cannot read KEYS.md");
        assert_eq!(
            on_disk.replace("\r\n", "\n"),
            generated,
            "KEYS.md is out of date — rerun with ROUGHCUT_REGEN_KEYS=1"
        );
    }

    #[test]
    fn every_section_has_bindings() {
        for s in KEY_MAP {
            assert!(!s.bindings.is_empty(), "section {} is empty", s.title);
        }
    }
}
