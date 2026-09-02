// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Fixing footage that arrived on its side.
//!
//! Phones record landscape frames and describe the intended orientation in a
//! display matrix; when that matrix is missing or wrong the clip plays on its
//! side everywhere. The fix is to rewrite the matrix — not the pixels. That
//! makes the whole operation a stream copy: a second or two on a large file,
//! no generation loss, and, decisively, **the frame count and frame rate do
//! not change**, so every mark and every cut already made still means the
//! frame it meant before.
//!
//! Re-encoding would be the wrong tool here. It would cost minutes per clip,
//! degrade the picture, and — because encoders are free to alter frame counts
//! — risk shifting the timing the rest of Roughcut is careful to keep exact.

use crate::probe::{probe, MediaInfo};
use crate::tools::quiet_command;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Which way a quarter turn goes, from the viewer's side of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    Clockwise,
    CounterClockwise,
}

impl Turn {
    /// Degrees counter-clockwise, which is how ffmpeg's `-display_rotation`
    /// and the display matrix both count.
    fn degrees(self) -> i32 {
        match self {
            Turn::CounterClockwise => 90,
            Turn::Clockwise => 270,
        }
    }
}

/// Turn the file at `path` a quarter turn, in place.
///
/// Returns the file's new [`MediaInfo`]. The rotation is applied *relative* to
/// whatever the file already claimed, so pressing rotate four times returns a
/// clip exactly to where it started.
///
/// The original is only replaced once the rewritten file has been probed and
/// found to have the same frame count and rate — a rewrite that changed either
/// would silently invalidate existing marks, so it is discarded instead.
pub fn rotate_in_place(
    ffmpeg: &Path,
    ffprobe: &Path,
    path: &Path,
    turn: Turn,
) -> Result<MediaInfo> {
    let before = probe(ffprobe, path)
        .with_context(|| format!("cannot read {} before rotating it", path.display()))?;
    let target = (before.rotation + turn.degrees()).rem_euclid(360);

    let temp = temp_sibling(path);
    let _ = std::fs::remove_file(&temp);

    // The muxer is normally chosen from the file's extension, which is right
    // almost always. It is wrong for the codecs QuickTime never specified: a
    // .MOV holding VP9 or AV1 is something phones and screen recorders write
    // happily, but ffmpeg's `mov` muxer refuses to write it back out. The MP4
    // muxer accepts them and produces a file QuickTime and everything else
    // still open, so a rejection is retried that way rather than reported.
    let mut result = run_ffmpeg(ffmpeg, path, &temp, target, None)?;
    if !result.0 && mp4_family(path) {
        let _ = std::fs::remove_file(&temp);
        log::debug!(
            "the container's own muxer rejected {}; retrying as mp4",
            path.display()
        );
        result = run_ffmpeg(ffmpeg, path, &temp, target, Some("mp4"))?;
    }
    if !result.0 {
        let _ = std::fs::remove_file(&temp);
        bail!(
            "ffmpeg could not rotate {}: {}",
            path.display(),
            result.1.trim()
        );
    }

    let after = match probe(ffprobe, &temp) {
        Ok(after) => after,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e.context("the rotated file could not be read back"));
        }
    };

    if after.native_frames != before.native_frames || after.fps != before.fps {
        let _ = std::fs::remove_file(&temp);
        bail!(
            "rotating {} would have changed its timing ({} frames at {} became {} at {}) — \
             the original has been left alone",
            path.display(),
            before.native_frames,
            before.fps,
            after.native_frames,
            after.fps
        );
    }
    if after.rotation != target {
        let _ = std::fs::remove_file(&temp);
        bail!(
            "this file's container cannot record a rotation ({} asked for, {} written) — \
             it would have to be re-encoded, which Roughcut will not do to your original",
            target,
            after.rotation
        );
    }

    replace(&temp, path)?;
    Ok(after)
}

