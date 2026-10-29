# =============================================================================
# Corvus GCS - build the Windows distributable.
#
# The Linux and macOS scripts hand-roll a relocatable interpreter because both
# platforms make that the shortest honest path: an AppImage is a squashfs of a
# tree you lay out yourself, and a .app is a directory with rules about where
# dylibs may point. Windows has neither constraint and one well-trodden tool,
# so the backend (serve.py) is frozen with PyInstaller rather than reinventing
# the same 300 lines in PowerShell. The interface is the Rust app
# (rust/corvus_gui, built with cargo), which starts that backend as its child.
# The result is the same shape as the other two: one directory that runs on a
# machine with no Python and no internet.
#
#   Corvus GCS\Corvus GCS.exe      the interface
#   Corvus GCSackend\            the frozen backend, started by the interface
#
#   Windows x64  ->  dist\Corvus GCS\Corvus GCS.exe
#                ->  Corvus_GCS-<version>-windows-x64.zip   (with -Zip)
#
# Usage:
#   .\build-windows.ps1 [-Zip] [-NoVerify] [-Python <path to python.exe>]
#
# Version is read exclusively from the repo-root VERSION file - never
# hardcoded, exactly as build-appimage.sh and build-macos-app.sh do.
#
# Why onedir and not --onefile: --onefile unpacks the whole backend into a
# temp directory on every launch, which costs several seconds and defeats the
# "ready to fly in seconds" property the project is built around. A folder
# starts immediately.
# =============================================================================

[CmdletBinding()]
param(
    [switch]$Zip,
    [switch]$NoVerify,
    [string]$Python = ""
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

# Everything the bundle is assembled from, checked before anything is built.
# The other two build scripts used to discover a missing file one `cp` at a
# time, hundreds of lines in and after the whole dependency install; naming it
# in the first second is cheaper for everyone.
$Missing = @()
foreach ($required in @(
    "VERSION", "pyproject.toml", "LICENSE.md",
    "corvus", "src", "assets", "plugins", "assets\CorvusGCS_logo.png", "serve.py",
    "rust\corvus_gui\Cargo.toml", "rust\corvus_link\Cargo.toml")) {
    if (-not (Test-Path (Join-Path $RepoDir $required))) { $Missing += $required }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found; install Rust from https://rustup.rs and run this again."
}
if ($Missing.Count -gt 0) {
    throw ("the bundle is assembled from files that are not in the repo: " +
           ($Missing -join ", ") + " - restore them (git checkout -- <path>) and run this again.")
}

# ---- 1. interpreter ---------------------------------------------------------
# A conda interpreter cannot be packaged reliably (its DLLs live outside the
# prefix), which is the same rejection build-macos-app.sh makes and for the
# same reason. Say so plainly rather than failing deep inside PyInstaller.
if (-not $Python) {
    $Python = (Get-Command python -ErrorAction SilentlyContinue).Source
}
if (-not $Python) {
    throw "No python.exe found. Install Python 3.12+ from python.org, or pass -Python <path>."
}
$pyInfo = & $Python -c @"
import sys, json
print(json.dumps({
    'version': '%d.%d' % sys.version_info[:2],
    'conda': 'conda' in sys.version.lower() or 'Continuum' in sys.version,
    'bits': 64 if sys.maxsize > 2**32 else 32,
}))
"@ | ConvertFrom-Json

if ($pyInfo.conda) {
    throw "$Python is a conda interpreter; it cannot be packaged. Use a python.org build."
}
if ($pyInfo.bits -ne 64) {
    throw "$Python is 32-bit. Corvus ships x64 only; install the 64-bit python.org build."
}
# 3.12 is the floor pyproject.toml declares and the version CI tests.
# Nothing checked this before, so the shipped .exe could be built on an
# interpreter older than anything the suite had ever run against.
$pyParts = $pyInfo.version.Split(".")
if ([int]$pyParts[0] -lt 3 -or ([int]$pyParts[0] -eq 3 -and [int]$pyParts[1] -lt 12)) {
    throw "$Python is Python $($pyInfo.version); Corvus requires >= 3.12. Install it from python.org, or pass -Python <path>."
}
Write-Host ">>> Interpreter: $Python (Python $($pyInfo.version), x64)"

# ---- 2. build venv ----------------------------------------------------------
# Kept out of the developer's own environment: the build installs PyInstaller
# and the runtime deps, and doing that to whatever interpreter happens to be on
# PATH is how a machine ends up unable to reproduce its own release.
$VenvDir = Join-Path $BuildDir "venv"
$VenvPy = Join-Path $VenvDir "Scripts\python.exe"
if (-not (Test-Path $VenvPy)) {
    Write-Host ">>> Creating build venv in $VenvDir ..."
    New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
    & $Python -m venv $VenvDir
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $VenvPy)) {
        throw "could not create the build venv at $VenvDir"
    }
}

