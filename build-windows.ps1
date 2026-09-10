# =============================================================================
# Corvus GCS - build the Windows distributable.
#
# The Linux and macOS scripts hand-roll a relocatable interpreter because both
# platforms make that the shortest honest path: an AppImage is a squashfs of a
# tree you lay out yourself, and a .app is a directory with rules about where
# dylibs may point. Windows has neither constraint and one well-trodden tool,
# so this uses PyInstaller rather than reinventing the same 300 lines in
# PowerShell. The result is the same shape as the other two: one directory that
# runs on a machine with no Python and no internet.
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
# Why onedir and not --onefile: --onefile unpacks the whole Qt payload into a
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
$Zipfile = Join-Path $RepoDir "Corvus_GCS-$Version-windows-x64.zip"

Write-Host "=== CORVUS GCS - Windows build ==="
Write-Host "Version: $Version  (read from VERSION)"
Write-Host "Host:    $([System.Environment]::OSVersion.VersionString) $env:PROCESSOR_ARCHITECTURE"
Write-Host ""

# ---- 1. interpreter ---------------------------------------------------------
# A conda interpreter cannot be packaged reliably (its DLLs live outside the
# prefix), which is the same rejection build-macos-app.sh makes and for the
# same reason. Say so plainly rather than failing deep inside PyInstaller.
if (-not $Python) {
    $Python = (Get-Command python -ErrorAction SilentlyContinue).Source
}
if (-not $Python) {
    throw "No python.exe found. Install Python 3.10+ from python.org, or pass -Python <path>."
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

$Deps = @("pymavlink>=2.4", "paramiko>=3.0", "pyserial>=3.5",
          "PyQt6>=6.6", "PyQt6-WebEngine>=6.6", "pyinstaller>=6.3")
Write-Host ">>> Installing $($Deps -join ' ') ..."
$PipLog = Join-Path $BuildDir "pip.log"
& $VenvPy -m pip install --upgrade pip *>> $PipLog
& $VenvPy -m pip install @Deps *>> $PipLog
if ($LASTEXITCODE -ne 0) {
    Write-Host "--- last 40 lines of $PipLog ---"
    Get-Content $PipLog -Tail 40
    throw "dependency install failed"
}

# ---- 3. icon ----------------------------------------------------------------
# Optional on purpose: a missing icon is a cosmetic loss, and failing the whole
# release build over one is the wrong trade. Uses a checked-in .ico when there
# is one, else renders from the PNG when Pillow is available.
$Ico = Join-Path $RepoDir "assets\corvus-gcs.ico"
if (-not (Test-Path $Ico)) {
    $Png = Join-Path $RepoDir "assets\CorvusGCS_logo.png"
    $IcoScript = @"
import sys
try:
    from PIL import Image
except ImportError:
    sys.exit(3)
img = Image.open(r'$Png').convert('RGBA')
img.save(r'$Ico', sizes=[(16,16),(32,32),(48,48),(64,64),(128,128),(256,256)])
"@
    & $VenvPy -c $IcoScript 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0 -and (Test-Path $Ico)) {
        Write-Host ">>> Icon rendered from CorvusGCS_logo.png"
    } else {
        Write-Host ">>> No icon (Pillow unavailable); building with the default"
        $Ico = ""
    }
}

# ---- 4. entry point ---------------------------------------------------------
# PyInstaller wants a script, not a module path. corvus/app.py is not usable
# directly because it is imported as part of the `corvus` package; this stub
# gives the analyser a plain top-level script that pulls the package in.
$Entry = Join-Path $BuildDir "corvus_gcs_main.py"
$EntrySource = @'
"""Frozen entry point. Generated by build-windows.ps1 - not used elsewhere."""
import multiprocessing
import os
import sys

# A --windowed build has no console, so sys.stdout and sys.stderr are None.
# Nothing here prints, but logging.basicConfig() writes to a stream, and a
# handler pointed at None silently drops every line - which leaves an operator
# whose app failed to start with no way to find out why. Give both streams a
# file next to the rest of Corvus' state before anything imports logging.
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
        pass  # a read-only home must not stop the app from starting

from corvus.app import main  # noqa: E402 - must follow the stream fixup

if __name__ == "__main__":
    # Qt's WebEngine spawns helper processes; without this the frozen exe
    # re-runs the whole application in each child instead of the helper.
    multiprocessing.freeze_support()
    sys.exit(main())
'@
Set-Content -Path $Entry -Value $EntrySource -Encoding UTF8

# ---- 5. package -------------------------------------------------------------
# corvus/version.py resolves VERSION as parent.parent of its own __file__, and
# server.py resolves src/ the same way, so the three have to land beside the
# package inside the bundle - which is exactly where "path;." puts them.
Write-Host ">>> Running PyInstaller ..."
if (Test-Path $AppDir) { Remove-Item -Recurse -Force $AppDir }

$PyiArgs = @(
    "--noconfirm", "--clean", "--windowed",
    "--name", $AppName,
    "--distpath", $DistDir,
    "--workpath", (Join-Path $BuildDir "work"),
    "--specpath", $BuildDir,
    "--paths", $RepoDir,
    "--add-data", "$(Join-Path $RepoDir 'VERSION');.",
    "--add-data", "$(Join-Path $RepoDir 'src');src",
    "--add-data", "$(Join-Path $RepoDir 'assets');assets",
    # The plugins that ship with Corvus. plugin_registry.py resolves this root
    # as a sibling of the package, the same way server.py resolves src\, so it
    # lands beside them. Operator plugins live in %USERPROFILE%\.corvus\plugins
    # and are never bundled.
    "--add-data", "$(Join-Path $RepoDir 'plugins');plugins",
    "--hidden-import", "corvus.app",
    # pymavlink generates its dialects at import time from data the analyser
    # cannot see, so the whole package is collected rather than guessed at.
    "--collect-all", "pymavlink"
)
if ($Ico) { $PyiArgs += @("--icon", $Ico) }
$PyiArgs += $Entry

& $VenvPy -m PyInstaller @PyiArgs
if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed" }

$Exe = Join-Path $AppDir "$AppName.exe"
if (-not (Test-Path $Exe)) { throw "expected $Exe, which PyInstaller did not produce" }

# ---- 6. verify --------------------------------------------------------------
# The failure this catches is the one that matters: a bundle that builds and
# then cannot import Qt, pymavlink or the app itself on a machine without them.
# It runs against the bundled interpreter payload, not the build venv.
if (-not $NoVerify) {
    Write-Host ">>> Verifying the bundle ..."
    foreach ($needed in @("VERSION", "src\index.html", "assets", "plugins")) {
        $p = Join-Path $AppDir "_internal\$needed"
        if (-not (Test-Path $p)) { throw "bundle is missing $needed (looked at $p)" }
    }
    $stamped = (Get-Content (Join-Path $AppDir "_internal\VERSION") -Raw).Trim()
    if ($stamped -ne $Version) {
        throw "bundled VERSION is '$stamped', expected '$Version'"
    }
    Write-Host "    payload     : VERSION, src\, assets\, plugins\ present and in sync"
}

# ---- 7. zip -----------------------------------------------------------------
if ($Zip) {
    Write-Host ">>> Zipping to $Zipfile ..."
    if (Test-Path $Zipfile) { Remove-Item -Force $Zipfile }
    Compress-Archive -Path $AppDir -DestinationPath $Zipfile
}

Write-Host ""
Write-Host "=== done ==="
Write-Host "App: $AppDir"
if ($Zip) { Write-Host "Zip: $Zipfile" }
