// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Choosing the resolution and frame rate a project works in, and the ones it
//! exports at.
//!
//! The original design fixed the profile from the first file imported. That is
//! the wrong anchor: the first file is whichever one the dialog happened to
//! list first, and footage routinely arrives from several phones at once, at
//! several rates and sizes. So nothing is decided up front. The working
//! profile is re-derived from the whole bin while it is still free to move,
//! and the *export* profile is derived from the clips actually cut into the
//! timeline — the only ones whose format the finished video depends on.

use crate::model::{Profile, Project, SourceClip, TimelineItem};
use crate::time::{convert_frames, Rational};

/// The format that best represents a set of clips.
///
/// * the **most common** frame rate, ties broken by the higher rate, so a
///   handful of oddities cannot drag the whole project down to their rate;
/// * the **largest** frame size, so nothing is ever scaled up and detail is
///   only discarded deliberately;
/// * the sample aspect, scan and colourspace of a clip at the winning rate.
///
/// Returns `None` for an empty set.
pub fn suggest<'a>(clips: impl IntoIterator<Item = &'a SourceClip>) -> Option<Profile> {
    let clips: Vec<&SourceClip> = clips.into_iter().collect();
    let first = *clips.first()?;

    // Most common native rate; the higher rate wins a tie, since dropping
    // frames later is lossless where inventing them is not.
    let mut tally: Vec<(Rational, usize)> = Vec::new();
    for c in &clips {
        let r = c.native_fps().reduced();
        match tally.iter_mut().find(|(k, _)| *k == r) {
            Some((_, n)) => *n += 1,
            None => tally.push((r, 1)),
        }
    }
    let (rate, _) = tally
        .into_iter()
        .max_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| (a.0.num * b.0.den).cmp(&(b.0.num * a.0.den)))
        })
        .expect("clips is non-empty");

    let representative = clips
        .iter()
        .find(|c| c.native_fps().reduced() == rate)
        .copied()
        .unwrap_or(first);

    Some(Profile {
        frame_rate_num: rate.num,
        frame_rate_den: rate.den,
        width: clips.iter().map(|c| c.width).max().unwrap_or(1920).max(1),
        height: clips.iter().map(|c| c.height).max().unwrap_or(1080).max(1),
        sample_aspect_num: representative.sample_aspect_num.max(1),
        sample_aspect_den: representative.sample_aspect_den.max(1),
        progressive: representative.progressive,
        colorspace: representative.colorspace,
    })
}

/// Whether frame positions in this project already carry editorial meaning.
///
/// Until you mark or assemble anything, every frame number in the project is
/// derived — a duration nobody has referred to yet — so the working rate can
/// still change freely. The first mark or timeline item pins it, because from
/// then on a frame number records a decision you made while watching.
pub fn positions_committed(project: &Project) -> bool {
    !project.timeline.is_empty()
        || project
            .clips
            .iter()
            .any(|c| c.mark_in.is_some() || c.mark_out.is_some())
}

/// Re-derive the working profile from the whole bin, unless positions have
/// been committed. Returns true when the profile actually moved.
pub fn refresh_working(project: &mut Project) -> bool {
    if positions_committed(project) {
        return false;
    }
    let Some(target) = suggest(&project.clips) else {
        return false;
    };
    if target == project.profile {
        return false;
    }
    *project = retime(project, &target);
    true
}

/// The profile an export should use.
///
/// Derived from the clips actually cut into the timeline — footage sitting
/// unused in the bin has no say in the shape of the finished video. Falls back
/// to the working profile when nothing has been assembled.
pub fn for_export(project: &Project) -> Profile {
    let used: Vec<&SourceClip> = project
        .clips
        .iter()
        .filter(|c| project.timeline_uses(c.id) > 0)
        .collect();
    suggest(used).unwrap_or_else(|| project.profile.clone())
}

/// A copy of `project` expressed in `target`.
///
/// Clip durations are recomputed from each file's own native frame count
/// rather than from the current durations, so repeated retimes cannot
/// accumulate rounding. Timeline in/out points are rescaled by the rate
/// ratio, treating each as the half-open interval it really is: `out` names
/// the last frame, so it is `out + 1` that scales.
///
/// Only the frame rate can move a frame number. Resolution, sample aspect and
/// colourspace change nothing about timing, which is why they can be chosen at
/// export with no consequences at all.
pub fn retime(project: &Project, target: &Profile) -> Project {
    let from = project.fps();
    let to = target.fps();

    let mut out = project.clone();
    out.profile = target.clone();

    for clip in &mut out.clips {
        let native = clip.native_fps();
        clip.rate_mismatch = native != to.reduced();
        clip.duration_frames = if clip.rate_mismatch {
            convert_frames(clip.native_frames, native, to).max(1)
        } else {
            clip.native_frames.max(1)
        };
    }

    if from.reduced() != to.reduced() {
        let durations: Vec<(crate::model::ClipId, i64)> =
            out.clips.iter().map(|c| (c.id, c.duration_frames)).collect();
        out.timeline = project
            .timeline
            .iter()
            .filter_map(|item| {
                let last = durations
                    .iter()
                    .find(|(id, _)| *id == item.clip_id)
                    .map(|(_, d)| d - 1)?;
                let i = scale(item.in_frame, from, to).clamp(0, last.max(0));
                let o = (scale(item.out_frame + 1, from, to) - 1).clamp(i, last.max(0));
                Some(TimelineItem {
                    clip_id: item.clip_id,
                    in_frame: i,
                    out_frame: o,
                })
            })
            .collect();
    }

    out
}

