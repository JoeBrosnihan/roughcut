# Roughcut

A keyboard-driven assembly editor. Ingest a folder of video, review it fast,
mark in/out points, assemble an ordered sequence, trim it, and export a `.mlt`
project that Shotcut opens with every cut on the exact intended frame.

That is the entire product. No effects, no transitions, no audio mixing, no
titles, no second video track, and it cannot render video. Export is one-way:
Roughcut writes MLT, it never reads it. The value is in what it refuses to do.

## Design pillars

**1. Simplicity.** Every feature is a liability. Ambiguity resolves toward
removal. New options, modes and settings are the failure mode to watch for —
each one doubles the states the tool can be in, and Roughcut is only useful
while it stays a tool you can hold in your head.

**2. Performance.** The tool disappears when it is fast and intrudes the moment
it is not. Idle costs literally nothing. The budgets in
[docs/verification.md](docs/verification.md) are acceptance criteria measured
on real files, not aspirations.

Where they conflict, simplicity wins unless the cost is one you would feel on
every keypress.

## Status

| Target | State |
| --- | --- |
| `x86_64-pc-windows-msvc` | Built, run, measured |
| `x86_64-apple-darwin` | Type-checks clean; never linked or run |
| `aarch64-apple-darwin` | Type-checks clean; never linked or run |

Frame accuracy is verified end to end: the exported project is rendered through
MLT and compared frame by frame against the source, and Shotcut itself opens
the result with no complaints. See
[docs/verification.md](docs/verification.md).

## Quickstart

```
cargo run --release -- path\to\clip.mp4     # run it
cargo test --workspace                      # 89 tests
.\tools\promote.ps1                         # keep a stable copy
```

Needs `ffprobe` on `PATH` to import and **libmpv 2** to show video; both are
found automatically if Shotcut or mpv is installed. Files named on the command
line are opened at startup — a `.roughcut` file as a project, anything else as
media to import.

Press `?` in the app for the keyboard map, or read
[KEYS.md](KEYS.md). The core loop is `I` → `O` → `A`: mark in, mark out,
append to the timeline.

## Documentation

| Document | What is in it |
| --- | --- |
| [KEYS.md](KEYS.md) | The keyboard map. Generated from the code; a test fails if it drifts |
| [docs/building.md](docs/building.md) | Building per platform, external dependencies, libmpv, installing a stable copy |
| [docs/timing.md](docs/timing.md) | The frame-exact timing model, and the seek bug that measurement caught |
| [docs/mlt.md](docs/mlt.md) | The MLT export: how the schema was derived and what is emitted |
| [docs/design.md](docs/design.md) | Crate layout, video integration, autosave, undo |
| [docs/verification.md](docs/verification.md) | Measured performance, the test suite, what still needs a human |
| [docs/deviations.md](docs/deviations.md) | Every departure from the original brief, and why |
| [docs/licensing.md](docs/licensing.md) | Why MIT is safe, and the one thing that would change it |

## Layout

```
crates/roughcut-core   model, timing, timeline ops, undo, ffprobe, MLT writer
crates/roughcut-mpv    libmpv client + OpenGL render API bindings
crates/roughcut-app    egui application
```

`roughcut-core` knows nothing about egui, mpv or threads, so the parts that
must be correct are testable without a window or a GPU.

## Licence

MIT — see [LICENSE](LICENSE). No dependency is copyleft-only, and the external
tools are separate processes. The one caveat is that libmpv should be assumed
GPL, so do not ship it alongside a binary without reading
[docs/licensing.md](docs/licensing.md) first.

## Conventions

This README stays **under 100 lines**. Anything longer belongs in `docs/` with
a link from the table above.
