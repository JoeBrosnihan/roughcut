# Licensing

Roughcut is MIT. Nothing in the dependency tree prevents that. The one thing
that needs care is **shipping binaries**, not the licence of the source.

*This is an engineering summary, not legal advice.*

## The Rust dependencies are all permissive

335 third-party crates, checked with `cargo metadata`:

| Licence | Crates |
| --- | --- |
| MIT / Apache-2.0 (either) | ~200 |
| MIT only | 69 |
| Apache-2.0 only | 12 |
| Unicode-3.0 | 18 |
| Zlib, ISC, BSD-2/3, BSL-1.0, 0BSD, CC0 | ~25 |

**No crate is copyleft-only.** Reproduce with:

```
cargo metadata --format-version 1 --all-features
```

Three worth knowing about:

- **`r-efi`** offers `MIT OR Apache-2.0 OR LGPL-2.1-or-later`. It is a choice,
  so take MIT. Nothing LGPL is imposed.
- **`epaint_default_fonts`** is `(MIT OR Apache-2.0) AND OFL-1.1 AND
  Ubuntu-font-1.0`. The `AND` is real: it bundles fonts whose licences must be
  reproduced when you distribute a binary. Attribution only — neither is
  copyleft over your code.
- **`Unicode-3.0`** (the ICU crates) is permissive with an attribution
  requirement.

So a distributed binary needs a third-party notices file. The source repository
needs nothing beyond `LICENSE`.

## The external tools do not affect the licence

`ffprobe`, `ffmpeg` and `melt` are run as **separate processes**. Roughcut does
not link them; it hands them argv and reads their output. Running a GPL program
as a subprocess has never imposed the GPL on the caller, and Roughcut does not
ship any of them — it finds them on `PATH` or in a Shotcut installation.

The exported `.mlt` is data. MLT's licence governs MLT, not files describing a
timeline.

## libmpv is the one to be careful with

mpv is **GPLv2+** unless deliberately built with `--enable-lgpl`, which yields
LGPLv2.1+. The Windows build used in development
(`shinchiro/mpv-winbuild-cmake`) should be assumed GPL: it is a general-purpose
build, and the FFmpeg configure string is not recoverable from the binary, so
its exact terms cannot be confirmed by inspection.

Two facts keep this from constraining Roughcut's own licence:

1. **Roughcut does not link libmpv.** It is opened at runtime with
   `LoadLibrary` / `dlopen` and every entry point is resolved by name. There is
   no libmpv code, header-derived or otherwise, in the binary.
2. **Roughcut does not distribute libmpv.** The user supplies it, and the
   application runs without it — the monitor reports that video is unavailable
   and every other feature continues to work.

Nothing is being combined and redistributed, so no obligation is triggered.
MIT source is fine.

### What would change that

**Shipping a bundle containing `libmpv-2.dll`.** That distributes a combined
work, and a GPL libmpv would then require the whole distribution be offered
under the GPL. Options, in order of preference:

1. **Ship no libmpv.** Tell users to install mpv. This is what
   [building.md](building.md) already describes, and it is the current
   position.
2. **Ship an LGPL build** (mpv configured with `--enable-lgpl`). LGPL
   explicitly permits dynamic linking from software under any licence, provided
   users can substitute their own build — which runtime `dlopen` plus the
   `ROUGHCUT_MPV` override already allows. The source stays MIT.
3. **Ship a GPL libmpv and license the distribution GPLv2+.** The source could
   still be MIT; the *binary distribution* would be GPL. Workable, but it makes
   the combined thing more restrictive than the code.

`vendor/` is in `.gitignore`, so the development copy of libmpv is not in the
repository.

## If you open source it

- Keep `LICENSE` (MIT) at the root; `Cargo.toml` already declares `license =
  "MIT"`.
- Replace "Roughcut contributors" in `LICENSE` with your name if you prefer.
- Do **not** attach libmpv to a release without deciding between the options
  above.
- If you publish binaries, generate a third-party notices file — `cargo-about`
  or `cargo-deny` does this from the same metadata used here — covering at
  minimum the bundled fonts and the Unicode-3.0 crates.
