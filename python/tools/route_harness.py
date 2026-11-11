#!/usr/bin/env python3
"""Hold the Rust core to the Python server, route by route.

Starts the Python backend (``serve.py`` in a sandbox ``HOME``) and the Rust
core (``corvus-core``, the headless binary of ``src/core``) on identical state
folders, replays the same requests against both and diffs the JSON. The tests
that use it are in ``tests/test_core_routes.py``; each area of the port adds
its own routes to a test there and reuses everything here:

    with Pair(seed=standard_seed()) as pair:
        diffs = pair.compare_get("/api/battery", ignore=["battery_temperature"])
        assert not diffs, "\\n".join(diffs)

or from a shell, for the routes the foundation owns:

    python tools/route_harness.py            # prints every difference, exits 1 if any
    python tools/route_harness.py /api/version /api/config

The Rust binary is ``$CORVUS_CORE_BIN`` or ``target/debug/corvus-core`` (build
it with ``cargo build -p corvus_core --bin corvus-core``). Nothing here touches
the operator's own ``~/.corvus``: both servers run on sandbox folders, opt out
of the instance lock and keep MAVLink off the ports a real station holds.
"""
from __future__ import annotations

import contextlib
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Iterable
from typing import Any

PYTHON_ROOT = pathlib.Path(__file__).resolve().parent.parent
REPO_ROOT = PYTHON_ROOT.parent


def core_binary() -> pathlib.Path | None:
    """The ``corvus-core`` binary to test, or None when it is not built."""
    env = os.environ.get("CORVUS_CORE_BIN")
    candidates = [pathlib.Path(env)] if env else []
    suffix = ".exe" if os.name == "nt" else ""
    candidates += [REPO_ROOT / "target" / profile / f"corvus-core{suffix}" for profile in ("debug", "release")]
    return next((c for c in candidates if c.is_file()), None)


def free_port(kind: int = socket.SOCK_STREAM) -> int:
    with socket.socket(socket.AF_INET, kind) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def standard_seed() -> dict[str, Any]:
    """The state both servers start from: a config (so this is not a first
    start) and one operator language beside the bundled ones."""
    return {
        "config": {
            "mavlink_connection": "udp:0.0.0.0:14550",
            "http_port": 8000,
            "theme": {"name": "green"},
            "ui": {"scale": 1.25, "units": {"length": "ft"}},
            "ssh_connections": [{"name": "a", "host": "h", "port": 22, "username": "u",
                                 "key_path": "", "password": "secret"}],
            "map_tokens": {"mapbox": "pk.secret"},
            "language": "de",
        },
        "languages": {
            "es.json": {"meta": {"name": "Spanish", "native_name": "Espanol"}, "strings": {"Settings": "Ajustes"}},
        },
    }


def seed_state(state_dir: pathlib.Path, seed: dict[str, Any] | None) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    if not seed:
        return
    if seed.get("setup") is not None:
        seed["setup"](state_dir)
    if seed.get("config") is not None:
        (state_dir / "config.json").write_text(json.dumps(seed["config"], indent=2) + "\n", encoding="utf-8")
    for name, data in (seed.get("languages") or {}).items():
        path = state_dir / "languages" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data), encoding="utf-8")


def http(method: str, url: str, body: Any = None, timeout: float = 20.0) -> tuple[int, Any]:
    """One request; ``(status, parsed JSON)`` or the text when it is not JSON."""
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"} if data is not None else {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status, raw = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        status, raw = exc.code, exc.read()
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, raw.decode("utf-8", "replace")


