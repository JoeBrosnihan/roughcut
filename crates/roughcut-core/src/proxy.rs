//! Optional proxy transcoding.
//!
//! The one correctness rule that matters: a proxy must have the same frame
//! count and frame rate as its source, so frame numbers map 1:1. `-r` is never
//! passed, and the result is verified with ffprobe before it is used.
//!
//! Exported MLT XML always references the original file, never a proxy.

use crate::model::{ClipId, SourceClip};
use crate::probe::{probe, MediaInfo};
use crate::time::Rational;
use crate::tools::{background_command, background_threads};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Vertical resolution of generated proxies.
pub const PROXY_HEIGHT: u32 = 540;

/// Frames between keyframes in a proxy — about half a second at any ordinary
/// rate. Seeking, not file size, is what a proxy is for.
const GOP: u32 = 15;

pub fn proxy_path(proxy_dir: &Path, id: ClipId) -> PathBuf {
    proxy_dir.join(format!("{id}.mp4"))
}

/// Where a proxy is written while it is being transcoded and verified. Only a
/// finished, checked proxy is renamed to [`proxy_path`], so a file at the
/// final name is always complete — which is what lets a session adopt one by
/// existence alone instead of re-probing the whole bin first.
pub fn partial_path(proxy_dir: &Path, id: ClipId) -> PathBuf {
    proxy_dir.join(format!("{id}.part.mp4"))
}

/// The exact command line from §10.
pub fn proxy_args(source: &Path, dest: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        // A proxy is minutes of transcoding for a clip nobody asked about
        // yet. Unbounded, four of these together bury the machine.
        "-threads".into(),
        background_threads().to_string().into(),
        "-i".into(),
        source.as_os_str().to_os_string(),
        "-vf".into(),
        format!("scale=-2:{PROXY_HEIGHT}").into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        "23".into(),
        // A keyframe every half second.
        //
        // The whole point of a proxy is that landing on an arbitrary frame is
        // cheap, and an exact seek costs a decode from the preceding keyframe.
        // x264 would otherwise place them up to 250 frames apart. Source
        // footage measured here runs about a second between keyframes; half
        // that, at 540p, is a handful of milliseconds of decoding.
        "-g".into(),
        GOP.to_string().into(),
        "-keyint_min".into(),
        GOP.to_string().into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-movflags".into(),
        "+faststart".into(),
        dest.as_os_str().to_os_string(),
    ]
}

/// Why a generated proxy was thrown away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyReject {
    FrameCountMismatch { source: i64, proxy: i64 },
    FrameRateMismatch { source: Rational, proxy: Rational },
}

impl std::fmt::Display for ProxyReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameCountMismatch { source, proxy } => write!(
                f,
                "proxy has {proxy} frames but the source has {source} — discarded"
            ),
            Self::FrameRateMismatch { source, proxy } => write!(
                f,
                "proxy runs at {proxy} but the source runs at {source} — discarded"
            ),
        }
    }
}

/// Frame-for-frame equivalence check between a source and its proxy.
pub fn check_proxy(source: &MediaInfo, proxy: &MediaInfo) -> Result<(), ProxyReject> {
    if source.fps.reduced() != proxy.fps.reduced() {
        return Err(ProxyReject::FrameRateMismatch {
            source: source.fps.reduced(),
            proxy: proxy.fps.reduced(),
        });
    }
    if source.native_frames != proxy.native_frames {
        return Err(ProxyReject::FrameCountMismatch {
            source: source.native_frames,
            proxy: proxy.native_frames,
        });
    }
    Ok(())
}

/// Transcode a proxy and verify it. On any mismatch the file is deleted and an
/// error is returned, so a bad proxy can never be silently used for marking.
pub fn generate(
    ffmpeg: &Path,
    ffprobe: &Path,
    clip: &SourceClip,
    source_info: &MediaInfo,
    proxy_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(proxy_dir)
        .with_context(|| format!("cannot create proxy directory {}", proxy_dir.display()))?;
    let dest = proxy_path(proxy_dir, clip.id);

    // A proxy that is already there and still matches its source is adopted
    // rather than rebuilt. Without this, reopening a project re-transcodes
    // every clip in it — hours of work to arrive back where it started. The
    // check is the same one a fresh transcode has to pass, so an adopted proxy
    // is no more trusted than a new one.
    if dest.is_file() {
        match probe(ffprobe, &dest) {
            Ok(existing) if check_proxy(source_info, &existing).is_ok() => return Ok(dest),
            _ => {
                log::debug!("discarding stale proxy at {}", dest.display());
                let _ = std::fs::remove_file(&dest);
            }
        }
    }

    // Transcode somewhere else and rename only once verified. Writing
    // straight to `dest` meant a crash mid-transcode left a half-written file
    // at the final name, and anything trusting the name would play garbage.
    let part = partial_path(proxy_dir, clip.id);
    let _ = std::fs::remove_file(&part);
    let output = crate::tools::run(
        "proxy",
        background_command(ffmpeg).args(proxy_args(&clip.path, &part)),
    )
    .with_context(|| format!("failed to run ffmpeg at {}", ffmpeg.display()))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        bail!(
            "ffmpeg failed for {}: {}",
            clip.file_name(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let proxy_info = match probe(ffprobe, &part) {
        Ok(info) => info,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    if let Err(reject) = check_proxy(source_info, &proxy_info) {
        let _ = std::fs::remove_file(&part);
        bail!("{}: {reject}", clip.file_name());
    }
    std::fs::rename(&part, &dest)
        .with_context(|| format!("cannot move the finished proxy to {}", dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(frames: i64, fps: Rational) -> MediaInfo {
        MediaInfo {
            width: 1920,
            height: 1080,
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
        }
    }

    const NTSC: Rational = Rational::new(30000, 1001);

    #[test]
    fn identical_timing_is_accepted() {
        assert!(check_proxy(&info(300, NTSC), &info(300, NTSC)).is_ok());
    }

    #[test]
    fn a_dropped_frame_is_rejected() {
        let e = check_proxy(&info(300, NTSC), &info(299, NTSC)).unwrap_err();
        assert_eq!(
            e,
            ProxyReject::FrameCountMismatch {
                source: 300,
                proxy: 299
            }
        );
    }

    #[test]
    fn a_resampled_rate_is_rejected() {
        let e = check_proxy(&info(300, NTSC), &info(300, Rational::new(30, 1))).unwrap_err();
        assert!(matches!(e, ProxyReject::FrameRateMismatch { .. }));
    }

    #[test]
    fn command_line_never_forces_a_frame_rate() {
        let args = proxy_args(Path::new("/in.mp4"), Path::new("/out.mp4"));
        let joined: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            !joined.iter().any(|a| a == "-r"),
            "-r would resample and break the 1:1 frame mapping: {joined:?}"
        );
        assert!(joined.contains(&"scale=-2:540".to_string()));
        assert!(joined.contains(&"veryfast".to_string()));
    }
}
