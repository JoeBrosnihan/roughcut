//! Frame-exact time handling.
//!
//! Every position in Roughcut is an `i64` frame number in profile time. This
//! module is the only place allowed to convert between frames and anything
//! else. Frame rates are exact rationals; a rate is never stored as a float.

use serde::{Deserialize, Serialize};
use std::fmt;

/// An exact frame rate, e.g. 30000/1001.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Self { num, den }
    }

    /// Normalise to a positive denominator and divide out the GCD.
    pub fn reduced(self) -> Self {
        let (mut n, mut d) = (self.num, self.den);
        if d == 0 {
            return Self::new(25, 1);
        }
        if d < 0 {
            n = -n;
            d = -d;
        }
        let g = gcd(n.unsigned_abs(), d.unsigned_abs()) as i64;
        if g > 1 {
            n /= g;
            d /= g;
        }
        Self::new(n, d)
    }

    /// Advisory only — for display and for handing seconds to mpv.
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }

    /// Frames per second rounded to the nearest whole frame, used for
    /// "step one second" and for timecode field width. 29.97 -> 30.
    pub fn nominal_fps(self) -> i64 {
        if self.den == 0 {
            return 25;
        }
        let r = (self.num as f64 / self.den as f64).round() as i64;
        r.max(1)
    }

}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Two decimals is enough to distinguish 29.97 from 30 in the status bar.
        let v = self.as_f64();
        if (v - v.round()).abs() < 1e-6 {
            write!(f, "{}", v.round() as i64)
        } else {
            write!(f, "{v:.2}")
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.max(1)
}

/// Seconds for the *start* of `frame`. Used only when handing a seek target to
/// mpv; the integer frame number remains the source of truth.
pub fn frame_to_seconds(frame: i64, fps: Rational) -> f64 {
    if fps.num == 0 {
        return 0.0;
    }
    frame as f64 * fps.den as f64 / fps.num as f64
}

/// Seek target, in seconds, that makes mpv land on exactly `frame`.
///
/// mpv's `seek <t> absolute+exact` displays the first frame whose timestamp is
/// at or after `t`. Aiming at the frame's own timestamp is therefore a coin
/// flip — a value a hair high tips onto the next frame — and aiming at the
/// middle of the frame lands one frame late every time. The target is
/// deliberately placed a fifth of a frame *before* the frame's timestamp,
/// which is unambiguously inside the previous frame's interval and so resolves
/// to `frame` with room to spare on either side.
pub fn frame_to_seek_seconds(frame: i64, fps: Rational) -> f64 {
    if fps.num == 0 {
        return 0.0;
    }
    let period = fps.den as f64 / fps.num as f64;
    (frame as f64 * period - 0.2 * period).max(0.0)
}

/// mpv's `time-pos` is advisory. Convert it once, then trust the integer.
pub fn seconds_to_frame(seconds: f64, fps: Rational) -> i64 {
    if fps.den == 0 {
        return 0;
    }
    // floor, not round: a time-pos anywhere inside frame N's display interval
    // identifies frame N. Guard against tiny negative float error at zero.
    let f = seconds * fps.num as f64 / fps.den as f64;
    if f <= 0.0 {
        0
    } else {
        // Nudge by half a frame's worth of float slop before flooring, because
        // demuxer timestamps sit a hair below the mathematically exact value.
        (f + 1e-6).floor() as i64
    }
}

/// Convert a frame count between two rates, rounding to nearest.
/// Used on import to express a source duration in profile time.
pub fn convert_frames(frames: i64, from: Rational, to: Rational) -> i64 {
    if from.num == 0 || to.den == 0 {
        return frames;
    }
    // frames * (from.den / from.num) * (to.num / to.den), in i128 to avoid
    // overflow on long files with large rationals.
    let n = frames as i128 * from.den as i128 * to.num as i128;
    let d = from.num as i128 * to.den as i128;
    if d == 0 {
        return frames;
    }
    // Round half away from zero.
    let q = (n * 2 + d.signum() * n.signum() * d) / (2 * d);
    q as i64
}

/// Inclusive duration. A clip with `in=0, out=99` is 100 frames long.
pub fn inclusive_len(in_frame: i64, out_frame: i64) -> i64 {
    out_frame - in_frame + 1
}

// ---------------------------------------------------------------------------
// Timecode — display format only, produced at the last moment before text is
// rendered. Never stored, never used for arithmetic.
// ---------------------------------------------------------------------------

/// Clock time as `MM:SS`, or `HH:MM:SS` once there are hours.
///
/// Deliberately *not* SMPTE `HH:MM:SS:FF`. The hours group is zero for
/// essentially everything edited here, and a frames group is a second field of
/// noise when all you want is how long something runs. Positions inside the
/// application remain `i64` frame counts regardless of how they are spelled
/// here — that is what keeps the export landing where you marked.
///
/// Truncates rather than rounds, so a displayed second has actually elapsed.
pub fn format_timecode(frame: i64, fps: Rational) -> String {
    let sign = if frame < 0 { "-" } else { "" };
    let total_secs = frame.abs() / fps.nominal_fps();
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    if hours > 0 {
        format!("{sign}{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{sign}{mins:02}:{secs:02}")
    }
}

