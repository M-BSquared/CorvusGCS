"""``tools/qt_bundle.py``: the PySide6 a packaged build ships, trimmed safely.

The trim decides from the binaries' own load commands what Corvus can load,
and the build fails when a binary left behind links one that was taken out.
Both halves are tested here on small, real Mach-O and ELF files written by the
tests, so they run on every CI host and need neither otool nor binutils.
"""
from __future__ import annotations

import importlib.util
import os
import struct
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]

_spec = importlib.util.spec_from_file_location("corvus_qt_bundle_tool", ROOT / "tools" / "qt_bundle.py")
qb = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
# Registered before it runs: a dataclass looks its own module up by name.
sys.modules[_spec.name] = qb
_spec.loader.exec_module(qb)


# ---- tiny binaries ----------------------------------------------------------

def elf(*needed: str) -> bytes:
    """A 64-bit little-endian ELF whose dynamic section lists *needed*."""
    base = 0x400000
    phoff, phnum = 64, 2
    dyn_off = phoff + 56 * phnum
    strtab = b"\0"
    offsets = []
    for name in needed:
        offsets.append(len(strtab))
        strtab += name.encode() + b"\0"
    entries = [(1, o) for o in offsets] + [(5, 0), (0, 0)]
    str_off = dyn_off + 16 * len(entries)
    entries[-2] = (5, base + str_off)
    dynamic = b"".join(struct.pack("<qQ", tag, value) for tag, value in entries)
    total = str_off + len(strtab)
    header = (b"\x7fELF" + bytes([2, 1, 1, 0]) + bytes(8)
              + struct.pack("<HHIQQQIHHHHHH", 3, 0x3E, 1, 0, phoff, 0, 0, 64, 56, phnum, 64, 0, 0))
    load = struct.pack("<IIQQQQQQ", 1, 5, 0, base, base, total, total, 0x1000)
    dyn = struct.pack("<IIQQQQQQ", 2, 6, dyn_off, base + dyn_off, base + dyn_off,
                      len(dynamic), len(dynamic), 8)
    return header + load + dyn + dynamic + strtab


def macho(*needed: str, weak: tuple[str, ...] = ()) -> bytes:
    """A thin 64-bit Mach-O that loads *needed* (and *weak*, weakly)."""
    commands = struct.pack("<II16s", 0x1B, 24, bytes(16))           # LC_UUID, not a library
    for cmd, names in ((0xC, needed), (0x80000018, weak)):
        for name in names:
            raw = name.encode() + b"\0"
            size = (24 + len(raw) + 7) // 8 * 8
            commands += struct.pack("<IIIIII", cmd, size, 24, 2, 0x10000, 0x10000)
            commands += raw.ljust(size - 24, b"\0")
    ncmds = 1 + len(needed) + len(weak)
    header = struct.pack("<IiiIIIII", 0xFEEDFACF, 0x0100000C, 0, 6, ncmds, len(commands), 0, 0)
    return header + commands


def fat(*slices: bytes) -> bytes:
    """A universal binary holding *slices*."""
    out = struct.pack(">II", 0xCAFEBABE, len(slices))
    offset = 4096
    table, body = b"", b""
    for data in slices:
        table += struct.pack(">iiIII", 0x0100000C, 0, offset + len(body), len(data), 12)
        body += data.ljust((len(data) + 4095) // 4096 * 4096, b"\0")
    return (out + table).ljust(offset, b"\0") + body


def _write(path: Path, data: bytes | str = b"") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)
    return path


# ---- the readers ------------------------------------------------------------

def test_elf_needed_entries_are_read(tmp_path):
    path = _write(tmp_path / "lib.so", elf("libQt6Core.so.6", "libc.so.6"))
    assert qb.binary_kind(path) == "elf"
    assert qb.binary_deps(path) == ["libQt6Core.so.6", "libc.so.6"]


