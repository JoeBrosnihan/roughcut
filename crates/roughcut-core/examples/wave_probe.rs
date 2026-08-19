//! Time the audio envelope against real media.
//!
//!     cargo run -p roughcut-core --example wave_probe -- <file>...
//!
//! Prints how long each clip took and a coarse picture of what came back, so
//! the cost of the waveform can be checked against real footage rather than
//! against a generated tone.

use roughcut_core::tools::Tools;
use roughcut_core::waveform;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        eprintln!("usage: wave_probe <file>...");
        std::process::exit(2);
    }
    let Some(ffmpeg) = Tools::discover().ffmpeg else {
        eprintln!("ffmpeg not found");
        std::process::exit(1);
    };

    for file in files {
        let started = Instant::now();
        match waveform::extract(&ffmpeg, &file, waveform::BUCKETS) {
            Ok(peaks) => {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                let loud = peaks.iter().filter(|&&v| v > 64).count();
                let sparkline: String = peaks
                    .chunks(peaks.len() / 40)
                    .map(|c| {
                        let peak = c.iter().copied().max().unwrap_or(0);
                        " .:-=+*#%@".chars().nth((peak as usize * 9) / 255).unwrap()
                    })
                    .collect();
                println!(
                    "{:>8.0} ms  {:>4}/{} loud  {sparkline}  {}",
                    ms,
                    loud,
                    peaks.len(),
                    file.display()
                );
            }
            Err(e) => println!("     ---  {e:#}  {}", file.display()),
        }
    }
}
