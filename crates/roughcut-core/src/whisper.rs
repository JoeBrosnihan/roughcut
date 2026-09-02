//! Driving whisper.cpp to produce a transcript.
//!
//! Nothing is vendored and nothing is linked: `whisper-cli` is discovered on
//! disk exactly as ffmpeg, ffprobe, melt and mpv are, and Roughcut degrades to
//! "no transcripts" without it. That also keeps the model — over a gigabyte of
//! it — out of the repository and out of the build.
//!
//! Transcription happens entirely on this machine. The audio of somebody's
//! family holiday is not something to post to a third party in order to find
//! out where the laughing is.

use crate::tools::{background_command, background_threads};
use crate::transcript::{Segment, Transcript, Word as TWord};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Sample rate whisper.cpp requires. Not a preference — it resamples nothing
/// and refuses anything else.
const RATE: u32 = 16_000;

/// Decode a clip's audio to the mono 16-bit WAV whisper.cpp expects.
pub fn wav_args(source: &Path, dest: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-threads".into(),
        background_threads().to_string().into(),
        "-i".into(),
        source.as_os_str().to_os_string(),
        // The picture is the expensive half of the file and none of it is
        // wanted.
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        RATE.to_string().into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        dest.as_os_str().to_os_string(),
    ]
}

/// Where a model lives, given the `whisper-cli` binary.
///
/// Beside the executable in `models/`, which is whisper.cpp's own layout, so a
/// stock unzip of its release plus a downloaded model needs no configuration.
pub fn find_model(whisper: &Path) -> Option<PathBuf> {
    let dir = whisper.parent()?.join("models");
    let mut best: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if !name.starts_with("ggml-") || !name.ends_with(".bin") {
            continue;
        }
        // Prefer the largest, which is the most accurate of whatever is
        // installed. Somebody who downloaded two models meant to use the
        // better one.
        let bigger = best
            .as_ref()
            .and_then(|b| Some(path.metadata().ok()?.len() > b.metadata().ok()?.len()))
            .unwrap_or(true);
        if bigger {
            best = Some(path);
        }
    }
    best
}

/// Extract `source`'s audio to `dest` as 16 kHz mono WAV.
pub fn extract_audio(ffmpeg: &Path, source: &Path, dest: &Path) -> Result<()> {
    let output = background_command(ffmpeg)
        .args(wav_args(source, dest))
        .output()
        .with_context(|| format!("cannot run ffmpeg at {}", ffmpeg.display()))?;
    if !output.status.success() {
        bail!(
            "ffmpeg could not read the audio of {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !dest.is_file() {
        bail!("ffmpeg wrote no audio for {}", source.display());
    }
    Ok(())
}

/// The command line, split out so the test can assert on it without running a
/// transcription.
///
/// `-ml 1 -sow` is the important part. whisper.cpp will happily emit
/// token-level timestamps instead, and they cannot be used: on real speech
/// they come back non-monotonic — a word measured here claimed to end at
/// 16410 ms having started at 24120. Asking for one word per *segment* routes
/// every word through the segment timing path, which round-trips correctly:
/// cut the range it reports and whisper reads back the same words.
pub fn whisper_args(model: &Path, wav: &Path, out_prefix: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "-m".into(),
        model.as_os_str().to_os_string(),
        "-f".into(),
        wav.as_os_str().to_os_string(),
        // One word per segment, split on word rather than mid-token.
        "-ml".into(),
        "1".into(),
        "-sow".into(),
        "-oj".into(),
        "-of".into(),
        out_prefix.as_os_str().to_os_string(),
        // Nothing on stdout that has to be parsed; the JSON file is the
        // contract.
        "-np".into(),
    ]
}

