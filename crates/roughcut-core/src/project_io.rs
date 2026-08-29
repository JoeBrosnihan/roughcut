//! `.roughcut` project files: pretty-printed JSON, absolute paths, atomic
//! writes. This is Roughcut's own format and has nothing to do with MLT.

use crate::model::{Project, SCHEMA_VERSION};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROJECT_EXTENSION: &str = "roughcut";

/// Write `bytes` to `path` by way of a sibling temp file and a rename, so a
/// crash mid-write cannot destroy the previous good file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
    }
    let tmp = temp_sibling(path);
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("cannot create {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        f.sync_all().ok();
    }
    // Windows `rename` fails if the destination exists, so clear it first.
    // The window between the two is why the temp file is kept on failure.
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e).with_context(|| format!("cannot move into place: {}", path.display()))
        }
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp{}", std::process::id()));
    path.with_file_name(name)
}

pub fn save(project: &Project, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(project).context("cannot serialise project")?;
    atomic_write(path, format!("{json}\n").as_bytes())
}

pub fn load(path: &Path) -> Result<Project> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut project: Project = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid Roughcut project", path.display()))?;
    if project.version > SCHEMA_VERSION {
        bail!(
            "{} was written by a newer Roughcut (schema {} > {})",
            path.display(),
            project.version,
            SCHEMA_VERSION
        );
    }
    // Projects written before clips remembered their own native length: recover
    // it from the profile-time duration, which is the same number whenever the
    // rates match and an exact conversion of it when they do not.
    for clip in &mut project.clips {
        if clip.native_frames <= 0 {
            clip.native_frames =
                crate::time::convert_frames(clip.duration_frames, project.profile.fps(), clip.native_fps())
                    .max(1);
        }
    }
    Ok(project)
}

// ---------------------------------------------------------------------------
// Autosave
//
// Written after every mutation rather than on a timer: a timer would have to
// wake the event loop while the user is doing nothing, which is exactly what
// §3's zero-idle-CPU rule forbids. Nothing changes while idle, so there is
// never anything a timer would catch that an edit hook does not.
//
// One slot, not a directory of timestamped candidates. Roughcut edits one
// project in one window, so a second slot could only ever be a decision to
// put in front of the user.
// ---------------------------------------------------------------------------

/// A recovery snapshot: the project plus enough context to put the user back
/// where they were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoSave {
    pub version: u32,
    /// Where this project would be saved, if it has ever been saved at all.
    #[serde(default)]
    pub project_path: Option<PathBuf>,
    /// Seconds since the Unix epoch, so the prompt can say how old this is.
    pub saved_at: u64,
    /// Whether this snapshot holds work that was never written to a project
    /// file. Only a dirty snapshot is worth interrupting startup for — but a
    /// clean one is still kept, so "recover last session" can always reach it.
    #[serde(default = "yes")]
    pub unsaved: bool,
    pub project: Project,
}

fn yes() -> bool {
    true
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_autosave(
    path: &Path,
    project: &Project,
    project_path: Option<&Path>,
    unsaved: bool,
) -> Result<()> {
    let snapshot = AutoSave {
        version: SCHEMA_VERSION,
        project_path: project_path.map(Path::to_path_buf),
        saved_at: now_unix(),
        unsaved,
        project: project.clone(),
    };
    let json = serde_json::to_string(&snapshot).context("cannot serialise the autosave")?;
    atomic_write(path, json.as_bytes())
}

/// Read a recovery snapshot. A snapshot that cannot be parsed is not an error
/// worth interrupting startup for — the caller logs it and moves on.
pub fn read_autosave(path: &Path) -> Result<AutoSave> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let snapshot: AutoSave =
        serde_json::from_str(&text).context("the autosave is not readable")?;
    if snapshot.version > SCHEMA_VERSION {
        bail!("the autosave was written by a newer Roughcut");
    }
    Ok(snapshot)
}

