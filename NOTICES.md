# Third-party notices

Roughcut is GPL-3.0-or-later; see [LICENSE](LICENSE). This file covers the
components a **binary release** may include or depend on.

## Bundled with releases

### libmpv

Playback uses libmpv, loaded at runtime.

- Upstream: <https://github.com/mpv-player/mpv>
- Licence: **GPLv2 or later** for a normal build (LGPLv2.1+ only if built with
  `--enable-lgpl`). Assume GPLv2+.
- Windows binaries used here come from
  <https://github.com/shinchiro/mpv-winbuild-cmake>, which also publishes the
  build scripts and source references for each release.

GPLv3 §6 requires that anyone receiving the binary can obtain its
Corresponding Source. **When publishing a release that bundles
`libmpv-2.dll`, record the exact upstream release tag it came from and link
to it from the release notes.** Linking the specific published build is what
makes the source actually obtainable; "get mpv from the internet" is not.

libmpv in turn includes FFmpeg (LGPLv2.1+, or GPLv2+ when built with
`--enable-gpl`) and other libraries; their notices are carried in the mpv
distribution.

## Used but not bundled

`ffprobe`, `ffmpeg` and `melt` are located on `PATH` or in a Shotcut
installation and run as **separate processes**. Roughcut does not link them and
does not ship them.

- FFmpeg — <https://ffmpeg.org> — LGPLv2.1+ or GPLv2+ depending on build
- MLT (`melt`) — <https://www.mltframework.org> — LGPLv2.1+ / GPLv2+

## Rust dependencies

335 crates, all permissive. Regenerate the list with:

```
cargo metadata --format-version 1 --all-features
```

| Licence | Approx. crates |
| --- | --- |
| MIT OR Apache-2.0 (either may be taken) | ~200 |
| MIT | 69 |
| Apache-2.0 | 12 — includes `winit`, `glutin`, `ab_glyph` |
| Unicode-3.0 | 18 — the ICU crates |
| Zlib, ISC, BSD-2-Clause, BSD-3-Clause, BSL-1.0, 0BSD, CC0-1.0 | ~25 |

None is copyleft-only. Two need naming:

- **`epaint_default_fonts`** — `(MIT OR Apache-2.0) AND OFL-1.1 AND
  Ubuntu-font-1.0`. It bundles Ubuntu, Noto and Emoji fonts; the SIL Open Font
  Licence 1.1 and the Ubuntu Font Licence apply to those files and their terms
  travel with any binary that embeds them.
- **`r-efi`** — offers `MIT OR Apache-2.0 OR LGPL-2.1-or-later`. MIT is taken;
  no LGPL obligation arises.

The Apache-2.0-only crates are why the project is **GPLv3 and not GPLv2**:
Apache-2.0 is compatible with GPLv3 but not with GPLv2.

A complete machine-generated attribution file can be produced with
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) when the first
binary release is cut.
