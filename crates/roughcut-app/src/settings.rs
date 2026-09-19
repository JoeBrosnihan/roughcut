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
    /// Whether an edit to the picture drags the sound under it along. Off by
    /// default, matching Shotcut, because a music bed should stay where it is
    /// while an effect pinned to a moment should not.
    #[serde(default)]
    pub ripple_all_tracks: bool,
    /// Show clips that have been set aside, faded, alongside the rest.
    ///
    /// Off by default: the whole point of archiving is that the bin stops
    /// showing you footage you have already rejected. This is the way back
    /// in — to restore something, or to remove it for good.
    #[serde(default)]
    pub show_archived: bool,
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
            ripple_all_tracks: false,
            show_archived: false,
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

/// Where settings, the recovery snapshot and the derived caches live.
///
/// Defined once in `roughcut_core::paths` and re-exported here, because the
/// command line front door has to find exactly the same files this window
/// does. A second definition of "where" is a second definition that can drift,
/// and the one that would drift silently is the transcript cache.
pub use roughcut_core::paths::{
    autosave_path, config_dir, thumb_cache_dir, transcript_cache_dir, waveform_cache_dir,
};

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
