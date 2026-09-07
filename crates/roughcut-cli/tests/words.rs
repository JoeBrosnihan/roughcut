//! Finding a sentence and cutting exactly it.
//!
//! This is the part of the surface that has no equivalent in the window
//! keyboard: an agent cannot watch footage, so the transcript is the only way
//! it can answer "put the bit where I say that on the timeline". Everything
//! here runs against the transcript Roughcut actually recorded from whisper —
//! `crates/roughcut-core/tests/fixtures_whisper.json` — rather than one
//! written from memory, because a fixture invented to match the code proves
//! only that the code matches itself.
//!
//! A separate integration test from `editing.rs` on purpose: these need
//! `ROUGHCUT_CONFIG_DIR` pointed at a scratch cache, and that is process wide.

use roughcut_core::model::{ClipId, Project, SourceClip};
use roughcut_core::transcript::Transcript;
use roughcut_core::{paths, project_io, transcript, whisper};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Once;

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

/// One scratch configuration directory for the whole binary. Every test in
/// this file shares it and uses its own file names, because the environment is
/// process wide and these run alongside each other.
fn scratch() -> PathBuf {
    static ONCE: Once = Once::new();
    let dir = std::env::temp_dir().join(format!("roughcut-cli-words-{}", std::process::id()));
    ONCE.call_once(|| {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("cannot make a scratch directory");
        std::env::set_var("ROUGHCUT_CONFIG_DIR", &dir);
        std::env::remove_var("ROUGHCUT_PROJECT");
    });
    dir
}

/// The transcript Roughcut recorded from whisper on a real clip.
fn recorded() -> Transcript {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../roughcut-core/tests/fixtures_whisper.json");
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    whisper::parse_json(&json).expect("the recorded whisper output should parse")
}

