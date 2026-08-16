<#
.SYNOPSIS
    Promote the current build to stable.

.DESCRIPTION
    Roughcut exists in two forms:

      stable  the promoted copy in %LOCALAPPDATA%\Programs\Roughcut, which you
              actually use for editing, and which only ever changes when this
              script is run
      dev     the source tree, run with `cargo run`, which changes constantly

    Promoting builds release and copies the binary and libmpv into the stable
    location. Nothing there is touched by `cargo build`, `cargo clean`, or a
    broken commit — it only changes when you run this script again.

    Tag the commit you promote, so `stable` is always recoverable from source:

        git tag -a v0.2.0 -m "..." && .\tools\promote.ps1

    The installed copy is self-contained: libmpv sits beside the executable,
    which is the first place Roughcut looks for it.

    State is kept separate too. The installed copy uses the real settings
    directory; anything run through cargo uses target/dev-config, via
    .cargo/config.toml. A development build cannot clobber the settings or the
    recovery snapshot of the copy you rely on.

.PARAMETER Dest
    Where to install. Defaults to %LOCALAPPDATA%\Programs\Roughcut.

.PARAMETER NoShortcut
    Skip creating the Start Menu shortcut.

.EXAMPLE
    .\tools\install.ps1
    .\tools\install.ps1 -Dest D:\Tools\Roughcut
#>
[CmdletBinding()]
param(
    [string]$Dest = "$env:LOCALAPPDATA\Programs\Roughcut",
    [switch]$NoShortcut
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

Write-Host "Building release..." -ForegroundColor Cyan
Push-Location $repo
try {
    # cargo writes progress to stderr, and with ErrorActionPreference = Stop
    # PowerShell 5.1 treats any native stderr output as a terminating error.
    # Relax it for the call and judge success by the exit code instead.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & cargo build --release --workspace
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { throw "cargo build failed (exit $code)" }
} finally {
    Pop-Location
}

$exe = Join-Path $repo 'target\release\roughcut.exe'
if (-not (Test-Path $exe)) { throw "not found: $exe" }

# libmpv must travel with the binary or the monitor will not work.
$mpv = Get-ChildItem (Join-Path $repo 'vendor\mpv') -Filter 'libmpv-2.dll' -ErrorAction SilentlyContinue |
    Select-Object -First 1
if (-not $mpv) {
    throw "libmpv-2.dll not found under vendor\mpv - see the README for where to get it"
}

New-Item -ItemType Directory -Force $Dest | Out-Null

# Copy to a temp name and swap, so a copy that fails partway cannot leave a
# broken executable in place of a working one.
$staged = Join-Path $Dest 'roughcut.exe.new'
Copy-Item $exe $staged -Force
if (Test-Path (Join-Path $Dest 'roughcut.exe')) {
    Remove-Item (Join-Path $Dest 'roughcut.exe') -Force
}
Move-Item $staged (Join-Path $Dest 'roughcut.exe')
Copy-Item $mpv.FullName (Join-Path $Dest 'libmpv-2.dll') -Force

# Record what this build was, so you can tell it from a later one.
$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$stamp = Get-Date -Format 'yyyy-MM-dd HH:mm'
@"
Roughcut $version
installed $stamp
from      $repo
"@ | Set-Content (Join-Path $Dest 'VERSION.txt') -Encoding utf8

if (-not $NoShortcut) {
    $startMenu = [Environment]::GetFolderPath('Programs')
    $link = Join-Path $startMenu 'Roughcut.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $sc = $shell.CreateShortcut($link)
    $sc.TargetPath = Join-Path $Dest 'roughcut.exe'
    $sc.WorkingDirectory = $Dest
    $sc.Description = "Roughcut $version - keyboard-driven assembly editor"
    $sc.Save()
    Write-Host "Shortcut : $link"
}

Write-Host ""
Write-Host "Installed Roughcut $version" -ForegroundColor Green
Write-Host "  Location : $Dest"
Write-Host "  Settings : $env:APPDATA\Roughcut  (dev builds use target\dev-config)"
Write-Host ""
Write-Host "The installed copy is now independent of the source tree."
