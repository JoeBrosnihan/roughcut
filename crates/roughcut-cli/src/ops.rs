//! What the commands actually do.
//!
//! Every one of these is a thin wrapper over `roughcut_core`. Nothing here
//! knows how it was called, and nothing here decides when the project is
//! written back — that comes from the table, so a batch of edits is one save.

use crate::args::{self, Args};
use crate::spec::{self, Body, Ctx};
use anyhow::{anyhow, bail, Context, Result};
use roughcut_core::audio::{AudioItem, AudioTrack};
use roughcut_core::model::{ClipId, Project, SourceClip, TimelineItem};
use roughcut_core::time::{format_timecode, Rational};
use roughcut_core::timeline::{self, Edge, Ripple};
use roughcut_core::tools::Tools;
use roughcut_core::transcript::{Selection, Transcript};
use roughcut_core::{import, mlt, paths, probe, profile, project_io, render, rotate, transcript, whisper};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Shared shapes
//
// Every position is reported as a frame — the only thing Roughcut does
// arithmetic on — with its timecode beside it, because a caller reasoning
// about a cut wants to read one and compute with the other.
// ---------------------------------------------------------------------------

fn at(frame: i64, fps: Rational) -> Value {
    json!({ "frame": frame, "timecode": format_timecode(frame, fps) })
}

fn clip_json(clip: &SourceClip, fps: Rational, uses: usize) -> Value {
    json!({
        "id": clip.id.to_string(),
        "name": clip.file_name(),
        "path": clip.path.to_string_lossy(),
        "frames": clip.duration_frames,
        "duration": format_timecode(clip.duration_frames, fps),
        "in": clip.mark_in,
        "out": clip.mark_out,
        "flagged": clip.flagged,
        "still": clip.still,
        "audio": clip.has_audio,
        "width": clip.width,
        "height": clip.height,
        "rate_mismatch": clip.rate_mismatch,
        "variable_rate": clip.variable_rate,
        "missing": !clip.path.exists(),
        "timeline_uses": uses,
    })
}

/// One cut, with its position on the timeline worked out.
fn item_json(project: &Project, index: usize, item: &TimelineItem) -> Value {
    let fps = project.fps();
    let start = timeline::item_start(&project.timeline, index);
    json!({
        "index": index,
        "clip": item.clip_id.to_string(),
        "name": project.clip(item.clip_id).map(|c| c.file_name()),
        "in": item.in_frame,
        "out": item.out_frame,
        "frames": item.len(),
        "start": at(start, fps),
        "end": at(start + item.len() - 1, fps),
    })
}

/// What every editing command returns: enough of the new state that the caller
/// does not have to ask a second question to know what happened.
fn timeline_state(project: &Project) -> Value {
    let fps = project.fps();
    let total = timeline::total_frames(&project.timeline);
    json!({
        "items": project.timeline.len(),
        "frames": total,
        "duration": format_timecode(total, fps),
    })
}

fn ripple_mode(args: &Args) -> Ripple {
    match args.choice("ripple", "picture") {
        "all" => Ripple::AllTracks,
        _ => Ripple::PictureOnly,
    }
}

/// The source range a command should use: the explicit one if given, else the
/// clip marks, else the whole clip.
fn range_for(clip: &SourceClip, args: &Args) -> Result<(i64, i64)> {
    let (mut in_frame, mut out_frame) = clip
        .marked_range()
        .ok_or_else(|| anyhow!("{} has no usable range", clip.file_name()))?;
    if let Some(v) = args.opt_int("in") {
        in_frame = v;
    }
    if let Some(v) = args.opt_int("out") {
        out_frame = v;
    }
    let last = clip.last_frame();
    if in_frame < 0 || out_frame > last {
        bail!(
            "{} has frames 0 to {last}; {in_frame} to {out_frame} is outside it",
            clip.file_name()
        );
    }
    if out_frame < in_frame {
        bail!("out ({out_frame}) is before in ({in_frame})");
    }
    Ok((in_frame, out_frame))
}

/// The external tools, honouring the overrides set in the window settings so
/// the two agree about which ffmpeg is meant.
pub fn tools() -> Tools {
    let (ffprobe, ffmpeg) = settings_overrides();
    Tools::discover().with_overrides(ffprobe.as_deref(), ffmpeg.as_deref())
}

fn settings_overrides() -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(dir) = paths::config_dir() else {
        return (None, None);
    };
    let Ok(text) = std::fs::read_to_string(dir.join("settings.json")) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let read = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    };
    (read("ffprobe_path"), read("ffmpeg_path"))
}

