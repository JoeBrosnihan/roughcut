//! Print what Roughcut makes of one file.
//!
//! Exists so the timing model can be checked against real footage rather than
//! only against synthetic clips — variable-rate phone video in particular is
//! not something `ffmpeg -f lavfi` can produce.
//!
//! ```text
//! cargo run -p roughcut-core --example probe_one -- "F:/clips/IMG_3810.MOV"
//! ```

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os().nth(1).expect("usage: probe_one <file>");
    let path = std::path::PathBuf::from(path);
    let tools = roughcut_core::tools::Tools::discover();
    let ffprobe = tools.ffprobe.expect("ffprobe was not found");
    let info = roughcut_core::probe::probe(&ffprobe, &path)?;
    let seconds = info.native_frames as f64 / info.fps.as_f64();
    println!(
        "{}\n  {}x{}  rotation {}  {} fps\n  {} frames = {:.3} s   variable_rate: {}",
        path.display(),
        info.width,
        info.height,
        info.rotation,
        info.fps,
        info.native_frames,
        seconds,
        info.variable_rate
    );
    Ok(())
}
