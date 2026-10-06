"""``tools/appimage_libs.py``: the AppImage carries the libraries a clean desktop lacks.

AppImageHub rejected an AppImage whose Qt could not load its X11 plugin:
``libxkbcommon-x11.so.0`` and ``libxcb-cursor.so.0`` were on the build host
but on no clean machine, so Qt aborted before a window existed. These tests
build small AppDirs from real ELF files and check that the tool copies such a
library in, follows what it links in turn, leaves the host's own core alone,
and fails the build when a library the window needs cannot be found.
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
_TESTS_DIR = Path(__file__).resolve().parent
if str(_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_TESTS_DIR))

from test_qt_bundle import elf  # noqa: E402

_spec = importlib.util.spec_from_file_location("corvus_appimage_libs_tool", ROOT / "tools" / "appimage_libs.py")
al = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
sys.modules[_spec.name] = al
_spec.loader.exec_module(al)

QXCB = "usr/lib/python3.12/site-packages/PySide6/Qt/plugins/platforms/libqxcb.so"
CUPS_PLUGIN = "usr/lib/python3.12/site-packages/PySide6/Qt/plugins/printsupport/libcupsprintersupport.so"


def write(path: Path, data: bytes) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


@pytest.fixture
def appdir(tmp_path: Path) -> Path:
    root = tmp_path / "AppDir"
    write(root / "usr/bin/python3", elf("libc.so.6", "libpython3.12.so.1.0"))
    write(root / "usr/lib/libpython3.12.so.1.0", elf("libc.so.6", "libm.so.6"))
    write(root / QXCB, elf("libQt6XcbQpa.so.6", "libxcb-cursor.so.0", "libxcb.so.1", "libc.so.6"))
    write(root / "usr/lib/python3.12/site-packages/PySide6/Qt/lib/libQt6XcbQpa.so.6",
          elf("libxkbcommon-x11.so.0", "libX11.so.6", "libstdc++.so.6"))
    return root


@pytest.fixture
def host(tmp_path: Path) -> Path:
    lib = tmp_path / "host"
    write(lib / "libxcb-cursor.so.0", elf("libxcb-render.so.0", "libxcb.so.1"))
    write(lib / "libxcb-render.so.0", elf("libxcb.so.1"))
    write(lib / "libxkbcommon-x11.so.0", elf("libxkbcommon.so.0", "libxcb-xkb.so.1"))
    write(lib / "libxkbcommon.so.0", elf("libc.so.6"))
    write(lib / "libxcb-xkb.so.1", elf("libxcb.so.1"))
    return lib


def finder(lib: Path):
    return lambda name: (lib / name) if (lib / name).is_file() else None


def test_the_x11_plugin_libraries_a_clean_desktop_lacks_are_bundled(appdir: Path, host: Path) -> None:
    report = al.bundle(appdir, find=finder(host), log=lambda _m: None)
    bundled = {p.name for p in (appdir / "usr/lib").iterdir()}
    # Direct, and what those link in turn.
    for name in ("libxcb-cursor.so.0", "libxkbcommon-x11.so.0",
                 "libxcb-render.so.0", "libxkbcommon.so.0", "libxcb-xkb.so.1"):
        assert name in bundled
    assert not report.missing
    al.require(report, log=lambda _m: None)


def test_the_hosts_own_core_is_never_bundled(appdir: Path, host: Path) -> None:
    write(host / "libxcb.so.1", elf("libc.so.6"))
    write(host / "libc.so.6", elf())
    al.bundle(appdir, find=finder(host), log=lambda _m: None)
    bundled = {p.name for p in (appdir / "usr/lib").iterdir()}
    assert not bundled & {"libc.so.6", "libxcb.so.1", "libX11.so.6", "libstdc++.so.6"}


@pytest.mark.parametrize("name", [
    "libc.so.6", "ld-linux-x86-64.so.2", "libGL.so.1", "libEGL.so.1", "libdrm.so.2",
    "libX11.so.6", "libxcb.so.1", "libfontconfig.so.1", "libnss3.so", "libwayland-client.so.0",
])
def test_host_provided(name: str) -> None:
    assert al.host_provided(name)


@pytest.mark.parametrize("name", [
    "libxcb-cursor.so.0", "libxkbcommon-x11.so.0", "libxcb-icccm.so.4", "libXcomposite.so.1",
])
def test_not_host_provided(name: str) -> None:
    assert not al.host_provided(name)


def test_a_window_library_nobody_can_supply_fails_the_build(appdir: Path, tmp_path: Path) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()
    report = al.bundle(appdir, find=finder(empty), log=lambda _m: None)
    assert "libxcb-cursor.so.0" in report.missing
    assert QXCB in report.missing["libxcb-cursor.so.0"]
    with pytest.raises(al.BundleError, match="libxcb-cursor.so.0"):
        al.require(report, log=lambda _m: None)


def test_an_optional_plugin_without_its_library_is_only_noted(appdir: Path, host: Path) -> None:
    write(appdir / CUPS_PLUGIN, elf("libcupsimage.so.2"))
    report = al.bundle(appdir, find=finder(host), log=lambda _m: None)
    assert "libcupsimage.so.2" in report.optional_missing
    notes: list[str] = []
    al.require(report, log=notes.append)
    assert any("libcupsimage.so.2" in n for n in notes)


def test_an_optional_plugin_never_pulls_its_libraries_in(appdir: Path, host: Path) -> None:
    gtk = "usr/lib/python3.12/site-packages/PySide6/Qt/plugins/platformthemes/libqgtk3.so"
    write(appdir / gtk, elf("libgtk-3.so.0"))
    write(host / "libgtk-3.so.0", elf("libc.so.6"))
    report = al.bundle(appdir, find=finder(host), log=lambda _m: None)
    assert "libgtk-3.so.0" not in report.copied
    assert not (appdir / "usr/lib/libgtk-3.so.0").exists()
    assert gtk in report.optional_missing["libgtk-3.so.0"]
    al.require(report, log=lambda _m: None)


def test_a_library_the_appdir_carries_is_not_copied_again(appdir: Path, host: Path) -> None:
    write(appdir / "usr/lib/libxcb-cursor.so.0", elf("libxcb.so.1"))
    report = al.bundle(appdir, find=finder(host), log=lambda _m: None)
    assert "libxcb-cursor.so.0" not in report.copied


def test_ldconfig_listing_is_read_for_64_bit_entries() -> None:
    class Out:
        stdout = (
            "1234 libs found in cache `/etc/ld.so.cache'\n"
            "\tlibxcb-cursor.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libxcb-cursor.so.0\n"
            "\tlibxcb-cursor.so.0 (libc6) => /lib/i386-linux-gnu/libxcb-cursor.so.0\n"
            "\tlibfoo.so.1 (libc6) => /lib/i386-linux-gnu/libfoo.so.1\n"
        )
    index = al.ldconfig_index(run=lambda *a, **k: Out())
    assert index == {"libxcb-cursor.so.0": Path("/lib/x86_64-linux-gnu/libxcb-cursor.so.0")}


def test_the_appimage_build_runs_it_after_the_qt_trim() -> None:
    script = (ROOT / "build-appimage.sh").read_text(encoding="utf-8")
    trim = script.index("tools/qt_bundle.py\" prune")
    libs = script.index("tools/appimage_libs.py\" bundle \"$APPDIR\"")
    pack = script.index("APPIMAGETOOL")
    assert trim < libs < pack


def test_ci_installs_what_the_tool_copies() -> None:
    workflow = (ROOT / ".github" / "workflows" / "build.yml").read_text(encoding="utf-8")
    for package in ("libxcb-cursor0", "libxkbcommon-x11-0", "libxcb-icccm4",
                    "libxcb-image0", "libxcb-keysyms1", "libxcb-render-util0"):
        assert package in workflow