fn need_ffprobe(tools: &Tools) -> Result<&Path> {
    tools
        .ffprobe
        .as_deref()
        .ok_or_else(|| anyhow!("ffprobe was not found. It comes with Shotcut; run `doctor` to see where Roughcut looked."))
}

// ---------------------------------------------------------------------------
// The project itself
// ---------------------------------------------------------------------------

pub fn new_project(args: &Args) -> Result<Value> {
    let path = args.path("project")?;
    if path.exists() && !args.flag("force") {
        bail!("{} already exists. Pass --force to replace it.", path.display());
    }
    let project = Project::new();
    project_io::save(&project, &path)?;
    Ok(json!({
        "project": path.to_string_lossy(),
        "profile": project.profile.description(),
    }))
}

pub fn info(ctx: &mut Ctx) -> Result<Value> {
    let p = &*ctx.project;
    let fps = p.fps();
    let total = timeline::total_frames(&p.timeline);
    Ok(json!({
        "project": ctx.path.to_string_lossy(),
        "profile": {
            "width": p.profile.width,
            "height": p.profile.height,
            "fps": fps.to_string(),
            "fps_num": p.profile.frame_rate_num,
            "fps_den": p.profile.frame_rate_den,
            "description": p.profile.description(),
        },
        "clips": p.clips.len(),
        "flagged": p.clips.iter().filter(|c| c.flagged).count(),
        "missing": project_io::missing_media(p).len(),
        "timeline": { "items": p.timeline.len(), "frames": total, "duration": format_timecode(total, fps) },
        "audio_tracks": p.audio.len(),
        "export_profile": profile::for_export(p).description(),
    }))
}

pub fn doctor(_args: &Args) -> Result<Value> {
    let t = tools();
    let model = t.whisper.as_deref().and_then(whisper::find_model);
    let show = |p: &Option<PathBuf>| match p {
        Some(p) => Value::String(p.to_string_lossy().into_owned()),
        None => Value::Null,
    };
    Ok(json!({
        "ffprobe": show(&t.ffprobe),
        "ffmpeg": show(&t.ffmpeg),
        "melt": show(&t.melt),
        "whisper": show(&t.whisper),
        "whisper_model": show(&model),
        "config_dir": show(&paths::config_dir()),
        "transcript_cache": show(&paths::transcript_cache_dir()),
        "project": std::env::var("ROUGHCUT_PROJECT").ok(),
    }))
}

/// Several edits, applied to the project in memory and written back once.
///
/// The value is not saving a few milliseconds. It is that a sequence of edits
/// either all happened or none did: a run that fails on step nine does not
/// leave a half-assembled cut behind, because nothing was written before step
/// nine succeeded.
pub fn batch(ctx: &mut Ctx) -> Result<Value> {
    let text = ctx.args.text("steps")?;
    let steps: Vec<Value> = serde_json::from_str(text)
        .context("--steps wants a JSON array of steps, each an object with a command name")?;

    let mut results = Vec::with_capacity(steps.len());
    for (n, step) in steps.iter().enumerate() {
        let Value::Object(fields) = step else {
            bail!("step {n} is not an object");
        };
        let mut fields = fields.clone();
        let name = fields
            .remove("command")
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| anyhow!("step {n} has no \"command\""))?;
        let cmd = spec::find(&name)
            .ok_or_else(|| anyhow!("step {n}: there is no command called {name}"))?;
        let Body::OnProject(run) = cmd.body else {
            bail!("step {n}: {name} does not work on the project, so it cannot be batched");
        };
        if name == "batch" {
            bail!("step {n}: a batch inside a batch has no meaning");
        }
        // The project path is the outer one; a step naming a different project
        // would be edited in memory and written to the wrong file.
        fields.remove("project");
        let step_args = Args::new(cmd, fields).with_context(|| format!("step {n} ({name})"))?;
        let mut step_ctx = Ctx { project: ctx.project, args: &step_args, path: ctx.path };
        let value = run(&mut step_ctx).with_context(|| format!("step {n} ({name})"))?;
        results.push(json!({ "command": name, "result": value }));
    }
    Ok(json!({ "applied": results.len(), "steps": results, "timeline": timeline_state(ctx.project) }))
}

// ---------------------------------------------------------------------------
// The bin
// ---------------------------------------------------------------------------

