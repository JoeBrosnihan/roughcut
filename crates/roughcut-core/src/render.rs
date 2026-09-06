// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Rendering the cut to an MP4, by handing the MLT to `melt`.
//!
//! Roughcut does not encode video itself and has no intention of learning how.
//! `melt` is MLT's own renderer, it ships with Shotcut, and it consumes exactly
//! the XML this program already writes — so the whole feature is one child
//! process and the discipline to report what it is doing.
//!
//! What it is doing matters: a long timeline takes minutes, and a progress bar
//! that only moves at the end is worse than none. `melt` writes its position to
//! stderr as it goes, which is parsed here into frame counts.

use crate::tools::quiet_command;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// H.264 quality, as x264's constant-rate-factor.
///
/// 18 is visually lossless for the rough cuts this produces. Roughcut's output
/// is something you watch or hand to someone, not a master — the master comes
/// out of Shotcut later, from the same original files.
const CRF: &str = "18";

/// The melt command line, split out so the test can assert on it without
/// running a render.
pub fn melt_args(project: &Path, out: &Path) -> Vec<std::ffi::OsString> {
    vec![
        // Without this melt renders in silence and prints two lines of
        // `Current Position:` when it is already finished. It is the only
        // thing that makes it emit `Current Frame: N, percentage: P` as it
        // goes, and therefore the only thing that makes the progress bar move.
        "-progress".into(),
        project.into(),
        "-consumer".into(),
        format!("avformat:{}", out.display()).into(),
        "vcodec=libx264".into(),
        format!("crf={CRF}").into(),
        // `veryfast` because this is a rough cut and waiting is the cost that
        // actually gets felt; the quality difference at crf 18 is slight.
        "preset=veryfast".into(),
        "pix_fmt=yuv420p".into(),
        // Faststart, so the file plays while it is still being copied
        // somewhere, which is what happens to these.
        "movflags=+faststart".into(),
        "acodec=aac".into(),
        "ab=192k".into(),
        // Render as fast as the machine allows rather than in real time, and
        // stop at the end instead of sitting on the last frame.
        "real_time=-1".into(),
        "terminate_on_pause=1".into(),
    ]
}

/// `melt` reports progress as `Current Frame: N, percentage: P`.
///
/// Returned as the frame number, which is the useful half: the percentage is
/// derived from a length this side already knows exactly.
pub fn parse_progress(line: &str) -> Option<i64> {
    let at = line.find("Current Frame:")? + "Current Frame:".len();
    let rest = &line[at..];
    let end = rest.find(',').unwrap_or(rest.len());
    rest[..end].trim().parse().ok()
}

/// Render `project` (an MLT file) to `out`, reporting progress as it goes.
///
/// `on_progress` is called with the frame `melt` has reached. `cancel` is
/// checked between reports; setting it kills the child and removes the partial
/// file, because a truncated MP4 left lying around looks like a finished one.
pub fn to_mp4(
    melt: &Path,
    project: &Path,
    out: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(i64),
) -> Result<()> {
    let mut cmd = melt_command(melt);
    cmd.args(melt_args(project, out));
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run melt at {}", melt.display()))?;
    let render_started = std::time::Instant::now();
    let render_pid = child.id();
    crate::tools::note_run_started(render_pid, "render");

    // stderr is drained on a thread of its own so that cancelling never waits
    // on melt to say something. Progress arrives on melt's own timer rather
    // than per frame, so a loop that only checked the cancel flag between
    // messages would leave the button dead for as long as melt stayed quiet.
    let (tx, rx) = std::sync::mpsc::channel::<Report>();
    let reader = child.stderr.take().map(|stderr| {
        std::thread::Builder::new()
            .name("roughcut-melt-stderr".into())
            .spawn(move || pump(BufReader::new(stderr), &tx))
            .expect("cannot spawn the melt reader thread")
    });

    let mut tail = String::new();
    let mut killed = false;
    let status = loop {
        while let Ok(report) = rx.try_recv() {
            match report {
                Report::Frame(f) => on_progress(f),
                Report::Line(l) => {
                    tail.push_str(&l);
                    tail.push('\n');
                    if tail.len() > 2000 {
                        tail = tail.split_off(tail.len() - 1000);
                    }
                }
            }
        }
        if cancel.load(Ordering::Relaxed) && !killed {
            let _ = child.kill();
            killed = true;
        }
        match child.try_wait().context("melt did not finish cleanly")? {
            Some(status) => break status,
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };

    crate::tools::note_run_finished(
        render_pid,
        "render",
        render_started.elapsed().as_secs_f64(),
        status.success() && !killed,
    );

    // The pipe is closed now, so the reader is finishing; collect the rest.
    if let Some(handle) = reader {
        let _ = handle.join();
    }
    while let Ok(report) = rx.try_recv() {
        match report {
            Report::Frame(f) => on_progress(f),
            Report::Line(l) => {
                tail.push_str(&l);
                tail.push('\n');
            }
        }
    }

    if killed || cancel.load(Ordering::Relaxed) {
        // A truncated MP4 left lying about looks exactly like a finished one.
        let _ = std::fs::remove_file(out);
        bail!("cancelled");
    }
    if !status.success() {
        let _ = std::fs::remove_file(out);
        bail!("melt failed ({status}): {}", tail.trim());
    }
    if !out.is_file() {
        bail!("melt reported success but wrote no file");
    }
    Ok(())
}

/// One thing the stderr reader saw.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Report {
    Frame(i64),
    Line(String),
}

