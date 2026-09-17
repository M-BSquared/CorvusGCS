"""Standing a scene up: a sandboxed Corvus, an aircraft, and a URL.

Running a scene starts two things. The aircraft is in-process (see
:mod:`vehicle`). Corvus itself is a subprocess — the real ``serve.py``, not a
stub — because the whole value of these pictures is that they are the real
program.

The one thing this module is careful about is *the operator's own Corvus*.
A screenshot run must not touch it, so:

* The backend runs with ``HOME`` pointed at a sandbox directory, which is what
  moves ``~/.corvus`` — config, tlogs, downloaded logs, exported parameters —
  out of the operator's installation and into a throwaway one.
* ``CORVUS_ALLOW_MULTI=1`` opts out of the one-instance lock, so a scene can be
  photographed while the operator's own Corvus is running.
* The tile cache is the exception: it is read-only for our purposes and takes a
  long time to fill, so by default the sandbox *reuses* the real one. The map in
  a screenshot should be the imagery the operator already has on disk, not an
  empty grid. ``--isolated-tiles`` opts out.
* The MAVLink port defaults to 14555, not 14550, so a scene never fights a real
  ground station or a SITL for the socket.
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

from . import recipe as recipe_mod
from .library import CHATTER, Scene
from .vehicle import SimVehicle, VehicleOptions

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_HTTP_PORT = 8777
DEFAULT_MAV_PORT = 14555


@dataclass
class RunnerOptions:
    http_port: int = DEFAULT_HTTP_PORT
    mav_port: int = DEFAULT_MAV_PORT
    sandbox: Path | None = None        # None = a per-scene directory under /tmp
    reuse_tiles: bool = True
    start_backend: bool = True         # False = drive a Corvus already running
    verbose: bool = False
    keep_sandbox: bool = False


class SceneRunner:
    """Start a scene, hand back its URL, and take it all down again."""

    def __init__(self, scene: Scene, options: RunnerOptions | None = None,
                 log: Callable[[str], None] | None = None) -> None:
        self.scene = scene
        self.opts = options or RunnerOptions()
        self.log = log or (lambda text: print(text, flush=True))
        self.sandbox = (self.opts.sandbox
                        or Path(os.environ.get("TMPDIR", "/tmp"))
                        / f"corvus-scene-{scene.id}")
        self.backend: subprocess.Popen | None = None
        self.vehicle: SimVehicle | None = None
        self._backend_log: Any = None

    # -- the sandbox ------------------------------------------------------

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.opts.http_port}/"

    def _write_config(self) -> Path:
        """Write the sandbox's ``~/.corvus/config.json`` for this scene.

        The backend half of the interface state lives here — theme, interface
        size, map provider, the on-screen controls — and the frontend takes the
        config as authoritative over its own cached copy, which is what makes a
        scene reproducible in a browser profile that has been used before.
        """
        corvus_dir = self.sandbox / ".corvus"
        corvus_dir.mkdir(parents=True, exist_ok=True)
        scene = self.scene

        tiles = (str(Path.home() / ".corvus" / "tiles") if self.opts.reuse_tiles
                 else str(corvus_dir / "tiles"))
        config = {
            "mavlink_connection": f"udpin:127.0.0.1:{self.opts.mav_port}",
            "http_port": self.opts.http_port,
            "tile_cache_dir": tiles,
            "tlog_dir": str(corvus_dir / "logs"),
            "params_dir": str(corvus_dir / "params"),
            "firmware_dir": str(corvus_dir / "firmware"),
            "log_download_dir": str(corvus_dir / "flightlogs"),
            "theme": {"name": scene.theme},
            "map": {"provider": scene.map_provider, "base_layer": scene.map_layer},
            "ui": {"scale": scene.scale, "topbar_status_dots": scene.topbar_dots},
            "controls": {"virtual_joystick": scene.virtual_joystick},
            # No release check: a screenshot run must not reach the network, and
            # an update dialog across the frame is the one thing that would
            # ruin every picture at once.
            "updates": {"check": False},
            "forwarding": {"enabled": False},
        }
        path = corvus_dir / "config.json"
        path.write_text(json.dumps(config, indent=2) + "\n")
        return path

    # -- lifecycle --------------------------------------------------------

    def start(self) -> None:
        config_path = self._write_config()
        self.log(f"sandbox    {self.sandbox}")
        self.log(f"config     {config_path}")

        if self.opts.start_backend:
            self._start_backend()
        else:
            self.log("backend    (not started — using the Corvus already running)")

        self._start_vehicle()

        if self.opts.start_backend and not self._wait_for_link(timeout=25.0):
            self.log("warning    the link did not come up within 25 s; the "
                     "backend log is in the sandbox")

    def _start_backend(self) -> None:
        if _port_in_use(self.opts.http_port):
            raise RuntimeError(
                f"port {self.opts.http_port} is already in use — pass --port, or "
                f"--no-backend to photograph the Corvus that is already there")

        env = dict(os.environ)
        env["HOME"] = str(self.sandbox)
        env["USERPROFILE"] = str(self.sandbox)     # the same move on Windows
        env["CORVUS_ALLOW_MULTI"] = "1"
        env["PYTHONUNBUFFERED"] = "1"

        log_path = self.sandbox / "backend.log"
        self._backend_log = log_path.open("w")
        self.backend = subprocess.Popen(
            [sys.executable, "serve.py", str(self.opts.http_port),
             f"udpin:127.0.0.1:{self.opts.mav_port}"],
            cwd=str(REPO_ROOT), env=env,
            stdout=(None if self.opts.verbose else self._backend_log),
            stderr=subprocess.STDOUT if not self.opts.verbose else None,
        )
        self.log(f"backend    serve.py on {self.url} (pid {self.backend.pid})")
        if not _wait_for_http(self.url + "api/version", timeout=20.0):
            raise RuntimeError(
                f"the backend did not answer within 20 s — see {log_path}")

    def _start_vehicle(self) -> None:
        scene = self.scene
        options = VehicleOptions(
            params=scene.build_params(),
            model=scene.build_model(),
            armed=scene.armed,
            mode=scene.mode,
            rc=scene.rc_state,
            logs=scene.build_logs(),
            chatter=CHATTER(scene.mode) if scene.chatter else (),
            calibration_pause_after=scene.calibration_pause_after,
        )
        self.vehicle = SimVehicle(
            f"udpout:127.0.0.1:{self.opts.mav_port}", options,
            on_event=(lambda text: self.log(f"vehicle    {text}"))
            if self.opts.verbose else (lambda _text: None),
        )
        self.vehicle.start()
        self.log(f"vehicle    {scene.airframe}, {len(options.params)} parameters, "
                 f"flying {scene.path} from {scene.home[0]:.5f},{scene.home[1]:.5f}")

    def _wait_for_link(self, timeout: float) -> bool:
        """Poll ``/api/state`` until the backend says it has the aircraft."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(self.url + "api/state", timeout=2.0) as r:
                    state = json.loads(r.read().decode("utf-8"))
                if state.get("connected"):
                    self.log(f"link       up — {state.get('vehicle_type', '?')} / "
                             f"{state.get('autopilot', '?')} / "
                             f"{state.get('px4_version') or 'version pending'}")
                    return True
            except (urllib.error.URLError, OSError, ValueError):
                pass
            time.sleep(0.5)
        return False

    def stop(self) -> None:
        if self.vehicle is not None:
            self.vehicle.stop()
            self.vehicle = None
        backend = self.backend
        self.backend = None
        if backend is not None:
            backend.terminate()
            try:
                backend.wait(timeout=10)
            except subprocess.TimeoutExpired:
                backend.kill()
                backend.wait(timeout=5)
        if self._backend_log is not None:
            self._backend_log.close()
            self._backend_log = None
        if not self.opts.keep_sandbox and self.opts.sandbox is None:
            # Only a sandbox we invented is ours to delete; one the operator
            # named stays, whatever is in it.
            shutil.rmtree(self.sandbox, ignore_errors=True)

    def __enter__(self) -> "SceneRunner":
        self.start()
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.stop()

    # -- what to do with it ----------------------------------------------

    def recipe_text(self) -> str:
        return recipe_mod.as_text(self.scene, self.url)

    def recipe_json(self) -> dict[str, Any]:
        return recipe_mod.as_json(self.scene, self.url)


def _port_in_use(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.3)
        return sock.connect_ex(("127.0.0.1", port)) == 0


def _wait_for_http(url: str, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2.0) as response:
                if response.status == 200:
                    return True
        except (urllib.error.URLError, OSError):
            time.sleep(0.4)
    return False