pub fn import(ctx: &mut Ctx) -> Result<Value> {
    let tools = tools();
    let ffprobe = need_ffprobe(&tools)?;

    let mut files: Vec<PathBuf> = Vec::new();
    for path in ctx.args.paths("path") {
        if !path.exists() {
            bail!("{} is not there", path.display());
        }
        files.extend(roughcut_core::tools::expand_drop(&path));
    }
    if files.is_empty() {
        bail!("nothing to import: no media files were found");
    }

    let mut added = Vec::new();
    let mut duplicate = Vec::new();
    let mut refused = Vec::new();
    for file in files {
        let info = match probe::probe(ffprobe, &file) {
            Ok(info) => info,
            Err(e) => {
                refused.push(json!({ "path": file.to_string_lossy(), "why": e.to_string() }));
                continue;
            }
        };
        match import::add_clip(ctx.project, &file, &info) {
            import::ImportOutcome::Added(id) => added.push(id),
            import::ImportOutcome::Duplicate(id) => duplicate.push(id),
        }
    }
    // Importing may move the working format while nothing is marked or
    // assembled — the same rule the window follows.
    profile::refresh_working(ctx.project);

    let fps = ctx.project.fps();
    let describe = |ids: &[ClipId], project: &Project| -> Vec<Value> {
        ids.iter()
            .filter_map(|id| project.clip(*id))
            .map(|c| clip_json(c, fps, project.timeline_uses(c.id)))
            .collect()
    };
    Ok(json!({
        "added": describe(&added, ctx.project),
        "already_there": duplicate.len(),
        "refused": refused,
        "profile": ctx.project.profile.description(),
        "clips": ctx.project.clips.len(),
    }))
}

pub fn clips(ctx: &mut Ctx) -> Result<Value> {
    let fps = ctx.project.fps();
    let want_flag = ctx.args.opt_flag("flagged");
    let needle = ctx.args.opt_text("match").map(str::to_lowercase);
    let out: Vec<Value> = ctx
        .project
        .clips
        .iter()
        .filter(|c| want_flag.is_none_or(|want| c.flagged == want))
        .filter(|c| {
            needle
                .as_ref()
                .is_none_or(|n| c.file_name().to_lowercase().contains(n))
        })
        .map(|c| clip_json(c, fps, ctx.project.timeline_uses(c.id)))
        .collect();
    Ok(json!({ "clips": out }))
}

pub fn mark(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clear = ctx.args.flag("clear");
    let (in_frame, out_frame) = (ctx.args.opt_int("in"), ctx.args.opt_int("out"));
    if !clear && in_frame.is_none() && out_frame.is_none() {
        bail!("mark needs --in, --out or --clear");
    }
    let fps = ctx.project.fps();
    let clip = ctx
        .project
        .clip_mut(id)
        .ok_or_else(|| anyhow!("that clip is not in the bin"))?;
    let last = clip.last_frame();
    for (name, value) in [("in", in_frame), ("out", out_frame)] {
        if let Some(v) = value {
            if v < 0 || v > last {
                bail!("{} has frames 0 to {last}; --{name} {v} is outside it", clip.file_name());
            }
        }
    }
    if clear {
        clip.mark_in = None;
        clip.mark_out = None;
    }
    if let Some(v) = in_frame {
        clip.mark_in = Some(v);
    }
    if let Some(v) = out_frame {
        clip.mark_out = Some(v);
    }
    // Marking one end past the other is a real mistake, and silently swapping
    // them would hide it.
    if let (Some(i), Some(o)) = (clip.mark_in, clip.mark_out) {
        if o < i {
            bail!("out ({o}) would be before in ({i})");
        }
    }
    let range = clip.marked_range();
    Ok(json!({
        "clip": clip.file_name(),
        "in": clip.mark_in,
        "out": clip.mark_out,
        "frames": range.map(|(i, o)| o - i + 1),
        "duration": range.map(|(i, o)| format_timecode(o - i + 1, fps)),
    }))
}

pub fn flag(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let on = !ctx.args.flag("off");
    let clip = ctx
        .project
        .clip_mut(id)
        .ok_or_else(|| anyhow!("that clip is not in the bin"))?;
    clip.flagged = on;
    Ok(json!({ "clip": clip.file_name(), "flagged": on }))
}

pub fn remove_clip(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let name = ctx.project.clip(id).map(SourceClip::file_name).unwrap_or_default();
    let uses = ctx.project.timeline_uses(id);
    if !ctx.project.remove_clip(id) {
        bail!("{name} is used {uses} times on the timeline; remove those cuts first");
    }
    Ok(json!({ "removed": name, "clips": ctx.project.clips.len() }))
}

pub fn relink(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let to = ctx.args.path("to")?;
    if !to.is_file() {
        bail!("{} is not a file", to.display());
    }
    let to = to.canonicalize().unwrap_or(to);
    if !project_io::relink(ctx.project, id, &to) {
        bail!("that clip is not in the bin");
    }
    Ok(json!({ "clip": id.to_string(), "path": to.to_string_lossy() }))
}

