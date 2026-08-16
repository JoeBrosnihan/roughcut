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
    pub last_import_dir: Option<PathBuf>,
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
            last_import_dir: None,
        }
    }
}

/// Where settings and the recovery snapshot live.
///
/// `ROUGHCUT_CONFIG_DIR` overrides the location entirely. That is what keeps a
/// development build from writing over the settings and autosave of the copy
/// you actually use: `.cargo/config.toml` points anything launched through
/// cargo at a scratch directory under `target/`, while an installed binary,
/// run directly, keeps using the real one.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ROUGHCUT_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
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

/// The single recovery snapshot. Beside `settings.json`, so it exists even for
/// a project that has never been saved anywhere.
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
    pub fn resolve_proxy_dir(&self, project_path: Option<&Path>) -> Option<PathBuf> {
        self.proxy_dir.clone().or_else(|| {
            roughcut_core::project_io::default_proxy_dir(project_path)
        })
    }
}
