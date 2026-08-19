//! User settings, stored next to the app's config directory as JSON.
//!
//! Settings are not part of the project and are not undoable.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// §10: proxies are optional and off by default.
    pub proxies_enabled: bool,
    /// `None` means "beside the project file, in .roughcut-proxies".
    pub proxy_dir: Option<PathBuf>,
    pub ffprobe_path: Option<PathBuf>,
    pub ffmpeg_path: Option<PathBuf>,
    pub mpv_path: Option<PathBuf>,
    pub volume: f64,
    pub last_project_dir: Option<PathBuf>,
    /// Projects opened or saved, most recent first. Capped at
    /// [`MAX_RECENT`] — a list long enough to need scrolling is a file
    /// dialog with extra steps.
    pub recent_projects: Vec<PathBuf>,
    pub last_import_dir: Option<PathBuf>,
    /// Where the window was last time, so it opens where you left it instead
    /// of at a fixed size every launch. Windows does not remember this for an
    /// application; applications remember it for themselves.
    pub window: Option<WindowGeometry>,
}

/// How many projects the recents list keeps.
pub const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Recorded separately: the position and size above are the *restored*
    /// ones, so un-maximising puts the window back somewhere sensible.
    pub maximized: bool,
}

impl WindowGeometry {
    /// Reject nonsense before handing it to the window system — a saved
    /// position from a monitor that is no longer attached, or a size from a
    /// resolution that no longer exists.
    pub fn is_plausible(&self) -> bool {
        self.width >= 640.0
            && self.height >= 480.0
            && self.width <= 32_000.0
            && self.height <= 32_000.0
            && self.x > -32_000.0
            && self.y > -32_000.0
            && self.x < 32_000.0
            && self.y < 32_000.0
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            proxies_enabled: false,
            proxy_dir: None,
            ffprobe_path: None,
            ffmpeg_path: None,
            mpv_path: None,
            volume: 80.0,
            last_project_dir: None,
            recent_projects: Vec::new(),
            last_import_dir: None,
            window: None,
        }
    }
}

fn recent_key(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Where settings and the recovery snapshot live.
///
/// A dev build must never touch the settings or — far worse — the recovery
/// snapshot of the promoted copy someone is actually editing in. Three layers,
/// most specific first:
///
/// 1. `ROUGHCUT_CONFIG_DIR`, if set.
/// 2. **Where the binary is.** An executable sitting in `target/debug` or
///    `target/release` is by definition a dev build, however it was launched.
///    Relying on `.cargo/config.toml` alone was not enough: it only applies to
///    `cargo run`, so launching `target\release\roughcut.exe` directly — which
///    is exactly what a test script does — silently shared production's state.
/// 3. The platform's config directory, for a promoted binary living anywhere
///    else.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ROUGHCUT_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dev) = dev_config_dir() {
        return Some(dev);
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("Roughcut"))
}

/// `<target>/dev-config` when this executable is running from a cargo build
/// directory, otherwise `None`.
fn dev_config_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    let profile = parent.file_name()?.to_str()?;
    if profile != "debug" && profile != "release" {
        return None;
    }
    let target = parent.parent()?;
    if target.file_name()?.to_str()? != "target" {
        return None;
    }
    Some(target.join("dev-config"))
}

/// The single recovery snapshot. Beside `settings.json`, so it exists even for
/// a project that has never been saved anywhere.
/// Where bin filmstrips are kept between sessions.
///
/// Under the config directory, so a development build cannot serve or poison
/// the cache of the copy you actually edit with — the same isolation the
/// settings and the recovery snapshot get.
pub fn thumb_cache_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("thumbnails"))
}

/// Where audio envelopes are kept between sessions, for the same reason.
pub fn waveform_cache_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("waveforms"))
}

pub fn autosave_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("autosave.roughcut"))
}

fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("ignoring unreadable settings at {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Err(e) = roughcut_core::project_io::atomic_write(&path, json.as_bytes()) {
            log::warn!("cannot save settings: {e}");
        }
    }

    /// Where proxies for `project_path` should live.
    /// Put `path` at the top of the recents list.
    ///
    /// Case-insensitive de-duplication on Windows, where `C:\A.roughcut` and
    /// `c:.roughcut` are the same file and listing both would be nonsense.
    pub fn remember_recent(&mut self, path: &Path) {
        let key = recent_key(path);
        self.recent_projects.retain(|p| recent_key(p) != key);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(MAX_RECENT);
    }

    /// Drop a project from the list — used when opening one fails, since a
    /// path that no longer resolves is not worth offering again.
    pub fn forget_recent(&mut self, path: &Path) {
        let key = recent_key(path);
        self.recent_projects.retain(|p| recent_key(p) != key);
    }

    pub fn resolve_proxy_dir(&self, project_path: Option<&Path>) -> Option<PathBuf> {
        self.proxy_dir.clone().or_else(|| {
            roughcut_core::project_io::default_proxy_dir(project_path)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule that keeps a dev build away from production's state. Getting
    /// this wrong once already cost a recovery snapshot.
    #[test]
    fn only_cargo_build_directories_count_as_dev() {
        let cases = [
            (r"C:\src\roughcut\target\release\roughcut.exe", true),
            (r"C:\src\roughcut\target\debug\roughcut.exe", true),
            (r"/home/j/roughcut/target/release/roughcut", true),
            // A promoted copy, wherever it lives.
            (r"C:\Users\j\AppData\Local\Programs\Roughcut\roughcut.exe", false),
            (r"/usr/local/bin/roughcut", false),
            // Near misses that must not be mistaken for a build directory.
            (r"C:\target\roughcut.exe", false),
            (r"C:\apps\release\roughcut.exe", false),
            (r"C:\target\staging\roughcut.exe", false),
        ];
        for (path, expect_dev) in cases {
            let p = PathBuf::from(path);
            let is_dev = p.parent().is_some_and(|parent| {
                let profile = parent.file_name().and_then(|s| s.to_str());
                let target = parent.parent().and_then(|t| t.file_name()).and_then(|s| s.to_str());
                matches!(profile, Some("debug") | Some("release")) && target == Some("target")
            });
            assert_eq!(is_dev, expect_dev, "{path}");
        }
    }
}