pub fn missing(ctx: &mut Ctx) -> Result<Value> {
    let out: Vec<Value> = project_io::missing_media(ctx.project)
        .into_iter()
        .map(|(id, path)| {
            json!({
                "id": id.to_string(),
                "name": ctx.project.clip(id).map(SourceClip::file_name),
                "path": path.to_string_lossy(),
            })
        })
        .collect();
    Ok(json!({ "missing": out }))
}

pub fn probe(args: &Args) -> Result<Value> {
    let tools = tools();
    let ffprobe = need_ffprobe(&tools)?;
    let path = args.path("path")?;
    let info = probe::probe(ffprobe, &path)?;
    Ok(json!({
        "path": path.to_string_lossy(),
        "width": info.width,
        "height": info.height,
        "rotation": info.rotation,
        "fps": info.fps.reduced().to_string(),
        "frames": info.native_frames,
        "audio": info.has_audio,
        "still": info.still,
        "variable_rate": info.variable_rate,
    }))
}

pub fn rotate(ctx: &mut Ctx) -> Result<Value> {
    let tools = tools();
    let ffprobe = need_ffprobe(&tools)?;
    let ffmpeg = tools
        .ffmpeg
        .as_deref()
        .ok_or_else(|| anyhow!("ffmpeg was not found; run `doctor` to see where Roughcut looked"))?;
    let id = ctx.args.clip(ctx.project, "clip")?;
    let turn = match ctx.args.choice("turn", "cw") {
        "ccw" => rotate::Turn::CounterClockwise,
        _ => rotate::Turn::Clockwise,
    };
    let path = ctx
        .project
        .clip(id)
        .map(|c| c.path.clone())
        .ok_or_else(|| anyhow!("that clip is not in the bin"))?;

    let info = rotate::rotate_in_place(ffmpeg, ffprobe, &path, turn)?;
    // The file is a different shape now, and the clip has to say so or the
    // export writes the old one.
    let changed = import::remeasure(ctx.project, id, &info);
    Ok(json!({
        "clip": ctx.project.clip(id).map(SourceClip::file_name),
        "width": info.width,
        "height": info.height,
        "remeasured": changed.any(),
    }))
}

// ---------------------------------------------------------------------------
// The timeline
// ---------------------------------------------------------------------------

pub fn timeline(ctx: &mut Ctx) -> Result<Value> {
    let items: Vec<Value> = ctx
        .project
        .timeline
        .iter()
        .enumerate()
        .map(|(i, item)| item_json(ctx.project, i, item))
        .collect();
    let mut out = timeline_state(ctx.project);
    out.as_object_mut()
        .expect("timeline_state is an object")
        .insert("timeline".into(), Value::Array(items));
    Ok(out)
}

pub fn append(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clip = ctx.project.clip(id).expect("resolved from this bin").clone();
    let (in_frame, out_frame) = range_for(&clip, ctx.args)?;
    if !timeline::append(ctx.project, id, in_frame, out_frame) {
        bail!("{} could not be appended", clip.file_name());
    }
    let index = ctx.project.timeline.len() - 1;
    Ok(json!({
        "added": item_json(ctx.project, index, &ctx.project.timeline[index]),
        "timeline": timeline_state(ctx.project),
    }))
}

/// Insert a range at a timeline frame and report which item it became.
///
/// `insert_at_with` answers in frames, which is the right answer for a
/// playhead and the wrong one for a caller that wants to be told what it just
/// made. Converting it in one place keeps the two callers honest.
fn insert_range(
    project: &mut Project,
    id: ClipId,
    in_frame: i64,
    out_frame: i64,
    frame: i64,
    mode: Ripple,
) -> Result<usize> {
    let landed = timeline::insert_at_with(project, frame, id, in_frame, out_frame, mode)
        .ok_or_else(|| anyhow!("nothing could be inserted at frame {frame}"))?;
    timeline::item_at(&project.timeline, landed)
        .map(|(i, _)| i)
        .ok_or_else(|| anyhow!("the timeline lost track of frame {landed}"))
}

pub fn insert(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clip = ctx.project.clip(id).expect("resolved from this bin").clone();
    let (in_frame, out_frame) = range_for(&clip, ctx.args)?;
    let frame = ctx.args.int("at")?;
    let mode = ripple_mode(ctx.args);
    let index = insert_range(ctx.project, id, in_frame, out_frame, frame, mode)?;
    Ok(json!({
        "added": item_json(ctx.project, index, &ctx.project.timeline[index]),
        "timeline": timeline_state(ctx.project),
    }))
}

