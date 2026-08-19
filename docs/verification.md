# Verification

`cargo test --workspace` — 159 tests, no warnings, clippy clean on Windows and
type-checking clean on both macOS targets.

## Frame accuracy

`crates/roughcut-core/tests/frame_accuracy.rs` implements the brief's required
check:

1. Builds a 300-frame video at 30000/1001 with each frame's number burned in.
2. Imports it, marks in at 100 and out at 199, appends to the timeline.
3. Exports MLT XML and asserts `in="100" out="199"` and a 100-frame result.
4. If `melt` is available, **renders the exported project losslessly and
   compares all 100 frames** against frames decoded straight from the source,
   identifying any mismatch by content — so a failure reads "timeline frame 0:
   expected source frame 100, got 99 (off by -1)" rather than "pixels differ".

Rendering the whole timeline rather than two stills also catches drift in the
middle. A negative control asserts the comparison can tell adjacent frames
apart, so the pass cannot be vacuous. With Shotcut installed the test reports:

```
VERIFIED with melt: all 100 rendered frames match source frames 100..=199 exactly.
```

`proxy_timing.rs` covers the rule that a proxy must match its source frame for
frame, including a negative case where a deliberately resampled proxy is
rejected.

## Shotcut compatibility

Confirmed. Shotcut opens the export with no errors, and its own
`MltXmlChecker` — the component that detects and repairs malformed MLT XML —
returns an empty error string. Details and the log evidence are in
[mlt.md](mlt.md).

`acceptance/` holds a ready-made pair for eyeballing it yourself:

| File | What it is |
| --- | --- |
| `frames-000-299.mp4` | 300 frames at 30000/1001, each showing its own number |
| `marked-100-199.mlt` | The export of that clip marked in at 100, out at 199 |

Open the `.mlt` in Shotcut: one clip, 100 frames, first frame reads **100**,
last reads **199**. Regenerate with:

```
set ROUGHCUT_KEEP_OUTPUT=1
cargo test -p roughcut-core --test frame_accuracy -- --nocapture
```

## Measured performance

Release build, Windows 11, NVIDIA GPU with `nvdec` active. Numbers come from
the instrumentation in `monitor.rs`; set `RUST_LOG=roughcut=debug` and
`ROUGHCUT_LOG_FILE=<path>` to reproduce them. A summary line is written on
exit.

| Requirement | Target | Measured |
| --- | --- | --- |
| CPU while idle, focused, no input | 0% | **0.00%** over 10 s |
| CPU while idle, 50 clips imported | 0% | **0.00%** |
| Cold start to interactive | < 1.0 s | **356 ms** |
| Step forward one frame, 4K H.264 GOP 250 | < 33 ms | **33.7 ms** median |
| Step forward one frame, 1080p all-intra | < 33 ms | **33.6 ms** median |
| Step backward one frame, 1080p all-intra | < 33 ms | **7.4 ms** median |
| Step backward one frame, 4K H.264 GOP 250 | < 33 ms | **180 ms** median — misses |
| Step backward one frame, via 540p proxy | < 33 ms | **26.3 ms** median |
| Seek to arbitrary frame, 4K H.264 | < 200 ms | **180 ms** median, 342 ms worst |
| RSS, 50 clips imported with thumbnails | < 300 MB | **258 MB** |
| Hardware decode | Mandatory | **nvdec**; logged, not shown — see [deviations.md](deviations.md) |

Two need explanation:

- **33.7 ms forward stepping is a floor, not a cost that can be shaved.** mpv's
  `frame-step` plays exactly one frame and then pauses, so it takes one frame
  period — 33.37 ms at 30000/1001 — by construction. Crucially it does not grow
  with position in the GOP, which the seek-based alternative did.
- **Backward stepping on 4K long-GOP material misses the budget**, at 180 ms.
  Going backwards one frame requires a fresh exact seek that re-decodes from
  the keyframe. The fix is the one the brief already provides: proxies bring it
  to 26 ms. This is the honest reason proxies exist, and it is worth knowing
  before deciding whether to keep them.

## Known gaps

- **Photograph playback is verified below mpv, not through the interface.**
  `cargo run -p roughcut-mpv --example still_probe -- clip.mov photo.heic`
  shows that mpv holds a still in an EDL for exactly the length it is given,
  and `stills_in_the_cut.rs` shows that a cut containing one renders to the
  right number of frames. What has not been watched is the playhead crossing a
  photograph in the running application.
- **`J` shuttle speeds are aspirational on heavy media.** Reverse playback is
  timer-driven backward seeks; at 180 ms each on 4K, even 1× cannot keep up, so
  the `8x` indicator does not mean what it says. It is honest on 1080p and on
  proxies.
- **macOS has never been run.** See [building.md](building.md).