/// Mark the snapshot as no longer holding unsaved work, without destroying it.
///
/// Nothing in Roughcut deletes a snapshot. Deleting one is how work gets lost
/// for good — a file that is only ever overwritten can always be reached
/// again, even after the recovery prompt has been dismissed.
pub fn mark_autosave_saved(path: &Path) {
    let Ok(mut snapshot) = read_autosave(path) else {
        return;
    };
    if !snapshot.unsaved {
        return;
    }
    snapshot.unsaved = false;
    match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if let Err(e) = atomic_write(path, json.as_bytes()) {
                log::warn!("cannot update {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("cannot update {}: {e}", path.display()),
    }
}

/// How old a snapshot is, in seconds. Saturates at zero for clocks that have
/// gone backwards.
pub fn autosave_age_secs(snapshot: &AutoSave) -> u64 {
    now_unix().saturating_sub(snapshot.saved_at)
}

/// Bin clips whose source file is no longer where it was, for the relink
/// dialog on load.
pub fn missing_media(project: &Project) -> Vec<(crate::model::ClipId, PathBuf)> {
    project
        .clips
        .iter()
        .filter(|c| !c.path.exists())
        .map(|c| (c.id, c.path.clone()))
        .collect()
}

/// Point a bin clip at a new file. Any proxy is dropped, since it described
/// the old file.
pub fn relink(project: &mut Project, id: crate::model::ClipId, new_path: &Path) -> bool {
    let Some(clip) = project.clip_mut(id) else {
        return false;
    };
    clip.path = new_path.to_path_buf();
    clip.proxy_path = None;
    true
}

/// Given a project path, the default proxy directory beside it.
pub fn default_proxy_dir(project_path: Option<&Path>) -> Option<PathBuf> {
    project_path
        .and_then(|p| p.parent())
        .map(|d| d.join(".roughcut-proxies"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipId, SourceClip};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("roughcut-io-{}-{}", tag, std::process::id()));
        let _ = fs::create_dir_all(&d);
        d
    }

    fn sample() -> Project {
        let mut p = Project::new();
        p.clips.push(SourceClip {
            id: ClipId::new(),
            still: false,
            path: PathBuf::from("/media/one.mp4"),
            proxy_path: None,
            duration_frames: 300,
            native_frames: 300,
            native_fps_num: 30000,
            native_fps_den: 1001,
            width: 1920,
            height: 1080,
            sample_aspect_num: 1,
            sample_aspect_den: 1,
            progressive: true,
            colorspace: 709,
            has_audio: true,
            video_index: 0,
            audio_index: 1,
            mark_in: Some(10),
            mark_out: Some(20),
            rate_mismatch: false,
            variable_rate: false,
            flagged: true,
        });
        p
    }

    /// Audio positions are stored rather than derived, so unlike every other
    /// position in the project they can be lost in a save.
    #[test]
    fn audio_tracks_survive_a_save() {
        use crate::audio::{AudioItem, AudioTrack};
        let dir = tmpdir("audio");
        let path = dir.join("p.roughcut");
        let mut p = sample();
        let clip_id = p.clips[0].id;

        let mut track = AudioTrack::new("A1");
        track.place(AudioItem {
            clip_id,
            in_frame: 10,
            out_frame: 109,
            start: 500,
        });
        track.muted = true;
        p.audio.push(track);

        save(&p, &path).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(p, back);

        let item = back.audio[0].items()[0];
        assert_eq!(item.start, 500, "the one position that is not derived");
        assert_eq!((item.in_frame, item.out_frame), (10, 109));
        assert!(back.audio[0].muted, "muting is part of the project");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every project saved before audio tracks existed has no `audio` key at
    /// all, and must still open.
    #[test]
    fn a_project_written_before_audio_tracks_still_opens() {
        let dir = tmpdir("legacy");
        let path = dir.join("old.roughcut");
        let p = sample();
        save(&p, &path).unwrap();

        // Strip the key back out, which is exactly what an older file is.
        let text = fs::read_to_string(&path).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
        json.as_object_mut().unwrap().remove("audio");
        assert!(json.get("audio").is_none());
        fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();

        let back = load(&path).expect("an older project must still open");
        assert!(back.audio.is_empty());
        assert_eq!(back.timeline, p.timeline);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_through_json() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("p.roughcut");
        let p = sample();
        save(&p, &path).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(p, back);
        // Flagging a folder of footage is real work; it has to outlive the
        // session that did it.
        assert!(back.clips[0].flagged);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_is_pretty_printed_for_diffability() {
        let dir = tmpdir("pretty");
        let path = dir.join("p.roughcut");
        save(&sample(), &path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n  \"profile\""), "{text}");
        assert!(text.ends_with('\n'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_replaces_an_existing_file() {
        let dir = tmpdir("atomic");
        let path = dir.join("f.bin");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tmpdir("schema");
        let path = dir.join("p.roughcut");
        let mut p = sample();
        p.version = SCHEMA_VERSION + 1;
        save(&p, &path).unwrap();
        assert!(load(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_media_is_detected() {
        let p = sample();
        assert_eq!(missing_media(&p).len(), 1);
    }

    #[test]
    fn autosave_round_trips_with_its_project_path() {
        let dir = tmpdir("autosave");
        let path = dir.join("autosave.roughcut");
        let p = sample();
        let original = PathBuf::from("/work/cut.roughcut");

        write_autosave(&path, &p, Some(&original), true).unwrap();
        let back = read_autosave(&path).unwrap();
        assert_eq!(back.project, p);
        assert_eq!(back.project_path, Some(original));
        assert!(back.unsaved);
        assert!(autosave_age_secs(&back) < 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autosave_records_that_a_project_was_never_saved() {
        let dir = tmpdir("autosave-unsaved");
        let path = dir.join("autosave.roughcut");
        write_autosave(&path, &sample(), None, true).unwrap();
        assert_eq!(read_autosave(&path).unwrap().project_path, None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The property that matters most: saving must not destroy the snapshot,
    /// only stop it claiming to hold unsaved work.
    #[test]
    fn marking_saved_keeps_the_snapshot() {
        let dir = tmpdir("autosave-mark");
        let path = dir.join("autosave.roughcut");
        let p = sample();
        write_autosave(&path, &p, None, true).unwrap();

        mark_autosave_saved(&path);
        assert!(path.exists(), "the snapshot must survive being marked saved");
        let back = read_autosave(&path).unwrap();
        assert!(!back.unsaved);
        assert_eq!(back.project, p, "the work itself is untouched");

        // Idempotent, and harmless when there is nothing there.
        mark_autosave_saved(&path);
        assert!(!read_autosave(&path).unwrap().unsaved);
        mark_autosave_saved(&dir.join("nothing.roughcut"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_autosave_is_an_error_not_a_panic() {
        let dir = tmpdir("autosave-corrupt");
        let path = dir.join("autosave.roughcut");
        atomic_write(&path, b"{ not json").unwrap();
        assert!(read_autosave(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