/// Transcribe `source`, start to finish.
///
/// Writes a WAV beside the output prefix, runs whisper over it, reads the JSON
/// back, and removes both. The scratch files are the caller's directory to
/// choose, so a cancelled or crashed run leaves nothing anywhere surprising.
pub fn transcribe(
    ffmpeg: &Path,
    whisper: &Path,
    model: &Path,
    source: &Path,
    scratch: &Path,
) -> Result<Transcript> {
    std::fs::create_dir_all(scratch)
        .with_context(|| format!("cannot create {}", scratch.display()))?;
    let stem = format!(
        "rc-{:x}",
        source.to_string_lossy().bytes().fold(0u64, |h, b| h
            .wrapping_mul(31)
            .wrapping_add(b as u64))
    );
    let wav = scratch.join(format!("{stem}.wav"));
    let prefix = scratch.join(&stem);
    let json_path = scratch.join(format!("{stem}.json"));

    let cleanup = |wav: &Path, json: &Path| {
        let _ = std::fs::remove_file(wav);
        let _ = std::fs::remove_file(json);
    };

    extract_audio(ffmpeg, source, &wav)?;

    let output = background_command(whisper)
        .args(whisper_args(model, &wav, &prefix))
        // whisper.cpp finds its CUDA and ggml DLLs beside the executable.
        .current_dir(whisper.parent().unwrap_or(Path::new(".")))
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            cleanup(&wav, &json_path);
            return Err(e).with_context(|| format!("cannot run {}", whisper.display()));
        }
    };
    if !output.status.success() {
        cleanup(&wav, &json_path);
        bail!(
            "whisper failed on {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("no output")
        );
    }

    let json = std::fs::read_to_string(&json_path)
        .with_context(|| format!("whisper wrote no transcript for {}", source.display()));
    cleanup(&wav, &json_path);
    parse_json(&json?)
}

/// Markers whisper emits that are not words.
fn is_noise(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        // Speaker-change marker. Real information, but not a word, and it
        // would be selected and cut like one.
        || t == ">>"
        // [BLANK_AUDIO], [_BEG_], [_TT_1302], [ Silence ], (wind blowing)
        || (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || t.chars().all(|c| c == '♪')
}

/// A word ends a sentence, and so ends a paragraph in the document.
fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', ')', ']', '♪'])
        .ends_with(['.', '!', '?'])
}

/// Silence long enough to be a change of subject, even without punctuation.
/// Whisper omits full stops surprisingly often on conversational speech.
const PARAGRAPH_GAP_MS: i64 = 2000;

/// A paragraph never runs longer than this, whatever the punctuation says.
///
/// Singing is the case that forces it: whisper transcribes lyrics without a
/// full stop anywhere, so a song becomes one unbroken run of words. Measured
/// on a clip here, 29 words with no break at all — readable, but a long take
/// would be a wall of text with nothing to aim a selection at.
const MAX_PARAGRAPH_WORDS: usize = 40;

