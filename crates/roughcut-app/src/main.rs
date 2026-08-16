//! Roughcut — a keyboard-driven assembly editor that exports MLT XML.

// A release build is a GUI app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod keys;
mod monitor;
mod settings;
mod theme;
mod ui;
mod video;
mod workers;

use app::RoughcutApp;

fn main() -> eframe::Result<()> {
    // The bin target is `roughcut`, so that — not the package name
    // `roughcut-app` — is the root of this crate's module paths, and is what
    // a `RUST_LOG` filter has to name.
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("roughcut=info,roughcut_mpv=info,roughcut_core=info"),
    );
    builder.format_timestamp_millis();
    // A release build has no console, so `ROUGHCUT_LOG_FILE` is the only way
    // to see the log — including the seek-latency summary written on exit.
    if let Some(path) = std::env::var_os("ROUGHCUT_LOG_FILE") {
        match std::fs::File::create(&path) {
            Ok(f) => {
                builder.target(env_logger::Target::Pipe(Box::new(f)));
            }
            Err(e) => eprintln!("cannot open {}: {e}", path.to_string_lossy()),
        }
    }
    builder.init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Roughcut")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 640.0])
            .with_drag_and_drop(true),
        // §4 binds the UI to the glow (OpenGL) backend, which is also what
        // mpv's render API integration needs.
        renderer: eframe::Renderer::Glow,
        // vsync keeps playback smooth without a busy loop; it costs nothing
        // when idle because no frames are being submitted at all.
        vsync: true,
        ..Default::default()
    };

    // Anything on the command line is opened at startup: a `.roughcut` file
    // as a project, anything else as media to import.
    let mut args: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();

    // Regenerates KEYS.md from the one authoritative key table.
    if args.iter().any(|a| a == std::path::Path::new("--dump-keys")) {
        print!("{}", actions::key_map_markdown());
        return Ok(());
    }
    args.retain(|a| !a.to_string_lossy().starts_with("--"));

    eframe::run_native(
        "Roughcut",
        options,
        Box::new(move |cc| {
            let mut app = RoughcutApp::new(cc);
            app.open_from_command_line(&args);
            Ok(Box::new(app))
        }),
    )
}