/// MLT's "clock" time format, `HH:MM:SS.mmm`. Emitted only if a target refuses
/// integer frame positions; the exported XML uses integers by default.
pub fn format_clock(frame: i64, fps: Rational) -> String {
    let secs = frame_to_seconds(frame, fps);
    let neg = secs < 0.0;
    let s = secs.abs();
    // Round to milliseconds the way MLT does.
    let total_ms = (s * 1000.0).round() as i64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let ss = total_secs % 60;
    let mm = (total_secs / 60) % 60;
    let hh = total_secs / 3600;
    format!(
        "{}{hh:02}:{mm:02}:{ss:02}.{ms:03}",
        if neg { "-" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC30: Rational = Rational::new(30000, 1001);
    const P25: Rational = Rational::new(25, 1);

    #[test]
    fn inclusive_duration() {
        assert_eq!(inclusive_len(0, 99), 100);
        assert_eq!(inclusive_len(100, 199), 100);
        assert_eq!(inclusive_len(7, 7), 1);
    }

    /// The property that actually matters for frame-exact seeking: the target
    /// must sit strictly between frame N-1's timestamp and frame N's, so that
    /// "first frame at or after the target" can only be frame N.
    #[test]
    fn seek_target_brackets_the_wanted_frame() {
        for f in [1i64, 2, 99, 100, 199, 1000, 29970, 100_000] {
            let target = frame_to_seek_seconds(f, NTSC30);
            let this_frame = frame_to_seconds(f, NTSC30);
            let prev_frame = frame_to_seconds(f - 1, NTSC30);
            assert!(
                target < this_frame,
                "frame {f}: target {target} is not before its own timestamp {this_frame}"
            );
            assert!(
                target > prev_frame,
                "frame {f}: target {target} fell back into frame {}", f - 1
            );
        }
        // Frame 0 has no predecessor to undershoot into, so it clamps to zero.
        assert_eq!(frame_to_seek_seconds(0, NTSC30), 0.0);
    }

    /// A position *reported* by mpv must map back to the frame it names.
    #[test]
    fn reported_position_maps_back_to_its_frame() {
        for f in [0i64, 1, 99, 100, 199, 1000, 29970, 100_000] {
            let s = frame_to_seconds(f, NTSC30);
            assert_eq!(seconds_to_frame(s, NTSC30), f, "frame {f} did not survive");
        }
    }

    #[test]
    fn seconds_to_frame_handles_boundaries() {
        // Exactly on the boundary of frame 100 must resolve to 100, not 99.
        let s = frame_to_seconds(100, NTSC30);
        assert_eq!(seconds_to_frame(s, NTSC30), 100);
        // A hair before the boundary is still frame 99.
        assert_eq!(seconds_to_frame(s - 0.01, NTSC30), 99);
    }

    #[test]
    fn ntsc_rate_is_exact() {
        let r = Rational::new(30000, 1001);
        assert_eq!(r.nominal_fps(), 30);
        // 30000 frames is exactly 1001 seconds — the defining property of the
        // rate, and the thing that breaks the moment anyone stores 29.97.
        assert!((frame_to_seconds(30000, r) - 1001.0).abs() < 1e-12);
        // 107892 frames is one hour of wall clock to within 4 ms, which is why
        // drop-frame timecode exists. Roughcut counts frames, so it does not
        // care — but the arithmetic had better agree.
        assert!((frame_to_seconds(107892, r) - 3600.0).abs() < 0.005);
        // A whole hour of non-drop timecode is 108000 frames, and that is
        // 3603.6 s of real time.
        assert!((frame_to_seconds(108000, r) - 3603.6).abs() < 1e-9);
    }

    #[test]
    fn timecode_is_minutes_and_seconds() {
        assert_eq!(format_timecode(0, NTSC30), "00:00");
        assert_eq!(format_timecode(30, NTSC30), "00:01");
        assert_eq!(format_timecode(1800, NTSC30), "01:00");
        assert_eq!(format_timecode(100, P25), "00:04");
        assert_eq!(format_timecode(-30, NTSC30), "-00:01");
    }

    /// Frames within a second do not show, and do not round the second up:
    /// a displayed second has actually elapsed.
    #[test]
    fn timecode_truncates_within_a_second() {
        for f in 0..30 {
            assert_eq!(format_timecode(f, NTSC30), "00:00", "frame {f}");
        }
        assert_eq!(format_timecode(59, NTSC30), "00:01");
        assert_eq!(format_timecode(60, NTSC30), "00:02");
    }

    #[test]
    fn timecode_shows_hours_only_when_there_are_some() {
        // 30 nominal fps, so an hour is 108000 frames.
        assert_eq!(format_timecode(107_999, NTSC30), "59:59");
        assert_eq!(format_timecode(108_000, NTSC30), "1:00:00");
        assert_eq!(format_timecode(108_030, NTSC30), "1:00:01");
    }

    #[test]
    fn clock_format_matches_mlt() {
        // MLT writes 4 seconds at 30fps as 00:00:04.000.
        assert_eq!(format_clock(120, Rational::new(30, 1)), "00:00:04.000");
        assert_eq!(format_clock(0, NTSC30), "00:00:00.000");
    }

    #[test]
    fn rate_conversion_rounds_to_nearest() {
        // 100 frames of 30fps material is 100 frames at 30000/1001 to the
        // nearest frame (the durations differ by 0.1%).
        assert_eq!(
            convert_frames(100, Rational::new(30, 1), NTSC30),
            100
        );
        // 10 seconds of 50fps is 250 frames at 25fps.
        assert_eq!(
            convert_frames(500, Rational::new(50, 1), P25),
            250
        );
        // 10 seconds of 24fps is 240 frames at 24fps, 250 at 25fps.
        assert_eq!(
            convert_frames(240, Rational::new(24, 1), P25),
            250
        );
    }

    #[test]
    fn reduced_normalises() {
        assert_eq!(Rational::new(60000, 2002).reduced(), Rational::new(30000, 1001));
        assert_eq!(Rational::new(50, 2).reduced(), Rational::new(25, 1));
    }
}
