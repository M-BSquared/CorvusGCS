#!/usr/bin/env bash
# Build Corvus GCS as a Linux AppImage (Ubuntu/Debian x86_64).
#
# Bundles the native Rust interface (Cargo.toml and src/, built with cargo)
# with the backend (corvus_core) inside it, then packs it with appimagetool.
# No CPython, no pip, no PyInstaller, no Qt, no web engine; corvus_link is built
# without its `python` feature. The build host needs a Rust toolchain (rustup,
# cargo); python3 is used only for small build helpers (the library bundler,
# the icon fallback) and never ends up in the artifact.
#
# The same AppDir is also packed as a Debian package (build-deb.sh, installed
# under /opt/corvus-gcs), so one assembly feeds both Linux artifacts.
#
#   ./build-appimage.sh             AppImage and .deb
#   ./build-appimage.sh --no-deb    AppImage only
#   ./build-appimage.sh --deb-only  .deb only (no appimagetool download)
#   ./build-appimage.sh --fast      the `fast` cargo profile (release code, no LTO) for a local try
#
# The interface is built into the repository's target folder (or
# $CARGO_TARGET_DIR), the one `./run.sh --release` uses, so a second build
# recompiles only what changed.
#
# Version is read exclusively from the repo-root VERSION file — never hardcoded.

set -euo pipefail

WANT_APPIMAGE=1
WANT_DEB=1
PROFILE=release
for arg in "$@"; do
    case "$arg" in
        --no-deb)   WANT_DEB=0 ;;
        --deb-only) WANT_APPIMAGE=0 ;;
        --fast)     PROFILE=fast ;;
        *) echo "ERROR: unknown argument '$arg' (--no-deb, --deb-only, --fast)" >&2; exit 1 ;;
    esac
done

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
BUILD_DIR="$REPO_DIR/build"
APPDIR="$BUILD_DIR/AppDir"
# Artifacts go to dist/, which is gitignored — not to the repo root.
# Two finished .dmg files (600 MB between them) were sitting in the
# checkout from builds two weeks old, because this is where they landed.
OUTPUT="$REPO_DIR/dist/Corvus_GCS-${VERSION}-x86_64.AppImage"

# ---- preflight --------------------------------------------------------------
echo "=== CORVUS GCS — AppImage build ==="
echo "Repo:    $REPO_DIR"
echo "Version: $VERSION  (read from VERSION)"
echo "AppDir:  $APPDIR"
echo "Output:  $OUTPUT"
echo ""