pub fn split(ctx: &mut Ctx) -> Result<Value> {
    let frame = ctx.args.int("at")?;
    if !timeline::split_at(ctx.project, frame) {
        bail!("frame {frame} is not inside a cut, or is already on one");
    }
    Ok(json!({ "split_at": frame, "timeline": timeline_state(ctx.project) }))
}

fn index_arg(ctx: &Ctx, name: &str) -> Result<usize> {
    let raw = ctx.args.int(name)?;
    let len = ctx.project.timeline.len();
    if raw < 0 || raw as usize >= len {
        bail!("the timeline has {len} items, so --{name} {raw} is not one of them");
    }
    Ok(raw as usize)
}

pub fn delete(ctx: &mut Ctx) -> Result<Value> {
    let index = index_arg(ctx, "index")?;
    let removed = item_json(ctx.project, index, &ctx.project.timeline[index]);
    let mode = ripple_mode(ctx.args);
    if !timeline::ripple_delete_with(ctx.project, index, mode) {
        bail!("that item could not be removed");
    }
    Ok(json!({ "removed": removed, "timeline": timeline_state(ctx.project) }))
}

pub fn trim(ctx: &mut Ctx) -> Result<Value> {
    let index = index_arg(ctx, "index")?;
    let edge = match ctx.args.choice("edge", "head") {
        "tail" => Edge::Tail,
        _ => Edge::Head,
    };
    let by = ctx.args.int("by")?;
    let allowed = timeline::clamp_trim(ctx.project, index, edge, by);
    let mode = ripple_mode(ctx.args);
    if allowed == 0 || !timeline::trim_edge_with(ctx.project, index, edge, allowed, mode) {
        bail!("that edge cannot move by {by} — there is no more source, or nothing would be left");
    }
    Ok(json!({
        "moved": allowed,
        "clamped": allowed != by,
        "item": item_json(ctx.project, index, &ctx.project.timeline[index]),
        "timeline": timeline_state(ctx.project),
    }))
}

pub fn move_item(ctx: &mut Ctx) -> Result<Value> {
    let from = index_arg(ctx, "index")?;
    let to = index_arg(ctx, "to")?;
    if !timeline::reorder(ctx.project, from, to) {
        bail!("nothing moved");
    }
    Ok(json!({
        "item": item_json(ctx.project, to, &ctx.project.timeline[to]),
        "timeline": timeline_state(ctx.project),
    }))
}

pub fn clear(ctx: &mut Ctx) -> Result<Value> {
    let had = ctx.project.timeline.len();
    ctx.project.timeline.clear();
    Ok(json!({ "removed": had, "timeline": timeline_state(ctx.project) }))
}

// ---------------------------------------------------------------------------
// Audio tracks
// ---------------------------------------------------------------------------

fn track_arg(ctx: &Ctx) -> Result<usize> {
    let raw = ctx.args.int("track")?;
    let len = ctx.project.audio.len();
    if raw < 0 || raw as usize >= len {
        bail!("there are {len} audio tracks, so --track {raw} is not one of them");
    }
    Ok(raw as usize)
}

pub fn audio(ctx: &mut Ctx) -> Result<Value> {
    let fps = ctx.project.fps();
    let tracks: Vec<Value> = ctx
        .project
        .audio
        .iter()
        .enumerate()
        .map(|(n, track)| {
            let items: Vec<Value> = track
                .items()
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    json!({
                        "index": i,
                        "clip": item.clip_id.to_string(),
                        "name": ctx.project.clip(item.clip_id).map(SourceClip::file_name),
                        "in": item.in_frame,
                        "out": item.out_frame,
                        "frames": item.len(),
                        "start": at(item.start, fps),
                        "end": at(item.end() - 1, fps),
                    })
                })
                .collect();
            json!({ "track": n, "name": track.name, "muted": track.muted, "items": items })
        })
        .collect();
    Ok(json!({ "audio_tracks": tracks }))
}

pub fn add_audio_track(ctx: &mut Ctx) -> Result<Value> {
    let name = ctx
        .args
        .opt_text("name")
        .map(str::to_string)
        .unwrap_or_else(|| format!("A{}", ctx.project.audio.len() + 1));
    ctx.project.audio.push(AudioTrack::new(name.clone()));
    Ok(json!({ "track": ctx.project.audio.len() - 1, "name": name }))
}

pub fn remove_audio_track(ctx: &mut Ctx) -> Result<Value> {
    let n = track_arg(ctx)?;
    let track = ctx.project.audio.remove(n);
    Ok(json!({ "removed": track.name, "items": track.items().len(), "audio_tracks": ctx.project.audio.len() }))
}

