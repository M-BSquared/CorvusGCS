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
SCRIPT = (ROOT.parent / "build-macos-app.sh").read_text(encoding="utf-8")
LAUNCHER = ROOT / "tools" / "macos_launcher.c"


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("cc"),
                    reason="compiles and runs the launcher; macOS with a C compiler only")
def test_the_launcher_starts_the_interface_with_the_bundled_interpreter(tmp_path: Path) -> None:
    contents = tmp_path / "with space" / "Corvus GCS.app" / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    res = contents / "Resources"
    (res / "python" / "lib" / "python3.12" / "site-packages").mkdir(parents=True)
    (res / "app").mkdir(parents=True)
    (res / "python" / "bin").mkdir(parents=True)
    (res / "python" / "bin" / "python3").write_text("")
    fake = contents / "MacOS" / "corvus-gcs-ui"
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
    assert lines[4:] == [
        "--root",
        str(real / "app"),
        "--python",
        str(real / "python" / "bin" / "python3"),
        "8001",
        "udp:0.0.0.0:14550",
    ]


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
