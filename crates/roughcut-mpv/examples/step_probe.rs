//! Diagnostic: which seek-target formula makes mpv land on exactly the frame
//! we asked for, and how do the frame-step commands behave?
//!
//!     cargo run -p roughcut-mpv --example step_probe -- path/to/clip.mp4
//!
//! Everything in Roughcut's timing model depends on the answer, so it is
//! measured rather than assumed. `vo=null` keeps this free of any GL context.

use roughcut_mpv::player::Event;
use roughcut_mpv::{MpvLib, Player};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
enum Strategy {
    /// The frame's own presentation timestamp.
    ExactPts,
    /// Half a frame after it — the middle of the frame's display interval.
    Midpoint,
    /// A fifth of a frame before it.
    Undershoot,
}

impl Strategy {
    fn target(self, frame: i64, period: f64) -> f64 {
        let pts = frame as f64 * period;
        match self {
            Self::ExactPts => pts,
            Self::Midpoint => pts + 0.5 * period,
            Self::Undershoot => (pts - 0.2 * period).max(0.0),
        }
    }
}

/// Wait for mpv to finish a seek, then read where it actually is.
fn settle(player: &Player, period: f64) -> Option<i64> {
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut restarted = false;
    while Instant::now() < deadline {
        while let Some(ev) = player.poll_event() {
            if matches!(ev, Event::PlaybackRestart) {
                restarted = true;
            }
        }
        if restarted {
            // Give the property a moment to catch up, then read it directly
            // rather than trusting a possibly stale event.
            std::thread::sleep(Duration::from_millis(30));
            let t = player.get_f64("time-pos")?;
            return Some(((t / period) + 1e-6).floor() as i64);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

fn main() -> anyhow::Result<()> {
    let path: PathBuf = std::env::args_os()
        .nth(1)
        .map(Into::into)
        .expect("usage: step_probe <video file>");

    let lib = Arc::new(MpvLib::load()?);
    let player = Player::new(lib)?;
    player.set_option("vo", "null")?;
    player.load_file(&path)?;

    let start = Instant::now();
    let mut loaded = false;
    while start.elapsed() < Duration::from_secs(10) && !loaded {
        while let Some(ev) = player.poll_event() {
            loaded |= matches!(ev, Event::FileLoaded);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    anyhow::ensure!(loaded, "file never loaded");

    let fps = player.get_f64("container-fps").unwrap_or(30000.0 / 1001.0);
    let period = 1.0 / fps;
    let total = player
        .get_f64("duration")
        .map(|d| (d * fps).round() as i64)
        .unwrap_or(300);
    println!("fps {fps:.6}  frames {total}\n");

    let targets: Vec<i64> = vec![0, 1, 2, 3, 5, 10, 37, 99, 100, 101, 150, 200, 249, 250, 251, 300]
        .into_iter()
        .filter(|f| *f < total)
        .collect();

    for strategy in [Strategy::ExactPts, Strategy::Midpoint, Strategy::Undershoot] {
        let mut exact = 0usize;
        let mut deltas: Vec<(i64, i64)> = Vec::new();
        for &want in &targets {
            // Force a genuine seek by going somewhere far away first.
            let away = if want > total / 2 { 0 } else { total - 1 };
            player.seek_exact(Strategy::Midpoint.target(away, period))?;
            let _ = settle(&player, period);

            player.seek_exact(strategy.target(want, period))?;
            match settle(&player, period) {
                Some(got) if got == want => exact += 1,
                Some(got) => deltas.push((want, got - want)),
                None => deltas.push((want, i64::MIN)),
            }
        }
        println!(
            "{strategy:>12?}: {exact}/{} exact{}",
            targets.len(),
            if deltas.is_empty() {
                String::new()
            } else {
                format!(
                    "   misses: {}",
                    deltas
                        .iter()
                        .map(|(w, d)| if *d == i64::MIN {
                            format!("{w}:timeout")
                        } else {
                            format!("{w}:{d:+}")
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        );
    }

    // Frame stepping, measured from a known-good position.
    println!("\nframe stepping:");
    player.seek_exact(Strategy::Midpoint.target(100, period))?;
    let mut at = settle(&player, period).unwrap_or(-1);
    println!("  start at {at}");
    for i in 1..=6 {
        let t0 = Instant::now();
        player.frame_step()?;
        std::thread::sleep(Duration::from_millis(60));
        let now = player
            .get_f64("time-pos")
            .map(|t| ((t / period) + 1e-6).floor() as i64)
            .unwrap_or(-1);
        println!(
            "  frame-step  #{i}: {at} -> {now}  ({:+})  [{:.0} ms incl. 60 ms settle]",
            now - at,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        at = now;
    }
    for i in 1..=3 {
        let t0 = Instant::now();
        player.frame_back_step()?;
        std::thread::sleep(Duration::from_millis(120));
        let now = player
            .get_f64("time-pos")
            .map(|t| ((t / period) + 1e-6).floor() as i64)
            .unwrap_or(-1);
        println!(
            "  frame-back  #{i}: {at} -> {now}  ({:+})  [{:.0} ms incl. 120 ms settle]",
            now - at,
            t0.elapsed().as_secs_f64() * 1000.0
        );
        at = now;
    }

    Ok(())
}
