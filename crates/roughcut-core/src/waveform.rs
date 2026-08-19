//! Audio peaks for the scrub bar.
//!
//! A clip's loudness envelope is the cheapest way to find the moment something
//! happens in it: a laugh, a door, the point the music starts. This reduces a
//! whole clip to a couple of kilobytes — one byte per bucket — small enough
//! that every clip you open can keep one resident for good.
//!
//! Peaks, not RMS. The question being answered is "where is something loud",
//! and an averaged envelope flattens exactly the transients that mark it.

use anyhow::{bail, Context, Result};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::Stdio;

/// Buckets per clip.
///
/// The bar is at most a couple of thousand pixels wide, so this is roughly one
/// bucket per pixel at full screen and never fewer. At one byte each a clip
/// costs 2 KiB, which is why these are never evicted.
pub const BUCKETS: usize = 2048;

/// Sample rate asked of ffmpeg. Far below anything audible, because nothing
/// here is listened to — 8 kHz still gives ~230 samples per bucket on a
/// one-minute clip, and decoding less costs less.
const RATE: u32 = 8000;

/// Samples reduced to a single stored peak before bucketing.
///
/// Bucketing directly would need the total sample count up front, which means
/// either trusting the container's duration or holding the whole decode in
/// memory. Reducing in fixed slices first needs neither: an hour of audio
/// becomes 450 KiB of slice peaks, and the exact bucket boundaries are worked
/// out at the end, when the real length is known.
const SLICE: usize = 128;

/// The quietest peak still stretched to full height.
///
/// Each clip is scaled to its own loudest moment, because the question this
/// answers is "where in *this* clip does something happen" — a clip is looked
/// at on its own, in the monitor, not next to another one. Scaling to full
/// scale instead would draw almost everything as a flat line: phone audio
/// rarely comes near 0 dBFS, and one measured here peaks at -39.
///
/// The floor is what stops that reasoning running away with itself. Below
/// about -40 dBFS there is nothing there but room tone, and stretching it to
/// full height would draw a picture of silence that looks like a picture of a
/// conversation.
const FULL_HEIGHT_AT: u32 = 328;

/// Decode the audio to raw mono 16-bit samples on stdout.
pub fn ffmpeg_args(path: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "-v".into(),
        "error".into(),
        "-i".into(),
        path.as_os_str().to_os_string(),
        // Video decoding is by far the expensive half of this file, and none
        // of it is wanted.
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        RATE.to_string().into(),
        "-f".into(),
        "s16le".into(),
        "-".into(),
    ]
}

/// Reduce a stream of raw mono 16-bit samples to `buckets` peak levels.
pub fn peaks<R: Read>(reader: R, buckets: usize) -> Result<Vec<u8>> {
    let buckets = buckets.max(1);
    let mut reader = BufReader::new(reader);

    let mut slices: Vec<u16> = Vec::new();
    let mut peak: u16 = 0;
    let mut n = 0usize;
    let mut sample = [0u8; 2];
    loop {
        match reader.read_exact(&mut sample) {
            Ok(()) => {}
            // A stream that ends mid-sample is truncated, not broken: keep
            // whatever did arrive.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e).context("cannot read decoded audio"),
        }
        peak = peak.max(i16::from_le_bytes(sample).unsigned_abs());
        n += 1;
        if n == SLICE {
            slices.push(peak);
            peak = 0;
            n = 0;
        }
    }
    if n > 0 {
        slices.push(peak);
    }
    if slices.is_empty() {
        bail!("no audio samples");
    }

    let loudest = slices.iter().copied().max().unwrap_or(0) as u32;
    let scale = 255.0 / loudest.max(FULL_HEIGHT_AT) as f32;

    let len = slices.len();
    Ok((0..buckets)
        .map(|i| {
            let from = i * len / buckets;
            let to = ((i + 1) * len / buckets).max(from + 1).min(len);
            let peak = slices[from..to].iter().copied().max().unwrap_or(0);
            (peak as f32 * scale).min(255.0) as u8
        })
        .collect())
}

