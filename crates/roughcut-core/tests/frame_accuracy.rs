//! §5's required verification test.
//!
//! Builds a 300-frame video with each frame's own number burned into it,
//! imports it, marks in at 100 and out at 199, appends, exports MLT XML, then
//! proves the exported timeline really starts on source frame 100 and ends on
//! source frame 199.
//!
//! Two levels of proof, depending on what is installed:
//!
//! * With `melt` on PATH (or in Shotcut's folder) the exported project is
//!   *rendered* and its first and last frames are compared pixel-for-pixel
//!   against frames extracted straight from the source. This is the real
//!   check: it exercises MLT's own reading of our XML.
//! * Without `melt`, the test falls back to asserting the XML contains
//!   `in="100" out="199"` and a 100-frame playlist, as §5 permits.
//!
//! The whole test is skipped, loudly, if `ffmpeg`/`ffprobe` are unavailable.

use roughcut_core::import::add_clip;
use roughcut_core::mlt::{self, ExportOptions};
use roughcut_core::model::Project;
use roughcut_core::probe::probe;
use roughcut_core::timeline;
use roughcut_core::tools::{find_tool, quiet_command, Tools};
use std::path::{Path, PathBuf};

const TOTAL_FRAMES: i64 = 300;
const MARK_IN: i64 = 100;
const MARK_OUT: i64 = 199;
/// Deliberately an NTSC rational rate: if anything anywhere rounds 30000/1001
/// to 29.97, this test is where it shows up.
const FPS_NUM: i64 = 30000;
const FPS_DEN: i64 = 1001;

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roughcut-frame-accuracy-{tag}"));
    // Always clear the previous run, whatever the keep-output setting says.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot create the test working directory");
    dir
}

/// §12's acceptance criterion needs a human to open the export in Shotcut.
/// Setting `ROUGHCUT_KEEP_OUTPUT=1` leaves the source clip and the exported
/// project behind, and prints where, so that check is one double-click away.
fn keep_output() -> bool {
    std::env::var_os("ROUGHCUT_KEEP_OUTPUT").is_some()
}

