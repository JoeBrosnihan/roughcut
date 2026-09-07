//! Turning a file on disk into a bin clip.

use crate::model::{ClipId, Project, SourceClip};
use crate::probe::MediaInfo;
use crate::time::convert_frames;
use std::path::Path;

/// Result of importing one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Added(ClipId),
    /// The file is already in the bin.
    Duplicate(ClipId),
}

/// Add a probed file to the bin.
///
/// Importing may move the working profile — see [`crate::profile`] — but only
/// while nothing has been marked or assembled. Once it holds still, a clip at
/// a different rate is accepted as-is and flagged, because MLT will resample
/// it and frame-exactness for that clip is no longer guaranteed.
/// How long a photograph lasts once it is on the timeline.
///
/// Ten seconds is long enough to look at and short enough to trim down, and
/// trimming is exactly what the timeline edges are for.
pub const STILL_SECONDS: i64 = 10;

/// The most a photograph can be stretched to by retrimming.
///
/// A still has no footage behind it, so something has to bound the edge drag.
/// A minute is far more than a rough cut ever holds one for, and keeping it
/// finite means every clip in the project still obeys the same rule: you can
/// never trim past the end of what you have.
pub const STILL_MAX_SECONDS: i64 = 60;

/// Frames in `seconds` of project time, rounded to whole frames.
fn seconds_to_frames(seconds: i64, fps: crate::time::Rational) -> i64 {
    ((seconds * fps.num) as f64 / fps.den as f64).round() as i64
}

pub fn add_clip(project: &mut Project, path: &Path, info: &MediaInfo) -> ImportOutcome {
    let path = absolutise(path);

    if let Some(existing) = project.clips.iter().find(|c| c.path == path) {
        return ImportOutcome::Duplicate(existing.id);
    }

    let profile_fps = project.fps();

    // A photograph has no rate and no length of its own, so it takes the
    // project's rate and is given a length. Nothing downstream then has to
    // know it is a photograph in order to do arithmetic about it.
    let (native_fps, rate_mismatch, duration_frames) = if info.still {
        (
            profile_fps,
            false,
            seconds_to_frames(STILL_MAX_SECONDS, profile_fps),
        )
    } else if info.audio_only {
        // Sound has a real length but no frames to measure it in, so it is
        // expressed in the project's, exactly as a photograph is. It can
        // never be rate-mismatched: there is no rate to mismatch.
        (
            profile_fps,
            false,
            // Rounded, not truncated: a 3-minute-30.6-second track losing
            // its last half second would end the bed early.
            (info.seconds * profile_fps.as_f64()).round() as i64,
        )
    } else {
        let native_fps = info.fps.reduced();
        let mismatch = native_fps != profile_fps;
        // Durations live in profile time so every position in the app is
        // directly comparable, whatever the source rate was.
        let frames = if mismatch {
            convert_frames(info.native_frames, native_fps, profile_fps)
        } else {
            info.native_frames
        };
        (native_fps, mismatch, frames)
    };

    // A photo arrives already marked, because the answer to "how much of this
    // do you want" is the same every time and typing it out for each one is
    // not editing.
    let (mark_in, mark_out) = if info.still {
        (
            Some(0),
            Some(seconds_to_frames(STILL_SECONDS, profile_fps) - 1),
        )
    } else {
        (None, None)
    };

    let id = ClipId::new();
    project.clips.push(SourceClip {
        id,
        path,
        proxy_path: None,
        still: info.still,
        audio_only: info.audio_only,
        duration_frames: duration_frames.max(1),
        native_frames: if info.still || info.audio_only {
            duration_frames.max(1)
        } else {
            info.native_frames.max(1)
        },
        native_fps_num: native_fps.num,
        native_fps_den: native_fps.den,
        width: info.width,
        height: info.height,
        sample_aspect_num: info.sample_aspect_num,
        sample_aspect_den: info.sample_aspect_den,
        progressive: info.progressive,
        colorspace: info.colorspace,
        has_audio: info.has_audio,
        video_index: info.video_index,
        audio_index: info.audio_index,
        mark_in,
        mark_out,
        rate_mismatch,
        variable_rate: info.variable_rate,
        flagged: false,
        highlights: Vec::new(),
        archived: false,
    });

    crate::profile::refresh_working(project);
    ImportOutcome::Added(id)
}

/// What changed when a clip was measured again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Remeasured {
    /// The clip turned out to be a different length than the project recorded.
    pub duration_changed: bool,
    /// Marks or timeline cuts pointed past the end and had to be pulled back.
    pub trimmed: bool,
}