/// One rewrite attempt. Returns whether it succeeded, and ffmpeg's stderr.
fn run_ffmpeg(
    ffmpeg: &Path,
    path: &Path,
    temp: &Path,
    target: i32,
    force_format: Option<&str>,
) -> Result<(bool, String)> {
    let mut cmd = quiet_command(ffmpeg);
    cmd.args(["-y", "-v", "error", "-display_rotation"])
        .arg(target.to_string())
        .arg("-i")
        .arg(path)
        // Every stream, copied verbatim: audio, timecode and subtitles ride
        // along untouched.
        .args(["-map", "0", "-c", "copy"]);
    if let Some(f) = force_format {
        cmd.args(["-f", f]);
    }
    let output = cmd
        .arg(temp)
        .output()
        .with_context(|| format!("failed to run ffmpeg at {}", ffmpeg.display()))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Whether the MP4 muxer is a reasonable substitute for this file's own.
fn mp4_family(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "mov" | "mp4" | "m4v" | "m4a" | "3gp" | "3g2"))
}

/// Swap the rewritten file in for the original.
///
/// Windows `rename` will not overwrite, so the original is moved aside first
/// and only deleted once the replacement is in place. If the rename fails
/// halfway the original is put back, so the worst case is a wasted temp file
/// rather than a lost clip.
fn replace(temp: &Path, path: &Path) -> Result<()> {
    let backup = path.with_extension(format!(
        "{}.roughcut-original",
        path.extension().map(|e| e.to_string_lossy()).unwrap_or_default()
    ));
    let _ = std::fs::remove_file(&backup);
    rename_when_free(path, &backup)
        .with_context(|| format!("cannot move {} aside", path.display()))?;
    if let Err(e) = rename_when_free(temp, path) {
        let _ = std::fs::rename(&backup, path);
        let _ = std::fs::remove_file(temp);
        return Err(e).with_context(|| format!("cannot put the rotated {} in place", path.display()));
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

/// How long to wait for whoever else has the file to finish with it.
///
/// Windows refuses to rename a file another process holds open, and on any
/// bin of real size something usually does: a thumbnail being sampled, an
/// envelope being read, the audio being extracted for a transcript. All of
/// them are reads that finish on their own in seconds, so the answer is to
/// wait rather than to fail — the alternative is telling somebody their clip
/// cannot be rotated when in truth it could be a moment later.
///
/// Two seconds is past the longest of those reads and short enough that a
/// genuinely stuck file still reports promptly.
const RENAME_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Rename, waiting out another process that has the file open.
fn rename_when_free(from: &Path, to: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + RENAME_PATIENCE;
    let mut wait = std::time::Duration::from_millis(20);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(e);
                }
                std::thread::sleep(wait);
                // Back off, so a long read is not polled hundreds of times.
                wait = (wait * 2).min(std::time::Duration::from_millis(250));
            }
        }
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".to_string());
    // Same directory, so the rename that follows stays on one volume.
    path.with_extension(format!("roughcut-rotating.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tools;

    fn asymmetric_clip(ffmpeg: &Path, dir: &Path) -> PathBuf {
        let out = dir.join("clip.mp4");
        let status = quiet_command(ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=640x360:rate=30:d=2")
            .args([
                "-frames:v", "60", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt",
                "yuv420p",
            ])
            .arg(&out)
            .status()
            .expect("cannot run ffmpeg");
        assert!(status.success());
        out
    }

    /// The failure the user actually hit: a rotation refused because some
    /// other process still had the clip open.
    ///
    /// Reproduced with a real reader rather than a Rust `File`, deliberately.
    /// Rust opens files with `FILE_SHARE_DELETE`, which permits the rename and
    /// so would not reproduce this at all; ffmpeg does not, which is why a
    /// thumbnail or a transcript being read was enough to stop a rotation.
    #[cfg(windows)]
    #[test]
    fn a_rotation_waits_for_whoever_else_has_the_file() {
        let Some(ffmpeg) = Tools::discover().ffmpeg else {
            eprintln!("SKIPPED: ffmpeg is required");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-rotate-busy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = asymmetric_clip(&ffmpeg, &dir);
        let target = dir.join("moved.mp4");

        // A reader that holds the file for about half a second, the way a
        // waveform or a transcript extraction does.
        let mut reader = quiet_command(&ffmpeg)
            .args(["-v", "error", "-re", "-i"])
            .arg(&clip)
            .args(["-t", "0.5", "-f", "null", "-"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("cannot run ffmpeg");

        // Immediately, while it is definitely still reading.
        let started = std::time::Instant::now();
        let result = rename_when_free(&clip, &target);
        let waited = started.elapsed();
        let _ = reader.wait();

        assert!(
            result.is_ok(),
            "gave up on a file that was only briefly busy: {result:?}"
        );
        assert!(target.is_file(), "the rename reported success but did nothing");
        assert!(
            waited < RENAME_PATIENCE,
            "waited the full patience ({waited:?}), so it never actually succeeded early"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Patience is bounded: a file nothing will ever release still reports.
    #[test]
    fn a_rename_that_can_never_work_still_gives_up() {
        let dir = std::env::temp_dir().join("roughcut-rotate-nope");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("not-here.mp4");

        let started = std::time::Instant::now();
        let result = rename_when_free(&missing, &dir.join("x.mp4"));
        assert!(result.is_err(), "renamed a file that does not exist");
        // It waited, but it did stop.
        assert!(started.elapsed() < RENAME_PATIENCE * 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quarter_turn_swaps_the_shape_and_keeps_every_frame() {
        let tools = Tools::discover();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
            eprintln!("SKIPPED: ffmpeg and ffprobe are required to test rotation");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-rotate-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let clip = asymmetric_clip(&ffmpeg, &dir);
        let before = probe(&ffprobe, &clip).unwrap();
        assert_eq!((before.width, before.height), (640, 360));
        assert_eq!(before.rotation, 0);

        let after = rotate_in_place(&ffmpeg, &ffprobe, &clip, Turn::Clockwise).unwrap();
        assert_eq!(after.rotation, 270);
        assert_eq!((after.width, after.height), (360, 640));
        // The whole point: timing is untouched, so existing marks survive.
        assert_eq!(after.native_frames, before.native_frames);
        assert_eq!(after.fps, before.fps);

        // Nothing left behind next to the user's file.
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "clip.mp4")
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A .MOV holding VP9 — what some phones and screen recorders write.
    /// ffmpeg's `mov` muxer refuses to write it back out, so the rewrite has
    /// to fall back to the MP4 muxer rather than reporting failure.
    #[test]
    fn a_codec_the_containers_own_muxer_refuses_still_rotates() {
        let tools = Tools::discover();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
            eprintln!("SKIPPED: ffmpeg and ffprobe are required");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-rotate-vp9");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let clip = dir.join("vp9.MOV");
        let built = quiet_command(&ffmpeg)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=320x180:rate=30:d=2")
            .args([
                "-frames:v", "60", "-c:v", "libvpx-vp9", "-speed", "8", "-f", "mp4",
            ])
            .arg(&clip)
            .status()
            .expect("cannot run ffmpeg");
        if !built.success() {
            eprintln!("SKIPPED: this ffmpeg cannot encode VP9");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let before = probe(&ffprobe, &clip).unwrap();
        let after = rotate_in_place(&ffmpeg, &ffprobe, &clip, Turn::CounterClockwise)
            .expect("the mp4 muxer fallback should have handled this");
        assert_eq!(after.rotation, 90);
        assert_eq!((after.width, after.height), (180, 320));
        assert_eq!(after.native_frames, before.native_frames);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn four_turns_return_the_clip_to_where_it_started() {
        let tools = Tools::discover();
        let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
            eprintln!("SKIPPED: ffmpeg and ffprobe are required to test rotation");
            return;
        };
        let dir = std::env::temp_dir().join("roughcut-rotate-cycle");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let clip = asymmetric_clip(&ffmpeg, &dir);
        let before = probe(&ffprobe, &clip).unwrap();
        for _ in 0..4 {
            rotate_in_place(&ffmpeg, &ffprobe, &clip, Turn::CounterClockwise).unwrap();
        }
        let after = probe(&ffprobe, &clip).unwrap();
        assert_eq!(after.rotation, 0);
        assert_eq!((after.width, after.height), (before.width, before.height));
        assert_eq!(after.native_frames, before.native_frames);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
