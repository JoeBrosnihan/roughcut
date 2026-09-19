# macOS local build

Roughcut has been built and run natively on Apple Silicon. The existing
OpenGL/libmpv integration works on the tested Mac without a renderer rewrite.
This is a local development setup, not a signed, self-contained release.

## Setup and launch

Install the Xcode Command Line Tools and a current stable Rust toolchain.
Install the media dependencies with native ARM64 Homebrew:

```sh
brew install mpv ffmpeg mlt
cargo run --locked --release --bin roughcut
```

Alternatively, build a Finder-launchable app:

```sh
./tools/build-macos.sh
open target/macos/Roughcut.app
```

Quit and reopen after rebuilding. The bundle still uses this Mac's Homebrew
or Shotcut dependencies. Its settings, autosave and caches are isolated under
`target/dev-config/`, and its log is `target/macos/roughcut.log`. The bundle
records the checkout's absolute config path; rebuild it if the checkout moves.
It is not intended to be copied to another machine as a standalone installer.

`target/release/roughcut-cli doctor` reports external tool discovery.
`ROUGHCUT_MPV` can point to a specific libmpv library when necessary. The app
and library must have matching architectures.

## Verified on Apple Silicon

Local verification on 2026-09-18 used macOS 26.6, Rust 1.98.1,
mpv 0.41.0, FFmpeg 9.0.1 and MLT 7.40.0.

- Native ARM64 debug and release builds link successfully.
- The app bundle launches, finds Homebrew tools, and uses the intended dev config.
- Command-I opens a native picker and imports a generated 1280×720, 30 fps
  H.264/AAC clip. Thumbnails and the waveform are visible.
- The embedded OpenGL monitor displays source and timeline playback. The log
  reports `hardware decode active: videotoolbox`.
- Mark-in/out, stepping/seeking and appending a range work through the GUI.
- Command-S saves a project through the native dialog; after quitting and
  relaunching, the recent-project menu reopens it with media and edits intact.
  Command-E exports MLT
  and renders MP4 through the GUI. The 0–90 inclusive cut produces 91 H.264
  video frames at 1280×720/30 fps, with AAC audio.
- All 333 workspace tests pass with real media dependencies installed. The
  frame-accuracy test renders and matches all 100 requested source frames.
  MP4 rendering, progress, cancellation, proxies, rotation, stills and audio
  integration tests run rather than skip for missing tools.

Homebrew's standard FFmpeg 9 omits `drawtext`, used by the frame-accuracy
fixture generator. The full test command is:

```sh
brew install ffmpeg-full
PATH="$(brew --prefix ffmpeg-full)/bin:$PATH" cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets
```

Clippy completes, but Rust 1.98.1 reports existing float-literal fallback
warnings in UI drawing code and two iterator/sort suggestions. This is not
a warning-free lint baseline. The initial Mac run also exposed a stale test
using Windows path separators and worker tests assuming suspension stops
transcription or that background work starts within 500 ms; these are fixed.

## Remaining gaps

- Screenshot paste remains Windows-only. Import saved image files instead.
- Child-process memory accounting is Windows-only, so the memory-budget hold
  is not effective on Mac. Worker concurrency and FFmpeg thread limits remain.
- Transcription needs a separately downloaded Whisper model in `models/`
  beside the discovered `whisper-cli`. There is no model-path setting yet.
  Transcription has not been verified on this Mac.
- Intel Mac runtime behavior, heavy 4K/HEVC/HDR footage, external-monitor
  scaling changes, and performance/battery benchmarks have not been checked.
- The exported MLT was rendered with Homebrew MLT; reopening it in Shotcut's
  Mac UI has not been checked. Homebrew MLT warns about the disabled
  `frei0r.cairoblend` transition from Shotcut's schema; tested renders still pass.
- Distributable packaging, bundled libraries, an app icon, signing and
  notarization remain separate release work.