# The runtime list is pyproject.toml's `headless` group, which every other
# installer also reads. The build-time tool - PyInstaller - is its
# `package-windows` group: named in the same file, never shipped as a runtime
# dependency.
#
# Which runtime source, in order: $env:CORVUS_REQUIREMENTS, then
# requirements.lock if the repo has one, then the `headless` group. The lock is
# what makes a release rebuildable - the group states floors, so installing
# from it in six months resolves to whatever is newest then. Every build
# writes the set it actually installed to dist\*.lock; promoting one to
# requirements.lock at tag time pins the next rebuild to it.
#
# "${Pyproject}:headless", braced: unbraced, PowerShell would read "$Pyproject:app"
# as a scope-qualified variable named app and pass pip an empty string.
$Pyproject = Join-Path $RepoDir "pyproject.toml"
$Requirements = $env:CORVUS_REQUIREMENTS
if (-not $Requirements) {
    $RepoLock = Join-Path $RepoDir "requirements.lock"
    if (Test-Path $RepoLock) { $Requirements = $RepoLock }
}
if ($Requirements) {
    if (-not (Test-Path $Requirements)) { throw "missing $Requirements" }
    $DepsArgs = @("-r", $Requirements)
    $DepsLabel = Split-Path -Leaf $Requirements
} else {
    $DepsArgs = @("--group", "${Pyproject}:headless")
    $DepsLabel = "pyproject.toml [headless]"
}
$LockOut = Join-Path $DistDir "Corvus_GCS-$Version-windows-x64.lock"
Write-Host ">>> Installing $DepsLabel plus pyproject.toml [package-windows] ..."
$PipLog = Join-Path $BuildDir "pip.log"
# pip >= 25.1 is what reads [dependency-groups]; a fresh venv ships older.
& $VenvPy -m pip install --upgrade pip *>> $PipLog
if ($LASTEXITCODE -eq 0) { & $VenvPy -m pip install @DepsArgs *>> $PipLog }
if ($LASTEXITCODE -eq 0) { & $VenvPy -m pip install --group "${Pyproject}:package-windows" *>> $PipLog }
# MAVLink is corvus_link, the Rust extension; building it needs cargo here.
if ($LASTEXITCODE -eq 0) { & $VenvPy -m pip install (Join-Path $RepoDir "rust\corvus_link") *>> $PipLog }
if ($LASTEXITCODE -ne 0) {
    Write-Host "--- last 40 lines of $PipLog ---"
    Get-Content $PipLog -Tail 40
    throw "dependency install failed"
}

# What went into THIS bundle, exactly. Ships beside the artifact so a rebuild
# can be told to resolve to the same versions:
#   copy dist\Corvus_GCS-<version>-windows-x64.lock requirements.lock
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
$Frozen = & $VenvPy -m pip freeze --all --exclude-editable --exclude corvus-link
if ($LASTEXITCODE -eq 0) {
    @(
        "# Corvus GCS $Version - Windows x64, python $($pyInfo.version)",
        "# The resolved runtime set this artifact was built from.",
        "# Install with: pip install -r <this file>"
    ) + $Frozen | Set-Content -Path $LockOut -Encoding UTF8
    Write-Host "    resolved set: $LockOut"
} else {
    Write-Host "WARNING: could not write $LockOut"
}

