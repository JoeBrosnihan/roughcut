//! §10's correctness requirement: a proxy must have identical frame count and
//! frame rate to its source, so frame numbers map 1:1 and a mark made while
//! watching the proxy means the same thing in the exported XML.
//!
//! Skipped, loudly, when ffmpeg/ffprobe are unavailable.

use roughcut_core::model::{ClipId, SourceClip};
use roughcut_core::probe::probe;
use roughcut_core::proxy::{self, check_proxy};
use roughcut_core::tools::{quiet_command, Tools};
use std::path::{Path, PathBuf};

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-proxy-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A long-GOP NTSC-rate clip: the case where a careless proxy command would
/// resample and shift every frame number.
fn build_source(ffmpeg: &Path, dir: &Path) -> PathBuf {
    let out = dir.join("source.mp4");
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        // Taller than the 540p proxy height, so the proxy genuinely
        // downscales rather than blowing the picture up.
        .arg("testsrc2=size=1280x720:rate=30000/1001:d=8")
        .args([
            "-frames:v", "240", "-c:v", "libx264", "-preset", "ultrafast", "-g", "60",
            "-pix_fmt", "yuv420p",
        ])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success());
    out
}

#[test]
fn a_generated_proxy_maps_frame_for_frame() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required for the proxy test");
        return;
    };

    let dir = workdir("match");
    let source = build_source(&ffmpeg, &dir);
    let info = probe(&ffprobe, &source).unwrap();
    assert_eq!(info.native_frames, 240);

    let clip = SourceClip {
        id: ClipId::new(),
        still: false,
        audio_only: false,
        path: source.clone(),
        proxy_path: None,
        duration_frames: info.native_frames,
        native_frames: info.native_frames,
        native_fps_num: info.fps.num,
        native_fps_den: info.fps.den,
        width: info.width,
        height: info.height,
        sample_aspect_num: info.sample_aspect_num,
        sample_aspect_den: info.sample_aspect_den,
        progressive: info.progressive,
        colorspace: info.colorspace,
        has_audio: info.has_audio,
        video_index: info.video_index,
        audio_index: info.audio_index,
        mark_in: None,
        mark_out: None,
        rate_mismatch: false,
        variable_rate: false,
        flagged: false,
        highlights: Vec::new(),
        archived: false,
    };

    let proxy_dir = dir.join("proxies");
    let proxy_path = proxy::generate(&ffmpeg, &ffprobe, &clip, &info, &proxy_dir, proxy::Tier::Edit)
        .expect("proxy generation failed");
    assert!(proxy_path.exists());
    // The final name is the adoption contract: a session that reopens a
    // project trusts a file at this exact path to be a complete, verified
    // proxy, so the transcode-in-progress name must be gone.
    assert_eq!(proxy_path, proxy::proxy_path(&proxy_dir, clip.id));
    assert!(
        !proxy::partial_path(&proxy_dir, clip.id).exists(),
        "the in-progress file should have been renamed away"
    );

    let proxy_info = probe(&ffprobe, &proxy_path).unwrap();
    // The whole point: same count, same exact rational rate, smaller picture.
    assert_eq!(
        proxy_info.native_frames, info.native_frames,
        "proxy frame count drifted — frame numbers would no longer line up"
    );
    assert_eq!(
        proxy_info.fps.reduced(),
        info.fps.reduced(),
        "proxy frame rate drifted"
    );
    assert_eq!(proxy_info.height, proxy::PROXY_HEIGHT);
    assert!(
        proxy_info.width < info.width,
        "the proxy should be smaller than the source"
    );
    assert!(check_proxy(&info, &proxy_info).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A proxy that really was resampled must be rejected, not silently used.
#[test]
fn a_resampled_proxy_is_rejected_and_deleted() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };

    let dir = workdir("resampled");
    let source = build_source(&ffmpeg, &dir);
    let info = probe(&ffprobe, &source).unwrap();

    // Deliberately do what §10 forbids: force a different frame rate.
    let bad = dir.join("bad-proxy.mp4");
    let status = quiet_command(&ffmpeg)
        .args(["-y", "-v", "error"])
        .arg("-i")
        .arg(&source)
        .args(["-r", "25", "-vf", "scale=-2:540", "-c:v", "libx264", "-preset", "ultrafast"])
        .arg(&bad)
        .status()
        .unwrap();
    assert!(status.success());

    let bad_info = probe(&ffprobe, &bad).unwrap();
    let verdict = check_proxy(&info, &bad_info);
    assert!(
        verdict.is_err(),
        "a 25 fps proxy of 30000/1001 material must be rejected, got {bad_info:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
