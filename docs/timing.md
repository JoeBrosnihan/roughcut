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

## The scrub bar shows the audio

A clip's loudness envelope is the cheapest way to find the moment something
happens in it. `waveform.rs` decodes the audio alone — `-vn`, mono, 8 kHz —
and reduces it to 2048 peak levels, one byte each. Measured on real phone
footage: **45–383 ms per clip**, and 2 KiB to keep. That ratio is why these are
cached on disk like the sheets, and why, unlike the sheets, they are never
evicted: a thousand-clip bin would hold 2 MB.

Peaks, not RMS — the question is "where is something loud", and averaging
flattens exactly the transients that answer it. Each clip is scaled to its own
loudest moment, with a floor at about -40 dBFS. Both halves of that matter: a
clip measured here peaks at **-39.2 dBFS**, so scaling to full scale would have
drawn it as a flat line, while scaling with no floor at all would draw room
tone as though it were a conversation.

## Why there is no low-resolution stand-in while seeking

There was, briefly. While a seek was outstanding the monitor drew the frame
being sought from the scrub sheet, on the theory that a blurry frame now beats
a sharp one later. It was removed the day it shipped, because the frame it drew
was the wrong one: a sheet holds 112 samples of a clip, so on anything longer
than a minute the nearest tile is *seconds* from the playhead. A confident
picture of the wrong moment is worse than a stale picture of the right one.

The real answer is a low-resolution decode at the actual position, which is
what a proxy is. Seeking a 540p proxy was measured at 26 ms against 180 ms on
4K — see [verification.md](verification.md) — and proxies are now reachable
from the File menu rather than only from `settings.json`.

Keyframe-only seeking was considered as a cheaper middle ground and rejected on
measurement: keyframes in the phone footage here land about a second apart,
which is twice the error anyone would accept from a scrub.

## A photograph has no duration, so it is given one

Everything else in the bin is measured. A still is not: ffprobe reports no
frame count, no duration, and a frame rate it made up — a PNG comes back as
25/1. So `import::add_clip` supplies both. The clip takes the project's rate,
is made an artificial **60 seconds** long, and arrives with the first **10
seconds** already marked, which is what `A` appends. The minute is not
arbitrary: an edge drag has to have something to drag against, and a still with
no footage behind it would otherwise be the one clip in the project that can be
stretched without limit.

Past that point nothing knows it is a photograph. The length is in profile
frames like every other length, so marking, timeline arithmetic and the EDL are
unchanged. Three places do know:

- **The MLT writer** emits MLT's image producer, not an `avformat` chain.
  `avformat` reads a photo as a stream of exactly one frame and will not hold
  it. Verified end to end: a cut of 90 frames of video, 60 of photo and 30 more
  of video renders to exactly 180 frames.
- **No proxy and no scrub sheet** is built for one. There is nothing to
  transcode and every tile would be the same picture.
- **Playback carries the playhead across it on wall clock.** mpv holds the
  still for exactly the length the EDL gives it — measured to the frame — but
  reports no `time-pos` at all while it does, because there is no new frame to
  timestamp. Left alone, the playhead would freeze for ten seconds and then
  jump. Whole frames only, remainder carried, so it cannot drift.

PNG, JPEG and HEIC were all checked through ffmpeg, melt and mpv. HEIC is worth
naming because it is what phones actually produce, and because by codec it is
indistinguishable from video — it is an HEVC frame in an MP4-family container.
Stills are therefore recognised by extension, which is unambiguous.

## Reading a clip instead of watching it

Skimming two hours of footage by picture means dragging past four and a half
seconds per pixel. Reading it means finding the moment somebody says the thing.
Every clip with audio is transcribed as it is imported, and `T` shows the
result as a document: click a word to go there, drag across a sentence to
select it.

**The selection is not a new verb.** It sets the same `in` and `out` that `I`
and `O` do, so the scrub bar shades it, `A` appends it, and dragging its
handles trims it. Nothing else in the application had to learn that text
exists.

Transcription is whisper.cpp, discovered on disk exactly as ffmpeg and melt
are, running entirely on this machine — the audio of somebody's family holiday
is not something to upload in order to find out where the laughing is. Measured
on an RTX 3080 with `medium.en`: **24x realtime**, so a seven-minute clip takes
18 seconds and a two-hour one about five minutes. Results are cached, and the
second open of the same two clips restored them in **286 ms** against 23
seconds to produce.

### Two things measurement decided

**Word timestamps come from `-ml 1 -sow`, not from tokens.** whisper.cpp will
emit token-level timestamps and they cannot be used: on real speech they come
back non-monotonic, one word here claiming to start at 24120 ms and end at
16410. Asking instead for one word per *segment* routes every word through the
segment timing path, which was checked by cutting each range and transcribing
it again — the words came back the same.

**A cut is padded by 250 ms before and 400 ms after.** Landing exactly on the
first consonant clips it, and the listener hears somebody already talking. The
head gets more air than the tail because a late start is much more noticeable
than an early end.

Whisper also emits `>>` for a change of speaker, `[BLANK_AUDIO]`, and `♪` for
music. All of them are dropped: they would otherwise be selectable and cuttable
as though they were words. Paragraphs break on punctuation, on two seconds of
silence, and — because singing is transcribed without a full stop anywhere — at
forty words regardless.

While a clip plays, the word being spoken is highlighted and the document
follows it, but only once the word has actually scrolled out of sight.
Re-centring every frame would keep the whole page sliding, which is harder to
read than the thing it is trying to help you read.

