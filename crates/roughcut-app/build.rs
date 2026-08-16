// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Stamps the Windows executable with its icon and version information.
//!
//! Done here rather than with a build dependency: the whole job is one call to
//! the Windows SDK's `rc.exe`, and the alternative is a crate that exists to
//! find `rc.exe` for you. If the SDK is not installed the build still
//! succeeds, just without the icon — an icon is not worth failing a build over.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    windows_icon::embed();
}

#[cfg(windows)]
mod windows_icon {
    use std::path::{Path, PathBuf};

    pub fn embed() {
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let icon = manifest.join("../../assets/icon.ico");
        if !icon.exists() {
            println!("cargo:warning=assets/icon.ico is missing; building without an icon");
            return;
        }
        let Some(rc) = find_rc() else {
            println!(
                "cargo:warning=rc.exe was not found (install the Windows SDK); \
                 building without an icon"
            );
            return;
        };

        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
        // `1` is the lowest icon id, which is what Explorer shows for the file.
        let script = format!(
            r#"1 ICON "{}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Roughcut"
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "Roughcut"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon.display().to_string().replace('\\', r"\\"),
            comma = comma_version(&version),
            version = version,
        );

        let rc_path = out.join("roughcut.rc");
        let res_path = out.join("roughcut.res");
        if std::fs::write(&rc_path, script).is_err() {
            return;
        }

        let status = std::process::Command::new(&rc)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res_path)
            .arg(&rc_path)
            .status();
        match status {
            Ok(s) if s.success() && res_path.exists() => {
                println!("cargo:rustc-link-arg-bins={}", res_path.display());
            }
            _ => println!("cargo:warning=rc.exe failed; building without an icon"),
        }
    }

    /// `1.2.3` as the `1,2,3,0` that VERSIONINFO wants.
    fn comma_version(v: &str) -> String {
        let mut parts: Vec<String> = v
            .split(['.', '-', '+'])
            .filter_map(|p| p.parse::<u32>().ok())
            .map(|n| n.to_string())
            .collect();
        parts.resize(4, "0".to_string());
        parts.join(",")
    }

    /// `rc.exe` ships in the Windows SDK, in a versioned directory that is not
    /// on `PATH`. Prefer whatever `PATH` has, then take the newest SDK found.
    fn find_rc() -> Option<PathBuf> {
        if std::process::Command::new("rc.exe")
            .arg("/?")
            .output()
            .is_ok()
        {
            return Some(PathBuf::from("rc.exe"));
        }
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        let roots = [
            r"C:\Program Files (x86)\Windows Kits\10\bin",
            r"C:\Program Files\Windows Kits\10\bin",
        ];
        let mut found: Vec<PathBuf> = Vec::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(Path::new(root)) else {
                continue;
            };
            for e in entries.flatten() {
                let candidate = e.path().join(arch).join("rc.exe");
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
        }
        // Directory names are SDK versions, so the last one sorted is newest.
        found.sort();
        found.pop()
    }
}
