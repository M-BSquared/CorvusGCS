#!/usr/bin/env bash
# Corvus GCS — build the distributable artifact(s) for the current host.
#
# One entry point for humans and CI. Packaging is inherently per-platform, so
# this script dispatches to the platform build and never pretends to
# cross-build: a Linux AppImage needs a Linux host, a macOS .app needs a Mac.
#
#   Linux  x86_64  ->  ./build-appimage.sh    ->  Corvus_GCS-<version>-x86_64.AppImage
#   macOS  arm64/x86_64 -> ./build-macos-app.sh -> dist/Corvus GCS.app
#                                              (+ Corvus_GCS-<version>-macOS-<arch>.dmg)
#
# Usage:
#   ./build.sh [--dmg] [--no-verify] [--python <path>] [-- <extra platform args>]
#
# Unknown flags are passed through to the platform script. Flags that do not
# apply to the current platform are dropped with a note, so the same CI
# invocation (`./build.sh --dmg`) works on both.
#
# Version is read exclusively from the repo-root VERSION file — never hardcoded.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
OS="$(uname -s)"
ARCH="$(uname -m)"

echo "=== CORVUS GCS — build ==="
echo "Version: $VERSION  (read from VERSION)"
echo "Host:    $OS $ARCH"
echo ""

case "$OS" in
    Linux)
        SCRIPT="$REPO_DIR/build-appimage.sh"
        ARTIFACT="$REPO_DIR/Corvus_GCS-${VERSION}-x86_64.AppImage"
        # --dmg is macOS-only; the AppImage script rejects unknown args.
        ARGS=()
        for arg in "$@"; do
            case "$arg" in
                --dmg) echo ">>> note: --dmg is macOS-only, ignoring on Linux" ;;
                *)     ARGS+=("$arg") ;;
            esac
        done
        ;;
    Darwin)
        SCRIPT="$REPO_DIR/build-macos-app.sh"
        ARTIFACT="$REPO_DIR/dist/Corvus GCS.app"
        ARGS=("$@")
        ;;
    *)
        echo "ERROR: unsupported build host '$OS'." >&2
        echo "       Corvus GCS packages for Linux (AppImage) and macOS (.app)." >&2
        exit 1
        ;;
esac

[ -x "$SCRIPT" ] || chmod +x "$SCRIPT" 2>/dev/null || true
[ -f "$SCRIPT" ] || { echo "ERROR: $SCRIPT not found" >&2; exit 1; }

echo ">>> Running $(basename "$SCRIPT") ${ARGS[*]:-}"
echo ""
"$SCRIPT" ${ARGS[@]+"${ARGS[@]}"}

[ -e "$ARTIFACT" ] || { echo "ERROR: expected artifact missing: $ARTIFACT" >&2; exit 1; }
echo ""
echo ">>> build.sh done — artifact: $ARTIFACT"
