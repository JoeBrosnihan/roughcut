//! Audio tracks: music, voiceover, and effects laid under the picture.
//!
//! These differ from the video track in exactly one structural way, and it is
//! the one that matters. The video track is gapless, so a clip's position is
//! the sum of the lengths before it — derived, never stored, and therefore
//! incapable of desynchronising. Audio has gaps: an effect sits at the moment
//! it happens and there is silence either side. So an audio item carries its
//! own `start`, which is a stored position, which is a thing that can go
//! wrong. Everything here exists to keep it from going wrong.
//!
//! A video clip's own sound stays welded to its picture. These tracks are for
//! sound that has no picture.

use crate::model::ClipId;
use serde::{Deserialize, Serialize};

/// One piece of audio, placed at a position on its track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioItem {
    pub clip_id: ClipId,
    /// Source range, inclusive at both ends, exactly as `TimelineItem`.
    pub in_frame: i64,
    pub out_frame: i64,
    /// Where it begins on the timeline. Stored, because audio has gaps.
    pub start: i64,
}

impl AudioItem {
    /// Inclusive length: a clip from 0 to 99 is 100 frames.
    pub fn len(&self) -> i64 {
        (self.out_frame - self.in_frame + 1).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One past the last frame it occupies.
    pub fn end(&self) -> i64 {
        self.start + self.len()
    }

    /// Does this item cover `frame`?
    pub fn covers(&self, frame: i64) -> bool {
        frame >= self.start && frame < self.end()
    }

    fn overlaps(&self, from: i64, to: i64) -> bool {
        self.start < to && from < self.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrack {
    /// Shown on the lane and written into the MLT as `shotcut:name`.
    pub name: String,
    /// Ordered by `start`, never overlapping. Both are invariants this module
    /// maintains; nothing outside it should be inserting into `items`.
    items: Vec<AudioItem>,
    pub muted: bool,
}

impl AudioTrack {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            items: Vec::new(),
            muted: false,
        }
    }

    pub fn items(&self) -> &[AudioItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// One past the last frame anything on this track occupies.
    pub fn end(&self) -> i64 {
        self.items.last().map_or(0, |i| i.end())
    }

    /// The item covering `frame`, if any.
    pub fn item_at(&self, frame: i64) -> Option<usize> {
        self.items.iter().position(|i| i.covers(frame))
    }

    /// Put `item` on the track, overwriting whatever it lands on.
    ///
    /// Overwrite rather than refuse, because dropping a sound onto a spot is
    /// an instruction about where it goes, not a question about whether it
    /// fits. Anything underneath is trimmed to make room, and an existing item
    /// straddling the new one is split in two — which is what every editor
    /// does and what the eye expects.
    ///
    /// Returns the index the new item ended up at.
    pub fn place(&mut self, item: AudioItem) -> Option<usize> {
        if item.len() <= 0 || item.start < 0 {
            return None;
        }
        let (from, to) = (item.start, item.end());
        let mut kept: Vec<AudioItem> = Vec::with_capacity(self.items.len() + 2);

        for existing in std::mem::take(&mut self.items) {
            if !existing.overlaps(from, to) {
                kept.push(existing);
                continue;
            }
            // The head that survives to the left of the new item.
            if existing.start < from {
                let keep = from - existing.start;
                kept.push(AudioItem {
                    out_frame: existing.in_frame + keep - 1,
                    ..existing
                });
            }
            // And the tail to the right, whose source range starts further in.
            if existing.end() > to {
                let skip = to - existing.start;
                kept.push(AudioItem {
                    in_frame: existing.in_frame + skip,
                    start: to,
                    ..existing
                });
            }
        }

        kept.push(item);
        kept.sort_by_key(|i| i.start);
        self.items = kept;
        self.items.iter().position(|i| *i == item)
    }

    /// Take an item off the track, leaving silence where it was.
    pub fn remove(&mut self, index: usize) -> Option<AudioItem> {
        (index < self.items.len()).then(|| self.items.remove(index))
    }

    /// Move everything starting at or after `from` by `delta` frames.
    ///
    /// This is what a ripple on the video track does to the sound under it,
    /// and the reason it is one function rather than a loop at each call site:
    /// a stored position that five different edits have to remember to update
    /// is a position that will eventually be wrong.
    pub fn ripple(&mut self, from: i64, delta: i64) {
        if delta == 0 {
            return;
        }
        for item in &mut self.items {
            if item.start >= from {
                // Never past the beginning, and never onto a negative frame.
                item.start = (item.start + delta).max(0);
            }
        }
        self.items.sort_by_key(|i| i.start);
    }
}

/// One piece of sound to lay into the bed: where it comes from, what part of
/// it, and where it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct MixPiece {
    pub path: std::path::PathBuf,
    /// Seconds into the source file.
    pub from: f64,
    pub to: f64,
    /// Seconds into the timeline.
    pub at: f64,
}

/// Sample rate of the rendered bed. Matches what mpv will be mixing it with
/// and what every consumer expects; there is no reason to be clever here.
const BED_RATE: u32 = 48_000;

/// Flatten the audio tracks into one file with ffmpeg.
///
/// The preview plays a single mixed bed rather than N sources, and the
/// difference is not cosmetic: mpv would need one decoder per piece, running
/// for the whole timeline, and a project with fifty effects would need fifty.
/// Mixing once costs a fraction of a second — measured at 444 ms for
/// twenty-four pieces across ten minutes — and it is redone only when the
/// tracks actually change.
///
/// Returns `None` when there is nothing to mix, which is not a failure: it
/// means the bed should be removed rather than rebuilt.
pub fn mix_args(pieces: &[MixPiece], dest: &std::path::Path) -> Option<Vec<std::ffi::OsString>> {
    if pieces.is_empty() {
        return None;
    }
    let mut args: Vec<std::ffi::OsString> = vec!["-y".into(), "-v".into(), "error".into()];
    for piece in pieces {
        args.push("-i".into());
        args.push(piece.path.as_os_str().to_os_string());
    }

    let mut graph = String::new();
    for (i, piece) in pieces.iter().enumerate() {
        let delay = (piece.at * 1000.0).round().max(0.0) as i64;
        // `asetpts` resets the timestamps the trim left behind; without it
        // `adelay` has nothing predictable to delay from. `all=1` applies the
        // delay to every channel rather than only the first, which is what
        // silently produced one-sided audio otherwise.
        graph.push_str(&format!(
            "[{i}:a]atrim=start={:.6}:end={:.6},asetpts=PTS-STARTPTS,\
             aresample={BED_RATE},adelay={delay}:all=1[p{i}];",
            piece.from, piece.to
        ));
    }
    for i in 0..pieces.len() {
        graph.push_str(&format!("[p{i}]"));
    }
    // `normalize=0` keeps each piece at the level it was recorded at. amix
    // otherwise divides by the number of inputs, so adding a second effect
    // would quietly halve the first.
    graph.push_str(&format!(
        "amix=inputs={}:normalize=0:dropout_transition=0[bed]",
        pieces.len()
    ));

    args.push("-filter_complex".into());
    args.push(graph.into());
    args.push("-map".into());
    args.push("[bed]".into());
    args.push("-c:a".into());
    args.push("pcm_s16le".into());
    args.push("-ar".into());
    args.push(BED_RATE.to_string().into());
    args.push(dest.as_os_str().to_os_string());
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(start: i64, len: i64) -> AudioItem {
        AudioItem {
            clip_id: ClipId::new(),
            in_frame: 0,
            out_frame: len - 1,
            start,
        }
    }

    fn spans(track: &AudioTrack) -> Vec<(i64, i64)> {
        track.items().iter().map(|i| (i.start, i.end())).collect()
    }

    fn piece(at: f64, from: f64, to: f64) -> MixPiece {
        MixPiece {
            path: std::path::PathBuf::from("/m/a.mp4"),
            from,
            to,
            at,
        }
    }

    #[test]
    fn nothing_to_mix_is_not_a_failure() {
        assert!(mix_args(&[], std::path::Path::new("/t/bed.wav")).is_none());
    }

    #[test]
    fn each_piece_is_trimmed_then_delayed_to_its_place() {
        let args = mix_args(
            &[piece(0.0, 1.0, 3.0), piece(12.5, 0.0, 2.0)],
            std::path::Path::new("/t/bed.wav"),
        )
        .unwrap();
        let joined: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let graph = joined
            .iter()
            .find(|a| a.contains("amix"))
            .expect("a filter graph");

        assert!(graph.contains("atrim=start=1.000000:end=3.000000"), "{graph}");
        // Twelve and a half seconds in, in milliseconds.
        assert!(graph.contains("adelay=12500:all=1"), "{graph}");
        assert!(graph.contains("amix=inputs=2"), "{graph}");
        // Levels must not be divided down by the number of pieces.
        assert!(graph.contains("normalize=0"), "{graph}");
        // One input flag per piece.
        assert_eq!(joined.iter().filter(|a| *a == "-i").count(), 2);
    }

    #[test]
    fn a_piece_at_the_very_start_is_not_given_a_negative_delay() {
        let args = mix_args(&[piece(0.0, 0.0, 1.0)], std::path::Path::new("/t/b.wav")).unwrap();
        let graph = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .find(|a| a.contains("amix"))
            .unwrap();
        assert!(graph.contains("adelay=0:all=1"), "{graph}");
    }

    #[test]
    fn length_is_inclusive_like_everything_else() {
        let i = item(50, 100);
        assert_eq!(i.len(), 100);
        assert_eq!(i.end(), 150);
        assert!(i.covers(50) && i.covers(149));
        assert!(!i.covers(49) && !i.covers(150), "end is exclusive");
    }

    #[test]
    fn items_are_kept_in_order_however_they_arrive() {
        let mut t = AudioTrack::new("A1");
        t.place(item(300, 50));
        t.place(item(0, 50));
        t.place(item(100, 50));
        assert_eq!(spans(&t), vec![(0, 50), (100, 150), (300, 350)]);
        assert_eq!(t.end(), 350);
    }

    #[test]
    fn a_gap_is_a_real_thing_and_nothing_fills_it() {
        let mut t = AudioTrack::new("A1");
        t.place(item(0, 50));
        t.place(item(200, 50));
        assert_eq!(t.item_at(25), Some(0));
        assert_eq!(t.item_at(100), None, "the gap is silence, not an item");
        assert_eq!(t.item_at(210), Some(1));
    }

    #[test]
    fn dropping_onto_an_occupied_spot_overwrites_it() {
        let mut t = AudioTrack::new("A1");
        t.place(item(0, 100));
        // Lands over the back half.
        t.place(item(50, 100));
        assert_eq!(spans(&t), vec![(0, 50), (50, 150)]);
    }

    /// The case that needs the source range adjusting, not just the position.
    #[test]
    fn an_item_straddled_by_a_new_one_is_split_in_two() {
        let mut t = AudioTrack::new("A1");
        let long = AudioItem {
            clip_id: ClipId::new(),
            in_frame: 1000,
            out_frame: 1299,
            start: 0,
        };
        t.place(long);
        t.place(item(100, 100));

        assert_eq!(spans(&t), vec![(0, 100), (100, 200), (200, 300)]);
        let parts = t.items();
        // The head keeps its own start of source.
        assert_eq!((parts[0].in_frame, parts[0].out_frame), (1000, 1099));
        // The tail resumes 200 frames into the source, not at the beginning.
        assert_eq!((parts[2].in_frame, parts[2].out_frame), (1200, 1299));
        assert_eq!(parts[0].clip_id, parts[2].clip_id);
    }

    #[test]
    fn an_item_completely_covered_is_gone() {
        let mut t = AudioTrack::new("A1");
        t.place(item(100, 50));
        t.place(item(50, 200));
        assert_eq!(spans(&t), vec![(50, 250)]);
    }

    #[test]
    fn rippling_moves_what_is_after_the_cut_and_leaves_the_rest() {
        let mut t = AudioTrack::new("A1");
        t.place(item(0, 100));
        t.place(item(200, 100));
        t.place(item(400, 100));

        // A hundred frames deleted at 200.
        t.ripple(200, -100);
        assert_eq!(spans(&t), vec![(0, 100), (100, 200), (300, 400)]);

        // And inserted back again.
        t.ripple(100, 100);
        assert_eq!(spans(&t), vec![(0, 100), (200, 300), (400, 500)]);
    }

    #[test]
    fn a_ripple_can_never_push_anything_before_the_start() {
        let mut t = AudioTrack::new("A1");
        t.place(item(50, 100));
        t.ripple(0, -1000);
        assert_eq!(spans(&t), vec![(0, 100)], "clamped, not negative");
    }

    #[test]
    fn nonsense_placements_are_refused_rather_than_stored() {
        let mut t = AudioTrack::new("A1");
        assert!(t.place(item(-5, 100)).is_none());
        assert!(t
            .place(AudioItem {
                clip_id: ClipId::new(),
                in_frame: 10,
                out_frame: 9,
                start: 0
            })
            .is_none());
        assert!(t.is_empty());
    }

    #[test]
    fn removing_leaves_silence_and_moves_nothing() {
        let mut t = AudioTrack::new("A1");
        t.place(item(0, 100));
        t.place(item(200, 100));
        assert!(t.remove(0).is_some());
        assert_eq!(spans(&t), vec![(200, 300)], "the survivor stayed put");
        assert!(t.remove(9).is_none());
    }
}
