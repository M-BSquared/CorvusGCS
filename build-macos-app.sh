#!/usr/bin/env bash
# Build Corvus GCS as a macOS application bundle (.app, optionally a .dmg).
#
# Mirrors build-appimage.sh: a relocatable CPython (venv --copies + host stdlib
# + the framework's Python dylib) plus the PySide6/QtWebEngine wheels, packed into
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
#   CORVUS_DIST        where the .app and .dmg are delivered (default: dist/)
#   CODESIGN_IDENTITY  Developer ID to sign with (default: "-", ad-hoc)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
BUILD_DIR="$REPO_DIR/build"
DIST_DIR="${CORVUS_DIST:-$REPO_DIR/dist}"
APP_NAME="Corvus GCS"
# The finished bundle lands here. It is not built here: see WORK_DIR below.
APP_OUT="$DIST_DIR/$APP_NAME.app"
# Personal reverse-DNS, not the institution's: the bundle identifier is an
# ownership claim macOS shows to the user, and it follows LICENSE.md.
BUNDLE_ID="de.mbsquared.corvus.gcs"
ARCH="$(uname -m)"
# Artifacts go to dist/, which is gitignored — not to the repo root.
# Two finished .dmg files (600 MB between them) were sitting in the
# checkout from builds two weeks old, because this is where they landed.
DMG="$DIST_DIR/Corvus_GCS-${VERSION}-macOS-${ARCH}.dmg"
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
            sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
echo "Bundle:  $APP_OUT"
echo ""

[ "$(uname -s)" = "Darwin" ] || { echo "ERROR: this build must run on macOS" >&2; exit 1; }
for tool in rsync install_name_tool otool codesign sips iconutil ditto lipo cc; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: '$tool' not found — install the Xcode command line tools:" >&2
        echo "       xcode-select --install" >&2
        exit 1
    }
done

# Everything the bundle is assembled from, checked before anything is built.
# This list used to be discovered one `cp` at a time, two hundred lines in: a
# missing LICENSE.md stopped the build *after* the venv was created and the
# wheels were downloaded, with `cp: ... No such file or directory` and no hint
# of which step wanted it. A build that cannot succeed should say so in its
# first second, and name the file.
MISSING=""
for required in \
    VERSION pyproject.toml LICENSE.md \
    corvus src assets plugins assets/CorvusGCS_logo.png
do
    [ -e "$REPO_DIR/$required" ] || MISSING="$MISSING  $required"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: the bundle is assembled from files that are not in the repo:" >&2
    for m in $MISSING; do echo "         $m" >&2; done
    echo "       Restore them (git checkout -- <path>) and run this again." >&2
    exit 1
fi

# ---- pick a framework CPython ----------------------------------------------
# A .app needs a *framework* build: the interpreter re-execs through
# Python.app/Contents/MacOS/Python (the GUI stub), which is what lets a Qt
# window own a Dock icon and a menu bar. Conda pythons are not framework
# builds and are not relocatable into a bundle, so they are rejected.
py_ok() {  # <path> -> 0 if usable
    local p="$1"
    [ -x "$p" ] || return 1
    ARCH="$ARCH" "$p" - <<'PY' >/dev/null 2>&1 || return 1
import os, platform, sys, sysconfig
# 3.12 is the floor pyproject.toml declares and the version CI
# tests. This used to accept 3.10, so the shipped .app could be
# built on an interpreter older than the one anything was tested on --
# "it passes CI" then said nothing about what operators run.
assert sys.version_info >= (3, 12), sys.version
assert sysconfig.get_config_var("PYTHONFRAMEWORK"), "not a framework build"
assert "conda" not in sys.prefix.lower(), "conda interpreter"
# The interpreter must run as the architecture we are building for. A
# universal2 python.org build satisfies this natively on either arch; an
# x86_64-only Homebrew install under /usr/local does not on Apple Silicon,
# and picking it would produce an Intel bundle carrying an arm64 name --
# uname -m answers for the shell, not for what ends up inside the .app.
assert platform.machine() == os.environ["ARCH"], (
    "%s runs as %s, building for %s" % (sys.executable, platform.machine(),
                                        os.environ["ARCH"]))
PY
}

if [ -n "$PYBIN" ]; then
    py_ok "$PYBIN" || { echo "ERROR: $PYBIN is not a usable framework python >= 3.12" >&2; exit 1; }
else
    # 3.12 first because it is the version CI tests and run.sh prefers;
    # newer ones are accepted after it. Nothing below 3.12 is listed, and
    # py_ok would reject it anyway.
    for cand in \
        /Library/Frameworks/Python.framework/Versions/3.12/bin/python3 \
        /Library/Frameworks/Python.framework/Versions/3.13/bin/python3 \
        /Library/Frameworks/Python.framework/Versions/3.14/bin/python3 \
        /opt/homebrew/bin/python3.12 /opt/homebrew/bin/python3.13 /opt/homebrew/bin/python3.14 \
        /usr/local/bin/python3.12 /usr/local/bin/python3.13 /usr/local/bin/python3.14 \
        "$(command -v python3 || true)"
    do
        if [ -n "$cand" ] && py_ok "$cand"; then PYBIN="$cand"; break; fi
    done
