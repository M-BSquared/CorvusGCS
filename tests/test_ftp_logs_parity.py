"""The Rust FTP and log engines against the Python protocols they replaced.

``tests/reference/`` keeps Corvus' MAVLink FTP client and log requests as they
were in Python. Each test runs the same transfers through a bridge on the
reference and a bridge on ``corvus_link.ftp.FtpEngine`` /
``corvus_link.logs.LogEngine``, and compares the bytes read, every request on
the link, the progress reports, the errors and the command errors.
"""
from __future__ import annotations

import threading
import time
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from tests.reference import mavlink_ftp_python as reference_ftp  # noqa: E402
from tests.reference import mavlink_logs_python as reference_logs  # noqa: E402
from tests.test_param_metadata import FtpVehicle  # noqa: E402


class ReferenceBridge(reference_ftp.FtpClientMixin, reference_logs.LogTransferMixin, MavlinkBridge):
    """A bridge whose FTP client and log requests are the Python reference."""


for _name in ("_ftp_listening", "_ftp_inbox", "_ftp_cancel"):
    setattr(ReferenceBridge, _name, None)


def make(kind: type, files: dict[str, bytes], armed: bool = False, **vehicle: Any) -> tuple[Any, FtpVehicle]:
    store = VehicleStateStore()
    store.heartbeat()
    if armed:
        store.update(armed=True)
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    link = FtpVehicle(bridge, files, **vehicle)
    bridge._conn = SimpleNamespace(mav=link, source_system=255, source_component=190)
    return bridge, link


def outcome(call, *args: Any, **kwargs: Any) -> tuple:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc), getattr(exc, "code", None))


def requests(link: FtpVehicle) -> list[tuple]:
    return [(r.opcode, r.seq, r.session, r.size, r.offset, r.data) for r in link.requests]


@pytest.fixture(autouse=True)
def quick(monkeypatch: pytest.MonkeyPatch) -> None:
    from corvus import mavlink_ftp as rust_side
    for module in (reference_ftp, rust_side):
        monkeypatch.setattr(module, "FTP_REPLY_TIMEOUT_S", 0.05)
        monkeypatch.setattr(module, "FTP_REPLY_TIMEOUT_S_SLOW", 0.05)


CONTENT = bytes(i % 251 for i in range(239 * 10 + 17))


@pytest.mark.parametrize("case", [
    {"files": {"/f": CONTENT}},
    {"files": {"/f": CONTENT}, "drop": {239 * 3, 239 * 7}},
    {"files": {"/f": CONTENT}, "drop": {0, 239, 239 * 9}, "report_size": False},
    {"files": {"/f": b"hello"}, "busy": True},
    {"files": {"/f": b""}},
    {"files": {}},
    {"files": {"/f": CONTENT}, "silent": True},
], ids=["plain", "lossy", "lossy-unsized", "busy", "empty", "missing", "silent"])
def test_reads_match(case: dict) -> None:
    case = dict(case)
    files = case.pop("files")
    silent = case.pop("silent", False)
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, link = make(kind, files, **case)
        if silent:
            link._answer = lambda request: []
        progress: list[tuple] = []
        result = outcome(bridge.ftp_read_file, "/f", progress=lambda a, b, seen=progress: seen.append((a, b)))
        results.append((result, requests(link), progress, bridge._ftp_lock.locked(), list(bridge._ftp_inbox)))
    assert results[1] == results[0]


def test_a_progress_hook_that_raises_does_not_end_the_transfer() -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, link = make(kind, {"/f": CONTENT})

        def broken(_a: int, _b: int) -> None:
            raise RuntimeError("hook")

        results.append((outcome(bridge.ftp_read_file, "/f", progress=broken), requests(link)))
    assert results[1] == results[0]


