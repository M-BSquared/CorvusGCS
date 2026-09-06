#!/usr/bin/env bash
# Build Corvus GCS as a macOS application bundle (.app, optionally a .dmg).
#
# Mirrors build-appimage.sh: a relocatable CPython (venv --copies + host stdlib
# + the framework's Python dylib) plus the PyQt6/QtWebEngine wheels, packed into
# a standard .app layout. No conda on the build host; only a framework python3
# with `-m venv` + pip is used.
#
# Version is read exclusively from the repo-root VERSION file — never hardcoded.
#
# Usage:
#   ./build-macos-app.sh [--dmg] [--python /path/to/python3] [--no-verify]
#
# Env:
#   CORVUS_PYTHON      same as --python
#   CODESIGN_IDENTITY  Developer ID to sign with (default: "-", ad-hoc)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
BUILD_DIR="$REPO_DIR/build"
DIST_DIR="$REPO_DIR/dist"
APP_NAME="Corvus GCS"
APP="$DIST_DIR/$APP_NAME.app"
BUNDLE_ID="de.unibw.corvus.gcs"
ARCH="$(uname -m)"
DMG="$REPO_DIR/Corvus_GCS-${VERSION}-macOS-${ARCH}.dmg"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"

MAKE_DMG=0
VERIFY=1
PYBIN="${CORVUS_PYTHON:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --dmg)        MAKE_DMG=1 ;;
        --no-verify)  VERIFY=0 ;;
        --python)     PYBIN="${2:?--python needs a path}"; shift ;;
        -h|--help)
            sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

# ---- preflight --------------------------------------------------------------
echo "=== CORVUS GCS — macOS .app build ==="
echo "Repo:    $REPO_DIR"
echo "Version: $VERSION  (read from VERSION)"
echo "Arch:    $ARCH"
echo "Bundle:  $APP"
echo ""

[ "$(uname -s)" = "Darwin" ] || { echo "ERROR: this build must run on macOS" >&2; exit 1; }
for tool in rsync install_name_tool otool codesign sips iconutil; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: '$tool' not found — install the Xcode command line tools:" >&2
        echo "       xcode-select --install" >&2
        exit 1
    }
done

# ---- pick a framework CPython ----------------------------------------------
# A .app needs a *framework* build: the interpreter re-execs through
# Python.app/Contents/MacOS/Python (the GUI stub), which is what lets a Qt
# window own a Dock icon and a menu bar. Conda pythons are not framework
# builds and are not relocatable into a bundle, so they are rejected.
py_ok() {  # <path> -> 0 if usable
    local p="$1"
    [ -x "$p" ] || return 1
    "$p" - <<'PY' >/dev/null 2>&1 || return 1
import sys, sysconfig
assert sys.version_info >= (3, 10), sys.version
assert sysconfig.get_config_var("PYTHONFRAMEWORK"), "not a framework build"
assert "conda" not in sys.prefix.lower(), "conda interpreter"
PY
}

if [ -n "$PYBIN" ]; then
    py_ok "$PYBIN" || { echo "ERROR: $PYBIN is not a usable framework python >= 3.10" >&2; exit 1; }
else
    for cand in \
        /Library/Frameworks/Python.framework/Versions/3.12/bin/python3 \
        /Library/Frameworks/Python.framework/Versions/3.11/bin/python3 \
        /Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
        /opt/homebrew/bin/python3.12 /opt/homebrew/bin/python3.11 /opt/homebrew/bin/python3.13 \
        /usr/local/bin/python3.12 /usr/local/bin/python3.11 /usr/local/bin/python3.13 \
        "$(command -v python3 || true)"
    do
        if [ -n "$cand" ] && py_ok "$cand"; then PYBIN="$cand"; break; fi
    done
fi
[ -n "$PYBIN" ] || {
    echo "ERROR: no framework CPython >= 3.10 found." >&2
    echo "       Install one:  brew install python@3.11" >&2
    echo "       or point at it: ./build-macos-app.sh --python /path/to/python3" >&2
    exit 1
}