fi
[ -n "$PYBIN" ] || {
    echo "ERROR: no framework CPython >= 3.12 found." >&2
    echo "       Install one:  brew install python@3.12" >&2
    echo "       or point at it: ./build-macos-app.sh --python /path/to/python3" >&2
    exit 1
}

PY_MM="$("$PYBIN" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
PY_STDLIB="$("$PYBIN" -c 'import sysconfig; print(sysconfig.get_path("stdlib"))')"
PY_FW_PREFIX="$("$PYBIN" -c 'import sysconfig; print(sysconfig.get_config_var("PYTHONFRAMEWORKPREFIX"))')"
PY_FW_DIR="$PY_FW_PREFIX/Python.framework/Versions/$PY_MM"
echo ">>> Interpreter: $PYBIN (python $PY_MM, framework at $PY_FW_DIR)"

# ---- runtime deps -----------------------------------------------------------
# pyproject.toml's `app` group, read by pip itself. This used to be a Python
# regex that screen-scraped a conda environment file's pip: block — against a
# YAML file it could not actually parse — with a hardcoded fallback list for
# when that failed. It was the only build script that tried to stay in sync,
# and it did so by guessing at a format.
#
# Which source, in order: $CORVUS_REQUIREMENTS, then requirements.lock if the
# repo has one, then the `app` group. The lock is what makes a release
# rebuildable — the group states floors, so installing from it in six months
# resolves to whatever is newest then, and the "same" tag produces a different
# binary. Every build writes the set it actually installed to dist/*.lock
# (below); promoting one of those to requirements.lock at tag time is what
# pins the next rebuild to it.
REQUIREMENTS="${CORVUS_REQUIREMENTS:-}"
if [ -z "$REQUIREMENTS" ] && [ -f "$REPO_DIR/requirements.lock" ]; then
    REQUIREMENTS="$REPO_DIR/requirements.lock"
fi
if [ -n "$REQUIREMENTS" ]; then
    if [ ! -f "$REQUIREMENTS" ]; then
        echo "ERROR: missing $REQUIREMENTS" >&2
        exit 1
    fi
    DEPS_ARGS=(-r "$REQUIREMENTS")
    DEPS_LABEL="$(basename "$REQUIREMENTS")"
else
    DEPS_ARGS=(--group "$REPO_DIR/pyproject.toml:app")
    DEPS_LABEL="pyproject.toml [app]"
fi
LOCK_OUT="$DIST_DIR/Corvus_GCS-${VERSION}-macOS-${ARCH}.lock"
echo ">>> Runtime deps: $DEPS_LABEL"
echo ""

# ---- where the bundle is built ----------------------------------------------
# Assembled, signed and verified in a private folder under the local temp dir,
# and only moved to $DIST_DIR once all of that passed. A checkout inside a
# folder a sync agent manages (iCloud Drive, OneDrive, Dropbox) has that agent
# re-apply com.apple.FinderInfo to directories inside the bundle while the
# build runs, and codesign refuses the whole .app with "resource fork, Finder
# information, or similar detritus not allowed". Nothing wins that race in
# place; building where the agent does not look avoids it, whatever folder the
# repository is in.
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/corvus-gcs-build.XXXXXX")"
APP="$WORK_DIR/$APP_NAME.app"
MNT=""

