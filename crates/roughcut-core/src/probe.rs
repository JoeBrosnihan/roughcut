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
    /// DISPLAY width, i.e. after the file's rotation is applied. Phone footage
    /// is routinely stored landscape with a quarter-turn in the display
    /// matrix, and every consumer of these numbers — the profile, the proxy
    /// scaler, mpv — works in display space, so this does too.
    pub width: u32,
    pub height: u32,
    /// The file's display-matrix rotation, normalised to 0, 90, 180 or 270
    /// degrees counter-clockwise. Already applied to `width`/`height`.
    pub rotation: i32,
    /// Exact rational from `r_frame_rate`.
    pub fps: Rational,
    /// Frame count in the file's own time base, always consistent with `fps`
    /// and the file's real duration — see [`frame_count`].
    pub native_frames: i64,
    /// The file records more frames than its nominal rate and duration allow,
    /// which is what a variable frame rate looks like from outside. Positions
    /// in such a file are only as exact as its average rate; the count above
    /// is taken from the duration so playback and export at least agree about
    /// when the clip ends.
    pub variable_rate: bool,
    pub sample_aspect_num: u32,
    pub sample_aspect_den: u32,
    pub progressive: bool,
    pub colorspace: u32,
    pub video_index: i32,
    /// `-1` when the file has no audio.
    pub audio_index: i32,
    pub has_audio: bool,
    /// A photograph. It has a size but no duration and no real frame rate, so
    /// the two are supplied by the project rather than measured — see
    /// `import::add_clip`.
    pub still: bool,
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
    parse_still_or_clip(&json, crate::tools::is_still_image(path))
        .with_context(|| format!("cannot use {}", path.display()))
}

/// Split out from `probe` so it can be unit tested without an ffprobe binary.
pub fn parse_probe_json(json: &Value) -> Result<MediaInfo> {
    parse_still_or_clip(json, false)
}

/// As `parse_probe_json`, but told whether the file is a photograph.
///
/// A still has to be recognised before the timing is read, not after: ffprobe
/// reports no duration and no frame count for one, and a nominal 25/1 rate it
/// invented, so the ordinary path rejects it outright.
pub fn parse_still_or_clip(json: &Value, still: bool) -> Result<MediaInfo> {
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

    let coded_width = video
        .get("width")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("video stream has no width"))? as u32;
    let coded_height = video
        .get("height")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("video stream has no height"))? as u32;

    let rotation = parse_rotation(video);
    let quarter_turned = rotation == 90 || rotation == 270;
    let (width, height) = if quarter_turned {
        (coded_height, coded_width)
    } else {
        (coded_width, coded_height)
    };

    // `r_frame_rate` is the real (constant) rate; `avg_frame_rate` is an
    // average that lies on files with a trailing partial frame.
    let fps = if still {
        // Whatever ffprobe says here is fiction — a PNG reports 25/1. The
        // project supplies the real one when the clip is added.
        Rational::new(1, 1)
    } else {
        video
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
            .reduced()
    };

    let (native_frames, variable_rate) = if still {
        // Zero, meaning "not measured". A photograph lasts as long as it is
        // given, and only the project knows the rate to express that in.
        (0, false)
    } else {
        frame_count(video, json, fps)?
    };

    let (sar_n, sar_d) = video
        .get("sample_aspect_ratio")
        .and_then(Value::as_str)
        .and_then(parse_aspect)
        // ffprobe omits SAR entirely when it is 1:1.
        .unwrap_or((1, 1));
    // SAR describes coded pixels; a quarter turn inverts it along with
    // everything else.
    let (sar_n, sar_d) = if quarter_turned { (sar_d, sar_n) } else { (sar_n, sar_d) };

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
        rotation,
        fps,
        native_frames,
        variable_rate,
        sample_aspect_num: sar_n,
        sample_aspect_den: sar_d,
        progressive,
        colorspace,
        video_index,
        audio_index,
        has_audio: audio.is_some(),
        still,
    })
}

/// Frame count, and whether the file turned out to be variable rate.
///
/// `-count_frames` is never used: it decodes the whole file and would make
/// import unusable.
///
/// The count has to agree with `fps` about when the clip ends, because that
/// agreement is what the rest of Roughcut is built on — playback drives mpv in
/// seconds, seeking converts back, and MLT is handed frame numbers at the
/// profile rate. A phone shooting variable rate breaks it badly: one nominally
/// 24 fps clip records 8653 frames across 319 seconds, which is 27 fps really,
/// and trusting that count makes Roughcut believe the clip runs 41 seconds
/// longer than it does. Playback then ends on mpv running out of file, tens of
/// seconds early, and rolls on to the next cut.
///
/// So the container's count is used only where it agrees with the duration,
/// and the duration wins wherever they disagree.
fn frame_count(video: &Value, json: &Value, fps: Rational) -> Result<(i64, bool)> {
    let seconds = duration_seconds(video, json);
    let from_duration = seconds
        .map(|s| (s * fps.as_f64()).round() as i64)
        .filter(|&n| n > 0);
    let recorded = video
        .get("nb_frames")
        .and_then(str_or_num)
        .filter(|&n| n > 0);

    match (recorded, from_duration, seconds) {
        (Some(recorded), Some(derived), Some(seconds)) => {
            // Tolerate a frame or so at either end: a trailing partial frame
            // is ordinary and does not make a file variable rate. Beyond that
            // the two sources genuinely disagree about the clip's length.
            let slack = ((seconds * 0.01) * fps.as_f64()).max(2.0);
            if ((recorded - derived).abs() as f64) <= slack {
                Ok((recorded, false))
            } else {
                Ok((derived, true))
            }
        }
        (Some(recorded), None, _) => Ok((recorded, false)),
        (None, Some(derived), _) => Ok((derived, false)),
        _ => bail!("cannot determine a frame count for this file"),
    }
}