def test_macho_load_commands_are_read_weak_ones_too(tmp_path):
    path = _write(tmp_path / "QtGui", macho("@rpath/QtCore.framework/Versions/A/QtCore",
                                            "/usr/lib/libSystem.B.dylib",
                                            weak=("@rpath/libavcodec.61.dylib",)))
    assert qb.binary_kind(path) == "macho"
    assert qb.binary_deps(path) == ["@rpath/QtCore.framework/Versions/A/QtCore",
                                    "/usr/lib/libSystem.B.dylib",
                                    "@rpath/libavcodec.61.dylib"]


def test_a_universal_binary_answers_for_every_slice(tmp_path):
    path = _write(tmp_path / "QtWidgets", fat(macho("@rpath/QtCore.framework/Versions/A/QtCore"),
                                              macho("@rpath/QtGui.framework/Versions/A/QtGui",
                                                    "@rpath/QtCore.framework/Versions/A/QtCore")))
    assert qb.binary_deps(path) == ["@rpath/QtCore.framework/Versions/A/QtCore",
                                    "@rpath/QtGui.framework/Versions/A/QtGui"]


def test_a_java_class_file_is_not_a_universal_binary(tmp_path):
    path = _write(tmp_path / "Thing.class", b"\xca\xfe\xba\xbe\x00\x00\x00\x41" + bytes(64))
    assert qb.binary_deps(path) == []


def test_text_is_not_a_binary(tmp_path):
    path = _write(tmp_path / "qt.conf", "[Paths]\nPrefix=..\n")
    assert qb.binary_kind(path) == ""
    assert qb.binary_deps(path) == []


def test_a_truncated_binary_is_a_build_error(tmp_path):
    path = _write(tmp_path / "broken.so", elf("libQt6Core.so.6")[:70])
    with pytest.raises(qb.BundleError, match="broken.so"):
        qb.binary_deps(path)


@pytest.mark.skipif(os.name == "nt", reason="the interpreter is a PE file on Windows")
def test_the_readers_understand_this_interpreter():
    """A real binary from the host: libc or libSystem, or libpython, at least."""
    deps = qb.binary_deps(Path(os.path.realpath(sys.executable)))
    assert deps, "no libraries read from the running interpreter"


def test_library_keys_and_their_modules():
    assert qb.library_key("@rpath/QtCore.framework/Versions/A/QtCore") == "QtCore"
    assert qb.library_key("@rpath/libpyside6.abi3.6.11.dylib") == "libpyside6.abi3.6.11.dylib"
    assert qb.library_key("libQt6Core.so.6") == "libQt6Core.so.6"
    assert qb.module_for("QtPrintSupport") == "QtPrintSupport"
    assert qb.module_for("libQt6PrintSupport.so.6") == "QtPrintSupport"
    assert qb.module_for("libQt63DCore.so.6") == "Qt3DCore"
    assert qb.module_for("libicuuc.so.73") == ""
    assert qb.module_for("libavcodec.61.dylib") == ""


# ---- a Linux wheel, in small ------------------------------------------------

