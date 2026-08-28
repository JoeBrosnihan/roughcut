//! Diagnostic: can mpv preview a cut *and* a music bed at the same time?
//!
//!     cargo run -p roughcut-mpv --example music_probe -- clip.mp4 music.wav
//!
//! Timeline playback hands mpv the whole cut list as one EDL, and an EDL
//! concatenates — it does not mix. A music track therefore needs mpv to play
//! two audio sources at once, which is `lavfi-complex` and nothing else.
//! Whether that works decides how hard a music track is; the model and the MLT
//! are additive either way.
//!
//! Output goes to a WAV rather than a speaker, so the result can be measured.
//! Give the clip a 300 Hz tone and the music 1000 Hz, then look for both:
//!
//!     ffmpeg -i out.wav -af bandpass=f=1000:width_type=h:w=80,volumedetect \
//!       -f null -
//!
//! ## What this established
//!
//! **It works.** Mixed output measured 300 Hz at -23.1 dB and 1000 Hz at
//! -18.1 dB, against -43.9 and -35.9 for each source alone. Both are really
//! there.
//!
//! **`lavfi-complex` must be set after the file loads.** As a startup option
//! the file never loads at all, with nothing said about why. Set as a property
//! once `FileLoaded` has arrived, it is accepted.
//!
//! **The graph cannot say `[aid1] [aid2]` and hope.** Every iPhone clip
//! sampled here carries a second four-channel spatial audio track in
//! `apple_apac`, which mpv cannot decode: naming `aid2` picks that rather than
//! the music and the whole graph falls over with "Audio: no audio". The real
//! tracks have to be identified before the graph is built. Ordinary playback
//! is unaffected — mpv selects the usable AAC track on its own.

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
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    anyhow::ensure!(args.len() == 2, "usage: music_probe <clip> <music>");
    let (clip, music) = (&args[0], &args[1]);
    let out = std::env::temp_dir().join("roughcut-music-probe.wav");
    let _ = std::fs::remove_file(&out);

    // Two three-second pieces of the clip, which is what a cut list looks like.
    let edl_path = std::env::temp_dir().join("roughcut-music-probe.edl");
    {
        let s = clip.to_string_lossy();
        let mut f = std::fs::File::create(&edl_path)?;
        writeln!(f, "# mpv EDL v0")?;
        writeln!(f, "%{}%{},0.000000,3.000000", s.len(), s)?;
        writeln!(f, "%{}%{},10.000000,3.000000", s.len(), s)?;
    }

    // Three attempts, least ambitious first, so a failure says which layer
    // could not do it rather than just "no".
    let modes: [(&str, bool, bool); 2] = [
        ("EDL + external audio, lavfi-complex set BEFORE load", true, true),
        ("EDL + external audio, lavfi-complex set AFTER load", true, false),
    ];
    let lib = Arc::new(MpvLib::load()?);
    for (name, add_music, mix) in modes {
        println!("\n=== {name}");
        match attempt(&lib, &edl_path, music, &out, add_music, mix) {
            Ok(()) => {}
            Err(e) => println!("RESULT: {e:#}"),
        }
    }
    Ok(())
}

fn attempt(
    lib: &Arc<MpvLib>,
    edl_path: &std::path::Path,
    music: &std::path::Path,
    out: &std::path::Path,
    add_music: bool,
    mix: bool,
) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(out);
    let player = Player::new(lib.clone())?;
    player.request_log_messages("info")?;
    player.set_option("vo", "null")?;
    // Write the mix to a file so it can be measured rather than heard.
    player.set_option("ao", "pcm")?;
    player.set_option("ao-pcm-file", &out.to_string_lossy())?;
    player.set_option("ao-pcm-waveheader", "yes")?;
    // Render as fast as it will go; this is not a listening test.
    player.set_option("audio-wait-open", "0")?;

    // The music is an external audio track laid over the cut.
    if add_music {
        player.set_option("audio-files", &music.to_string_lossy())?;
    }
    // Play both at once. Without this mpv selects one audio track and the
    // other is simply not heard.
    if mix {
        player.set_option("lavfi-complex", "[aid1] [aid2] amix=inputs=2 [ao]")?;
    }

    player.load_file(edl_path)?;
    if !wait_loaded(&player, 10) {
        drain_log(&player);
        anyhow::bail!("never loaded");
    }
    if !mix {
        match player.set_property_string("lavfi-complex", "[aid1] [aid2] amix=inputs=2 [ao]") {
            Ok(()) => println!("   lavfi-complex accepted after load"),
            Err(e) => println!("   lavfi-complex refused after load: {e}"),
        }
    }
    println!(
        "loaded. duration {:?}  tracks {:?}",
        player.get_f64("duration"),
        player.get_i64("track-list/count")
    );

    player.set_paused(false)?;
    let start = Instant::now();
    let mut furthest = 0.0_f64;
    let mut sought = false;
    while start.elapsed() < Duration::from_secs(20) {
        while let Some(ev) = player.poll_event() {
            if let Event::LogMessage { level, text } = ev {
                let t = text.trim();
                if !t.is_empty() {
                    println!("   mpv[{level}] {t}");
                }
            }
        }
        if let Some(t) = player.get_f64("time-pos") {
            furthest = furthest.max(t);
            // Mid-way, seek: a music bed that loses sync on a seek is no use.
            if !sought && t > 1.5 {
                sought = true;
                player.seek_exact(4.0)?;
            }
        }
        if player.get_flag("eof-reached").unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("played to {furthest:.2} s, seek issued: {sought}");
    drain_log(&player);
    drop(player);

    std::thread::sleep(Duration::from_millis(300));
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 1000 => println!("wrote {} ({} bytes)", out.display(), m.len()),
        Ok(m) => println!("RESULT: output is empty ({} bytes)", m.len()),
        Err(e) => println!("RESULT: no output file: {e}"),
    }
    Ok(())
}

/// Whatever mpv complained about, which is the only place the real reason for
/// a refusal appears.
fn drain_log(player: &Player) {
    let mut shown = 0;
    while let Some(ev) = player.poll_event() {
        if let Event::LogMessage { level, text } = ev {
            if shown < 6 {
                println!("   mpv[{level}] {}", text.trim());
                shown += 1;
            }
        }
    }
}
