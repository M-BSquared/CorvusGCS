"""The one-shot pinger behind Schwalby's companion indicator (corvus/net_probe.py)."""
from __future__ import annotations

import shutil
import sys
import threading
import time
from typing import Any

import pytest

from corvus import net_probe
from corvus.server import CorvusHandler


@pytest.mark.parametrize("host", ["192.168.2.10", "companion.local", "pi", "fe80::1%en0", "::1"])
def test_a_name_or_address_is_taken(host):
    assert net_probe.clean_host(f"  {host} ") == (host, "")


@pytest.mark.parametrize("host", ["", "   ", None, "-c 9 x", "-f", "a b", "a;reboot", "$(id)",
                                  "x" * 254, "host.", "10.0.0.1\nx"])
def test_anything_that_could_be_an_option_or_a_command_is_refused(host):
    cleaned, problem = net_probe.clean_host(host)
    assert cleaned == "" and problem


def test_one_echo_per_platform_with_the_host_last():
    assert net_probe.ping_command("ping", "10.0.0.7", 2, "linux") == \
        ["ping", "-c", "1", "-n", "-W", "2", "10.0.0.7"]
    assert net_probe.ping_command("ping", "10.0.0.7", 1.5, "darwin") == \
        ["ping", "-c", "1", "-n", "-t", "2", "10.0.0.7"]
    assert net_probe.ping_command("ping", "10.0.0.7", 1.5, "win32") == \
        ["ping", "-n", "1", "-w", "1500", "10.0.0.7"]
    assert net_probe.ping_command("ping", "::1", 2, "darwin")[-1] == "::1"


LINUX_REPLY = """PING 10.0.0.7 (10.0.0.7) 56(84) bytes of data.
64 bytes from 10.0.0.7: icmp_seq=1 ttl=64 time=0.412 ms

--- 10.0.0.7 ping statistics ---
1 packets transmitted, 1 received, 0% packet loss, time 0ms
rtt min/avg/max/mdev = 0.412/0.412/0.412/0.000 ms
"""
WINDOWS_REPLY = "Reply from 10.0.0.7: bytes=32 time<1ms TTL=64\r\n"
WINDOWS_GERMAN = "Antwort von 10.0.0.7: Bytes=32 Zeit=12ms TTL=64\r\n"
WINDOWS_UNREACHABLE = "Reply from 10.0.0.1: Destination host unreachable.\r\n"
WINDOWS_IPV6 = "Reply from ::1: time<1ms\r\n"


def test_a_reply_and_its_round_trip_are_read():
    assert net_probe.parse_reply(0, LINUX_REPLY, "10.0.0.7", "linux") == (True, 0.412)
    assert net_probe.parse_reply(0, WINDOWS_REPLY, "10.0.0.7", "win32") == (True, 1.0)
    assert net_probe.parse_reply(0, WINDOWS_GERMAN, "10.0.0.7", "win32") == (True, 12.0)
    assert net_probe.parse_reply(0, WINDOWS_IPV6, "::1", "win32") == (True, 1.0)


def test_no_reply_is_no_reply():
    assert net_probe.parse_reply(1, "", "10.0.0.7", "linux") == (False, None)
    assert net_probe.parse_reply(2, LINUX_REPLY, "10.0.0.7", "darwin") == (False, None)


def test_a_router_saying_unreachable_is_not_the_companion_answering():
    """Windows exits 0 for this one."""
    assert net_probe.parse_reply(0, WINDOWS_UNREACHABLE, "10.0.0.7", "win32")[0] is False


def test_without_a_ping_program_it_says_so():
    result = net_probe.Pinger(ping="").ping("10.0.0.7")
    assert result == {"ok": False, "error": "There is no ping program on this computer."}


def test_a_bad_host_never_starts_a_process(monkeypatch):
    started: list[Any] = []
    monkeypatch.setattr(net_probe.subprocess, "Popen", lambda *a, **k: started.append(a))
    result = net_probe.Pinger(ping="ping").ping("-f 10.0.0.7")
    assert result["ok"] is False
    assert started == []