PY_MM="$("$PYBIN" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
PY_STDLIB="$("$PYBIN" -c 'import sysconfig; print(sysconfig.get_path("stdlib"))')"
PY_FW_PREFIX="$("$PYBIN" -c 'import sysconfig; print(sysconfig.get_config_var("PYTHONFRAMEWORKPREFIX"))')"
PY_FW_DIR="$PY_FW_PREFIX/Python.framework/Versions/$PY_MM"
echo ">>> Interpreter: $PYBIN (python $PY_MM, framework at $PY_FW_DIR)"

# ---- runtime deps: kept in sync with environment.yml's pip: section ---------
read -r -a RUNTIME_DEPS <<< "$(
    "$PYBIN" - "$REPO_DIR/environment.yml" <<'PY'
import re, sys
fallback = ["pymavlink>=2.4", "paramiko>=3.0", "pyserial>=3.5",
            "PyQt6>=6.6", "PyQt6-WebEngine>=6.6"]
try:
    lines = open(sys.argv[1]).read().splitlines()
    out, inpip = [], False
    for line in lines:
        if re.match(r"\s*-\s*pip:\s*$", line):
            inpip = True
            continue
        if inpip:
            m = re.match(r"\s{6,}-\s*(\S+)\s*$", line)
            if not m:
                break
            out.append(m.group(1))
    print(" ".join(out or fallback))
except OSError:
    print(" ".join(fallback))
PY
)"
echo ">>> Runtime deps: ${RUNTIME_DEPS[*]}"
echo ""

