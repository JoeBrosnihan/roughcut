//! Discovery of the external binaries Roughcut shells out to.
//!
//! `ffprobe` is a hard requirement for import, `ffmpeg` only for proxies and
//! thumbnails, `melt` only for the frame-accuracy test. Nothing is vendored.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` — keeps a console from flashing on every ffprobe call.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`, so background transcoding yields to whatever
/// the user is doing rather than competing with it.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// How many threads one background ffmpeg may use.
///
/// ffmpeg helps itself to one thread per core unless told otherwise, and the
/// worker pool runs up to four at once. On a sixteen-core machine that is
/// sixty-four threads of transcoding against sixteen cores, which is what made
/// the whole machine stutter while a bin was being prepared. Bounding it so
/// that pool x threads lands near half the machine leaves the other half for
/// the thing the user is actually looking at.
pub fn background_threads() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // Mirrors the pool size in the app: min(cpus / 2, 4).
    let pool = (cpus / 2).clamp(1, 4);
    ((cpus / 2) / pool).clamp(1, 4)
}

/// A `Command` for speculative work: thumbnails, proxies, waveforms,
/// transcription — anything the user did not ask for and is not waiting on.
///
/// Lowered to below-normal priority, which the worker threads' own priority
/// does **not** do for them. `SetThreadPriority` applies to the calling thread
/// and nothing else; a child process starts at normal priority whatever the
/// thread that spawned it was set to. So all the careful thread-priority work
/// in the pool had no effect on the actual load, because the actual load is
/// entirely in these child processes.
///
/// Foreground work — rendering an export, rotating a clip, probing a file
/// during import — deliberately does not use this. The user is waiting on
/// those.
pub fn background_command(program: impl AsRef<Path>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = quiet_command(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    cmd
}

/// Build a `Command` that never pops a console window on Windows.
pub fn quiet_command(program: impl AsRef<Path>) -> Command {
    // `mut` is only needed for the Windows-only call below.
    #[allow(unused_mut)]
    let mut cmd = Command::new(program.as_ref());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(windows)]
const EXE: &str = ".exe";
#[cfg(not(windows))]
const EXE: &str = "";

/// Directories that commonly hold a usable ffmpeg suite, checked after `PATH`.
/// Shotcut bundles ffmpeg, ffprobe and melt, which is the copy most users
/// already have.
fn well_known_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for base in [
            r"C:\Program Files\Shotcut",
            // whisper.cpp, unzipped where `promote.ps1` puts Roughcut itself.
            &format!(
                r"{}\Programs\whisper",
                std::env::var("LOCALAPPDATA").unwrap_or_default()
            ),
            r"C:\Program Files\ffmpeg\bin",
            r"C:\Program Files (x86)\Shotcut",
        ] {
            dirs.push(PathBuf::from(base));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Programs\Shotcut"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        for base in [
            "/Applications/Shotcut.app/Contents/MacOS",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/local/bin",
        ] {
            dirs.push(PathBuf::from(base));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for base in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
            dirs.push(PathBuf::from(base));
        }
    }
    dirs
}

// ---------------------------------------------------------------------------
// The child-process ledger
//
// Every substantial child this program runs — a proxy encode, a filmstrip, a
// transcription — goes through `run`, which names it, times it, and tells
// whoever is listening when it starts and stops. The point is that "ffmpeg is
// eating the machine" stops being a mystery: something can watch the ledger
// and say *which* ffmpeg, doing *what*, holding *how much*.
//
// The ledger itself carries no policy and no platform code: it reports, and
// the observer (installed by the application; see the app's `gauge` module)
// decides what to measure and when to complain. With no observer installed —
// the CLI, the tests — `run` degrades to exactly `.output()` plus a debug log.
// ---------------------------------------------------------------------------

