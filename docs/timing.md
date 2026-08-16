# The timing model

This is the part most likely to produce a silently broken tool, so it is the
part with the most tests.

Everything is `i64` frame numbers in profile time. `in` and `out` are
**inclusive**, so a clip with `in=0, out=99` is 100 frames and every duration
is `out - in + 1`. Frame rates are exact rationals; `30000/1001` is never
rounded to `29.97` anywhere, including in the exported XML. Timecode exists
only as a display format, produced at the last moment and parsed immediately on
entry.

The profile — rate, size, sample aspect, colourspace — is fixed by the first
clip imported and never changes afterwards. A later import at a different rate
is accepted, flagged with a persistent `FPS` badge in the bin and on its
timeline blocks, and its duration converted into profile time. MLT will
resample it and frame-exactness for that clip is not guaranteed, which is what
the badge is telling you.

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