# ---- 3. backend entry point -------------------------------------------------
# PyInstaller wants a script. serve.py is the backend's own entry, but a
# windowed build has no console, so this stub gives both streams a file first
# and then runs serve.py's main().
$Entry = Join-Path $BuildDir "corvus_backend_main.py"
$EntrySource = @'
"""Frozen backend entry point. Generated by build-windows.ps1 - not used elsewhere."""
import multiprocessing
import os
import sys

# A windowed build has no console, so sys.stdout and sys.stderr are None.
# logging.basicConfig() writes to a stream, and a handler pointed at None
# silently drops every line - which leaves an operator whose backend failed to
# start with no way to find out why. Give both streams a file next to the rest
# of Corvus' state before anything imports logging.
if sys.stderr is None or sys.stdout is None:
    try:
        _dir = os.path.expanduser("~/.corvus")
        os.makedirs(_dir, exist_ok=True)
        _log = open(os.path.join(_dir, "corvus-gcs.log"), "a", encoding="utf-8",
                    buffering=1)
        if sys.stdout is None:
            sys.stdout = _log
        if sys.stderr is None:
            sys.stderr = _log
    except OSError:
        pass  # a read-only home must not stop the backend from starting

import serve  # noqa: E402 - must follow the stream fixup

if __name__ == "__main__":
    multiprocessing.freeze_support()
    sys.exit(serve.main())
'@
Set-Content -Path $Entry -Value $EntrySource -Encoding UTF8

# ---- 4. package -------------------------------------------------------------
# corvus/version.py resolves VERSION as parent.parent of its own __file__, and
# server.py resolves src/ the same way, so the three have to land beside the
# package inside the bundle - which is exactly where "path;." puts them.
Write-Host ">>> Running PyInstaller (the backend) ..."
if (Test-Path $AppDir) { Remove-Item -Recurse -Force $AppDir }
$BackendDist = Join-Path $BuildDir "backend-dist"
if (Test-Path $BackendDist) { Remove-Item -Recurse -Force $BackendDist }

# Only the plugins .gitignore lets back in, without their tests\: a plugin
# installed into plugins\ for development is not part of the release.
$PluginStage = Join-Path $BuildDir "plugins"
& $VenvPy (Join-Path $RepoDir "tools\bundle_plugins.py") $PluginStage
if ($LASTEXITCODE -ne 0) { throw "bundling the plugins failed" }

# Only what the app reads from assets: the logos and licences. The README and
# website screenshots stay in the repository.
$AssetStage = Join-Path $BuildDir "assets"
& $VenvPy (Join-Path $RepoDir "tools\bundle_assets.py") $AssetStage
if ($LASTEXITCODE -ne 0) { throw "bundling the assets failed" }

$PyiArgs = @(
    "--noconfirm", "--clean", "--windowed",
    "--name", "corvus-backend",
    "--distpath", $BackendDist,
    "--workpath", (Join-Path $BuildDir "work"),
    "--specpath", $BuildDir,
    "--paths", $RepoDir,
    "--add-data", "$(Join-Path $RepoDir 'VERSION');.",
    # The Sustainable Use License requires that anyone who receives a copy of
    # the software also receives a copy of its terms. The .app and the AppImage
    # have always shipped it; this bundle did not, which made the Windows
    # artifact the one that did not satisfy its own licence.
    "--add-data", "$(Join-Path $RepoDir 'LICENSE.md');.",
    "--add-data", "$(Join-Path $RepoDir 'src');src",
    "--add-data", "$AssetStage;assets",
    # The plugins that ship with Corvus. plugin_registry.py resolves this root
    # as a sibling of the package, the same way server.py resolves src\, so it
    # lands beside them. Operator plugins live in %USERPROFILE%\.corvus\plugins
    # and are never bundled.
    "--add-data", "$PluginStage;plugins",
    "--hidden-import", "serve",
    # corvus_link registers corvus_link.mavutil at import time, which the
    # analyser cannot see; collecting the package keeps the extension whole.
    "--collect-all", "corvus_link"
)
# lxml came in with pymavlink and nothing Corvus ships loads it
# (tools/bundle_trim.py).
$PyiArgs += @("--exclude-module", "lxml")
$PyiArgs += $Entry