/// Turn whisper.cpp's JSON into a transcript.
pub fn parse_json(json: &str) -> Result<Transcript> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("whisper produced unparseable JSON")?;
    let entries = value
        .get("transcription")
        .and_then(|t| t.as_array())
        .context("whisper's JSON has no transcription")?;

    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Vec<TWord> = Vec::new();
    let mut previous_end: Option<i64> = None;

    for entry in entries {
        let text = entry.get("text").and_then(|t| t.as_str()).unwrap_or("");
        if is_noise(text) {
            continue;
        }
        let offsets = entry.get("offsets");
        let (Some(start), Some(end)) = (
            offsets.and_then(|o| o.get("from")).and_then(|v| v.as_i64()),
            offsets.and_then(|o| o.get("to")).and_then(|v| v.as_i64()),
        ) else {
            continue;
        };

        // A long silence starts a new paragraph, so the document breaks where
        // the conversation does.
        if let Some(prev) = previous_end {
            if start - prev >= PARAGRAPH_GAP_MS && !current.is_empty() {
                segments.push(Segment {
                    words: std::mem::take(&mut current),
                });
            }
        }
        previous_end = Some(end);

        current.push(TWord {
            text: text.trim().to_string(),
            start_ms: start,
            // Whisper reports zero-length words when it is unsure. Give them
            // the frame or two they need to be selectable.
            end_ms: end.max(start),
        });

        if ends_sentence(text) || current.len() >= MAX_PARAGRAPH_WORDS {
            segments.push(Segment {
                words: std::mem::take(&mut current),
            });
        }
    }
    if !current.is_empty() {
        segments.push(Segment { words: current });
    }

    Ok(Transcript { segments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Rational;

    /// Real whisper.cpp output, captured from a clip of the user's own
    /// footage rather than composed to match the parser. Writing a fixture
    /// from memory is how the melt progress bar shipped broken.
    const REAL: &str = include_str!("../tests/fixtures_whisper.json");

    #[test]
    fn a_recorded_transcript_becomes_sentences_of_words() {
        let t = parse_json(REAL).expect("the real output has to parse");

        // Four sentences, and none of the markers.
        let texts: Vec<String> = t.segments.iter().map(|s| s.text()).collect();
        assert_eq!(
            texts,
            vec!["What is that?", "It's fine?", "I've been there.", "It's fine."]
        );

        // `>>`, `[BLANK_AUDIO]` and the empty entries are gone.
        assert!(
            !t.words().any(|w| w.text.contains(">>") || w.text.contains("BLANK")),
            "a marker survived as a word"
        );
        assert_eq!(t.word_count(), 10);
    }

    #[test]
    fn every_word_ends_no_earlier_than_it_starts_and_they_run_forwards() {
        let t = parse_json(REAL).unwrap();
        let words: Vec<_> = t.words().collect();
        for w in &words {
            assert!(w.end_ms >= w.start_ms, "{w:?} ends before it starts");
        }
        for pair in words.windows(2) {
            assert!(
                pair[1].start_ms >= pair[0].start_ms,
                "{:?} then {:?} runs backwards",
                pair[0],
                pair[1]
            );
        }
    }

    /// The point of the whole thing: pick a sentence, get frames to cut.
    #[test]
    fn selecting_a_sentence_gives_the_frames_it_occupies() {
        use crate::transcript::Selection;
        let t = parse_json(REAL).unwrap();
        let fps = Rational::new(30, 1);

        // "I've been there." is words 6..=8 of the flattened transcript.
        let words: Vec<_> = t.words().map(|w| w.text.clone()).collect();
        let from = words.iter().position(|w| w == "I've").unwrap();
        let to = words.iter().position(|w| w == "there.").unwrap();
        let (in_f, out_f) = t.range(Selection::new(from, to), fps, 100_000).unwrap();

        // 26.830 s - 0.25 padding = 26.580 -> frame 797.
        // 27.420 s + 0.40 padding = 27.820 -> frame 834.
        assert_eq!((in_f, out_f), (797, 834));
    }

    #[test]
    fn a_blank_or_broken_response_is_an_error_rather_than_a_panic() {
        assert!(parse_json("not json").is_err());
        assert!(parse_json("{}").is_err());
        // Valid, but nothing was said.
        let empty = parse_json(r#"{"transcription":[]}"#).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn silence_starts_a_new_paragraph_even_without_punctuation() {
        let json = r#"{"transcription":[
            {"offsets":{"from":0,"to":500},"text":" one"},
            {"offsets":{"from":500,"to":900},"text":" two"},
            {"offsets":{"from":9000,"to":9400},"text":" three"}
        ]}"#;
        let t = parse_json(json).unwrap();
        assert_eq!(t.segments.len(), 2, "an eight-second gap is a new paragraph");
        assert_eq!(t.segments[0].text(), "one two");
        assert_eq!(t.segments[1].text(), "three");
    }

    /// Lyrics arrive without a full stop anywhere, so something other than
    /// punctuation has to break them up.
    #[test]
    fn an_unpunctuated_run_is_broken_into_readable_paragraphs() {
        let words: Vec<String> = (0..95)
            .map(|i| {
                format!(
                    r#"{{"offsets":{{"from":{},"to":{}}},"text":" la"}}"#,
                    i * 300,
                    i * 300 + 250
                )
            })
            .collect();
        let json = format!(r#"{{"transcription":[{}]}}"#, words.join(","));
        let t = parse_json(&json).unwrap();

        assert_eq!(t.word_count(), 95, "no word is lost to the break");
        assert!(t.segments.len() >= 3, "{} paragraphs", t.segments.len());
        for seg in &t.segments {
            assert!(
                seg.words.len() <= MAX_PARAGRAPH_WORDS,
                "a paragraph of {} words is a wall of text",
                seg.words.len()
            );
        }
    }

    #[test]
    fn the_command_asks_for_one_word_per_segment() {
        let args: Vec<String> = whisper_args(
            Path::new("/m/ggml.bin"),
            Path::new("/t/a.wav"),
            Path::new("/t/out"),
        )
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
        // Token timestamps are not usable; this is what makes whisper report
        // per-word segment timings instead.
        let ml = args.iter().position(|a| a == "-ml").expect("-ml");
        assert_eq!(args[ml + 1], "1");
        assert!(args.contains(&"-sow".to_string()), "{args:?}");
        assert!(args.contains(&"-oj".to_string()), "{args:?}");
    }

    #[test]
    fn the_audio_is_extracted_at_whispers_one_acceptable_rate() {
        let args: Vec<String> = wav_args(Path::new("/m/a.mov"), Path::new("/t/a.wav"))
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"16000".to_string()), "{args:?}");
        assert!(args.contains(&"pcm_s16le".to_string()), "{args:?}");
        assert!(args.contains(&"-vn".to_string()), "the video is not wanted");
        // Mono: whisper.cpp rejects anything else outright.
        let ac = args.iter().position(|a| a == "-ac").expect("channel count");
        assert_eq!(args[ac + 1], "1");
    }
}
