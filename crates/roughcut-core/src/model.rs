//! The whole application state that is worth persisting or undoing.
//!
//! Deliberately small and `Clone`-cheap: undo snapshots the entire `Project`.

use crate::time::{inclusive_len, Rational};
use crate::audio::AudioTrack;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Current on-disk schema version for `.roughcut` files.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipId(pub Uuid);

impl ClipId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClipId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ClipId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The resolution, rate and colour the project currently works in.
///
/// Not fixed at creation: see `crate::profile`. While the timeline is empty
/// and nothing is marked this tracks the bin; after that it holds still, and
/// the export derives its own from the clips actually used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub frame_rate_num: i64,
    pub frame_rate_den: i64,
    pub width: u32,
    pub height: u32,
    pub sample_aspect_num: u32,
    pub sample_aspect_den: u32,
    pub progressive: bool,
    /// 601 | 709 | 2020
    pub colorspace: u32,
}

impl Default for Profile {
    /// HD 1080p 30000/1001 — only used for an empty project with nothing
    /// imported yet. The first import replaces it wholesale.
    fn default() -> Self {
        Self {
            frame_rate_num: 30000,
            frame_rate_den: 1001,
            width: 1920,
            height: 1080,
            sample_aspect_num: 1,
            sample_aspect_den: 1,
            progressive: true,
            colorspace: 709,
        }
    }
}

impl Profile {
    pub fn fps(&self) -> Rational {
        Rational::new(self.frame_rate_num, self.frame_rate_den)
    }

    /// Display aspect, reduced. MLT wants this alongside the sample aspect.
    pub fn display_aspect(&self) -> (u32, u32) {
        let n = self.width as u64 * self.sample_aspect_num.max(1) as u64;
        let d = self.height as u64 * self.sample_aspect_den.max(1) as u64;
        let g = gcd_u64(n, d);
        ((n / g) as u32, (d / g) as u32)
    }

    pub fn description(&self) -> String {
        format!("{}x{} {}", self.width, self.height, self.fps())
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.max(1)
}

/// A file in the bin. `duration_frames` is already expressed in profile time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceClip {
    pub id: ClipId,
    /// A photograph rather than footage. Its duration is invented rather than
    /// measured, it has no audio and no proxy, and it exports through a
    /// different MLT producer. Absent from projects written before photos
    /// were supported, which is what the default is for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still: bool,
    /// Absolute path to the ORIGINAL file. Never a proxy.
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_path: Option<PathBuf>,
    /// Length in profile time, inclusive count (a 100-frame clip stores 100).
    /// Derived from `native_frames`; recomputed whenever the working profile
    /// moves, so it never accumulates rounding.
    pub duration_frames: i64,
    /// Length in the file's OWN time, as ffprobe reported it. The durable
    /// fact; `duration_frames` is a view of it. Zero in projects written
    /// before this field existed — see `project_io::load`.
    #[serde(default)]
    pub native_frames: i64,
    pub native_fps_num: i64,
    pub native_fps_den: i64,
    /// The clip's own format, kept so a project format can be suggested from
    /// everything in the bin rather than anchored to whichever file happened
    /// to be imported first. Defaulted when loading older projects.
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "one")]
    pub sample_aspect_num: u32,
    #[serde(default = "one")]
    pub sample_aspect_den: u32,
    #[serde(default = "yes")]
    pub progressive: bool,
    #[serde(default = "bt709")]
    pub colorspace: u32,
    pub has_audio: bool,
    /// Stream indices as ffprobe reported them, forwarded to MLT so it does
    /// not have to re-detect (`avformat-novalidate` relies on these).
    #[serde(default)]
    pub video_index: i32,
    #[serde(default = "minus_one")]
    pub audio_index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_out: Option<i64>,
    /// Set when the source frame rate differs from the project profile.
    #[serde(default)]
    pub rate_mismatch: bool,
    /// The file does not hold a steady frame rate, so its positions are only
    /// as exact as its average. Recorded so the bin can say so.
    #[serde(default)]
    pub variable_rate: bool,
    /// Picked out as worth using, before deciding where. Culling and
    /// assembling are separate passes: on a first watch you know a clip is
    /// good long before you know what it follows, and the timeline is the
    /// wrong place to park that judgement.
    #[serde(default)]
    pub flagged: bool,
    /// The good parts of this clip, in order and never overlapping.
    ///
    /// `flagged` at the granularity that actually matters: a twenty-minute
    /// take is rarely good or bad as a whole, and the judgement worth keeping
    /// is *which stretches* of it are worth using. Marks cannot hold this —
    /// there is one pair of them, so noting a second good stretch destroys
    /// the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<Highlight>,
    /// Set aside without being thrown away.
    ///
    /// Rejecting footage is most of a first pass, and it has to be as cheap
    /// as keeping it — but "cheap" cannot mean "irreversible", because a
    /// clip dismissed in the first ten minutes is exactly the one wanted in
    /// the last. An archived clip keeps its marks, its highlights and its
    /// place; it is only hidden, and no background work is spent on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