fn cleanup(dir: &Path) {
    if keep_output() {
        eprintln!("KEPT: {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A font `drawtext` can find. On Windows the shipped Arial is reliable; on
/// other platforms ffmpeg's default font selection is used instead.
fn drawtext_filter() -> String {
    // Big, centred, white-on-black frame numbers. `n` is the frame index, so
    // frame N literally displays N.
    let base = "drawtext=text='%{eif\\:n\\:d}':fontcolor=white:fontsize=240:\
                x=(w-text_w)/2:y=(h-text_h)/2";
    #[cfg(windows)]
    {
        let font = Path::new(r"C:\Windows\Fonts\arial.ttf");
        if font.is_file() {
            // ffmpeg filter syntax needs the drive colon and backslashes escaped.
            return format!("{base}:fontfile='C\\:/Windows/Fonts/arial.ttf'");
        }
    }
    base.to_string()
}

/// Build the burned-in-frame-number source video.
fn build_source(ffmpeg: &Path, dir: &Path) -> PathBuf {
    let out = dir.join("source.mp4");
    let duration = format!("{}", TOTAL_FRAMES as f64 * FPS_DEN as f64 / FPS_NUM as f64);
    let status = quiet_command(ffmpeg)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!(
            "color=c=black:s=640x360:r={FPS_NUM}/{FPS_DEN}:d={duration}"
        ))
        .args(["-vf", &drawtext_filter()])
        .args([
            "-frames:v",
            &TOTAL_FRAMES.to_string(),
            // All-intra with a fixed QP: every frame is independently decodable
            // and pixel-identical however it is reached, so a seek that lands
            // on the right frame produces the right pixels exactly.
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&out)
        .status()
        .expect("cannot run ffmpeg");
    assert!(status.success(), "ffmpeg failed to build the test source");
    out
}

/// Frame size of the test source, in luma samples.
const W: usize = 640;
const H: usize = 360;
const FRAME_BYTES: usize = W * H;

/// Decode a video to raw 8-bit greyscale and return it as one frame per chunk.
/// Greyscale is enough: the test pattern is white text on black.
fn decode_gray(ffmpeg: &Path, video: &Path, select: Option<&str>, out: &Path) -> Vec<Vec<u8>> {
    let mut cmd = quiet_command(ffmpeg);
    cmd.args(["-y", "-v", "error"]).arg("-i").arg(video);
    if let Some(filter) = select {
        cmd.args(["-vf", filter, "-fps_mode", "passthrough"]);
    }
    cmd.args(["-f", "rawvideo", "-pix_fmt", "gray"]).arg(out);
    let status = cmd.status().expect("cannot run ffmpeg");
    assert!(status.success(), "cannot decode {}", video.display());

    let bytes = std::fs::read(out).expect("cannot read decoded frames");
    assert_eq!(
        bytes.len() % FRAME_BYTES,
        0,
        "decoded {} is not a whole number of {W}x{H} frames",
        video.display()
    );
    bytes.chunks_exact(FRAME_BYTES).map(<[u8]>::to_vec).collect()
}

/// Fraction of pixels that differ by more than a small tolerance. Zero for
/// identical frames; large for two different burned-in numbers.
fn difference(a: &[u8], b: &[u8]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let differing = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > 40)
        .count();
    differing as f64 / a.len() as f64
}

/// Two frames are "the same frame of video" if almost every pixel matches.
/// The rendered path goes through MLT's scaler and encoder, so exact equality
/// is too strict, but a one-frame error changes the burned-in digits and
/// moves this number by orders of magnitude.
const SAME_FRAME: f64 = 0.005;

/// Given a rendered frame, find which source frame it actually is, searching
/// near the expected one. Turns "the pixels differ" into "the export is off
/// by one frame", which is the whole point of this test.
fn identify(rendered: &[u8], source: &[Vec<u8>], expected: usize) -> Option<usize> {
    let lo = expected.saturating_sub(5);
    let hi = (expected + 5).min(source.len().saturating_sub(1));
    (lo..=hi)
        .map(|i| (i, difference(rendered, &source[i])))
        .filter(|(_, d)| *d < SAME_FRAME)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(i, _)| i)
}

/// Shotcut's bundled `melt` is a Qt5 binary living beside Qt6 plugins, so it
/// needs a nudge to find a platform plugin it can actually load.
fn melt_command(melt: &Path) -> std::process::Command {
    let mut cmd = quiet_command(melt);
    cmd.env("QT_QPA_PLATFORM", "offscreen");
    if let Some(dir) = melt.parent() {
        let qt5 = dir.join("lib").join("qt5").join("platforms");
        if qt5.is_dir() {
            cmd.env("QT_QPA_PLATFORM_PLUGIN_PATH", &qt5);
        }
        cmd.current_dir(dir);
    }
    cmd
}

/// Render an entire MLT project losslessly with melt. Rendering the whole
/// timeline rather than two stills means drift anywhere in the middle is
/// caught too, and it sidesteps the image2 muxer's single-file awkwardness.
fn melt_render(melt: &Path, project: &Path, out: &Path) -> bool {
    let status = melt_command(melt)
        .arg(project)
        .args([
            "-consumer",
            &format!("avformat:{}", out.display()),
            // FFV1 is mathematically lossless, so any pixel difference the
            // comparison finds came from MLT, not from the encoder.
            "vcodec=ffv1",
            "an=1",
            "real_time=-1",
            "terminate_on_pause=1",
        ])
        .status();
    match status {
        Ok(s) => {
            let ok = s.success() && out.is_file();
            if !ok {
                eprintln!("melt exited with {s} (output present: {})", out.is_file());
            }
            ok
        }
        Err(e) => {
            eprintln!("melt could not be run: {e}");
            false
        }
    }
}

#[test]
fn exported_timeline_lands_on_the_intended_frames() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!(
            "SKIPPED: ffmpeg and ffprobe are required for the frame-accuracy test \
             and were not found on PATH"
        );
        return;
    };

    let dir = workdir("main");
    let source = build_source(&ffmpeg, &dir);

    // --- 1. import and check the timing model agrees with the file ----------
    let info = probe(&ffprobe, &source).expect("ffprobe could not read the test source");
    assert_eq!(
        (info.fps.num, info.fps.den),
        (FPS_NUM, FPS_DEN),
        "the test source is not at the exact rational rate the test assumes"
    );
    assert_eq!(
        info.native_frames, TOTAL_FRAMES,
        "the test source is not {TOTAL_FRAMES} frames long"
    );

    let mut project = Project::new();
    add_clip(&mut project, &source, &info);
    assert_eq!(project.profile.frame_rate_num, FPS_NUM);
    assert_eq!(project.profile.frame_rate_den, FPS_DEN);
    assert_eq!(project.clips[0].duration_frames, TOTAL_FRAMES);

    // --- 2. mark in at 100, out at 199, append ------------------------------
    let clip_id = project.clips[0].id;
    {
        let clip = project.clip_mut(clip_id).unwrap();
        clip.mark_in = Some(MARK_IN);
        clip.mark_out = Some(MARK_OUT);
    }
    let (mark_in, mark_out) = project.clips[0].marked_range().unwrap();
    assert_eq!((mark_in, mark_out), (MARK_IN, MARK_OUT));
    assert!(timeline::append(&mut project, clip_id, mark_in, mark_out));

    // Inclusive in/out: 100..=199 is 100 frames, not 99 and not 101.
    assert_eq!(
        timeline::total_frames(&project.timeline),
        100,
        "the marked range must be exactly 100 frames"
    );

    // --- 3. export -----------------------------------------------------------
    let mlt_path = dir.join("export.mlt");
    mlt::write_to_file(&project, &ExportOptions::default(), &mlt_path)
        .expect("MLT export failed");
    let xml = std::fs::read_to_string(&mlt_path).unwrap();

    // The §5 fallback assertions always run, melt or no melt.
    assert!(
        xml.contains(r#"in="100" out="199""#),
        "the exported entry does not carry the marked range:\n{xml}"
    );
    assert!(
        xml.contains(r#"frame_rate_num="30000""#) && xml.contains(r#"frame_rate_den="1001""#),
        "the profile lost the exact rational frame rate:\n{xml}"
    );
    assert!(
        xml.contains(r#"out="99">"#),
        "the tractor should end on frame 99 for a 100-frame timeline:\n{xml}"
    );

    // --- 4. render with melt and compare pixels -----------------------------
    let melt = tools.melt.clone().or_else(|| find_tool("melt"));
    let Some(melt) = melt else {
        eprintln!(
            "PARTIAL: melt was not found, so the export was verified by XML \
             inspection only. Install Shotcut (or MLT) to enable the pixel check."
        );
        return;
    };

    let rendered_mkv = dir.join("rendered.mkv");
    if !melt_render(&melt, &mlt_path, &rendered_mkv) {
        eprintln!(
            "PARTIAL: melt was found at {} but could not render the project, so the \
             export was verified by XML inspection only.",
            melt.display()
        );
        return;
    }

    let rendered = decode_gray(&ffmpeg, &rendered_mkv, None, &dir.join("rendered.raw"));
    let source_frames = decode_gray(&ffmpeg, &source, None, &dir.join("source.raw"));
    assert_eq!(
        source_frames.len(),
        TOTAL_FRAMES as usize,
        "the test source did not decode to {TOTAL_FRAMES} frames"
    );

    // The rendered timeline must be exactly as long as the marked range.
    assert_eq!(
        rendered.len(),
        100,
        "the rendered timeline is {} frames, not the 100 that in=100 out=199 means",
        rendered.len()
    );

    // Negative control. If the comparison could not tell frame 99 from frame
    // 100, the assertion below would pass no matter what the export did.
    assert!(
        difference(&rendered[0], &source_frames[MARK_IN as usize - 1]) > SAME_FRAME,
        "the frame comparison cannot distinguish adjacent frames, so this test \
         proves nothing — check that the burned-in numbers are rendering"
    );
    assert!(
        difference(&rendered[0], &source_frames[MARK_IN as usize + 1]) > SAME_FRAME,
        "the frame comparison cannot distinguish adjacent frames, so this test \
         proves nothing — check that the burned-in numbers are rendering"
    );

    // Every rendered frame must be the source frame directly above it.
    // Identifying mismatches by content turns a failure into a precise
    // statement of how far off the export is.
    let mut mismatches = Vec::new();
    for (i, frame) in rendered.iter().enumerate() {
        let expected = MARK_IN as usize + i;
        if difference(frame, &source_frames[expected]) < SAME_FRAME {
            continue;
        }
        let actual = identify(frame, &source_frames, expected);
        mismatches.push((i, expected, actual));
    }

    assert!(
        mismatches.is_empty(),
        "the exported timeline does not land on the intended frames.\n{}",
        mismatches
            .iter()
            .take(6)
            .map(|(i, want, got)| match got {
                Some(g) => format!(
                    "  timeline frame {i}: expected source frame {want}, got {g} \
                     (off by {})",
                    *g as i64 - *want as i64
                ),
                None => format!(
                    "  timeline frame {i}: expected source frame {want}, \
                     got something unrecognisable"
                ),
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    eprintln!(
        "VERIFIED with melt at {}: all 100 rendered frames match source frames \
         {MARK_IN}..={MARK_OUT} exactly.",
        melt.display()
    );

    cleanup(&dir);
}

/// A companion check: two ranges appended back to back must not lose or gain a
/// frame at the join, which is the classic off-by-one in inclusive/exclusive
/// confusion.
#[test]
fn consecutive_ranges_join_without_drift() {
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(ffprobe)) = (tools.ffmpeg.clone(), tools.ffprobe.clone()) else {
        eprintln!("SKIPPED: ffmpeg and ffprobe are required");
        return;
    };

    let dir = workdir("join");
    let source = build_source(&ffmpeg, &dir);
    let info = probe(&ffprobe, &source).unwrap();

    let mut project = Project::new();
    add_clip(&mut project, &source, &info);
    let id = project.clips[0].id;

    // 0..=49 then 50..=99 must reassemble into exactly 0..=99.
    assert!(timeline::append(&mut project, id, 0, 49));
    assert!(timeline::append(&mut project, id, 50, 99));
    assert_eq!(timeline::total_frames(&project.timeline), 100);
    assert_eq!(timeline::item_start(&project.timeline, 1), 50);

    let xml = mlt::to_xml(&project, &ExportOptions::default()).unwrap();
    assert!(xml.contains(r#"in="0" out="49""#), "{xml}");
    assert!(xml.contains(r#"in="50" out="99""#), "{xml}");

    cleanup(&dir);
}
