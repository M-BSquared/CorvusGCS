#!/usr/bin/env bash
# Build Corvus GCS as a macOS application bundle (.app, optionally a .dmg).
#
# The bundle is the native Rust interface (Cargo.toml and src/, built with
# cargo) and nothing else that runs: the backend (corvus_core) is a library
# inside that one executable. No CPython, no pip, no PyInstaller, no Qt, no web
# engine, and corvus_link is built without its `python` feature (PyO3 is never
# compiled). The build host needs the Xcode command line tools and a Rust
# toolchain (rustup, cargo); the target Mac needs neither.
#
# Version is read exclusively from the repo-root VERSION file — never hardcoded.
#
# Usage:
#   ./build-macos-app.sh [--dmg] [--no-verify]
#
# Env:
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
# The oldest macOS the interface runs on. Both the Rust build and Info.plist
# read it, so they cannot disagree.
MIN_MACOS="12.0"
DMG="$DIST_DIR/Corvus_GCS-${VERSION}-macOS-${ARCH}.dmg"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"

MAKE_DMG=0
VERIFY=1
while [ $# -gt 0 ]; do
    case "$1" in
        --dmg)        MAKE_DMG=1 ;;
        --no-verify)  VERIFY=0 ;;
        -h|--help)
            sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
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
for tool in install_name_tool otool codesign sips iconutil lipo cargo; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: '$tool' not found — install the Xcode command line tools:" >&2
        echo "       xcode-select --install   (and rustup from https://rustup.rs for cargo)" >&2
        exit 1
    }
done

# Everything the bundle is assembled from, checked before anything is built, so
# a build that cannot succeed says so in its first second and names the file.
MISSING=""
for required in \
    VERSION LICENSE.md assets/CorvusGCS_logo.png assets/licenses \
    Cargo.toml Cargo.lock src/main.rs
do
    [ -e "$REPO_DIR/$required" ] || MISSING="$MISSING  $required"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: the bundle is assembled from files that are not in the repo:" >&2
    for m in $MISSING; do echo "         $m" >&2; done
    echo "       Restore them (git checkout -- <path>) and run this again." >&2
    exit 1
fi

# ---- where the bundle is built ----------------------------------------------
# Assembled, signed and verified in a private folder under the local temp dir,
# and only moved to $DIST_DIR once all of that passed. A checkout inside a
# folder a sync agent manages (iCloud Drive, OneDrive, Dropbox) has that agent
# re-apply com.apple.FinderInfo to directories inside the bundle while the
# build runs, and codesign refuses the whole .app with "resource fork, Finder
# information, or similar detritus not allowed".
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/corvus-gcs-build.XXXXXX")"
APP="$WORK_DIR/$APP_NAME.app"
MNT=""

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
mkdir -p "$BUILD_DIR" "$APP/Contents/MacOS" "$APP/Contents/Resources"
RES="$APP/Contents/Resources"