def diff_json(py: Any, rs: Any, ignore: Iterable[str] = (), path: str = "") -> list[str]:
    """Differences between two parsed answers, as ``path: python != rust``.

    *ignore* names keys (at any depth) whose values are not compared, for what
    legitimately differs between two processes (a clock, a pid, a path).
    Numbers compare by value, so ``1`` equals ``1.0``; key order is not compared.
    """
    skip = set(ignore)
    out: list[str] = []
    if isinstance(py, dict) and isinstance(rs, dict):
        for key in sorted(set(py) | set(rs)):
            if key in skip:
                continue
            here = f"{path}.{key}" if path else key
            if key not in py:
                out.append(f"{here}: missing in python, rust has {json.dumps(rs[key])[:120]}")
            elif key not in rs:
                out.append(f"{here}: missing in rust, python has {json.dumps(py[key])[:120]}")
            else:
                out += diff_json(py[key], rs[key], skip, here)
    elif isinstance(py, list) and isinstance(rs, list):
        if len(py) != len(rs):
            out.append(f"{path}: python has {len(py)} items, rust {len(rs)}")
        for i, (a, b) in enumerate(zip(py, rs)):
            out += diff_json(a, b, skip, f"{path}[{i}]")
    elif isinstance(py, bool) or isinstance(rs, bool):
        if py is not rs:
            out.append(f"{path}: {py!r} != {rs!r}")
    elif isinstance(py, (int, float)) and isinstance(rs, (int, float)):
        if py != rs:
            out.append(f"{path}: {py!r} != {rs!r}")
    elif py != rs:
        out.append(f"{path}: {json.dumps(py)[:120]} != {json.dumps(rs)[:120]}")
    return out


class _Server:
    def __init__(self, name: str, state_dir: pathlib.Path, log_dir: pathlib.Path) -> None:
        self.name, self.state_dir, self.proc = name, state_dir, None
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self._log = open(log_dir / f"{name}.log", "wb")

    def _wait(self, timeout: float = 60.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"{self.name} exited at startup ({self.proc.returncode}); see {self._log.name}")
            try:
                if http("GET", self.url + "/api/version", timeout=2)[0] == 200:
                    return
            except OSError:
                pass
            time.sleep(0.2)
        raise RuntimeError(f"{self.name} did not answer on {self.url}")

    def stop(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self._log.close()


class PythonServer(_Server):
    def start(self) -> None:
        env = dict(os.environ, HOME=str(self.state_dir.parent), USERPROFILE=str(self.state_dir.parent),
                   CORVUS_ALLOW_MULTI="1")
        env.pop("CORVUS_BIND", None)
        mav = f"udp:127.0.0.1:{free_port(socket.SOCK_DGRAM)}"
        self.proc = subprocess.Popen([sys.executable, "serve.py", str(self.port), mav], cwd=PYTHON_ROOT, env=env,
                                     stdout=self._log, stderr=subprocess.STDOUT)
        self._wait()


class RustServer(_Server):
    def start(self, binary: pathlib.Path) -> None:
        env = dict(os.environ, CORVUS_ALLOW_MULTI="1")
        env.pop("CORVUS_BIND", None)
        self.proc = subprocess.Popen([str(binary), "--port", str(self.port), "--state-dir", str(self.state_dir)],
                                     env=env, stdout=self._log, stderr=subprocess.STDOUT)
        self._wait()


class Pair:
    """The Python server and the Rust core, started on the same seed."""

    def __init__(self, seed: dict[str, Any] | None = None, binary: pathlib.Path | None = None) -> None:
        self.seed = seed
        self.binary = binary or core_binary()
        self._tmp: pathlib.Path | None = None
        self.py: PythonServer
        self.rs: RustServer

    def __enter__(self) -> Pair:
        if self.binary is None:
            raise RuntimeError("corvus-core is not built: cargo build -p corvus_core --bin corvus-core")
        self._tmp = pathlib.Path(tempfile.mkdtemp(prefix="corvus-harness-"))
        py_state = self._tmp / "py" / ".corvus"
        rs_state = self._tmp / "rs" / ".corvus"
        for state in (py_state, rs_state):
            seed_state(state, self.seed)
        self.py = PythonServer("python", py_state, self._tmp)
        self.rs = RustServer("rust", rs_state, self._tmp)
        try:
            self.py.start()
            self.rs.start(self.binary)
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *exc: object) -> None:
        for server in (getattr(self, "rs", None), getattr(self, "py", None)):
            if server is not None:
                with contextlib.suppress(Exception):
                    server.stop()
        if self._tmp is not None and not os.environ.get("CORVUS_HARNESS_KEEP"):
            shutil.rmtree(self._tmp, ignore_errors=True)

    def _norm(self, value: Any) -> Any:
        """Paths of the two sandboxes read the same."""
        text = json.dumps(value)
        text = text.replace(str(self.py.state_dir), "<state>").replace(str(self.rs.state_dir), "<state>")
        return json.loads(text)

    def both(self, method: str, path: str, body: Any = None) -> tuple[tuple[int, Any], tuple[int, Any]]:
        return http(method, self.py.url + path, body), http(method, self.rs.url + path, body)

    def compare(self, method: str, path: str, body: Any = None, ignore: Iterable[str] = (),
                transform: Callable[[Any], Any] | None = None) -> list[str]:
        """Send one request to both; the differences in status and body."""
        (ps, pb), (rs, rb) = self.both(method, path, body)
        pb, rb = self._norm(pb), self._norm(rb)
        if transform is not None:
            pb, rb = transform(pb), transform(rb)
        out = [f"status: python {ps} != rust {rs}"] if ps != rs else []
        return [f"{method} {path} {d}" for d in out + diff_json(pb, rb, ignore)]

    def rs_json(self, method: str, path: str, body: Any = None) -> Any:
        """The Rust core's answer alone, for a test that states what it must be."""
        return http(method, self.rs.url + path, body)[1]

    def compare_get(self, path: str, ignore: Iterable[str] = (), **kw: Any) -> list[str]:
        return self.compare("GET", path, None, ignore, **kw)

    def compare_post(self, path: str, body: Any, ignore: Iterable[str] = (), **kw: Any) -> list[str]:
        return self.compare("POST", path, body, ignore, **kw)


