# Roughcut

Assemble a rough cut fast, then finish it in Shotcut.

Point Roughcut at a folder of video, skim it, mark the good bits, and build an
ordered sequence. Export a `.mlt` project that Shotcut opens with every cut
exactly where you put it.

It is deliberately small: no effects, transitions, audio mixing, titles, second
video track, or rendering. Those are Shotcut's job — this is the part before
them, which is mostly watching and choosing.

## What you need

Windows 10 or 11, plus **[Shotcut](https://shotcut.org)** (it supplies `ffprobe`
and `ffmpeg`, which Roughcut finds on its own — and you want it anyway to finish
the edit) and **[mpv](https://mpv.io)** for playback. Without mpv everything
still works except the picture.

## Using it

Drop files on the window, or press `Ctrl+I`. The whole loop is three keys:

| Key | Does |
| --- | --- |
| `I` | mark where a good bit starts |
| `O` | mark where it ends |
| `A` | append it to the timeline |

Then repeat. `Space` plays, `←` `→` step a frame, `Tab` moves between the source
and the timeline. `S` splits and `X` deletes and closes the gap — Shotcut's keys.
Hover a thumbnail to skim that clip; click to open it at that moment. Drag clips
onto the timeline, or along it to reorder; drag the ruler above them to scrub.
Right-click a clip to rotate one that came off a phone sideways.
**Press `?` for the full keyboard map**, or read [KEYS.md](KEYS.md).

`Ctrl+E` exports a `.mlt`, sized and timed from the clips you used. Open it in
Shotcut and finish there; the trip is one-way, so treat it as a handoff. Your
work saves itself continuously, and is offered back after a crash.

## Building it, or changing it

Start with [docs/building.md](docs/building.md), which indexes the rest.

## Licence

**GPL-3.0-or-later** — see [LICENSE](LICENSE), [NOTICES.md](NOTICES.md) and
[docs/licensing.md](docs/licensing.md). Issues welcome; code contributions are
not currently accepted.
