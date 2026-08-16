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
use crate::tools::quiet_command;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Vertical resolution of generated proxies.
pub const PROXY_HEIGHT: u32 = 540;

pub fn proxy_path(proxy_dir: &Path, id: ClipId) -> PathBuf {
    proxy_dir.join(format!("{id}.mp4"))
}

/// The exact command line from §10.
pub fn proxy_args(source: &Path, dest: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
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

    let output = quiet_command(ffmpeg)
        .args(proxy_args(&clip.path, &dest))
        .output()
        .with_context(|| format!("failed to run ffmpeg at {}", ffmpeg.display()))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&dest);
        bail!(
            "ffmpeg failed for {}: {}",
            clip.file_name(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let proxy_info = probe(ffprobe, &dest)?;
    if let Err(reject) = check_proxy(source_info, &proxy_info) {
        let _ = std::fs::remove_file(&dest);
        bail!("{}: {reject}", clip.file_name());
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(frames: i64, fps: Rational) -> MediaInfo {
        MediaInfo {
            width: 1920,
            height: 1080,
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
