//! Diagnostic: does mpv's EDL play a cut list without a gap at each join?
//!
//!     cargo run -p roughcut-mpv --example edl_probe -- a.mp4 b.mp4
//!
//! Timeline playback loads one clip at a time and reloads at every cut, which
//! costs a visible pause. mpv can be handed the whole cut list as an EDL
//! instead. This measures whether that is really seamless before anything is
//! built on it: it reports the EDL's duration against the sum of its parts,
//! and watches `time-pos` across a join for a stall.

use roughcut_mpv::player::Event;
use roughcut_mpv::{MpvLib, Player};
use std::io::Write;
use std::path::{Path, PathBuf};
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

fn duration_of(lib: &Arc<MpvLib>, path: &Path) -> anyhow::Result<f64> {
    let player = Player::new(lib.clone())?;
    player.set_option("vo", "null")?;
    player.load_file(path)?;
    anyhow::ensure!(wait_loaded(&player, 10), "{} never loaded", path.display());
    player
        .get_f64("duration")
        .ok_or_else(|| anyhow::anyhow!("no duration for {}", path.display()))
}

fn main() -> anyhow::Result<()> {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    anyhow::ensure!(files.len() >= 2, "usage: edl_probe <file> <file> [...]");

    let lib = Arc::new(MpvLib::load()?);

    // Take three seconds from the middle of each file, which is what a cut
    // list actually looks like.
    let mut parts = Vec::new();
    for f in &files {
        let d = duration_of(&lib, f)?;
        let start = (d / 3.0).min(d - 3.0).max(0.0);
        let len = 3.0_f64.min(d - start);
        println!("{}  duration {d:.3}  taking {start:.3}..{:.3}", f.display(), start + len);
        parts.push((f.clone(), start, len));
    }
    let expected: f64 = parts.iter().map(|p| p.2).sum();

    // mpv's EDL v0: one `path,start,length` per line. Paths holding a comma or
    // a percent sign need `%<bytes>%` quoting, which is why this is written
    // rather than concatenated blindly.
    let edl_path = std::env::temp_dir().join("roughcut-edl-probe.edl");
    {
        let mut f = std::fs::File::create(&edl_path)?;
        writeln!(f, "# mpv EDL v0")?;
        for (p, start, len) in &parts {
            let s = p.to_string_lossy();
            writeln!(f, "%{}%{},{start:.6},{len:.6}", s.len(), s)?;
        }
    }
    println!("\nEDL at {}", edl_path.display());
    println!("{}", std::fs::read_to_string(&edl_path)?);

    let player = Player::new(lib)?;
    player.set_option("vo", "null")?;
    player.load_file(&edl_path)?;
    anyhow::ensure!(wait_loaded(&player, 10), "the EDL never loaded");

    match player.get_f64("duration") {
        Some(d) => println!(
            "EDL duration {d:.3} s, sum of parts {expected:.3} s  -> {}",
            if (d - expected).abs() < 0.25 { "MATCHES" } else { "MISMATCH" }
        ),
        None => println!("EDL reported no duration"),
    }

    // Play it through and record the largest gap between consecutive
    // observations of time-pos. A reload at a join shows up as a long stall.
    player.set_paused(false)?;
    let mut last: Option<(Instant, f64)> = None;
    let mut worst = (0.0f64, 0.0f64);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(expected + 4.0) {
        while player.poll_event().is_some() {}
        if let Some(t) = player.get_f64("time-pos") {
            let now = Instant::now();
            if let Some((pt, ptime)) = last {
                let wall = now.duration_since(pt).as_secs_f64();
                // A stall: wall clock moved but playback did not.
                if wall > 0.02 && (t - ptime).abs() < 0.001 && wall > worst.0 {
                    worst = (wall, ptime);
                }
            }
            last = Some((now, t));
        }
        if player.get_flag("eof-reached").unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "longest stall {:.0} ms at t={:.2}s  (joins at {:?})",
        worst.0 * 1000.0,
        worst.1,
        parts
            .iter()
            .scan(0.0, |acc, p| {
                *acc += p.2;
                Some((*acc * 100.0).round() / 100.0)
            })
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_file(&edl_path);
    Ok(())
}
