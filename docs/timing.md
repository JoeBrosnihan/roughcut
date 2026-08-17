# The timing model

This is the part most likely to produce a silently broken tool, so it is the
part with the most tests.

Everything is `i64` frame numbers in profile time. `in` and `out` are
**inclusive**, so a clip with `in=0, out=99` is 100 frames and every duration
is `out - in + 1`. Frame rates are exact rationals; `30000/1001` is never
rounded to `29.97` anywhere, including in the exported XML. Timecode exists
only as a display format, produced at the last moment and parsed immediately on
entry.

## Which rate and size the project uses

Two profiles, deliberately.

The **working profile** is the coordinate system frame numbers are quoted in
while you edit. It is derived from the whole bin — most common rate, ties to
the higher one; largest frame size, so nothing is upscaled — and it is
re-derived on every import for as long as it is still free to move. It stops
moving the moment you mark or cut anything, because from then on a frame number
records a decision you made while watching, and silently rescaling it would
move the cut. The first clip imported gets no special status: it is whichever
file the dialog happened to list first, and footage routinely arrives from
several phones at once.

The **export profile** is derived at export time from the clips actually cut
into the timeline. Footage left unused in the bin has no say in the shape of
the finished video. When it differs from the working profile the project is
re-expressed in it first — durations recomputed from each file's own native
frame count, and each `in`/`out` rescaled, treating `out + 1` as the boundary
that scales. In the ordinary case, where everything is at one rate, that
conversion is the identity and the export path is byte-for-byte what it was.

Size, sample aspect and colourspace can be deferred to export for free: none of
them can move a frame number. Only the rate can, which is why only the rate
needs the rescale.

A clip whose own rate differs from the working profile is accepted, flagged
with a persistent `FPS` badge in the bin and on its timeline blocks, and its
duration converted into working time. MLT will resample it and frame-exactness
for that clip is not guaranteed, which is what the badge is telling you.

## Clips that arrive on their side

Rotation is read from the display matrix at probe time and applied to the
reported frame size, so everything downstream — the profile, proxies,
thumbnails, mpv — works in display orientation. Right-clicking a clip and
rotating it rewrites that matrix in the file itself with a stream copy, so the
frame count and rate cannot move and marks made before the rotation still name
the same frames after it. Re-encoding was rejected: minutes per clip,
generation loss, and encoders are free to alter frame counts.

`crates/roughcut-core/src/time.rs` is the only module allowed to convert
between frames and anything else.

## One thing measurement changed

mpv's `time-pos` is advisory, as the brief says. What the brief does *not* say
is how to aim a seek, and getting it wrong is silent — the picture is simply
one frame away from the number you are marking, forever.

Three targeting strategies were measured against real files with
`cargo run -p roughcut-mpv --example step_probe`:

| Strategy | Result |
| --- | --- |
| Middle of the frame's interval | **Wrong on every frame** — lands one frame late |
| The frame's exact timestamp | Correct, but a float a hair high tips to the next frame |
| A fifth of a frame *before* the timestamp | Correct, with margin either side |

Roughcut uses the third. The first is what shipped initially, and it was wrong:
every mark would have been made one frame later than the picture on screen.
`frame_to_seek_seconds` carries the reasoning, and
`seek_target_brackets_the_wanted_frame` locks down the property that actually
matters — the target must sit strictly between frame N-1's timestamp and frame
N's, so "first frame at or after the target" can only resolve to N.

That example binary is kept precisely because the finding is not obvious and
would be easy to undo by accident.

## Stepping

Single-frame stepping forward uses mpv's `frame-step`, which decodes exactly
one more frame. Reaching the same frame with `seek absolute+exact` costs a
re-decode from the start of the GOP: measured at 120–180 ms on 4K H.264 and
getting steadily worse the further into the GOP you are, against a 33 ms
budget.

The brief describes stepping as "increment the integer counter and seek". The
counter is still the sole source of truth — that is the part that matters — but
the seek is skipped when a step will do. Backward steps still seek, because
mpv's `frame-back-step` needs a backward decode cache that Roughcut's
`cache=no` disables and was measured to be a silent no-op.

## The safety net

After any position change, if mpv reports a frame other than the one requested,
Roughcut logs it and issues one corrective seek — once per target, so a frame
that stubbornly refuses to match cannot start a seek loop.

In current measurements it never fires: 0 corrections over hundreds of steps on
both 4K long-GOP and 1080p all-intra material. It exists so that if the
assumption behind `frame-step` ever stops holding, the result is a logged
correction rather than a silently wrong cut.

## Thumbnails, in two passes

Every clip gets a one-frame **poster** before any clip gets its **scrub
sheet** — 112 frames, one per pixel of the tile's width, so a single pixel of
pointer movement lands on a frame that was really extracted. The bin is
unusable until its pictures appear, and building scrub data for the first clip
while the fortieth is still a grey rectangle gets that backwards.

Priority alone would not be enough. It decides what comes off the queue next,
not who is free to take it, so a reserved thread outside the pool takes nothing
but interactive work — a rotation the user just asked for would otherwise sit
behind four multi-second transcodes. It sleeps on a condvar essentially always,
so reserving it costs nothing.

Sheets are cached on disk and evicted from memory least-recently-seen, capped
at 48 resident; posters are small enough to keep for every clip. A clip
scrolled out of sight for long enough loses only the ability to scrub until the
cache hands it back.
