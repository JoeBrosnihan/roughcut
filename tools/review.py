#!/usr/bin/env python3
"""Review a Roughcut bin from your phone, over your own network.

The companion to `watch.py`: that one reviews the cut, this one reviews the
material it is cut from. Point a phone at it over a private network and you
get every clip in the bin, one at a time -- the real video, scrubbable and
playable -- with two decisions to make about each: which stretches of it are
good, and whether it belongs in the bin at all.

Under that is what was said in the clip, from the transcripts whisper has
already cached. Each line carries the frame it starts on, so tapping one
seeks the video to it, and the line being spoken stays marked as it plays.
Lines already inside a kept stretch are marked too, so a second pass can
see what has been claimed without reading it off the bar.

Both land straight in the .roughcut project file, which is what the window
reads. Nothing leaves the machine.

    python tools/review.py                    serve on 0.0.0.0:8788
    python tools/review.py --project X.roughcut --port 9000
    python tools/review.py --prepare          transcode, then exit

Phone-sized copies live in `.roughcut-review` beside the project: 270p, which
is enough to judge a face by and small enough to seek over a tailnet. They
are built on first run and reused after that. The originals are 4K and are
never served -- a phone would spend the whole review buffering.

No dependencies. Python's own http.server has no Range support, and without
Range iOS Safari refuses to play a video at all, so that is written out here.
"""

import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse, unquote

DEFAULT_PROJECT = Path(r"F:\NYC Aug 2026\nyc.roughcut")
REVIEW_DIRNAME = ".roughcut-review"
HEIGHT = 270          # enough to judge a face by
FPS_OUT = 24
CRF = "33"

LOCK = threading.Lock()
PROJECT = DEFAULT_PROJECT
REVIEW_DIR = None
FPS = 30.0


# ---------------------------------------------------------------------------
# The project
# ---------------------------------------------------------------------------

def load():
    return json.loads(PROJECT.read_text(encoding="utf-8"))