def linux_site(tmp_path: Path) -> Path:
    """The layout of the PySide6 Linux wheels, with the links that matter."""
    site = tmp_path / "site-packages"
    py, qt = site / "PySide6", site / "PySide6" / "Qt"
    lib = qt / "lib"
    _write(py / "__init__.py", "")
    _write(py / "support" / "__init__.py", "")
    for module, links in {
        "QtCore": ["libQt6Core.so.6"], "QtGui": ["libQt6Gui.so.6"],
        "QtWidgets": ["libQt6Widgets.so.6"], "QtNetwork": ["libQt6Network.so.6"],
        "QtWebChannel": ["libQt6WebChannel.so.6"], "QtWebEngineCore": ["libQt6WebEngineCore.so.6"],
        "QtWebEngineWidgets": ["libQt6WebEngineWidgets.so.6"],
        "QtPrintSupport": ["libQt6PrintSupport.so.6"], "Qt3DCore": ["libQt63DCore.so.6"],
        "QtCharts": ["libQt6Charts.so.6"],
    }.items():
        _write(py / f"{module}.abi3.so", elf("libpyside6.abi3.so.6.11", *links, "libc.so.6"))
        _write(py / f"{module}.pyi", "")
    _write(py / "libpyside6.abi3.so.6.11", elf("libshiboken6.abi3.so.6.11", "libQt6Core.so.6"))
    _write(py / "libpyside6qml.abi3.so.6.11", elf("libQt6Qml.so.6"))
    _write(py / "designer", elf("libQt6Designer.so.6"))
    _write(py / "include" / "pyside.h", "")
    _write(py / "glue" / "qtcore.cpp", "")
    _write(site / "shiboken6" / "Shiboken.abi3.so", elf("libshiboken6.abi3.so.6.11"))
    _write(site / "shiboken6" / "libshiboken6.abi3.so.6.11", elf("libc.so.6"))
    _write(site / "shiboken6" / "__init__.py", "")
    for name, links in {
        "libQt6Core.so.6": ["libicuuc.so.73"], "libicuuc.so.73": [],
        "libQt6Gui.so.6": ["libQt6Core.so.6", "libQt6DBus.so.6"], "libQt6DBus.so.6": [],
        "libQt6Widgets.so.6": ["libQt6Gui.so.6"], "libQt6Network.so.6": [],
        "libQt6WebChannel.so.6": ["libQt6Qml.so.6"], "libQt6Qml.so.6": [],
        "libQt6WebEngineCore.so.6": ["libQt6Quick.so.6", "libQt6WebChannel.so.6"],
        "libQt6Quick.so.6": ["libQt6Qml.so.6"],
        "libQt6WebEngineWidgets.so.6": ["libQt6WebEngineCore.so.6", "libQt6PrintSupport.so.6"],
        "libQt6PrintSupport.so.6": ["libQt6Widgets.so.6"],
        "libQt6XcbQpa.so.6": ["libQt6Gui.so.6"],
        "libQt63DCore.so.6": [], "libQt6Charts.so.6": [], "libQt6Designer.so.6": [],
        "libQt6VirtualKeyboard.so.6": [], "libavcodec.so.61": [], "libavcodec.so": [],
    }.items():
        _write(lib / name, elf(*links))
    _write(qt / "plugins" / "platforms" / "libqxcb.so", elf("libQt6XcbQpa.so.6"))
    _write(qt / "plugins" / "platforminputcontexts" / "libcomposeplatforminputcontextplugin.so",
           elf("libQt6Gui.so.6"))
    _write(qt / "plugins" / "platforminputcontexts" / "libqtvirtualkeyboardplugin.so",
           elf("libQt6VirtualKeyboard.so.6"))
    _write(qt / "plugins" / "sceneparsers" / "libassimpsceneimport.so", elf("libQt63DCore.so.6"))
    _write(qt / "plugins" / "multimedia" / "libffmpegmediaplugin.so", elf("libavcodec.so.61"))
    _write(qt / "libexec" / "QtWebEngineProcess", elf("libQt6WebEngineCore.so.6"))
    _write(qt / "libexec" / "rcc", elf("libQt6Core.so.6"))
    _write(qt / "resources" / "qtwebengine_resources.pak", "pak")
    _write(qt / "translations" / "qtwebengine_locales" / "en-US.pak", "pak")
    _write(qt / "qml" / "QtQuick" / "qmldir", "module QtQuick")
    _write(qt / "metatypes" / "qt6core_metatypes.json", "{}")
    return site


def _left(site: Path) -> set[str]:
    return {p.relative_to(site).as_posix() for p in site.rglob("*") if p.is_file()}


