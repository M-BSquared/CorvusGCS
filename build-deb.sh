#!/usr/bin/env bash
# Build Corvus GCS as a Debian package (amd64) from an assembled AppDir.
#
# build-appimage.sh assembles the AppDir (the Rust interface with its backend
# inside, the licence) and calls this with its path, so the .deb carries
# exactly the payload the AppImage does. It installs under /opt/corvus-gcs, adds
# /usr/bin/corvus-gcs, the desktop entry and the icon, and depends only on what
# every desktop already has: the C library, OpenGL, and the X11 or Wayland
# client libraries the interface opens at run time.
#
#   ./build-deb.sh <AppDir>
#
# Needs dpkg-deb (present on Debian and Ubuntu, and on the CI runner).
# Version is read exclusively from the repo-root VERSION file.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
APPDIR="${1:-$REPO_DIR/build/AppDir}"
STAGE="$REPO_DIR/build/deb"
OUTPUT="$REPO_DIR/dist/Corvus_GCS-${VERSION}-amd64.deb"

command -v dpkg-deb >/dev/null 2>&1 || { echo "ERROR: dpkg-deb not found (apt install dpkg)" >&2; exit 1; }
[ -x "$APPDIR/AppRun" ] || { echo "ERROR: $APPDIR is not an assembled AppDir (run build-appimage.sh)" >&2; exit 1; }

echo ">>> Packing $OUTPUT"
rm -rf "$STAGE"
mkdir -p "$STAGE/opt" "$STAGE/DEBIAN" "$STAGE/usr/bin" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/icons/hicolor/256x256/apps" \
    "$STAGE/usr/share/doc/corvus-gcs" "$REPO_DIR/dist"

# A copy, because the AppImage step that follows packs the original. The
# AppDir is a few hundred MB, so this is the slow line of the package.
cp -a "$APPDIR" "$STAGE/opt/corvus-gcs"
rm -f "$STAGE/opt/corvus-gcs/corvus-gcs.desktop" "$STAGE/opt/corvus-gcs/.DirIcon" \
    "$STAGE/opt/corvus-gcs/corvus-gcs.png"

cat > "$STAGE/usr/bin/corvus-gcs" <<'WRAP'
#!/bin/sh
APPDIR=/opt/corvus-gcs
export APPDIR
exec "$APPDIR/AppRun" "$@"
WRAP
chmod 0755 "$STAGE/usr/bin/corvus-gcs"

cat > "$STAGE/usr/share/applications/corvus-gcs.desktop" <<'DESKTOP'
[Desktop Entry]
Name=Corvus GCS
Comment=Ground Control Station for PX4 and ArduPilot aircraft
Exec=corvus-gcs
Icon=corvus-gcs
StartupWMClass=corvus-gcs
Type=Application
Categories=Utility;Science;
Terminal=false
DESKTOP
install -m 0644 "$APPDIR/corvus-gcs.png" "$STAGE/usr/share/icons/hicolor/256x256/apps/corvus-gcs.png"
# The Sustainable Use License travels with the package.
install -m 0644 "$REPO_DIR/LICENSE.md" "$STAGE/usr/share/doc/corvus-gcs/copyright"

SIZE_KB="$(du -sk "$STAGE" | cut -f1)"
cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: corvus-gcs
Version: $VERSION
Section: science
Priority: optional
Architecture: amd64
Installed-Size: $SIZE_KB
Maintainer: Maximilian Böck <maximilian.boeck.1999@web.de>
Depends: libc6 (>= 2.35), libgl1, libudev1, libxkbcommon0, libx11-6 | libwayland-client0
Homepage: https://github.com/M-BSquared/CorvusGCS
Description: Ground Control Station for PX4 and ArduPilot aircraft
 Corvus GCS runs offline on a laptop and talks to the autopilot over serial,
 UDP or TCP. It is one native program and needs no network at run time.
CONTROL

cat > "$STAGE/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database -q /usr/share/applications || true; fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true; fi
exit 0
POST
cp "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/postrm"
chmod 0755 "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/postrm"

chmod -R go-w "$STAGE"
rm -f "$OUTPUT"
# xz keeps the package close to the AppImage's size; zstd, the default, is
# not read by dpkg older than 1.21.18.
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$OUTPUT" >/dev/null
rm -rf "$STAGE"
echo ">>> Built: $OUTPUT"
