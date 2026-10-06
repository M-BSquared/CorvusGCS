"""The macOS .app starts from a download: natively, and with nothing from the build Mac.

The released .app died on every Mac it was downloaded to, approved or not:

* Its launcher was a bash script. LaunchServices cannot read an architecture
  off a script and started it under Rosetta; the universal interpreter followed
  into x86_64, and the arm64-only Qt did not load.
* Python's ``_ssl`` and ``_hashlib`` linked OpenSSL by absolute path inside the
  build Mac's python.org install, so ``import ssl`` failed anywhere else.

The launcher is now a small C program compiled for the bundle's architecture,
the interpreter is thinned to that architecture, and every Mach-O in the bundle
is checked for a library outside the bundle and outside macOS.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = (ROOT / "build-macos-app.sh").read_text(encoding="utf-8")
LAUNCHER = ROOT / "tools" / "macos_launcher.c"


def test_the_launcher_is_compiled_for_the_bundle_architecture() -> None:
    assert LAUNCHER.is_file()
    assert 'cc -arch "$ARCH"' in SCRIPT
    assert '"$REPO_DIR/tools/macos_launcher.c"' in SCRIPT
    assert "LAUNCH_EOF" not in SCRIPT, "the launcher is a script again"


def test_the_launcher_and_interpreter_must_be_one_architecture() -> None:
    assert 'thin_to_arch "$FW_DST/Python"' in SCRIPT
    assert 'it must be $ARCH only' in SCRIPT
    check = SCRIPT.index('it must be $ARCH only')
    assert '"$APP/Contents/MacOS/corvus-gcs" "$PYROOT/bin/python3"' in SCRIPT[check - 400:check]


def test_every_binary_is_checked_for_libraries_from_the_build_mac() -> None:
    assert "foreign_refs()" in SCRIPT
    assert "/System/*|/usr/lib/*|@*) ;;" in SCRIPT
    check = SCRIPT.index("links a library outside the bundle and outside macOS")
    assert '/usr/bin/find "$APP" -type f' in SCRIPT[check - 800:check]


def test_the_stdlib_libraries_are_bundled_before_the_check() -> None:
    bundle = SCRIPT.index("Bundling the libraries the stdlib links")
    check = SCRIPT.index("links a library outside the bundle and outside macOS")
    assert bundle < check
    assert "@loader_path/$rel/$name" in SCRIPT


def test_the_runtime_check_imports_the_native_stdlib_modules() -> None:
    assert "import ssl, hashlib" in SCRIPT


def test_tkinter_is_not_bundled() -> None:
    """_tkinter links Tcl/Tk from the build host and nothing imports it."""
    assert "--exclude '_tkinter*'" in SCRIPT


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("cc"),
                    reason="compiles and runs the launcher; macOS with a C compiler only")
def test_the_launcher_starts_the_bundled_interpreter(tmp_path: Path) -> None:
    contents = tmp_path / "with space" / "Corvus GCS.app" / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    res = contents / "Resources"
    (res / "python" / "lib" / "python3.12" / "site-packages").mkdir(parents=True)
    (res / "app" / "corvus").mkdir(parents=True)
    (res / "app" / "corvus" / "app.py").write_text("")
    fake = res / "python" / "bin" / "python3"
    fake.parent.mkdir(parents=True)
    fake.write_text('#!/bin/sh\nprintf "%s\\n" "$PWD" "$PYTHONHOME" "$PYTHONPATH" '
                    '"$PYTHONDONTWRITEBYTECODE" "$@"\n')
    fake.chmod(0o755)
    launcher = contents / "MacOS" / "corvus-gcs"
    subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-o", str(launcher), str(LAUNCHER)],
                   check=True)

    out = subprocess.run([str(launcher), "8001", "udp:0.0.0.0:14550"], capture_output=True,
                         text=True, check=True, env={"PATH": os.environ.get("PATH", "")})
    lines = out.stdout.splitlines()
    real = res.resolve()
    assert lines[0] == str(real / "app")
    assert lines[1] == str(real / "python")
    assert lines[2] == f"{real / 'app'}:{real / 'python' / 'lib' / 'python3.12' / 'site-packages'}"
    assert lines[3] == "1"
    assert lines[4:] == [str(real / "app" / "corvus" / "app.py"), "8001", "udp:0.0.0.0:14550"]


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("cc"),
                    reason="compiles and runs the launcher; macOS with a C compiler only")
def test_the_launcher_says_what_is_missing(tmp_path: Path) -> None:
    contents = tmp_path / "Corvus GCS.app" / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources" / "python" / "lib").mkdir(parents=True)
    launcher = contents / "MacOS" / "corvus-gcs"
    subprocess.run(["cc", "-o", str(launcher), str(LAUNCHER)], check=True)
    out = subprocess.run([str(launcher)], capture_output=True, text=True)
    assert out.returncode == 1
    assert "bundled python3.x not found" in out.stderr
