#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_NAME="corvus-gcs"
YML="$REPO_DIR/environment.yml"

echo "=== CORVUS GCS — build & run ==="
echo "Repo: $REPO_DIR"
echo ""

# Create conda env if it does not exist
if ! conda env list | grep -qE "^$ENV_NAME\s"; then
    echo ">>> Creating conda environment '$ENV_NAME' …"
    conda env create -f "$YML" -y
else
    echo ">>> Conda environment '$ENV_NAME' already exists."
    echo ">>> Updating dependencies …"
    conda env update -f "$YML"
fi

echo ""
echo ">>> Launching standalone app …"
echo ""

# Run the app inside the conda env
conda run -n "$ENV_NAME" --no-capture-output python "$REPO_DIR/corvus/app.py" "$@"
