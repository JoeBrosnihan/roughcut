//! Time transcription against real media, and show what comes back.
//!
//!     cargo run --release -p roughcut-core --example whisper_probe -- <file>...

use roughcut_core::tools::Tools;
use roughcut_core::whisper;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let tools = Tools::discover();
    let (Some(ffmpeg), Some(w)) = (tools.ffmpeg.clone(), tools.whisper.clone()) else {
        eprintln!("need ffmpeg and whisper-cli");
        std::process::exit(1);
    };
    let Some(model) = whisper::find_model(&w) else {
        eprintln!("no model beside {}", w.display());
        std::process::exit(1);
    };
    println!("whisper: {}\nmodel:   {}\n", w.display(), model.display());
    let scratch = std::env::temp_dir().join("roughcut-whisper");

    for f in files {
        let started = Instant::now();
        match whisper::transcribe(&ffmpeg, &w, &model, &f, &scratch) {
            Ok(t) => {
                let secs = started.elapsed().as_secs_f64();
                println!(
                    "{:>7.1} s   {:>4} words in {:>3} sentences   {}",
                    secs,
                    t.word_count(),
                    t.segments.len(),
                    f.display()
                );
                for s in t.segments.iter().take(3) {
                    println!("      [{:>6} ms] {}", s.start_ms(), s.text());
                }
            }
            Err(e) => println!("   ---   {e:#}   {}", f.display()),
        }
    }
}
