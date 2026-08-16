//! MLT XML export — the single most important output of this program.
//!
//! The structure here was derived from a project saved by Shotcut itself
//! (see `reference/` and the notes in README.md), not from memory:
//!
//! * root `<mlt LC_NUMERIC="C" ... producer="main_bin">`
//! * one `<chain mlt_service="avformat-novalidate">` per distinct source file,
//!   reused by every `<entry>` that cites it
//! * a `main_bin` playlist carrying `xml_retain`, so the bin survives the trip
//! * a `black` colour producer plus a `background` playlist
//! * the video track as `playlist0` with `shotcut:video` / `shotcut:name`
//! * a `<tractor>` holding `background` at track 0 and `playlist0` at track 1,
//!   with the `mix` and `frei0r.cairoblend` transitions Shotcut expects
//!
//! Positions are integer frame numbers. `in`/`out` are inclusive, matching
//! MLT, so a producer of N frames has `length="N"` and `out="N-1"`.

use crate::model::Project;
use crate::time::format_clock;
use crate::timeline;
use anyhow::{Context, Result};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;
use std::io::Cursor;
use std::path::Path;

/// The MLT release this writer targets. Emitted in the root `version`
/// attribute the way Shotcut emits the MLT it was built against.
pub const MLT_VERSION: &str = "7.33.0";

/// How to spell positions in the XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeFormat {
    /// Integer frame numbers — the default, and unambiguous.
    #[default]
    Frames,
    /// `HH:MM:SS.mmm`, the format Shotcut itself writes.
    ///
    /// Kept only until §12's manual check — open `acceptance/` in the Shotcut
    /// GUI — confirms it accepts integer positions. `melt` already does. Once
    /// that check passes, delete this variant, `time::format_clock`, the
    /// `fmt` closure in `to_xml` and `ExportOptions` along with them.
    Clock,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub title: String,
    pub time_format: TimeFormat,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            title: "Roughcut".to_string(),
            time_format: TimeFormat::default(),
        }
    }
}

