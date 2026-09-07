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

/// The frame rate a project should not exceed by default.
///
/// Phones shoot 120 and 240 fps slow motion, and one folder here holds 600.
/// Those are captures meant to be retimed, not a statement about the project,
/// and letting one of them set the rate makes every export enormous for no
/// benefit. The highest rate at or below this wins; only if every clip is
/// high-speed does the cap itself get used.
pub const RATE_CAP: Rational = Rational::new(60, 1);

/// The format that best represents a set of clips.
///
/// * the **most common aspect ratio**, so the shape of the finished video is
///   the shape most of the footage already is. Mixed orientations are a fact
///   of life when the material comes off several phones, and something has to
///   be pillarboxed; this picks the minority to bar rather than the majority.
/// * sized so its **longest side matches the longest side of the largest
///   clip**, which keeps the best material at full resolution without
///   inventing a non-standard frame to contain an odd one whole.
/// * the **highest frame rate at or below [`RATE_CAP`]**, so a 120 fps clip
///   raises the project above 30 but a 600 fps one does not take it with it.
/// * the sample aspect, scan and colourspace of the largest clip.
///
/// Every part of this is a default. The export dialog can override all of it,
/// and none of it is binding until then.
///
/// Returns `None` for an empty set.
pub fn suggest<'a>(clips: impl IntoIterator<Item = &'a SourceClip>) -> Option<Profile> {
    // Photographs are excluded. A 12-megapixel 4:3 still is not evidence
    // about what shape the video should be, and it has no frame rate at all —
    // letting one vote would let a single holiday snap decide the export.
    // Sound is excluded outright, and unlike a photograph it is never the
    // fallback either: a still at least has a shape, and an mp3 has nothing
    // to offer but zeroes. A bin holding only music keeps the default
    // profile until some picture arrives to set one.
    let all: Vec<&SourceClip> = clips.into_iter().filter(|c| !c.audio_only).collect();
    let footage: Vec<&SourceClip> = all.iter().copied().filter(|c| !c.still).collect();
    // Unless photographs are all there is, in which case their shape is the
    // only information available.
    let clips = if footage.is_empty() { all } else { footage };
    if clips.is_empty() {
        return None;
    }

    // --- shape ------------------------------------------------------------
    // Most common aspect, ties going to the one with more pixels behind it.
    let mut shapes: Vec<((u32, u32), usize, u64)> = Vec::new();
    for c in &clips {
        let a = aspect_of(c);
        let px = c.width as u64 * c.height as u64;
        match shapes.iter_mut().find(|(k, _, _)| *k == a) {
            Some((_, n, total)) => {
                *n += 1;
                *total += px;
            }
            None => shapes.push((a, 1, px)),
        }
    }
    let (aspect, _, _) = shapes
        .into_iter()
        .max_by(|x, y| x.1.cmp(&y.1).then_with(|| x.2.cmp(&y.2)))
        .expect("clips is non-empty");

    // --- size -------------------------------------------------------------
    let largest = clips
        .iter()
        .max_by_key(|c| c.width as u64 * c.height as u64)
        .copied()
        .expect("clips is non-empty");
    let (width, height) = fit_aspect(aspect, largest.width.max(largest.height).max(2));

    // --- rate -------------------------------------------------------------
    let rate = clips
        .iter()
        .map(|c| c.native_fps().reduced())
        .filter(|r| at_most(*r, RATE_CAP))
        .max_by(|a, b| (a.num * b.den).cmp(&(b.num * a.den)))
        // Everything is high-speed footage: fall back to the cap itself.
        .unwrap_or(RATE_CAP);

    Some(Profile {
        frame_rate_num: rate.num,
        frame_rate_den: rate.den,
        width,
        height,
        // The picture's own properties come from the clip that defined the
        // size, so they describe the material the profile was built around.
        sample_aspect_num: largest.sample_aspect_num.max(1),
        sample_aspect_den: largest.sample_aspect_den.max(1),
        progressive: largest.progressive,
        colorspace: largest.colorspace,
    })
}

/// A clip's display aspect, reduced.
fn aspect_of(c: &SourceClip) -> (u32, u32) {
    let (w, h) = (c.width.max(1), c.height.max(1));
    let g = gcd(w, h);
    (w / g, h / g)
}

