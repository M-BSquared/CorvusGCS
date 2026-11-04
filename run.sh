#!/usr/bin/env bash
# Corvus GCS — run the desktop app from source.
#
#   ./run.sh [corvus-gcs flags]    e.g. ./run.sh --page motors
#   ./run.sh --web [PORT] [MAVLINK_CONNECTION]   # the Qt window around the web UI
#   ./run.sh --desktop-entry      # Linux: add this checkout to the app launcher
#
# The desktop app is the native Rust interface (Cargo.toml and src/). It starts the
# backend (serve.py, from .venv) as its child and takes it down on every way
# out. It is built with cargo from the repository root, whose rust-toolchain.toml
# pins the Rust it needs; rustup installs that on the first run.
#
# The first run creates .venv from a Python >= 3.12 and installs the `dev`
# group from pyproject.toml into it. Later runs re-install only when
# pyproject.toml has changed since, so relaunching on a field laptop needs no
# network — and when an update is due but the network is not there, the app
# still starts on the set already installed.
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
# checkout's window carries (desktop_id in corvus/app_icon.py).
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

# The web interface in its Qt window, kept while the native one is completed.
if [ "${1:-}" = "--web" ]; then
    shift
    # exec, so SIGINT/SIGTERM reach corvus/app.py's own handlers directly.
    exec "$PY" "$REPO_DIR/python/corvus/app.py" "$@"
fi

command -v cargo >/dev/null 2>&1 || {
    echo "ERROR: cargo not found; install Rust from https://rustup.rs" >&2
    exit 1
}
# cargo is a no-op when nothing changed. Its own output goes to the terminal:
# the first build compiles for a couple of minutes.
( cd "$REPO_DIR" && cargo build --release --quiet )
BIN="$REPO_DIR/target/release/corvus-gcs"
[ -x "$BIN" ] || BIN="$BIN.exe"
# exec, so SIGINT/SIGTERM reach the interface's own handlers directly, which
# take the backend down.
CORVUS_APP_ID=corvus-gcs-dev exec "$BIN" --root "$REPO_DIR" --python "$PY" "$@"
