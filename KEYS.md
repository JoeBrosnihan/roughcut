# Roughcut keyboard map

Generated from `crates/roughcut-app/src/actions.rs` — run
`cargo test -p roughcut-app keys_md_is_current` to check it is up to date.

Scope is the region a binding applies to. `Source` and `Timeline` bindings
act on whichever region currently has focus; press `Tab` to switch.

## Transport

| Key | Action | Scope |
| --- | --- | --- |
| `Space` | Play / pause | Any |
| `L` | Play forward — repeat to cycle 1x, 2x, 4x, 8x | Any |
| `J` | Play reverse — repeat to cycle 1x, 2x, 4x, 8x | Any |
| `K` | Pause | Any |
| `Right / Left` | Step one frame forward / back | Any |
| `Shift+Right / Shift+Left` | Step one second forward / back | Any |
| `Home / End` | Go to start / end | Any |
| `Up / Down` | Jump to previous / next edit point | Timeline |
| `Alt+Left / Alt+Right` | Jump to previous / next edit point — Shotcut's binding | Timeline |

## Marking

| Key | Action | Scope |
| --- | --- | --- |
| `I` | Mark in at the playhead | Source |
| `O` | Mark out at the playhead | Source |
| `Shift+I` | Clear in | Source |
| `Shift+O` | Clear out | Source |
| `Shift+X` | Clear both | Source |
| `G` | Keep the marked range as good material — press again inside one to drop it | Source |

## Assembly

| Key | Action | Scope |
| --- | --- | --- |
| `A  or  Enter` | Append the marked source range to the timeline | Any |
| `V` | Insert the marked range at the playhead, rippling | Any |

## Timeline editing

| Key | Action | Scope |
| --- | --- | --- |
| `Ctrl+C` | Copy the clip under the playhead | Timeline |
| `Ctrl+X` | Cut it: copy, then close the gap | Timeline |
| `Ctrl+V` | Paste at the playhead, rippling the rest | Timeline |
| `S` | Split at the playhead — the selected sound, or the cut under it | Timeline |
| `X  or  Delete` | Ripple delete, closing the gap | Timeline |
| `[` | Trim the head of the clip to the playhead | Timeline |
| `]` | Trim the tail of the clip to the playhead | Timeline |
| `Ctrl+Left / Ctrl+Right` | Move the clip one position earlier / later | Timeline |

## File

| Key | Action | Scope |
| --- | --- | --- |
| `Ctrl+N` | New project — the first clip imported sets the format | Any |
| `Ctrl+O` | Open project | Any |
| `Ctrl+S` | Save project | Any |
| `Ctrl+Shift+S` | Save project as | Any |
| `Ctrl+I` | Import media | Any |
| `Ctrl+V` | Paste a picture from the clipboard into the bin, as a file beside the project | Source |
| `Ctrl+E` | Export the cut — a Shotcut project, or a rendered MP4 | Any |
| `Ctrl+Z / Ctrl+Shift+Z` | Undo / redo | Any |

## View

| Key | Action | Scope |
| --- | --- | --- |
| `Tab` | Switch focus between source and timeline | Any |
| `- / =` | Zoom the timeline out / in | Timeline |
| `0` | Fit the timeline to the window | Timeline |
| `Ctrl+Wheel` | Zoom the timeline about the pointer | Timeline |
| `T` | Read the transcript instead of the picture | Any |
| `F11` | Toggle fullscreen | Any |
| `?` | Show this help | Any |