@pytest.mark.parametrize("files,crc", [({"/f": CONTENT}, True), ({}, True), ({"/f": CONTENT}, False)])
def test_checksums_match(files: dict, crc: bool) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, link = make(kind, files, crc=crc)
        results.append((outcome(bridge.ftp_file_crc32, "/f"), requests(link)))
    assert results[1] == results[0]


def test_long_paths_are_refused_alike() -> None:
    path = "/" + "x" * 300
    for method in ("ftp_read_file", "ftp_file_crc32"):
        results = []
        for kind in (ReferenceBridge, MavlinkBridge):
            bridge, link = make(kind, {})
            results.append((outcome(getattr(bridge, method), path), requests(link)))
        assert results[1] == results[0]


def test_aborting_a_waiting_transfer_matches() -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, link = make(kind, {})
        link._answer = lambda request: []
        out: list[tuple] = []
        worker = threading.Thread(
            target=lambda seen=out, read=bridge.ftp_read_file: seen.append(outcome(read, "/f")))
        worker.start()
        time.sleep(0.03)
        bridge.abort_ftp()
        worker.join(timeout=2)
        assert not worker.is_alive()
        results.append((out, bridge._ftp_lock.locked()))
    assert results[1] == results[0]
    assert results[0][0][0][2] == "cancelled"


def test_a_cancel_event_set_before_the_read_matches() -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, link = make(kind, {"/f": CONTENT})
        cancel = threading.Event()
        cancel.set()
        results.append((outcome(bridge.ftp_read_file, "/f", cancel=cancel), requests(link)))
    assert results[1] == results[0]


def test_replies_addressed_elsewhere_match() -> None:
    from corvus.mavlink_ftp import FTP_OP_ACK, FTP_OP_READ_FILE, encode_ftp_payload
    messages = [
        SimpleNamespace(target_system=254, target_component=190, payload=encode_ftp_payload(1, 0, FTP_OP_ACK)),
        SimpleNamespace(target_system=255, target_component=7, payload=encode_ftp_payload(1, 0, FTP_OP_ACK)),
        SimpleNamespace(target_system=0, target_component=0, payload=encode_ftp_payload(2, 0, FTP_OP_ACK)),
        SimpleNamespace(target_system=255, target_component=190, payload=encode_ftp_payload(3, 0, FTP_OP_READ_FILE)),
        SimpleNamespace(target_system=255, target_component=190, payload=b"short"),
        SimpleNamespace(target_system=255, target_component=190, payload=None),
        SimpleNamespace(target_system=255, target_component=190),
    ]
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, _link = make(kind, {})
        bridge._ftp_listening = True
        for msg in messages:
            bridge._handle_ftp_message(msg)
        results.append([(r.seq, r.opcode) for r in bridge._ftp_inbox])
    assert results[1] == results[0]


# ---------------------------------------------------------------- logs ----

class LogMav:
    def __init__(self, fail: bool = False) -> None:
        self.sent: list[tuple] = []
        self.fail = fail

    def __getattr__(self, name: str) -> Any:
        if not name.startswith("log_"):
            raise AttributeError(name)

        def send(*args: Any) -> None:
            if self.fail:
                raise OSError("radio gone")
            self.sent.append((name, *args))
        return send


@pytest.mark.parametrize("fail", [False, True])
@pytest.mark.parametrize("armed", [False, True])
@pytest.mark.parametrize("connected", [True, False])
def test_log_requests_match(fail: bool, armed: bool, connected: bool) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge, _ = make(kind, {}, armed=armed)
        mav = LogMav(fail)
        bridge._conn = SimpleNamespace(mav=mav, source_system=255, source_component=190)
        if not connected:
            bridge._store.update(connected=False)
            bridge._connection_ready = lambda: False
        calls = [
            bridge.request_log_list(),
            bridge.request_log_list(3, 9.7),
            bridge.request_log_data(2, 4096, 90),
            bridge.erase_logs(),
            bridge.log_request_end(),
        ]
        results.append((calls, mav.sent, bridge.get_last_command_error()))
    assert results[1] == results[0]
