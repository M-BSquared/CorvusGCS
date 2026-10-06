"""The AppImage is packed with the current appimagetool and the static runtime.

AppImageKit's retired appimagetool embeds a runtime that links libfuse2 and the
system glibc, so the AppImage would not start on a distribution without
libfuse2. The build only shows that on the target machine, which is why these
tests read the scripts instead.
"""
from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "build-appimage.sh").read_text(encoding="utf-8")
WORKFLOW = (ROOT / ".github" / "workflows" / "build.yml").read_text(encoding="utf-8")


def test_appimagetool_comes_from_the_current_repo():
    assert "github.com/AppImage/AppImageKit/" not in SCRIPT
    assert "github.com/AppImage/appimagetool/releases/download/continuous/" in SCRIPT


def test_every_pack_passes_the_type2_runtime():
    assert "github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64" in SCRIPT
    calls = [line for line in SCRIPT.splitlines()
             if '"$APPDIR" "$OUTPUT"' in line and "APPIMAGETOOL" in line]
    assert len(calls) == 3
    assert all('--runtime-file "$RUNTIME"' in line for line in calls)


def test_build_refuses_a_dynamic_runtime():
    assert "carries the old AppImage runtime" in SCRIPT


def test_ci_neither_caches_the_tool_nor_needs_libfuse2():
    assert "key: appimagetool" not in WORKFLOW
    assert "libfuse2" not in WORKFLOW
