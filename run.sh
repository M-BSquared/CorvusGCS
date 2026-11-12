#!/usr/bin/env bash
# Corvus GCS — run the desktop app from source.
#
#   ./run.sh [corvus-gcs flags]    e.g. ./run.sh --page motors
#   ./run.sh --release [flags]     the optimised build a bundle ships (slow to build)
#   ./run.sh --fast [flags]        release code without LTO (a few minutes, not ten)
#   ./run.sh --venv               # only set up .venv for the tests and python/tools
#   ./run.sh --desktop-entry      # Linux: add this checkout to the app launcher
#
# The desktop app is the native Rust interface (Cargo.toml and src/), built with
# cargo from the repository root, whose rust-toolchain.toml pins the Rust it
# needs; rustup installs that on the first run. The backend (corvus_core) is
# inside it: nothing else is needed, and no Python is started.
#
# Only --venv creates .venv (from a Python >= 3.12), installs the `dev` group of
# python/pyproject.toml and builds corvus_link into it, for the tests and the
# simulator scripts in python/tools. Later runs re-install only when
# pyproject.toml or src/link changed, and an update that fails offline keeps the
# set already installed.
#
# $CORVUS_PYTHON picks the interpreter the venv is created from.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$REPO_DIR/.venv"
PY="$VENV/bin/python"
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) PY="$VENV/Scripts/python.exe" ;;
esac
STAMP="$VENV/.corvus-deps"

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

py_ok() {  # <interpreter> -> 0 if it is Python >= 3.12
    "$1" -c 'import sys; sys.exit(sys.version_info < (3, 12))' >/dev/null 2>&1
}

ensure_venv() {

if [ ! -x "$PY" ]; then
    BASE="${CORVUS_PYTHON:-}"
    if [ -z "$BASE" ]; then
        # 3.12 first: it is the version CI tests and the builds ship.
        for cand in python3.12 python3.13 python3.14 python3 python; do
            path="$(command -v "$cand" 2>/dev/null || true)"
            if [ -n "$path" ] && py_ok "$path"; then BASE="$path"; break; fi
        done
    fi
    if [ -z "$BASE" ] || ! py_ok "$BASE"; then
        echo "ERROR: Corvus GCS needs Python >= 3.12; none found on PATH." >&2
        echo "       Install one, or point at it: CORVUS_PYTHON=/path/to/python3 ./run.sh" >&2
        exit 1
    fi
    echo ">>> Creating .venv with $BASE ..."
    "$BASE" -m venv "$VENV"
fi

# The stamp covers corvus_link's sources too, so a change to the Rust side
# rebuilds the extension on the next start.
WANT="$("$PY" - "$REPO_DIR" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
h = hashlib.sha256((root / "python" / "pyproject.toml").read_bytes())
crate = root / "src" / "link"
for path in sorted(crate.rglob("*")):
    if path.is_file() and "target" not in path.relative_to(crate).parts:
        h.update(str(path.relative_to(crate)).encode())
        h.update(path.read_bytes())
print(h.hexdigest())
PY
)"
HAVE="$(cat "$STAMP" 2>/dev/null || true)"
if [ "$WANT" != "$HAVE" ]; then
    echo ">>> Installing dependencies (pyproject.toml, group dev) and building corvus_link ..."
    # pip >= 25.1 is what reads [dependency-groups]; a fresh venv ships older.
    if "$PY" -m pip install --quiet --upgrade pip \
        && "$PY" -m pip install --quiet --group "$REPO_DIR/python/pyproject.toml:dev" \
        && "$PY" -m pip install --quiet "$REPO_DIR/src/link"; then
        echo "$WANT" > "$STAMP"
    elif [ -n "$HAVE" ]; then
        echo "WARNING: dependency update failed (offline?); starting with the set already installed" >&2
    else
        echo "ERROR: dependency install failed; see the pip output above" >&2
        exit 1
    fi
fi
}

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

if [ "${1:-}" = "--venv" ]; then
    ensure_venv
    echo ">>> .venv is ready: $PY"
    exit 0
fi

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