@pytest.mark.skipif(not shutil.which("ping"), reason="no ping on this machine")
def test_this_computer_answers_its_own_ping():
    result = net_probe.Pinger().ping("127.0.0.1", 2.0)
    if not result.get("ok") or not result.get("reachable"):
        pytest.skip(f"ICMP not permitted here: {result}")
    assert result["host"] == "127.0.0.1"


@pytest.mark.skipif(sys.platform == "win32", reason="the stand-in ping is a shell script")
def test_shutdown_ends_a_probe_in_flight_and_refuses_new_ones(tmp_path):
    slow = tmp_path / "ping"
    slow.write_text("#!/bin/sh\nexec sleep 30\n")
    slow.chmod(0o755)
    pinger = net_probe.Pinger(ping=str(slow))
    answer: list[dict[str, Any]] = []
    worker = threading.Thread(target=lambda: answer.append(pinger.ping("10.0.0.7", 10.0)))
    worker.start()
    deadline = time.monotonic() + 5
    while not pinger._procs and time.monotonic() < deadline:
        time.sleep(0.02)
    assert pinger._procs, "the probe started"
    began = time.monotonic()
    pinger.shutdown()
    worker.join(5)
    assert not worker.is_alive()
    assert time.monotonic() - began < 5, "ended, not waited out"
    assert answer and answer[0]["reachable"] is False
    assert pinger.ping("10.0.0.7") == {"ok": False, "error": "Corvus is shutting down."}


def test_a_probe_that_hangs_is_bounded_by_its_deadline(tmp_path, monkeypatch):
    if sys.platform == "win32":
        pytest.skip("the stand-in ping is a shell script")
    slow = tmp_path / "ping"
    slow.write_text("#!/bin/sh\nexec sleep 30\n")
    slow.chmod(0o755)
    monkeypatch.setattr(net_probe, "GRACE_S", 0.2)
    began = time.monotonic()
    result = net_probe.Pinger(ping=str(slow)).ping("10.0.0.7", 0.5)
    assert time.monotonic() - began < 5
    assert result == {"ok": True, "host": "10.0.0.7", "reachable": False, "rtt_ms": None}


# ---- POST /api/local/ping ---------------------------------------------------


class _PingerSpy:
    def __init__(self) -> None:
        self.calls: list[tuple[str, float]] = []

    def ping(self, host: str, wait_s: float) -> dict[str, Any]:
        self.calls.append((host, wait_s))
        return {"ok": True, "host": host, "reachable": True, "rtt_ms": 0.5}


def _handler(client: str = "127.0.0.1", **attrs: Any):
    handler = object.__new__(CorvusHandler)
    handler.client_address = (client, 50123)
    handler.pinger = None
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    for key, value in attrs.items():
        setattr(handler, key, value)
    return handler, responses


def test_the_endpoint_pings_with_the_wait_in_seconds():
    spy = _PingerSpy()
    handler, responses = _handler(pinger=spy)
    handler._api_local_ping({"host": "10.0.0.7", "wait_ms": 3000})
    assert spy.calls == [("10.0.0.7", 3.0)]
    assert responses == [({"ok": True, "host": "10.0.0.7", "reachable": True, "rtt_ms": 0.5}, 200)]


@pytest.mark.parametrize("client", ["10.0.0.5", "fe80::1", ""])
def test_only_this_computer_may_ping_through_corvus(client):
    spy = _PingerSpy()
    handler, responses = _handler(client, pinger=spy)
    handler._api_local_ping({"host": "10.0.0.7"})
    assert responses[0][1] == 403
    assert spy.calls == []


@pytest.mark.parametrize("payload", [{}, {"host": 5}, {"host": "x", "wait_ms": "2"},
                                     {"host": "x", "wait_ms": True}, {"host": "-f x"},
                                     {"host": "x", "wait_ms": float("nan")}])
def test_the_endpoint_refuses_a_bad_payload(payload):
    spy = _PingerSpy()
    handler, responses = _handler(pinger=spy)
    handler._api_local_ping(payload)
    assert responses[0][1] == 400
    assert spy.calls == []


def test_without_a_pinger_the_endpoint_is_unavailable():
    handler, responses = _handler()
    handler._api_local_ping({"host": "10.0.0.7"})
    assert responses[0][1] == 503
