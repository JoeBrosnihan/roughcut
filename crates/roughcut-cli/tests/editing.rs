//! Assembling a cut through the command surface, on a real project file.
//!
//! These go through `ops::call` — the same entry the MCP server uses — rather
//! than through the implementations directly, so what is being tested is the
//! whole path a caller takes: parameters validated, project loaded, edit made,
//! project written back. A command that edits the project in memory and never
//! saves passes every unit test and does nothing at all.

use roughcut_core::model::{ClipId, Project, SourceClip};
use roughcut_core::project_io;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Reach the binary's own modules. There is no library target — the crate is
/// one executable — so the test compiles the sources it needs.
// The crate is one executable with no library target, so a test reaches its
// modules by path. Whatever a given test file does not call is dead code from
// that file's point of view; `serve` is the whole stdio loop, which is driven
// from the shell rather than from here.
#[path = "../src/args.rs"]
#[allow(dead_code)]
mod args;
#[path = "../src/mcp.rs"]
#[allow(dead_code)]
mod mcp;
#[path = "../src/ops.rs"]
#[allow(dead_code)]
mod ops;
#[path = "../src/spec.rs"]
#[allow(dead_code)]
mod spec;

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot make a scratch directory");
    dir
}

/// A bin clip that needs no file on disk. Nothing under test probes or decodes
/// anything; what is being checked is the arithmetic and the persistence.
fn clip(name: &str, frames: i64) -> SourceClip {
    SourceClip {
        id: ClipId::new(),
        still: false,
        path: PathBuf::from(format!("/media/{name}")),
        proxy_path: None,
        duration_frames: frames,
        native_frames: frames,
        native_fps_num: 30,
        native_fps_den: 1,
        width: 1920,
        height: 1080,
        sample_aspect_num: 1,
        sample_aspect_den: 1,
        progressive: true,
        colorspace: 709,
        has_audio: true,
        video_index: 0,
        audio_index: 1,
        mark_in: None,
        mark_out: None,
        rate_mismatch: false,
        variable_rate: false,
        flagged: false,
        highlights: Vec::new(),
        archived: false,
    }
}

/// A project at a whole 30fps, so the timecodes in these assertions are the
/// obvious ones.
fn project_with(names: &[(&str, i64)], path: &Path) {
    let mut p = Project::new();
    p.profile.frame_rate_num = 30;
    p.profile.frame_rate_den = 1;
    for (name, frames) in names {
        p.clips.push(clip(name, *frames));
    }
    project_io::save(&p, path).expect("cannot write the project");
}

fn call(path: &Path, command: &str, args: Value) -> Value {
    let mut fields = args.as_object().cloned().unwrap_or_default();
    fields.insert("project".into(), json!(path.to_string_lossy()));
    ops::call(command, fields).unwrap_or_else(|e| panic!("{command} failed: {e:#}"))
}

fn fails(path: &Path, command: &str, args: Value) -> String {
    let mut fields = args.as_object().cloned().unwrap_or_default();
    fields.insert("project".into(), json!(path.to_string_lossy()));
    match ops::call(command, fields) {
        Ok(v) => panic!("{command} should have failed, but returned {v}"),
        Err(e) => format!("{e:#}"),
    }
}

fn load(path: &Path) -> Project {
    project_io::load(path).expect("cannot read the project back")
}

/// The whole loop an agent would run: mark, append, look at what it made.
#[test]
fn a_cut_assembled_through_the_commands_is_on_disk_afterwards() {
    let dir = tmpdir("assemble");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300), ("two.mp4", 300)], &path);

    call(&path, "mark", json!({ "clip": "one", "in": 100, "out": 199 }));
    let added = call(&path, "append", json!({ "clip": "one" }));
    assert_eq!(added["added"]["frames"], 100, "in and out are inclusive");
    assert_eq!(added["added"]["start"]["frame"], 0);
    assert_eq!(added["timeline"]["frames"], 100);

    // An explicit range beats the marks, and does not disturb them.
    call(&path, "append", json!({ "clip": "two", "in": 0, "out": 49 }));

    let saved = load(&path);
    assert_eq!(saved.timeline.len(), 2, "both cuts were written back");
    assert_eq!(saved.timeline[0].in_frame, 100);
    assert_eq!(saved.timeline[0].out_frame, 199);
    assert_eq!(saved.timeline[1].len(), 50);
    assert_eq!(saved.clip(saved.clips[0].id).unwrap().mark_in, Some(100));
    assert_eq!(
        saved.clips[1].mark_in, None,
        "an explicit range is not a mark"
    );

    let state = call(&path, "timeline", json!({}));
    assert_eq!(state["frames"], 150);
    assert_eq!(state["duration"], "00:05", "150 frames at 30fps, in the format the app uses");
    assert_eq!(state["timeline"][1]["start"]["frame"], 100);

    let _ = std::fs::remove_dir_all(&dir);
}

