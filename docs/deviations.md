# Deviations from the brief

The brief asks for substitutions to be flagged. These are all of them.

**1. The status bar was removed.** §8 specified timecode, frame, fps and clip
count along the bottom. Every field was either already on screen — the position
readout sits under the scrub bar, durations on each timeline block, the focused
region behind an accent border — or static for the life of the project. What
replaced it is an alert bar that occupies zero height unless there is a warning
or a result to report. Requested by the user, and consistent with pillar 1.

**2. Forward single-frame stepping uses mpv's `frame-step`**, not §5 rule 6's
"increment the counter and seek". The integer counter remains the sole source
of truth and a corrective-seek net verifies every move; the literal reading
cost 120–180 ms per step on 4K and got worse further into the GOP, failing §3
outright. See [timing.md](timing.md).

**3. libmpv is loaded at runtime, not linked at build time** (§4). Still the C
client API plus the render API, as specified. This keeps `cargo build` working
without the mpv development package and degrades a missing library to "no
monitor" rather than "will not start".

**4. `<chain>` instead of `<producer>`** in the export. §12 shows `<producer>`
in its skeleton but also says to match a real Shotcut file, and current Shotcut
writes `<chain>` for avformat sources. See [mlt.md](mlt.md).

**5. Background workers suspend dispatch on focus loss, but an in-flight
`ffmpeg` job runs to completion** (§3). A transcode cannot be paused mid-write
without leaving a corrupt file behind.

**6. §10's proxy command upscales sources shorter than 540p.** The command line
is given verbatim in the brief and is used verbatim; worth knowing if you feed
it SD material.

**7. macOS type-checks but has never been linked or run.** Both
`x86_64-apple-darwin` and `aarch64-apple-darwin` pass
`cargo check --workspace --all-targets`. aarch64 was added beyond the brief's
Intel-only requirement because most Macs are no longer Intel.

**8. Autosave is not in the brief at all.** It was added because §11's
save-on-demand model loses everything between saves. It is event-driven
specifically so it does not break §3's idle budget. See
[design.md](design.md).

**9. `ROUGHCUT_CONFIG_DIR` and `tools/promote.ps1` are not in the brief.** They
exist so a stable installed copy and an actively changing source tree can
coexist without sharing settings or an autosave slot. See
[building.md](building.md).

**9. The profile is not fixed by the first clip imported** (§9). It tracks the
whole bin until the first mark or cut pins it, and the *export* profile is
derived separately from the clips actually used. Requested by the user, whose
footage routinely arrives from several phones at once, where the first file the
dialog lists is a meaningless anchor. See [timing.md](timing.md).

**10. Rotating a clip edits the user's original file.** Nothing else in
Roughcut does. It is a display-matrix rewrite done as a stream copy — no
re-encode, no generation loss, no change to frame count or rate — and it is the
only fix that survives leaving the program. Requested by the user.

**11. Roughcut renders MP4 as well as writing MLT.** The brief, and this
README until now, said rendering was Shotcut's job. It still does none of the
encoding itself: `melt` is MLT's own renderer, ships with Shotcut, and reads
exactly the XML already being written, so the whole feature is one child
process with a progress bar. Requested by the user.

**12. The export profile is chosen in a dialog**, not silently. §9 fixed the
profile at import. It is now suggested from the clips actually used and
overridable at export, because footage off several phones has no single obvious
answer. Requested by the user. See [timing.md](timing.md).

**13. The software-decode warning is gone** (§3). The brief made hardware
decode mandatory and required a standing notice when it is unavailable. mpv
leaves `hwdec-current` unset until it has actually built a decoder, so the
notice appeared briefly on every clip load and was wrong nearly every time it
appeared. Worse, it lived in a panel that took height only when it had
something to say, so each false alarm reflowed the video, the bin and the
timeline twice.

The condition is still logged, once, at the point the file loads, which is
where anyone diagnosing slow playback will look. The alert bar that remains
floats over the empty strip below the timeline blocks instead of occupying a
panel, so no message can move the rest of the application again. Requested by
the user.
