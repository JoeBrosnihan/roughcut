//! An audio track, end to end: model, MLT, and the file melt renders.
//!
//! The measurement is deliberately by frequency rather than by eye. The video
//! clip carries a 300 Hz tone and the audio track a 1000 Hz one, so "did both
//! survive" is a number rather than an opinion.
//!
//! Skipped, loudly, when ffmpeg/ffprobe/melt are unavailable.

use roughcut_core::audio::{AudioItem, AudioTrack};
use roughcut_core::import::add_clip;
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::Project;
use roughcut_core::probe::probe;
use roughcut_core::render;
use roughcut_core::timeline;
use roughcut_core::tools::{find_tool, quiet_command, Tools};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-audio-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A clip whose sound is a single tone, so it can be found again afterwards.
fn toned(ffmpeg: &Path, dir: &Path, name: &str, hz: u32, seconds: u32) -> PathBuf {
    let out = dir.join(name);
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size=640x360:rate=30:d={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency={hz}:duration={seconds}"))
        .args([
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac",
            "-shortest",
        ])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success());
    out
}

/// Loudest level in a narrow band around `hz`, in dB.
fn band(ffmpeg: &Path, file: &Path, hz: u32) -> f64 {
    let out = quiet_command(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(file)
        .args(["-af"])
        .arg(format!(
            "bandpass=f={hz}:width_type=h:w=80,volumedetect"
        ))
        .args(["-f", "null", "-"])
        .output()
        .expect("cannot run ffmpeg");
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .find(|l| l.contains("max_volume"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().trim_end_matches(" dB").parse().ok())
        .unwrap_or(-99.0)
}

#[test]
fn an_audio_track_survives_into_the_rendered_file() {
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
    let picture = toned(&ffmpeg, &dir, "picture.mp4", 300, 6);
    let sound = toned(&ffmpeg, &dir, "sound.mp4", 1000, 6);

    let mut project = Project::new();
    add_clip(&mut project, &picture, &probe(&ffprobe, &picture).unwrap());
    add_clip(&mut project, &sound, &probe(&ffprobe, &sound).unwrap());
    let (video_id, audio_id) = (project.clips[0].id, project.clips[1].id);

    // 120 frames of picture; the sound starts 30 frames in and runs 60.
    assert!(timeline::append(&mut project, video_id, 0, 119));
    let mut track = AudioTrack::new("A1");
    track.place(AudioItem {
        clip_id: audio_id,
        in_frame: 0,
        out_frame: 59,
        start: 30,
    });
    project.audio.push(track);

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path).unwrap();
    let xml = std::fs::read_to_string(&mlt_path).unwrap();
    assert!(xml.contains(r#"shotcut:audio">1"#), "no audio playlist:\n{xml}");
    assert!(xml.contains(r#"hide="video""#), "the track is not audio-only");
    // The thirty frames of silence before it are a real part of the track.
    assert!(xml.contains(r#"<blank length="30"/>"#), "no leading silence");

    let out = dir.join("out.mp4");
    render::to_mp4(&melt, &mlt_path, &out, &AtomicBool::new(false), |_| {})
        .expect("melt could not render a project with an audio track");

    let picture_tone = band(&ffmpeg, &out, 300);
    let track_tone = band(&ffmpeg, &out, 1000);
    // Both have to be there. A track written into the XML but missing its
    // `mix` transition renders silently, which is exactly the failure this
    // catches.
    assert!(
        picture_tone > -30.0,
        "the picture's own sound is missing ({picture_tone} dB)"
    );
    assert!(
        track_tone > -30.0,
        "the audio track is missing ({track_tone} dB)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A bed longer than the picture must not be cut off by it.
#[test]
fn the_programme_is_as_long_as_the_longest_track() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };
    let dir = workdir("length");
    let picture = toned(&ffmpeg, &dir, "picture.mp4", 300, 4);
    let sound = toned(&ffmpeg, &dir, "sound.mp4", 1000, 8);

    let mut project = Project::new();
    add_clip(&mut project, &picture, &probe(&ffprobe, &picture).unwrap());
    add_clip(&mut project, &sound, &probe(&ffprobe, &sound).unwrap());
    let (video_id, audio_id) = (project.clips[0].id, project.clips[1].id);

    assert!(timeline::append(&mut project, video_id, 0, 59));
    let mut track = AudioTrack::new("A1");
    track.place(AudioItem {
        clip_id: audio_id,
        in_frame: 0,
        out_frame: 179,
        start: 0,
    });
    project.audio.push(track);

    let mlt_path = dir.join("cut.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path).unwrap();
    let xml = std::fs::read_to_string(&mlt_path).unwrap();
    // 180 frames of sound against 60 of picture: the tractor runs to 179.
    assert!(
        xml.contains(r#"out="179""#),
        "the programme was cut to the picture:\n{xml}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Loudest level in a band, within a window of the file.
fn band_between(ffmpeg: &Path, file: &Path, hz: u32, from: f64, to: f64) -> f64 {
    let trimmed = file.with_extension(format!("{hz}-{from}.wav"));
    let ok = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-ss"])
        .arg(from.to_string())
        .args(["-t"])
        .arg((to - from).to_string())
        .args(["-i"])
        .arg(file)
        .arg(&trimmed)
        .status()
        .expect("cannot run ffmpeg")
        .success();
    assert!(ok);
    let db = band(ffmpeg, &trimmed, hz);
    let _ = std::fs::remove_file(&trimmed);
    db
}

/// The preview plays one pre-mixed bed rather than N live sources. This checks
/// the mix puts each piece where it belongs, which is the whole job: a bed
/// that is correct but two seconds early is worse than no bed.
#[test]
fn the_preview_bed_places_each_sound_at_its_own_time() {
    let Some(ffmpeg) = Tools::discover().ffmpeg else {
        eprintln!("SKIPPED: ffmpeg is required");
        return;
    };
    let dir = workdir("bed");
    let low = toned(&ffmpeg, &dir, "low.mp4", 300, 3);
    let high = toned(&ffmpeg, &dir, "high.mp4", 1000, 3);

    // Low at the very start, high four seconds in.
    let pieces = vec![
        roughcut_core::audio::MixPiece {
            path: low.clone(),
            from: 0.0,
            to: 2.0,
            at: 0.0,
        },
        roughcut_core::audio::MixPiece {
            path: high.clone(),
            from: 0.0,
            to: 2.0,
            at: 4.0,
        },
    ];
    let bed = dir.join("bed.wav");
    let args = roughcut_core::audio::mix_args(&pieces, &bed).expect("something to mix");
    let out = quiet_command(&ffmpeg).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "mixing failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let low_early = band_between(&ffmpeg, &bed, 300, 0.5, 1.5);
    let high_early = band_between(&ffmpeg, &bed, 1000, 0.5, 1.5);
    let low_late = band_between(&ffmpeg, &bed, 300, 4.5, 5.5);
    let high_late = band_between(&ffmpeg, &bed, 1000, 4.5, 5.5);

    // Each tone against itself in the other window, rather than against an
    // absolute floor. A bandpass leaks, and these tones come through AAC, so
    // the quiet reading is a noise floor around -36 dB rather than nothing at
    // all; the contrast is the real signal and is nearly 20 dB.
    assert!(
        low_early > low_late + 12.0,
        "the low tone is not confined to the start ({low_early:.1} then {low_late:.1} dB)"
    );
    assert!(
        high_late > high_early + 12.0,
        "the high tone is not confined to 4 s ({high_early:.1} then {high_late:.1} dB)"
    );

    // The strongest evidence, and the one that pins the timing: between them,
    // where neither piece is playing, there is nothing at all.
    let gap = band_between(&ffmpeg, &bed, 300, 2.5, 3.5).max(band_between(
        &ffmpeg, &bed, 1000, 2.5, 3.5,
    ));
    assert!(
        gap < -60.0,
        "the gap between the two sounds is not silent ({gap:.1} dB)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