# ---- 2. the interface and the backend in it ---------------------------------
# rust-toolchain.toml at the repository root pins the Rust, so cargo runs from
# there and rustup fetches it on first use. The fonts and icons are cut out of
# src/vendor by build.rs and the logos are read from assets/, so the binary
# carries them; the languages, the version and the bundled plugins are
# compiled in as well. corvus_link comes in without its `python` feature:
# nothing here links PyO3 or a Python.
echo ">>> Building the interface (release, locked) ..."
GUI_LOG="$BUILD_DIR/cargo-gui-macos.log"
: > "$GUI_LOG"
if ! ( cd "$REPO_DIR" \
        && MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS" CARGO_TARGET_DIR="$BUILD_DIR/cargo-gui" \
           cargo build --release --locked ) >>"$GUI_LOG" 2>&1; then
    echo "ERROR: the interface did not build; log: $GUI_LOG" >&2
    tail -n 30 "$GUI_LOG" >&2 || true
    exit 1
fi
cp -X "$BUILD_DIR/cargo-gui/release/corvus-gcs" "$APP/Contents/MacOS/corvus-gcs"
chmod 0755 "$APP/Contents/MacOS/corvus-gcs"

# ---- 3. files that travel with it -------------------------------------------
# The Sustainable Use License requires that anyone who receives a copy of the
# software also receives a copy of its terms, so the licence ships in the
# bundle. -X: a working copy's extended attributes are nothing the bundle
# should carry.
cp -aX "$REPO_DIR/VERSION" "$RES/VERSION"
cp -aX "$REPO_DIR/LICENSE.md" "$RES/LICENSE.md"
mkdir -p "$RES/assets"
cp -RX "$REPO_DIR/assets/licenses" "$RES/assets/licenses"
find "$RES/assets" -name .DS_Store -delete

# ---- 4. icon ----------------------------------------------------------------
echo ">>> Generating icon ..."
LOGO="$REPO_DIR/assets/CorvusGCS_logo.png"
ICONSET="$BUILD_DIR/corvus-gcs.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$LOGO" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null 2>&1
    sips -z "$((size * 2))" "$((size * 2))" "$LOGO" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null 2>&1
done
iconutil -c icns "$ICONSET" -o "$RES/corvus-gcs.icns" 2>/dev/null \
    || echo "WARNING: iconutil failed; the bundle ships without an icon"
rm -rf "$ICONSET"

# ---- 5. Info.plist (version read from VERSION, never a literal) -------------
echo ">>> Writing Info.plist ..."
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleExecutable</key><string>corvus-gcs</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleIconFile</key><string>corvus-gcs</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSRequiresAquaSystemAppearance</key><false/>
    <key>NSHumanReadableCopyright</key><string>Copyright (c) 2026 Maximilian Bock</string>
</dict>
</plist>
PLIST
printf 'APPL????' > "$APP/Contents/PkgInfo"

# ---- 6. the bundle holds no Python and nothing from this Mac -----------------
# Fails the build rather than shipping a bundle that only runs here, or one
# that carries an interpreter again. python/tools/bundle_trim.py and
# python/tests/test_bundle_smoke.py make the same check on a finished bundle.
foreign_refs() {  # <binary>
    local ref
    otool -L "$1" 2>/dev/null | awk 'NR>1 && !/:$/ {print $1}' | while IFS= read -r ref; do
        case "$ref" in /System/*|/usr/lib/*|@*) ;; /*) printf '%s\n' "$ref" ;; esac
    done
}
host_refs="$(foreign_refs "$APP/Contents/MacOS/corvus-gcs")"
if [ -n "$host_refs" ]; then
    echo "ERROR: the interface links a library outside macOS:" >&2
    printf '%s\n' "$host_refs" | sed 's/^/    /' >&2
    exit 1
fi
python_inside="$(/usr/bin/find "$APP" \( -iname 'python*' -o -iname 'libpython*' -o -name 'site-packages' \
    -o -name 'pip' -o -name '*.pyc' -o -name 'serve.py' -o -name 'corvus_link*' -o -name 'PySide*' \) -print)"
if [ -n "$python_inside" ]; then
    echo "ERROR: the bundle must hold no Python, and it does:" >&2
    printf '%s\n' "$python_inside" | sed 's/^/    /' >&2
    exit 1
fi
if nm "$APP/Contents/MacOS/corvus-gcs" 2>/dev/null | grep -qi 'PyInit_\|Py_Initialize'; then
    echo "ERROR: the interface was built with PyO3 (corvus_link's python feature)" >&2
    exit 1
fi

# ---- 7. sign the bundle -----------------------------------------------------
# Strip the two attributes codesign objects to ("resource fork, Finder
# information, or similar detritus not allowed"); the others are
# system-managed and refuse to be removed.
echo ">>> Stripping Finder metadata ..."
xattr -rd com.apple.FinderInfo "$APP" 2>/dev/null || true
xattr -rd com.apple.ResourceFork "$APP" 2>/dev/null || true

echo ">>> Signing bundle (identity: $CODESIGN_IDENTITY) ..."
: > "$BUILD_DIR/codesign.log"
if ! codesign --force --sign "$CODESIGN_IDENTITY" "$APP" >>"$BUILD_DIR/codesign.log" 2>&1; then
    echo "WARNING: bundle signing failed; see $BUILD_DIR/codesign.log"
    tail -n 5 "$BUILD_DIR/codesign.log" >&2 || true
    detritus="$(xattr -r "$APP" 2>/dev/null \
        | grep -E 'com\.apple\.(FinderInfo|ResourceFork)' | head -n 5 || true)"
    if [ -n "$detritus" ]; then
        echo "       Still carrying Finder metadata after the strip:" >&2
        printf '%s\n' "$detritus" | sed 's/^/         /' >&2
    fi
fi

# ---- 8. verify --------------------------------------------------------------
if [ "$VERIFY" -eq 1 ]; then
    BUNDLED_VERSION="$(cat "$RES/VERSION")"
    [ "$BUNDLED_VERSION" = "$VERSION" ] || {
        echo "ERROR: bundled VERSION ($BUNDLED_VERSION) != $VERSION" >&2; exit 1; }
    [ -f "$RES/LICENSE.md" ] || { echo "ERROR: LICENSE.md is not in the bundle" >&2; exit 1; }

    # The artifact is named from `uname -m`, which answers for the shell and not
    # for what cargo produced; the interface must be $ARCH and nothing else.
    echo ">>> Verifying the bundle is $ARCH ..."
    archs="$(lipo -archs "$APP/Contents/MacOS/corvus-gcs" 2>/dev/null || file -b "$APP/Contents/MacOS/corvus-gcs")"
    if [ "$archs" != "$ARCH" ]; then
        echo "ERROR: corvus-gcs is [$archs]; it must be $ARCH only." >&2
        exit 1
    fi
    echo "    corvus-gcs: $archs"

    # --deep --strict is the check Gatekeeper makes on a downloaded copy, and
    # the only place a broken seal shows before an operator sees "is damaged".
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

# ---- 9. optional .dmg -------------------------------------------------------
# Built in the work folder too, and moved to $DIST_DIR only once the copy
# inside it verified, so a failed build never leaves a .dmg behind.
DMG_WORK="$WORK_DIR/$(basename "$DMG")"
if [ "$MAKE_DMG" -eq 1 ]; then
    echo ">>> Building $(basename "$DMG") ..."
    STAGE="$WORK_DIR/dmg-stage"
    rm -rf "$STAGE"; mkdir -p "$STAGE"
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    # hdiutil create fails now and then on a busy machine ("Resource busy"
    # while it detaches the image it just filled); a retry goes through.
    : > "$BUILD_DIR/hdiutil.log"
    DMG_MADE=0
    for attempt in 1 2 3 4; do
        echo "--- hdiutil create, attempt $attempt" >>"$BUILD_DIR/hdiutil.log"
        if hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" \
                -ov -format ULMO "$DMG_WORK" >>"$BUILD_DIR/hdiutil.log" 2>&1; then
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

# ---- 10. deliver ------------------------------------------------------------
# Only now does the previous build in $DIST_DIR get replaced.
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
# another Mac. grep -c rather than -q: under pipefail a -q that exits early can
# SIGPIPE xattr and read as "no".
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
