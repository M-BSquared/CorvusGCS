#!/usr/bin/env bash
# Build Corvus GCS as a Linux AppImage (Ubuntu/Debian x86_64).
#
# Bundles a relocatable CPython (venv --copies + host stdlib + libpython) and
# the PyQt6/QtWebEngine wheels, then packs them with appimagetool. No conda on
# the build host; only stdlib python3 -m venv + pip are used.
#
# Version is read exclusively from the repo-root VERSION file — never hardcoded.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
BUILD_DIR="$REPO_DIR/build"
APPDIR="$BUILD_DIR/AppDir"
OUTPUT="$REPO_DIR/Corvus_GCS-${VERSION}-x86_64.AppImage"

# ---- preflight --------------------------------------------------------------
echo "=== CORVUS GCS — AppImage build ==="
echo "Repo:    $REPO_DIR"
echo "Version: $VERSION  (read from VERSION)"
echo "AppDir:  $APPDIR"
echo "Output:  $OUTPUT"
echo ""

command -v python3 >/dev/null 2>&1 || { echo "ERROR: python3 not found" >&2; exit 1; }

PY_MAJOR="$(python3 -c 'import sys; print(sys.version_info.major)')"
PY_MINOR="$(python3 -c 'import sys; print(sys.version_info.minor)')"
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 10 ]; }; then
    echo "ERROR: Python >= 3.10 required, found $PY_MAJOR.$PY_MINOR" >&2
    exit 1
fi
PY_MM="${PY_MAJOR}.${PY_MINOR}"

if [ "$(uname -m)" != "x86_64" ]; then
    echo "ERROR: this build targets x86_64; host arch is $(uname -m)" >&2
    exit 1
fi

if command -v wget >/dev/null 2>&1; then
    DOWNLOADER=wget
elif command -v curl >/dev/null 2>&1; then
    DOWNLOADER=curl
else
    echo "ERROR: wget or curl required" >&2
    exit 1
fi
download() {  # <url> <dest>
    local url="$1" dest="$2"
    if [ "$DOWNLOADER" = wget ]; then wget -q -O "$dest" "$url"
    else curl -fsSL -o "$dest" "$url"; fi
}

# host interpreter layout (detected, never assumed)
PY_STDLIB="$(python3 -c 'import sysconfig; print(sysconfig.get_path("stdlib"))')"
PY_LIBDIR="$(python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"