impl Remeasured {
    pub fn any(&self) -> bool {
        self.duration_changed || self.trimmed
    }
}

/// Measure a clip already in the bin against a fresh probe, and correct the
/// project if it disagrees.
///
/// Needed because a project can outlive a bug in the measuring. Variable-rate
/// phone video was recorded at its container's frame count, which for one
/// clip was 41 seconds longer than the file really runs; every mark and cut
/// made against that clip refers to frames that do not exist. Nothing else
/// notices until playback runs out of picture early, or the export hands MLT
/// an out point past the end of the file.
///
/// Only ever shortens or lengthens to what the file actually is, and pulls
/// marks and cuts back inside it. Cuts that would be left empty are dropped,
/// since a zero-length cut is not something the timeline can represent.
pub fn remeasure(project: &mut Project, id: ClipId, info: &MediaInfo) -> Remeasured {
    let profile_fps = project.fps();
    let native_fps = info.fps.reduced();
    let rate_mismatch = native_fps != profile_fps;
    let duration = if rate_mismatch {
        convert_frames(info.native_frames, native_fps, profile_fps)
    } else {
        info.native_frames
    }
    .max(1);

    let Some(clip) = project.clip_mut(id) else {
        return Remeasured::default();
    };
    let mut out = Remeasured {
        duration_changed: clip.duration_frames != duration,
        trimmed: false,
    };

    clip.duration_frames = duration;
    clip.native_frames = info.native_frames.max(1);
    clip.native_fps_num = native_fps.num;
    clip.native_fps_den = native_fps.den;
    clip.width = info.width;
    clip.height = info.height;
    clip.sample_aspect_num = info.sample_aspect_num;
    clip.sample_aspect_den = info.sample_aspect_den;
    clip.progressive = info.progressive;
    clip.colorspace = info.colorspace;
    clip.rate_mismatch = rate_mismatch;
    clip.variable_rate = info.variable_rate;

    let last = duration - 1;
    for m in [&mut clip.mark_in, &mut clip.mark_out].into_iter().flatten() {
        if *m > last {
            *m = last;
            out.trimmed = true;
        }
    }

    let before = project.timeline.len();
    project.timeline.retain_mut(|item| {
        if item.clip_id != id {
            return true;
        }
        if item.in_frame > last {
            return false;
        }
        if item.out_frame > last {
            item.out_frame = last;
        }
        item.out_frame >= item.in_frame
    });
    if project.timeline.len() != before
        || project
            .timeline
            .iter()
            .any(|i| i.clip_id == id && i.out_frame == last)
    {
        out.trimmed |= project.timeline.len() != before;
    }
    out
}