def save(project):
    """Write the project the way Roughcut does: a sibling temp file and a
    rename, so an interrupted write cannot destroy the previous good file."""
    tmp = PROJECT.with_suffix(PROJECT.suffix + f".tmp{os.getpid()}")
    tmp.write_text(json.dumps(project, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, PROJECT)


def backup_once():
    """One backup per run, before the first change of the session."""
    global _backed_up
    if _backed_up:
        return
    dest = PROJECT.with_name(PROJECT.name + time.strftime(".bak-%Y%m%d-%H%M%S"))
    shutil.copy2(PROJECT, dest)
    print(f"  backup: {dest}")
    _backed_up = True


_backed_up = False


def ffmpeg():
    for exe in (r"C:\Program Files\ffmpeg\bin\ffmpeg.exe",
                r"C:\Program Files\Shotcut\ffmpeg.exe", "ffmpeg"):
        if shutil.which(exe) or Path(exe).is_file():
            return exe
    return None


def keep(highlights, a, b, last):
    """Insert a stretch, merging what it overlaps or touches.

    The same rule as `SourceClip::keep` in the model crate, so a stretch made
    here and one made in the window end up in the same shape: sorted and
    disjoint, and "the stretch covering this frame" has exactly one answer.
    """
    a, b = max(0, min(a, b)), min(last, max(a, b))
    if b < a:
        return highlights, False
    merged = []
    for h in highlights:
        if h["out_frame"] + 1 < a or h["in_frame"] > b + 1:
            merged.append(h)
        else:
            a = min(a, h["in_frame"])
            b = max(b, h["out_frame"])
    merged.append({"in_frame": a, "out_frame": b})
    merged.sort(key=lambda h: h["in_frame"])
    return merged, True


# ---------------------------------------------------------------------------
# Phone-sized copies
# ---------------------------------------------------------------------------

def review_path(clip):
    return REVIEW_DIR / (clip["id"] + ".mp4")


def poster_path(clip):
    return REVIEW_DIR / (clip["id"] + ".jpg")


def prepare(project, quiet=False):
    """Build a phone-sized copy and a poster for anything missing one."""
    exe = ffmpeg()
    todo = [c for c in project["clips"]
            if not review_path(c).is_file() or not poster_path(c).is_file()]
    if not todo:
        return 0
    if not exe:
        print("  ! ffmpeg not found — cannot build phone copies", file=sys.stderr)
        return 0
    REVIEW_DIR.mkdir(parents=True, exist_ok=True)
    if not quiet:
        print(f"  preparing {len(todo)} clip(s) — this happens once")

    def one(clip):
        src = clip["path"]
        name = src.split("/")[-1]
        if not Path(src).is_file():
            return f"    {name}: file is missing"
        if not review_path(clip).is_file():
            r = subprocess.run(
                [exe, "-v", "error", "-threads", "2", "-i", src,
                 "-vf", f"scale=-2:{HEIGHT}:flags=fast_bilinear,fps={FPS_OUT}",
                 "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
                 "-crf", CRF, "-preset", "veryfast", "-g", "48",
                 "-c:a", "aac", "-ac", "1", "-b:a", "32k",
                 # Header first, so playback starts before the file is buffered.
                 "-movflags", "+faststart", "-y", str(review_path(clip))],
                capture_output=True, text=True)
            if r.returncode != 0:
                return f"    {name}: {r.stderr.strip()[:120]}"
        if not poster_path(clip).is_file():
            at = (clip["duration_frames"] / FPS) / 3.0
            subprocess.run(
                [exe, "-v", "error", "-threads", "2", "-ss", f"{at:.3f}", "-i", src,
                 "-frames:v", "1", "-vf", "scale=-2:200", "-q:v", "5",
                 "-y", str(poster_path(clip))], capture_output=True, text=True)
        return f"    {name}: ready"

    with ThreadPoolExecutor(max_workers=3) as pool:
        for line in pool.map(one, todo):
            if not quiet:
                print(line, flush=True)
    return len(todo)


# ---------------------------------------------------------------------------
# What was said, and when
# ---------------------------------------------------------------------------

CLI_CANDIDATES = [
    Path(os.path.expandvars("%LOCALAPPDATA%")) / "Programs" / "Roughcut" / "roughcut-cli.exe",
    Path(__file__).resolve().parent.parent / "target" / "release" / "roughcut-cli.exe",
]
_transcripts = {}          # clip id -> lines, so whisper is asked once


def roughcut_cli():
    for p in CLI_CANDIDATES:
        if p.is_file():
            return p
    return None


def lines_for(clip_id):
    """The clip's transcript, grouped into tappable lines.

    Whisper gives words, each carrying the frame it is spoken on, and a wall
    of 217 separate words is not something to read or aim a thumb at. Lines
    break where a sentence ends, and failing that where a breath does -- a
    gap long enough to be a pause -- so a line is a thing somebody said
    rather than an arbitrary ten words.
    """
    if clip_id in _transcripts:
        return _transcripts[clip_id]

    cli = roughcut_cli()
    if cli is None:
        return None
    out = subprocess.run([str(cli), "transcript", "--project", str(PROJECT),
                          "--clip", clip_id, "--words"],
                         capture_output=True, text=True)
    try:
        data = json.loads(out.stdout)
    except ValueError:
        return None
    if not data.get("transcribed"):
        _transcripts[clip_id] = []
        return []

    words = data.get("word_list") or []
    lines, cur = [], []
    # Punctuation is what actually ends a thought. A pause only breaks a
    # line when it is a long one and there is already a line's worth of
    # words -- speech is full of half-second hesitations, and treating
    # those as breaks chopped "the best piece of software / engineering /
    # advice he ever gave me" into three lines.
    GAP_MS = 1500
    MIN_BEFORE_GAP = 5
    MAX_WORDS = 16        # so a monologue without punctuation still breaks
    for i, w in enumerate(words):
        cur.append(w)
        text = (w.get("text") or "").strip()
        nxt = words[i + 1] if i + 1 < len(words) else None
        gap = (nxt.get("ms", 0) - w.get("ms", 0)) if nxt else 0
        ends = text.endswith((".", "?", "!", "…"))
        breathed = gap >= GAP_MS and len(cur) >= MIN_BEFORE_GAP
        if nxt is None or ends or breathed or len(cur) >= MAX_WORDS:
            lines.append({
                "frame": cur[0].get("frame", 0),
                "end": cur[-1].get("frame", 0),
                "text": " ".join((c.get("text") or "").strip() for c in cur).strip(),
            })
            cur = []
    lines = [l for l in lines if l["text"]]
    _transcripts[clip_id] = lines
    return lines


# ---------------------------------------------------------------------------
# What the phone is given
# ---------------------------------------------------------------------------

def state():
    project = load()
    clips = []
    for c in project["clips"]:
        clips.append({
            "id": c["id"],
            "name": c["path"].split("/")[-1],
            "frames": c["duration_frames"],
            "hi": [[h["in_frame"], h["out_frame"]] for h in c.get("highlights", [])],
            "arch": bool(c.get("archived", False)),
            "ready": review_path(c).is_file(),
        })
    return {"project": PROJECT.stem, "fps": FPS, "clips": clips}


PAGE = r"""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>__PROJECT__ — bin review</title>
<style>
  /* Roughcut's own palette: these judgements go straight into that window,
     and footage is judged against a dark ground in both places. */
  :root { color-scheme: dark;
    --bg:#141416; --panel:#1b1b1e; --raised:#232327; --line:#333338;
    --text:#dcdce0; --dim:#8a8a92; --accent:#4c9aff;
    --keep:#2fa86a; --keep-lit:#4cd17a; --out:#ff8c42; --error:#ff5c5c;
    --ui:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;
    --mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,Consolas,monospace; }
  * { box-sizing:border-box; -webkit-tap-highlight-color:transparent; }
  [hidden] { display:none !important; }
  body { margin:0; background:var(--bg); color:var(--text);
         font:15px/1.45 var(--ui);
         padding-bottom:calc(18px + env(safe-area-inset-bottom)); }
  .wrap { max-width:620px; margin:0 auto; }

  /* The header and the video are one pinned block. Sticky elements that
     each claim top:0 separately fight over which is in front; this way
     there is nothing to resolve, and the z-index only has to beat the
     positioned elements further down the page. */
  .top { position:sticky; top:0; z-index:10; background:var(--bg); }
  header { background:var(--panel); border-bottom:1px solid var(--line);
           padding:calc(8px + env(safe-area-inset-top)) 12px 8px; }
  .titles { display:flex; align-items:baseline; justify-content:space-between;
            gap:6px 10px; flex-wrap:wrap; min-width:0; }
  .project { min-width:0; font-size:13px; font-weight:600; letter-spacing:.06em;
             text-transform:uppercase; }
  .tally { min-width:0; font-family:var(--mono); font-size:12px;
           font-variant-numeric:tabular-nums; color:var(--dim); }
  .tally b { color:var(--keep-lit); font-weight:600; }
  .rail { display:flex; gap:2px; margin-top:8px; }
  .cell { flex:1 1 0; height:16px; min-width:0; padding:0; border:0;
          border-radius:1px; background:var(--raised); position:relative; }
  .cell[data-state="kept"] { background:var(--keep); }
  .cell[data-state="archived"] { background:#2a2a2e; }
  .cell[data-state="archived"]::after { content:""; position:absolute;
          inset:6px 2px auto; height:1px; background:var(--dim); }
  .cell[data-current="1"] { box-shadow:inset 0 0 0 2px var(--accent); }

  .stage { background:#000; }
  video { width:100%; display:block; background:#000; max-height:52vh; }
  .missing { padding:30px 12px; text-align:center; color:var(--dim); font-size:13px; }

  .clip { padding:12px; }
  .name { display:flex; align-items:center; gap:8px; font-family:var(--mono);
          font-size:13px; margin-bottom:8px; }
  .name .index { color:var(--dim); font-variant-numeric:tabular-nums; }
  .name .len { margin-left:auto; color:var(--dim); font-variant-numeric:tabular-nums; }
  .archived .name { color:var(--dim); }

  /* The same shape as the scrub bar under the monitor in the window:
     kept stretches in green, the range being chosen on top of them. */
  /* The one control that matters: drag across it to choose a stretch, tap
     it to move the playhead. Tall enough to grab with a thumb, and
     touch-action:none so the page does not scroll out from under the drag. */
  .bar { position:relative; height:46px; margin-top:10px; background:var(--raised);
         border:1px solid var(--line); overflow:hidden; touch-action:none;
         cursor:ew-resize; }
  .hint { position:absolute; inset:0; display:grid; place-items:center;
          font-size:12px; color:var(--dim); pointer-events:none; }
  .bar .kept { position:absolute; top:0; bottom:0; background:rgba(47,168,106,.72);
          border-left:1px solid var(--keep-lit); border-right:1px solid var(--keep-lit); }
  .bar .sel  { position:absolute; top:0; bottom:0; background:rgba(76,154,255,.26);
          border-left:2px solid var(--keep-lit); border-right:2px solid var(--out); }
  .bar .head { position:absolute; top:0; bottom:0; width:2px;
          background:var(--playhead,#ffd84c); }

  .readout { display:flex; gap:12px; margin-top:6px; font-family:var(--mono);
             font-size:13px; font-variant-numeric:tabular-nums; color:var(--dim); }
  .readout .span { color:var(--text); }
  .readout .dur { margin-left:auto; color:var(--keep-lit); }
  /* Dismissing a half-made choice is not worth a button the size of the
     ones that commit one. */
  .readout .x { flex:0 0 auto; min-height:0; padding:0 6px; font-size:15px;
                line-height:1.2; background:none; border:0; color:var(--dim); }

  .row { display:flex; gap:8px; margin-top:10px; }
  /* min-width:0 matters: a flex item will not shrink below its own
     min-content width without it, so one long label ("Restore to bin")
     pushed the whole row wider than the phone. */
  button { font:600 15px var(--ui); color:var(--text); background:var(--raised);
           border:1px solid var(--line); border-radius:3px; padding:13px 10px;
           min-height:46px; flex:1 1 0; min-width:0; overflow:hidden;
           text-overflow:ellipsis; white-space:nowrap; }
  button:active { background:#2c2c31; }
  button:disabled { opacity:.4; }
  .primary { background:var(--keep); border-color:var(--keep-lit); color:#06130c; }
  .primary:disabled { background:var(--raised); border-color:var(--line); color:var(--text); }
  .on { border-color:var(--out); color:var(--out); }

  .keeps { margin-top:14px; }
  .keeps h2 { font-size:11px; font-weight:600; letter-spacing:.08em;
              text-transform:uppercase; color:var(--dim); margin:0 0 6px; }
  .keeps ul { list-style:none; margin:0; padding:0; display:flex;
              flex-direction:column; gap:4px; }
  /* The row itself plays the stretch, so only dropping needs a control. */
  .keeps li { display:flex; align-items:center; gap:10px; background:var(--panel);
              border:1px solid var(--line); border-left:3px solid var(--keep);
              padding:11px 10px; font-family:var(--mono); font-size:12px;
              font-variant-numeric:tabular-nums; cursor:pointer; }
  .keeps li:active { background:var(--raised); }
  .keeps .of { color:var(--dim); margin-left:auto; }
  .keeps .drop { flex:0 0 auto; min-height:0; padding:2px 8px; font-size:15px;
              line-height:1.2; background:none; border:0; color:var(--dim); }

  /* What was said, under the decisions about it. Whisper gives words with
     the frame each is spoken on, so a line is a seek target as well as
     something to read -- which is most of why the transcript is worth
     having on the phone at all. */
  .script { margin-top:18px; }
  .script h2 { font-size:11px; font-weight:600; letter-spacing:.08em;
               text-transform:uppercase; color:var(--dim); margin:0 0 6px;
               display:flex; gap:8px; align-items:baseline; }
  .script ol { list-style:none; margin:0; padding:0; }
  .script li { display:flex; gap:10px; padding:9px 10px; cursor:pointer;
               border-bottom:1px solid var(--line); align-items:baseline; }
  .script li:active { background:var(--raised); }
  .script li.now { background:#1c2128; box-shadow:inset 3px 0 0 var(--accent); }
  .script li.claimed { box-shadow:inset 3px 0 0 var(--keep); }
  .script li.now.claimed { box-shadow:inset 3px 0 0 var(--keep-lit); }
  .script .at { flex:0 0 auto; font-family:var(--mono); font-size:12px;
                font-variant-numeric:tabular-nums; color:var(--accent);
                min-width:44px; }
  .script li.claimed .at { color:var(--keep-lit); }
  .script .said { flex:1 1 auto; min-width:0; font-size:15px; }

  /* Shown only when something failed, and gone the moment it stops being
     true -- so it costs nothing at rest. */
  .status { margin:14px 12px 0; padding:9px 11px; font-size:13px;
            color:var(--text); background:var(--panel);
            border:1px solid var(--line); border-left:3px solid var(--error);
            overflow-wrap:anywhere; }
</style></head><body>
<div class="wrap">
  <div class="top">
  <header>
    <div class="titles">
      <span class="project" id="project">—</span>
      <span class="tally" id="tally"></span>
    </div>
    <div class="rail" id="rail"></div>
  </header>

  <div class="stage">
    <video id="v" controls playsinline preload="metadata"></video>
    <div class="missing" id="missing" hidden>No phone copy of this clip yet.</div>
  </div>
  </div>

  <main class="clip" id="clip">
    <div class="name">
      <span class="index" id="index"></span>
      <span id="filename">—</span>
      <span class="len" id="length"></span>
    </div>

    <div class="bar" id="bar">
      <div class="hint" id="hint">Drag across to choose a stretch</div>
      <div class="head" id="head" style="left:0"></div>
    </div>
    <div class="readout" id="readout" hidden>
      <span class="span" id="span"></span>
      <span class="dur" id="dur"></span>
      <button class="x" id="clear" aria-label="Discard this stretch">&times;</button>
    </div>

    <div class="row">
      <button id="keep" class="primary" disabled>Keep this stretch</button>
    </div>
    <div class="row">
      <button id="prev">‹ Prev</button>
      <button id="archive">Archive</button>
      <button id="next">Next ›</button>
    </div>

    <section class="keeps" id="keepsSection" hidden>
      <h2 id="keepsTitle">Kept stretches</h2>
      <ul id="keepList"></ul>
    </section>

    <section class="script" id="scriptSection" hidden>
      <h2>Transcript</h2>
      <ol id="script"></ol>
    </section>
  </main>

  <p class="status" id="status" hidden></p>
</div>

<script>
(function () {
  "use strict";
  var S = null, at = 0, sel = null, FPS = 30;
  var $ = function (id) { return document.getElementById(id); };
  var v = $("v");

  // Frames stay the unit -- every position sent to the server is a frame
  // number, and the project stores frames -- but a second is the finest
  // thing shown. A frame is imperceptible at a glance, so displaying one
  // is precision nobody can act on and everybody has to read past.
  // Minutes, to one decimal: "3.5m". A running total is a sense of scale,
  // not a cue point -- nobody needs it to the frame, and m:ss invites you
  // to read it as one.
  function mins(f) {
    if (!f) return "0m";
    return (f / FPS / 60).toFixed(1) + "m";
  }
  function mmss(f) {
    var t = Math.round(f / FPS);
    return Math.floor(t / 60) + ":" + (t % 60 < 10 ? "0" : "") + (t % 60);
  }
  // Failures only. Anything else clears it: a later success means whatever
  // went wrong is no longer the state of things.
  function status(t) {
    var el = $("status");
    el.textContent = t || "";
    el.hidden = !t;
  }
  function clip() { return S.clips[at]; }
  // The playhead is the video's own clock, so a frame number always means
  // the frame that is actually on screen.
  function frameNow() { return Math.round((v.currentTime || 0) * FPS); }

  function send(path, body) {
    return fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body)
    }).then(function (r) {
      if (!r.ok) throw new Error("HTTP " + r.status);
      return r.json();
    });
  }

  function paintBar() {
    var c = clip(), bar = $("bar"), last = Math.max(1, c.frames - 1);
    Array.prototype.slice.call(bar.querySelectorAll(".kept,.sel")).forEach(function (n) { n.remove(); });
    c.hi.forEach(function (h) {
      var d = document.createElement("div");
      d.className = "kept";
      d.style.left = (100 * h[0] / last) + "%";
      d.style.width = "max(3px," + (100 * (h[1] - h[0]) / last) + "%)";
      bar.insertBefore(d, $("head"));
    });
    if (sel) {
      var a = Math.min(sel[0], sel[1]), b = Math.max(sel[0], sel[1]);
      var d2 = document.createElement("div");
      d2.className = "sel";
      d2.style.left = (100 * a / last) + "%";
      d2.style.width = "max(3px," + (100 * (b - a) / last) + "%)";
      bar.insertBefore(d2, $("head"));
      $("span").textContent = mmss(a) + " → " + mmss(b);
      $("dur").textContent = "+" + mmss(b - a + 1);
    }
    $("readout").hidden = !sel;
    $("keep").disabled = !sel;
    $("hint").hidden = !!sel || c.hi.length > 0;
  }

  function paintHead() {
    var c = clip(), last = Math.max(1, c.frames - 1);
    $("head").style.left = (100 * Math.min(frameNow(), last) / last) + "%";
  }

  function paintRail() {
    var cells = $("rail").children, kept = 0, arch = 0, all = 0;
    S.clips.forEach(function (c, i) {
      // Footage, not clips: a pass is heading for a cut of some length, and
      // thirty clips holding four seconds each is a different position from
      // three holding a minute. How many clips are in which state is already
      // on the rail, cell by cell; the totals are what it cannot show.
      c.hi.forEach(function (h) { kept += h[1] - h[0] + 1; });
      if (c.arch) arch += c.frames;
      all += c.frames;
      cells[i].setAttribute("data-state", c.arch ? "archived" : (c.hi.length ? "kept" : "new"));
      cells[i].setAttribute("data-current", i === at ? "1" : "0");
    });
    $("tally").innerHTML = "<b>" + mins(kept) + "</b> kept &middot; " +
                           mins(arch) + " archived &middot; " + mins(all) + " total";
  }

  function render() {
    var c = clip();
    $("index").textContent = (at + 1) + "/" + S.clips.length;
    $("filename").textContent = c.name;
    $("length").textContent = mmss(c.frames);
    $("clip").classList.toggle("archived", c.arch);
    $("archive").textContent = c.arch ? "Restore" : "Archive";
    $("archive").classList.toggle("on", c.arch);
    $("prev").disabled = at === 0;
    $("next").disabled = at === S.clips.length - 1;
    $("missing").hidden = c.ready;
    v.hidden = !c.ready;
    if (c.ready) {
      var want = "/video/" + c.id;
      if (v.getAttribute("src") !== want) { v.setAttribute("src", want); v.load(); }
    } else {
      v.removeAttribute("src");
    }

    var list = $("keepList");
    list.textContent = "";
    c.hi.forEach(function (h, i) {
      var li = document.createElement("li");
      var span = document.createElement("span");
      span.textContent = mmss(h[0]) + " → " + mmss(h[1]);
      var of = document.createElement("span");
      of.className = "of";
      of.textContent = mmss(h[1] - h[0] + 1);
      li.title = "Play this stretch";
      li.onclick = function () { v.currentTime = h[0] / FPS; v.play(); };
      var drop = document.createElement("button");
      drop.className = "drop"; drop.innerHTML = "&times;";
      drop.setAttribute("aria-label", "Drop the stretch at " + mmss(h[0]));
      drop.onclick = function (e) {
        e.stopPropagation();          // the row plays; the cross removes
        send("/drop", { clip: c.id, index: i }).then(function (r) {
          c.hi = r.hi; render(); paintScript(); status(null);
        }).catch(function (e2) { status("Could not drop that: " + e2.message); });
      };
      li.appendChild(span); li.appendChild(of); li.appendChild(drop);
      list.appendChild(li);
    });
    var clipFrames = 0;
    c.hi.forEach(function (h) { clipFrames += h[1] - h[0] + 1; });
    $("keepsSection").hidden = !c.hi.length;
    $("keepsTitle").textContent =
      "Kept stretches \u00b7 " + mmss(clipFrames) + " of " + mmss(c.frames);
    paintBar(); paintHead(); paintRail();
  }

  function go(i) {
    at = Math.max(0, Math.min(S.clips.length - 1, i));
    sel = null;
    render();
    loadScript();
    window.scrollTo({ top: 0 });
    // Moving to a clip is a request to watch it. This runs inside the tap
    // that asked for it, which is the only reason a phone will start it
    // with the sound on; a refusal is not worth reporting, since the
    // controls are right there. Deliberately not in render(): the first
    // load is not a gesture, and a page that starts talking on open is a
    // different thing entirely.
    if (clip().ready) {
      var started = v.play();
      if (started && started.catch) started.catch(function () {});
    }
  }

  // --- transcript --------------------------------------------------------

  var script = [], scriptFor = null, activeLine = -1;

  function loadScript() {
    var c = clip();
    if (scriptFor === c.id) { paintScript(); return; }
    scriptFor = c.id;
    script = [];
    activeLine = -1;
    $("script").textContent = "";
    $("scriptSection").hidden = true;
    fetch("/transcript/" + c.id).then(function (r) { return r.json(); })
      .then(function (d) {
        if (scriptFor !== c.id) return;      // moved on while it loaded
        script = d.lines || [];
        paintScript();
      })
      .catch(function () {
        if (scriptFor === c.id) status("Could not read the transcript.");
      });
  }

  function inKept(frame) {
    return clip().hi.some(function (h) { return frame >= h[0] && frame <= h[1]; });
  }

  function paintScript() {
    var ol = $("script");
    ol.textContent = "";
    // A clip nobody spoke in has no transcript to head, so the whole
    // section goes rather than announcing its own emptiness.
    $("scriptSection").hidden = !script.length;
    script.forEach(function (line, i) {
      var li = document.createElement("li");
      li.dataset.i = i;
      // A line already inside a kept stretch is marked, so a second pass
      // can see what has been claimed without cross-checking the bar.
      if (inKept(line.frame)) li.classList.add("claimed");
      var at_ = document.createElement("span");
      at_.className = "at";
      at_.textContent = mmss(line.frame);
      var said = document.createElement("span");
      said.className = "said";
      said.textContent = line.text;
      li.appendChild(at_); li.appendChild(said);
      li.onclick = function () { v.currentTime = line.frame / FPS; v.play(); };
      ol.appendChild(li);
    });
    followScript();
  }

  // Keep the spoken line under the playhead marked, so reading and watching
  // stay in step without having to look between them.
  function followScript() {
    if (!script.length) return;
    var f = frameNow(), found = -1;
    for (var i = 0; i < script.length; i++) {
      if (f >= script[i].frame) found = i; else break;
    }
    if (found === activeLine) return;
    var ol = $("script");
    if (activeLine >= 0 && ol.children[activeLine]) {
      ol.children[activeLine].classList.remove("now");
    }
    activeLine = found;
    if (found >= 0 && ol.children[found]) {
      ol.children[found].classList.add("now");
    }
  }

  $("clear").onclick = function () { sel = null; paintBar(); };

  // One gesture on the bar does both jobs: a drag paints the stretch you are
  // choosing, a tap moves the playhead. They are told apart by distance, so
  // neither has to be a button.
  (function () {
    var bar = $("bar"), mode = null, anchor = 0, startX = 0, moved = false;
    var SLOP = 6;                       // px before a tap becomes a drag

    function frameAt(clientX) {
      var r = bar.getBoundingClientRect(), c = clip();
      var t = (clientX - r.left) / r.width;
      return Math.max(0, Math.min(c.frames - 1, Math.round(t * (c.frames - 1))));
    }

    bar.addEventListener("pointerdown", function (e) {
      bar.setPointerCapture(e.pointerId);
      startX = e.clientX;
      moved = false;
      var f = frameAt(e.clientX), c = clip();
      // Grabbing near an edge of the pending stretch adjusts that edge --
      // the commonest correction after a coarse drag with a thumb.
      var grab = (c.frames - 1) * 0.05;
      if (sel && Math.abs(f - sel[0]) <= grab) { mode = "in"; anchor = sel[1]; }
      else if (sel && Math.abs(f - sel[1]) <= grab) { mode = "out"; anchor = sel[0]; }
      else { mode = "new"; anchor = f; }
      e.preventDefault();
    });

    bar.addEventListener("pointermove", function (e) {
      if (mode === null) return;
      if (!moved && Math.abs(e.clientX - startX) < SLOP) return;
      moved = true;
      var f = frameAt(e.clientX);
      sel = mode === "in" ? [f, anchor] : [anchor, f];
      paintBar();
    });

    function end(e) {
      if (mode === null) return;
      if (!moved) {
        // A tap: go there, and leave any pending stretch alone.
        v.currentTime = frameAt(e.clientX) / FPS;
      } else if (sel) {
        sel = [Math.min(sel[0], sel[1]), Math.max(sel[0], sel[1])];
      }
      mode = null;
      paintBar();
    }
    bar.addEventListener("pointerup", end);
    bar.addEventListener("pointercancel", function () { mode = null; });
  })();

  $("keep").onclick = function () {
    var c = clip();
    send("/keep", { clip: c.id, in: sel[0], out: sel[1] }).then(function (r) {
      c.hi = r.hi; sel = null; render(); paintScript(); status(null);
    }).catch(function (e) { status("Could not keep that: " + e.message); });
  };

  $("archive").onclick = function () {
    var c = clip(), want = !c.arch;
    send("/archive", { clip: c.id, archived: want }).then(function () {
      c.arch = want; render(); status(null);
      if (want && at < S.clips.length - 1) setTimeout(function () { go(at + 1); }, 200);
    }).catch(function (e) { status("Could not archive that: " + e.message); });
  };

  $("prev").onclick = function () { go(at - 1); };
  $("next").onclick = function () { go(at + 1); };
  v.addEventListener("timeupdate", function () { paintHead(); followScript(); });
  v.addEventListener("seeked", function () { paintHead(); followScript(); });

  fetch("/state").then(function (r) { return r.json(); }).then(function (s) {
    S = s; FPS = s.fps;
    $("project").textContent = s.project;
    var rail = $("rail");
    s.clips.forEach(function (c, i) {
      var b = document.createElement("button");
      b.className = "cell"; b.type = "button"; b.title = c.name;
      b.onclick = function () { go(i); };
      rail.appendChild(b);
    });
    render();
    loadScript();
  }).catch(function (e) { status("Could not load the bin: " + e.message); });
})();
</script></body></html>
"""


# ---------------------------------------------------------------------------
# Serving, with the Range support that makes video work at all
# ---------------------------------------------------------------------------

RANGE = re.compile(r"bytes=(\d*)-(\d*)")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "roughcut-review"

    def log_message(self, fmt, *args):
        if not self.path.startswith(("/video/", "/poster/")):
            sys.stderr.write("  %s %s\n" % (self.command, self.path))

    def _send(self, body, ctype="text/html; charset=utf-8", code=200):
        data = body.encode("utf-8") if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def _json(self, obj, code=200):
        self._send(json.dumps(obj), "application/json", code)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/":
            self._send(PAGE.replace("__PROJECT__", PROJECT.stem))
        elif url.path == "/state":
            self._json(state())
        elif url.path.startswith("/transcript/"):
            lines = lines_for(unquote(url.path[len("/transcript/"):]))
            if lines is None:
                self._json({"lines": [], "available": False})
            else:
                self._json({"lines": lines, "available": True})
        elif url.path.startswith("/video/"):
            self.serve_file(unquote(url.path[7:]) + ".mp4", "video/mp4")
        elif url.path.startswith("/poster/"):
            self.serve_file(unquote(url.path[8:]) + ".jpg", "image/jpeg")
        else:
            self._send("not here", "text/plain; charset=utf-8", 404)

    def do_POST(self):
        url = urlparse(self.path)
        try:
            n = int(self.headers.get("Content-Length", 0))
            body = json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return self._json({"error": "bad json"}, 400)

        try:
            if url.path == "/keep":
                return self._json(self.do_keep(body))
            if url.path == "/drop":
                return self._json(self.do_drop(body))
            if url.path == "/archive":
                return self._json(self.do_archive(body))
        except KeyError:
            return self._json({"error": "no such clip"}, 404)
        except Exception as e:
            return self._json({"error": str(e)}, 500)
        self._json({"error": "not here"}, 404)

    # Each of these is the whole read-modify-write, under one lock: the phone
    # and the window must never interleave halfway through a project file.

    def do_keep(self, body):
        with LOCK:
            backup_once()
            project = load()
            clip = next(c for c in project["clips"] if c["id"] == body["clip"])
            last = max(0, clip["duration_frames"] - 1)
            hi, ok = keep(list(clip.get("highlights", [])),
                          int(body["in"]), int(body["out"]), last)
            if ok:
                clip["highlights"] = hi
                save(project)
                name = clip["path"].split("/")[-1]
                print(f"  KEEP {name} {body['in']}-{body['out']} "
                      f"({len(hi)} stretch(es) now)")
            return {"hi": [[h["in_frame"], h["out_frame"]] for h in hi]}

    def do_drop(self, body):
        with LOCK:
            backup_once()
            project = load()
            clip = next(c for c in project["clips"] if c["id"] == body["clip"])
            hi = list(clip.get("highlights", []))
            i = int(body["index"])
            if 0 <= i < len(hi):
                gone = hi.pop(i)
                clip["highlights"] = hi
                save(project)
                print(f"  DROP {clip['path'].split('/')[-1]} "
                      f"{gone['in_frame']}-{gone['out_frame']}")
            return {"hi": [[h["in_frame"], h["out_frame"]] for h in hi]}

    def do_archive(self, body):
        with LOCK:
            backup_once()
            project = load()
            clip = next(c for c in project["clips"] if c["id"] == body["clip"])
            clip["archived"] = bool(body.get("archived"))
            save(project)
            print(f"  {'ARCHIVE' if clip['archived'] else 'RESTORE'} "
                  f"{clip['path'].split('/')[-1]}")
            return {"archived": clip["archived"]}

    def serve_file(self, name, ctype):
        """A byte-range file server.

        iOS Safari will not play a video from a server that ignores Range: it
        asks for the first few bytes to read the header, and a 200 with the
        whole file makes it give up rather than buffer. So the 206 path is
        the normal path here, not an optimisation.
        """
        path = (REVIEW_DIR / name).resolve()
        if REVIEW_DIR.resolve() not in path.parents or not path.is_file():
            return self._send("not here", "text/plain; charset=utf-8", 404)

        size = path.stat().st_size
        start, end, partial = 0, size - 1, False
        m = RANGE.match(self.headers.get("Range", "") or "")
        if m:
            partial = True
            first, lastb = m.group(1), m.group(2)
            if first:
                start = int(first)
                if lastb:
                    end = int(lastb)
            elif lastb:                       # bytes=-N, the final N bytes
                start = max(0, size - int(lastb))
            if start >= size:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            end = min(end, size - 1)

        length = end - start + 1
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", ctype)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(length))
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()

        with path.open("rb") as f:
            f.seek(start)
            left = length
            while left > 0:
                chunk = f.read(min(1 << 18, left))
                if not chunk:
                    break
                try:
                    self.wfile.write(chunk)
                except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
                    return          # ordinary: the phone seeked and moved on
                left -= len(chunk)


def tailscale_host():
    for exe in (r"C:\Program Files\Tailscale\tailscale.exe", "tailscale"):
        try:
            out = subprocess.run([exe, "status", "--json"], capture_output=True,
                                 text=True, timeout=6)
            name = json.loads(out.stdout)["Self"]["DNSName"].rstrip(".")
            ip = subprocess.run([exe, "ip", "-4"], capture_output=True,
                                text=True, timeout=6).stdout.strip().splitlines()[0]
            return name, ip
        except Exception:
            continue
    return None, None


def main():
    global PROJECT, REVIEW_DIR, FPS
    ap = argparse.ArgumentParser()
    ap.add_argument("--project", type=Path, default=DEFAULT_PROJECT)
    ap.add_argument("--port", type=int, default=8788)
    ap.add_argument("--prepare", action="store_true",
                    help="build the phone copies and exit")
    args = ap.parse_args()

    PROJECT = args.project.resolve()
    if not PROJECT.is_file():
        sys.exit(f"no project at {PROJECT}")
    REVIEW_DIR = PROJECT.parent / REVIEW_DIRNAME

    project = load()
    FPS = project["profile"]["frame_rate_num"] / project["profile"]["frame_rate_den"]

    print("Roughcut — review the bin from your phone")
    print(f"  project : {PROJECT}")
    print(f"  copies  : {REVIEW_DIR}")
    prepare(project)
    if args.prepare:
        return

    ready = sum(1 for c in project["clips"] if review_path(c).is_file())
    total = sum(review_path(c).stat().st_size
                for c in project["clips"] if review_path(c).is_file())
    print(f"  {ready}/{len(project['clips'])} clips playable "
          f"({total / 1e6:.0f} MB of 270p, originals never served)")

    name, ip = tailscale_host()
    print()
    if name:
        print(f"  ON YOUR PHONE:  http://{name}:{args.port}/")
        print(f"                  http://{ip}:{args.port}/")
    else:
        print(f"  http://<this machine>:{args.port}/")
    print(f"\n  every decision is written straight into {PROJECT.name}"
          f"\n  ctrl-c to stop\n")

    ThreadingHTTPServer(("0.0.0.0", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
