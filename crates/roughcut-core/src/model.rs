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
