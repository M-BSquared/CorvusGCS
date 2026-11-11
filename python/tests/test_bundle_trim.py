"""``tools/bundle_trim.py``: a packaged build holds no Python.

Each case builds a small fake bundle, so the tests run on every CI host without
a build.
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


def write(path: Path, data: bytes | str = b"x", mode: int = 0o644) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)
    path.chmod(mode)
    return path


def good_bundle(root: Path) -> Path:
    write(root / "corvus-gcs", b"\x7fELF native code", 0o755)
    write(root / "VERSION", "2026.11.1\n")
    write(root / "LICENSE.md", "terms")
    write(root / "assets" / "licenses" / "GPL-3.0.txt", "text")
    return root


def test_a_bundle_of_one_program_and_its_licence_passes(tmp_path: Path) -> None:
    assert bt.check(good_bundle(tmp_path), ("corvus-gcs", "VERSION", "LICENSE.md")) == []


@pytest.mark.parametrize("rel", [
    "python/bin/python3", "lib/python3.12/os.py", "usr/lib/libpython3.12.so.1.0", "Python.framework/Versions/3.12/Python",
    "python/python312.dll", "site-packages/serial/__init__.py", "backend/serve.py", "pyvenv.cfg",
    "lib/corvus_link.abi3.so", "x/PySide6/QtCore.so", "a/b.pyc", "pip/__init__.py",
])
def test_every_part_of_an_interpreter_or_its_packages_is_refused(tmp_path: Path, rel: str) -> None:
    good_bundle(tmp_path)
    write(tmp_path / rel)
    problems = bt.check(tmp_path)
    assert problems and any("Python" in p for p in problems), problems


@pytest.mark.parametrize("symbol", [b"PyInit_corvus_link", b"Py_InitializeEx"])
def test_a_native_file_built_against_python_is_refused(tmp_path: Path, symbol: bytes) -> None:
    good_bundle(tmp_path)
    write(tmp_path / "corvus-gcs", b"\x7fELF" + b"\0" * 10 + symbol, 0o755)
    assert [p for p in bt.check(tmp_path) if "PyO3" in p]


def test_a_symbol_split_across_the_read_blocks_is_still_found(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(bt, "_CHUNK", 8)
    write(tmp_path / "corvus-gcs", b"abcdefgh" + b"xxPyInit_a" + b"yyyy", 0o755)
    assert bt.built_against_python(tmp_path / "corvus-gcs")


def test_a_missing_required_file_fails(tmp_path: Path) -> None:
    good_bundle(tmp_path)
    (tmp_path / "LICENSE.md").unlink()
    assert bt.check(tmp_path, ("LICENSE.md",)) == ["LICENSE.md: required and missing"]


def test_the_command_line_says_what_it_found(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    good_bundle(tmp_path)
    assert bt.main(["check", str(tmp_path)]) == 0
    write(tmp_path / "python3", b"x", 0o755)
    assert bt.main(["check", str(tmp_path)]) == 1
    assert "python3: a Python file or folder" in capsys.readouterr().err


def test_the_build_scripts_check_the_same_rule() -> None:
    for name in ("build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"):
        text = (ROOT.parent / name).read_text(encoding="utf-8")
        assert "must hold no Python" in text, name
        code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
        assert "pip install" not in code and "pyinstaller" not in code.lower() and "venv" not in code, name
