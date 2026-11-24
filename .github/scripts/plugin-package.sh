#!/usr/bin/env bash
# Builds one plugin for a published version of Corvus GCS, in a checkout of that version with the
# plugin's own source at plugins/<id>, and proves that the PUBLISHED program loads it. What the
# program would refuse is not published. Opens no window. Run by .github/workflows/plugin.yml.
#
#   .github/scripts/plugin-package.sh <id> <tag of Corvus GCS>      leaves dist/plugins
#
# The program and all libraries are built in the same cargo call the platform's build script
# uses: apart from each other cargo compiles the crates they share differently, and the program
# refuses a library whose types are not its own. --locked: a plugin can use what Cargo.lock of
# that version holds, and nothing else.

set -euo pipefail

ID="${1:?usage: plugin-package.sh <id> <tag>}"
TAG="${2:?usage: plugin-package.sh <id> <tag>}"
WORK="${RUNNER_TEMP:-build}/corvus-published"
TARGET="${CARGO_TARGET_DIR:-target}"

cargo build --release --locked --bin corvus-gcs --lib
tools() { cargo run -q --release --locked -p corvus_tools --bin corvus-build -- "$@"; }
rm -rf dist/plugins build/plugin-check "$WORK"
mkdir -p "$WORK"
tools plugin-pack --only "$ID" --target "$TARGET/release" --out dist/plugins
tools plugin-unpack dist/plugins build/plugin-check

fetch() { gh release download "$TAG" --repo M-BSquared/CorvusGCS --dir "$WORK" --pattern "$1"; }
case "$(uname -s)" in
    Linux)
        fetch 'Corvus_GCS-*-x86_64.AppImage'
        chmod +x "$WORK"/*.AppImage
        ( cd "$WORK" && ./*.AppImage --appimage-extract >/dev/null )
        PROGRAM="$WORK/squashfs-root/usr/bin/corvus-gcs-ui"
        ;;
    Darwin)
        fetch 'Corvus_GCS-*-macOS-*.dmg'
        hdiutil attach -quiet -nobrowse -readonly -mountpoint "$WORK/volume" "$WORK"/*.dmg
        trap 'hdiutil detach -quiet "$WORK/volume" || true' EXIT
        PROGRAM="$WORK/volume/Corvus GCS.app/Contents/MacOS/corvus-gcs"
        ;;
    *)
        fetch 'Corvus_GCS-*-windows-x64.zip'
        ( cd "$WORK" && 7z x -y ./*.zip >/dev/null )
        PROGRAM="$WORK/Corvus GCS/Corvus GCS.exe"
        ;;
esac
"$PROGRAM" --check-plugins build/plugin-check
