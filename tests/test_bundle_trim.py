"""``tools/bundle_trim.py``: what a packaged build drops besides unused Qt.

Each step is run on a small fake ``site-packages`` laid out like the real
wheels, so the tests run on every CI host without a build.
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]

_spec = importlib.util.spec_from_file_location("corvus_bundle_trim_tool", ROOT / "tools" / "bundle_trim.py")
bt = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
sys.modules[_spec.name] = bt
_spec.loader.exec_module(bt)


def write(path: Path, data: bytes | str = b"x") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        data = data.encode()
    path.write_bytes(data)
    return path


def fake_site(tmp_path: Path) -> Path:
    site = tmp_path / "site-packages"
    for wire in ("v10", "v20"):
        folder = site / "pymavlink" / "dialects" / wire
        write(folder / "__init__.py", "")
        for dialect in ("all", "common", "ardupilotmega"):
            write(folder / f"{dialect}.py", dialect)
            write(folder / f"{dialect}.xml", "<mavlink/>")
            write(folder / "__pycache__" / f"{dialect}.cpython-313.pyc")
    write(site / "pymavlink" / "mavutil.py", "")
    write(site / "pymavlink" / "message_definitions" / "v1.0" / "common.xml")
    write(site / "lxml" / "__init__.py", "")
    write(site / "lxml-6.0.dist-info" / "METADATA")
    write(site / "pip" / "__init__.py", "")
    write(site / "pip-26.0.dist-info" / "METADATA")
    write(site / "paramiko" / "__init__.py", "")
    qt = site / "PySide6" / "Qt"
    write(site / "PySide6" / "QtCore.pyi")
    write(site / "PySide6" / "QtCore.abi3.so")
    for name in ("qtbase_de.qm", "qt_fr.qm"):
        write(qt / "translations" / name)
    for locale in ("en-US", "de", "fr", "ja"):
        write(qt / "translations" / "qtwebengine_locales" / f"{locale}.pak")
    return site


def test_only_the_dialect_mavutil_loads_stays(tmp_path):
    site = fake_site(tmp_path)
    bt.trim(site, log=lambda _line: None)
    for wire in ("v10", "v20"):
        folder = site / "pymavlink" / "dialects" / wire
        assert sorted(p.name for p in folder.glob("*.py")) == ["__init__.py", "all.py"]
        assert [p.name for p in folder.glob("*.xml")] == ["all.xml"]
        assert [p.name for p in (folder / "__pycache__").iterdir()] == ["all.cpython-313.pyc"]
    assert (site / "pymavlink" / "mavutil.py").is_file()
    assert (site / "pymavlink" / "message_definitions" / "v1.0" / "common.xml").is_file()


def test_lxml_and_pip_go_with_their_metadata(tmp_path):
    site = fake_site(tmp_path)
    bt.trim(site, log=lambda _line: None)
    for gone in ("lxml", "lxml-6.0.dist-info", "pip", "pip-26.0.dist-info"):
        assert not (site / gone).exists()
    assert (site / "paramiko" / "__init__.py").is_file()


def test_qt_translations_and_foreign_locales_go(tmp_path):
    site = fake_site(tmp_path)
    bt.trim(site, log=lambda _line: None)
    translations = site / "PySide6" / "Qt" / "translations"
    assert not list(translations.glob("*.qm"))
    assert sorted(p.name for p in (translations / "qtwebengine_locales").iterdir()) == ["de.pak", "en-US.pak"]
    assert not (site / "PySide6" / "QtCore.pyi").exists()
    assert (site / "PySide6" / "QtCore.abi3.so").is_file()


def test_the_devtools_pack_goes_and_the_web_engine_resources_stay(tmp_path):
    site = fake_site(tmp_path)
    resources = site / "PySide6" / "Qt" / "resources"
    for name in ("qtwebengine_resources.pak", "qtwebengine_devtools_resources.pak", "icudtl.dat",
                 "v8_context_snapshot.bin"):
        write(resources / name)
    bt.trim(site, log=lambda _line: None)
    assert sorted(p.name for p in resources.iterdir()) == [
        "icudtl.dat", "qtwebengine_resources.pak", "v8_context_snapshot.bin"]


def test_only_the_build_architecture_keeps_its_v8_snapshot(tmp_path):
    site = fake_site(tmp_path)
    resources = site / "PySide6" / "Qt" / "lib" / "QtWebEngineCore.framework" / "Versions" / "A" / "Resources"
    for arch in ("arm64", "x86_64"):
        write(resources / f"v8_context_snapshot.{arch}.bin")
    removed = bt.plan(site, arch="arm64").remove
    assert resources / "v8_context_snapshot.x86_64.bin" in removed
    assert resources / "v8_context_snapshot.arm64.bin" not in removed
    # A host whose own snapshot is not there keeps every one, rather than none.
    assert not any("v8_context" in p.name for p in bt.plan(site, arch="riscv64").remove)


def test_dry_run_changes_nothing(tmp_path):
    site = fake_site(tmp_path)
    before = sorted(str(p) for p in site.rglob("*"))
    lines: list[str] = []
    bt.trim(site, dry_run=True, log=lines.append)
    assert sorted(str(p) for p in site.rglob("*")) == before
    assert any("would remove" in line for line in lines)


def test_a_missing_all_dialect_fails_the_build(tmp_path):
    site = fake_site(tmp_path)
    (site / "pymavlink" / "dialects" / "v20" / "all.py").unlink()
    with pytest.raises(bt.TrimError, match="v20/all.py"):
        bt.trim(site, log=lambda _line: None)


def test_a_missing_en_us_locale_fails_the_build(tmp_path):
    site = fake_site(tmp_path)
    (site / "PySide6" / "Qt" / "translations" / "qtwebengine_locales" / "en-US.pak").unlink()
    with pytest.raises(bt.TrimError, match="en-US.pak"):
        bt.trim(site, log=lambda _line: None)


def test_a_frozen_layout_without_dialect_sources_passes(tmp_path):
    internal = tmp_path / "_internal"
    for wire in ("v10", "v20"):
        write(internal / "pymavlink" / "dialects" / wire / "all.xml")
        write(internal / "pymavlink" / "dialects" / wire / "common.xml")
    bt.trim(internal, log=lambda _line: None)
    for wire in ("v10", "v20"):
        assert [p.name for p in (internal / "pymavlink" / "dialects" / wire).iterdir()] == ["all.xml"]


# A .framework and its directory symlinks exist only in the macOS bundle; on
# Windows a plain symlink_to makes a file link that a chained path cannot open.
_posix_links = pytest.mark.skipif(
    sys.platform == "win32",
    reason="macOS framework symlinks; no Windows bundle has a .framework",
)


def framework(site: Path, name: str) -> Path:
    fw = site / "PySide6" / "Qt" / "lib" / f"{name}.framework"
    for base in (fw, fw / "Versions" / "A"):
        write(base / "Resources" / "Info.plist", "plist")
        write(base / "Resources" / "qtwebengine_resources.pak", "pak" * 100)
    write(fw / "Versions" / "A" / name, b"\xcf\xfa\xed\xfe")
    return fw


@_posix_links
def test_identical_framework_copies_become_symlinks(tmp_path):
    site = fake_site(tmp_path)
    fw = framework(site, "QtWebEngineCore")
    bt.trim(site, log=lambda _line: None)
    assert (fw / "Versions" / "Current").is_symlink()
    assert (fw / "Versions" / "Current").readlink() == Path("A")
    assert (fw / "Resources").is_symlink()
    assert (fw / "Resources").readlink() == Path("Versions/Current/Resources")
    assert (fw / "Resources" / "qtwebengine_resources.pak").read_text() == "pak" * 100


@_posix_links
def test_a_framework_copy_that_differs_is_left_alone(tmp_path):
    site = fake_site(tmp_path)
    fw = framework(site, "QtWebEngineCore")
    write(fw / "Helpers" / "QtWebEngineProcess", "signed")
    write(fw / "Versions" / "A" / "Helpers" / "QtWebEngineProcess", "unsigned")
    bt.trim(site, log=lambda _line: None)
    assert (fw / "Resources").is_symlink()
    assert not (fw / "Helpers").is_symlink()
    assert (fw / "Helpers" / "QtWebEngineProcess").read_text() == "signed"


def test_every_build_script_runs_the_trim_after_the_qt_prune():
    for script in ("build-macos-app.sh", "build-appimage.sh"):
        text = (ROOT / script).read_text()
        assert text.index("tools/qt_bundle.py\" prune") < text.index("tools/bundle_trim.py\""), script
    windows = (ROOT / "build-windows.ps1").read_text()
    assert windows.index("-m PyInstaller") < windows.index("tools\\bundle_trim.py")
    assert '"--exclude-module", "lxml"' in windows


def test_the_app_pins_the_dialect_the_bundles_keep():
    source = (ROOT / "corvus" / "app.py").read_text()
    assert 'os.environ["MAVLINK_DIALECT"] = "all"' in source
    assert source.index('os.environ["MAVLINK_DIALECT"]') < source.index("from corvus")
    assert bt.MAVLINK_DIALECTS == {"all"}