command -v python3 >/dev/null 2>&1 || { echo "ERROR: python3 not found (a build helper, not shipped)" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || {
    echo "ERROR: cargo not found; install Rust from https://rustup.rs" >&2
    exit 1
}

if [ "$(uname -m)" != "x86_64" ]; then
    echo "ERROR: this build targets x86_64; host arch is $(uname -m)" >&2
    exit 1
fi

# Everything the AppDir is assembled from, checked before anything is built.
# A missing LICENSE.md used to stop the build at its `cp`, long after the
# interpreter had been downloaded and the wheels installed, with no hint of
# which step wanted it. Name the file, in the first second.
MISSING=""
for required in \
    VERSION LICENSE.md assets/CorvusGCS_logo.png assets/licenses \
    Cargo.toml Cargo.lock src/main.rs
do
    [ -e "$REPO_DIR/$required" ] || MISSING="$MISSING  $required"
done
if [ -n "$MISSING" ]; then
    echo "ERROR: the AppImage is assembled from files that are not in the repo:" >&2
    for m in $MISSING; do echo "         $m" >&2; done
    echo "       Restore them (git checkout -- <path>) and run this again." >&2
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

# ---- cleanup trap: keep $OUTPUT + cached build/tools, drop $APPDIR on failure
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

# ---- 1. the interface and the backend in it --------------------------------------------------------
# The interface (Cargo.toml and src/), built for this host. rust-toolchain.toml at the repository root pins
# the Rust it needs, so cargo runs from there and rustup fetches it on first
# use. The fonts and icons are cut out of assets/ by its build.rs and the
# logos are read from assets/, so the binary carries them, and so are the
# languages, the version and the bundled plugins. It links the C
# library and the OpenGL, X11 and Wayland libraries are opened at run time from
# the host, which every desktop that can show a window has.
echo ">>> Building the interface (Cargo.toml and src/, $PROFILE profile) ..."
GUI_LOG="$BUILD_DIR/cargo-gui-linux.log"
mkdir -p "$BUILD_DIR"
: > "$GUI_LOG"
TARGET="${CARGO_TARGET_DIR:-$REPO_DIR/target}"
if ! ( cd "$REPO_DIR" \
        && CARGO_TARGET_DIR="$TARGET" cargo build --profile "$PROFILE" --locked --bin corvus-gcs ) >>"$GUI_LOG" 2>&1; then
    echo "ERROR: the interface did not build; log: $GUI_LOG" >&2
    tail -n 30 "$GUI_LOG" >&2 || true
    exit 1
fi
mkdir -p "$APPDIR/usr/bin"
install -m 0755 "$TARGET/$PROFILE/corvus-gcs" "$APPDIR/usr/bin/corvus-gcs-ui"

# ---- 3d. the system libraries the AppDir links but a clean host lacks ---------
# Read from the binary's own DT_NEEDED entries: whatever the interface links
# and the AppDir does not carry is copied in from this host, and the build fails if anything the app cannot
# start without is still unresolved.
echo ">>> Bundling the system libraries the AppDir links ..."
python3 "$REPO_DIR/python/tools/appimage_libs.py" bundle "$APPDIR"

# ---- 2. files that travel with it ------------------------------------------
# The Sustainable Use License requires that anyone who receives a copy of the
# software also receives a copy of its terms, so the licence ships in the
# bundle rather than only in the repository.
echo ">>> Copying the licence and version ..."
cp -a "$REPO_DIR/VERSION" "$APPDIR/VERSION"
cp -a "$REPO_DIR/LICENSE.md" "$APPDIR/LICENSE.md"
mkdir -p "$APPDIR/assets"
cp -a "$REPO_DIR/assets/licenses" "$APPDIR/assets/licenses"
find "$APPDIR/assets" -name .DS_Store -delete

# The bundle holds no Python and no PyO3: the build fails if one came back.
python_inside="$(find "$APPDIR" \( -iname 'python*' -o -iname 'libpython*' -o -name 'site-packages' \
    -o -name 'pip' -o -name '*.pyc' -o -name 'serve.py' -o -name 'corvus_link*' -o -name 'PySide*' \) -print)"
if [ -n "$python_inside" ]; then
    echo "ERROR: the AppDir must hold no Python, and it does:" >&2
    printf '%s\n' "$python_inside" | sed 's/^/    /' >&2
    exit 1
fi

# ---- 3. generate AppRun (no version literal) ---------------------------------
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

# Ensure a writable HOME for the config, tile cache and logs when none is set/unwritable.
if [ -z "${HOME:-}" ] || [ ! -w "${HOME:-}" ]; then
    export HOME="${XDG_CACHE_HOME:-${TMPDIR:-/tmp}}/corvus-gcs"
    mkdir -p "$HOME" 2>/dev/null || true
fi

# exec replaces the shell so SIGINT/SIGTERM reach the interface directly. The
# backend runs inside it, so there is no child to leave behind: the core is
# stopped and its threads joined on every way out (no zombies, no leaked
# sockets).
exec "$APPDIR/usr/bin/corvus-gcs-ui" "$@"
APPRUN_EOF
chmod 0755 "$APPDIR/AppRun"

# ---- 4. desktop entry (no version literal) ---------------------------------
# StartupWMClass is how a dock ties the running window to this entry. An
# integrator (appimaged, AppImageLauncher) installs the file under a new name,
# appimagekit_<hash>-corvus-gcs.desktop, so the window's app id, which
# the interface sets to corvus-gcs (src/app/mod.rs), can no longer find it by
# file name alone.
cat > "$APPDIR/corvus-gcs.desktop" <<'DESKTOP_EOF'
[Desktop Entry]
Name=Corvus GCS
Comment=Ground Control Station for PX4 and ArduPilot aircraft
Exec=corvus-gcs
Icon=corvus-gcs
StartupWMClass=corvus-gcs
Type=Application
Categories=Utility;Science;
Terminal=false
DESKTOP_EOF

# ---- 5. icon: a square 256x256 PNG ------------------------------------------
# Square on purpose. An integrator files the icon under hicolor/<W>x<H>/apps
# from the image's real size, and the logo is 753x756: scaled with its aspect
# kept it came out 254x256, a directory no icon theme looks in, so the
# launcher fell back to a generic icon. Padded onto a transparent square
# instead, and checked, because a wrong size here fails silently later.
LOGO="$REPO_DIR/assets/CorvusGCS_logo.png"
ICON="$APPDIR/corvus-gcs.png"
if [ ! -f "$LOGO" ]; then
    echo "ERROR: assets/CorvusGCS_logo.png not found" >&2
    exit 1
fi
if command -v convert >/dev/null 2>&1; then
    convert "$LOGO" -resize 256x256 -background none -gravity center \
        -extent 256x256 "PNG32:$ICON"
else
    echo ">>> ImageMagick 'convert' not found; fitting the icon with tools/png_icon.py"
    python3 "$REPO_DIR/python/tools/png_icon.py" "$LOGO" "$ICON" 256
fi
ICON_SIZE="$(python3 -c 'import struct, sys; d = open(sys.argv[1], "rb").read(24); print("%dx%d" % struct.unpack(">II", d[16:24]))' "$ICON")"
if [ "$ICON_SIZE" != "256x256" ]; then
    echo "ERROR: the AppImage icon is $ICON_SIZE, not 256x256" >&2
    exit 1
fi
# The same file where the icon theme spec looks, and as .DirIcon, which is
# what file managers and integrators read when they show the AppImage itself.
mkdir -p "$APPDIR/usr/share/icons/hicolor/256x256/apps"
cp -a "$ICON" "$APPDIR/usr/share/icons/hicolor/256x256/apps/corvus-gcs.png"
ln -sf corvus-gcs.png "$APPDIR/.DirIcon"

# ---- 5b. the Debian package ---------------------------------------------------
if [ "$WANT_DEB" -eq 1 ]; then
    "$REPO_DIR/build-deb.sh" "$APPDIR"
fi
if [ "$WANT_APPIMAGE" -eq 0 ]; then
    SUCCESS=1
    echo ""
    echo ">>> Built the .deb only (--deb-only)"
    exit 0
fi

# ---- 8. obtain appimagetool + the static type-2 runtime ---------------------
# AppImageKit's appimagetool is retired and embeds the old runtime, which links
# libfuse2 and the system glibc. The current tool lives in AppImage/appimagetool
# and the runtime in AppImage/type2-runtime: static, FUSE 3 built in. The
# runtime is fetched explicitly and passed with --runtime-file so a cached or
# PATH appimagetool can never fall back to an old one. `rm -rf build/tools`
# refreshes both; CI does not cache them, so it always packs with the latest.
TOOLS_DIR="$BUILD_DIR/tools"
APPIMAGETOOL="$TOOLS_DIR/appimagetool-x86_64.AppImage"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
RUNTIME="$TOOLS_DIR/runtime-x86_64"
RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64"
mkdir -p "$TOOLS_DIR"
if [ -x "$APPIMAGETOOL" ]; then
    echo ">>> Using cached appimagetool"
elif [ -n "${CORVUS_APPIMAGETOOL:-}" ]; then
    APPIMAGETOOL="$CORVUS_APPIMAGETOOL"
    echo ">>> Using appimagetool from CORVUS_APPIMAGETOOL ($APPIMAGETOOL)"
else
    echo ">>> Downloading appimagetool ..."
    download "$APPIMAGETOOL_URL" "$APPIMAGETOOL"
    chmod +x "$APPIMAGETOOL"
fi
if [ -s "$RUNTIME" ]; then
    echo ">>> Using cached AppImage runtime"
else
    echo ">>> Downloading AppImage runtime ..."
    download "$RUNTIME_URL" "$RUNTIME"
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
if ! ARCH=x86_64 "$APPIMAGETOOL" --runtime-file "$RUNTIME" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
    echo ">>> Direct run failed; retrying with APPIMAGE_EXTRACT_AND_RUN=1 ..."
    if ! APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$APPIMAGETOOL" --runtime-file "$RUNTIME" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
        if command -v unsquashfs >/dev/null 2>&1; then
            echo ">>> Extract-and-run failed; manually unsquashfs-ing appimagetool (bypasses FUSE + binfmt) ..."
            if ! run_appimagetool_extracted "$APPIMAGETOOL" --runtime-file "$RUNTIME" "$APPDIR" "$OUTPUT" >>"$AIM_LOG" 2>&1; then
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

# The old runtime is a dynamic ELF (PT_INTERP, libfuse.so.2); the type-2
# runtime is static. Refuse to ship the former.
if ! python3 - "$OUTPUT" <<'PY'
import struct, sys
with open(sys.argv[1], "rb") as f:
    head = f.read(64)
    if head[:4] != b"\x7fELF" or head[4] != 2:
        sys.exit("not a 64-bit ELF runtime")
    phoff, = struct.unpack_from("<Q", head, 32)
    phentsize, phnum = struct.unpack_from("<HH", head, 54)
    f.seek(phoff)
    table = f.read(phentsize * phnum)
for i in range(phnum):
    if struct.unpack_from("<I", table, i * phentsize)[0] == 3:
        sys.exit("dynamically linked runtime (needs libfuse2 and the system glibc)")
PY
then
    echo "ERROR: $OUTPUT carries the old AppImage runtime" >&2
    exit 1
fi

SUCCESS=1
echo ""
echo ">>> Built: $OUTPUT"
echo "    (appimagetool log: $AIM_LOG)"
