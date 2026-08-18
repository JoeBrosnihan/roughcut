//! Diagnostic: does `render::to_mp4` actually see melt's progress?
//!
//!     cargo run -p roughcut-core --example render_probe -- in.mlt out.mp4

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let project: PathBuf = args.next().expect("usage: render_probe <mlt> <out>").into();
    let out: PathBuf = args.next().expect("usage: render_probe <mlt> <out>").into();
    let tools = roughcut_core::tools::Tools::discover();
    let melt = tools
        .melt
        .or_else(|| roughcut_core::tools::find_tool("melt"))
        .expect("melt not found");
    println!("melt: {}", melt.display());

    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let r = roughcut_core::render::to_mp4(&melt, &project, &out, &cancel, |f| seen.push(f));
    println!("result: {r:?}");
    println!("progress callbacks: {} -> {seen:?}", seen.len());
    Ok(())
}
