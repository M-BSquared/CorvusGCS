# =============================================================================
# Corvus GCS - build the Windows distributable.
#
# No PyInstaller. The interface is the Rust app (Cargo.toml and src/, built
# with cargo, one .exe). Until the backend is ported to Rust it starts
# python\serve.py as its child, and that runs on python.org's embeddable
# CPython: a zip of the interpreter that needs no installer, laid out beside the
# packages pip resolved and the backend's own files. Nothing is frozen or
# unpacked at launch, so it starts as fast as a folder can. Once the backend is
# part of the Rust workspace (see src\BACKEND_PORT.md) the whole backend\
# folder and every Python step below disappear and the folder is the .exe.
#
#   Corvus GCS\Corvus GCS.exe      the interface
#   Corvus GCS\backend\             serve.py, corvus\, src\, plugins\, assets\
#   Corvus GCS\backend\python\      the embedded interpreter + site-packages
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
# A folder and not a self-extracting single file: unpacking into a temp
# directory on every launch costs seconds the field laptop should not spend.
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
    "VERSION", "LICENSE.md", "assets", "assets\CorvusGCS_logo.png",
    "python\pyproject.toml", "python\corvus", "python\src", "python\plugins", "python\serve.py",
    "Cargo.toml", "src\main.rs", "src\link\Cargo.toml")) {
    if (-not (Test-Path (Join-Path $RepoDir $required))) { $Missing += $required }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found; install Rust from https://rustup.rs and run this again."
}
if ($Missing.Count -gt 0) {
    throw ("the bundle is assembled from files that are not in the repo: " +
           ($Missing -join ", ") + " - restore them (git checkout -- <path>) and run this again.")
}


# ---- 1. build interpreter ----------------------------------------------------
# Only used to resolve and build the packages (pip, maturin); the shipped
# interpreter is the embeddable zip below. A conda interpreter is rejected for
# the same reason build-macos-app.sh rejects it.
if (-not $Python) {
    $Python = (Get-Command python -ErrorAction SilentlyContinue).Source
}
if (-not $Python) {
    throw "No python.exe found. Install Python 3.12+ from python.org, or pass -Python <path>."
}
$pyInfo = & $Python -c @"
import sys, json
print(json.dumps({
    'full': '%d.%d.%d' % sys.version_info[:3],
    'version': '%d.%d' % sys.version_info[:2],
    'conda': 'conda' in sys.version.lower() or 'Continuum' in sys.version,
    'bits': 64 if sys.maxsize > 2**32 else 32,
}))
"@ | ConvertFrom-Json

if ($pyInfo.conda) { throw "$Python is a conda interpreter; use a python.org build." }
if ($pyInfo.bits -ne 64) { throw "$Python is 32-bit. Corvus ships x64 only." }
$pyParts = $pyInfo.version.Split(".")
if ([int]$pyParts[0] -lt 3 -or ([int]$pyParts[0] -eq 3 -and [int]$pyParts[1] -lt 12)) {
    throw "$Python is Python $($pyInfo.version); Corvus requires >= 3.12."
}
Write-Host ">>> Build interpreter: $Python (Python $($pyInfo.full), x64)"

# ---- 2. packages -------------------------------------------------------------
$VenvDir = Join-Path $BuildDir "venv"
$VenvPy = Join-Path $VenvDir "Scripts\python.exe"
New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
$Pyproject = Join-Path $RepoDir "python\pyproject.toml"
$LinkSrc = Join-Path $RepoDir "src\link"

# The stamp covers what the venv is made from, so a second build with the same
# pyproject.toml, corvus_link sources and requirements installs nothing.
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
$hashInputs = @($Pyproject) + (Get-ChildItem -Recurse -File $LinkSrc |
    Where-Object { $_.FullName -notmatch '\\target\\' } | ForEach-Object { $_.FullName })
if ($Requirements) { $hashInputs += $Requirements }
$Stamp = (($hashInputs | Sort-Object | ForEach-Object { (Get-FileHash $_ -Algorithm SHA256).Hash }) -join "") + $pyInfo.full
$StampFile = Join-Path $VenvDir ".corvus-deps"
$Have = if (Test-Path $StampFile) { (Get-Content $StampFile -Raw).Trim() } else { "" }
$StampHash = [System.BitConverter]::ToString(
    [System.Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes($Stamp))).Replace("-", "")