# ---- cleanup trap: the work folder goes, logs + delivered artifacts stay -----
cleanup() {
    if [ -n "${MNT:-}" ] && [ -d "$MNT" ]; then
        hdiutil detach "$MNT" -force -quiet 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR" 2>/dev/null || true
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ---- 1. bundle skeleton -----------------------------------------------------
echo ">>> Preparing bundle skeleton in $WORK_DIR ..."
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
echo ">>> Installing $DEPS_LABEL ..."
if ! "$PYROOT/bin/python3" -m pip install "${DEPS_ARGS[@]}" >>"$PIP_LOG" 2>&1; then
    echo "ERROR: dependency install failed; log: $PIP_LOG" >&2
    tail -n 20 "$PIP_LOG" >&2 || true
    exit 1
fi
echo "    (deps install log: $PIP_LOG)"
# What went into THIS bundle, exactly. Ships beside the artifact so a rebuild
# six months from now can be told to resolve to the same versions:
#   cp dist/Corvus_GCS-<version>-macOS-<arch>.lock requirements.lock
mkdir -p "$DIST_DIR"
{
    echo "# Corvus GCS $VERSION — macOS $ARCH, python $PY_MM"
    echo "# The resolved runtime set this artifact was built from."
    echo "# Install with: pip install -r <this file>"
    "$PYROOT/bin/python3" -m pip freeze --all --exclude-editable
} > "$LOCK_OUT" 2>>"$PIP_LOG" || {
    echo "WARNING: could not write $LOCK_OUT" >&2
    rm -f "$LOCK_OUT"
}
if [ -f "$LOCK_OUT" ]; then echo "    (resolved set: $LOCK_OUT)"; fi

# ---- 3b. Qt: this architecture, the modules Corvus loads --------------------
# The PySide6 wheels are universal2: every Qt binary carries an arm64 and an
# x86_64 slice, and the bundle is built for one of them. ditto keeps the slice
# for $ARCH and drops the other, which halves Qt without touching a byte of
# the slice that stays, its signature included. Then tools/qt_bundle.py takes
# out the Qt modules, plugins and tools nothing in Corvus loads, and fails the
# build if anything left would miss a library it links.
SITE_PKGS="$PYROOT/lib/python$PY_MM/site-packages"
echo ">>> Thinning PySide6 to $ARCH ..."
for pkg in PySide6 shiboken6; do
    [ -d "$SITE_PKGS/$pkg" ] || { echo "ERROR: $pkg is not installed in the bundle" >&2; exit 1; }
    rm -rf "$SITE_PKGS/$pkg.thin"
    ditto --arch "$ARCH" "$SITE_PKGS/$pkg" "$SITE_PKGS/$pkg.thin"
    rm -rf "$SITE_PKGS/$pkg"
    mv "$SITE_PKGS/$pkg.thin" "$SITE_PKGS/$pkg"
done
echo ">>> Trimming PySide6 to the Qt modules Corvus loads ..."
"$PYBIN" "$REPO_DIR/tools/qt_bundle.py" prune "$SITE_PKGS"

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
    --exclude 'tkinter' --exclude '_tkinter*' --exclude 'ensurepip' \
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

# The python.org interpreter is universal2. Left that way, a process started
# under Rosetta (anything that asks for x86_64, or an older launcher) runs it as
# x86_64, and the Qt in this bundle, thinned to $ARCH above, does not load: the
# app dies on its first import. With one architecture there is nothing to pick.
thin_to_arch() {  # <mach-o>
    local archs
    archs="$(lipo -archs "$1" 2>/dev/null)" || return 0
    [ "$archs" = "$ARCH" ] && return 0
    case " $archs " in
        *" $ARCH "*) ;;
        *) echo "ERROR: $1 is [$archs] and has no $ARCH slice" >&2; return 1 ;;
    esac
    chmod u+w "$1"
    lipo -thin "$ARCH" "$1" -output "$1.thin" && mv "$1.thin" "$1"
}
thin_to_arch "$FW_DST/Python"
# Its own install name still names the host framework; nothing loads it by
# that name, but the bundle-wide check below rightly would not tell the two
# apart. Signed with the framework in step 10.
install_name_tool -id "@rpath/Python.framework/Versions/$PY_MM/Python" "$FW_DST/Python"
[ -f "$FW_DST/Resources/Python.app/Contents/MacOS/Python" ] \
    && thin_to_arch "$FW_DST/Resources/Python.app/Contents/MacOS/Python"
for b in "$PYROOT"/bin/python "$PYROOT"/bin/python3 "$PYROOT"/bin/python"$PY_MM"; do
    [ -f "$b" ] && [ ! -L "$b" ] && thin_to_arch "$b"
done

# A framework is not a directory that happens to contain Versions/<x>: codesign
# checks for the standard layout and rejects anything else with "bundle format
# unrecognized, invalid, or unsuitable" — naming the subcomponent but not what
# is wrong with it. Versions/Current and the two root symlinks pointing through
# it are what make this a framework rather than a folder. Without them the
# whole .app fails to sign and ships unsigned, which on another Mac is a worse
# first launch than the ad-hoc signature this build is trying to give it.
ln -sfn "$PY_MM" "$APP/Contents/Frameworks/Python.framework/Versions/Current"
ln -sfn Versions/Current/Python "$APP/Contents/Frameworks/Python.framework/Python"
ln -sfn Versions/Current/Resources "$APP/Contents/Frameworks/Python.framework/Resources"