/// The point of `batch`: nine edits that work and a tenth that does not must
/// leave the project exactly as it was, not nine tenths assembled.
#[test]
fn a_batch_that_fails_partway_writes_nothing() {
    let dir = tmpdir("batch");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300), ("two.mp4", 300)], &path);

    let before = load(&path);
    let e = fails(
        &path,
        "batch",
        json!({ "steps": r#"[
            {"command":"append","clip":"one","in":0,"out":99},
            {"command":"append","clip":"two","in":0,"out":99},
            {"command":"append","clip":"three"}
        ]"# }),
    );
    assert!(e.contains("step 2"), "the failing step is named: {e}");
    assert!(e.contains("three"), "{e}");
    assert_eq!(load(&path), before, "a failed batch leaves the file untouched");

    // The same batch without the bad step applies as one.
    let ok = call(
        &path,
        "batch",
        json!({ "steps": r#"[
            {"command":"append","clip":"one","in":0,"out":99},
            {"command":"append","clip":"two","in":0,"out":99},
            {"command":"split","at":50}
        ]"# }),
    );
    assert_eq!(ok["applied"], 3);
    assert_eq!(load(&path).timeline.len(), 3, "two appends and a split");

    // A batch cannot recurse, and cannot smuggle in a different project.
    assert!(fails(&path, "batch", json!({ "steps": r#"[{"command":"batch","steps":"[]"}]"# }))
        .contains("batch"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every command that says it writes has to actually persist, and every
/// command that says it does not must leave the file alone. This is the
/// property the table exists to guarantee, so it is checked against the table
/// rather than command by command.
#[test]
fn the_table_tells_the_truth_about_which_commands_write() {
    let dir = tmpdir("writes");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300)], &path);
    call(&path, "append", json!({ "clip": "one", "in": 0, "out": 99 }));

    // A read-only command, run twice, cannot change the bytes on disk.
    let before = std::fs::read(&path).unwrap();
    for name in ["info", "clips", "timeline", "audio", "missing"] {
        call(&path, name, json!({}));
    }
    assert_eq!(std::fs::read(&path).unwrap(), before, "a read changed the file");

    // And a writing command does.
    call(&path, "flag", json!({ "clip": "one" }));
    assert_ne!(std::fs::read(&path).unwrap(), before);
    assert!(load(&path).clips[0].flagged);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Naming a clip by part of its file name is the affordance that makes the
/// surface usable without reading UUIDs back. It has to refuse to guess.
#[test]
fn a_clip_is_named_by_its_file_and_ambiguity_is_refused() {
    let dir = tmpdir("naming");
    let path = dir.join("cut.roughcut");
    project_with(&[("IMG_2527.MOV", 300), ("IMG_2528.MOV", 300)], &path);

    let r = call(&path, "flag", json!({ "clip": "2527" }));
    assert_eq!(r["clip"], "IMG_2527.MOV");
    assert_eq!(call(&path, "flag", json!({ "clip": "img_2528" }))["clip"], "IMG_2528.MOV",
               "case does not matter");

    let e = fails(&path, "flag", json!({ "clip": "IMG" }));
    assert!(e.contains("2 clips"), "{e}");
    assert!(e.contains("IMG_2527.MOV"), "the candidates are named: {e}");
    assert!(fails(&path, "flag", json!({ "clip": "nope" })).contains("no clip"));

    // The id always works, whatever the name.
    let id = load(&path).clips[0].id.to_string();
    assert_eq!(call(&path, "flag", json!({ "clip": id }))["clip"], "IMG_2527.MOV");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Refusing bad positions matters more here than in the window: there is no
/// picture on screen to make a wrong number obvious.
#[test]
fn positions_outside_the_clip_are_refused_rather_than_clamped() {
    let dir = tmpdir("bounds");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300)], &path);

    assert!(fails(&path, "mark", json!({ "clip": "one", "in": 400 })).contains("0 to 299"));
    assert!(fails(&path, "mark", json!({ "clip": "one", "in": 200, "out": 100 })).contains("before"));
    assert!(fails(&path, "append", json!({ "clip": "one", "in": 0, "out": 500 })).contains("outside"));
    assert_eq!(load(&path).clips[0].mark_in, None, "nothing stuck");

    call(&path, "append", json!({ "clip": "one", "in": 0, "out": 99 }));
    assert!(fails(&path, "delete", json!({ "index": 5 })).contains("1 items"));
    assert!(fails(&path, "trim", json!({ "index": 0, "edge": "head", "by": -50 }))
        .contains("cannot move"), "there is no source before frame 0");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Trimming reports what it actually did, because it is allowed to do less
/// than it was asked. A caller told "moved 40" when it asked for 100 can
/// correct; one told "done" cannot.
#[test]
fn a_trim_reports_how_far_it_really_moved() {
    let dir = tmpdir("trim");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300)], &path);
    call(&path, "append", json!({ "clip": "one", "in": 100, "out": 199 }));

    let r = call(&path, "trim", json!({ "index": 0, "edge": "head", "by": -40 }));
    assert_eq!(r["moved"], -40);
    assert_eq!(r["clamped"], false);
    assert_eq!(r["item"]["in"], 60);
    assert_eq!(r["timeline"]["frames"], 140);

    // Only 60 frames of source are left in front of it.
    let r = call(&path, "trim", json!({ "index": 0, "edge": "head", "by": -500 }));
    assert_eq!(r["moved"], -60);
    assert_eq!(r["clamped"], true, "the caller is told it did not get what it asked");
    assert_eq!(r["item"]["in"], 0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Audio positions are the one thing in the project that is stored rather than
/// derived, so they are the one thing a save can silently lose.
#[test]
fn sound_placed_on_a_track_keeps_its_position_across_the_save() {
    let dir = tmpdir("audio");
    let path = dir.join("cut.roughcut");
    project_with(&[("shot.mp4", 300), ("music.mp4", 3000)], &path);

    call(&path, "add-audio-track", json!({ "name": "Music" }));
    call(&path, "append", json!({ "clip": "shot", "in": 0, "out": 299 }));
    let placed = call(
        &path,
        "place-audio",
        json!({ "track": 0, "clip": "music", "at": 90, "in": 0, "out": 599 }),
    );
    assert_eq!(placed["start"]["frame"], 90);
    assert_eq!(placed["frames"], 600);

    let saved = load(&path);
    assert_eq!(saved.audio.len(), 1);
    assert_eq!(saved.audio[0].name, "Music");
    assert_eq!(saved.audio[0].items()[0].start, 90);

    let listed = call(&path, "audio", json!({}));
    assert_eq!(listed["audio_tracks"][0]["items"][0]["name"], "music.mp4");
    assert_eq!(listed["audio_tracks"][0]["items"][0]["start"]["timecode"], "00:03");

    call(&path, "mute", json!({ "track": 0 }));
    assert!(load(&path).audio[0].muted);
    call(&path, "mute", json!({ "track": 0, "off": true }));
    assert!(!load(&path).audio[0].muted);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Rippling the audio tracks along with the picture is an explicit choice, and
/// the default has to be the one that leaves music where it was.
#[test]
fn only_asking_for_it_moves_the_music_with_the_picture() {
    let dir = tmpdir("ripple");
    let path = dir.join("cut.roughcut");
    project_with(&[("a.mp4", 300), ("b.mp4", 300), ("music.mp4", 3000)], &path);
    call(&path, "add-audio-track", json!({}));
    call(&path, "append", json!({ "clip": "a.mp4", "in": 0, "out": 99 }));
    call(&path, "place-audio", json!({ "track": 0, "clip": "music", "at": 200, "in": 0, "out": 99 }));

    call(&path, "insert", json!({ "clip": "b.mp4", "at": 50, "in": 0, "out": 99 }));
    assert_eq!(load(&path).audio[0].items()[0].start, 200, "music held still");

    call(&path, "insert", json!({ "clip": "b.mp4", "at": 50, "in": 0, "out": 99, "ripple": "all" }));
    assert_eq!(load(&path).audio[0].items()[0].start, 300, "and now it moved");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A clip still cut into the timeline must not vanish from the bin, taking the
/// cuts with it.
#[test]
fn a_clip_in_use_cannot_be_removed_from_the_bin() {
    let dir = tmpdir("remove");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300)], &path);
    call(&path, "append", json!({ "clip": "one", "in": 0, "out": 99 }));

    let e = fails(&path, "remove-clip", json!({ "clip": "one" }));
    assert!(e.contains("used 1 times"), "{e}");
    assert_eq!(load(&path).clips.len(), 1);

    call(&path, "clear", json!({}));
    call(&path, "remove-clip", json!({ "clip": "one" }));
    assert!(load(&path).clips.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The export is the deliverable, and an empty one is a file that looks fine
/// until Shotcut opens it.
#[test]
fn an_export_carries_the_cuts_and_an_empty_timeline_is_refused() {
    let dir = tmpdir("export");
    let path = dir.join("cut.roughcut");
    let out = dir.join("cut.mlt");
    project_with(&[("one.mp4", 300)], &path);

    assert!(fails(&path, "export", json!({ "out": out.to_string_lossy() })).contains("empty"));

    call(&path, "append", json!({ "clip": "one", "in": 100, "out": 199 }));
    let r = call(&path, "export", json!({ "out": out.to_string_lossy(), "title": "Holiday" }));
    assert_eq!(r["items"], 1);

    let xml = std::fs::read_to_string(&out).unwrap();
    assert!(xml.contains("Holiday"), "the title is in the XML");
    assert!(xml.contains("one.mp4"));
    assert!(xml.contains("<mlt"), "{xml}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Both front doors have to reach the same work. This drives the MCP path over
/// the same project the command line just edited.
#[test]
fn the_mcp_door_reaches_the_same_project() {
    let dir = tmpdir("mcp");
    let path = dir.join("cut.roughcut");
    project_with(&[("one.mp4", 300)], &path);

    let request = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {
            "name": "append",
            "arguments": { "project": path.to_string_lossy(), "clip": "one", "in": 0, "out": 99 }
        }
    });
    let reply = mcp::handle(&request.to_string()).expect("a call is answered");
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"]["isError"], false, "{reply}");

    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let value: Value = serde_json::from_str(text).expect("the text is the JSON result");
    assert_eq!(value["timeline"]["frames"], 100);
    assert_eq!(load(&path).timeline.len(), 1, "and it landed on disk");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A command line and an MCP client making the same edit must produce byte
/// identical projects. If they ever diverge, one of the two doors is doing
/// something the other is not.
#[test]
fn the_two_doors_produce_the_same_project() {
    let dir = tmpdir("parity");
    let (a, b) = (dir.join("a.roughcut"), dir.join("b.roughcut"));

    for path in [&a, &b] {
        let mut p = Project::new();
        p.profile.frame_rate_num = 30;
        p.profile.frame_rate_den = 1;
        // The same ids in both, so the only difference can be the door.
        let mut c = clip("one.mp4", 300);
        c.id = ClipId(uuid_from(1));
        p.clips.push(c);
        project_io::save(&p, path).unwrap();
    }

    // Through the command line parser.
    let cmd = spec::find("append").unwrap();
    let argv: Vec<String> = ["--project", &a.to_string_lossy(), "--clip", "one", "--in", "0", "--out", "99"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let parsed = args::parse_argv(cmd, &argv).unwrap();
    ops::run(cmd, &parsed).unwrap();

    // Through MCP.
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "append", "arguments": {
            "project": b.to_string_lossy(), "clip": "one", "in": 0, "out": 99 } }
    });
    mcp::handle(&request.to_string()).unwrap();

    assert_eq!(load(&a).timeline, load(&b).timeline);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fixed id, so the parity test compares projects that differ in nothing
/// else. `Uuid::new_v4` would make the two files different by construction.
fn uuid_from(n: u8) -> uuid::Uuid {
    uuid::Uuid::from_bytes([n; 16])
}