/// A bin clip a minute long, comfortably past the last word in the fixture.
fn bin_clip(path: &Path) -> SourceClip {
    SourceClip {
        id: ClipId::new(),
        still: false,
        audio_only: false,
        path: path.to_path_buf(),
        proxy_path: None,
        duration_frames: 1800,
        native_frames: 1800,
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

/// A project at a whole 30fps, so the frame numbers below are the obvious ones.
fn project_at_30(clips: Vec<SourceClip>, path: &Path) {
    let mut project = Project::new();
    project.profile.frame_rate_num = 30;
    project.profile.frame_rate_den = 1;
    project.clips = clips;
    project_io::save(&project, path).unwrap();
}

/// A bin clip whose file exists, cached transcript and all, exactly as the
/// window would have left it.
fn project_with_transcript(tag: &str) -> (PathBuf, PathBuf) {
    let dir = scratch();
    let media = dir.join(format!("{tag}.mp4"));
    std::fs::write(&media, b"not really a video, but a real file").unwrap();

    let project_path = dir.join(format!("{tag}.roughcut"));
    project_at_30(vec![bin_clip(&media)], &project_path);

    // Written where the window writes it, keyed the way the window keys it.
    // If these two ever disagree the CLI silently sees no transcripts at all,
    // which is why the key is not spelled out again here.
    let cache = paths::transcript_cache_dir().expect("the scratch config directory is set");
    std::fs::create_dir_all(&cache).unwrap();
    let file = transcript::cache_file(&cache, paths::fingerprint(&media, &[]));
    std::fs::write(&file, serde_json::to_string(&recorded()).unwrap()).unwrap();

    (project_path, media)
}

fn call(path: &Path, command: &str, args: Value) -> Value {
    let mut fields = args.as_object().cloned().unwrap_or_default();
    fields.insert("project".into(), json!(path.to_string_lossy()));
    ops::call(command, fields).unwrap_or_else(|e| panic!("{command} failed: {e:#}"))
}

#[test]
fn the_cache_the_window_fills_is_the_cache_the_commands_read() {
    let (project, _) = project_with_transcript("shared");
    let r = call(&project, "transcript", json!({ "clip": "shared", "words": true }));
    assert_eq!(r["transcribed"], true, "the cached transcript was not found: {r}");
    // Ten words survive; the blank, the two speaker markers and the empty
    // entries in the recording are not words and must not be selectable.
    assert_eq!(r["words"], 10, "{}", r["text"]);
    // Paragraphs, as the document view shows them — whisper's speaker changes
    // become breaks rather than disappearing entirely.
    assert_eq!(
        r["text"],
        "What is that?\n\nIt's fine?\n\nI've been there.\n\nIt's fine."
    );

    let listed = r["word_list"].as_array().unwrap();
    assert_eq!(listed[0]["text"], "What");
    assert_eq!(listed[0]["i"], 0);
    // 24.12 seconds at 30fps.
    assert_eq!(listed[0]["frame"], 723);
    assert!(listed.iter().all(|w| !w["text"].as_str().unwrap().contains(">>")));
}

/// A clip with no transcript says so rather than looking empty. "No hits" and
/// "nothing has been transcribed yet" are entirely different answers.
#[test]
fn a_clip_without_a_transcript_is_reported_not_skipped() {
    let dir = scratch();
    let media = dir.join("never-transcribed.mp4");
    std::fs::write(&media, b"a file with no transcript beside it").unwrap();
    let path = dir.join("bare.roughcut");
    project_at_30(vec![bin_clip(&media)], &path);

    let r = call(&path, "transcript", json!({ "clip": "never" }));
    assert_eq!(r["transcribed"], false);
    assert!(r["why"].as_str().unwrap().contains("transcribe"), "{r}");

    let s = call(&path, "search", json!({ "query": "anything" }));
    assert!(s["hits"].as_array().unwrap().is_empty());
    assert_eq!(
        s["clips_without_a_transcript"][0], "never-transcribed.mp4",
        "an empty search must not look like a searched-and-found-nothing"
    );
}

/// The whole point: ask for words, get frames.
#[test]
fn a_search_hands_back_the_frames_to_cut() {
    let (project, _) = project_with_transcript("search");

    let r = call(&project, "search", json!({ "query": "been there" }));
    let hits = r["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{r}");
    let hit = &hits[0];
    assert_eq!(hit["said"], "been there.");
    assert_eq!(hit["from"], 6);
    assert_eq!(hit["to"], 7);
    assert!(
        hit["context"].as_str().unwrap().contains("I've been there."),
        "a hit carries enough either side to read: {}",
        hit["context"]
    );
    // Frames, ready to hand straight to `cut-words`.
    assert!(hit["in"].as_i64().unwrap() < hit["out"].as_i64().unwrap());
    assert!(!hit["at"]["timecode"].as_str().unwrap().is_empty());

    // Punctuation and case are not part of the question.
    assert_eq!(
        call(&project, "search", json!({ "query": "THAT" }))["hits"][0]["said"],
        "that?"
    );
    // A phrase that is only two separate words is not a phrase.
    assert!(call(&project, "search", json!({ "query": "what fine" }))["hits"]
        .as_array()
        .unwrap()
        .is_empty());
    // The same phrase twice is found twice.
    assert_eq!(
        call(&project, "search", json!({ "query": "it's" }))["hits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

/// A search result fed straight into `cut-words` has to land on the timeline
/// covering what was actually said. This is the round trip the whole feature
/// is for, so it is checked end to end rather than in halves.
#[test]
fn what_the_search_found_is_what_lands_on_the_timeline() {
    let (project, _) = project_with_transcript("roundtrip");

    let hit = call(&project, "search", json!({ "query": "been there" }))["hits"][0].clone();
    let (from, to) = (hit["from"].as_i64().unwrap(), hit["to"].as_i64().unwrap());

    let cut = call(&project, "cut-words", json!({ "clip": "roundtrip", "from": from, "to": to }));
    assert_eq!(cut["said"], "been there.");
    assert_eq!(cut["timeline"]["items"], 1);

    let saved = project_io::load(&project).unwrap();
    let item = saved.timeline[0];
    assert_eq!(
        (item.in_frame, item.out_frame),
        (hit["in"].as_i64().unwrap(), hit["out"].as_i64().unwrap()),
        "the cut is exactly the range the search promised"
    );

    // The words are spoken between these frames, with a little padding, so the
    // cut must contain them and not be wildly longer than they are.
    let words: Vec<_> = recorded().words().cloned().collect();
    let spoken_from = transcript::ms_to_frame(words[from as usize].start_ms, saved.fps());
    let spoken_to = transcript::ms_to_frame(words[to as usize].end_ms, saved.fps());
    assert!(item.in_frame <= spoken_from, "the cut starts before the first word");
    assert!(item.out_frame >= spoken_to, "and ends after the last");
    assert!(
        item.len() < (spoken_to - spoken_from) + 60,
        "padding is a fraction of a second, not seconds: {} frames for {} spoken",
        item.len(),
        spoken_to - spoken_from
    );

    // The marks follow, so opening the project in the window shows the cut.
    assert_eq!(saved.clips[0].mark_in, Some(item.in_frame));
    assert_eq!(saved.clips[0].mark_out, Some(item.out_frame));
}

/// Word indices are the caller's only handle on a transcript, so an index past
/// the end has to say how many there are rather than cut something arbitrary.
#[test]
fn a_word_index_past_the_end_is_refused() {
    let (project, _) = project_with_transcript("bounds");
    let mut fields = serde_json::Map::new();
    fields.insert("project".into(), json!(project.to_string_lossy()));
    fields.insert("clip".into(), json!("bounds"));
    fields.insert("from".into(), json!(0));
    fields.insert("to".into(), json!(99));
    let e = ops::call("cut-words", fields).unwrap_err().to_string();
    assert!(e.contains("10 words"), "{e}");
    assert!(project_io::load(&project).unwrap().timeline.is_empty());
}
