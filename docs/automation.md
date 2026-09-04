# Driving Roughcut without the window

`roughcut-cli` is the whole editor with the picture taken away: the same
project files, the same timeline arithmetic, the same MLT writer. It exists so
that an agent — Claude Code, or anything else that can run a command — can
assemble a cut end to end, and so that the parts of editing that are typing
rather than watching stop being done by hand.

It has two front doors and one implementation.

```
roughcut-cli <command> [--name value ...]     a shell
roughcut-cli mcp                              an MCP server on stdin/stdout
```

Both read `crates/roughcut-cli/src/spec.rs`, which declares every command once:
its name, its parameters, their types, whether it writes. The MCP tool list is
generated from that table, the `--flag` parser is driven by it, the usage text
is printed from it, and the dispatch goes through it. A command therefore
cannot exist on one door and not the other, and its parameters cannot be
spelled differently in the two places. `tests/editing.rs` makes the same edit
through both and asserts the resulting projects are identical.

## What it is for

Three things the window is bad at, in rising order of how much they matter.

**Bulk.** Importing a folder, flagging by name, clearing a timeline, placing
twenty pieces of sound. Every one is a keypress in the window and a line here.

**Repeatability.** A cut assembled by a script is a cut you can rebuild after
changing your mind about the first ten seconds.

**Words.** This is the real reason. An agent cannot watch footage. A transcript
is the only handle it has on what is in a clip, and `search` turns "find where
I talk about the bridge" into an exact frame range:

```
$ roughcut-cli search --query "been there"
{ "hits": [ { "clip": "IMG_3803.MOV", "said": "been there.",
              "from": 6, "to": 7, "in": 704, "out": 761 } ] }

$ roughcut-cli cut-words --clip IMG_3803 --from 6 --to 7
```

The frames come from whisper's word timings, padded a quarter of a second at
the front and a little more at the back so the cut does not clip the speech.
Measured on real footage: cutting words 1–2 of a clip, rendering only that, and
transcribing the render returns the sentence intact.

## Conventions

**Positions are frame numbers.** Never seconds, never timecode — the same rule
[timing.md](timing.md) sets out for the rest of the program, for the same
reason. Ranges are inclusive: 100 to 199 is 100 frames. Output carries a
timecode beside each frame so a person can read it; input never accepts one,
because two spellings of a position is how a cut ends up a frame out.

**Output is JSON**, pretty-printed, on standard output. Errors are JSON too, on
standard error, with a non-zero exit. There is no `--json` flag and no table
renderer, because a second output format is a second thing to keep correct.

**Clips are named by their file.** Any unambiguous part of the name will do —
`--clip 3803` finds `IMG_3803.MOV`. An ambiguous name is refused with the
candidates listed rather than guessed at. Ids still work everywhere.

**`--project` can come from the environment.** Set `ROUGHCUT_PROJECT` and stop
repeating it.

## Several edits at once

`batch` takes a JSON array of steps and applies them to the project in memory,
writing back once:

```
roughcut-cli batch --steps '[
  {"command":"mark","clip":"3706","in":100,"out":399},
  {"command":"append","clip":"3706"},
  {"command":"add-audio-track","name":"Music"},
  {"command":"place-audio","track":0,"clip":"3803","at":0,"in":0,"out":199}
]'
```

The point is not speed. It is that a run which fails on step nine leaves the
project exactly as it was, rather than nine tenths assembled — nothing is
written until every step has succeeded.

## Using it with the window open

The window notices. It records when the project file was last written, checks
on the one moment worth checking — when it regains focus, so there is no timer
and no polling in the idle loop — and:

- with no unsaved edits of its own, reloads and says so, keeping the clip you
  were looking at;
- with unsaved edits, changes nothing and warns, because there is no merge to
  do and no way to guess which side is wanted.

So the ordinary arrangement works: leave Roughcut open on the cut, edit from
Claude Code, and alt-tab back to watch what happened.

The two share their caches as well. `roughcut_core::paths` defines the
configuration and cache directories once for both binaries, so a transcript the
window spent forty seconds making is one `search` reads immediately. Note that
a `roughcut-cli` sitting in `target/release` is a **development** build by the
same rule the window follows, and reads `target/dev-config`; the promoted copy
in `%LOCALAPPDATA%\Programs\Roughcut` reads the real one.

## As an MCP server

`roughcut-cli mcp` speaks JSON-RPC over stdin and stdout. Register it:

```
claude mcp add roughcut -- <path>\roughcut-cli.exe mcp
```

or, pinned to one project so no call has to name it:

```
claude mcp add roughcut --env ROUGHCUT_PROJECT=D:\cuts\holiday.roughcut -- <path>\roughcut-cli.exe mcp
```

Every command is offered as a tool, with `readOnlyHint` taken from the table's
`writes` column. A command that fails comes back as a result with `isError`
rather than a JSON-RPC error, so the model reads what went wrong and corrects
itself; a JSON-RPC error would not reach it.

`transcribe` and `render` block until they finish — tens of seconds a clip, and
minutes for a long timeline. There is no job queue, because a job queue is a
second state machine to keep correct and the alternative is raising a timeout.

## Adding a command

Add an entry to `COMMANDS` in `spec.rs` and a function in `ops.rs`. Nothing
else: the tool schema, the argument parser, the help and the save-afterwards
behaviour all follow from the table. `writes: true` means the project is
written back if the command changed it, and the tests in `tests/editing.rs`
check that claim against what actually lands on disk — a command that edits the
project in memory and is never saved passes every unit test and does nothing at
all.