pub fn place_audio(ctx: &mut Ctx) -> Result<Value> {
    let n = track_arg(ctx)?;
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clip = ctx.project.clip(id).expect("resolved from this bin").clone();
    if !clip.has_audio {
        bail!("{} has no sound to place", clip.file_name());
    }
    let (in_frame, out_frame) = range_for(&clip, ctx.args)?;
    let start = ctx.args.int("at")?;
    if start < 0 {
        bail!("--at cannot be before the start of the timeline");
    }
    let fps = ctx.project.fps();
    let index = ctx.project.audio[n]
        .place(AudioItem { clip_id: id, in_frame, out_frame, start })
        .ok_or_else(|| anyhow!("nothing was placed"))?;
    let item = ctx.project.audio[n].items()[index];
    Ok(json!({
        "track": n,
        "index": index,
        "name": clip.file_name(),
        "start": at(item.start, fps),
        "frames": item.len(),
    }))
}

pub fn remove_audio(ctx: &mut Ctx) -> Result<Value> {
    let n = track_arg(ctx)?;
    let raw = ctx.args.int("index")?;
    let len = ctx.project.audio[n].items().len();
    if raw < 0 || raw as usize >= len {
        bail!("track {n} holds {len} pieces, so --index {raw} is not one of them");
    }
    let removed = ctx.project.audio[n]
        .remove(raw as usize)
        .ok_or_else(|| anyhow!("nothing was removed"))?;
    Ok(json!({
        "track": n,
        "name": ctx.project.clip(removed.clip_id).map(SourceClip::file_name),
        "frames": removed.len(),
        "remaining": ctx.project.audio[n].items().len(),
    }))
}

pub fn mute(ctx: &mut Ctx) -> Result<Value> {
    let n = track_arg(ctx)?;
    let muted = !ctx.args.flag("off");
    ctx.project.audio[n].muted = muted;
    Ok(json!({ "track": n, "name": ctx.project.audio[n].name, "muted": muted }))
}

// ---------------------------------------------------------------------------
// Words
//
// The reason any of this is worth exposing: a transcript turns "find where I
// talk about the bridge" into a frame range, which is the one thing an agent
// cannot get by watching.
// ---------------------------------------------------------------------------

