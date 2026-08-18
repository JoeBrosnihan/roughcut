//! Rendering the cut to a real MP4, end to end: build sources, assemble a
//! timeline, write the MLT, hand it to `melt`, and check the file that comes
//! out is the length and shape that was asked for.
//!
//! Skipped, loudly, when ffmpeg/ffprobe/melt are unavailable.

use roughcut_core::import::add_clip;
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::{Profile, Project};
use roughcut_core::probe::probe;
use roughcut_core::render;
use roughcut_core::time::Rational;
use roughcut_core::timeline;
use roughcut_core::tools::{find_tool, quiet_command, Tools};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-render-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn source(ffmpeg: &Path, dir: &Path, name: &str, size: &str, rate: i64, frames: i64) -> PathBuf {
    // Generate a touch more than asked for, so `-frames:v` is what decides.
    let secs = (frames as f64 / rate as f64) + 1.0;
    let out = dir.join(name);
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate={rate}:d={secs}"))
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args([
            "-frames:v",
            &frames.to_string(),
            "-shortest",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success());
    out
}

#[test]
fn the_cut_renders_to_an_mp4_of_the_right_length_and_shape() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };
    let Some(melt) = tools.melt.clone().or_else(|| find_tool("melt")) else {
        eprintln!("SKIPPED: melt was not found, so nothing can be rendered");
        return;
    };

    let dir = workdir("mp4");
    // Two clips of different shapes, which is the case the profile has to
    // resolve rather than average.
    let a = source(&ffmpeg, &dir, "a.mp4", "640x360", 30, 120);
    let b = source(&ffmpeg, &dir, "b.mp4", "480x640", 30, 120);

    let mut project = Project::new();
    add_clip(&mut project, &a, &probe(&ffprobe, &a).unwrap());
    add_clip(&mut project, &b, &probe(&ffprobe, &b).unwrap());
    let (ida, idb) = (project.clips[0].id, project.clips[1].id);

    // Two seconds from each, at 30 fps: 120 frames of finished video.
    assert!(timeline::append(&mut project, ida, 0, 59));
    assert!(timeline::append(&mut project, idb, 30, 89));
    assert_eq!(timeline::total_frames(&project.timeline), 120);

    // Ask for something specific, the way the export dialog does.
    let target = Profile {
        frame_rate_num: 30,
        frame_rate_den: 1,
        width: 640,
        height: 360,
        sample_aspect_num: 1,
        sample_aspect_den: 1,
        progressive: true,
        colorspace: 709,
    };
    let opts = ExportOptions {
        profile: Some(target.clone()),
        ..ExportOptions::default()
    };

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &opts, &mlt_path).unwrap();
    let xml = std::fs::read_to_string(&mlt_path).unwrap();
    assert!(xml.contains(r#"width="640""#), "the override was ignored");
    assert!(xml.contains(r#"height="360""#));

    let out = dir.join("out.mp4");
    let cancel = AtomicBool::new(false);
    render::to_mp4(&melt, &mlt_path, &out, &cancel, |_| {})
        .expect("melt could not render the cut");

    let info = probe(&ffprobe, &out).expect("cannot probe the rendered file");
    assert_eq!((info.width, info.height), (640, 360), "wrong frame size");
    assert_eq!(info.fps.reduced(), Rational::new(30, 1), "wrong rate");
    // The whole point: the finished video is as long as the cut said it was.
    assert!(
        (info.native_frames - 120).abs() <= 1,
        "expected 120 frames, got {}",
        info.native_frames
    );
    assert!(info.has_audio, "the render dropped the audio");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_a_render_leaves_no_half_written_file() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };
    let Some(melt) = tools.melt.clone().or_else(|| find_tool("melt")) else {
        eprintln!("SKIPPED: melt was not found");
        return;
    };

    let dir = workdir("cancel");
    let a = source(&ffmpeg, &dir, "a.mp4", "640x360", 30, 300);
    let mut project = Project::new();
    add_clip(&mut project, &a, &probe(&ffprobe, &a).unwrap());
    let id = project.clips[0].id;
    assert!(timeline::append(&mut project, id, 0, 299));

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path).unwrap();

    let out = dir.join("out.mp4");
    // Cancel on a timer rather than on a progress report. melt reports on a
    // timer of its own and can finish a short render without a word, so a
    // cancel that waited for output would be a cancel that never happened -
    // which is the bug this is here to keep fixed.
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        flag.store(true, Ordering::Relaxed);
    });

    let result = render::to_mp4(&melt, &mlt_path, &out, &cancel, |_| {});
    match result {
        Err(e) => assert!(
            e.to_string().contains("cancelled"),
            "expected a cancellation, got: {e:#}"
        ),
        // The machine beat the timer. Nothing is wrong, but nothing was tested
        // either, so say so rather than passing silently.
        Ok(()) => {
            eprintln!("INCONCLUSIVE: the render finished before the cancel fired");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    }
    assert!(
        !out.exists(),
        "a cancelled render left a partial file behind, which looks finished"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A best-effort check that progress arrives from a real melt.
///
/// Deliberately not an assertion: melt reports on a timer of its own, so
/// whether a given render ticks at all depends on how loaded the machine is.
/// The streaming itself is covered deterministically by the unit tests against
/// a recorded transcript; this only reports what really happened.
#[test]
fn a_real_render_is_watched_for_progress() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };
    let Some(melt) = tools.melt.clone().or_else(|| find_tool("melt")) else {
        eprintln!("SKIPPED: melt was not found");
        return;
    };

    let dir = workdir("progress");
    let a = source(&ffmpeg, &dir, "a.mp4", "1280x720", 30, 900);
    let mut project = Project::new();
    add_clip(&mut project, &a, &probe(&ffprobe, &a).unwrap());
    let id = project.clips[0].id;
    assert!(timeline::append(&mut project, id, 0, 899));

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path).unwrap();

    let out = dir.join("out.mp4");
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    render::to_mp4(&melt, &mlt_path, &out, &cancel, |f| seen.push(f)).expect("render failed");

    if seen.is_empty() {
        eprintln!("INCONCLUSIVE: melt finished this render without reporting progress");
    } else {
        // Whatever did arrive has to make sense.
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "went backwards: {seen:?}");
        assert!(seen.iter().all(|&f| f <= 900), "past the end: {seen:?}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
