//! Discovery of the external binaries Roughcut shells out to.
//!
//! `ffprobe` is a hard requirement for import, `ffmpeg` only for proxies and
//! thumbnails, `melt` only for the frame-accuracy test. Nothing is vendored.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` — keeps a console from flashing on every ffprobe call.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Paths to the external tools, with optional user overrides from settings.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub ffprobe: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub melt: Option<PathBuf>,
}

impl Tools {
    /// Discover everything on `PATH` and in the well-known locations.
    pub fn discover() -> Self {
        Self {
            ffprobe: find_tool("ffprobe"),
            ffmpeg: find_tool("ffmpeg"),
            melt: find_tool("melt"),
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

pub fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            MEDIA_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
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
