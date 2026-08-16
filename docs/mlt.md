# MLT XML export

The single most important output of the program. `Ctrl+E`, written to a temp
file and atomically renamed.

Export is **one-way**. Roughcut writes MLT and never reads it; its own project
format is `.roughcut`, pretty-printed JSON. One format serving both purposes
would mean either parsing everything Shotcut can put in a file — filters, extra
tracks, transitions — or silently dropping it, and silently dropping things is
the worst possible outcome for a tool whose value is that cuts land on the
right frame.

## How the schema was determined

Not from memory. The structure came from:

- **Real Shotcut-authored `.mlt` files** (Shotcut 22.11 / MLT 7.13 and Shotcut
  21.03 / MLT 6.26), read to establish element order, required properties and
  the `shotcut:` namespace.
- **`melt`'s own XML consumer**, run against the installed MLT 7.33, to confirm
  how that exact library writes positions and profiles.

Findings that the brief's skeleton does not show, and that the writer
implements:

- Current Shotcut writes **`<chain>`**, not `<producer>`, for `avformat`
  sources. Roughcut emits `<chain>`. This departs from the literal skeleton in
  the brief and matches the real files the brief says to follow.
- A `main_bin` playlist carrying `xml_retain`, so the bin survives the trip.
- Two transitions per video track above the background — `mix` and a disabled
  `frei0r.cairoblend` — which Shotcut writes for every track and expects back.
- Integer frame positions are accepted; `melt` writes them itself.

## What is emitted

```xml
<mlt LC_NUMERIC="C" version="7.33.0" title="Roughcut" producer="main_bin">
  <profile .../>                      <!-- exact rational rate -->
  <chain id="chain0" out="299">       <!-- one per distinct source file -->
    <property name="mlt_service">avformat-novalidate</property>
    ...
  </chain>
  <playlist id="main_bin">            <!-- the bin, with xml_retain -->
  <producer id="black">               <!-- background colour producer -->
  <playlist id="background">
  <playlist id="playlist0">           <!-- the one video track, V1 -->
    <entry producer="chain0" in="100" out="199"/>
  </playlist>
  <tractor id="tractor0">             <!-- background at track 0, V1 at 1 -->
</mlt>
```

Rules the writer holds to:

- One `<chain>` per distinct source file, reused by every `<entry>` that cites
  it — never one producer per timeline item.
- Absolute paths in `resource`, and **always the original file, never a
  proxy**.
- `in`/`out` inclusive, so a producer of N frames has `length="N"` and
  `out="N-1"`.
- The whole bin is exported, not just the clips cut into the timeline, so the
  bin carries over into Shotcut.

A `Clock` time format (`HH:MM:SS.mmm`, what Shotcut itself writes) exists and
is unit tested as a fallback, but integers are the default and are confirmed
working. It is marked for deletion in `mlt.rs`.

## Confirmed against Shotcut

Shotcut opens the export with no errors or warnings. The decisive evidence is
its own validator:

```
<MltXmlChecker::check> begin
<MltXmlChecker::check> QList("Roughcut")
<MltXmlChecker::check> end ""
```

`MltXmlChecker` is the component Shotcut uses to detect and repair malformed
MLT XML. It returned an empty error string — nothing to fix. The rest of the
log shows each piece being consumed: `setPreviewScale 640 x 360` (the profile),
`profileChanged 601` (colourspace), `setAudioChannels 2` and
`setProjectFolder ""` (the `shotcut:` properties), a populated
`PlaylistModel::refreshThumbnails` (the bin survived), and
`TimelineDock::setSelection ... isMultitrack true` (the tractor was recognised
as a timeline).

Frame accuracy is verified separately — see
[verification.md](verification.md).
