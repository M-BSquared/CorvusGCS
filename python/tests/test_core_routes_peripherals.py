"""The Rust peripherals area held to the Python server.

``serve.py`` and the Rust ``corvus-core`` binary run on identical state folders
and the same request goes to both (``tools/route_harness.py``). Skipped when the
binary is not built:

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import pathlib
import sys

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")


@pytest.fixture(scope="module")
def pair():
    with harness.Pair(seed=harness.standard_seed()) as p:
        yield p


@pytest.mark.parametrize("body", [
    {"host": "127.0.0.1"},
    {"host": "127.0.0.1", "wait_ms": 600},
    {"host": "127.0.0.1", "wait_ms": 100000},
    {"host": "127.0.0.1", "wait_ms": 1.5},
    {"host": "-oProxyCommand=x"},
    {"host": ""},
    {"host": "   "},
    {"host": "a b"},
    {"host": "a" * 300},
    {"host": 5},
    {"host": None},
    {},
    {"host": "127.0.0.1", "wait_ms": "2000"},
    {"host": "127.0.0.1", "wait_ms": True},
    {"host": "127.0.0.1", "wait_ms": None},
    {"wait_ms": 10},
])
def test_a_ping_answers_as_python_does(pair, body) -> None:
    diffs = pair.compare_post("/api/local/ping", body, ignore=["rtt_ms"])
    assert not diffs, "\n".join(diffs)


def test_the_rtk_status_is_the_pythons_while_it_is_off(pair) -> None:
    diffs = pair.compare_get("/api/rtk/status", ignore=["uptime", "source_age", "ports"])
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("name,body", [
    ("not_enough_for_ntrip", {"source": "ntrip", "enabled": True}),
    ("ntrip_without_mountpoint", {"source": "ntrip", "ntrip": {"host": "caster.example.com"}}),
    ("fixed_at_zero", {"source": "usb", "mode": "fixed", "enabled": True}),
    ("fixed_out_of_range_is_bounded", {"source": "usb", "mode": "fixed", "fixed": {"latitude": 91, "longitude": 8}}),
    ("fixed_ok", {"source": "usb", "mode": "fixed", "fixed": {"latitude": 47.123456789012, "longitude": 8.5, "altitude": 512.1}}),
    ("ntrip_ok", {"source": "ntrip", "ntrip": {"host": " caster.example.com ", "port": "2102", "mountpoint": "M",
                                               "username": "u", "password": "secret"}}),
    ("ntrip_keeps_the_password", {"source": "ntrip", "ntrip": {"host": "caster.example.com", "mountpoint": "M", "password": ""}}),
    ("ntrip_new_password", {"source": "ntrip", "ntrip": {"host": "caster.example.com", "mountpoint": "M", "password": "new"}}),
    ("survey_bounds", {"source": "usb", "mode": "survey", "survey_accuracy": 0, "survey_duration": 1, "baud": 99999999}),
    ("junk", {"source": 5, "mode": [], "enabled": "yes", "device": 3, "unknown": 1}),
    ("empty", {}),
])
def test_rtk_settings_are_refused_stored_and_answered_as_python_does(pair, name, body) -> None:
    diffs = pair.compare_post("/api/rtk/settings", body, ignore=["uptime", "source_age", "ports", "message", "state"])
    diffs += pair.compare_get("/api/config")
    assert not diffs, "\n".join(diffs)


def test_the_rtk_block_of_the_config_file_is_the_pythons_byte_for_byte(pair) -> None:
    py = (pair.py.state_dir / "config.json").read_text(encoding="utf-8")
    rs = (pair.rs.state_dir / "config.json").read_text(encoding="utf-8")
    assert rs == py


def test_restarting_a_survey_answers_as_python_does(pair) -> None:
    diffs = pair.compare_post("/api/rtk/restart", {}, ignore=["uptime", "source_age", "ports", "message", "state"])
    assert not diffs, "\n".join(diffs)


def test_the_sik_status_is_the_pythons(pair) -> None:
    diffs = pair.compare_get("/api/sik/status")
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path,body", [
    ("/api/sik/load", {}),
    ("/api/sik/load", {"device": ""}),
    ("/api/sik/load", {"device": 5}),
    ("/api/sik/load", {"device": "/dev/corvus-no-such-port", "baud": 0}),
    ("/api/sik/load", {"device": "/dev/corvus-no-such-port", "baud": True}),
    ("/api/sik/load", {"device": "/dev/corvus-no-such-port", "baud": "57600"}),
    ("/api/sik/load", {"device": "/dev/corvus-no-such-port", "baud": 57600.0}),
    ("/api/sik/load", {"device": "/dev/corvus-no-such-port", "remote": "yes"}),
    ("/api/sik/save", {"device": "/dev/corvus-no-such-port"}),
    ("/api/sik/save", {"device": "/dev/corvus-no-such-port", "local": {}, "remote": {}}),
    ("/api/sik/save", {"device": "/dev/corvus-no-such-port", "local": []}),
    ("/api/sik/save", {"device": "/dev/corvus-no-such-port", "remote": "x"}),
    ("/api/sik/save", {"device": "/dev/corvus-no-such-port", "local": {"NETID": 3}}),
    ("/api/sik/reset", {"device": "/dev/corvus-no-such-port", "target": "both"}),
    ("/api/sik/reset", {"device": "/dev/corvus-no-such-port", "target": 5}),
    ("/api/sik/reset", {"device": "/dev/corvus-no-such-port"}),
    ("/api/sik/reset", {"device": "  /dev/corvus-no-such-port  ", "target": "remote"}),
])
def test_the_sik_endpoints_refuse_and_fail_as_python_does(pair, path, body) -> None:
    diffs = pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("command", [
    "", "   ", "help", "HELP me", "arm", "disarm", "mode", "mode auto", "takeoff", "takeoff 12", "takeoff 1 2", "takeoff abc",
    "takeoff nan", "takeoff 500", "takeoff 0", "takeoff 1_0", "land", "rtl", "shell", "nsh", "shell ls /fs", "nsh  Listener x",
    "listener sensor_combined", "top", "perf", "whatever", "Arm now", 5, None,
])
def test_a_console_command_answers_as_python_does(pair, command) -> None:
    diffs = pair.compare_post("/api/console/command", {"command": command})
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("name,body", [
    ("no_text", {}),
    ("blank", {"text": "  \n "}),
    ("not_text", {"text": 5}),
    ("plain", {"text": "a line\nanother", "filename": "my log"}),
    ("with_newline", {"text": "x\n", "filename": "keep.LOG"}),
    ("path_escape", {"text": "x", "filename": "../../etc/evil"}),
    ("odd_chars", {"text": "x", "filename": 'a<b>:c"d|e?f*.log'}),
    ("dots", {"text": "x", "filename": ".."}),
    ("long", {"text": "x", "filename": "n" * 300}),
    ("unicode", {"text": "grüßé", "filename": "ä ö"}),
])
def test_a_console_transcript_is_saved_as_python_saves_it(pair, name, body) -> None:
    diffs = pair.compare_post("/api/console/save", body)
    assert not diffs, "\n".join(diffs)
    for d in (pair.py.state_dir / "logs", pair.rs.state_dir / "logs"):
        assert d.is_dir() or name in {"no_text", "blank", "not_text"}


def test_the_saved_transcripts_are_the_same_files(pair) -> None:
    def files(state):
        d = state / "logs"
        return {p.name: p.read_text(encoding="utf-8") for p in sorted(d.glob("*.log")) if not p.name.startswith("corvus-console_")}

    assert files(pair.rs.state_dir) == files(pair.py.state_dir)
    assert files(pair.rs.state_dir)


def test_the_local_status_is_the_pythons(pair) -> None:
    diffs = pair.compare_get("/api/local/status")
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("body", [
    {"command": "echo hi"},
    {"command": "echo out; echo err 1>&2; exit 3"},
    {"command": "exit 0"},
    {"command": "  "},
    {"command": ""},
    {"command": "x" * 9000},
    {"command": "true", "directory": "/no/such/folder"},
    {"command": "pwd", "directory": "/"},
    {"command": 5},
    {"command": "true", "directory": 5},
    {},
    {"command": "nonexistent-program-xyz"},
])
def test_a_background_program_answers_as_python_does(pair, body) -> None:
    diffs = pair.compare_post("/api/local/run", body)
    assert not diffs, "\n".join(diffs)


def _sse(url: str, path: str, until: str, timeout: float = 10.0) -> list[tuple[str, dict]]:
    import http.client
    import json as _json
    import time as _time
    import urllib.parse as _up

    parts = _up.urlparse(url)
    conn = http.client.HTTPConnection(parts.hostname, parts.port, timeout=timeout)
    conn.request("GET", path)
    resp = conn.getresponse()
    assert resp.status == 200 and resp.getheader("Content-Type") == "text/event-stream"
    events: list[tuple[str, dict]] = []
    text = ""
    end = _time.time() + timeout
    event = ""
    while _time.time() < end:
        line = resp.fp.readline().decode()
        if not line:
            break
        line = line.rstrip("\n")
        if line.startswith("event: "):
            event = line[7:]
        elif line.startswith("data: "):
            data = _json.loads(line[6:])
            events.append((event, data))
            text += data.get("text", "") if isinstance(data, dict) else ""
            if until in text or event == "closed":
                break
    conn.close()
    return events


@pytest.mark.skipif(sys.platform == "win32", reason="a terminal needs a pty")
def test_a_local_terminal_works_through_the_ssh_endpoints_as_python_does(pair) -> None:
    for server in (pair.py, pair.rs):
        status, body = harness.http("POST", server.url + "/api/local/connect", {"name": "t1", "directory": "/"})
        assert (status, body["ok"], body["connected"], body["name"]) == (200, True, True, "t1"), (server.name, body)
        status, sessions = harness.http("GET", server.url + "/api/ssh/sessions")
        assert [(s["name"], s["host"], s["port"], s["connected"]) for s in sessions["sessions"]] == [("t1", "this computer", 0, True)]
        assert harness.http("POST", server.url + "/api/ssh/send", {"name": "t1", "data": "echo marker-$((20+22))\n"}) == (200, {"ok": True})
        assert harness.http("POST", server.url + "/api/ssh/resize", {"name": "t1", "cols": 100, "rows": 30}) == (200, {"ok": True})
        events = _sse(server.url, "/api/ssh/stream?name=t1", "marker-42\r")
        joined = "".join(d.get("text", "") for e, d in events if e == "output")
        assert "Shell on this computer (" in joined and "marker-42" in joined, (server.name, joined)
        assert all(d["name"] == "t1" for e, d in events if e == "output")
        assert harness.http("POST", server.url + "/api/ssh/disconnect", {"name": "t1"}) == (200, {"ok": True})
        assert harness.http("POST", server.url + "/api/ssh/disconnect", {"name": "t1"}) == (200, {"ok": False})
        events = _sse(server.url, "/api/ssh/stream?name=t1", "x")
        assert events == [("closed", {"name": "t1"})], (server.name, events)
        assert _sse(server.url, "/api/ssh/stream", "x") == [("closed", {"name": None})]


@pytest.mark.parametrize("path,body", [
    ("/api/local/connect", {}),
    ("/api/local/connect", {"name": ""}),
    ("/api/local/connect", {"name": "x" * 200}),
    ("/api/local/connect", {"name": "t2", "directory": "/no/such/dir"}),
    ("/api/ssh/send", {"name": "ghost", "data": "x"}),
    ("/api/ssh/send", {"name": "ghost", "data": 5}),
    ("/api/ssh/send", {"data": "x"}),
    ("/api/ssh/resize", {"name": "ghost", "cols": 10, "rows": 10}),
    ("/api/ssh/resize", {"name": "ghost", "cols": 0, "rows": 10}),
    ("/api/ssh/resize", {"name": "ghost", "cols": "x", "rows": 10}),
    ("/api/ssh/resize", {"name": "ghost", "cols": True, "rows": 10}),
    ("/api/ssh/resize", {"name": "ghost", "cols": 10}),
    ("/api/ssh/resize", {"cols": 10, "rows": 10}),
    ("/api/ssh/disconnect", {}),
    ("/api/ssh/disconnect", {"name": "ghost"}),
])
def test_the_terminal_endpoints_refuse_as_python_does(pair, path, body) -> None:
    diffs = pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


class _Ws:
    """The least of a WebSocket client: one handshake, masked text frames out, frames in."""

    def __init__(self, server, path: str, timeout: float = 10.0) -> None:
        import base64
        import os as _os
        import socket

        self.sock = socket.create_connection(("127.0.0.1", server.port), timeout=timeout)
        key = base64.b64encode(_os.urandom(16)).decode()
        self.sock.sendall((
            f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{server.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            head += self.sock.recv(1)
        self.status = int(head.split(b" ", 2)[1])
        self.buf = b""

    def send(self, text: str) -> None:
        import os as _os

        data = text.encode()
        mask = _os.urandom(4)
        header = bytes([0x81, 0x80 | len(data)]) + mask if len(data) < 126 else bytes([0x81, 0x80 | 126]) + len(data).to_bytes(2, "big") + mask
        self.sock.sendall(header + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _need(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def receive(self) -> tuple[int, str]:
        b0, b1 = self._need(2)
        n = b1 & 0x7F
        if n == 126:
            n = int.from_bytes(self._need(2), "big")
        return b0 & 0x0F, self._need(n).decode("utf-8", "replace")

    def close(self) -> None:
        self.sock.close()


@pytest.mark.skipif(sys.platform == "win32", reason="a terminal needs a pty")
def test_a_terminal_over_a_websocket_answers_as_python_does(pair) -> None:
    import json as _json

    for server in (pair.py, pair.rs):
        ws = _Ws(server, "/api/ssh/ws?name=ghost")
        assert ws.status == 101, server.name
        assert _json.loads(ws.receive()[1]) == {"type": "closed", "name": "ghost"}
        ws.close()
        ws = _Ws(server, "/api/ssh/ws")
        assert _json.loads(ws.receive()[1]) == {"type": "closed", "name": None}
        ws.close()

        status, body = harness.http("POST", server.url + "/api/local/connect", {"name": "w1", "directory": "/"})
        assert body["connected"], (server.name, body)
        ws = _Ws(server, "/api/ssh/ws?name=w1")
        assert ws.status == 101
        ws.send(_json.dumps({"type": "input", "data": "echo ws-$((6*7))\n"}))
        ws.send("not json")
        ws.send(_json.dumps({"type": "resize", "cols": 90, "rows": 20}))
        ws.send(_json.dumps({"type": "resize", "cols": 0, "rows": 20}))
        seen = ""
        while "ws-42\r" not in seen:
            op, text = ws.receive()
            msg = _json.loads(text)
            assert msg["type"] == "output", (server.name, msg)
            seen += msg["text"]
        assert "Shell on this computer (" in seen
        ws.send(_json.dumps({"type": "input", "data": "exit\n"}))
        kinds = []
        try:
            while True:
                op, text = ws.receive()
                kinds.append(_json.loads(text)["type"])
                if kinds[-1] == "closed":
                    break
        except EOFError:
            pass
        assert kinds[-1] == "closed", (server.name, kinds)
        ws.close()
        harness.http("POST", server.url + "/api/ssh/disconnect", {"name": "w1"})