$LockOut = Join-Path $DistDir "Corvus_GCS-$Version-windows-x64.lock"
$PipLog = Join-Path $BuildDir "pip.log"
if (-not (Test-Path $VenvPy) -or $Have -ne $StampHash) {
    Write-Host ">>> Installing $DepsLabel and corvus_link (maturin, release) ..."
    if (Test-Path $VenvDir) { Remove-Item -Recurse -Force $VenvDir }
    & $Python -m venv $VenvDir
    if ($LASTEXITCODE -ne 0) { throw "could not create the build venv at $VenvDir" }
    # pip >= 25.1 is what reads [dependency-groups]; a fresh venv ships older.
    & $VenvPy -m pip install --upgrade pip *>> $PipLog
    if ($LASTEXITCODE -eq 0) { & $VenvPy -m pip install @DepsArgs *>> $PipLog }
    # corvus_link is Rust (abi3, so it runs on any CPython 3.12.x).
    if ($LASTEXITCODE -eq 0) { & $VenvPy -m pip install $LinkSrc *>> $PipLog }
    if ($LASTEXITCODE -ne 0) {
        Get-Content $PipLog -Tail 40
        throw "dependency install failed; log: $PipLog"
    }
    $StampHash | Set-Content $StampFile
} else {
    Write-Host ">>> Packages unchanged since the last build; reusing $VenvDir"
}

New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
$Frozen = & $VenvPy -m pip freeze --all --exclude-editable --exclude corvus-link
if ($LASTEXITCODE -eq 0) {
    @(
        "# Corvus GCS $Version - Windows x64, python $($pyInfo.full)",
        "# The resolved runtime set this artifact was built from.",
        "# Install with: pip install -r <this file>"
    ) + $Frozen | Set-Content -Path $LockOut -Encoding UTF8
    Write-Host "    resolved set: $LockOut"
}

# ---- 3. the embeddable interpreter -------------------------------------------
# python.org's zip, cached in build\ (the build downloads once, the app never
# needs the network). 3.12 stops getting Windows binaries once it is
# security-only, so a host micro version without a zip falls back to
# $EmbedFallback, which only has to be a 3.12.x release (corvus_link is abi3).
$EmbedFallback = "3.12.10"
$EmbedZip = $null
foreach ($v in @($env:CORVUS_EMBED_VERSION, $pyInfo.full, $EmbedFallback) | Where-Object { $_ }) {
    $cached = Join-Path $BuildDir "python-$v-embed-amd64.zip"
    if (Test-Path $cached) { $EmbedZip = $cached; break }
    try {
        Invoke-WebRequest "https://www.python.org/ftp/python/$v/python-$v-embed-amd64.zip" -OutFile $cached -UseBasicParsing
        $EmbedZip = $cached; break
    } catch {
        Remove-Item -Force $cached -ErrorAction SilentlyContinue
        Write-Host "    no embeddable zip for $v, trying the next"
    }
}
if (-not $EmbedZip) { throw "could not obtain python.org's embeddable CPython" }
Write-Host ">>> Interpreter: $(Split-Path -Leaf $EmbedZip)"

# ---- 4. assemble the folder ---------------------------------------------------
if (Test-Path $AppDir) { Remove-Item -Recurse -Force $AppDir }
$Backend = Join-Path $AppDir "backend"
$PyHome = Join-Path $Backend "python"
New-Item -ItemType Directory -Force -Path $PyHome | Out-Null
Expand-Archive -Path $EmbedZip -DestinationPath $PyHome -Force

# The ._pth file is the embeddable interpreter's whole search path (it ignores
# PYTHONPATH and does not add the script's folder): the stdlib zip, itself,
# the packages, and the backend root so `import corvus` resolves.
$Pth = Get-ChildItem $PyHome -Filter "python*._pth" | Select-Object -First 1
if (-not $Pth) { throw "the embeddable zip has no python*._pth" }
$StdZip = [IO.Path]::GetFileNameWithoutExtension($Pth.Name) + ".zip"
@($StdZip, ".", "Lib\site-packages", "..") | Set-Content -Path $Pth.FullName -Encoding ASCII

# The packages are copied from the build venv, minus what only pip and the
# build need.
$Site = Join-Path $PyHome "Lib\site-packages"
New-Item -ItemType Directory -Force -Path $Site | Out-Null
$VenvSite = Join-Path $VenvDir "Lib\site-packages"
& robocopy $VenvSite $Site /E /NFL /NDL /NJH /NJS /NP /XD __pycache__ pip pip-* setuptools _distutils_hack | Out-Null
if ($LASTEXITCODE -ge 8) { throw "copying the packages failed (robocopy $LASTEXITCODE)" }
$global:LASTEXITCODE = 0

# lxml, pip and the pymavlink dialects but `all` (tools\bundle_trim.py), which
# also fails when corvus_link or the `all` dialect is gone.
Write-Host ">>> Trimming what the app never loads ..."
& $VenvPy (Join-Path $RepoDir "python\tools\bundle_trim.py") $Site
if ($LASTEXITCODE -ne 0) { throw "trimming the bundle failed" }

