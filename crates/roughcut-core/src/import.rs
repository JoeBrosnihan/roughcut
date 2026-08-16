//! Turning a file on disk into a bin clip, and deriving the project profile
//! from the first import.

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
/// The first import fixes the project profile; every later import is accepted
/// as-is and flagged when its rate differs, because MLT will resample it and
/// frame-exactness for that clip is no longer guaranteed.
pub fn add_clip(project: &mut Project, path: &Path, info: &MediaInfo) -> ImportOutcome {
    let path = absolutise(path);

    if let Some(existing) = project.clips.iter().find(|c| c.path == path) {
        return ImportOutcome::Duplicate(existing.id);
    }

    if !project.profile_locked {
        project.profile = info.to_profile();
        project.profile_locked = true;
    }

    let profile_fps = project.fps();
    let native_fps = info.fps.reduced();
    let rate_mismatch = native_fps != profile_fps;
    // Durations live in profile time so every position in the app is directly
    // comparable, whatever the source rate was.
    let duration_frames = if rate_mismatch {
        convert_frames(info.native_frames, native_fps, profile_fps)
    } else {
        info.native_frames
    };

    let id = ClipId::new();
    project.clips.push(SourceClip {
        id,
        path,
        proxy_path: None,
        duration_frames: duration_frames.max(1),
        native_fps_num: native_fps.num,
        native_fps_den: native_fps.den,
        has_audio: info.has_audio,
        video_index: info.video_index,
        audio_index: info.audio_index,
        mark_in: None,
        mark_out: None,
        rate_mismatch,
    });
    ImportOutcome::Added(id)
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
    use super::*;
    use crate::time::Rational;

    fn info(w: u32, h: u32, fps: Rational, frames: i64) -> MediaInfo {
        MediaInfo {
            width: w,
            height: h,
            fps,
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

    #[test]
    fn the_first_import_fixes_the_profile() {
        let mut p = Project::new();
        assert!(!p.profile_locked);
        add_clip(
            &mut p,
            Path::new("/m/a.mp4"),
            &info(3840, 2160, Rational::new(24000, 1001), 500),
        );
        assert!(p.profile_locked);
        assert_eq!(p.profile.width, 3840);
        assert_eq!(p.profile.frame_rate_num, 24000);
        assert_eq!(p.profile.frame_rate_den, 1001);
    }

    #[test]
    fn later_imports_never_change_the_profile() {
        let mut p = Project::new();
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        add_clip(&mut p, Path::new("/m/b.mp4"), &info(3840, 2160, Rational::new(50, 1), 500));
        assert_eq!(p.profile.width, 1920);
        assert_eq!(p.profile.frame_rate_num, 25);
    }

    #[test]
    fn a_rate_mismatch_is_flagged_and_the_duration_converted() {
        let mut p = Project::new();
        add_clip(&mut p, Path::new("/m/a.mp4"), &info(1920, 1080, Rational::new(25, 1), 250));
        add_clip(&mut p, Path::new("/m/b.mp4"), &info(1920, 1080, Rational::new(50, 1), 500));
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
