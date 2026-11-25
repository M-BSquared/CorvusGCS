#!/usr/bin/env bash
# Builds the plugin modules, installs them as the store does and proves that the finished
# program loads every one of them. Publishes nothing and opens no window. A module is the
# same on every system; each platform's job runs this to try its own program with them.
#
#   .github/scripts/plugins.sh <program>

set -euo pipefail

PROGRAM="${1:?usage: plugins.sh <the finished corvus-gcs program>}"
tools() { cargo run -q --release --locked -p corvus_tools --bin corvus-build -- "$@"; }

rm -rf dist/plugins build/plugin-check
tools plugin-pack --build --out dist/plugins
tools plugin-unpack dist/plugins build/plugin-check
"$PROGRAM" --check-plugins build/plugin-check
