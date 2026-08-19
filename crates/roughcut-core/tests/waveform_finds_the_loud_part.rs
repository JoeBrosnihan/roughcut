//! The audio envelope, end to end against real ffmpeg.
//!
//! The unit tests prove the reduction is right given samples. This proves the
//! samples arrive at all: that the command decodes what it should, that the
//! peaks land at the moment of the sound rather than somewhere else in the
//! clip, and that a clip with no audio track fails instead of hanging.
//!
//! Skipped, loudly, when ffmpeg is unavailable.

use roughcut_core::tools::{quiet_command, Tools};
use roughcut_core::waveform;
use std::path::{Path, PathBuf};

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-wave-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Ten seconds of near-silence with one loud second in the middle, from 4 s to
/// 5 s. Anything claiming to find the loud part has to point there.
fn clip_with_a_bang(ffmpeg: &Path, dir: &Path) -> PathBuf {
    let out = dir.join("bang.mp4");
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:d=10"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=10"])
        // Full volume for one second in the middle, 1/50th either side.
        .args([
            "-af",
            "volume=enable=between(t\\,4\\,5):volume=1.0, \
             volume=enable=lt(t\\,4)+gt(t\\,5):volume=0.02",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success(), "ffmpeg could not build the test clip");
    out
}

#[test]
fn the_peaks_land_on_the_loud_second() {
    let Some(ffmpeg) = Tools::discover().ffmpeg else {
        eprintln!("SKIPPED: ffmpeg is required");
        return;
    };
    let dir = workdir("bang");
    let clip = clip_with_a_bang(&ffmpeg, &dir);

    let buckets = 100; // one per tenth of a second
    let peaks = waveform::extract(&ffmpeg, &clip, buckets).expect("no peaks came back");
    assert_eq!(peaks.len(), buckets);

    // The loudest bucket must be inside the fifth second, which is buckets
    // 40 to 50 of 100 — the far boundary included, because the sound is still
    // at full level when it is cut off there.
    let loudest = peaks
        .iter()
        .enumerate()
        .max_by_key(|(_, &v)| v)
        .map(|(i, _)| i)
        .unwrap();
    assert!(
        (40..=50).contains(&loudest),
        "the loudest bucket is {loudest}, not in the loud second: {peaks:?}"
    );

    // And the quiet part has to actually look quiet, or the picture is useless
    // however well the maximum is placed. The AAC round trip smears the edges
    // of the enabled window, so this checks well clear of them.
    let loud = peaks[42];
    let quiet = peaks[..35].iter().copied().max().unwrap();
    assert!(
        loud > quiet * 3,
        "the loud second ({loud}) is not visibly louder than the rest ({quiet})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clip_with_no_audio_is_an_error_rather_than_a_hang() {
    let Some(ffmpeg) = Tools::discover().ffmpeg else {
        eprintln!("SKIPPED: ffmpeg is required");
        return;
    };
    let dir = workdir("silent");
    let out = dir.join("mute.mp4");
    let status = quiet_command(&ffmpeg)
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:d=2"])
        .args(["-an", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success());

    assert!(
        waveform::extract(&ffmpeg, &out, waveform::BUCKETS).is_err(),
        "a clip with no audio track should not produce a waveform"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
