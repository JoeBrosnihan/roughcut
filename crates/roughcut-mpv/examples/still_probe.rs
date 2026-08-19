//! Diagnostic: can mpv's EDL hold a still image alongside video?
//!
//!     cargo run -p roughcut-mpv --example still_probe -- clip.mp4 photo.png
//!
//! Timeline playback hands mpv the whole cut list as an EDL. Putting a photo
//! on the timeline therefore means putting an image file in that list, with a
//! length it was never measured to have. This checks whether mpv accepts that
//! at all before anything is built on it: the EDL's duration against the sum
//! of its parts, and whether playback runs through the still or stops dead at
//! it.

use roughcut_mpv::player::Event;
use roughcut_mpv::{MpvLib, Player};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn wait_loaded(player: &Player, secs: u64) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        while let Some(ev) = player.poll_event() {
            if matches!(ev, Event::FileLoaded) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn main() -> anyhow::Result<()> {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    anyhow::ensure!(!files.is_empty(), "usage: still_probe <image> | <video> <image>");
    let lib = Arc::new(MpvLib::load()?);

    // One argument: load it the way the source monitor does, on its own, and
    // report whether mpv produces a picture at all.
    if files.len() == 1 {
        return direct(&lib, &files[0]);
    }
    let (video, image) = (&files[0], &files[1]);

    // Three seconds of video, then ten seconds of photo, then three more of
    // video: a still in the middle of a cut, which is the case that has to
    // work.
    let parts = [
        (video.clone(), 0.0_f64, 3.0_f64),
        // Deliberately not starting at zero. A photo that has been trimmed
        // on the timeline has an in point, and the EDL quotes it.
        (
            image.clone(),
            std::env::var("STILL_START").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            10.0,
        ),
        (video.clone(), 3.0, 3.0),
    ];
    let expected: f64 = parts.iter().map(|p| p.2).sum();

    let edl_path = std::env::temp_dir().join("roughcut-still-probe.edl");
    {
        let mut f = std::fs::File::create(&edl_path)?;
        writeln!(f, "# mpv EDL v0")?;
        for (p, start, len) in &parts {
            let s = p.to_string_lossy();
            writeln!(f, "%{}%{},{start:.6},{len:.6}", s.len(), s)?;
        }
    }
    println!("{}", std::fs::read_to_string(&edl_path)?);

    let player = Player::new(lib)?;
    player.set_option("vo", "null")?;
    if !wait_loaded_after(&player, &edl_path) {
        println!("RESULT: the EDL never loaded — mpv will not take an image this way");
        return Ok(());
    }

    match player.get_f64("duration") {
        Some(d) => println!(
            "duration {d:.3} s against {expected:.3} s expected  -> {}",
            if (d - expected).abs() < 0.5 {
                "MATCHES"
            } else {
                "MISMATCH — the still is not holding its length"
            }
        ),
        None => println!("no duration reported"),
    }

    // Run it and see whether the position actually crosses the still.
    player.set_paused(false)?;
    let mut furthest = 0.0_f64;
    let mut stalled_at = None;
    let mut last_move = Instant::now();
    let mut trace: Vec<(f64, f64)> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(expected + 5.0) {
        while player.poll_event().is_some() {}
        if let Some(t) = player.get_f64("time-pos") {
            if t > furthest + 0.001 {
                if trace.last().is_none_or(|l| (t - l.1).abs() > 0.24) {
                    trace.push((start.elapsed().as_secs_f64(), t));
                }
                furthest = t;
                last_move = Instant::now();
            } else if last_move.elapsed() > Duration::from_secs(2) && stalled_at.is_none() {
                stalled_at = Some(furthest);
            }
        }
        if player.get_flag("eof-reached").unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    println!("played to {furthest:.3} s of {expected:.3} s");
    println!("wall -> time-pos, sampled:");
    for (wall, t) in &trace {
        println!("  {wall:6.2} s  ->  {t:6.3} s");
    }
    match stalled_at {
        Some(t) => println!("RESULT: playback stalled at {t:.3} s"),
        None if furthest > expected - 1.0 => {
            println!("RESULT: the still plays for its full length, in line with the video")
        }
        None => println!("RESULT: playback ended early, at {furthest:.3} s"),
    }
    Ok(())
}

/// Load one file exactly as the source monitor does, and report what mpv makes
/// of it: whether it loads, what duration it claims, and whether a frame is
/// actually decoded.
fn direct(lib: &Arc<MpvLib>, path: &std::path::Path) -> anyhow::Result<()> {
    let player = Player::new(lib.clone())?;
    player.set_option("vo", "null")?;
    if !wait_loaded_after(&player, path) {
        println!("RESULT: {} never loaded", path.display());
        return Ok(());
    }
    println!(
        "duration {:?}  size {:?}x{:?}",
        player.get_f64("duration"),
        player.get_f64("width"),
        player.get_f64("height")
    );
    // The monitor seeks to the playhead the moment the file is open.
    for target in [0.0_f64, 5.0, 29.97] {
        player.seek_exact(target)?;
        std::thread::sleep(Duration::from_millis(300));
        while player.poll_event().is_some() {}
        println!(
            "  after seek to {target:>6.2} s: time-pos {:?}  eof {:?}",
            player.get_f64("time-pos"),
            player.get_flag("eof-reached")
        );
    }
    Ok(())
}

fn wait_loaded_after(player: &Player, path: &std::path::Path) -> bool {
    if player.load_file(path).is_err() {
        return false;
    }
    wait_loaded(player, 10)
}