# What differs between two live processes on purpose, per route.
STATE_IGNORE = ("mission_revision", "link_status", "link_connection", "link_error", "link_auto", "link_quality", "link_suggestion",
                "link_bootloader", "time", "boot_ms", "px4_version")

FOUNDATION_GET: list[tuple[str, tuple[str, ...]]] = [
    ("/api/version", ()),
    ("/api/config", ()),
    ("/api/welcome", ()),
    ("/api/languages", ()),
    ("/api/languages/strings?code=en", ()),
    ("/api/languages/strings?code=de", ()),
    ("/api/languages/strings?code=fr", ()),
    ("/api/languages/strings?code=dl", ()),
    ("/api/languages/strings?code=es", ()),
    ("/api/languages/strings?code=xx", ()),
    ("/api/languages/strings?code=de-1", ()),
    ("/api/languages/strings?code=Deutsch", ()),
    ("/api/languages/strings", ()),
    ("/api/state", STATE_IGNORE),
]


def own_strings_only(code: str, seed: dict[str, Any] | None = None) -> Callable[[Any], Any]:
    """Keep the texts of the language files themselves in an answer of
    ``/api/languages/strings``. The bundled plugins' own ``lang/`` files are
    merged into it by the Python server; those belong to the plugins area,
    whose test compares them and drops this filter."""
    own: set[str] = set()
    for path in (REPO_ROOT / "src" / "lang" / f"{code}.json",):
        if path.is_file():
            own |= set(json.loads(path.read_text(encoding="utf-8")).get("strings", {}))
    own |= set(((seed or {}).get("languages") or {}).get(f"{code}.json", {}).get("strings", {}))

    def keep(answer: Any) -> Any:
        if isinstance(answer, dict) and isinstance(answer.get("strings"), dict):
            answer = dict(answer, strings={k: v for k, v in answer["strings"].items() if k in own})
        return answer
    return keep


def run_foundation(pair: Pair) -> list[str]:
    diffs: list[str] = []
    for path, ignore in FOUNDATION_GET:
        transform = None
        if path.startswith("/api/languages/strings?code="):
            transform = own_strings_only(path.rsplit("=", 1)[1], pair.seed)
        diffs += pair.compare_get(path, ignore, transform=transform)
    return diffs


def main(argv: list[str]) -> int:
    if core_binary() is None:
        print("corvus-core is not built: cargo build -p corvus_core --bin corvus-core", file=sys.stderr)
        return 2
    wanted = argv[1:]
    with Pair(seed=standard_seed()) as pair:
        diffs: list[str] = []
        if wanted:
            for path in wanted:
                diffs += pair.compare_get(path, STATE_IGNORE if path.startswith("/api/state") else ())
        else:
            diffs = run_foundation(pair)
    for d in diffs:
        print(d)
    print(f"{len(diffs)} difference(s)")
    return 1 if diffs else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