/// A stretch of a clip worth using. Inclusive at both ends, like every other
/// range in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    pub in_frame: i64,
    pub out_frame: i64,
}

impl Highlight {
    pub fn len(&self) -> i64 {
        inclusive_len(self.in_frame, self.out_frame)
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0
    }

    pub fn contains(&self, frame: i64) -> bool {
        frame >= self.in_frame && frame <= self.out_frame
    }
}

fn minus_one() -> i32 {
    -1
}
fn default_width() -> u32 {
    1920
}
fn default_height() -> u32 {
    1080
}
fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn bt709() -> u32 {
    709
}

impl SourceClip {
    pub fn native_fps(&self) -> Rational {
        Rational::new(self.native_fps_num, self.native_fps_den)
    }

    /// Last valid frame index.
    pub fn last_frame(&self) -> i64 {
        (self.duration_frames - 1).max(0)
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// The file playback should read from: the proxy when one exists on disk.
    pub fn playback_path(&self) -> &Path {
        match &self.proxy_path {
            Some(p) if p.exists() => p.as_path(),
            _ => self.path.as_path(),
        }
    }

    /// The highlight covering `frame`, if any.
    pub fn highlight_at(&self, frame: i64) -> Option<usize> {
        self.highlights.iter().position(|h| h.contains(frame))
    }

    /// Frames of this clip marked as worth using.
    pub fn highlighted_frames(&self) -> i64 {
        self.highlights.iter().map(|h| h.len()).sum()
    }

    /// Record a stretch as worth using.
    ///
    /// Overlapping and abutting stretches are merged rather than stacked, so
    /// the list stays sorted and disjoint however it was built up. Without
    /// that, re-keeping a range you had already kept would leave two entries
    /// covering the same frames, and "the highlight under the playhead"
    /// would stop being a single answer.
    ///
    /// Returns false if the range is empty or lies outside the clip.
    pub fn keep(&mut self, in_frame: i64, out_frame: i64) -> bool {
        if self.duration_frames <= 0 {
            return false;
        }
        let last = self.last_frame();
        let mut a = in_frame.min(out_frame).clamp(0, last);
        let mut b = in_frame.max(out_frame).clamp(0, last);
        if b < a {
            return false;
        }
        // Abutting counts as overlapping: two stretches that touch are one
        // stretch, and leaving a zero-frame seam between them would show as
        // a hairline gap on the strip that no amount of scrubbing could
        // close.
        let mut merged = Vec::with_capacity(self.highlights.len() + 1);
        for h in self.highlights.drain(..) {
            if h.out_frame + 1 < a || h.in_frame > b + 1 {
                merged.push(h);
            } else {
                a = a.min(h.in_frame);
                b = b.max(h.out_frame);
            }
        }
        merged.push(Highlight { in_frame: a, out_frame: b });
        merged.sort_by_key(|h| h.in_frame);
        self.highlights = merged;
        true
    }

    /// Forget the highlight covering `frame`. Returns what was removed.
    pub fn unkeep_at(&mut self, frame: i64) -> Option<Highlight> {
        let at = self.highlight_at(frame)?;
        Some(self.highlights.remove(at))
    }

    /// Resolved marks, applying the §9 defaults (missing in = 0,
    /// missing out = last frame). Returns `None` for a zero-length clip.
    pub fn marked_range(&self) -> Option<(i64, i64)> {
        if self.duration_frames <= 0 {
            return None;
        }
        let last = self.last_frame();
        let i = self.mark_in.unwrap_or(0).clamp(0, last);
        let o = self.mark_out.unwrap_or(last).clamp(0, last);
        if o < i {
            None
        } else {
            Some((i, o))
        }
    }
}

/// One cut on the timeline. Position is implied by order — never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub clip_id: ClipId,
    /// Inclusive.
    pub in_frame: i64,
    /// Inclusive.
    pub out_frame: i64,
}

impl TimelineItem {
    pub fn len(&self) -> i64 {
        inclusive_len(self.in_frame, self.out_frame)
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub profile: Profile,
    /// The bin.
    pub clips: Vec<SourceClip>,
    /// The single video track, in order.
    pub timeline: Vec<TimelineItem>,
    /// Sound with no picture of its own: music, voiceover, effects. Absent
    /// from projects written before audio tracks existed, which is what the
    /// default is for.
    #[serde(default)]
    pub audio: Vec<AudioTrack>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            profile: Profile::default(),
            clips: Vec::new(),
            timeline: Vec::new(),
            audio: Vec::new(),
        }
    }
}