def test_the_linux_trim_keeps_what_the_app_loads(tmp_path):
    site = linux_site(tmp_path)
    decided = qb.prune(site, log=lambda _line: None)
    left = _left(site)
    lib = "PySide6/Qt/lib/"
    for kept in ("libQt6Core.so.6", "libicuuc.so.73", "libQt6Gui.so.6", "libQt6DBus.so.6",
                 "libQt6WebEngineCore.so.6", "libQt6Quick.so.6", "libQt6Qml.so.6",
                 "libQt6PrintSupport.so.6", "libQt6XcbQpa.so.6"):
        assert lib + kept in left, kept
    for gone in ("libQt63DCore.so.6", "libQt6Charts.so.6", "libQt6Designer.so.6",
                 "libQt6VirtualKeyboard.so.6", "libavcodec.so.61", "libavcodec.so"):
        assert lib + gone not in left, gone
    # A kept Qt library keeps its Python module: QtWebEngineWidgets imports
    # QtPrintSupport when it loads.
    assert "QtPrintSupport" in decided.modules
    assert "PySide6/QtPrintSupport.abi3.so" in left
    assert "PySide6/QtPrintSupport.pyi" in left
    assert "PySide6/Qt3DCore.abi3.so" not in left
    assert "PySide6/QtCharts.pyi" not in left
    # PySide6's QML glue is linked only by the QtQml and QtQuick modules, and
    # neither is kept here.
    assert "PySide6/libpyside6qml.abi3.so.6.11" not in left
    assert "PySide6/libpyside6.abi3.so.6.11" in left
    assert {"shiboken6/Shiboken.abi3.so", "shiboken6/libshiboken6.abi3.so.6.11"} <= left


def test_the_linux_trim_keeps_the_web_engine_and_drops_the_tools(tmp_path):
    site = linux_site(tmp_path)
    qb.prune(site, log=lambda _line: None)
    left = _left(site)
    assert "PySide6/Qt/libexec/QtWebEngineProcess" in left
    assert "PySide6/Qt/libexec/rcc" not in left
    assert "PySide6/Qt/resources/qtwebengine_resources.pak" in left
    assert "PySide6/Qt/translations/qtwebengine_locales/en-US.pak" in left
    assert "PySide6/Qt/plugins/platforms/libqxcb.so" in left
    assert "PySide6/Qt/plugins/platforminputcontexts/libcomposeplatforminputcontextplugin.so" in left
    assert "PySide6/Qt/plugins/platforminputcontexts/libqtvirtualkeyboardplugin.so" not in left
    assert not any(p.startswith(("PySide6/Qt/plugins/sceneparsers", "PySide6/Qt/plugins/multimedia"))
                   for p in left)
    assert not any(p.startswith(("PySide6/Qt/qml", "PySide6/Qt/metatypes", "PySide6/include",
                                 "PySide6/glue")) for p in left)
    assert "PySide6/designer" not in left
    assert {"PySide6/__init__.py", "PySide6/support/__init__.py", "shiboken6/__init__.py"} <= left


def test_a_dry_run_removes_nothing(tmp_path):
    site = linux_site(tmp_path)
    before = _left(site)
    lines: list[str] = []
    decided = qb.prune(site, dry_run=True, log=lines.append)
    assert _left(site) == before
    assert decided.remove
    assert any("would remove PySide6/Qt/lib/libQt63DCore.so.6" in line for line in lines)


def test_a_binary_left_linking_a_removed_library_fails_the_build(tmp_path):
    site = linux_site(tmp_path)
    decided = qb.prune(site, log=lambda _line: None)
    _write(site / "PySide6" / "Qt" / "plugins" / "platforms" / "libqbroken.so",
           elf("libQt63DCore.so.6"))
    with pytest.raises(qb.BundleError, match="libqbroken.so links libQt63DCore.so.6"):
        qb.check(site, decided.removed_keys)


def test_a_site_without_pyside6_is_refused(tmp_path):
    site = tmp_path / "site-packages"
    _write(site / "PySide6" / "__init__.py", "")
    with pytest.raises(qb.BundleError, match="QtWebEngineWidgets"):
        qb.plan(site)


def test_the_command_line_reports_a_refusal(tmp_path, capsys):
    assert qb.main(["prune", str(tmp_path)]) == 1
    assert "ERROR:" in capsys.readouterr().err


# ---- a macOS wheel, in small ------------------------------------------------

def _fw(name: str) -> str:
    return f"@rpath/{name}.framework/Versions/A/{name}"