/// Read a clip transcript out of the cache the window fills.
///
/// Deliberately never runs whisper: a search across a full bin would otherwise
/// take an hour, and the caller could not tell a slow answer from a hung one.
fn cached_transcript(clip: &SourceClip) -> Option<Transcript> {
    let dir = paths::transcript_cache_dir()?;
    let file = transcript::cache_file(&dir, paths::fingerprint(&clip.path, &[]));
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cached_transcript(clip: &SourceClip, t: &Transcript) -> Result<()> {
    let dir = paths::transcript_cache_dir().ok_or_else(|| anyhow!("no cache directory"))?;
    std::fs::create_dir_all(&dir)?;
    let file = transcript::cache_file(&dir, paths::fingerprint(&clip.path, &[]));
    project_io::atomic_write(&file, serde_json::to_string(t)?.as_bytes())
}

pub fn transcript(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clip = ctx.project.clip(id).expect("resolved from this bin");
    let Some(t) = cached_transcript(clip) else {
        return Ok(json!({
            "clip": clip.file_name(),
            "transcribed": false,
            "why": "no transcript has been made for this clip yet — run `transcribe`",
        }));
    };
    let fps = ctx.project.fps();
    let last = clip.last_frame();
    let mut out = json!({
        "clip": clip.file_name(),
        "transcribed": true,
        "words": t.word_count(),
        "text": t.text(),
    });
    if ctx.args.flag("words") {
        let mut index = 0usize;
        let listed: Vec<Value> = t
            .words()
            .map(|w| {
                let frame = transcript::ms_to_frame(w.start_ms, fps).min(last);
                let v = json!({ "i": index, "text": w.text.trim(), "frame": frame, "ms": w.start_ms });
                index += 1;
                v
            })
            .collect();
        out.as_object_mut()
            .expect("object")
            .insert("word_list".into(), Value::Array(listed));
    }
    Ok(out)
}

pub fn transcribe(ctx: &mut Ctx) -> Result<Value> {
    let tools = tools();
    let ffmpeg = tools
        .ffmpeg
        .as_deref()
        .ok_or_else(|| anyhow!("ffmpeg was not found; run `doctor` to see where Roughcut looked"))?;
    let binary = tools.whisper.as_deref().ok_or_else(|| {
        anyhow!("whisper-cli was not found. Transcripts need whisper.cpp on PATH; run `doctor`.")
    })?;
    let model = whisper::find_model(binary)
        .ok_or_else(|| anyhow!("{} has no model beside it", binary.display()))?;

    let only = ctx.args.opt_clip(ctx.project, "clip")?;
    let force = ctx.args.flag("force");
    let limit = ctx.args.opt_int("limit").unwrap_or(i64::MAX).max(0) as usize;
    let scratch = std::env::temp_dir().join("roughcut-whisper");

    let candidates: Vec<SourceClip> = ctx
        .project
        .clips
        .iter()
        .filter(|c| only.is_none_or(|id| c.id == id))
        .filter(|c| c.has_audio && !c.still)
        .cloned()
        .collect();

    let mut done = Vec::new();
    let mut skipped = 0usize;
    let mut failed = Vec::new();
    for clip in candidates {
        if done.len() >= limit {
            break;
        }
        if !force && cached_transcript(&clip).is_some() {
            skipped += 1;
            continue;
        }
        eprintln!("transcribing {}", clip.file_name());
        match whisper::transcribe(ffmpeg, binary, &model, &clip.path, &scratch) {
            Ok(t) => {
                write_cached_transcript(&clip, &t)?;
                done.push(json!({ "clip": clip.file_name(), "words": t.word_count() }));
            }
            Err(e) => failed.push(json!({ "clip": clip.file_name(), "why": e.to_string() })),
        }
    }
    Ok(json!({ "transcribed": done, "already_had_one": skipped, "failed": failed }))
}

pub fn search(ctx: &mut Ctx) -> Result<Value> {
    let query = ctx.args.text("query")?.trim().to_lowercase();
    if query.is_empty() {
        bail!("--query is empty");
    }
    let needles: Vec<String> = query.split_whitespace().map(str::to_string).collect();
    let limit = ctx.args.opt_int("limit").unwrap_or(20).max(1) as usize;
    let only = ctx.args.opt_clip(ctx.project, "clip")?;
    let fps = ctx.project.fps();

    let mut hits = Vec::new();
    let mut without = Vec::new();
    for clip in ctx.project.clips.iter().filter(|c| only.is_none_or(|id| c.id == id)) {
        let Some(t) = cached_transcript(clip) else {
            if clip.has_audio && !clip.still {
                without.push(clip.file_name());
            }
            continue;
        };
        let words: Vec<&roughcut_core::transcript::Word> = t.words().collect();
        let flat: Vec<String> = words.iter().map(|w| normalise(&w.text)).collect();
        for start in 0..flat.len() {
            if hits.len() >= limit {
                break;
            }
            if !matches_at(&flat, start, &needles) {
                continue;
            }
            let end = start + needles.len() - 1;
            let selection = Selection::new(start, end);
            let range = t.range(selection, fps, clip.last_frame());
            // Enough either side to read what was actually said.
            let from = start.saturating_sub(6);
            let to = (end + 7).min(words.len());
            let context: Vec<&str> = words[from..to].iter().map(|w| w.text.trim()).collect();
            hits.push(json!({
                "clip": clip.file_name(),
                "id": clip.id.to_string(),
                "from": start,
                "to": end,
                "said": words[start..=end].iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" "),
                "context": context.join(" "),
                "in": range.map(|(i, _)| i),
                "out": range.map(|(_, o)| o),
                "at": range.map(|(i, _)| at(i, fps)),
            }));
        }
    }
    Ok(json!({
        "hits": hits,
        "clips_without_a_transcript": without,
    }))
}