/// Turn melt's stderr into a stream of reports.
///
/// Split out from the process handling so it can be tested against a recorded
/// transcript, without a render and without a machine fast enough to make one
/// worth watching.
///
/// melt rewrites its progress line with a carriage return rather than a
/// newline, so this reads by `\r` as well — reading by lines alone would
/// deliver the entire render as one message at the end.
pub(crate) fn pump<R: BufRead>(mut reader: R, tx: &std::sync::mpsc::Sender<Report>) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\r', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        for part in String::from_utf8_lossy(&buf).split(['\r', '\n']) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let report = match parse_progress(part) {
                Some(frame) => Report::Frame(frame),
                // Keep what is not progress: if melt fails, the reason is in
                // here and nowhere else.
                None => Report::Line(part.to_string()),
            };
            if tx.send(report).is_err() {
                return;
            }
        }
    }
}

/// Shotcut's bundled `melt` is a Qt5 binary sitting beside Qt6 plugins, and
/// picks the wrong ones unless pointed at its own.
fn melt_command(melt: &Path) -> Command {
    let mut cmd = quiet_command(melt);
    if let Some(dir) = melt.parent() {
        cmd.env("QT_PLUGIN_PATH", dir.join("lib/qt5"));
        cmd.current_dir(dir);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_understood() {
        assert_eq!(
            parse_progress("Current Frame:         42, percentage:         14"),
            Some(42)
        );
        assert_eq!(parse_progress("Current Frame: 0, percentage: 0"), Some(0));
        assert_eq!(parse_progress("Current Frame: 1234"), Some(1234));
        assert_eq!(parse_progress("+-----------------------------+"), None);
        assert_eq!(parse_progress(""), None);
    }

    /// A transcript recorded from a real render, not written from memory.
    ///
    /// That distinction cost a release. The previous version of this test used
    /// a transcript I composed to match the parser, so it proved the parser
    /// agreed with my assumption rather than with melt -- and melt says
    /// nothing at all in this format unless `-progress` is passed, which it
    /// was not. The bar sat at zero for the whole render.
    ///
    /// Captured with:
    ///
    ///     melt -progress -profile atsc_1080p_30 color:blue out=1800     ///       -consumer avformat:out.mp4 vcodec=libx264 real_time=-1
    #[test]
    fn a_recorded_melt_transcript_becomes_progress_and_nothing_else_is_lost() {
        let transcript = concat!(
            "[mp4 @ 000001f7d24917c0] Timestamps are unset in a packet for stream 1.
",
            "Current Frame:         34, percentage:          1
",
            "Current Frame:         44, percentage:          2
",
            "Current Frame:        102, percentage:          5
",
            "Current Frame:       1800, percentage:        100
",
            "Current Position:       1800
",
        );
        let (tx, rx) = std::sync::mpsc::channel();
        pump(std::io::Cursor::new(transcript), &tx);
        drop(tx);
        let got: Vec<Report> = rx.into_iter().collect();

        let frames: Vec<i64> = got
            .iter()
            .filter_map(|r| match r {
                Report::Frame(f) => Some(*f),
                Report::Line(_) => None,
            })
            .collect();
        assert_eq!(frames, vec![34, 44, 102, 1800]);

        // Everything that is not progress is kept, because a failing melt
        // explains itself there and nowhere else.
        let lines = got.iter().filter(|r| matches!(r, Report::Line(_))).count();
        assert_eq!(lines, 2, "the non-progress output was dropped: {got:?}");
    }

    #[test]
    fn a_silent_render_produces_no_reports_and_does_not_hang() {
        let (tx, rx) = std::sync::mpsc::channel();
        pump(std::io::Cursor::new(""), &tx);
        drop(tx);
        assert!(rx.into_iter().next().is_none());
    }

    #[test]
    fn the_command_names_the_output_and_never_re_encodes_in_real_time() {
        let args = melt_args(Path::new("/p/cut.mlt"), Path::new("/p/out.mp4"));
        let joined: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The project is the first thing that is not an option to melt itself.
        assert!(joined[1].ends_with("cut.mlt"), "{joined:?}");
        assert!(joined.iter().any(|a| a.contains("avformat:") && a.contains("out.mp4")));
        assert!(joined.iter().any(|a| a == "real_time=-1"));
        assert!(joined.iter().any(|a| a == "terminate_on_pause=1"));
        assert!(joined.iter().any(|a| a == "vcodec=libx264"));
    }

    /// The flag that decides whether the progress bar is a progress bar.
    /// Without it melt renders in silence and the dialog sits at zero from
    /// start to finish.
    #[test]
    fn melt_is_asked_to_report_progress() {
        let args = melt_args(Path::new("/p/cut.mlt"), Path::new("/p/out.mp4"));
        assert_eq!(
            args.first().map(|a| a.to_string_lossy().into_owned()),
            Some("-progress".to_string()),
            "-progress is a melt option and has to come before the project"
        );
    }
}
