//! A photograph on the timeline, end to end.
//!
//! A still is the one thing in the bin with no duration of its own: ffprobe
//! reports no frame count, no real rate, and a nominal 25/1 it invented. The
//! length therefore comes from the project, and the whole question is whether
//! that invented length survives everything downstream — the model, the MLT,
//! and the renderer that reads it.
//!
//! Skipped, loudly, when ffmpeg/ffprobe/melt are unavailable.

use roughcut_core::import::{add_clip, STILL_MAX_SECONDS, STILL_SECONDS};
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::Project;
use roughcut_core::probe::probe;
use roughcut_core::render;
use roughcut_core::timeline;
use roughcut_core::tools::{find_tool, quiet_command, Tools};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-still-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn video(ffmpeg: &Path, dir: &Path, frames: i64) -> PathBuf {
    let out = dir.join("clip.mp4");
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=1920x1080:rate=30:d=10")
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

fn photo(ffmpeg: &Path, dir: &Path, name: &str) -> PathBuf {
    let out = dir.join(name);
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=1920x1080:d=1")
        .args(["-frames:v", "1"])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success());
    out
}

#[test]
fn a_photo_arrives_with_a_length_and_a_marked_range() {
    let Some(ffprobe) = Tools::discover().ffprobe else {
        eprintln!("SKIPPED: ffprobe is required");
        return;
    };
    let Some(ffmpeg) = Tools::discover().ffmpeg else {
        eprintln!("SKIPPED: ffmpeg is required");
        return;
    };
    let dir = workdir("import");
    let png = photo(&ffmpeg, &dir, "still.png");

    let info = probe(&ffprobe, &png).expect("a PNG has to probe");
    assert!(info.still, "a .png is a photograph");
    assert_eq!(info.native_frames, 0, "nothing was measured, and nothing claimed");

    let mut project = Project::new();
    add_clip(&mut project, &png, &info);
    let clip = &project.clips[0];
    // Lengths are asserted in seconds, against the project's exact rational
    // rate. Using the nominal 30 would be wrong by two frames a minute at
    // 29.97, which is the sort of arithmetic this whole program exists to
    // avoid getting wrong.
    let fps = project.fps().as_f64();
    let seconds = |frames: i64| frames as f64 / fps;

    assert!(clip.still);
    assert!(
        (seconds(clip.duration_frames) - STILL_MAX_SECONDS as f64).abs() < 0.05,
        "a photo gets a minute of room to be stretched into, got {:.3} s",
        seconds(clip.duration_frames)
    );
    assert_eq!(clip.mark_in, Some(0));

    // The marked range is what `A` appends, so it is the length that matters.
    let (in_frame, out_frame) = clip.marked_range().unwrap();
    let marked = seconds(out_frame - in_frame + 1);
    assert!(
        (marked - STILL_SECONDS as f64).abs() < 0.05,
        "a photo arrives with ten seconds marked, got {marked:.3} s"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The one that would have caught a wrong producer: MLT has to hold the photo
/// for its length rather than showing one frame and moving on.
#[test]
fn a_cut_of_video_and_a_photo_renders_to_the_right_length() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };
    let Some(melt) = tools.melt.clone().or_else(|| find_tool("melt")) else {
        eprintln!("SKIPPED: melt was not found");
        return;
    };

    let dir = workdir("render");
    let clip = video(&ffmpeg, &dir, 90);
    let png = photo(&ffmpeg, &dir, "still.png");

    let mut project = Project::new();
    add_clip(&mut project, &clip, &probe(&ffprobe, &clip).unwrap());
    add_clip(&mut project, &png, &probe(&ffprobe, &png).unwrap());
    let (video_id, photo_id) = (project.clips[0].id, project.clips[1].id);

    // 90 frames of video, then 60 of photo, then 30 more of video: a still in
    // the middle of a cut, which is the case that has to work.
    assert!(timeline::append(&mut project, video_id, 0, 89));
    assert!(timeline::append(&mut project, photo_id, 0, 59));
    assert!(timeline::append(&mut project, video_id, 0, 29));
    let expected = 90 + 60 + 30;
    assert_eq!(timeline::total_frames(&project.timeline), expected);

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path).unwrap();
    let xml = std::fs::read_to_string(&mlt_path).unwrap();
    assert!(
        xml.contains("qimage"),
        "the photo must not be written as an avformat chain"
    );

    let out = dir.join("out.mp4");
    render::to_mp4(&melt, &mlt_path, &out, &AtomicBool::new(false), |_| {})
        .expect("melt could not render a cut containing a photo");

    let rendered = probe(&ffprobe, &out).expect("the render has to probe");
    assert_eq!(
        rendered.native_frames, expected,
        "the photo did not hold its length in the finished file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
