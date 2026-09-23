#!/usr/bin/env bash
# Corvus GCS — run the desktop app from source.
#
#   ./run.sh [PORT] [MAVLINK_CONNECTION]
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

WANT="$("$PY" -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' \
        "$REPO_DIR/pyproject.toml")"
HAVE="$(cat "$STAMP" 2>/dev/null || true)"
if [ "$WANT" != "$HAVE" ]; then
    echo ">>> Installing dependencies (pyproject.toml, group dev) ..."
    # pip >= 25.1 is what reads [dependency-groups]; a fresh venv ships older.
    if "$PY" -m pip install --quiet --upgrade pip \
        && "$PY" -m pip install --quiet --group "$REPO_DIR/pyproject.toml:dev"; then
        echo "$WANT" > "$STAMP"
    elif [ -n "$HAVE" ]; then
        echo "WARNING: dependency update failed (offline?); starting with the set already installed" >&2
    else
        echo "ERROR: dependency install failed; see the pip output above" >&2
        exit 1
    fi
fi

# exec, so SIGINT/SIGTERM reach corvus/app.py's own handlers directly.
exec "$PY" "$REPO_DIR/corvus/app.py" "$@"