# The backend's own files, beside each other: corvus\version.py resolves
# VERSION and server.py resolves src\ as parent.parent of their package.
Write-Host ">>> Copying application code ..."
function Copy-Tree($from, $to) {
    & robocopy $from $to /E /NFL /NDL /NJH /NJS /NP /XD __pycache__ | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "copying $from failed (robocopy $LASTEXITCODE)" }
    $global:LASTEXITCODE = 0
}
Copy-Tree (Join-Path $RepoDir "python\corvus") (Join-Path $Backend "corvus")
Copy-Tree (Join-Path $RepoDir "python\src") (Join-Path $Backend "src")
# Only the plugins .gitignore lets back in, without their tests\.
& $VenvPy (Join-Path $RepoDir "python\tools\bundle_plugins.py") (Join-Path $Backend "plugins")
if ($LASTEXITCODE -ne 0) { throw "bundling the plugins failed" }
# Only what the app reads from assets: the logos and licences.
& $VenvPy (Join-Path $RepoDir "python\tools\bundle_assets.py") (Join-Path $Backend "assets")
if ($LASTEXITCODE -ne 0) { throw "bundling the assets failed" }
Copy-Item (Join-Path $RepoDir "python\serve.py") (Join-Path $Backend "serve.py")
Copy-Item (Join-Path $RepoDir "VERSION") (Join-Path $Backend "VERSION")
# The Sustainable Use License requires that anyone who receives a copy of the
# software also receives a copy of its terms.
Copy-Item (Join-Path $RepoDir "LICENSE.md") (Join-Path $Backend "LICENSE.md")
Copy-Item (Join-Path $RepoDir "LICENSE.md") (Join-Path $AppDir "LICENSE.md")

# ---- 5. the interface ---------------------------------------------------------
# rust-toolchain.toml at the repository root pins the Rust it needs, so cargo
# runs from there and rustup fetches it on first use. The fonts and icons are
# cut out of assets\ by its build.rs, so the executable carries them. The
# target folder persists in build\, so a second build only recompiles what
# changed.
Write-Host ">>> Building the interface (Cargo.toml and src\, release) ..."
$GuiLog = Join-Path $BuildDir "cargo-gui.log"
Push-Location $RepoDir
try {
    $env:CARGO_TARGET_DIR = Join-Path $BuildDir "cargo-gui"
    & cargo build --release --locked --bin corvus-gcs *> $GuiLog
    if ($LASTEXITCODE -ne 0) {
        Get-Content $GuiLog -Tail 30
        throw "the interface did not build; log: $GuiLog"
    }
} finally {
    Pop-Location
}
$Exe = Join-Path $AppDir "$AppName.exe"
Copy-Item (Join-Path $BuildDir "cargo-gui\release\corvus-gcs.exe") $Exe

# ---- 6. verify ----------------------------------------------------------------
# The failure this catches is a folder that builds and then cannot import
# corvus_link or the backend on a machine without them. It runs the bundled
# interpreter itself, not the build venv.
if (-not $NoVerify) {
    Write-Host ">>> Verifying the bundle ..."
    foreach ($needed in @("VERSION", "LICENSE.md", "serve.py", "src\index.html", "assets", "plugins", "corvus", "python\python.exe")) {
        $p = Join-Path $Backend $needed
        if (-not (Test-Path $p)) { throw "bundle is missing $needed (looked at $p)" }
    }
    $stamped = (Get-Content (Join-Path $Backend "VERSION") -Raw).Trim()
    if ($stamped -ne $Version) { throw "bundled VERSION is '$stamped', expected '$Version'" }
    $BundledPy = Join-Path $PyHome "python.exe"
    & $BundledPy -I -c "import corvus_link, corvus.version as v, sys; sys.exit(0 if v.__version__ else 1)"
    if ($LASTEXITCODE -ne 0) { throw "the bundled interpreter cannot import corvus_link and corvus" }
    Write-Host "    payload     : VERSION, LICENSE.md, serve.py, src\, assets\, plugins\ present and importable"
}

# ---- 7. zip -------------------------------------------------------------------
if ($Zip) {
    Write-Host ">>> Zipping to $Zipfile ..."
    if (Test-Path $Zipfile) { Remove-Item -Force $Zipfile }
    Compress-Archive -Path $AppDir -DestinationPath $Zipfile -CompressionLevel Optimal
}

Write-Host ""
Write-Host "=== done ==="
Write-Host "App: $AppDir"
if ($Zip) { Write-Host "Zip: $Zipfile" }
