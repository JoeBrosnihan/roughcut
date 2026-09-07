// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! The timeline as a single thing mpv can play.
//!
//! Playing the timeline used to mean loading one clip, watching for its out
//! point, then loading the next — which put a visible pause at every cut, and
//! there is no way to avoid that while mpv only ever knows about one file.
//! mpv's EDL format describes a cut list as one virtual stream, so it can open
//! the next segment before the current one runs out. Measured against real
//! footage the joins have no stall at all.
//!
//! It also collapses the playback arithmetic. With an EDL loaded, mpv's
//! `time-pos` *is* the timeline position, so there is nothing to convert and
//! no per-item bookkeeping to get wrong.

use crate::model::Project;
use crate::time::Rational;
use std::fmt::Write as _;

/// The timeline as an mpv EDL, or `None` when there is nothing to play.
///
/// Segment offsets are seconds into each source. A frame number divided by the
/// profile rate gives that directly, whatever the file's own rate is, because
/// a clip's duration in profile frames is defined to span its real running
/// time — see `probe::frame_count`.
pub fn to_text(project: &Project) -> Option<String> {
    if project.timeline.is_empty() {
        return None;
    }
    let fps = project.fps();
    let mut out = String::from("# mpv EDL v0\n");
    let mut wrote = 0usize;

    for item in &project.timeline {
        let Some(clip) = project.clip(item.clip_id) else {
            continue;
        };
        let len = item.len();
        if len <= 0 {
            continue;
        }
        let start = seconds(item.in_frame, fps);
        let length = seconds(len, fps);
        let path = clip.playback_path().to_string_lossy().into_owned();
        // `%<bytes>%` quoting, always. Paths hold commas, spaces and percent
        // signs, and every one of them means something to the EDL parser.
        let _ = writeln!(out, "%{}%{},{start:.6},{length:.6}", path.len(), path);
        wrote += 1;
    }

    (wrote > 0).then_some(out)
}

/// Identifies the cut list this EDL describes.
///
/// The file name carries it, so that editing the timeline produces a path mpv
/// has not seen. Rewriting one path in place would leave mpv holding the old
/// cut, since nothing about the name would have changed.
pub fn signature(project: &Project) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    project.profile.frame_rate_num.hash(&mut h);
    project.profile.frame_rate_den.hash(&mut h);
    for item in &project.timeline {
        item.clip_id.hash(&mut h);
        item.in_frame.hash(&mut h);
        item.out_frame.hash(&mut h);
        if let Some(clip) = project.clip(item.clip_id) {
            clip.playback_path().hash(&mut h);
        }
    }
    h.finish()
}

fn seconds(frames: i64, fps: Rational) -> f64 {
    if fps.num == 0 {
        return 0.0;
    }
    frames as f64 * fps.den as f64 / fps.num as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipId, Profile, Project, SourceClip, TimelineItem};
    use std::path::PathBuf;

    fn project(paths: &[&str]) -> Project {
        let mut p = Project {
            profile: Profile {
                frame_rate_num: 30,
                frame_rate_den: 1,
                ..Profile::default()
            },
            ..Project::default()
        };
        for path in paths {
            p.clips.push(SourceClip {
                id: ClipId::new(),
                still: false,
                path: PathBuf::from(path),
                proxy_path: None,
                duration_frames: 300,
                native_frames: 300,
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
            });
        }
        p
    }

    fn cut(p: &mut Project, clip: usize, in_frame: i64, out_frame: i64) {
        p.timeline.push(TimelineItem {
            clip_id: p.clips[clip].id,
            in_frame,
            out_frame,
        });
    }

    #[test]
    fn an_empty_timeline_has_nothing_to_play() {
        assert!(to_text(&project(&["/m/a.mp4"])).is_none());
    }

    #[test]
    fn each_cut_becomes_one_segment_in_order() {
        let mut p = project(&["/m/a.mp4", "/m/b.mp4"]);
        cut(&mut p, 0, 30, 89); // 1 s in, 2 s long
        cut(&mut p, 1, 0, 29); //  start,  1 s long
        let text = to_text(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# mpv EDL v0");
        assert_eq!(lines[1], "%8%/m/a.mp4,1.000000,2.000000");
        assert_eq!(lines[2], "%8%/m/b.mp4,0.000000,1.000000");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn a_path_with_a_comma_survives_the_quoting() {
        // The byte count is what makes this unambiguous; without it the EDL
        // parser would split the path at its comma.
        let mut p = project(&["/m/a,b.mp4"]);
        cut(&mut p, 0, 0, 29);
        let text = to_text(&p).unwrap();
        let line = text.lines().nth(1).unwrap();
        assert!(line.starts_with("%10%/m/a,b.mp4,"), "{line}");
    }

    #[test]
    fn the_signature_tracks_the_cut_and_nothing_else() {
        let mut p = project(&["/m/a.mp4"]);
        cut(&mut p, 0, 0, 29);
        let before = signature(&p);

        // Something that does not change what plays.
        p.clips[0].flagged = true;
        assert_eq!(signature(&p), before);

        // Something that does.
        p.timeline[0].out_frame = 59;
        assert_ne!(signature(&p), before);
    }

    #[test]
    fn segments_land_on_real_time_whatever_the_source_rate() {
        // A clip whose own rate differs from the project: its duration is
        // already expressed in profile frames, so seconds fall out directly.
        let mut p = project(&["/m/slow.mp4"]);
        p.clips[0].native_fps_num = 60;
        p.clips[0].rate_mismatch = true;
        cut(&mut p, 0, 60, 119); // profile frames 60..=119 at 30 fps: 2 s in, 2 s long
        let line = to_text(&p).unwrap().lines().nth(1).unwrap().to_string();
        assert!(line.ends_with(",2.000000,2.000000"), "{line}");
    }
}