/// The clip's real length in seconds, preferring the stream's own timing to
/// the container's.
fn duration_seconds(video: &Value, json: &Value) -> Option<f64> {
    if let (Some(dts), Some(tb)) = (
        video.get("duration_ts").and_then(Value::as_i64),
        video
            .get("time_base")
            .and_then(Value::as_str)
            .and_then(parse_ratio),
    ) {
        if dts > 0 && tb.num > 0 && tb.den > 0 {
            return Some(dts as f64 * tb.num as f64 / tb.den as f64);
        }
    }
    for holder in [Some(video), json.get("format")].into_iter().flatten() {
        if let Some(secs) = holder
            .get("duration")
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| v.as_f64())
            })
            .filter(|s| *s > 0.0)
        {
            return Some(secs);
        }
    }
    None
}

/// The display-matrix rotation, normalised to 0/90/180/270 counter-clockwise.
///
/// Written two ways depending on the writer's vintage: as `rotation` in the
/// Display Matrix side data (what ffmpeg emits now, and what may be negative),
/// or as a `rotate` tag (what older tools wrote). Anything that is not a
/// quarter turn is treated as no rotation, since nothing here can honour it.
fn parse_rotation(video: &Value) -> i32 {
    let raw = video
        .get("side_data_list")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .find_map(|d| d.get("rotation").and_then(|v| v.as_f64()))
        })
        .or_else(|| {
            video
                .get("tags")
                .and_then(|t| t.get("rotate"))
                .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
        })
        .unwrap_or(0.0);
    let deg = raw.round() as i64;
    match deg.rem_euclid(360) {
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    }
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

    /// The real shape of the bug, taken from the file that produced it: an
    /// iPhone clip nominally 24 fps holding 8653 frames across 319 seconds,
    /// i.e. 27 fps really. Trusting the frame count made Roughcut believe the
    /// clip ran 41 seconds longer than it does, so timeline playback ended
    /// early and jumped to the next cut.
    #[test]
    fn a_variable_rate_file_is_measured_by_its_duration() {
        let mut v = hd_probe();
        v["streams"][0]["r_frame_rate"] = json!("24/1");
        v["streams"][0]["avg_frame_rate"] = json!("5191800/191537");
        v["streams"][0]["nb_frames"] = json!("8653");
        v["streams"][0]["time_base"] = json!("1/600");
        v["streams"][0]["duration_ts"] = json!(191536);
        v["streams"][0]["duration"] = json!("319.226667");
        v["format"]["duration"] = json!("319.226700");

        let info = parse_probe_json(&v).unwrap();
        assert!(info.variable_rate, "this file is not constant rate");
        // 319.2267 s at 24 fps, not the 8653 frames the container claims.
        assert_eq!(info.native_frames, 7661);
        // The invariant the rest of Roughcut depends on: the clip runs out of
        // frames exactly when the file runs out of picture.
        let implied = info.native_frames as f64 / info.fps.as_f64();
        assert!(
            (implied - 319.2267).abs() < 0.5,
            "{implied} s of frames for a 319.2 s file"
        );
    }

    #[test]
    fn a_constant_rate_file_keeps_its_recorded_count() {
        let info = parse_probe_json(&hd_probe()).unwrap();
        assert!(!info.variable_rate);
        assert_eq!(info.native_frames, 300);
    }

    #[test]
    fn a_trailing_partial_frame_is_not_a_variable_rate() {
        // A frame short of the duration is ordinary and must not trip it.
        let mut v = hd_probe();
        v["streams"][0]["nb_frames"] = json!("299");
        let info = parse_probe_json(&v).unwrap();
        assert!(!info.variable_rate);
        assert_eq!(info.native_frames, 299);
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
    fn a_quarter_turn_is_reported_in_display_orientation() {
        // What a phone writes: landscape frames plus a display matrix.
        let mut v = hd_probe();
        v["streams"][0]["side_data_list"] = json!([{ "rotation": -90 }]);
        let info = parse_probe_json(&v).unwrap();
        assert_eq!(info.rotation, 270);
        assert_eq!((info.width, info.height), (1080, 1920));
    }

    #[test]
    fn a_half_turn_leaves_the_frame_shape_alone() {
        let mut v = hd_probe();
        v["streams"][0]["side_data_list"] = json!([{ "rotation": 180 }]);
        let info = parse_probe_json(&v).unwrap();
        assert_eq!(info.rotation, 180);
        assert_eq!((info.width, info.height), (1920, 1080));
    }

    #[test]
    fn the_legacy_rotate_tag_is_understood_too() {
        let mut v = hd_probe();
        v["streams"][0]["tags"] = json!({ "rotate": "90" });
        let info = parse_probe_json(&v).unwrap();
        assert_eq!(info.rotation, 90);
        assert_eq!((info.width, info.height), (1080, 1920));
    }

    #[test]
    fn an_unrotated_file_reports_no_rotation() {
        assert_eq!(parse_probe_json(&hd_probe()).unwrap().rotation, 0);
    }

    #[test]
    fn ratio_and_aspect_parsing() {
        assert_eq!(parse_ratio("30000/1001"), Some(Rational::new(30000, 1001)));
        assert_eq!(parse_ratio("25"), Some(Rational::new(25, 1)));
        assert_eq!(parse_aspect("64:45"), Some((64, 45)));
        assert_eq!(parse_aspect("0:1"), Some((1, 1)));
    }
}
