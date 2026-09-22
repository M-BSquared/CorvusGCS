#!/usr/bin/env bash
# Corvus GCS — cut a release.
#
# A release is: commit (the pre-commit hook already bumped VERSION) -> tag
# v<VERSION> -> push the branch and the tag to every remote. The tag push is
# what the CI pipelines watch for: .github/workflows/build.yml and
# .gitlab-ci.yml both build the AppImage / .app+.dmg / Windows zip from the
# tag and, on GitHub, attach them to a GitHub Release. There is no separate
# hand-edit of VERSION here — see AGENTS.md, "Version control".
#
# Usage:
#   ./release.sh              # test, tag, push to every remote
#   ./release.sh --no-verify  # skip the local pytest/ruff/frontend gate
#   ./release.sh --dry-run    # print what would happen; push nothing
#
# Re-run safely: an existing local or remote tag for the current VERSION is
# left alone rather than moved, so a second run after fixing a push failure
# does not silently retag.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_DIR"

VERIFY=1
DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --no-verify) VERIFY=0 ;;
        --dry-run)   DRY_RUN=1 ;;
        *)
            echo "release.sh: unknown flag: $arg" >&2
            echo "usage: ./release.sh [--no-verify] [--dry-run]" >&2
            exit 1
            ;;
    esac
done

VERSION="$(cat "$REPO_DIR/VERSION")"
TAG="v$VERSION"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"

echo "=== CORVUS GCS — release ==="
echo "Version: $VERSION"
echo "Tag:     $TAG"
echo "Branch:  $BRANCH"

# A release is cut from a clean tree: an uncommitted change to a TRACKED file
# would ship in the build artifacts (which package the working tree, not just
# HEAD) without ever having been committed to the version it claims to be.
# Untracked files are not part of any build script's copy list (they take
# corvus/, src/, assets/ and VERSION by name) and are ignored here on purpose
# — a root-level working note like AUDIT_FINDINGS.md must not block a release.
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "release.sh: tracked files have uncommitted changes — commit or stash first" >&2
    git status --short --untracked-files=no >&2
    exit 1
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "release.sh: tag $TAG already exists locally — nothing to do for the tag." >&2
    echo "If it never reached a remote, push it by hand: git push <remote> $TAG" >&2
    exit 1
fi

if (( VERIFY )); then
    echo "--- pytest"
    python3 -m pytest -q
    echo "--- ruff"
    ruff check corvus serve.py tests
    echo "--- frontend"
    for f in tests/*.js; do
        node "$f"
    done
else
    echo "--- skipping tests (--no-verify)"
fi

REMOTES="$(git remote)"
if [[ -z "$REMOTES" ]]; then
    echo "release.sh: no git remotes configured — nothing to push to" >&2
    exit 1
fi

echo "--- remotes: $(echo "$REMOTES" | tr '\n' ' ')"

if (( DRY_RUN )); then
    echo "--- dry run: would tag $TAG at $(git rev-parse --short HEAD) and push"
    echo "    $BRANCH and $TAG to: $(echo "$REMOTES" | tr '\n' ' ')"
    exit 0
fi

# Lightweight tag, matching every existing release tag in this repo (they are
# plain commit refs, not annotated tag objects).
git tag "$TAG"
echo "--- tagged $TAG at $(git rev-parse --short "$TAG")"

for remote in $REMOTES; do
    echo "--- pushing $BRANCH and $TAG to $remote"
    git push "$remote" "$BRANCH"
    git push "$remote" "$TAG"
done

echo "=== $TAG pushed to every remote. CI will build and release it. ==="
