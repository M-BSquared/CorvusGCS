#!/usr/bin/env bash
# Checks a finished bundle (a folder: an unpacked AppImage, a .app, the Windows
# folder) the way every artifact must be: the files it ships are there, and no
# Python of any kind is inside. Shell only, so a CI job needs no interpreter.
#
#   .github/scripts/check-bundle.sh <bundle-dir> [required-path ...]
#
# A required path is relative to the bundle, e.g. usr/bin/corvus-gcs-ui.

set -euo pipefail

BUNDLE="${1:?usage: check-bundle.sh <bundle-dir> [required-path ...]}"
shift
[ -d "$BUNDLE" ] || { echo "ERROR: $BUNDLE is not a folder" >&2; exit 1; }

fail=0
for required in "$@"; do
    if [ ! -e "$BUNDLE/$required" ]; then
        echo "ERROR: missing from the bundle: $required" >&2
        fail=1
    fi
done

python_inside="$(find "$BUNDLE" \( -iname 'python*' -o -iname 'libpython*' -o -name 'site-packages' \
    -o -name 'pip' -o -name '*.pyc' -o -name 'serve.py' -o -name 'corvus_link*' -o -name 'PySide*' \) -print)"
if [ -n "$python_inside" ]; then
    echo "ERROR: the bundle must hold no Python, and it does:" >&2
    printf '%s\n' "$python_inside" | sed 's/^/    /' >&2
    fail=1
fi

[ "$fail" -eq 0 ] && echo "bundle ok: $BUNDLE"
exit "$fail"
