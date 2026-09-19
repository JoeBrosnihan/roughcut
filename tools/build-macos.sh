#!/bin/bash
# Build a local app bundle. Media tools remain supplied by Homebrew/Shotcut.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
    echo "This script requires macOS." >&2
    exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
cargo build --locked --release --workspace

bundle="$repo_root/target/macos/Roughcut.app"
mkdir -p "$bundle/Contents/MacOS"
# Rename into place so rebuilding does not overwrite a running executable.
cp target/release/roughcut "$bundle/Contents/MacOS/roughcut.new"
mv "$bundle/Contents/MacOS/roughcut.new" "$bundle/Contents/MacOS/roughcut"

python3 - "$bundle" "$repo_root" <<'PY'
import pathlib
import plistlib
import sys

bundle, repo = map(pathlib.Path, sys.argv[1:])
info = {
    "CFBundleName": "Roughcut",
    "CFBundleDisplayName": "Roughcut",
    "CFBundleIdentifier": "org.roughcut.local",
    "CFBundleExecutable": "roughcut",
    "CFBundlePackageType": "APPL",
    "NSHighResolutionCapable": True,
    # This is a development bundle, sharing cargo's isolated settings/cache.
    "LSEnvironment": {
        "ROUGHCUT_CONFIG_DIR": str(repo / "target/dev-config"),
        "ROUGHCUT_LOG_FILE": str(repo / "target/macos/roughcut.log"),
    },
}
with (bundle / "Contents/Info.plist").open("wb") as out:
    plistlib.dump(info, out)
PY

echo "Built $bundle"
echo "Launch with: open \"$bundle\""