/// Serialise `project` to MLT XML.
pub fn to_xml(project: &Project, opts: &ExportOptions) -> Result<String> {
    let fps = project.fps();
    let fmt = |frame: i64| -> String {
        match opts.time_format {
            TimeFormat::Frames => frame.to_string(),
            TimeFormat::Clock => format_clock(frame, fps),
        }
    };

    // Which sources need a chain, and under what id. Order is bin order so the
    // output is stable and diffable. The whole bin is emitted, not just the
    // clips cut into the timeline, so the bin survives into Shotcut.
    let chain_ids: Vec<(crate::model::ClipId, String)> = project
        .clips
        .iter()
        .enumerate()
        .map(|(i, clip)| (clip.id, format!("chain{i}")))
        .collect();
    let chain_id_of = |id: crate::model::ClipId| -> Option<&str> {
        chain_ids
            .iter()
            .find(|(cid, _)| *cid == id)
            .map(|(_, s)| s.as_str())
    };

    let total = timeline::total_frames(&project.timeline);
    // A zero-length timeline still needs a legal background: one frame.
    let track_len = total.max(1);
    let track_out = track_len - 1;

    let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;

    let mut root = BytesStart::new("mlt");
    root.push_attribute(("LC_NUMERIC", "C"));
    root.push_attribute(("version", MLT_VERSION));
    root.push_attribute(("title", opts.title.as_str()));
    root.push_attribute(("producer", "main_bin"));
    w.write_event(Event::Start(root))?;

    write_profile(&mut w, project)?;

    // --- one chain per distinct source file ---------------------------------
    for (clip_id, chain_id) in &chain_ids {
        let clip = project
            .clip(*clip_id)
            .context("bin clip vanished during export")?;
        let len = clip.duration_frames.max(1);

        let mut chain = BytesStart::new("chain");
        chain.push_attribute(("id", chain_id.as_str()));
        chain.push_attribute(("out", fmt(len - 1).as_str()));
        w.write_event(Event::Start(chain))?;

        prop(&mut w, "length", &fmt(len))?;
        prop(&mut w, "eof", "pause")?;
        prop(&mut w, "resource", &absolute_resource(&clip.path))?;
        prop(&mut w, "mlt_service", "avformat-novalidate")?;
        prop(&mut w, "seekable", "1")?;
        prop(&mut w, "audio_index", &clip.audio_index.to_string())?;
        prop(&mut w, "video_index", &clip.video_index.to_string())?;
        prop(&mut w, "mute_on_pause", "0")?;
        prop(&mut w, "shotcut:caption", &clip.file_name())?;

        w.write_event(Event::End(BytesEnd::new("chain")))?;
    }

    // --- the bin ------------------------------------------------------------
    let mut bin = BytesStart::new("playlist");
    bin.push_attribute(("id", "main_bin"));
    bin.push_attribute(("title", opts.title.as_str()));
    w.write_event(Event::Start(bin))?;
    prop(&mut w, "shotcut:projectAudioChannels", "2")?;
    prop(&mut w, "shotcut:projectFolder", "0")?;
    prop(&mut w, "xml_retain", "1")?;
    for (clip_id, chain_id) in &chain_ids {
        let clip = project
            .clip(*clip_id)
            .context("bin clip vanished during export")?;
        let len = clip.duration_frames.max(1);
        entry(&mut w, chain_id, &fmt(0), &fmt(len - 1))?;
    }
    w.write_event(Event::End(BytesEnd::new("playlist")))?;

    // --- background ---------------------------------------------------------
    let mut black = BytesStart::new("producer");
    black.push_attribute(("id", "black"));
    black.push_attribute(("in", fmt(0).as_str()));
    black.push_attribute(("out", fmt(track_out).as_str()));
    w.write_event(Event::Start(black))?;
    prop(&mut w, "length", &fmt(track_len))?;
    prop(&mut w, "eof", "pause")?;
    prop(&mut w, "resource", "0")?;
    prop(&mut w, "aspect_ratio", "1")?;
    prop(&mut w, "mlt_service", "color")?;
    prop(&mut w, "mlt_image_format", "rgba")?;
    prop(&mut w, "set.test_audio", "0")?;
    w.write_event(Event::End(BytesEnd::new("producer")))?;

    let mut bg = BytesStart::new("playlist");
    bg.push_attribute(("id", "background"));
    w.write_event(Event::Start(bg))?;
    entry(&mut w, "black", &fmt(0), &fmt(track_out))?;
    w.write_event(Event::End(BytesEnd::new("playlist")))?;

    // --- the one video track ------------------------------------------------
    let mut track = BytesStart::new("playlist");
    track.push_attribute(("id", "playlist0"));
    w.write_event(Event::Start(track))?;
    prop(&mut w, "shotcut:video", "1")?;
    prop(&mut w, "shotcut:name", "V1")?;
    for item in &project.timeline {
        let Some(chain_id) = chain_id_of(item.clip_id) else {
            anyhow::bail!("timeline references a clip that is not in the bin");
        };
        entry(
            &mut w,
            chain_id,
            &fmt(item.in_frame),
            &fmt(item.out_frame),
        )?;
    }
    w.write_event(Event::End(BytesEnd::new("playlist")))?;

    // --- tractor ------------------------------------------------------------
    let mut tractor = BytesStart::new("tractor");
    tractor.push_attribute(("id", "tractor0"));
    tractor.push_attribute(("title", opts.title.as_str()));
    tractor.push_attribute(("in", fmt(0).as_str()));
    tractor.push_attribute(("out", fmt(track_out).as_str()));
    w.write_event(Event::Start(tractor))?;
    prop(&mut w, "shotcut", "1")?;
    prop(&mut w, "shotcut:projectAudioChannels", "2")?;
    prop(&mut w, "shotcut:projectFolder", "0")?;

    let mut t0 = BytesStart::new("track");
    t0.push_attribute(("producer", "background"));
    w.write_event(Event::Empty(t0))?;
    let mut t1 = BytesStart::new("track");
    t1.push_attribute(("producer", "playlist0"));
    w.write_event(Event::Empty(t1))?;

    // Shotcut writes these two for every video track above the background.
    // Without them the track is silent and composites wrongly on reopen.
    transition(
        &mut w,
        "transition0",
        &[
            ("a_track", "0"),
            ("b_track", "1"),
            ("mlt_service", "mix"),
            ("always_active", "1"),
            ("sum", "1"),
        ],
    )?;
    transition(
        &mut w,
        "transition1",
        &[
            ("a_track", "0"),
            ("b_track", "1"),
            ("version", "0.1"),
            ("mlt_service", "frei0r.cairoblend"),
            ("threads", "0"),
            ("disable", "1"),
        ],
    )?;

    w.write_event(Event::End(BytesEnd::new("tractor")))?;
    w.write_event(Event::End(BytesEnd::new("mlt")))?;

    let bytes = w.into_inner().into_inner();
    let mut out = String::from_utf8(bytes).context("MLT writer produced invalid UTF-8")?;
    out.push('\n');
    Ok(out)
}

/// Write the XML to `path` via a temp file and an atomic rename.
pub fn write_to_file(project: &Project, opts: &ExportOptions, path: &Path) -> Result<()> {
    let xml = to_xml(project, opts)?;
    crate::project_io::atomic_write(path, xml.as_bytes())
}