& $VenvPy -m PyInstaller @PyiArgs
if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed" }

$BackendExe = Join-Path $BackendDist "corvus-backend\corvus-backend.exe"
if (-not (Test-Path $BackendExe)) { throw "expected $BackendExe, which PyInstaller did not produce" }

# The folder the operator gets: the interface beside the backend it starts.
New-Item -ItemType Directory -Force -Path $AppDir | Out-Null
Move-Item (Join-Path $BackendDist "corvus-backend") (Join-Path $AppDir "backend")

# The interface. rust-toolchain.toml in rust\corvus_gui pins the Rust it needs,
# so cargo runs from there and rustup fetches it on first use. The fonts and
# icons are cut out of src\vendor by its build.rs and the logos are read from
# assets\, so the executable carries them. No version literal: the window
# title asks the backend.
Write-Host ">>> Building the interface (rust\corvus_gui, release) ..."
$GuiLog = Join-Path $BuildDir "cargo-gui.log"
Push-Location (Join-Path $RepoDir "rust\corvus_gui")
try {
    $env:CARGO_TARGET_DIR = Join-Path $BuildDir "cargo-gui"
    & cargo build --release --locked *> $GuiLog
    if ($LASTEXITCODE -ne 0) {
        Get-Content $GuiLog -Tail 30
        throw "the interface did not build; log: $GuiLog"
    }
} finally {
    Pop-Location
}
$Exe = Join-Path $AppDir "$AppName.exe"
Copy-Item (Join-Path $BuildDir "cargo-gui\release\corvus-gcs.exe") $Exe

# the bundled Python's data files PyInstaller collects and the app never reads:
# lxml, pip and the pymavlink dialects but `all`.
Write-Host ">>> Trimming what the app never loads ..."
& $VenvPy (Join-Path $RepoDir "tools\bundle_trim.py") (Join-Path $AppDir "backend\_internal")
if ($LASTEXITCODE -ne 0) { throw "trimming the bundle failed" }

# ---- 5. verify --------------------------------------------------------------
# The failure this catches is the one that matters: a bundle that builds and
# then cannot import corvus_link or the backend on a machine without them.
# It runs against the bundled interpreter payload, not the build venv.
if (-not $NoVerify) {
    Write-Host ">>> Verifying the bundle ..."
    foreach ($needed in @("VERSION", "LICENSE.md", "src\index.html", "assets", "plugins")) {
        $p = Join-Path $AppDir "backend\_internal\$needed"
        if (-not (Test-Path $p)) { throw "bundle is missing $needed (looked at $p)" }
    }
    $stamped = (Get-Content (Join-Path $AppDir "backend\_internal\VERSION") -Raw).Trim()
    if ($stamped -ne $Version) {
        throw "bundled VERSION is '$stamped', expected '$Version'"
    }
    foreach ($exe in @($Exe, (Join-Path $AppDir "backend\corvus-backend.exe"))) {
        if (-not (Test-Path $exe)) { throw "bundle is missing $exe" }
    }
    Write-Host "    payload     : VERSION, LICENSE.md, src\, assets\, plugins\ present and in sync"
}

# ---- 6. zip -----------------------------------------------------------------
if ($Zip) {
    Write-Host ">>> Zipping to $Zipfile ..."
    if (Test-Path $Zipfile) { Remove-Item -Force $Zipfile }
    Compress-Archive -Path $AppDir -DestinationPath $Zipfile
}

Write-Host ""
Write-Host "=== done ==="
Write-Host "App: $AppDir"
if ($Zip) { Write-Host "Zip: $Zipfile" }
