//! What was said, and when.
//!
//! On long footage the words are a far better index than the pictures: finding
//! the moment someone says the thing is reading, not hunting. A transcript
//! turns a two-hour recording into a document you can skim, and selecting a
//! sentence in that document is the same act as marking a range on the scrub
//! bar — it sets `in` and `out`, and every existing key works on it unchanged.
//!
//! Times are milliseconds in the **file's own** clock, never frames. A
//! transcript is a property of the media and outlives any particular project:
//! the project's frame rate can move underneath it, and `Transcript::range`
//! converts at the moment of use, exactly as `native_frames` relates to
//! `duration_frames`.

use crate::time::Rational;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One word, and the moment it is said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A run of words, as whisper divided them — roughly a sentence.
///
/// Kept as a unit because it is the natural thing to click: "give me that
/// remark" is a more common wish than "give me those three words".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub words: Vec<Word>,
}

impl Segment {
    pub fn text(&self) -> String {
        let mut out = String::new();
        for w in &self.words {
            push_word(&mut out, &w.text);
        }
        out
    }

    pub fn start_ms(&self) -> i64 {
        self.words.first().map_or(0, |w| w.start_ms)
    }

    pub fn end_ms(&self) -> i64 {
        self.words.last().map_or(0, |w| w.end_ms)
    }
}

/// Append `word` to `out`, inserting a space unless it is punctuation that
/// belongs to what came before. Whisper emits leading spaces on most words and
/// none on punctuation, but not dependably enough to simply concatenate.
fn push_word(out: &mut String, word: &str) {
    let word = word.trim();
    if word.is_empty() {
        return;
    }
    let joins_left = word
        .chars()
        .next()
        .is_some_and(|c| matches!(c, ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '\'' | '"' | '%'));
    if !out.is_empty() && !joins_left {
        out.push(' ');
    }
    out.push_str(word);
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
}

/// A selection, as a half-open range of word indices into the flattened
/// transcript. The UI works in these; nothing else needs to know how the words
/// are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub from: usize,
    pub to: usize,
}

impl Selection {
    /// Normalised, so a backwards drag means the same as a forwards one.
    pub fn new(a: usize, b: usize) -> Self {
        Self {
            from: a.min(b),
            to: a.max(b),
        }
    }

    pub fn contains(&self, i: usize) -> bool {
        i >= self.from && i <= self.to
    }
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|s| s.words.is_empty())
    }

    /// Every word in order, ignoring how they are grouped.
    pub fn words(&self) -> impl Iterator<Item = &Word> {
        self.segments.iter().flat_map(|s| s.words.iter())
    }

    pub fn word_count(&self) -> usize {
        self.segments.iter().map(|s| s.words.len()).sum()
    }

    pub fn word(&self, index: usize) -> Option<&Word> {
        self.words().nth(index)
    }

    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The word being spoken at `ms`.
    ///
    /// In a silence the last word spoken stays current, rather than the cursor
    /// running ahead to one nobody has said yet — a highlight that arrives
    /// before the sound reads as the transcript being out of sync.
    ///
    /// Used to follow playback in the document, so it scrolls itself while a
    /// clip plays.
    pub fn word_at(&self, ms: i64) -> Option<usize> {
        let mut last_before = None;
        for (i, w) in self.words().enumerate() {
            if ms < w.start_ms {
                return last_before.or(Some(i));
            }
            if ms <= w.end_ms {
                return Some(i);
            }
            last_before = Some(i);
        }
        last_before
    }

    /// The frame range a selection names, in project time.
    ///
    /// Padded, and deliberately so. Whisper places a word to within a few tens
    /// of milliseconds, and a cut landing exactly on the first consonant
    /// clips it — the listener hears the speaker already talking. A little air
    /// either side is what makes the result usable without hand-trimming, and
    /// trimming it back is what the timeline edges are for.
    ///
    /// Returns inclusive `(in, out)` frames, clamped to the clip, or `None` if
    /// the selection names nothing.
    pub fn range(&self, selection: Selection, fps: Rational, last_frame: i64) -> Option<(i64, i64)> {
        let words: Vec<&Word> = self.words().collect();
        let first = words.get(selection.from)?;
        let last = words.get(selection.to.min(words.len().saturating_sub(1)))?;

        let start = (first.start_ms - PAD_BEFORE_MS).max(0);
        let end = last.end_ms + PAD_AFTER_MS;

        let in_frame = ms_to_frame(start, fps).clamp(0, last_frame);
        // The word ends *during* this frame, so the frame it ends in is the
        // last one that must be kept — `out` is inclusive.
        let out_frame = ms_to_frame(end, fps).clamp(in_frame, last_frame);
        Some((in_frame, out_frame))
    }
}

/// Air before the first word. Larger than the tail because a cut that starts
/// fractionally late is much more noticeable than one that ends early: the
/// first syllable is simply missing.
const PAD_BEFORE_MS: i64 = 250;
const PAD_AFTER_MS: i64 = 400;

