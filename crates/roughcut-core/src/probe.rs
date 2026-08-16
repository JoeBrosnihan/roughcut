//! Media probing via the external `ffprobe` binary.

use crate::model::Profile;
use crate::time::Rational;
use crate::tools::quiet_command;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::Path;

/// What Roughcut needs to know about a file. Everything else ffprobe reports
/// is deliberately dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    /// Exact rational from `r_frame_rate`.
    pub fps: Rational,
    /// Frame count in the file's own time base.
    pub native_frames: i64,
    pub sample_aspect_num: u32,
    pub sample_aspect_den: u32,
    pub progressive: bool,
    pub colorspace: u32,
    pub video_index: i32,
    /// `-1` when the file has no audio.
    pub audio_index: i32,
    pub has_audio: bool,
}

impl MediaInfo {
    /// The profile this file would produce if it were the first import.
    pub fn to_profile(&self) -> Profile {
        let fps = self.fps.reduced();
        Profile {
            frame_rate_num: fps.num,
            frame_rate_den: fps.den,
            width: self.width,
            height: self.height,
            sample_aspect_num: self.sample_aspect_num,
            sample_aspect_den: self.sample_aspect_den,
            progressive: self.progressive,
            colorspace: self.colorspace,
        }
    }
}

/// Run `ffprobe` on `path` and extract the fields above.
pub fn probe(ffprobe: &Path, path: &Path) -> Result<MediaInfo> {
    let output = quiet_command(ffprobe)
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .with_context(|| format!("failed to run ffprobe at {}", ffprobe.display()))?;

    if !output.status.success() {
        bail!(
            "ffprobe failed on {} ({})",
            path.display(),
            output.status
        );
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("ffprobe produced unparseable JSON for {}", path.display()))?;
    parse_probe_json(&json).with_context(|| format!("cannot use {}", path.display()))
}

/// Split out from `probe` so it can be unit tested without an ffprobe binary.
pub fn parse_probe_json(json: &Value) -> Result<MediaInfo> {
    let streams = json
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("ffprobe reported no streams"))?;

    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
        // An attached cover image is a video stream but not footage.
        .filter(|s| {
            s.get("disposition")
                .and_then(|d| d.get("attached_pic"))
                .and_then(Value::as_i64)
                != Some(1)
        })
        .ok_or_else(|| anyhow!("no video stream — Roughcut does not handle audio-only files"))?;

    let audio = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"));

    let width = video
        .get("width")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("video stream has no width"))? as u32;
    let height = video
        .get("height")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("video stream has no height"))? as u32;

    // `r_frame_rate` is the real (constant) rate; `avg_frame_rate` is an
    // average that lies on files with a trailing partial frame.
    let fps = video
        .get("r_frame_rate")
        .and_then(Value::as_str)
        .and_then(parse_ratio)
        .or_else(|| {
            video
                .get("avg_frame_rate")
                .and_then(Value::as_str)
                .and_then(parse_ratio)
        })
        .filter(|r| r.num > 0 && r.den > 0)
        .ok_or_else(|| anyhow!("video stream has no usable frame rate"))?
        .reduced();

    let native_frames = frame_count(video, json, fps)?;

    let (sar_n, sar_d) = video
        .get("sample_aspect_ratio")
        .and_then(Value::as_str)
        .and_then(parse_aspect)
        // ffprobe omits SAR entirely when it is 1:1.
        .unwrap_or((1, 1));

    // `field_order` is absent on most progressive files, so anything that is
    // not explicitly one of the interlaced orders counts as progressive.
    let progressive = !matches!(
        video.get("field_order").and_then(Value::as_str),
        Some("tt" | "bb" | "tb" | "bt")
    );

    let colorspace = video
        .get("color_space")
        .and_then(Value::as_str)
        .and_then(map_colorspace)
        // Unflagged files: assume the usual convention for the frame size.
        .unwrap_or(if height > 1080 {
            2020
        } else if height > 576 {
            709
        } else {
            601
        });

    let video_index = video.get("index").and_then(Value::as_i64).unwrap_or(0) as i32;
    let audio_index = audio
        .and_then(|a| a.get("index"))
        .and_then(Value::as_i64)
        .map(|i| i as i32)
        .unwrap_or(-1);

    Ok(MediaInfo {
        width,
        height,
        fps,
        native_frames,
        sample_aspect_num: sar_n,
        sample_aspect_den: sar_d,
        progressive,
        colorspace,
        video_index,
        audio_index,
        has_audio: audio.is_some(),
    })
}

/// Frame count, in decreasing order of trustworthiness. `-count_frames` is
/// never used: it decodes the whole file and would make import unusable.
fn frame_count(video: &Value, json: &Value, fps: Rational) -> Result<i64> {
    // 1. Container-recorded frame count.
    if let Some(n) = video
        .get("nb_frames")
        .and_then(str_or_num)
        .filter(|&n| n > 0)
    {
        return Ok(n);
    }
    // 2. Stream duration in its own time base.
    if let (Some(dts), Some(tb)) = (
        video.get("duration_ts").and_then(Value::as_i64),
        video.get("time_base").and_then(Value::as_str).and_then(parse_ratio),
    ) {
        if dts > 0 && tb.num > 0 && tb.den > 0 {
            let secs = dts as f64 * tb.num as f64 / tb.den as f64;
            let n = (secs * fps.as_f64()).round() as i64;
            if n > 0 {
                return Ok(n);
            }
        }
    }
    // 3. Stream duration in seconds.
    for holder in [Some(video), json.get("format")].into_iter().flatten() {
        if let Some(secs) = holder
            .get("duration")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
        {
            let n = (secs * fps.as_f64()).round() as i64;
            if n > 0 {
                return Ok(n);
            }
        }
    }
    bail!("cannot determine a frame count for this file")
}