fn absolutise(path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {

    /// Sound is given a length in the project's own frames, and never gets
    /// to decide what the project looks like.
    #[test]
    fn a_sound_file_takes_the_projects_rate_and_gives_it_no_shape() {
        let mut p = Project::new();
        // A 4K 30fps clip sets the project.
        let video = info(3840, 2160, Rational::new(30, 1), 300);
        assert!(matches!(
            add_clip(&mut p, Path::new("/m/a.mp4"), &video),
            ImportOutcome::Added(_)
        ));
        let shape = (p.profile.width, p.profile.height, p.profile.fps());

        let mut music = info(0, 0, Rational::new(1, 1), 0);
        music.audio_only = true;
        music.has_audio = true;
        music.video_index = -1;
        music.seconds = 95.4;
        let ImportOutcome::Added(id) = add_clip(&mut p, Path::new("/m/bed.mp3"), &music) else {
            panic!("an mp3 should import");
        };

        let clip = p.clip(id).expect("in the bin");
        assert!(clip.audio_only);
        // 95.4 seconds at the project's 30 fps, rounded rather than truncated.
        assert_eq!(clip.duration_frames, 2862);
        assert_eq!(clip.native_fps(), Rational::new(30, 1));
        // There is no rate to disagree with, so it can never be mismatched.
        assert!(!clip.rate_mismatch);

        // And the project is exactly as the video left it.
        assert_eq!((p.profile.width, p.profile.height, p.profile.fps()), shape);
    }

    /// A bin holding only music has nothing to take a format from.
    #[test]
    fn music_alone_leaves_the_profile_where_it_was() {
        let mut p = Project::new();
        let before = p.profile.clone();
        let mut music = info(0, 0, Rational::new(1, 1), 0);
        music.audio_only = true;
        music.has_audio = true;
        music.seconds = 60.0;
        assert!(matches!(
            add_clip(&mut p, Path::new("/m/only.mp3"), &music),
            ImportOutcome::Added(_)
        ));
        assert_eq!(p.profile, before, "an mp3 set the project format");
    }

    use super::*;
    use crate::time::Rational;

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

    #[test]
    fn a_rate_mismatch_is_flagged_and_the_duration_converted() {
        let mut p = Project::new();
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        // Marking pins the working rate at 25, so the 50 fps import that
        // follows is the odd one out rather than moving the whole project.
        p.clips[0].mark_in = Some(0);
        add_clip(&mut p, Path::new("/m/b.mp4"), &info(1920, 1080, Rational::new(50, 1), 500));
        assert_eq!(p.fps(), Rational::new(25, 1));
        assert!(!p.clips[0].rate_mismatch);
        assert!(p.clips[1].rate_mismatch);
        // 500 frames of 50fps is 10 s, which is 250 frames of profile time.
        assert_eq!(p.clips[1].duration_frames, 250);
        assert_eq!(p.clips[1].native_fps(), Rational::new(50, 1));
    }

    #[test]
    fn matching_rates_are_not_flagged() {
        let mut p = Project::new();
        let ntsc = Rational::new(30000, 1001);
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, ntsc, 300));
        add_clip(&mut p, Path::new("/m/b.mp4"), &info(1280, 720, ntsc, 600));
        assert!(!p.clips[1].rate_mismatch);
        assert_eq!(p.clips[1].duration_frames, 600);
    }

    #[test]
    fn importing_the_same_file_twice_is_a_no_op() {
        let mut p = Project::new();
        let a = add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        let b = add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        assert!(matches!(a, ImportOutcome::Added(_)));
        assert!(matches!(b, ImportOutcome::Duplicate(_)));
        assert_eq!(p.clips.len(), 1);
    }

    /// A project written before variable-rate files were measured correctly
    /// holds cuts that run past the end of the media. Re-measuring must pull
    /// them back rather than leave MLT an out point that does not exist.
    #[test]
    fn remeasuring_shortens_the_clip_and_the_cuts_that_use_it() {
        let mut p = Project::new();
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(24, 1), 8653));
        let id = p.clips[0].id;
        p.clips[0].mark_in = Some(100);
        p.clips[0].mark_out = Some(8000);
        // Three cuts: wholly inside, straddling the new end, wholly past it.
        assert!(crate::timeline::append(&mut p, id, 0, 99));
        assert!(crate::timeline::append(&mut p, id, 7000, 8652));
        assert!(crate::timeline::append(&mut p, id, 8000, 8652));

        // What the file really is: 319.2 s at 24 fps.
        let truth = info(1920, 1080, Rational::new(24, 1), 7661);
        let out = remeasure(&mut p, id, &truth);

        assert!(out.duration_changed);
        assert!(out.trimmed);
        assert_eq!(p.clips[0].duration_frames, 7661);
        // The mark that pointed past the end came back to it; the other stayed.
        assert_eq!(p.clips[0].mark_in, Some(100));
        assert_eq!(p.clips[0].mark_out, Some(7660));
        // The cut wholly past the end is gone, the straddling one was clipped,
        // and the one that was always valid is untouched.
        assert_eq!(p.timeline.len(), 2);
        assert_eq!((p.timeline[0].in_frame, p.timeline[0].out_frame), (0, 99));
        assert_eq!((p.timeline[1].in_frame, p.timeline[1].out_frame), (7000, 7660));
        // Nothing may point past the media any more.
        assert!(p.timeline.iter().all(|i| i.out_frame < p.clips[0].duration_frames));
    }

    #[test]
    fn remeasuring_a_clip_that_has_not_changed_does_nothing() {
        let mut p = Project::new();
        let i = info(1920, 1080, Rational::new(30, 1), 300);
        add_clip(&mut p, Path::new("/m/a.mp4"), &i);
        let id = p.clips[0].id;
        assert!(crate::timeline::append(&mut p, id, 10, 200));
        let before = p.clone();
        let out = remeasure(&mut p, id, &i);
        assert!(!out.any());
        assert_eq!(p, before);
    }

    #[test]
    fn marks_default_to_the_whole_clip() {
        let mut p = Project::new();
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        assert_eq!(p.clips[0].marked_range(), Some((0, 249)));
        p.clips[0].mark_in = Some(10);
        assert_eq!(p.clips[0].marked_range(), Some((10, 249)));
        p.clips[0].mark_out = Some(20);
        assert_eq!(p.clips[0].marked_range(), Some((10, 20)));
    }
}