def macos_site(tmp_path: Path) -> Path:
    """The macOS layout: frameworks, a helper app inside one, universal binaries."""
    site = tmp_path / "site-packages"
    py, qt = site / "PySide6", site / "PySide6" / "Qt"
    lib = qt / "lib"
    for module in qb.MODULES + ("QtPrintSupport", "QtCharts"):
        _write(py / f"{module}.abi3.so",
               fat(macho("@rpath/libpyside6.abi3.6.11.dylib", _fw(module), "/usr/lib/libc++.1.dylib")))
    _write(py / "libpyside6.abi3.6.11.dylib", macho(_fw("QtCore")))
    _write(py / "Designer.app" / "Contents" / "MacOS" / "Designer", macho(_fw("QtDesigner")))
    _write(site / "shiboken6" / "libshiboken6.abi3.6.11.dylib", macho("/usr/lib/libSystem.B.dylib"))
    links = {
        "QtCore": [], "QtGui": ["QtCore"], "QtWidgets": ["QtGui"], "QtNetwork": ["QtCore"],
        "QtWebChannel": ["QtQml"], "QtQml": ["QtNetwork"], "QtQuick": ["QtQml"],
        "QtWebEngineCore": ["QtQuick", "QtWebChannel", "QtPositioning"], "QtPositioning": [],
        "QtWebEngineWidgets": ["QtWebEngineCore", "QtPrintSupport", "QtQuickWidgets"],
        "QtQuickWidgets": ["QtQuick", "QtWidgets"], "QtPrintSupport": ["QtWidgets"],
        "QtCharts": ["QtWidgets"], "QtDesigner": ["QtWidgets"], "QtPdf": ["QtGui"],
    }
    for name, deps in links.items():
        _write(lib / f"{name}.framework" / "Versions" / "A" / name, fat(macho(*map(_fw, deps))))
        _write(lib / f"{name}.framework" / "Resources" / "Info.plist", "<plist/>")
    helper = lib / "QtWebEngineCore.framework" / "Helpers" / "QtWebEngineProcess.app"
    _write(helper / "Contents" / "MacOS" / "QtWebEngineProcess", macho(_fw("QtWebEngineCore"), _fw("QtSvg")))
    _write(lib / "QtSvg.framework" / "Versions" / "A" / "QtSvg", macho(_fw("QtGui")))
    _write(lib / "libavcodec.61.dylib", macho())
    _write(qt / "plugins" / "platforms" / "libqcocoa.dylib", macho(_fw("QtGui")))
    _write(qt / "plugins" / "imageformats" / "libqpdf.dylib", macho(_fw("QtPdf")))
    _write(qt / "plugins" / "multimedia" / "libffmpegmediaplugin.dylib",
           macho("@rpath/libavcodec.61.dylib"))
    _write(qt / "libexec" / "rcc", macho(_fw("QtCore")))
    return site


def test_the_macos_trim_follows_frameworks_and_the_helper_app(tmp_path):
    site = macos_site(tmp_path)
    decided = qb.prune(site, log=lambda _line: None)
    left = _left(site)
    lib = "PySide6/Qt/lib/"
    for kept in ("QtCore", "QtQml", "QtQuick", "QtQuickWidgets", "QtPositioning",
                 "QtPrintSupport", "QtPdf", "QtSvg", "QtWebEngineCore"):
        assert f"{lib}{kept}.framework/Versions/A/{kept}" in left, kept
    assert f"{lib}QtWebEngineCore.framework/Helpers/QtWebEngineProcess.app/Contents/MacOS/QtWebEngineProcess" in left
    for gone in ("QtCharts.framework", "QtDesigner.framework", "libavcodec.61.dylib"):
        assert not any(p.startswith(lib + gone) for p in left), gone
    assert "PySide6/QtCharts.abi3.so" not in left
    assert "PySide6/QtPrintSupport.abi3.so" in left
    assert not any(p.startswith("PySide6/Designer.app") for p in left)
    assert "PySide6/Qt/libexec/rcc" not in left
    assert "PySide6/Qt/plugins/imageformats/libqpdf.dylib" in left
    assert decided.modules >= set(qb.MODULES)
