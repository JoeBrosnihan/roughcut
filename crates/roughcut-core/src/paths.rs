//! Where Roughcut keeps things that are not the project: settings, the
//! recovery snapshot, and the three derived caches.
//!
//! In the model crate rather than the application because the caches are not
//! the GUI's private business. A transcript costs tens of seconds to produce;
//! anything that can produce one has to be able to find one that already
//! exists, and a second definition of "where" is a second definition that can
//! drift. The command-line front door reads and writes exactly the same files
//! the window does.

use std::path::{Path, PathBuf};

/// The configuration directory. Three layers, most specific first:
///
/// 1. `ROUGHCUT_CONFIG_DIR`, if set.
/// 2. **Where the binary is.** An executable sitting in `target/debug` or
///    `target/release` is by definition a development build, however it was
///    launched. Relying on `.cargo/config.toml` alone was not enough: it only
///    applies to `cargo run`, so launching `target\release\roughcut.exe`
///    directly — which is exactly what a test script does — silently shared
///    production's state.
/// 3. The platform's configuration directory, for a promoted binary living
///    anywhere else.
///
/// A development build must never touch the settings or — far worse — the
/// recovery snapshot of the promoted copy someone is actually editing in.
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
    std::env::current_exe().ok().and_then(|exe| dev_config_for(&exe))
}

/// Split out from [`dev_config_dir`] so the rule can be tested against paths
/// this process is not actually running from.
fn dev_config_for(exe: &Path) -> Option<PathBuf> {
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
pub fn autosave_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("autosave.roughcut"))
}

/// Where bin filmstrips are kept between sessions.
///
/// Under the configuration directory, so a development build cannot serve or
/// poison the cache of the copy you actually edit with — the same isolation
/// the settings and the recovery snapshot get.
pub fn thumb_cache_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("thumbnails"))
}

/// Where audio envelopes are kept between sessions, for the same reason.
pub fn waveform_cache_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("waveforms"))
}

/// Where transcripts are kept. The most valuable cache of the three: a
/// transcript costs tens of seconds to produce and tens of kilobytes to keep.
pub fn transcript_cache_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("transcripts"))
}

/// Identifies a file, the version of it on disk right now, and whatever else
/// would change what is derived from it.
///
/// A rotated or replaced source therefore misses rather than serving a stale
/// strip or a waveform of the wrong audio, and no explicit invalidation is
/// needed anywhere.
pub fn fingerprint(path: &Path, extra: &[i64]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    extra.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut h);
        if let Ok(t) = meta.modified() {
            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                d.as_nanos().hash(&mut h);
            }
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_directory_is_recognised_as_a_development_build() {
        assert_eq!(
            dev_config_for(Path::new("/w/target/release/roughcut.exe")),
            Some(PathBuf::from("/w/target/dev-config"))
        );
        // The command line front door lives beside the window and must land in
        // the same place, or the two would keep separate caches.
        assert_eq!(
            dev_config_for(Path::new("/w/target/debug/roughcut-cli.exe")),
            Some(PathBuf::from("/w/target/dev-config"))
        );
        assert_eq!(dev_config_for(Path::new("/opt/roughcut/roughcut")), None);
        assert_eq!(dev_config_for(Path::new("/w/target/debug/deps/t-1a2b")), None);
    }

    /// Exercise the actual config-path resolver with native paths. A Windows
    /// backslash is a filename character on Unix, not a directory separator.
    #[test]
    fn only_cargo_build_directories_count_as_dev() {
        let root = if cfg!(windows) {
            Path::new(r"C:\src")
        } else {
            Path::new("/src")
        };
        for relative in [
            "roughcut/target/release/roughcut",
            "roughcut/target/debug/roughcut-cli",
        ] {
            assert_eq!(
                dev_config_for(&root.join(relative)),
                Some(root.join("roughcut/target/dev-config")),
            );
        }
        for relative in [
            "Programs/Roughcut/roughcut",
            "bin/roughcut",
            "target/roughcut",
            "apps/release/roughcut",
            "target/staging/roughcut",
        ] {
            assert_eq!(dev_config_for(&root.join(relative)), None, "{relative}");
        }
    }

    #[test]
    fn an_override_wins() {
        // Not tested through `config_dir` itself: the environment is process
        // wide and tests share one, so setting it would be visible to any test
        // running alongside.
        assert!(config_dir().is_some());
    }

    #[test]
    fn a_missing_file_still_fingerprints() {
        let a = fingerprint(Path::new("/nowhere/a.mp4"), &[]);
        let b = fingerprint(Path::new("/nowhere/b.mp4"), &[]);
        assert_ne!(a, b);
        assert_ne!(a, fingerprint(Path::new("/nowhere/a.mp4"), &[1]));
        assert_eq!(a, fingerprint(Path::new("/nowhere/a.mp4"), &[]));
    }
}
