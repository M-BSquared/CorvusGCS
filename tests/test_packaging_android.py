"""The Android app: one version, tablet mode on by default, wired into CI.

Read off disk as text; the Android SDK is not part of the test environment.
"""
from __future__ import annotations

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
ANDROID = ROOT / "android"
VERSION_RE = re.compile(r"\b20\d\d\.\d\d\.\d\d\b")


def _text(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8")


def test_version_comes_from_the_version_file() -> None:
    gradle = _text(ANDROID / "app" / "build.gradle")
    assert 'file("../../VERSION")' in gradle
    assert "versionName corvusVersion" in gradle


def test_no_version_literal_in_the_android_sources() -> None:
    files = [p for p in ANDROID.rglob("*") if p.is_file() and p.suffix in {".gradle", ".java", ".xml", ".properties"}]
    files.append(ROOT / "build-android.sh")
    assert files
    for path in files:
        assert not VERSION_RE.search(_text(path)), path


def test_user_agent_marker_matches_the_frontend() -> None:
    java = _text(ANDROID / "app" / "src" / "main" / "java" / "de" / "corvusgcs" / "app" / "MainActivity.java")
    assert '" CorvusAndroid/"' in java
    assert "CorvusAndroid" in _text(ROOT / "src" / "js" / "touch.js")
    assert "CorvusAndroid" in _text(ROOT / "src" / "index.html")


def test_cleartext_is_allowed_for_the_lan_server() -> None:
    manifest = _text(ANDROID / "app" / "src" / "main" / "AndroidManifest.xml")
    assert 'android:usesCleartextTraffic="true"' in manifest
    assert "android.permission.INTERNET" in manifest


def test_ci_builds_and_releases_the_apk() -> None:
    ci = _text(ROOT / ".github" / "workflows" / "build.yml")
    assert "./build-android.sh" in ci
    assert re.search(r"release:\s*\n\s*needs: \[[^\]]*android-app", ci)


def test_build_script_names_the_artifact_from_version() -> None:
    script = _text(ROOT / "build-android.sh")
    assert 'VERSION="$(cat "$REPO_DIR/VERSION")"' in script
    assert "Corvus_GCS-${VERSION}-android.apk" in script