/// ffprobe writes numbers as JSON strings in some fields and numbers in others.
fn str_or_num(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `"30000/1001"` or `"25"`.
fn parse_ratio(s: &str) -> Option<Rational> {
    let s = s.trim();
    match s.split_once('/') {
        Some((n, d)) => Some(Rational::new(n.trim().parse().ok()?, d.trim().parse().ok()?)),
        None => Some(Rational::new(s.parse().ok()?, 1)),
    }
}

/// `"1:1"` or `"64:45"`. `"0:1"` means "unknown", which means square.
fn parse_aspect(s: &str) -> Option<(u32, u32)> {
    let (n, d) = s.trim().split_once(':')?;
    let n: u32 = n.trim().parse().ok()?;
    let d: u32 = d.trim().parse().ok()?;
    if n == 0 || d == 0 {
        Some((1, 1))
    } else {
        Some((n, d))
    }
}

fn map_colorspace(s: &str) -> Option<u32> {
    Some(match s {
        "bt709" => 709,
        "bt470bg" | "smpte170m" | "smpte240m" => 601,
        "bt2020nc" | "bt2020_ncl" | "bt2020c" | "bt2020_cl" => 2020,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hd_probe() -> Value {
        json!({
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "width": 1920,
                    "height": 1080,
                    "sample_aspect_ratio": "1:1",
                    "field_order": "progressive",
                    "r_frame_rate": "30000/1001",
                    "avg_frame_rate": "30000/1001",
                    "time_base": "1/30000",
                    "duration_ts": 300300,
                    "duration": "10.010000",
                    "nb_frames": "300"
                },
                { "index": 1, "codec_type": "audio" }
            ],
            "format": { "duration": "10.010000" }
        })
    }

    #[test]
    fn parses_a_typical_hd_file() {
        let info = parse_probe_json(&hd_probe()).unwrap();
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
        assert_eq!(info.fps, Rational::new(30000, 1001));
        assert_eq!(info.native_frames, 300);
        assert_eq!(info.colorspace, 709);
        assert!(info.progressive);
        assert!(info.has_audio);
        assert_eq!(info.video_index, 0);
        assert_eq!(info.audio_index, 1);
    }

    #[test]
    fn derives_the_profile_from_the_file() {
        let p = parse_probe_json(&hd_probe()).unwrap().to_profile();
        assert_eq!(p.frame_rate_num, 30000);
        assert_eq!(p.frame_rate_den, 1001);
        assert_eq!(p.display_aspect(), (16, 9));
    }

    #[test]
    fn falls_back_to_duration_when_nb_frames_is_missing() {
        let mut v = hd_probe();
        v["streams"][0].as_object_mut().unwrap().remove("nb_frames");
        let info = parse_probe_json(&v).unwrap();
        // 300300 / 30000 s at 30000/1001 fps == 300 frames.
        assert_eq!(info.native_frames, 300);
    }

    #[test]
    fn missing_audio_yields_index_minus_one() {
        let mut v = hd_probe();
        v["streams"].as_array_mut().unwrap().pop();
        let info = parse_probe_json(&v).unwrap();
        assert!(!info.has_audio);
        assert_eq!(info.audio_index, -1);
    }

    #[test]
    fn interlaced_flag_is_detected() {
        let mut v = hd_probe();
        v["streams"][0]["field_order"] = json!("tt");
        assert!(!parse_probe_json(&v).unwrap().progressive);
    }

    #[test]
    fn audio_only_files_are_rejected() {
        let v = json!({ "streams": [{ "index": 0, "codec_type": "audio" }] });
        assert!(parse_probe_json(&v).is_err());
    }

    #[test]
    fn cover_art_does_not_count_as_video() {
        let v = json!({
            "streams": [{
                "index": 0,
                "codec_type": "video",
                "width": 600,
                "height": 600,
                "r_frame_rate": "90000/1",
                "nb_frames": "1",
                "disposition": { "attached_pic": 1 }
            }]
        });
        assert!(parse_probe_json(&v).is_err());
    }

    #[test]
    fn sd_defaults_to_601() {
        let mut v = hd_probe();
        v["streams"][0]["width"] = json!(720);
        v["streams"][0]["height"] = json!(576);
        assert_eq!(parse_probe_json(&v).unwrap().colorspace, 601);
    }

    #[test]
    fn ratio_and_aspect_parsing() {
        assert_eq!(parse_ratio("30000/1001"), Some(Rational::new(30000, 1001)));
        assert_eq!(parse_ratio("25"), Some(Rational::new(25, 1)));
        assert_eq!(parse_aspect("64:45"), Some((64, 45)));
        assert_eq!(parse_aspect("0:1"), Some((1, 1)));
    }
}
