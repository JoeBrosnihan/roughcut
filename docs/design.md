# Design notes

## Crate layout

```
crates/roughcut-core   model, timing, timeline ops, undo, ffprobe, MLT writer
crates/roughcut-mpv    libmpv client + OpenGL render API bindings
crates/roughcut-app    egui application
```

`roughcut-core` knows nothing about egui, mpv or threads, and is
`#![forbid(unsafe_code)]`. The correctness-critical parts — the timing model,
timeline arithmetic, the MLT writer — are therefore testable without a window
or a GPU.

## The data model

The timeline is a flat, ordered `Vec<TimelineItem>`. A clip's start position is
the cumulative sum of the durations before it — computed, never stored — so
ripple operations are just `Vec` insert and remove and cannot desynchronise.

Undo snapshots the entire `Project` on every mutation, capped at 500 entries.
This is deliberately naive: the model is a few kilobytes even with hundreds of
clips, so full snapshots cost nothing and are impossible to get subtly wrong.
It is not to be replaced with a command pattern.

`RoughcutApp::edit` is the single funnel every project mutation passes through.
It takes the undo snapshot, sets the dirty flag and arms the autosave, and it
restores the previous state if the edit refuses — so a rejected trim cannot
leave a half-applied change behind.

## Video integration

The brief offers two routes for getting mpv on screen and asks which one
shipped. **The render API path shipped**: `MPV_RENDER_API_TYPE_OPENGL`, with
mpv rendering into a framebuffer object Roughcut owns, drawn inside an `egui`
paint callback. The child-window / `--wid` / JSON-IPC fallback was not needed.

Three details worth knowing:

- OpenGL function pointers are resolved directly from `opengl32.dll` via
  `wglGetProcAddress`, falling back to the export table, because eframe hands
  out a `glow::Context` but not a `get_proc_address`. This is what glutin does
  internally; doing it in `roughcut-mpv::gl_loader` avoids threading a display
  handle through eframe.
- The FBO is composited with `glBlitFramebuffer` rather than a textured quad,
  so there is no shader, VAO or vertex buffer of our own to get wrong. mpv
  renders with `flip_y` set, because it writes the top of the picture to row
  zero while the blit treats row zero as the bottom.
- eframe makes the GL context current *before* calling `App::update`, so the
  render context is created there rather than inside the paint callback. That
  keeps the callback — which must be `Send + Sync` — from having to borrow the
  player.

## Idle discipline

The zero-CPU-at-idle requirement shapes more of the app than anything except
the timing model.

- The event loop is `ControlFlow::Wait`. Never `Poll`.
- Nothing in `update` requests a repaint unconditionally. The only scheduled
  repaint is a per-frame tick while the transport is actually running.
- Background workers block on a condvar — a real OS sleep, no timer — and wake
  the UI through `Context::request_repaint` only when they have a result.
- mpv wakes the UI the same way, from its own callbacks.

The trap found during development: re-sending `ViewportCommand::Title` every
pass schedules another pass, which spun the loop at ~40% of a core with the
window sitting idle. Hence `last_title`. Anything that sends a viewport command
unconditionally will do the same thing.

## Autosave

Every mutation is written to a recovery snapshot immediately. A conventional
"autosave every five minutes" timer would have to wake the event loop while you
are doing nothing, which is exactly what the idle rule forbids — and it would
be pointless, because nothing changes while idle. So the snapshot is
event-driven, riding on `edit`, with one write at the end of that pass.
Measured idle CPU is unchanged at 0%.

- One slot, not a directory of timestamped candidates. Roughcut edits one
  project in one window, so a second slot could only ever be a decision to put
  in front of the user.
- Deleted the moment the work is safe: on save, on opening another project, or
  on a clean exit with nothing unsaved.
- Kept when you quit with unsaved changes. Next launch offers it once — `⏎` to
  recover, `Esc` to discard. Recovering restores the work *unsaved*, so nothing
  is overwritten behind you.
- While that prompt is up, key dispatch pauses and the snapshot is not
  overwritten, so nothing can destroy the work before you have answered.

**There is deliberately no "are you sure you want to quit?" dialog.** Reliable
recovery makes it unnecessary, and a confirmation you dismiss every time you
quit on purpose trains you to dismiss it.

## Keyboard ownership

Roughcut owns the keyboard, and `update` surrenders egui's widget focus every
pass to keep it that way.

This is not incidental. `Context::wants_keyboard_input()` is
`memory.focused().is_some()` — true for *any* focused widget, not just a text
field. Guarding key dispatch on it meant that pressing `Tab` parked focus on
the scrub bar and silently disabled every binding in the application until
something was clicked. There are no text widgets anywhere in the app; file
names come from native dialogs. If one is ever added, the guard must come back
as a test for a *text* widget specifically.