fn write_profile<W: std::io::Write>(w: &mut Writer<W>, project: &Project) -> Result<()> {
    let p = &project.profile;
    let (dan, dad) = p.display_aspect();
    let mut e = BytesStart::new("profile");
    e.push_attribute(("description", "roughcut"));
    e.push_attribute(("width", p.width.to_string().as_str()));
    e.push_attribute(("height", p.height.to_string().as_str()));
    e.push_attribute(("progressive", if p.progressive { "1" } else { "0" }));
    e.push_attribute(("sample_aspect_num", p.sample_aspect_num.to_string().as_str()));
    e.push_attribute(("sample_aspect_den", p.sample_aspect_den.to_string().as_str()));
    e.push_attribute(("display_aspect_num", dan.to_string().as_str()));
    e.push_attribute(("display_aspect_den", dad.to_string().as_str()));
    e.push_attribute(("frame_rate_num", p.frame_rate_num.to_string().as_str()));
    e.push_attribute(("frame_rate_den", p.frame_rate_den.to_string().as_str()));
    e.push_attribute(("colorspace", p.colorspace.to_string().as_str()));
    w.write_event(Event::Empty(e))?;
    Ok(())
}

fn prop<W: std::io::Write>(w: &mut Writer<W>, name: &str, value: &str) -> Result<()> {
    let mut e = BytesStart::new("property");
    e.push_attribute(("name", name));
    w.write_event(Event::Start(e))?;
    w.write_event(Event::Text(BytesText::new(value)))?;
    w.write_event(Event::End(BytesEnd::new("property")))?;
    Ok(())
}

fn entry<W: std::io::Write>(
    w: &mut Writer<W>,
    producer: &str,
    in_pos: &str,
    out_pos: &str,
) -> Result<()> {
    let mut e = BytesStart::new("entry");
    e.push_attribute(("producer", producer));
    e.push_attribute(("in", in_pos));
    e.push_attribute(("out", out_pos));
    w.write_event(Event::Empty(e))?;
    Ok(())
}

fn transition<W: std::io::Write>(
    w: &mut Writer<W>,
    id: &str,
    props: &[(&str, &str)],
) -> Result<()> {
    let mut e = BytesStart::new("transition");
    e.push_attribute(("id", id));
    w.write_event(Event::Start(e))?;
    for (k, v) in props {
        prop(w, k, v)?;
    }
    w.write_event(Event::End(BytesEnd::new("transition")))?;
    Ok(())
}