/// The `aspect`-shaped frame whose longest side is `longest`.
///
/// Both sides are rounded to even numbers: odd dimensions are rejected outright
/// by every 4:2:0 encoder these files will ever meet.
fn fit_aspect((aw, ah): (u32, u32), longest: u32) -> (u32, u32) {
    let (aw, ah) = (aw.max(1) as u64, ah.max(1) as u64);
    let longest = longest.max(2) as u64;
    let (w, h) = if aw >= ah {
        (longest, (longest * ah).div_ceil(aw))
    } else {
        ((longest * aw).div_ceil(ah), longest)
    };
    (even(w), even(h))
}

fn even(n: u64) -> u32 {
    let n = n.max(2);
    (n + (n & 1)) as u32
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.max(1)
}

/// `a <= b`, in exact rational arithmetic.
fn at_most(a: Rational, b: Rational) -> bool {
    a.num * b.den <= b.num * a.den
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
            still: false,
            audio_only: false,
            seconds: 0.0,
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
    fn the_majority_aspect_decides_the_shape() {
        // Mixed orientations off several phones: the minority gets barred,
        // not the majority.
        let p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (1280, 720, Rational::new(30, 1), 100),
            (720, 1280, Rational::new(30, 1), 100),
        ]);
        assert_eq!((p.profile.width, p.profile.height), (1280, 720));

        let p = project_with(&[
            (720, 1280, Rational::new(30, 1), 100),
            (720, 1280, Rational::new(30, 1), 100),
            (1280, 720, Rational::new(30, 1), 100),
        ]);
        assert_eq!((p.profile.width, p.profile.height), (720, 1280));
    }

    /// The exact shape of the real folder this rule was written for: 84 clips
    /// of 16:9 720p, and a handful of 4:3 clips that are individually bigger.
    /// The old rule maxed width and height separately and produced 1920x1920,
    /// a square no clip was.
    #[test]
    fn a_bigger_odd_shaped_clip_sets_the_size_but_not_the_shape() {
        let mut specs = vec![(1280u32, 720u32, Rational::new(30, 1), 100i64); 8];
        specs.push((1920, 1440, Rational::new(30, 1), 100));
        let p = project_with(&specs);
        // 16:9 wins on count; 1920 is the longest side of the largest clip.
        assert_eq!((p.profile.width, p.profile.height), (1920, 1080));
    }

    #[test]
    fn a_uniform_folder_is_left_exactly_as_it_is() {
        // Nothing is upscaled when there is nothing bigger to scale to.
        let p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (1280, 720, Rational::new(30, 1), 100),
        ]);
        assert_eq!((p.profile.width, p.profile.height), (1280, 720));
        assert_eq!(p.profile.fps(), Rational::new(30, 1));
    }

    #[test]
    fn slow_motion_does_not_drag_the_project_up_with_it() {
        // 600 fps is a capture to be retimed, not a statement about the cut.
        let p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (1280, 720, Rational::new(600, 1), 100),
        ]);
        assert_eq!(p.profile.fps(), Rational::new(30, 1));

        // But a rate that is merely higher, and plausible, does win.
        let p = project_with(&[
            (1280, 720, Rational::new(30, 1), 100),
            (1280, 720, Rational::new(60, 1), 100),
        ]);
        assert_eq!(p.profile.fps(), Rational::new(60, 1));
    }

    #[test]
    fn all_high_speed_footage_falls_back_to_the_cap() {
        let p = project_with(&[
            (1280, 720, Rational::new(120, 1), 100),
            (1280, 720, Rational::new(240, 1), 100),
        ]);
        assert_eq!(p.profile.fps(), RATE_CAP);
    }

    #[test]
    fn profile_dimensions_are_always_even() {
        // 4:2:0 encoders reject odd dimensions outright.
        for (w, h) in [(1080u32, 1920u32), (1281, 721), (999, 333), (1440, 1080)] {
            let p = project_with(&[(w, h, Rational::new(30, 1), 100)]);
            assert_eq!(p.profile.width % 2, 0, "{w}x{h} gave {}", p.profile.width);
            assert_eq!(p.profile.height % 2, 0, "{w}x{h} gave {}", p.profile.height);
        }
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