impl Project {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fps(&self) -> Rational {
        self.profile.fps()
    }

    pub fn clip(&self, id: ClipId) -> Option<&SourceClip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut SourceClip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// How many cuts on the timeline come from this clip.
    pub fn timeline_uses(&self, id: ClipId) -> usize {
        self.timeline.iter().filter(|t| t.clip_id == id).count()
    }

    /// Drop a clip from the bin. Refuses while the timeline still references
    /// it — removing it would silently take assembled work with it, and the
    /// caller can explain that far more usefully than a bare `false` could.
    pub fn remove_clip(&mut self, id: ClipId) -> bool {
        if self.timeline_uses(id) > 0 {
            return false;
        }
        let before = self.clips.len();
        self.clips.retain(|c| c.id != id);
        self.clips.len() != before
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(duration_frames: i64) -> SourceClip {
        SourceClip {
            id: ClipId::new(),
            still: false,
            path: PathBuf::from("/media/a.mp4"),
            proxy_path: None,
            duration_frames,
            native_frames: duration_frames,
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
        }
    }

    fn spans(c: &SourceClip) -> Vec<(i64, i64)> {
        c.highlights.iter().map(|h| (h.in_frame, h.out_frame)).collect()
    }

    #[test]
    fn kept_stretches_stay_sorted_however_they_are_added() {
        let mut c = clip(1000);
        assert!(c.keep(500, 600));
        assert!(c.keep(100, 200));
        assert!(c.keep(800, 900));
        assert_eq!(spans(&c), [(100, 200), (500, 600), (800, 900)]);
        assert_eq!(c.highlighted_frames(), 303);
    }

    /// Keeping the same stretch twice must leave one entry, not two — the
    /// whole list is addressed by "which one covers this frame", and two
    /// answers to that is no answer.
    #[test]
    fn overlapping_stretches_merge_into_one() {
        let mut c = clip(1000);
        c.keep(100, 200);
        c.keep(150, 300);
        assert_eq!(spans(&c), [(100, 300)]);

        // Re-keeping ground already covered changes nothing at all.
        c.keep(120, 180);
        assert_eq!(spans(&c), [(100, 300)]);

        // A stretch that bridges two others swallows both.
        c.keep(600, 700);
        c.keep(250, 650);
        assert_eq!(spans(&c), [(100, 700)]);
    }

    /// Frame 200 and frame 201 are adjacent; a seam between them is invisible
    /// on screen but real to every lookup, so touching stretches merge too.
    #[test]
    fn abutting_stretches_merge_rather_than_leaving_a_seam() {
        let mut c = clip(1000);
        c.keep(100, 200);
        c.keep(201, 300);
        assert_eq!(spans(&c), [(100, 300)]);

        // One frame further apart is a genuine gap and stays one.
        let mut d = clip(1000);
        d.keep(100, 200);
        d.keep(202, 300);
        assert_eq!(spans(&d), [(100, 200), (202, 300)]);
    }

    #[test]
    fn a_stretch_is_found_and_forgotten_by_the_frame_inside_it() {
        let mut c = clip(1000);
        c.keep(100, 200);
        c.keep(500, 600);
        assert_eq!(c.highlight_at(150), Some(0));
        assert_eq!(c.highlight_at(550), Some(1));
        assert_eq!(c.highlight_at(300), None);
        // Inclusive at both ends, like every other range here.
        assert_eq!(c.highlight_at(100), Some(0));
        assert_eq!(c.highlight_at(200), Some(0));
        assert_eq!(c.highlight_at(201), None);

        assert_eq!(
            c.unkeep_at(550),
            Some(Highlight { in_frame: 500, out_frame: 600 })
        );
        assert_eq!(spans(&c), [(100, 200)]);
        assert_eq!(c.unkeep_at(550), None, "already gone");
    }

    #[test]
    fn a_stretch_is_clamped_to_the_clip_and_may_be_given_backwards() {
        let mut c = clip(300);
        // Given end-first, as a backwards drag produces.
        assert!(c.keep(250, 50));
        assert_eq!(spans(&c), [(50, 250)]);

        // Past the end clamps rather than storing a position that does not
        // exist in the file.
        let mut d = clip(300);
        assert!(d.keep(200, 99_999));
        assert_eq!(spans(&d), [(200, 299)]);

        // A single frame is a legal stretch.
        let mut e = clip(300);
        assert!(e.keep(42, 42));
        assert_eq!(spans(&e), [(42, 42)]);
        assert_eq!(e.highlighted_frames(), 1);

        // A clip with no frames has nothing to keep.
        let mut empty = clip(0);
        assert!(!empty.keep(0, 10));
        assert!(empty.highlights.is_empty());
    }
}
