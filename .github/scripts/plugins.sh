#!/usr/bin/env bash
# Packs the plugin libraries the platform's build script made into dist/plugins and
# proves that the finished program loads every one of them: a plugin it refuses must
# not be published. Opens no window.
#
#   .github/scripts/plugins.sh <program>

set -euo pipefail

PROGRAM="${1:?usage: plugins.sh <the finished corvus-gcs program>}"
tools() { cargo run -q --release --locked -p corvus_tools --bin corvus-build -- "$@"; }

rm -rf dist/plugins build/plugin-check
tools plugin-pack --target "${CARGO_TARGET_DIR:-target}/release" --out dist/plugins
tools plugin-unpack dist/plugins build/plugin-check
"$PROGRAM" --check-plugins build/plugin-check
