# Licensing

Roughcut is **GPL-3.0-or-later**. See [LICENSE](../LICENSE) for the text and
[NOTICES.md](../NOTICES.md) for third-party components.

*This is an engineering summary, not legal advice.*

## Why GPL rather than MIT

Nothing in the dependency tree forces it — every crate is permissive, and MIT
was the original choice. GPL was picked for a practical reason: **it lets a
release bundle everything into one download.**

Playback needs libmpv, and the readily available Windows builds are GPL. Under
MIT, shipping `libmpv-2.dll` next to the executable would distribute a combined
work and pull the whole release under the GPL anyway. The alternatives were:

1. Ship no libmpv and make every user install mpv themselves.
2. Build mpv from source with `--enable-lgpl` to get an LGPL libmpv, which
   permits bundling from an MIT project — a real toolchain project on Windows.
3. Adopt the GPL and bundle the prebuilt binary.

Three is the least work for the most convenient result, and it costs nothing
that matters for a tool published for people to use.

## Why version 3 specifically

**GPLv2 is not available.** `winit`, `glutin` and `ab_glyph` are Apache-2.0
only, with no MIT alternative, and they are unavoidable — they are the window
and OpenGL layer. Apache-2.0 is compatible with GPLv3 but **not** with GPLv2,
because its patent-termination and indemnity clauses count as further
restrictions under v2.

mpv is GPLv2-*or-later*, so it is happy being used as v3.

Verify the Apache-2.0-only set with:

```
cargo metadata --format-version 1 --all-features
```

## What this means in practice

- Anyone may use, modify and sell Roughcut.
- Anyone **distributing** it, modified or not, must provide source under the
  same licence. Private use and modification carry no obligation.
- A binary release must ship the licence text, the notices, and a way to obtain
  the Corresponding Source of the GPL components it bundles — for libmpv, a
  link to the exact upstream build. See [NOTICES.md](../NOTICES.md).

## Relicensing later

The copyright holder can release the same code under any terms. If a
commercial user ever needs a permissive copy, an MIT-licensed version can be
offered alongside the GPL one — standard dual licensing.

This works **only while the copyright is undivided**, which is why
contributions come with an explicit grant, agreed *before* the first patch
lands: [CONTRIBUTING.md](../CONTRIBUTING.md) states that a submission is
GPL-3.0-or-later and that the maintainer may also distribute it under other
terms. That keeps relicensing a decision rather than a negotiation.
Retrofitting such a grant would mean tracking down every past contributor,
so the policy was in place before any patch was accepted.

Note also that a release already published under the GPL stays GPL; it cannot
be retracted. Future releases may carry different terms.

## What does not affect the licence

`ffprobe`, `ffmpeg` and `melt` run as **separate processes** — Roughcut hands
them argv and reads their output. Running a program has never imposed its
licence on the caller, and none of them is shipped.

The exported `.mlt` is data. MLT's licence governs MLT, not files describing a
timeline.