/// What a measured run reports as it happens.
#[derive(Debug, Clone, Copy)]
pub enum RunEvent<'a> {
    Started { pid: u32, label: &'a str },
    Finished { pid: u32, label: &'a str, seconds: f64, ok: bool },
}

type RunObserver = Box<dyn Fn(RunEvent<'_>) + Send + Sync>;

static RUN_OBSERVER: OnceLock<RunObserver> = OnceLock::new();

/// Install the process-wide observer. First caller wins; later calls are
/// ignored, which keeps a second window or a test harness from fighting over
/// it.
pub fn observe_runs(observer: RunObserver) {
    let _ = RUN_OBSERVER.set(observer);
}

pub fn note_run_started(pid: u32, label: &str) {
    if let Some(f) = RUN_OBSERVER.get() {
        f(RunEvent::Started { pid, label });
    }
}

pub fn note_run_finished(pid: u32, label: &str, seconds: f64, ok: bool) {
    if let Some(f) = RUN_OBSERVER.get() {
        f(RunEvent::Finished { pid, label, seconds, ok });
    }
}

/// `.output()`, with a name attached.
///
/// Identical contract to [`Command::output`] — stdin closed, stdout and stderr
/// captured — plus the start/finish notifications above and a debug line with
/// the duration. Everything long-running should come through here; a child
/// that bypasses the ledger is invisible to every diagnostic built on it.
pub fn run(label: &str, cmd: &mut Command) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    let started = std::time::Instant::now();
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    note_run_started(pid, label);
    let output = child.wait_with_output();
    let seconds = started.elapsed().as_secs_f64();
    let ok = output.as_ref().map(|o| o.status.success()).unwrap_or(false);
    note_run_finished(pid, label, seconds, ok);
    log::debug!("{label}: {seconds:.1}s, ok={ok}");
    output
}

/// Look for `name` on `PATH`, then in the well-known directories.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{EXE}");

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in well_known_dirs() {
        let candidate = dir.join(&file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Find a `whisper-cli` that has a model beside it.
///
/// There can be more than one on a machine — Shotcut ships its own, without
/// any model — and a binary with nothing to load is worse than none at all,
/// because it fails per clip instead of once at startup. Every candidate is
/// considered and one with a model always wins; a bare binary is returned only
/// if that is all there is, so the error can still say something useful.
pub fn find_whisper() -> Option<PathBuf> {
    let file = format!("whisper-cli{EXE}");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|d| d.join(&file)));
    }
    candidates.extend(well_known_dirs().into_iter().map(|d| d.join(&file)));

    let present: Vec<PathBuf> = candidates.into_iter().filter(|p| p.is_file()).collect();
    present
        .iter()
        .find(|p| crate::whisper::find_model(p).is_some())
        .or_else(|| present.first())
        .cloned()
}

/// Paths to the external tools, with optional user overrides from settings.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub ffprobe: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub melt: Option<PathBuf>,
    /// whisper.cpp, for transcripts. Entirely optional: without it the
    /// transcript view says so and everything else is unaffected.
    pub whisper: Option<PathBuf>,
}

impl Tools {
    /// Discover everything on `PATH` and in the well-known locations.
    pub fn discover() -> Self {
        Self {
            ffprobe: find_tool("ffprobe"),
            ffmpeg: find_tool("ffmpeg"),
            melt: find_tool("melt"),
            whisper: find_whisper(),
        }
    }

    /// Apply explicit paths from settings, ignoring ones that do not exist.
    /// `melt` is not overridable: only the test harness runs it, and that
    /// discovers its own copy.
    pub fn with_overrides(mut self, ffprobe: Option<&Path>, ffmpeg: Option<&Path>) -> Self {
        if let Some(p) = ffprobe.filter(|p| p.is_file()) {
            self.ffprobe = Some(p.to_path_buf());
        }
        if let Some(p) = ffmpeg.filter(|p| p.is_file()) {
            self.ffmpeg = Some(p.to_path_buf());
        }
        self
    }

    pub fn has_ffprobe(&self) -> bool {
        self.ffprobe.is_some()
    }

    pub fn has_ffmpeg(&self) -> bool {
        self.ffmpeg.is_some()
    }
}

/// File extensions the importer will accept when a folder is dropped.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "avi", "mxf", "mts", "m2ts", "ts", "webm", "wmv", "flv", "mpg",
    "mpeg", "vob", "3gp", "braw", "r3d", "dv", "ogv",
];

