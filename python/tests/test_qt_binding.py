"""One Qt binding, PySide6, everywhere Corvus names one.

PyQt6 is GPL v3, and the GPL does not combine with the Sustainable Use
License. A PyQt6 import, dependency or bundle path coming back is therefore a
licence problem, not a style choice, and these tests are where it shows. They
also hold the trimmed bundles to the code: a Qt module ``corvus/app.py``
imports that ``tools/qt_bundle.py`` does not keep would be missing from the
AppImage and the .app, and only there.
"""
from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

_spec = importlib.util.spec_from_file_location("corvus_qt_bundle_binding", ROOT / "tools" / "qt_bundle.py")
qt_bundle = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
sys.modules[_spec.name] = qt_bundle
_spec.loader.exec_module(qt_bundle)

QT_CODE = [ROOT / "corvus" / "app.py", *sorted((ROOT / "tools" / "scene_kit").glob("*.py"))]


def _python_files() -> list[Path]:
    files = [ROOT / "serve.py"]
    for folder in ("corvus", "tools", "plugins", "tests"):
        files += [p for p in (ROOT / folder).rglob("*.py") if "__pycache__" not in p.parts]
    return files


def test_nothing_imports_another_qt_binding():
    pattern = re.compile(r"^\s*(?:from|import)\s+(?:PyQt[456]|PySide2|sip)\b", re.MULTILINE)
    offenders = [str(p.relative_to(ROOT)) for p in _python_files()
                 if pattern.search(p.read_text(encoding="utf-8"))]
    assert not offenders, f"another Qt binding is imported in {offenders}"


def test_no_pyqt_only_names_are_left():
    pattern = re.compile(r"\b(?:pyqtSlot|pyqtSignal|pyqtProperty|QT_VERSION_STR|PYQT_VERSION)\b")
    offenders = [str(p.relative_to(ROOT)) for p in _python_files()
                 if p.name != Path(__file__).name and pattern.search(p.read_text(encoding="utf-8"))]
    assert not offenders, f"PyQt names (use Slot, Signal, qVersion) in {offenders}"


def test_every_qt_module_the_app_imports_survives_the_trim():
    imported: set[str] = set()
    for path in QT_CODE:
        text = path.read_text(encoding="utf-8")
        imported |= set(re.findall(r"from PySide6\.(Qt\w+) import", text))
        for names in re.findall(r"from PySide6 import ([\w, ]+)", text):
            imported |= {n.strip() for n in names.split(",") if n.strip().startswith("Qt")}
    assert imported, "found no PySide6 import at all"
    missing = imported - set(qt_bundle.MODULES)
    assert not missing, (
        f"{sorted(missing)} imported but not in tools/qt_bundle.py MODULES: "
        "the trimmed AppImage and .app would not carry them"
    )


def test_every_bundle_carries_the_qt_licences():
    lgpl = (ROOT.parent / "assets" / "licenses" / "LGPL-3.0.txt").read_text(encoding="utf-8")
    gpl = (ROOT.parent / "assets" / "licenses" / "GPL-3.0.txt").read_text(encoding="utf-8")
    assert lgpl.lstrip().startswith("GNU LESSER GENERAL PUBLIC LICENSE")
    assert gpl.lstrip().startswith("GNU GENERAL PUBLIC LICENSE")
    assert "Version 3, 29 June 2007" in lgpl and "Version 3, 29 June 2007" in gpl
    # All three builds copy assets through tools/bundle_assets.py, which keeps licenses/.
    for script in ("build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"):
        assert "bundle_assets.py" in (ROOT.parent / script).read_text(encoding="utf-8"), script
    import importlib.util
    spec = importlib.util.spec_from_file_location("corvus_bundle_assets_tool", ROOT / "tools" / "bundle_assets.py")
    tool = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(tool)
    assert "licenses" in tool.SHIPPED


def test_no_build_script_ships_qt():
    """The bundles carry the Rust interface; Qt stays behind ./run.sh --web."""
    for name in ("build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"):
        text = (ROOT.parent / name).read_text(encoding="utf-8")
        assert "PySide6" not in text.replace("No Qt", ""), name
        assert "qt_bundle" not in text, name
        assert "cargo build --release --locked" in text, name


def test_macos_floor_is_one_value_for_cargo_and_the_plist():
    macos = (ROOT.parent / "build-macos-app.sh").read_text(encoding="utf-8")
    assert 'MIN_MACOS="12.0"' in macos
    assert '"LSMinimumSystemVersion": minimum' in macos
    assert 'MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"' in macos
    ci = (ROOT.parent / ".github" / "workflows" / "build.yml").read_text(encoding="utf-8")
    job = ci.split("  macos-app:", 1)[1].split("\n  # ---", 1)[0]
    assert "runs-on: macos-15" in job


def test_the_credits_name_the_binding_that_ships():
    credits = (ROOT / "src" / "js" / "credits.js").read_text(encoding="utf-8")
    assert "PySide6" in credits and "Shiboken6" in credits
    assert "PyQt" not in credits
