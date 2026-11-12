#!/usr/bin/env bash
# Corvus GCS — run the desktop app from source.
#
#   ./run.sh [corvus-gcs flags]    e.g. ./run.sh --page motors
#   ./run.sh --release [flags]     the optimised build a bundle ships (slow to build)
#   ./run.sh --fast [flags]        release code without LTO (a few minutes, not ten)
#   ./run.sh --desktop-entry      # Linux: add this checkout to the app launcher
#
# The desktop app is the native Rust interface (Cargo.toml and src/), built with
# cargo from the repository root, whose rust-toolchain.toml pins the Rust it
# needs; rustup installs that on the first run. The backend (corvus_core) is
# inside it: nothing else is needed, and no Python is involved anywhere.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Linux: `./run.sh --desktop-entry` writes a launcher entry for this checkout,
# so the applications grid and the dock show Corvus GCS with its icon. A
# Wayland dock has no other way to find one. Its paths are this checkout's,
# resolved here, and an existing entry is brought up to date on every run, so
# it follows the checkout when it moves. StartupWMClass is the id a source
# checkout's window carries (CORVUS_APP_ID below).
ENTRY="${XDG_DATA_HOME:-$HOME/.local/share}/applications/corvus-gcs-dev.desktop"
desktop_entry() {
    local exec_path
    exec_path="$(printf '%s' "$REPO_DIR/run.sh" | sed -e 's/[\\"`$]/\\&/g' -e 's/%/%%/g')"
    printf '%s\n' \
        "[Desktop Entry]" \
        "Type=Application" \
        "Name=Corvus GCS (dev)" \
        "Comment=Corvus GCS from the checkout in $REPO_DIR" \
        "Exec=\"$exec_path\"" \
        "Icon=$REPO_DIR/assets/CorvusGCS_logo.png" \
        "StartupWMClass=corvus-gcs-dev" \
        "Categories=Utility;Science;" \
        "Terminal=false"
}
sync_desktop_entry() {
    local want
    want="$(desktop_entry)"
    [ "$want" = "$(cat "$ENTRY" 2>/dev/null)" ] && return 0
    mkdir -p "$(dirname "$ENTRY")"
    printf '%s\n' "$want" > "$ENTRY"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$(dirname "$ENTRY")" || true
    fi
}
if [ "${1:-}" = "--desktop-entry" ]; then
    if [ "$(uname -s)" != "Linux" ]; then
        echo "ERROR: --desktop-entry is for Linux desktops" >&2
        exit 1
    fi
    sync_desktop_entry
    echo ">>> Desktop entry: $ENTRY"
    exit 0
fi
if [ "$(uname -s)" = "Linux" ] && [ -f "$ENTRY" ]; then
    sync_desktop_entry || true
fi

# Leading build flags, before anything meant for the program.
PROFILE=dev
while [ $# -gt 0 ]; do
    case "$1" in
        --release) PROFILE=release; shift ;;
        --fast)    PROFILE=fast; shift ;;
        *) break ;;
    esac
done
PROFILE="${CORVUS_PROFILE:-$PROFILE}"

command -v cargo >/dev/null 2>&1 || {
    echo "ERROR: cargo not found; install Rust from https://rustup.rs" >&2
    exit 1
}
# cargo is a no-op when nothing changed. Its own output goes to the terminal:
# the first build compiles for a few minutes, a rebuild after an edit for
# seconds. Profiles are in Cargo.toml: dev (the default) keeps the program
# lightly optimised and rebuilds fast, `fast` is release code without LTO,
# release is exactly what a bundle ships.
if [ "$PROFILE" = "dev" ]; then PROFILE_ARGS=(); DIR=debug; else PROFILE_ARGS=(--profile "$PROFILE"); DIR="$PROFILE"; fi
( cd "$REPO_DIR" && cargo build ${PROFILE_ARGS[@]+"${PROFILE_ARGS[@]}"} --bin corvus-gcs --quiet )
TARGET="${CARGO_TARGET_DIR:-$REPO_DIR/target}"
BIN="$TARGET/$DIR/corvus-gcs"
[ -x "$BIN" ] || BIN="$BIN.exe"
# exec, so SIGINT/SIGTERM reach the interface's own handlers directly, which
# stop the core and join its threads.
CORVUS_APP_ID=corvus-gcs-dev exec "$BIN" "$@"
