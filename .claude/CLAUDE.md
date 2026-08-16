# Roughcut — project instructions

## The README is for people using Roughcut, not building it

It describes what the tool does, what you need to run it, and how to drive it.
Nothing else. Toolchains, architecture, measurements and rationale live in
`docs/` and are reached from the one "Building it, or changing it" link.

**Keep it under 50 lines.** Check after any edit:

```
wc -l README.md
```

If a change would push it over, the change belongs in `docs/` with a link, not
in the README. A README nobody finishes reading communicates nothing — the same
reason the application has no permanent status bar.

`docs/building.md` carries the index of the other documents. Add new documents
there, not to the README.

## Design pillars

In order: **simplicity**, then **performance**. Where they conflict, simplicity
wins unless the cost is one the user would feel on every keypress.

Lead with what can be left out. Prefer deleting a control to adding a mode:
every new option doubles the states the tool can be in. This applies to the
documentation as much as the code.

The full statement is in `docs/design.md`.

## Two things that are easy to break

**Idle CPU must stay at 0%.** The event loop is `ControlFlow::Wait`. Never add
an unconditional repaint, a polling loop, or a timer that runs while nothing is
happening. Sending a viewport command every pass has exactly this effect and
once cost 40% of a core. Measure after any change to the update loop.

**Positions are `i64` frame numbers, everywhere.** `in` and `out` are
inclusive; every duration is `out - in + 1`. Frame rates are exact rationals.
Timecode is a display format produced at the last moment and never used for
arithmetic. `docs/timing.md` explains what goes wrong otherwise.

## Verifying

`cargo clippy --workspace --all-targets` and `cargo test --workspace` should
both be clean before anything is promoted. A green suite is not enough for
behaviour that only appears at runtime — playback, input, GL — so exercise it
in the running application as well.
