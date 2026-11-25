#!/usr/bin/env bash
# Builds one plugin in a checkout of a published Corvus GCS, with the plugin's own source at
# plugins/<id>, and proves that the PUBLISHED program of that version loads it. The version
# checked out is the oldest one the plugin says it runs in (min_corvus), so the claim is tried
# and not only made: what does not build against it, or does not load in it, is not published.
# Opens no window. Run by .github/workflows/plugin.yml on Linux.
#
#   .github/scripts/plugin-package.sh <id> <tag of Corvus GCS>      leaves dist/plugins

set -euo pipefail

ID="${1:?usage: plugin-package.sh <id> <tag>}"
TAG="${2:?usage: plugin-package.sh <id> <tag>}"
WORK="${RUNNER_TEMP:-build}/corvus-published"

tools() { cargo run -q --release --locked -p corvus_tools --bin corvus-build -- "$@"; }
rm -rf dist/plugins build/plugin-check "$WORK"
mkdir -p "$WORK"
tools plugin-pack --build --only "$ID" --out dist/plugins
tools plugin-unpack dist/plugins build/plugin-check

gh release download "$TAG" --repo M-BSquared/CorvusGCS --dir "$WORK" --pattern 'Corvus_GCS-*-x86_64.AppImage'
chmod +x "$WORK"/*.AppImage
( cd "$WORK" && ./*.AppImage --appimage-extract >/dev/null )
"$WORK/squashfs-root/usr/bin/corvus-gcs-ui" --check-plugins build/plugin-check
