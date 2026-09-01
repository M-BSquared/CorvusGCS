#!/usr/bin/env bash
set -euo pipefail
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cleanup() {
    echo ""
    echo ">>> Stopping CORVUS GCS …"
    kill -- -$$ 2>/dev/null || true
    exit 0
}
trap cleanup INT TERM EXIT

echo ">>> Launching CORVUS GCS (existing conda env) …"
conda run -n corvus-gcs --no-capture-output python "$REPO_DIR/corvus/app.py" "$@" &
APP_PID=$!
wait $APP_PID 2>/dev/null