/// Photographs, which a trip or an event produces alongside the video and
/// which belong in the same cut.
///
/// Recognised by extension rather than by what ffprobe says. A HEIC is an
/// HEVC frame in an MP4-family container, so by codec it is indistinguishable
/// from a video; by name it is unambiguous.
pub const STILL_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "heic", "heif", "webp", "bmp", "tif", "tiff"];

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| list.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// A photograph rather than footage: no duration of its own, and given one.
pub fn is_still_image(path: &Path) -> bool {
    has_extension(path, STILL_EXTENSIONS)
}

pub fn is_media_file(path: &Path) -> bool {
    has_extension(path, MEDIA_EXTENSIONS) || is_still_image(path)
}

/// Expand a dropped path into importable files. Folders recurse exactly one
/// level, as §9 specifies.
pub fn expand_drop(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return if is_media_file(path) {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    if !path.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(path) else {
        return out;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        if entry.is_file() {
            if is_media_file(&entry) {
                out.push(entry);
            }
        } else if entry.is_dir() {
            // One level only: list this child directory but do not descend.
            let Ok(children) = std::fs::read_dir(&entry) else {
                continue;
            };
            let mut children: Vec<_> = children
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_media_file(p))
                .collect();
            children.sort();
            out.append(&mut children);
        }
    }
    out
}

#[cfg(test)]
mod ledger_tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex as StdMutex;

    // The observer is process-wide and can be installed once, so every
    // assertion about it lives in this one test.
    static EVENTS: StdMutex<Option<mpsc::Sender<(u32, String, bool)>>> = StdMutex::new(None);

    #[test]
    fn a_measured_run_reports_itself() {
        let (tx, rx) = mpsc::channel();
        *EVENTS.lock().unwrap() = Some(tx);
        observe_runs(Box::new(|event| {
            let guard = EVENTS.lock().unwrap();
            let Some(tx) = guard.as_ref() else { return };
            match event {
                RunEvent::Started { pid, label } => {
                    let _ = tx.send((pid, format!("start:{label}"), true));
                }
                RunEvent::Finished { pid, label, seconds, ok } => {
                    assert!(seconds >= 0.0);
                    let _ = tx.send((pid, format!("finish:{label}"), ok));
                }
            }
        }));

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = Command::new("true");

        let output = run("probe-test", &mut cmd).expect("the shell should run");
        assert!(output.status.success());

        let (pid_a, first, _) = rx.recv().expect("a start event");
        let (pid_b, second, ok) = rx.recv().expect("a finish event");
        assert_eq!(first, "start:probe-test");
        assert_eq!(second, "finish:probe-test");
        assert_eq!(pid_a, pid_b, "start and finish describe the same child");
        assert_ne!(pid_a, 0);
        assert!(ok, "exit 0 is a success");

        // A failing child is reported as one, not hidden.
        #[cfg(windows)]
        let mut bad = {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 3"]);
            c
        };
        #[cfg(not(windows))]
        let mut bad = Command::new("false");
        let _ = run("probe-test", &mut bad).expect("spawning still works");
        let _ = rx.recv().expect("start");
        let (_, _, ok) = rx.recv().expect("finish");
        assert!(!ok, "exit 3 is a failure");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_extensions_are_case_insensitive() {
        assert!(is_media_file(Path::new("/a/B.MP4")));
        assert!(is_media_file(Path::new("/a/b.mov")));
        assert!(!is_media_file(Path::new("/a/b.txt")));
        assert!(!is_media_file(Path::new("/a/b")));
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;

    /// The pool runs several of these at once, so the point is the product:
    /// workers x threads must not exceed the machine.
    #[test]
    fn background_ffmpeg_never_claims_the_whole_machine() {
        let n = background_threads();
        assert!((1..=4).contains(&n), "{n} threads is not a sane bound");

        let cpus = std::thread::available_parallelism()
            .map(|c| c.get())
            .unwrap_or(4);
        let pool = (cpus / 2).clamp(1, 4);
        assert!(
            pool * n <= cpus,
            "{pool} workers x {n} threads oversubscribes {cpus} cores"
        );
    }
}