/// Run ffmpeg over `path` and reduce what comes back.
pub fn extract(ffmpeg: &Path, path: &Path, buckets: usize) -> Result<Vec<u8>> {
    let mut child = crate::tools::quiet_command(ffmpeg)
        .args(ffmpeg_args(path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Discarded rather than piped: nothing reads it, and a pipe nobody
        // drains is a decode that stops the moment the buffer fills.
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run ffmpeg at {}", ffmpeg.display()))?;

    let stdout = child.stdout.take().context("ffmpeg gave no output")?;
    let result = peaks(stdout, buckets);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().context("ffmpeg did not finish cleanly")?;
    let peaks = result?;

    // Samples arrived, so there is something worth drawing. ffmpeg complaining
    // afterwards about a damaged tail is not a reason to show nothing.
    if !status.success() {
        log::debug!(
            "ffmpeg exited {status} reading audio from {} — keeping what it gave",
            path.display()
        );
    }
    Ok(peaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` samples of a constant level, as raw little-endian bytes.
    fn tone(level: i16, n: usize) -> Vec<u8> {
        level
            .to_le_bytes()
            .iter()
            .copied()
            .cycle()
            .take(n * 2)
            .collect()
    }

    #[test]
    fn a_loud_clip_fills_the_bar() {
        let got = peaks(std::io::Cursor::new(tone(32000, SLICE * 8)), 8).unwrap();
        assert_eq!(got.len(), 8);
        assert!(got.iter().all(|&v| v > 240), "{got:?}");
    }

    #[test]
    fn silence_reads_as_silence() {
        let got = peaks(std::io::Cursor::new(tone(0, SLICE * 4)), 4).unwrap();
        assert_eq!(got, vec![0, 0, 0, 0]);
    }

    /// The whole point: a quiet passage next to a loud one has to look
    /// different, and it is the loud one that gets found.
    #[test]
    fn a_quiet_half_stays_visibly_quieter_than_a_loud_half() {
        let mut pcm = tone(2000, SLICE * 4);
        pcm.extend(tone(30000, SLICE * 4));
        let got = peaks(std::io::Cursor::new(pcm), 8).unwrap();
        let (quiet, loud) = got.split_at(4);
        assert!(quiet.iter().all(|&v| v < 40), "{quiet:?}");
        assert!(loud.iter().all(|&v| v > 200), "{loud:?}");
    }

    /// Room tone is not amplified into looking like something happening.
    #[test]
    fn near_silence_stays_near_the_floor() {
        // About -44 dBFS: below the floor, so it is drawn at its real size.
        let got = peaks(std::io::Cursor::new(tone(200, SLICE * 4)), 4).unwrap();
        assert!(got.iter().all(|&v| v < 160), "{got:?}");
    }

    /// A clip recorded quietly still has to show its shape. This is measured
    /// from real footage: a phone clip peaking at -39 dBFS is a normal thing
    /// to be handed, and drawing it as a flat line makes the bar useless for
    /// exactly the clips that most need skimming.
    #[test]
    fn a_quietly_recorded_clip_still_shows_its_shape() {
        let mut pcm = tone(40, SLICE * 4); // room tone
        pcm.extend(tone(360, SLICE * 4)); // -39 dBFS, the loudest thing in it
        let got = peaks(std::io::Cursor::new(pcm), 8).unwrap();
        let (quiet, loud) = got.split_at(4);
        assert!(loud.iter().all(|&v| v > 200), "the peak is lost: {loud:?}");
        assert!(quiet.iter().all(|&v| v < 60), "no contrast: {quiet:?}");
    }

    #[test]
    fn fewer_slices_than_buckets_still_fills_every_bucket() {
        // Two slices, sixteen buckets: stretched, never empty, never panicking.
        let got = peaks(std::io::Cursor::new(tone(32000, SLICE * 2)), 16).unwrap();
        assert_eq!(got.len(), 16);
        assert!(got.iter().all(|&v| v > 240));
    }

    #[test]
    fn a_trailing_part_sample_is_kept_rather_than_rejected() {
        let mut pcm = tone(32000, SLICE);
        pcm.push(0x7f); // half a sample
        let got = peaks(std::io::Cursor::new(pcm), 2).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn an_empty_stream_is_an_error_rather_than_a_flat_line() {
        assert!(peaks(std::io::Cursor::new(Vec::new()), 8).is_err());
    }

    #[test]
    fn the_command_decodes_audio_only() {
        let args: Vec<String> = ffmpeg_args(Path::new("/m/a.mp4"))
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-vn".to_string()), "{args:?}");
        assert!(args.contains(&"s16le".to_string()), "{args:?}");
        assert_eq!(args.last().unwrap(), "-", "output must go to stdout");
    }
}