# ---- cleanup trap: drop a half-built bundle, keep logs + finished artifacts --
SUCCESS=0
cleanup() {
    if [ "${SUCCESS:-0}" -ne 1 ]; then
        rm -rf "$APP" 2>/dev/null || true
    fi
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ---- 1. bundle skeleton -----------------------------------------------------
echo ">>> Preparing bundle skeleton ..."
rm -rf "$APP"
mkdir -p "$BUILD_DIR" "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
RES="$APP/Contents/Resources"
PYROOT="$RES/python"
APPROOT="$RES/app"

# ---- 2. relocatable venv ----------------------------------------------------
echo ">>> Creating venv (python$PY_MM) in Contents/Resources/python ..."
"$PYBIN" -m venv --copies "$PYROOT"

# ---- 3. dependencies --------------------------------------------------------
PIP_LOG="$BUILD_DIR/pip-install-macos.log"
: > "$PIP_LOG"
echo ">>> Upgrading pip ..."
if ! "$PYROOT/bin/python3" -m pip install --upgrade pip >>"$PIP_LOG" 2>&1; then
    echo "ERROR: pip self-upgrade failed; log: $PIP_LOG" >&2
    tail -n 20 "$PIP_LOG" >&2 || true
    exit 1
fi
echo ">>> Installing ${RUNTIME_DEPS[*]} ..."
if ! "$PYROOT/bin/python3" -m pip install "${RUNTIME_DEPS[@]}" >>"$PIP_LOG" 2>&1; then
    echo "ERROR: dependency install failed; log: $PIP_LOG" >&2
    tail -n 20 "$PIP_LOG" >&2 || true
    exit 1
fi
echo "    (deps install log: $PIP_LOG)"

# ---- 4. stdlib --------------------------------------------------------------
# pyvenv.cfg `home =` points at the build host and breaks on another Mac.
# PYTHONHOME (set in the launcher) repoints the interpreter at the bundle; to
# make that resolve we copy the host stdlib (with lib-dynload) under the venv's
# lib/pythonX.Y. Unlike Linux, the macOS framework stdlib *contains* a
# site-packages — it must be excluded or it would clobber the venv's.
echo ">>> Bundling stdlib + lib-dynload from $PY_STDLIB ..."
rsync -a \
    --exclude 'site-packages' --exclude '__pycache__' \
    --exclude 'test' --exclude 'idlelib' --exclude 'turtledemo' \
    --exclude 'tkinter' --exclude 'ensurepip' \
    "$PY_STDLIB/" "$PYROOT/lib/python$PY_MM/"

# ---- 5. relocate the interpreter -------------------------------------------
# The venv's python binaries link the framework dylib by absolute path, and on a
# framework build they re-exec through Resources/Python.app/Contents/MacOS/Python
# (the GUI stub). Both the dylib and that stub are copied into the bundle and
# every reference is rewritten to an @executable_path-relative one, then each
# modified binary is re-signed — on Apple Silicon a binary whose signature was
# invalidated by install_name_tool will not execute.
echo ">>> Relocating the interpreter into Contents/Frameworks ..."
FW_DST="$APP/Contents/Frameworks/Python.framework/Versions/$PY_MM"
mkdir -p "$FW_DST"

OLD_REF="$(otool -L "$PYROOT/bin/python3" | awk 'NR>1 && $1 ~ /Python\.framework/ {print $1; exit}')"
[ -n "$OLD_REF" ] || { echo "ERROR: could not find the Python.framework reference in the venv binary" >&2; exit 1; }
[ -f "$OLD_REF" ] || { echo "ERROR: framework dylib not found at $OLD_REF" >&2; exit 1; }

cp -L "$OLD_REF" "$FW_DST/Python"
chmod u+w "$FW_DST/Python"
rsync -a "$PY_FW_DIR/Resources/" "$FW_DST/Resources/"   # Python.app GUI stub

sign() {  # <path> — re-sign after an install_name_tool rewrite
    codesign --force --sign "$CODESIGN_IDENTITY" --timestamp=none "$1" >/dev/null 2>&1 \
        || { echo "ERROR: codesign failed for $1" >&2; return 1; }
}

# bin/python, bin/python3, bin/pythonX.Y are three copies of the same launcher.
for b in "$PYROOT"/bin/python "$PYROOT"/bin/python3 "$PYROOT"/bin/python"$PY_MM"; do
    [ -f "$b" ] || continue
    install_name_tool -change "$OLD_REF" \
        "@executable_path/../../../Frameworks/Python.framework/Versions/$PY_MM/Python" \
        "$b" 2>/dev/null
    sign "$b"
done

STUB="$FW_DST/Resources/Python.app/Contents/MacOS/Python"
if [ -f "$STUB" ]; then
    chmod u+w "$STUB"
    install_name_tool -change "$OLD_REF" "@executable_path/../../../../Python" "$STUB" 2>/dev/null
    sign "$STUB"
    # The stub is what actually runs, so [NSBundle mainBundle] resolves to this
    # nested Python.app — give it the product's identity or the menu bar and
    # Dock would read "Python".
    "$PYBIN" - "$FW_DST/Resources/Python.app/Contents/Info.plist" "$APP_NAME" "$BUNDLE_ID" "$VERSION" <<'PY'
import plistlib, sys
path, name, bundle_id, version = sys.argv[1:5]
with open(path, "rb") as fh:
    pl = plistlib.load(fh)
pl.update({
    "CFBundleName": name,
    "CFBundleDisplayName": name,
    "CFBundleIdentifier": bundle_id + ".launcher",
    "CFBundleShortVersionString": version,
    "CFBundleVersion": version,
    "CFBundleIconFile": "corvus-gcs",
    "NSHighResolutionCapable": True,
    "NSRequiresAquaSystemAppearance": False,
})
with open(path, "wb") as fh:
    plistlib.dump(pl, fh)
PY
else
    echo "WARNING: framework GUI stub not found at $STUB"
fi

# Fail loudly rather than shipping a bundle that only runs on this machine.
if otool -L "$PYROOT/bin/python3" "$STUB" 2>/dev/null | grep -q "$PY_FW_PREFIX"; then
    echo "ERROR: a bundled binary still references the build host at $PY_FW_PREFIX" >&2
    exit 1
fi

# ---- 6. application code ----------------------------------------------------
# corvus/version.py does parent.parent/VERSION; corvus/server.py + app.py do
# parent.parent/src — so VERSION, corvus/, src/, assets/ must be siblings.
echo ">>> Copying application code ..."
mkdir -p "$APPROOT"
rsync -a --exclude '__pycache__' "$REPO_DIR/corvus/" "$APPROOT/corvus/"
rsync -a --exclude '__pycache__' "$REPO_DIR/src/" "$APPROOT/src/"
rsync -a "$REPO_DIR/assets/" "$APPROOT/assets/"
cp -a "$REPO_DIR/VERSION" "$APPROOT/VERSION"
# The Sustainable Use License requires that anyone who receives a copy of
# the software also receives a copy of its terms, so the licence ships in
# the bundle rather than only in the repository.
cp -a "$REPO_DIR/LICENSE.md" "$APPROOT/LICENSE.md"
if [ -f "$REPO_DIR/serve.py" ]; then
    cp -a "$REPO_DIR/serve.py" "$APPROOT/serve.py"
fi

# ---- 7. icon ----------------------------------------------------------------
echo ">>> Generating icon ..."
LOGO="$REPO_DIR/assets/CorvusGCS_logo.png"
[ -f "$LOGO" ] || { echo "ERROR: $LOGO not found" >&2; exit 1; }
ICONSET="$BUILD_DIR/corvus-gcs.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$LOGO" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null 2>&1
    sips -z "$((size * 2))" "$((size * 2))" "$LOGO" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null 2>&1
done
if iconutil -c icns "$ICONSET" -o "$RES/corvus-gcs.icns" 2>/dev/null; then
    cp -a "$RES/corvus-gcs.icns" "$FW_DST/Resources/Python.app/Contents/Resources/corvus-gcs.icns" 2>/dev/null || true
else
    echo "WARNING: iconutil failed; the bundle ships without an icon"
fi
rm -rf "$ICONSET"

# ---- 8. Info.plist (version read from VERSION, never a literal) ------------
echo ">>> Writing Info.plist ..."
"$PYBIN" - "$APP/Contents/Info.plist" "$APP_NAME" "$BUNDLE_ID" "$VERSION" <<'PY'
import plistlib, sys
path, name, bundle_id, version = sys.argv[1:5]
pl = {
    "CFBundleName": name,
    "CFBundleDisplayName": name,
    "CFBundleExecutable": "corvus-gcs",
    "CFBundleIdentifier": bundle_id,
    "CFBundleIconFile": "corvus-gcs",
    "CFBundleInfoDictionaryVersion": "6.0",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": version,
    "CFBundleVersion": version,
    "LSApplicationCategoryType": "public.app-category.utilities",
    "LSMinimumSystemVersion": "11.0",
    "NSHighResolutionCapable": True,
    "NSRequiresAquaSystemAppearance": False,
    "NSHumanReadableCopyright": "Universitat der Bundeswehr Munchen",
}
with open(path, "wb") as fh:
    plistlib.dump(pl, fh)
PY
printf 'APPL????' > "$APP/Contents/PkgInfo"

# ---- 9. launcher ------------------------------------------------------------
# No version literal; pythonX.Y is resolved at runtime, same as AppRun.
echo ">>> Writing launcher ..."
cat > "$APP/Contents/MacOS/corvus-gcs" <<'LAUNCH_EOF'
#!/bin/bash
set -euo pipefail

CONTENTS="$(cd "$(dirname "$0")/.." && pwd)"
RES="$CONTENTS/Resources"
APPROOT="$RES/app"

# Resolve the bundled pythonX.Y directory at runtime (X.Y was fixed at build).
shopt -s nullglob
PY_BASE=""
for d in "$RES/python/lib"/python3.*/; do PY_BASE="$d"; break; done
shopt -u nullglob
if [ -z "$PY_BASE" ] || [ ! -d "$PY_BASE" ]; then
    echo "corvus-gcs: bundled python3.x not found under $RES/python/lib" >&2
    exit 1
fi

# PYTHONHOME overrides the stale pyvenv.cfg `home =` so the bundled interpreter
# finds its stdlib inside the bundle regardless of where the .app was copied.
# PYTHONPATH re-adds the venv site-packages (PYTHONHOME bypasses venv site
# discovery) and the app root so `import corvus` resolves.
export PYTHONHOME="$RES/python"
export PYTHONPATH="$APPROOT:${PY_BASE}site-packages"
export PYTHONDONTWRITEBYTECODE=1

# corvus/app.py's default Chromium flags target Linux GPU stacks (Vulkan /
# swiftshader). It sets them with os.environ.setdefault, so seeding a
# macOS-appropriate value here keeps Metal in play. An operator override wins.
export QTWEBENGINE_CHROMIUM_FLAGS="${QTWEBENGINE_CHROMIUM_FLAGS:---enable-webgl}"

cd "$APPROOT"

# exec replaces the shell so SIGINT/SIGTERM reach python directly, letting
# corvus/app.py's signal handlers tear down cleanly (no zombies, no leaked
# sockets, tlog flushed).
exec "$RES/python/bin/python3" "$APPROOT/corvus/app.py" "$@"
LAUNCH_EOF
chmod 0755 "$APP/Contents/MacOS/corvus-gcs"

# ---- 10. sign the bundle ----------------------------------------------------
# Ad-hoc by default. Nested Mach-O files (Qt frameworks, the interpreter) keep
# the signatures they were shipped/re-signed with; --deep would re-sign them
# all and is both slow and deprecated, so only the bundle itself is signed.
echo ">>> Signing bundle (identity: $CODESIGN_IDENTITY) ..."
if ! codesign --force --sign "$CODESIGN_IDENTITY" "$APP" >>"$BUILD_DIR/codesign.log" 2>&1; then
    echo "WARNING: bundle signing failed; see $BUILD_DIR/codesign.log"
fi

# ---- 11. verify -------------------------------------------------------------
if [ "$VERIFY" -eq 1 ]; then
    echo ">>> Verifying the bundled runtime ..."
    if ! env -u PYTHONPATH -u PYTHONHOME \
            PYTHONHOME="$PYROOT" \
            PYTHONPATH="$APPROOT:$PYROOT/lib/python$PY_MM/site-packages" \
            "$PYROOT/bin/python3" - <<'PY'
import sys
import serial, paramiko                      # noqa: F401
from pymavlink import mavutil                # noqa: F401
from PyQt6.QtWebEngineWidgets import QWebEngineView  # noqa: F401
from corvus.version import get_version
print("    interpreter : %s" % sys.version.split()[0])
print("    prefix      : %s" % sys.prefix)
print("    corvus      : %s" % get_version())
print("    imports     : pyserial, paramiko, pymavlink, PyQt6 QtWebEngine OK")
PY
    then
        echo "ERROR: the bundled runtime failed its import check" >&2
        exit 1
    fi
    BUNDLED_VERSION="$(cat "$APPROOT/VERSION")"
    [ "$BUNDLED_VERSION" = "$VERSION" ] || {
        echo "ERROR: bundled VERSION ($BUNDLED_VERSION) != $VERSION" >&2; exit 1; }
fi

SUCCESS=1

# ---- 12. optional .dmg ------------------------------------------------------
if [ "$MAKE_DMG" -eq 1 ]; then
    echo ">>> Building $DMG ..."
    rm -f "$DMG"
    STAGE="$BUILD_DIR/dmg-stage"
    rm -rf "$STAGE"; mkdir -p "$STAGE"
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    if ! hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" \
            -ov -format UDZO "$DMG" >>"$BUILD_DIR/hdiutil.log" 2>&1; then
        echo "ERROR: hdiutil failed; log: $BUILD_DIR/hdiutil.log" >&2
        exit 1
    fi
    rm -rf "$STAGE"
fi

# ---- done -------------------------------------------------------------------
echo ""
echo ">>> Built: $APP  ($(du -sh "$APP" | cut -f1))"
[ "$MAKE_DMG" -eq 1 ] && echo ">>> Built: $DMG  ($(du -sh "$DMG" | cut -f1))"
if [ "$CODESIGN_IDENTITY" = "-" ]; then
    echo ""
    echo "    Note: ad-hoc signed, not notarized. On another Mac the first launch"
    echo "    needs right-click -> Open, or:  xattr -dr com.apple.quarantine \"$APP_NAME.app\""
    echo "    Set CODESIGN_IDENTITY=\"Developer ID Application: ...\" to sign properly."
fi
