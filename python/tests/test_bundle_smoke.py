"""The built macOS bundle, run.

Skipped unless a bundle has been built (``./build-macos-app.sh`` puts it in
``dist/``; ``CORVUS_APP_BUNDLE`` names another). The bundle is one native
program with its backend inside it, so the checks are: it holds no Python of any
kind (``tools/bundle_trim.py``), it carries the version and the licence, its
only dynamic libraries are macOS's own, it starts and prints its usage, and its
signature still verifies afterwards (a run that wrote a file into the bundle
would break the seal an operator's Mac checks).

The interface itself starts only when ``CORVUS_BUNDLE_GUI=1``, because it needs a
desktop session: it must serve its own API from the one process, with no child
process, and a signal must end it cleanly and free its port.

Needs only pytest: the test imports nothing of the Python backend.
"""
from __future__ import annotations

import importlib.util
import json
import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import pytest

if platform.system() != "Darwin":
    pytest.skip("the macOS bundle", allow_module_level=True)

ROOT = Path(__file__).resolve().parent.parent
BUNDLE = Path(os.environ.get("CORVUS_APP_BUNDLE", ROOT.parent / "dist" / "Corvus GCS.app"))

if not BUNDLE.is_dir():
    pytest.skip(f"no bundle at {BUNDLE}; build one with ./build-macos-app.sh", allow_module_level=True)

RES = BUNDLE / "Contents" / "Resources"
EXE = BUNDLE / "Contents" / "MacOS" / "corvus-gcs"

_spec = importlib.util.spec_from_file_location("corvus_bundle_trim_tool", ROOT / "tools" / "bundle_trim.py")
trim = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = trim
_spec.loader.exec_module(trim)


def wait_for(check, timeout: float = 30.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if check():
                return True
        except OSError:
            pass
        time.sleep(0.2)
    return False


def get(port: int, path: str) -> tuple[int, dict]:
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


def test_the_bundle_holds_no_python() -> None:
    problems = trim.check(BUNDLE, ("Contents/MacOS/corvus-gcs", "Contents/Resources/VERSION", "Contents/Resources/LICENSE.md"))
    assert problems == []


def test_the_bundle_carries_the_version_and_the_licence() -> None:
    assert (RES / "VERSION").read_text().strip() == (ROOT.parent / "VERSION").read_text().strip()
    assert (RES / "LICENSE.md").is_file()
    assert (RES / "assets" / "licenses").is_dir()
    plist = (BUNDLE / "Contents" / "Info.plist").read_text()
    assert (RES / "VERSION").read_text().strip() in plist


def test_the_interface_links_only_what_macos_ships() -> None:
    out = subprocess.run(["otool", "-L", str(EXE)], capture_output=True, text=True, check=True).stdout.splitlines()[1:]
    paths = [line.split()[0] for line in out if line.strip()]
    foreign = [p for p in paths if not (p.startswith(("/System/", "/usr/lib/")) or p.startswith("@"))]
    assert foreign == []


def test_the_program_prints_its_usage_and_names_no_interpreter(tmp_path: Path) -> None:
    done = subprocess.run([str(EXE), "--help"], capture_output=True, text=True, timeout=30, env={**os.environ, "HOME": str(tmp_path)})
    assert done.returncode == 0, done.stderr
    assert done.stdout.startswith("usage: corvus-gcs")


def test_running_the_bundle_leaves_its_signature_intact() -> None:
    done = subprocess.run(["codesign", "--verify", "--deep", "--strict", str(BUNDLE)], capture_output=True, text=True)
    assert done.returncode == 0, done.stderr


@pytest.mark.skipif(os.environ.get("CORVUS_BUNDLE_GUI") != "1", reason="starts the window; set CORVUS_BUNDLE_GUI=1")
def test_the_interface_serves_its_own_core_and_a_signal_ends_it(tmp_path: Path) -> None:
    lock = tmp_path / ".corvus" / "corvus.lock"

    def port() -> int | None:
        try:
            return json.loads(lock.read_text().strip("\0\n ")).get("port") or None
        except (OSError, ValueError):
            return None

    process = subprocess.Popen([str(EXE)], env={**os.environ, "HOME": str(tmp_path), "CORVUS_ALLOW_MULTI": "1"},
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    served = None
    try:
        assert wait_for(lambda: port() is not None, 60), "the interface did not start its core"
        served = port()
        assert wait_for(lambda: get(served, "/api/version")[0] == 200)
        assert get(served, "/api/version")[1]["version"] == (RES / "VERSION").read_text().strip()
        assert get(served, "/api/mavlink/scope")[0] != 501
        children = subprocess.run(["pgrep", "-P", str(process.pid)], capture_output=True, text=True).stdout.split()
        assert children == [], "the backend is in the process: there must be no child"
        process.send_signal(signal.SIGTERM)
        deadline = time.monotonic() + 20
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.2)
        assert process.poll() is not None, "the interface did not quit"
        with pytest.raises(OSError), socket.create_connection(("127.0.0.1", served), timeout=1):
            pass
    finally:
        if process.poll() is None:
            process.kill()
        shutil.rmtree(tmp_path / ".corvus", ignore_errors=True)
