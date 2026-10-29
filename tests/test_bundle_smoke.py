"""The built macOS bundle, run.

Skipped unless a bundle has been built (``./build-macos-app.sh`` puts it in
``dist/``; ``CORVUS_APP_BUNDLE`` names another). The backend is started from
the bundle's own interpreter and files, the way the launcher sets it up, with
a simulated vehicle on UDP: the version it serves is the bundle's, the Rust
link carries the vehicle, the optional pieces compiled into corvus_link (TLS
for ``wss:``) are present, and a signal stops it cleanly. Afterwards the
bundle's signature must still verify, because a run that wrote a file into
the bundle would break the seal an operator's Mac checks.

The interface itself (``Contents/MacOS/corvus-gcs``, the Rust app that starts
that backend as its child) is started only when ``CORVUS_BUNDLE_GUI=1``,
because it needs a desktop session.
"""
from __future__ import annotations

import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

if platform.system() != "Darwin":
    pytest.skip("the macOS bundle", allow_module_level=True)

from tests.test_process_lifecycle import Sim, free_port, get, ports_are_free, wait_for  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
BUNDLE = Path(os.environ.get("CORVUS_APP_BUNDLE", ROOT / "dist" / "Corvus GCS.app"))

if not BUNDLE.is_dir():
    pytest.skip(f"no bundle at {BUNDLE}; build one with ./build-macos-app.sh", allow_module_level=True)

RES = BUNDLE / "Contents" / "Resources"
PYROOT = RES / "python"
APP = RES / "app"


def bundle_python_version() -> str:
    return next((PYROOT / "lib").glob("python3.*")).name.removeprefix("python")


def bundle_env(home: Path) -> dict:
    env = {k: v for k, v in os.environ.items() if k not in ("VIRTUAL_ENV", "PYTHONPATH", "PYTHONHOME", "CONDA_PREFIX")}
    env.update(
        HOME=str(home),
        PYTHONHOME=str(PYROOT),
        PYTHONPATH=f"{APP}:{PYROOT}/lib/python{bundle_python_version()}/site-packages",
        PYTHONDONTWRITEBYTECODE="1",
    )
    return env


def run_in_bundle(code: str, home: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(PYROOT / "bin" / "python3"), "-c", code, *args], cwd=APP, env=bundle_env(home),
                          capture_output=True, text=True, timeout=60)


def test_the_bundle_carries_its_own_version_and_runtime(tmp_path: Path) -> None:
    done = run_in_bundle(
        "import sys; from corvus.version import get_version; import corvus_link; "
        "print(get_version()); print(sys.prefix)", tmp_path)
    assert done.returncode == 0, done.stderr
    version, prefix = done.stdout.split("\n")[:2]
    assert version == (ROOT / "VERSION").read_text().strip() == (APP / "VERSION").read_text().strip()
    assert prefix == str(PYROOT)


def test_the_rust_extension_in_the_bundle_has_every_feature_compiled_in(tmp_path: Path) -> None:
    csv = tmp_path / "t.csv"
    csv.write_text("TimeUS;A\n1;2.5\n")
    done = run_in_bundle(
        "import sys, corvus_link; from corvus_link import mavutil, rtk, sik, elrs, flash;"
        "r = mavutil.mavlink_connection('csv:' + sys.argv[1]); print(r.recv_msg().A);"
        "c = mavutil.mavlink_connection('wss:127.0.0.1:1'); print('wss', c.fd);"
        "print(rtk.crc24q(b'123456789') == 0xcde703, flash.bl_crc32(b'123456789') == 0x2dfd2d88,"
        " elrs.crc8_dvb_s2(b'123456789') == 0xbc, sik.SikConfigEngine.__name__)", tmp_path, str(csv))
    assert done.returncode == 0, done.stderr
    assert done.stdout.split("\n")[:3] == ["2.5", "wss None", "True True True SikConfigEngine"]


def test_the_backend_from_the_bundle_serves_a_simulated_vehicle_and_stops(tmp_path: Path) -> None:
    udp = free_port(socket.SOCK_DGRAM)
    http = free_port()
    home = tmp_path / "home"
    home.mkdir()
    log = open(tmp_path / "serve.log", "wb")  # noqa: SIM115 - closed below
    process = subprocess.Popen([str(PYROOT / "bin" / "python3"), "serve.py", str(http), f"udpin:127.0.0.1:{udp}"],
                               cwd=APP, env=bundle_env(home), stdout=log, stderr=subprocess.STDOUT)
    sim = Sim(udp)
    try:
        assert wait_for(lambda: get(http, "/api/version")["version"] == (APP / "VERSION").read_text().strip(), 40)
        assert wait_for(lambda: get(http, "/api/state")["connected"] is True), (tmp_path / "serve.log").read_text()[-1500:]
        assert get(http, "/api/state")["autopilot_stack"] == "px4"
        assert get(http, "/api/rtk/status")["state"] == "off"
        process.send_signal(signal.SIGINT)
        assert process.wait(timeout=15) == 0
    finally:
        sim.stop()
        if process.poll() is None:
            process.kill()
        log.close()
    assert "Traceback" not in (tmp_path / "serve.log").read_text()
    assert ports_are_free(http, udp)


def test_running_the_bundle_leaves_its_signature_intact() -> None:
    done = subprocess.run(["codesign", "--verify", "--deep", "--strict", str(BUNDLE)], capture_output=True, text=True)
    assert done.returncode == 0, done.stderr


@pytest.mark.skipif(os.environ.get("CORVUS_BUNDLE_GUI") != "1", reason="starts the window; set CORVUS_BUNDLE_GUI=1")
def test_the_interface_starts_its_backend_and_takes_it_down_on_a_signal(tmp_path: Path) -> None:
    process = subprocess.Popen([str(BUNDLE / "Contents" / "MacOS" / "corvus-gcs")],
                               env={**os.environ, "HOME": str(tmp_path), "CORVUS_ALLOW_MULTI": "1"},
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    python = str(PYROOT / "bin" / "python3")

    def backends() -> list[str]:
        out = subprocess.run(["pgrep", "-f", python], capture_output=True, text=True).stdout
        return out.split()

    try:
        assert wait_for(lambda: bool(backends()), 60), "the interface did not start the backend"
        assert process.poll() is None
        process.send_signal(signal.SIGTERM)
        deadline = time.monotonic() + 20
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.2)
        assert process.poll() is not None, "the interface did not quit"
        assert wait_for(lambda: not backends(), 15), "the backend outlived the interface"
    finally:
        if process.poll() is None:
            process.kill()
        shutil.rmtree(tmp_path / ".corvus", ignore_errors=True)
    assert sys.platform == "darwin"