# ---- cleanup trap: keep $OUTPUT + cached appimagetool, drop $APPDIR on failure
SUCCESS=0
cleanup() {
    if [ "${SUCCESS:-0}" -ne 1 ]; then
        rm -rf "$APPDIR" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ---- fresh AppDir -----------------------------------------------------------
echo ">>> Preparing AppDir ..."
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr"
rm -f "$OUTPUT"

# ---- 1. relocatable venv (--copies the interpreter binary) ------------------
echo ">>> Creating venv (python$PY_MM) in AppDir/usr ..."
python3 -m venv --copies "$APPDIR/usr"

# ---- 2. fix portability: copy host stdlib + lib-dynload + libpython ---------
# pyvenv.cfg `home =` points at the build host and breaks on another machine.
# PYTHONHOME (set in AppRun) repoints the interpreter at AppDir/usr; to make
# that resolve we copy the host stdlib (with lib-dynload) under the venv's
# lib/pythonX.Y and the shared libpython next to it. The venv's own
# site-packages is preserved: the host stdlib dir has no site-packages subdir,
# and rsync below excludes dist-packages to avoid leaking system debs.
echo ">>> Bundling stdlib + lib-dynload from $PY_STDLIB ..."
rsync -a --exclude 'dist-packages' "$PY_STDLIB/" "$APPDIR/usr/lib/python$PY_MM/"

echo ">>> Bundling libpython${PY_MM}.so.1.0 from $PY_LIBDIR ..."
if [ -f "$PY_LIBDIR/libpython${PY_MM}.so.1.0" ]; then
    cp -a "$PY_LIBDIR/libpython${PY_MM}.so.1.0" "$APPDIR/usr/lib/"
else
    # Static-libpython builds (e.g. Debian/Ubuntu) embed it in the executable;
    # the .so is still bundled when present so --enable-shared hosts work too.
    echo "WARNING: libpython${PY_MM}.so.1.0 not found at $PY_LIBDIR (host likely static-libpython)"
fi

# ---- 3. pip-install deps (the env from environment.yml, minus conda) --------
PIP_LOG="$BUILD_DIR/pip-install.log"
mkdir -p "$BUILD_DIR"
: > "$PIP_LOG"
echo ">>> Upgrading pip ..."
if ! "$APPDIR/usr/bin/python3" -m pip install --upgrade pip >>"$PIP_LOG" 2>&1; then
    echo "ERROR: pip self-upgrade failed; log: $PIP_LOG" >&2
    exit 1
fi
echo ">>> Installing pymavlink paramiko pyserial PyQt6 PyQt6-WebEngine ..."
if ! "$APPDIR/usr/bin/python3" -m pip install \
        pymavlink paramiko pyserial PyQt6 PyQt6-WebEngine >>"$PIP_LOG" 2>&1; then
    echo "ERROR: dependency install failed; log: $PIP_LOG" >&2
    exit 1
fi
echo "    (deps install log: $PIP_LOG)"

# ---- 4. copy app code into AppDir root -------------------------------------
# corvus/version.py does parent.parent/VERSION; corvus/server.py + app.py do
# parent.parent/src — so VERSION, corvus/, src/ must be siblings at AppDir root.
echo ">>> Copying application code ..."
rsync -a --exclude '__pycache__' "$REPO_DIR/corvus/" "$APPDIR/corvus/"
rsync -a --exclude '__pycache__' "$REPO_DIR/src/" "$APPDIR/src/"
rsync -a "$REPO_DIR/assets/" "$APPDIR/assets/"
cp -a "$REPO_DIR/VERSION" "$APPDIR/VERSION"
# The Sustainable Use License requires that anyone who receives a copy of
# the software also receives a copy of its terms, so the licence ships in
# the bundle rather than only in the repository.
cp -a "$REPO_DIR/LICENSE.md" "$APPDIR/LICENSE.md"
if [ -f "$REPO_DIR/serve.py" ]; then
    cp -a "$REPO_DIR/serve.py" "$APPDIR/serve.py"
fi

# ---- 5. generate AppRun (no version literal; pythonX.Y resolved at runtime) --
echo ">>> Generating AppRun + desktop entry ..."
cat > "$APPDIR/AppRun" <<'APPRUN_EOF'
#!/usr/bin/env bash
set -euo pipefail

# AppImage type-2 sets $APPDIR to the mount point; fall back to the script dir
# so AppRun is also runnable directly from an extracted squashfs-root.
if [ -z "${APPDIR:-}" ]; then
    APPDIR="$(cd "$(dirname "$0")" && pwd)"
fi
cd "$APPDIR"

# Resolve the bundled pythonX.Y directory at runtime (X.Y was fixed at build).
shopt -s nullglob
PY_BASE=""
for d in "$APPDIR/usr/lib"/python3.*/; do PY_BASE="$d"; break; done
shopt -u nullglob
if [ -z "$PY_BASE" ] || [ ! -d "$PY_BASE" ]; then
    echo "corvus-gcs: bundled python3.x not found under $APPDIR/usr/lib" >&2
    exit 1
fi
PY_SITE="${PY_BASE}site-packages"

# PYTHONHOME overrides the stale pyvenv.cfg `home =` so the bundled interpreter
# finds its stdlib under $APPDIR/usr/lib/pythonX.Y regardless of mount path.
# PYTHONPATH re-adds the venv site-packages because PYTHONHOME bypasses venv
# site-discovery, and AppDir root so `import corvus` resolves.
export PYTHONHOME="$APPDIR/usr"
export PYTHONPATH="$APPDIR:$PY_SITE"

# Qt6 is bundled inside the PyQt6 wheel (site-packages/PyQt6/Qt6/). Point the Qt
# plugin/resource loaders and the dynamic linker at it so the bundled Qt is
# used despite the random /tmp/.mount_<XXXX> AppImage mount path.
PYQT6_QT6="$PY_SITE/PyQt6/Qt6"
export QT_PLUGIN_PATH="$PYQT6_QT6/plugins"
export QT_QPA_PLATFORM_PLUGIN_PATH="$PYQT6_QT6/plugins"
export QTWEBENGINE_RESOURCES_PATH="$PYQT6_QT6/resources"
if [ -d "$PYQT6_QT6/resources/qtwebengine_dictionaries" ]; then
    export QTWEBENGINE_DICTIONARIES_PATH="$PYQT6_QT6/resources/qtwebengine_dictionaries"
fi
export LD_LIBRARY_PATH="$PYQT6_QT6/lib:$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# AppImage cannot use the Chromium setuid sandbox; append the disabling flags.
# app.py uses os.environ.setdefault, so its own Vulkan/swiftshader flags stay.
export QTWEBENGINE_CHROMIUM_FLAGS="${QTWEBENGINE_CHROMIUM_FLAGS:-} --no-sandbox --disable-gpu-sandbox --disable-setuid-sandbox --disable-dev-shm-usage"

# Ensure a writable HOME for Qt/Chromium profile+cache when none is set/unwritable.
if [ -z "${HOME:-}" ] || [ ! -w "${HOME:-}" ]; then
    export HOME="${XDG_CACHE_HOME:-${TMPDIR:-/tmp}}/corvus-gcs"
    mkdir -p "$HOME" 2>/dev/null || true
fi

# exec replaces the shell so SIGINT/SIGTERM reach python directly, letting
# corvus/app.py's signal handlers tear down cleanly (no zombies, no leaked sockets).
exec "$APPDIR/usr/bin/python3" "$APPDIR/corvus/app.py" "$@"
APPRUN_EOF
chmod 0755 "$APPDIR/AppRun"

# ---- 6. desktop entry (no version literal) ---------------------------------
cat > "$APPDIR/corvus-gcs.desktop" <<'DESKTOP_EOF'
[Desktop Entry]
Name=Corvus GCS
Comment=Ground Control Station for PX4-based aircraft
Exec=corvus-gcs
Icon=corvus-gcs
Type=Application
Categories=Utility;Science;
Terminal=false
DESKTOP_EOF

# ---- 7. icon (256x256 preferred; fall back to the logo as-is) --------------
if [ ! -f "$REPO_DIR/assets/CorvusGCS_logo.png" ]; then
    echo "ERROR: assets/CorvusGCS_logo.png not found" >&2
    exit 1
fi
if command -v convert >/dev/null 2>&1; then
    if ! convert "$REPO_DIR/assets/CorvusGCS_logo.png" -resize 256x256 \
            "$APPDIR/corvus-gcs.png" 2>/dev/null; then
        cp -a "$REPO_DIR/assets/CorvusGCS_logo.png" "$APPDIR/corvus-gcs.png"
    fi
else
    echo "WARNING: ImageMagick 'convert' not found; using logo as-is (not 256x256)"
    cp -a "$REPO_DIR/assets/CorvusGCS_logo.png" "$APPDIR/corvus-gcs.png"
fi

# ---- 8. obtain appimagetool -------------------------------------------------
APPIMAGETOOL="$BUILD_DIR/appimagetool-x86_64.AppImage"
APPIMAGETOOL_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage"
if [ -x "$APPIMAGETOOL" ]; then
    echo ">>> Using cached appimagetool"
elif command -v appimagetool >/dev/null 2>&1; then
    APPIMAGETOOL="$(command -v appimagetool)"
    echo ">>> Using appimagetool from PATH"
else
    echo ">>> Downloading appimagetool ..."
    download "$APPIMAGETOOL_URL" "$APPIMAGETOOL"
    chmod +x "$APPIMAGETOOL"
fi

# ---- 9. pack ----------------------------------------------------------------
# Three execution tiers. On a normal laptop the direct run works (FUSE mounts
# the appimagetool AppImage). Sandboxes without FUSE fall back to
# APPIMAGE_EXTRACT_AND_RUN=1. Sandboxes where binfmt_misc routes AppImage exec
# through a broken appimagelauncher interpreter need the third tier: manually
# unsquashfs the appimagetool AppImage and run its AppRun directly (a plain
# ELF, not intercepted by binfmt).
AIM_LOG="$BUILD_DIR/appimagetool.log"
: > "$AIM_LOG"

run_appimagetool_extracted() {  # <tool_appimage> <args...>
    local tool="$1"; shift
    local workdir="$BUILD_DIR/appimagetool-extracted"
    local sqfs="$BUILD_DIR/appimagetool.squashfs"
    rm -rf "$workdir" "$sqfs"
    # Find the embedded squashfs 4.0 superblock and slice the file from there.
    local result
    result="$(python3 - "$tool" "$sqfs" <<'PY' || true
import struct, re, sys
data = open(sys.argv[1], 'rb').read()
for m in re.finditer(b'hsqs', data):
    off = m.start()
    if off + 56 > len(data):
        continue
    sb = data[off:off + 56]
    s_major, s_minor = struct.unpack('<HH', sb[28:32])
    block_size = struct.unpack('<I', sb[12:16])[0]
    block_log = struct.unpack('<H', sb[22:24])[0]
    compression = struct.unpack('<H', sb[20:22])[0]
    bytes_used = struct.unpack('<Q', sb[40:48])[0]
    if (s_major == 4 and s_minor == 0 and block_size == (1 << block_log)
            and compression in (1, 2, 3, 4, 5, 6)
            and 0 < bytes_used <= len(data) - off):
        open(sys.argv[2], 'wb').write(data[off:])
        print("OK %d" % off)
        sys.exit(0)
print("FAIL")
sys.exit(1)
PY
)"
    case "$result" in
        OK\ *) ;;
        *) echo "ERROR: no valid squashfs superblock in $tool ($result)" >&2; return 1 ;;
    esac
    unsquashfs -force -d "$workdir" "$sqfs" || return 1
    [ -x "$workdir/AppRun" ] || { echo "ERROR: extracted AppRun missing" >&2; return 1; }
    ARCH=x86_64 "$workdir/AppRun" "$@"
}

echo ">>> Packaging AppImage ..."
if ! ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
    echo ">>> Direct run failed; retrying with APPIMAGE_EXTRACT_AND_RUN=1 ..."
    if ! APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
        if command -v unsquashfs >/dev/null 2>&1; then
            echo ">>> Extract-and-run failed; manually unsquashfs-ing appimagetool (bypasses FUSE + binfmt) ..."
            if ! run_appimagetool_extracted "$APPIMAGETOOL" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
                echo "ERROR: appimagetool failed; log: $AIM_LOG" >&2
                tail -n 20 "$AIM_LOG" >&2 || true
                exit 1
            fi
        else
            echo "ERROR: appimagetool failed and unsquashfs is not available; log: $AIM_LOG" >&2
            tail -n 20 "$AIM_LOG" >&2 || true
            exit 1
        fi
    fi
fi

SUCCESS=1
echo ""
echo ">>> Built: $OUTPUT"
echo "    (appimagetool log: $AIM_LOG)"
