# Building and installing

The README is for people using Roughcut. This file, and the ones it indexes,
are for people building or changing it.

| Document | What is in it |
| --- | --- |
| building.md | This file: toolchain, dependencies, libmpv, stable vs dev |
| [timing.md](timing.md) | The frame-exact timing model, and the bug measurement caught |
| [mlt.md](mlt.md) | The MLT export: how the schema was derived, what is emitted |
| [design.md](design.md) | Design pillars, crate layout, video integration, autosave |
| [verification.md](verification.md) | Measured performance, the test suite, known gaps |
| [deviations.md](deviations.md) | Every departure from the original brief, and why |
| [licensing.md](licensing.md) | Why GPLv3, and what it means for releases |

## Status

| Target | State |
| --- | --- |
| `x86_64-pc-windows-msvc` | Built, run, measured |
| `x86_64-apple-darwin` | Type-checks clean; never linked or run |
| `aarch64-apple-darwin` | Type-checks clean; never linked or run |

## Toolchain

Requires a stable Rust toolchain. The workspace pins `eframe` 0.33, whose MSRV
is 1.88.

```
cargo build --release
cargo test --workspace
cargo run --release -- path\to\clip.mp4
```

Files named on the command line are opened at startup: a `.roughcut` file as a
project, anything else as media to import.

## External dependencies

Nothing is vendored into the build. Three external pieces are looked up at
runtime, on `PATH` first and then in well-known locations — including a Shotcut
installation, which bundles all three.

| Tool | Needed for | If missing |
| --- | --- | --- |
| `ffprobe` | Import — frame rates, durations, stream indices | Blocking dialog on first import, offering to locate it |
| `ffmpeg` | Bin thumbnails and proxy generation | Thumbnails and proxies are skipped |
| `melt` | The frame-accuracy test only | That test degrades to XML assertions and says so |

Paths can be overridden in `settings.json` (see *State locations* below).

## libmpv

Playback needs **libmpv 2**. It is opened at runtime with `LoadLibrary` /
`dlopen` rather than linked at build time, so `cargo build` works on a machine
without the mpv development package, and a missing library degrades to "no
monitor, everything else works" instead of failing to start.

Search order:

1. `ROUGHCUT_MPV` — full path to the library, if set
2. Next to the executable
3. `Contents/Frameworks`, when running from a macOS app bundle
4. `vendor/mpv/` at the workspace root, where a development copy is kept
5. The OS loader's own search path

On Windows, `shinchiro/mpv-winbuild-cmake` publishes `mpv-dev-x86_64-*.7z`;
extract `libmpv-2.dll` to one of the above. On macOS, `brew install mpv`.

## macOS

```
brew install mpv ffmpeg
cargo run --release
```

Build for the architecture of the machine you are on — `aarch64-apple-darwin`
on Apple Silicon, `x86_64-apple-darwin` on Intel. The application and libmpv
must be the same architecture; an x86_64 build under Rosetta cannot load
Homebrew's arm64 `libmpv.2.dylib`.

### What macOS still needs

Both Darwin targets pass `cargo check --workspace --all-targets`, which
compiles every macOS `cfg` branch — library discovery, the OpenGL framework
loader, thread QoS, config paths — and type-checks the whole app against
macOS's winit and eframe. That guarantees nothing is *missing*. It does not
link, and none of the following has been exercised even once:

- That it links at all — that needs the Apple SDK.
- **mpv's OpenGL render API on Apple's GL.** Apple deprecated OpenGL and caps
  it at 4.1. Everything Roughcut uses is well inside that (`glBlitFramebuffer`
  is GL 3.0), but "should be fine" is not "was tried".
- **`videotoolbox` hardware decode.** `hwdec=auto-safe` should select it; the
  alert bar will say so if it does not.
- **Retina scaling.** The render target is sized from `viewport_in_pixels()`,
  which is physical pixels, so a 2× display ought to be correct by
  construction — again, untested.
- Native file dialogs, and the app-bundle layout.

Expect the first run on a Mac to need fixing. Nothing on that list is
structural.

## stable and dev

Roughcut exists in two forms, and it is worth being precise about which one is
meant:

| | **stable** | **dev** |
| --- | --- | --- |
| What | The copy you edit with | The source tree |
| Where | `%LOCALAPPDATA%\Programs\Roughcut` | `target\release\` |
| Run by | Start Menu, or the exe directly | `cargo run --release` |
| Changes | Only when you **promote** | Every build |
| Settings | `%APPDATA%\Roughcut\` | `target\dev-config\` |
| Identified by | `VERSION.txt`, and a git tag | Whatever is checked out |

**Promoting** is the act of making the current build the stable one:

```
git tag -a v0.2.0 -m "what changed"
.\tools\promote.ps1
```

Tag first, so stable is always recoverable from source rather than only
existing as a binary. Rolling back is then `git checkout v0.1.0` followed by
another promotion.

`cargo build` overwrites `target/release/roughcut.exe`, which is exactly why
stable lives outside the repository:

That builds release, puts `roughcut.exe` and `libmpv-2.dll` into
`%LOCALAPPDATA%\Programs\Roughcut`, writes a `VERSION.txt` recording what and
when, and adds a Start Menu shortcut. Nothing there changes until you run the
script again — not on `cargo build`, not on `cargo clean`, not on a broken
commit. The copy is self-contained: libmpv sits beside the executable, which is
the first place Roughcut looks, so it reads nothing from the source tree
(verified by inspecting the running process's loaded modules).

The binary is swapped in via a rename, so an install that fails partway cannot
leave a broken executable where a working one used to be.

## State locations

Both copies would otherwise share one settings file and one autosave slot. A
development build could then overwrite the settings of the copy you rely on, or
leave a half-finished recovery snapshot that the stable one offers you on next
launch.

So the config location is overridable with `ROUGHCUT_CONFIG_DIR`, and
`.cargo/config.toml` points anything launched through cargo at a scratch
directory:

| How you run it | Settings and autosave |
| --- | --- |
| `cargo run` / `cargo test` | `target/dev-config/` |
| The installed copy | `%APPDATA%\Roughcut\` (`~/Library/Application Support/Roughcut/` on macOS) |

Nothing to remember and nothing to switch. Press `?` in either build to see
which binary and which config directory it is using.

## Environment variables

| Variable | Effect |
| --- | --- |
| `ROUGHCUT_MPV` | Full path to libmpv, overriding discovery |
| `ROUGHCUT_CONFIG_DIR` | Where settings and the autosave snapshot live |
| `ROUGHCUT_LOG_FILE` | Write the log here — the only way to see it from a release build, which has no console |
| `RUST_LOG` | Log filter. The bin target is `roughcut`, so use `roughcut=debug` |
| `ROUGHCUT_REGEN_KEYS` | Rewrite `KEYS.md` from the key table when running its test |
| `ROUGHCUT_KEEP_OUTPUT` | Keep the frame-accuracy test's working files instead of deleting them |

## The executable's icon

`assets/icon.ico` is stamped into the Windows binary by
`crates/roughcut-app/build.rs`, which shells out to the Windows SDK's `rc.exe`
rather than pulling in a crate whose job is to locate `rc.exe`. No SDK means no
icon and a build warning, never a failed build. `assets/icon.png` is the same
artwork, compiled in and handed to the window at startup — the embedded
resource covers Explorer and the shortcut, not the running window.