/// MLT wants an absolute path. Anything relative that reaches here is a bug
/// upstream, but canonicalising defensively is cheap.
fn absolute_resource(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    // `canonicalize` on Windows produces a `\\?\` UNC prefix that MLT's
    // avformat cannot open, so it is deliberately not used here.
    abs.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipId, Profile, SourceClip, TimelineItem};
    use std::path::PathBuf;

    fn project() -> Project {
        let a = ClipId::new();
        let b = ClipId::new();
        Project {
            version: 1,
            profile: Profile::default(),
            clips: vec![
                SourceClip {
                    id: a,
                    path: PathBuf::from(if cfg!(windows) {
                        r"C:\media\a.mp4"
                    } else {
                        "/media/a.mp4"
                    }),
                    proxy_path: Some(PathBuf::from("/proxies/a.mp4")),
                    duration_frames: 300,
                    native_fps_num: 30000,
                    native_fps_den: 1001,
                    has_audio: true,
                    video_index: 0,
                    audio_index: 1,
                    mark_in: Some(100),
                    mark_out: Some(199),
                    rate_mismatch: false,
                },
                SourceClip {
                    id: b,
                    path: PathBuf::from(if cfg!(windows) {
                        r"C:\media\b & c.mp4"
                    } else {
                        "/media/b & c.mp4"
                    }),
                    proxy_path: None,
                    duration_frames: 350,
                    native_fps_num: 30000,
                    native_fps_den: 1001,
                    has_audio: false,
                    video_index: 0,
                    audio_index: -1,
                    mark_in: None,
                    mark_out: None,
                    rate_mismatch: false,
                },
            ],
            timeline: vec![
                TimelineItem {
                    clip_id: a,
                    in_frame: 100,
                    out_frame: 199,
                },
                TimelineItem {
                    clip_id: b,
                    in_frame: 0,
                    out_frame: 349,
                },
                // Same source again — must not produce a second chain.
                TimelineItem {
                    clip_id: a,
                    in_frame: 0,
                    out_frame: 9,
                },
            ],
            profile_locked: true,
        }
    }

    #[test]
    fn emits_integer_frame_positions() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        assert!(xml.contains(r#"in="100" out="199""#), "{xml}");
        assert!(xml.contains(r#"in="0" out="349""#));
    }

    #[test]
    fn one_chain_per_distinct_source() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        assert_eq!(xml.matches("<chain ").count(), 2, "{xml}");
        assert_eq!(xml.matches(r#"mlt_service">avformat-novalidate"#).count(), 2);
    }

    #[test]
    fn track_has_one_entry_per_timeline_item() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        let track = xml
            .split(r#"<playlist id="playlist0">"#)
            .nth(1)
            .unwrap()
            .split("</playlist>")
            .next()
            .unwrap();
        assert_eq!(track.matches("<entry ").count(), 3, "{track}");
    }

    #[test]
    fn total_length_is_the_sum_of_inclusive_durations() {
        let p = project();
        // 100 + 350 + 10
        assert_eq!(timeline::total_frames(&p.timeline), 460);
        let xml = to_xml(&p, &ExportOptions::default()).unwrap();
        assert!(xml.contains(r#"<tractor id="tractor0" title="Roughcut" in="0" out="459">"#), "{xml}");
        assert!(xml.contains(r#"<property name="length">460</property>"#));
    }

    #[test]
    fn producer_length_is_out_plus_one() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        // 300-frame source: out="299", length 300.
        assert!(xml.contains(r#"<chain id="chain0" out="299">"#), "{xml}");
        assert!(xml.contains(r#"<property name="length">300</property>"#));
    }

    #[test]
    fn structure_matches_the_shotcut_reference() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        for needle in [
            r#"<mlt LC_NUMERIC="C""#,
            r#"producer="main_bin""#,
            r#"<playlist id="main_bin""#,
            r#"<property name="xml_retain">1</property>"#,
            r#"<producer id="black""#,
            r#"<property name="mlt_service">color</property>"#,
            r#"<playlist id="background">"#,
            r#"<property name="shotcut:video">1</property>"#,
            r#"<property name="shotcut:name">V1</property>"#,
            r#"<track producer="background"/>"#,
            r#"<track producer="playlist0"/>"#,
            r#"<property name="mlt_service">mix</property>"#,
            r#"<property name="mlt_service">frei0r.cairoblend</property>"#,
        ] {
            assert!(xml.contains(needle), "missing {needle}\n{xml}");
        }
    }

    #[test]
    fn export_references_originals_never_proxies() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        assert!(!xml.contains("/proxies/"), "proxy leaked into the export:\n{xml}");
        assert!(xml.contains("a.mp4"));
    }

    #[test]
    fn ampersands_in_filenames_are_escaped() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        assert!(xml.contains("b &amp; c.mp4"), "{xml}");
        assert!(!xml.contains("b & c.mp4"));
    }

    #[test]
    fn profile_carries_the_exact_rational_rate() {
        let xml = to_xml(&project(), &ExportOptions::default()).unwrap();
        assert!(xml.contains(r#"frame_rate_num="30000""#));
        assert!(xml.contains(r#"frame_rate_den="1001""#));
        assert!(!xml.contains("29.97"));
        assert!(xml.contains(r#"display_aspect_num="16" display_aspect_den="9""#));
    }

    #[test]
    fn clock_format_is_available_as_a_fallback() {
        let opts = ExportOptions {
            time_format: TimeFormat::Clock,
            ..Default::default()
        };
        let xml = to_xml(&project(), &opts).unwrap();
        // Frame 100 at 30000/1001 is 3.337 s; frame 199 is 6.640 s.
        assert!(
            xml.contains(r#"in="00:00:03.337" out="00:00:06.640""#),
            "{xml}"
        );
    }

    #[test]
    fn an_empty_timeline_still_produces_a_legal_file() {
        let mut p = project();
        p.timeline.clear();
        let xml = to_xml(&p, &ExportOptions::default()).unwrap();
        assert!(xml.contains(r#"<tractor id="tractor0" title="Roughcut" in="0" out="0">"#), "{xml}");
        assert!(xml.contains(r#"<playlist id="playlist0">"#));
    }

    #[test]
    fn the_whole_bin_is_exported_even_when_unused() {
        let mut p = project();
        // Drop everything but the first clip from the timeline; the second
        // clip is still in the bin and must still reach main_bin.
        p.timeline.retain(|t| t.clip_id == p.clips[0].id);
        let xml = to_xml(&p, &ExportOptions::default()).unwrap();
        assert_eq!(xml.matches("<chain ").count(), 2, "{xml}");
    }
}