/// A word stripped to what a search should match: no punctuation, no case.
///
/// The apostrophe survives, because "its" and "it's" are different words and a
/// search for one should not turn up the other. Whisper spells it either way,
/// so the curly one is folded onto the straight one first — otherwise the same
/// query finds a phrase in one clip and misses it in the next.
fn normalise(word: &str) -> String {
    word.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '\u{2019}' { '\'' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect()
}

fn matches_at(words: &[String], start: usize, needles: &[String]) -> bool {
    if start + needles.len() > words.len() {
        return false;
    }
    needles
        .iter()
        .enumerate()
        .all(|(n, needle)| words[start + n] == *needle)
}

pub fn cut_words(ctx: &mut Ctx) -> Result<Value> {
    let id = ctx.args.clip(ctx.project, "clip")?;
    let clip = ctx.project.clip(id).expect("resolved from this bin").clone();
    let t = cached_transcript(&clip)
        .ok_or_else(|| anyhow!("{} has no transcript yet — run `transcribe`", clip.file_name()))?;

    let (from, to) = (ctx.args.int("from")?, ctx.args.int("to")?);
    let count = t.word_count();
    for (name, v) in [("from", from), ("to", to)] {
        if v < 0 || v as usize >= count {
            bail!("{} has {count} words, so --{name} {v} is not one of them", clip.file_name());
        }
    }
    let selection = Selection::new(from as usize, to as usize);
    let (in_frame, out_frame) = t
        .range(selection, ctx.project.fps(), clip.last_frame())
        .ok_or_else(|| anyhow!("those words do not cover any frames"))?;

    // The marks move too, so the window opens on exactly what was cut.
    if let Some(c) = ctx.project.clip_mut(id) {
        c.mark_in = Some(in_frame);
        c.mark_out = Some(out_frame);
    }

    let said: Vec<&str> = t
        .words()
        .skip(selection.from)
        .take(selection.to - selection.from + 1)
        .map(|w| w.text.trim())
        .collect();

    let index = match ctx.args.opt_int("at") {
        Some(frame) => insert_range(ctx.project, id, in_frame, out_frame, frame, Ripple::PictureOnly)?,
        None => {
            if !timeline::append(ctx.project, id, in_frame, out_frame) {
                bail!("that range could not be appended");
            }
            ctx.project.timeline.len() - 1
        }
    };
    Ok(json!({
        "said": said.join(" "),
        "added": item_json(ctx.project, index, &ctx.project.timeline[index]),
        "timeline": timeline_state(ctx.project),
    }))
}

// ---------------------------------------------------------------------------
// Getting it out
// ---------------------------------------------------------------------------

pub fn export(ctx: &mut Ctx) -> Result<Value> {
    if ctx.project.timeline.is_empty() {
        bail!("the timeline is empty; there is nothing to export");
    }
    let out = ctx.args.path("out")?;
    let opts = mlt::ExportOptions {
        title: ctx.args.opt_text("title").unwrap_or("Roughcut").to_string(),
        ..Default::default()
    };
    mlt::write_to_file(ctx.project, &opts, &out)?;
    let target = profile::for_export(ctx.project);
    Ok(json!({
        "out": out.to_string_lossy(),
        "items": ctx.project.timeline.len(),
        "profile": target.description(),
    }))
}

pub fn render(ctx: &mut Ctx) -> Result<Value> {
    if ctx.project.timeline.is_empty() {
        bail!("the timeline is empty; there is nothing to render");
    }
    let tools = tools();
    let melt = tools.melt.as_deref().ok_or_else(|| {
        anyhow!("melt was not found — it comes with Shotcut, and is what does the encoding")
    })?;
    let out = ctx.args.path("out")?;

    // The XML melt reads is scratch, not something anyone asked for.
    let scratch = std::env::temp_dir().join(format!("roughcut-render-{}.mlt", std::process::id()));
    mlt::write_to_file(ctx.project, &mlt::ExportOptions::default(), &scratch)?;

    // Length in the export rate, which is what melt counts in.
    let target = profile::for_export(ctx.project);
    let retimed = profile::retime(ctx.project, &target);
    let total = timeline::total_frames(&retimed.timeline).max(1);

    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut last_percent = -1i64;
    let result = render::to_mp4(melt, &scratch, &out, &cancel, |frame| {
        let percent = (frame * 100 / total).clamp(0, 100);
        if percent != last_percent {
            last_percent = percent;
            eprintln!("rendering {percent}% ({frame}/{total})");
        }
    });
    let _ = std::fs::remove_file(&scratch);
    result?;

    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "out": out.to_string_lossy(),
        "frames": total,
        "bytes": size,
        "profile": target.description(),
    }))
}

// ---------------------------------------------------------------------------
// Running one
// ---------------------------------------------------------------------------

/// Load, run, and save if the command changed anything.
///
/// The save is decided by the table rather than by the command, so a command
/// physically cannot forget to persist what it did.
pub fn run(cmd: &'static spec::Cmd, args: &Args) -> Result<Value> {
    match cmd.body {
        Body::Standalone(f) => f(args),
        Body::OnProject(f) => {
            let path = args.project_path()?;
            let mut project = project_io::load(&path)?;
            let before = cmd.writes.then(|| project.clone());
            let mut ctx = Ctx { project: &mut project, args, path: &path };
            let value = f(&mut ctx)?;
            if let Some(before) = before {
                if before != project {
                    project_io::save(&project, &path)?;
                }
            }
            Ok(value)
        }
    }
}

/// Run a command by name against arguments that arrived as JSON. The MCP
/// server front door; the command line goes through [`args::parse_argv`] and
/// then [`run`].
pub fn call(name: &str, fields: Map<String, Value>) -> Result<Value> {
    let cmd = spec::find(name).ok_or_else(|| {
        anyhow!("there is no command called {name}. Ask for the tool list, or run `roughcut-cli`.")
    })?;
    let args = args::Args::new(cmd, fields)?;
    run(cmd, &args)
}