pub fn ms_to_frame(ms: i64, fps: Rational) -> i64 {
    if fps.den == 0 {
        return 0;
    }
    // Exact rational arithmetic in i128, so a long clip cannot overflow and
    // nothing is routed through a float.
    let num = ms as i128 * fps.num as i128;
    let den = 1000_i128 * fps.den as i128;
    (num / den) as i64
}

/// Where a clip's transcript is cached.
pub fn cache_file(cache_dir: &Path, fingerprint: u64) -> std::path::PathBuf {
    cache_dir.join(format!("{fingerprint:016x}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS30: Rational = Rational::new(30, 1);

    fn word(text: &str, start_ms: i64, end_ms: i64) -> Word {
        Word {
            text: text.to_string(),
            start_ms,
            end_ms,
        }
    }

    fn sample() -> Transcript {
        Transcript {
            segments: vec![
                Segment {
                    words: vec![
                        word("Watch", 1000, 1300),
                        word("this", 1300, 1600),
                        word(".", 1600, 1650),
                    ],
                },
                Segment {
                    words: vec![
                        word("That", 5000, 5300),
                        word("was", 5300, 5500),
                        word("amazing", 5500, 6200),
                        word("!", 6200, 6250),
                    ],
                },
            ],
        }
    }

    #[test]
    fn punctuation_joins_the_word_before_it() {
        assert_eq!(sample().segments[0].text(), "Watch this.");
        assert_eq!(sample().segments[1].text(), "That was amazing!");
    }

    #[test]
    fn the_document_separates_segments_into_paragraphs() {
        assert_eq!(sample().text(), "Watch this.\n\nThat was amazing!");
    }

    #[test]
    fn words_are_numbered_across_segment_boundaries() {
        let t = sample();
        assert_eq!(t.word_count(), 7);
        assert_eq!(t.word(3).unwrap().text, "That");
    }

    /// The whole feature in one assertion: pick some words, get the frames.
    #[test]
    fn a_selection_becomes_a_frame_range() {
        let t = sample();
        // "That was amazing!" — words 3..=6, 5.000 s to 6.250 s.
        let (in_f, out_f) = t.range(Selection::new(3, 6), FPS30, 100_000).unwrap();
        // 5.000 - 0.250 = 4.750 s -> frame 142; 6.250 + 0.400 = 6.650 -> 199.
        assert_eq!(in_f, 142);
        assert_eq!(out_f, 199);
        // Inclusive, so the length is out - in + 1.
        assert_eq!(out_f - in_f + 1, 58);
    }

    #[test]
    fn a_backwards_drag_selects_the_same_words() {
        let t = sample();
        assert_eq!(
            t.range(Selection::new(6, 3), FPS30, 100_000),
            t.range(Selection::new(3, 6), FPS30, 100_000)
        );
    }

    /// A selection at the very start must not produce a negative in point.
    #[test]
    fn padding_cannot_run_off_either_end() {
        let t = Transcript {
            segments: vec![Segment {
                words: vec![word("Hello", 0, 200)],
            }],
        };
        let (in_f, out_f) = t.range(Selection::new(0, 0), FPS30, 10).unwrap();
        assert_eq!(in_f, 0);
        assert_eq!(out_f, 10, "clamped to the clip rather than past its end");
    }

    #[test]
    fn a_selection_past_the_end_is_refused_rather_than_panicking() {
        let t = sample();
        assert!(t.range(Selection::new(99, 120), FPS30, 100_000).is_none());
        assert!(Transcript::default()
            .range(Selection::new(0, 0), FPS30, 100)
            .is_none());
    }

    #[test]
    fn the_word_under_the_playhead_is_found() {
        let t = sample();
        assert_eq!(t.word_at(1400), Some(1), "mid-word");
        assert_eq!(
            t.word_at(3000),
            Some(2),
            "in a silence, the last word spoken stays current"
        );
        assert_eq!(t.word_at(0), Some(0), "before the first word");
        assert_eq!(t.word_at(99_000), Some(6), "past the end, the last word");
    }

    /// Frame conversion is exact rational arithmetic, so a long clip at a
    /// broadcast rate cannot drift.
    #[test]
    fn milliseconds_convert_without_drift_at_29_97() {
        let ntsc = Rational::new(30000, 1001);
        // One hour in. 3600 s x 29.97002997 = 107892.1 frames.
        assert_eq!(ms_to_frame(3_600_000, ntsc), 107_892);
        assert_eq!(ms_to_frame(0, ntsc), 0);
        // Two hours, checking nothing overflows on the way.
        assert_eq!(ms_to_frame(7_200_000, ntsc), 215_784);
    }

    #[test]
    fn an_empty_transcript_says_so() {
        assert!(Transcript::default().is_empty());
        assert!(Transcript {
            segments: vec![Segment { words: vec![] }]
        }
        .is_empty());
        assert!(!sample().is_empty());
    }
}