# Load-command paths in <binary> that genuinely live under the host framework.
#
# Matching had been a bare substring search for $PY_FW_PREFIX anywhere in the
# otool line. With a python.org interpreter that prefix is /Library/Frameworks,
# which is a substring of /System/Library/Frameworks — so every binary linking
# the OS's own CoreFoundation looked like it referenced the build host, and the
# macOS build was rejected for a bundle that was actually fine. A Homebrew
# interpreter has a prefix that cannot collide, which is why this only ever
# failed in CI. Compare the path field against the prefix as a path, not as
# text anywhere in the line.
# Homebrew's prefix is a symlink (opt/python@X.Y) into the Cellar, and the
# binaries name the Cellar path, so both spellings count.
PY_FW_PREFIX_LINKED="${OLD_REF%%/Python.framework/*}"
host_fw_refs() {  # <binary>
    local ref
    otool -L "$1" 2>/dev/null | awk 'NR>1 && !/:$/ {print $1}' | while IFS= read -r ref; do
        case "$ref" in "$PY_FW_PREFIX"/*|"$PY_FW_PREFIX_LINKED"/*) printf '%s\n' "$ref" ;; esac
    done
}

# Load-command paths in <binary> that point anywhere outside macOS itself: the
# host framework, Homebrew's OpenSSL, /usr/local. Relative ones (@rpath,
# @loader_path, @executable_path) are the bundle's own and are left out.
foreign_refs() {  # <binary>
    local ref
    otool -L "$1" 2>/dev/null | awk 'NR>1 && !/:$/ {print $1}' | while IFS= read -r ref; do
        case "$ref" in /System/*|/usr/lib/*|@*) ;; /*) printf '%s\n' "$ref" ;; esac
    done
}

sign() {  # <path> — re-sign after an install_name_tool rewrite
    codesign --force --sign "$CODESIGN_IDENTITY" --timestamp=none "$1" >/dev/null 2>&1 \
        || { echo "ERROR: codesign failed for $1" >&2; return 1; }
}

# install_name_tool's own errors used to go to /dev/null, so a rewrite that did
# not happen looked exactly like one that did, and the only symptom was the
# host-reference check failing several lines later with no reason given. The
# most likely error is worth naming: a Mach-O header has a fixed amount of room
# for its load commands, and a replacement path longer than the original does
# not always fit.
retarget() {  # <binary> <new_ref> — repoint every host-framework reference
    local bin="$1" to="$2" ref out
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        if ! out="$(install_name_tool -change "$ref" "$to" "$bin" 2>&1)"; then
            echo "ERROR: install_name_tool failed on $bin" >&2
            echo "         from: $ref (${#ref} bytes)" >&2
            echo "         to:   $to (${#to} bytes)" >&2
            [ -n "$out" ] && echo "         $out" >&2
            return 1
        fi
        echo "    $(basename "$bin"): $ref -> $to"
    done <<< "$(host_fw_refs "$bin")"
    # A silent no-op is a real failure mode, so verify rather than assume.
    if [ -n "$(host_fw_refs "$bin")" ]; then
        echo "ERROR: $bin still references $PY_FW_PREFIX after the rewrite" >&2
        host_fw_refs "$bin" | sed 's/^/         /' >&2
        return 1
    fi
    return 0
}

# bin/python, bin/python3, bin/pythonX.Y are three copies of the same launcher.
NEW_REF="@executable_path/../../../Frameworks/Python.framework/Versions/$PY_MM/Python"
echo "    install name: $OLD_REF (${#OLD_REF} bytes) -> $NEW_REF (${#NEW_REF} bytes)"
for b in "$PYROOT"/bin/python "$PYROOT"/bin/python3 "$PYROOT"/bin/python"$PY_MM"; do
    [ -f "$b" ] || continue
    retarget "$b" "$NEW_REF"
    sign "$b"
done

STUB="$FW_DST/Resources/Python.app/Contents/MacOS/Python"
if [ -f "$STUB" ]; then
    chmod u+w "$STUB"
    retarget "$STUB" "@executable_path/../../../../Python"
    # Not signed here: its Info.plist is rewritten below and the icon lands
    # in step 7, and either one breaks a seal made before it. Step 10 signs
    # it once both are final.
    #
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

# ---- 5b. the libraries the stdlib's extension modules link ------------------
# python.org builds _ssl and _hashlib against an OpenSSL it keeps beside the
# framework (and _curses against its own ncurses), linked by absolute path:
# /Library/Frameworks/Python.framework/Versions/X.Y/lib/libssl.3.dylib. Homebrew
# links /opt/homebrew/opt/openssl@3 the same way. On the build Mac that path
# exists, so every check here passed; on a Mac without that exact install the
# app died at its first `import ssl`.
# Copied into the bundle's lib/ and linked relative to the module instead.
echo ">>> Bundling the libraries the stdlib links ..."
BUNDLE_LIB="$PYROOT/lib"
macho_under() {  # <dir> -> every .so / .dylib below it
    /usr/bin/find "$1" -type f \( -name '*.so' -o -name '*.dylib' \) ! -path '*/site-packages/*'
}
while :; do
    added=0
    while IFS= read -r bin; do
        [ -n "$bin" ] || continue
        while IFS= read -r ref; do
            [ -n "$ref" ] || continue
            name="$(basename "$ref")"
            [ -f "$BUNDLE_LIB/$name" ] && continue
            [ -f "$ref" ] || { echo "ERROR: $bin links $ref, which is missing" >&2; exit 1; }
            cp -L "$ref" "$BUNDLE_LIB/$name"
            chmod u+w "$BUNDLE_LIB/$name"
            echo "    bundled $name"
            added=1
        done <<< "$(foreign_refs "$bin")"
    done <<< "$(macho_under "$PYROOT/lib")"
    [ "$added" -eq 1 ] || break
done
while IFS= read -r bin; do
    [ -n "$bin" ] || continue
    refs="$(foreign_refs "$bin")"
    [ -n "$refs" ] || continue
    rel="$("$PYBIN" -c 'import os, sys; print(os.path.relpath(sys.argv[2], os.path.dirname(sys.argv[1])))' "$bin" "$BUNDLE_LIB")"
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        name="$(basename "$ref")"
        if [ "$(dirname "$bin")" = "$BUNDLE_LIB" ] && [ "$name" = "$(basename "$bin")" ]; then
            install_name_tool -id "@loader_path/$name" "$bin" 2>/dev/null \
                || { echo "ERROR: install_name_tool -id failed on $bin" >&2; exit 1; }
        else
            to="@loader_path/$rel/$name"
            [ "$rel" = "." ] && to="@loader_path/$name"
            install_name_tool -change "$ref" "$to" "$bin" 2>/dev/null \
                || { echo "ERROR: install_name_tool failed on $bin ($ref -> $to)" >&2; exit 1; }
        fi
    done <<< "$refs"
    sign "$bin"
done <<< "$(macho_under "$PYROOT/lib")"

# Fail loudly rather than shipping a bundle that only runs on this machine.
# Every Mach-O in the bundle, not a chosen few: the import check further down
# runs on the build Mac, where a path into the host framework resolves, so it
# cannot see one. Names what it found, because "still references the build
# host" alone leaves nothing to act on.
host_refs=""
while IFS= read -r b; do
    [ -n "$b" ] || continue
    hits="$(foreign_refs "$b")"
    if [ -n "$hits" ]; then
        host_refs="${host_refs}
  ${b#"$WORK_DIR"/}:
$(printf '%s\n' "$hits" | sed 's/^/    /')"
    fi
done <<< "$(/usr/bin/find "$APP" -type f \( -name '*.so' -o -name '*.dylib' -o -perm -u+x \) ! -name '*.py')"
if [ -n "$host_refs" ]; then
    echo "ERROR: a bundled binary links a library outside the bundle and outside macOS" >&2
    printf '%s\n' "$host_refs" >&2
    echo "  (the install name rewritten above was: $OLD_REF)" >&2
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
# The plugins that ship with Corvus. corvus/plugin_registry.py resolves this
# root as a sibling of the corvus package, exactly like src/, so it has to land
# beside it. Operator plugins live in ~/.corvus/plugins and are never bundled,
# and neither is a plugin installed into plugins/ for development: only the
# ones .gitignore lets back in, without their tests/.
"$PYBIN" "$REPO_DIR/tools/bundle_plugins.py" "$APPROOT/plugins"
# -X: rsync -a above leaves extended attributes behind, cp -a does not, and a
# working copy's (a sync agent's, Finder's) are nothing the bundle should carry.
cp -aX "$REPO_DIR/VERSION" "$APPROOT/VERSION"
# The Sustainable Use License requires that anyone who receives a copy of
# the software also receives a copy of its terms, so the licence ships in
# the bundle rather than only in the repository.
cp -aX "$REPO_DIR/LICENSE.md" "$APPROOT/LICENSE.md"
if [ -f "$REPO_DIR/serve.py" ]; then
    cp -aX "$REPO_DIR/serve.py" "$APPROOT/serve.py"
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
    # PySide6 6.10 and later is built for macOS 15 (its own binaries say so,
    # whatever its wheel tag claims), and 15 is the oldest macOS supported.
    "LSMinimumSystemVersion": "15.0",
    "NSHighResolutionCapable": True,
    "NSRequiresAquaSystemAppearance": False,
    # Follows LICENSE.md: the copyright is the author's. ASCII-folded because
    # this string is written into Info.plist.
    "NSHumanReadableCopyright": "Copyright (c) 2026 Maximilian Bock",
}
with open(path, "wb") as fh:
    plistlib.dump(pl, fh)
PY
printf 'APPL????' > "$APP/Contents/PkgInfo"

# ---- 9. launcher ------------------------------------------------------------
# A native executable compiled for $ARCH, not a script: LaunchServices starts a
# script bundle under Rosetta on Apple Silicon, and the interpreter then cannot
# load this bundle's Qt (see tools/macos_launcher.c). No version literal;
# pythonX.Y is read at runtime. No QTWEBENGINE_CHROMIUM_FLAGS either:
# corvus/app.py picks the macOS set itself and applies it only when that
# variable is unset.
echo ">>> Compiling launcher for $ARCH ..."
cc -arch "$ARCH" -mmacosx-version-min=15.0 -Os -Wall -Wextra -Werror \
    -o "$APP/Contents/MacOS/corvus-gcs" "$REPO_DIR/tools/macos_launcher.c" \
    || { echo "ERROR: the launcher did not compile" >&2; exit 1; }
chmod 0755 "$APP/Contents/MacOS/corvus-gcs"

# ---- 10. sign the bundle ----------------------------------------------------
# Strip extended attributes first. codesign refuses a bundle carrying them —
# "resource fork, Finder information, or similar detritus not allowed" — and
# this bundle is assembled by rsync and cp from a working copy on a Mac, so it
# arrives with thousands: com.apple.provenance on everything the OS has seen,
# com.apple.macl on files an app was ever granted access to. Eight thousand of
# them on a normal build, and every one is metadata about THIS machine that has
# no business in a shipped artifact. They carry nothing the app needs.
#
# Without this the build ended on "WARNING: bundle signing failed" and produced
# an unsigned .app, which on another Mac is a worse first launch than the
# ad-hoc-signed one this is trying to make.
# Only these two, not `xattr -cr`. codesign objects to exactly these; the
# other attributes a macOS working copy carries (com.apple.provenance, and a
# file provider's own) are system-managed, refuse to be removed, and make a
# blanket clear fail per file without touching the ones that matter.
echo ">>> Stripping Finder metadata ..."
xattr -rd com.apple.FinderInfo "$APP" 2>/dev/null || true
xattr -rd com.apple.ResourceFork "$APP" 2>/dev/null || true

# A symlink to nothing fails `codesign --verify --strict` with a bare "No such
# file or directory" on the .app, naming neither the link nor its target. The
# stdlib copy brings two: config-X.Y-darwin/libpythonX.Y.{a,dylib} point at
# ../../../Python, which is where the dylib sits in the host framework and not
# where it sits in this bundle. They only serve compiling against Python, and
# pointing at nothing they serve nothing here either.
echo ">>> Dropping dangling symlinks ..."
/usr/bin/find "$APP" -type l ! -exec test -e {} \; -print -delete \
    | sed "s|^$WORK_DIR/|    |"

# Ad-hoc by default, and inside out: each seal records the signatures of the
# code nested in it, so a container is signed only after everything in it is
# final. Mach-O files under Contents/Resources (Qt, the interpreter) are sealed
# as plain data by the outer signature and keep the ones they shipped with;
# --deep would re-sign them all and is both slow and deprecated.
#
# The framework has to be re-signed rather than kept. It arrives signed by the
# Python Software Foundation, and that signature seals a
# Versions/X.Y/_CodeSignature/CodeResources which is not copied, over a
# Resources/ whose Python.app this build rewrites. Nothing notices on the build
# machine, because nothing there asks Gatekeeper. A downloaded copy carries
# com.apple.quarantine, Gatekeeper verifies every nested seal, and the answer
# for this one was "is damaged and can't be opened", with no way past it.
echo ">>> Signing bundle (identity: $CODESIGN_IDENTITY) ..."
: > "$BUILD_DIR/codesign.log"   # one run per log, or a failure reads as four
SIGNED=1
for target in "$FW_DST/Resources/Python.app" "$FW_DST" "$APP"; do
    [ -e "$target" ] || continue
    echo "    ${target#"$WORK_DIR"/}"
    if ! codesign --force --sign "$CODESIGN_IDENTITY" "$target" \
            >>"$BUILD_DIR/codesign.log" 2>&1; then
        SIGNED=0
        break
    fi
done
if [ "$SIGNED" -ne 1 ]; then
    echo "WARNING: bundle signing failed; see $BUILD_DIR/codesign.log"
    tail -n 5 "$BUILD_DIR/codesign.log" >&2 || true
    # codesign names the bundle, never the file it objected to. `|| true`
    # because under pipefail a head that stops early SIGPIPEs xattr.
    detritus="$(xattr -r "$APP" 2>/dev/null \
        | grep -E 'com\.apple\.(FinderInfo|ResourceFork)' | head -n 5 || true)"
    if [ -n "$detritus" ]; then
        echo "       Still carrying Finder metadata after the strip:" >&2
        printf '%s\n' "$detritus" | sed 's/^/         /' >&2
    fi
fi

# ---- 11. verify -------------------------------------------------------------
if [ "$VERIFY" -eq 1 ]; then
    echo ">>> Verifying the bundled runtime ..."
    # PYTHONDONTWRITEBYTECODE, because this step runs the bundled interpreter
    # INSIDE the bundle that was just signed. Importing Qt and pymavlink writes
    # __pycache__/*.pyc next to their sources, and a file added after signing
    # breaks the seal: `codesign --verify` then lists a few hundred "file
    # added" lines for an .app that had signed cleanly a second earlier.
    if ! env -u PYTHONPATH -u PYTHONHOME \
            PYTHONDONTWRITEBYTECODE=1 \
            PYTHONHOME="$PYROOT" \
            PYTHONPATH="$APPROOT:$PYROOT/lib/python$PY_MM/site-packages" \
            "$PYROOT/bin/python3" - <<'PY'
import sys
import serial, paramiko                      # noqa: F401
import ssl, hashlib, sqlite3, lzma, ctypes   # noqa: F401  (the stdlib's own native libraries)
from pymavlink import mavutil                # noqa: F401
# Every module corvus/app.py imports, from the trimmed Qt.
from PySide6 import QtCore, QtGui, QtWidgets, QtNetwork  # noqa: F401
from PySide6 import QtWebChannel, QtWebEngineCore, QtWebEngineWidgets  # noqa: F401
# The windows out of the app need qwebchannel.js, which Qt keeps inside the
# QtWebChannel library as a resource.
channel_js = QtCore.QFile(":/qtwebchannel/qwebchannel.js")
assert channel_js.open(QtCore.QIODevice.OpenModeFlag.ReadOnly), "qwebchannel.js missing"
from corvus.version import get_version
print("    interpreter : %s" % sys.version.split()[0])
print("    prefix      : %s" % sys.prefix)
print("    corvus      : %s" % get_version())
print("    qt          : %s (PySide6 %s)" % (QtCore.qVersion(), __import__("PySide6").__version__))
print("    imports     : pyserial, paramiko, pymavlink, PySide6 QtWebEngine OK")
PY
    then
        echo "ERROR: the bundled runtime failed its import check" >&2
        exit 1
    fi
    BUNDLED_VERSION="$(cat "$APPROOT/VERSION")"
    [ "$BUNDLED_VERSION" = "$VERSION" ] || {
        echo "ERROR: bundled VERSION ($BUNDLED_VERSION) != $VERSION" >&2; exit 1; }

    # The artifact is named from `uname -m`, which answers for the shell and
    # not for what pip put inside the bundle. Nothing else here checks that the
    # two agree, so an x86_64 Qt wheel in a bundle called arm64 would ship
    # silently and only fail on the operator's Mac. Check the two heaviest
    # native payloads: the interpreter itself and QtWebEngineCore.
    echo ">>> Verifying the bundle is $ARCH ..."
    arch_of() {  # <mach-o path> -> the architectures it actually contains
        lipo -archs "$1" 2>/dev/null || file -b "$1"
    }
    SITE="$PYROOT/lib/python$PY_MM/site-packages"
    QT_CORE="$(/usr/bin/find "$SITE/PySide6" -name 'QtWebEngineCore' -type f 2>/dev/null | head -1)"
    # The launcher and the interpreter must be $ARCH and nothing else: a
    # second slice is what lets a Rosetta start pick the wrong one.
    for binary in "$APP/Contents/MacOS/corvus-gcs" "$PYROOT/bin/python3"; do
        archs="$(arch_of "$binary")"
        if [ "$archs" != "$ARCH" ]; then
            echo "ERROR: $binary is [$archs]; it must be $ARCH only." >&2
            exit 1
        fi
    done
    for binary in "$PYROOT/bin/python3" ${QT_CORE:+"$QT_CORE"}; do
        [ -f "$binary" ] || continue
        archs="$(arch_of "$binary")"
        case " $archs " in
            *" $ARCH "*) echo "    $(basename "$binary"): $archs" ;;
            *)
                echo "ERROR: $binary is [$archs], but this bundle is named $ARCH." >&2
                echo "       The interpreter or the Qt wheels came from the wrong" >&2
                echo "       architecture. Check which python was picked above." >&2
                exit 1
                ;;
        esac
    done

    # Last, so it also catches anything the import check above wrote into the
    # bundle. --deep --strict is the check Gatekeeper makes on a downloaded
    # copy, and the only place a broken seal shows before an operator sees
    # "is damaged": a local build is never quarantined, so it runs regardless.
    echo ">>> Verifying the code signature ..."
    if ! codesign --verify --deep --strict --verbose=2 "$APP" \
            >"$BUILD_DIR/codesign-verify.log" 2>&1; then
        echo "ERROR: the bundle's signature does not verify. A downloaded copy" >&2
        echo "       would refuse to open as damaged. $BUILD_DIR/codesign-verify.log:" >&2
        tail -n 20 "$BUILD_DIR/codesign-verify.log" >&2 || true
        exit 1
    fi
    echo "    $(tail -n 1 "$BUILD_DIR/codesign-verify.log")"
fi

# ---- 12. optional .dmg ------------------------------------------------------
# Built in the work folder too, and moved to $DIST_DIR only once the copy
# inside it verified, so a failed build never leaves a .dmg behind.
DMG_WORK="$WORK_DIR/$(basename "$DMG")"
if [ "$MAKE_DMG" -eq 1 ]; then
    echo ">>> Building $(basename "$DMG") ..."
    STAGE="$WORK_DIR/dmg-stage"
    rm -rf "$STAGE"; mkdir -p "$STAGE"
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    # hdiutil create fails now and then on a busy machine, typically "Resource
    # busy" while it detaches the image it just filled; GitHub's macOS runners
    # do it often enough to fail a release, and a retry a few seconds later
    # goes through. Its reason only ever reached the log, which CI does not
    # keep, so a failure there said nothing at all: it is printed now.
    : > "$BUILD_DIR/hdiutil.log"
    DMG_MADE=0
    for attempt in 1 2 3 4; do
        echo "--- hdiutil create, attempt $attempt" >>"$BUILD_DIR/hdiutil.log"
        if hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" \
                -ov -format UDZO "$DMG_WORK" >>"$BUILD_DIR/hdiutil.log" 2>&1; then
            DMG_MADE=1
            break
        fi
        rm -f "$DMG_WORK"
        echo "    attempt $attempt failed: $(tail -n 1 "$BUILD_DIR/hdiutil.log")"
        if [ "$attempt" -lt 4 ]; then sleep $((attempt * 10)); fi
    done
    if [ "$DMG_MADE" -ne 1 ]; then
        echo "ERROR: hdiutil failed; log: $BUILD_DIR/hdiutil.log" >&2
        tail -n 20 "$BUILD_DIR/hdiutil.log" >&2 || true
        exit 1
    fi
    rm -rf "$STAGE"

    # The copy an operator receives, not the one in dist/: it went through cp
    # and hdiutil, and either could have dropped something the seal covers.
    if [ "$VERIFY" -eq 1 ]; then
        echo ">>> Verifying the signature inside the .dmg ..."
        MNT="$(mktemp -d "$WORK_DIR/dmg-verify.XXXXXX")"
        if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$MNT" "$DMG_WORK" \
                >>"$BUILD_DIR/hdiutil.log" 2>&1; then
            echo "ERROR: could not mount $DMG_WORK to verify it; log: $BUILD_DIR/hdiutil.log" >&2
            exit 1
        fi
        DMG_OK=1
        codesign --verify --deep --strict --verbose=2 "$MNT/$APP_NAME.app" \
            >>"$BUILD_DIR/codesign-verify.log" 2>&1 || DMG_OK=0
        hdiutil detach "$MNT" -quiet 2>/dev/null \
            || hdiutil detach "$MNT" -force -quiet 2>/dev/null || true
        rmdir "$MNT" 2>/dev/null || true
        if [ "$DMG_OK" -ne 1 ]; then
            echo "ERROR: the .app inside the .dmg does not verify; log: $BUILD_DIR/codesign-verify.log" >&2
            tail -n 20 "$BUILD_DIR/codesign-verify.log" >&2 || true
            exit 1
        fi
    fi
fi

# ---- 13. deliver ------------------------------------------------------------
# Only now does the previous build in $DIST_DIR get replaced. mv keeps the
# extended attributes, which is where the launcher script's signature lives.
echo ">>> Delivering to $DIST_DIR ..."
mkdir -p "$DIST_DIR"
rm -rf "$APP_OUT"
mv "$APP" "$APP_OUT"
if [ "$MAKE_DMG" -eq 1 ]; then
    rm -f "$DMG"
    mv "$DMG_WORK" "$DMG"
fi

# ---- done -------------------------------------------------------------------
echo ""
echo ">>> Built: $APP_OUT  ($(du -sh "$APP_OUT" | cut -f1))"
[ "$MAKE_DMG" -eq 1 ] && echo ">>> Built: $DMG  ($(du -sh "$DMG" | cut -f1))"
# The .dmg is sealed and safe to hand out from anywhere. The loose .app is not
# once a sync agent has written Finder metadata into it, which happens the
# moment it lands: it still runs here, but a copy of it would not verify on
# another Mac. The file provider marks the root it syncs, not every folder
# below it, hence the walk up. grep -c rather than -q: under pipefail a -q that
# exits early can SIGPIPE xattr and read as "no".
in_synced_folder() {  # <dir>
    local d
    d="$(cd "$1" 2>/dev/null && pwd -P)" || return 1
    while [ -n "$d" ] && [ "$d" != "/" ]; do
        if [ "$(xattr "$d" 2>/dev/null | grep -Ec 'com\.apple\.file-?provider' || true)" -gt 0 ]; then
            return 0
        fi
        d="$(dirname "$d")"
    done
    return 1
}
if in_synced_folder "$DIST_DIR"; then
    echo ""
    echo "    Note: $DIST_DIR is in a cloud-synced folder, which writes Finder"
    echo "    metadata into the .app and breaks its seal. It runs from here, but"
    echo "    give other Macs the .dmg (./build.sh --dmg), not a copy of the .app."
fi
if [ "$CODESIGN_IDENTITY" = "-" ]; then
    echo ""
    echo "    Note: ad-hoc signed, not notarized. A downloaded copy is blocked on its"
    echo "    first launch: System Settings > Privacy & Security > Open Anyway, or"
    echo "      xattr -dr com.apple.quarantine \"/Applications/$APP_NAME.app\""
    echo "    Set CODESIGN_IDENTITY=\"Developer ID Application: ...\" to sign properly."
fi
