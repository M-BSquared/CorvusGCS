#!/usr/bin/env bash
# Corvus GCS — build the Android app (an APK) from android/.
#
# The Android app is a full screen shell around the Corvus GCS server running
# on a computer on the same network (see docs/guide/security.html, "A second
# screen on the network"). Its user agent carries CorvusAndroid, so the web app
# starts in tablet mode. Nothing of the Python side is bundled.
#
# Needs a JDK 17+, the Android SDK (ANDROID_HOME or ANDROID_SDK_ROOT) and
# either android/gradlew or `gradle` on PATH.
#
# Usage:
#   ./build-android.sh [--server <url>]
#
#   --server <url>   address the app opens first, e.g. http://192.168.1.20:8080/
#                    (without it the app asks on first start)
#
# Signing: with CORVUS_KEYSTORE (+ CORVUS_KEYSTORE_PASSWORD, CORVUS_KEY_ALIAS,
# CORVUS_KEY_PASSWORD) the APK is signed with that key, otherwise with the
# debug key so it still installs.
#
# Version is read exclusively from the repo-root VERSION file; never hardcoded.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="$(cat "$REPO_DIR/VERSION")"
DIST="${CORVUS_DIST:-$REPO_DIR/dist}"
SERVER=""

while [ $# -gt 0 ]; do
    case "$1" in
        --server) SERVER="${2:?--server needs a url}"; shift 2 ;;
        *) echo "ERROR: unknown argument '$1'" >&2; exit 1 ;;
    esac
done

echo "=== CORVUS GCS — Android build ==="
echo "Version: $VERSION  (read from VERSION)"

command -v java >/dev/null 2>&1 || { echo "ERROR: no JDK found (need 17+)." >&2; exit 1; }
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
[ -n "$SDK" ] && [ -d "$SDK" ] || {
    echo "ERROR: Android SDK not found; set ANDROID_HOME." >&2
    exit 1
}

if [ -x "$REPO_DIR/android/gradlew" ]; then
    GRADLE=("$REPO_DIR/android/gradlew")
elif command -v gradle >/dev/null 2>&1; then
    GRADLE=(gradle)
else
    echo "ERROR: neither android/gradlew nor gradle on PATH." >&2
    exit 1
fi

ARGS=(--no-daemon -q assembleRelease)
[ -n "$SERVER" ] && ARGS+=("-PcorvusServer=$SERVER")

( cd "$REPO_DIR/android" && ANDROID_HOME="$SDK" "${GRADLE[@]}" "${ARGS[@]}" )

APK="$(ls "$REPO_DIR"/android/app/build/outputs/apk/release/*.apk | head -n 1)"
[ -f "$APK" ] || { echo "ERROR: no APK produced." >&2; exit 1; }

mkdir -p "$DIST"
OUT="$DIST/Corvus_GCS-${VERSION}-android.apk"
cp "$APK" "$OUT"
echo ">>> build-android.sh done — artifact: $OUT"