The clip you have open jumps the transcription queue. Import order is the right
order to work through a bin, and entirely the wrong one when you have just
opened the hundredth clip: nothing is cancelled or re-run, the job that is
already waiting simply goes first.

## Sound with no picture

Audio tracks hold music, voiceover and effects. A video clip keeps its own
sound welded to its picture; these are for everything else.

They differ from the video track in exactly one structural way, and it is the
one that matters. The video track is gapless, so a clip position is the sum of
the lengths before it — derived, never stored, and therefore incapable of
desynchronising. Audio has gaps: an effect sits at the moment it happens and
there is silence either side. So an audio item carries its own `start`, which
is a stored position, which is a thing that can go wrong.

Five edits shift downstream positions — `insert_at`, `ripple_delete`,
`trim_head`, `trim_tail`, `trim_edge` — and each one routes through a single
`ripple_audio`, rather than five call sites each remembering. Whether it does
anything is the **Ripple all tracks** toggle, off by default, matching Shotcut.
That is the one mode worth its keep here, because there is genuinely no right
answer: an effect pinned to a door slam should follow a cut; a music bed should
not lose four seconds from its middle because a shot was shortened.

### The preview plays one pre-mixed bed

An mpv EDL concatenates; it does not mix. Two things follow.

First, the audio tracks are flattened to a single WAV by ffmpeg — `atrim`,
`adelay`, `amix` — and mpv plays that alongside the picture. One decoder rather
than one per piece: a project with fifty effects would otherwise ask mpv for
fifty, running for the whole timeline. Measured at **444 ms for twenty-four
pieces across ten minutes**, and redone only when a signature of the tracks
actually changes.

Second, mixing that bed with the clip audio is `lavfi-complex`, and two things
about it had to be measured rather than assumed:

- **It must be set after the file loads.** As a startup option, mpv silently
  fails to load the file at all.
- **The graph cannot name `aid1` and `aid2` blindly.** Every iPhone clip
  sampled here carries a second four-channel `apple_apac` spatial track that
  mpv cannot decode. Taking the second audio track picks that instead of the
  bed, and the graph collapses to "Audio: no audio". The first *internal* track
  is the clip; the *external* one is the bed.

Verified by frequency rather than by ear: a 300 Hz clip mixed with a 1000 Hz
bed came out at -17.1 dB and -17.8 dB, both present. `amix` is given
`normalize=0`, or adding a second sound would quietly halve the first.

### Export

An audio track is an ordinary playlist with `hide="video"` on its entry in the
tractor, gaps written as `<blank>`, and a `mix` transition of its own — without
that transition the track is in the XML and silent in the render. The programme
runs as long as the longest track, so a bed outlasting the last shot is not cut
off. Both are covered in `tests/audio_tracks.rs`, which renders through melt and
measures the tones that come back.

## Variable frame rate

Phones shoot it constantly, and it breaks the one agreement everything else
rests on: that the frame count and the frame rate say the same thing about when
a clip ends. Playback drives mpv in seconds, seeking converts back, and MLT is
handed frame numbers at the profile rate — all of which assume
`frames / rate == duration`.

`r_frame_rate` on such a file is the *nominal* rate, and `nb_frames` is the real
count, and the two do not reconcile. One clip in a test folder reports 24 fps
and 8653 frames across 319 seconds; that is 27 fps really, and believing the
count makes Roughcut think the clip runs 41 seconds longer than it does. The
symptom is timeline playback ending on mpv running out of file, tens of seconds
early, and rolling on to the next cut.

So the container's frame count is used only where it agrees with the duration,
and the duration wins wherever they disagree. The nominal rate is kept — it is
the sane, round number, and inventing a profile of 5191800/191537 fps to
preserve every frame would be worse everywhere else — and the clip is flagged
`VFR` in the bin, since its positions are only as exact as its average rate.

Opening a project re-probes every clip, because a project outlives the code
that wrote it: a clip measured wrongly by an older Roughcut stays wrong on disk
otherwise. Where the file disagrees with what was recorded, the clip is
corrected and any marks or cuts pointing past the end are pulled back to fit,
which is said plainly rather than done silently.

## Playing the timeline

The timeline is handed to mpv as an EDL — one virtual stream describing the
whole cut list — rather than played one clip at a time. Loading each item as it
came up put a visible pause at every join, and no amount of care avoids that
while mpv only knows about one file; with an EDL it can open the next segment
before the current one ends. Measured across three real clips the joins stall
for 0 ms.

It also removes the arithmetic. `time-pos` in an EDL *is* the timeline
position, so there is no per-item bookkeeping and nothing to convert.

The EDL's signature is part of its file name. Rewriting one path in place would
leave mpv holding the previous cut, since nothing about the name would have
changed; a new cut is simply a new path. Segment offsets are seconds, obtained
by dividing profile frames by the profile rate — which is exact for
variable-rate sources too, because a clip's duration in profile frames is
defined to span its real running time.

## Rendering

Roughcut does not encode video and has no intention of learning how. `melt` is
MLT's own renderer, ships with Shotcut, and consumes exactly the XML this
program already writes, so exporting an MP4 is one child process.

It runs on a thread of its own rather than in the worker pool. Pool jobs are
background work nobody is waiting on: suspended when the window loses focus and
run at the lowest priority the OS offers. A render is the opposite of all three.

melt reports progress on a timer, not per frame, which has two consequences.
A short render can finish without a word — so the cancel flag is checked on a
poll rather than between messages, or a quiet render could not be cancelled at
all. And whether any given render reports anything depends on machine load, so
the streaming is proved against a recorded transcript in a unit test; the
integration test only reports what really happened.
