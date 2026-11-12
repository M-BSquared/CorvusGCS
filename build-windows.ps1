# =============================================================================
# Corvus GCS - build the Windows distributable.
#
# The folder is one executable: the Rust interface (Cargo.toml and src/, built
# with cargo) with the backend (corvus_core) inside it. No CPython, no pip, no
# PyInstaller, no embeddable interpreter, no Qt, no PyO3. The build host needs a
# Rust toolchain (rustup, cargo) and nothing else; the target machine needs
# nothing.
#
#   Corvus GCS\Corvus GCS.exe      the interface and the backend
#   Corvus GCS\VERSION             the version of the build
#   Corvus GCS\LICENSE.md          the licence terms
#   Corvus GCS\assets\licenses\    the texts of the licences it carries
#
#   Windows x64  ->  dist\Corvus GCS\Corvus GCS.exe
#                ->  Corvus_GCS-<version>-windows-x64.zip   (with -Zip)
#
# Usage:
#   .\build-windows.ps1 [-Zip] [-NoVerify] [-Fast]
#
# -Fast builds with the `fast` cargo profile (release code, no LTO) for a local
# try. The interface is built into the repository's target folder (or
# $env:CARGO_TARGET_DIR), the one cargo uses, so a second build only recompiles
# what changed.
#
# Version is read exclusively from the repo-root VERSION file - never
# hardcoded, exactly as build-appimage.sh and build-macos-app.sh do.
#
# A folder and not a self-extracting single file: unpacking into a temp
# directory on every launch costs seconds the field laptop should not spend.
# =============================================================================

[CmdletBinding()]
param(
    [switch]$Zip,
    [switch]$NoVerify,
    [switch]$Fast
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Version = (Get-Content (Join-Path $RepoDir "VERSION") -Raw).Trim()
$AppName = "Corvus GCS"
$BuildDir = Join-Path $RepoDir "build\windows"
$DistDir = Join-Path $RepoDir "dist"
$AppDir = Join-Path $DistDir $AppName
# Artifacts go to dist\, which is gitignored, not to the repo root.
$Zipfile = Join-Path $RepoDir "dist\Corvus_GCS-$Version-windows-x64.zip"

Write-Host "=== CORVUS GCS - Windows build ==="
Write-Host "Version: $Version  (read from VERSION)"
Write-Host "Host:    $([System.Environment]::OSVersion.VersionString) $env:PROCESSOR_ARCHITECTURE"
Write-Host ""

# Everything the bundle is assembled from, checked before anything is built, so
# a build that cannot succeed says so in its first second and names the file.
$Missing = @()
foreach ($required in @(
    "VERSION", "LICENSE.md", "assets\CorvusGCS_logo.png", "assets\licenses",
    "Cargo.toml", "Cargo.lock", "src\main.rs")) {
    if (-not (Test-Path (Join-Path $RepoDir $required))) { $Missing += $required }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found; install Rust from https://rustup.rs and run this again."
}
if ($Missing.Count -gt 0) {
    throw ("the bundle is assembled from files that are not in the repo: " +
           ($Missing -join ", ") + " - restore them (git checkout -- <path>) and run this again.")
}

New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

# ---- 1. the interface and the backend in it ------------------------------------
# rust-toolchain.toml at the repository root pins the Rust it needs, so cargo
# runs from there and rustup fetches it on first use. The fonts and icons are
# cut out of src\vendor by its build.rs, and the languages, the version and the
# bundled plugins are compiled in, so the executable carries them. The target
# folder is the repository's own, so a second build only recompiles what changed.
$Profile = if ($Fast) { "fast" } else { "release" }
Write-Host ">>> Building the interface ($Profile profile, locked) ..."
$GuiLog = Join-Path $BuildDir "cargo-gui.log"
Push-Location $RepoDir
try {
    $Target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $RepoDir "target" }
    $env:CARGO_TARGET_DIR = $Target
    & cargo build --profile $Profile --locked --bin corvus-gcs *> $GuiLog
    if ($LASTEXITCODE -ne 0) {
        Get-Content $GuiLog -Tail 30
        throw "the interface did not build; log: $GuiLog"
    }
} finally {
    Pop-Location
}

# ---- 2. assemble the folder ---------------------------------------------------
if (Test-Path $AppDir) { Remove-Item -Recurse -Force $AppDir }
New-Item -ItemType Directory -Force -Path $AppDir | Out-Null
$Exe = Join-Path $AppDir "$AppName.exe"
Copy-Item (Join-Path $Target "$Profile\corvus-gcs.exe") $Exe
Copy-Item (Join-Path $RepoDir "VERSION") (Join-Path $AppDir "VERSION")
# The Sustainable Use License requires that anyone who receives a copy of the
# software also receives a copy of its terms.
Copy-Item (Join-Path $RepoDir "LICENSE.md") (Join-Path $AppDir "LICENSE.md")
New-Item -ItemType Directory -Force -Path (Join-Path $AppDir "assets") | Out-Null
Copy-Item -Recurse (Join-Path $RepoDir "assets\licenses") (Join-Path $AppDir "assets\licenses")

# ---- 3. verify ----------------------------------------------------------------
# The folder holds the executable and the licence files and no Python of any
# kind. `corvus-build bundle-check` (src/tools) makes the same check on a
# finished bundle.
if (-not $NoVerify) {
    Write-Host ">>> Verifying the bundle ..."
    foreach ($needed in @("$AppName.exe", "VERSION", "LICENSE.md", "assets\licenses")) {
        $p = Join-Path $AppDir $needed
        if (-not (Test-Path $p)) { throw "bundle is missing $needed (looked at $p)" }
    }
    $stamped = (Get-Content (Join-Path $AppDir "VERSION") -Raw).Trim()
    if ($stamped -ne $Version) { throw "bundled VERSION is '$stamped', expected '$Version'" }
    $python = Get-ChildItem -Recurse -Force $AppDir | Where-Object {
        $_.Name -match '^(python.*|libpython.*|serve\.py|pip|site-packages|corvus_link.*|PySide.*)$' -or $_.Extension -eq '.pyc'
    }
    if ($python) {
        throw ("the bundle must hold no Python, and it does: " + (($python | ForEach-Object { $_.FullName }) -join ", "))
    }
    Write-Host "    payload     : $AppName.exe, VERSION, LICENSE.md, assets\licenses; no Python inside"
}

# ---- 4. zip -------------------------------------------------------------------
if ($Zip) {
    Write-Host ">>> Zipping to $Zipfile ..."
    if (Test-Path $Zipfile) { Remove-Item -Force $Zipfile }
    Compress-Archive -Path $AppDir -DestinationPath $Zipfile -CompressionLevel Optimal
}

Write-Host ""
Write-Host "=== done ==="
Write-Host "App: $AppDir"
if ($Zip) { Write-Host "Zip: $Zipfile" }