/// `frame * to / from`, rounded half away from zero, in exact rational
/// arithmetic. Identical in spirit to [`convert_frames`] but named for
/// positions rather than counts.
fn scale(frame: i64, from: Rational, to: Rational) -> i64 {
    convert_frames(frame, from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::add_clip;
    use crate::probe::MediaInfo;
    use std::path::Path;

    fn info(w: u32, h: u32, fps: Rational, frames: i64) -> MediaInfo {
        MediaInfo {
            width: w,
            height: h,
            rotation: 0,
            fps,
            variable_rate: false,
            native_frames: frames,
            sample_aspect_num: 1,
            sample_aspect_den: 1,
            progressive: true,
            colorspace: 709,
            video_index: 0,
            audio_index: 1,
            has_audio: true,
        }
    }

    fn project_with(specs: &[(u32, u32, Rational, i64)]) -> Project {
        let mut p = Project::new();
        for (i, (w, h, fps, frames)) in specs.iter().enumerate() {
            add_clip(&mut p, Path::new(&format!("/m/{i}.mp4")), &info(*w, *h, *fps, *frames));
        }
        p
    }

    #[test]
    fn the_majority_rate_wins_not_the_first_clip() {
        // A stray 24 fps clip imported first must not define the project when
        // everything else came off 60 fps phones.
        let p = project_with(&[
            (1920, 1080, Rational::new(24, 1), 100),
            (1920, 1080, Rational::new(60, 1), 100),
            (1280, 720, Rational::new(60, 1), 100),
        ]);
        assert_eq!(p.profile.fps(), Rational::new(60, 1));
    }

    #[test]
    fn the_largest_frame_wins_so_nothing_is_upscaled() {
        let p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (3840, 2160, Rational::new(30, 1), 100),
        ]);
        assert_eq!((p.profile.width, p.profile.height), (3840, 2160));
    }

    #[test]
    fn a_tie_on_rate_goes_to_the_higher_one() {
        let p = project_with(&[
            (1920, 1080, Rational::new(30, 1), 100),
            (1920, 1080, Rational::new(60, 1), 100),
        ]);
        assert_eq!(p.profile.fps(), Rational::new(60, 1));
    }

    #[test]
    fn the_profile_stops_moving_once_anything_is_marked() {
        let mut p = project_with(&[(1920, 1080, Rational::new(30, 1), 100)]);
        p.clips[0].mark_in = Some(10);
        add_clip(&mut p, Path::new("/m/late.mp4"), &info(3840, 2160, Rational::new(60, 1), 100));
        assert_eq!(p.profile.fps(), Rational::new(30, 1));
        assert_eq!(p.profile.width, 1920);
        // ...and the mark still means the frame it meant when it was made.
        assert_eq!(p.clips[0].mark_in, Some(10));
    }

    #[test]
    fn export_ignores_footage_left_unused_in_the_bin() {
        let mut p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (3840, 2160, Rational::new(60, 1), 200),
        ]);
        // The bin suggests 4K60; only the 720p30 clip is actually cut in.
        let used = p.clips[0].id;
        crate::timeline::append(&mut p, used, 0, 49);
        let e = for_export(&p);
        assert_eq!((e.width, e.height), (1280, 720));
        assert_eq!(e.fps(), Rational::new(30, 1));
    }

    #[test]
    fn retiming_preserves_real_time_and_never_loses_a_clip() {
        let mut p = project_with(&[(1920, 1080, Rational::new(30, 1), 300)]);
        let id = p.clips[0].id;
        // Frames 30..=89 — one second in, two seconds long.
        crate::timeline::append(&mut p, id, 30, 89);

        let target = Profile {
            frame_rate_num: 60,
            frame_rate_den: 1,
            ..p.profile.clone()
        };
        let r = retime(&p, &target);
        assert_eq!(r.timeline.len(), 1);
        assert_eq!(r.timeline[0].in_frame, 60);
        assert_eq!(r.timeline[0].out_frame, 179);
        // Two seconds is still two seconds.
        assert_eq!(r.timeline[0].len(), 120);
        assert_eq!(r.clips[0].duration_frames, 600);
    }

    #[test]
    fn retiming_to_the_same_rate_changes_nothing() {
        let mut p = project_with(&[(1920, 1080, Rational::new(30000, 1001), 300)]);
        let id = p.clips[0].id;
        crate::timeline::append(&mut p, id, 7, 113);
        let r = retime(&p, &p.profile.clone());
        assert_eq!(r, p);
    }

    #[test]
    fn a_retimed_out_point_can_never_run_past_the_clip() {
        let mut p = project_with(&[(1920, 1080, Rational::new(60, 1), 100)]);
        let id = p.clips[0].id;
        crate::timeline::append(&mut p, id, 0, 99);
        let target = Profile {
            frame_rate_num: 30,
            frame_rate_den: 1,
            ..p.profile.clone()
        };
        let r = retime(&p, &target);
        assert_eq!(r.clips[0].duration_frames, 50);
        assert!(r.timeline[0].out_frame < r.clips[0].duration_frames);
    }
}
