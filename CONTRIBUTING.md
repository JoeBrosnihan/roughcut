# Contributing

Pull requests are welcome. Apple Silicon macOS now has a verified local
build with playback and export. The remaining work includes self-contained
packaging, screenshot paste, and child-process memory accounting.
[docs/macos.md](docs/macos.md) records what has been tested and what remains;
[docs/verification.md](docs/verification.md) describes the checks for changes
that affect playback, timing, or export.

## Terms

Roughcut is GPL-3.0-or-later, and the copyright is kept undivided so that the
maintainer can also offer it under other terms later
([docs/licensing.md](docs/licensing.md) explains why). So, by submitting a
contribution, you agree that:

1. You wrote it, or otherwise have the right to submit it.
2. It is licensed under GPL-3.0-or-later, like the rest of the project.
3. The maintainer may also distribute it under any other licence, as part of
   Roughcut, without asking again.

That is the whole agreement — no separate form to sign. If you cannot agree to
it, open an issue describing the change instead of a PR.

## How to make a change land

- One change per PR, with the reason in the description, not only the what.
- Keep the code reading like the code around it: the comments say *why*, the
  names match the neighbours, and nothing is added "while here".
- `cargo test --workspace` passes, and `cargo clippy --workspace` is clean.
- For anything that touches playback, timing or export, say what you ran it
  on and what you watched happen — [docs/verification.md](docs/verification.md)
  is the checklist.
