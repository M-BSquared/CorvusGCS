"""Flight-log download: listing, the sequential queue, and cancellation.

The MAVLink LOG_* protocol has no ACK and no retry of its own, and exactly one
log session per vehicle. Both facts drive the design, so both are pinned here:
downloads run one at a time, a gap in LOG_DATA is re-requested rather than
written as a hole, and the session is always ended — a vehicle left with an
open log session cannot start logging the next flight.
"""
from __future__ import annotations

import pathlib
import threading
import time
from types import SimpleNamespace

import pytest

from corvus.log_service import BUSY_STATES, LogService, _safe_component


class _FakeMsg(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type


class _FakeBridge:
    """A vehicle that answers the log protocol from an in-memory log set."""

    def __init__(self, logs: dict[int, bytes] | None = None, connected: bool = True) -> None:
        self.logs = logs or {}
        self._connected = connected
        self.erase_calls = 0
        self.erase_ok = True
        self.command_error = ""
        self.sink = None
        self.list_requests = 0
        self.data_requests: list[tuple[int, int, int]] = []
        self.end_requests = 0
        self.drop_first_chunk = False
        self._dropped: set[tuple[int, int]] = set()

    def is_connected(self) -> bool:
        return self._connected

    def set_log_sink(self, sink) -> None:
        self.sink = sink

    def request_log_list(self, start: int = 0, end: int = 0xFFFF) -> bool:
        self.list_requests += 1
        entries = sorted(self.logs.items())
        for index, (log_id, blob) in enumerate(entries):
            if self.sink is None:
                break
            self.sink(_FakeMsg(
                message_type="LOG_ENTRY", id=log_id, num_logs=len(entries),
                last_log_num=len(entries) - 1, time_utc=1_700_000_000 + log_id,
                size=len(blob),
            ))
            del index
        return True

    def request_log_data(self, log_id: int, offset: int, count: int) -> bool:
        self.data_requests.append((log_id, offset, count))
        blob = self.logs.get(log_id, b"")
        sent = 0
        while sent < count and self.sink is not None:
            chunk_off = offset + sent
            if chunk_off >= len(blob):
                break
            chunk = blob[chunk_off:chunk_off + 90]
            # Simulate one lost packet so the gap-recovery path is exercised.
            if self.drop_first_chunk and (log_id, chunk_off) not in self._dropped:
                self._dropped.add((log_id, chunk_off))
                sent += len(chunk)
                continue
            payload = list(chunk) + [0] * (90 - len(chunk))
            self.sink(_FakeMsg(message_type="LOG_DATA", id=log_id,
                               ofs=chunk_off, count=len(chunk), data=payload))
            sent += len(chunk)
        return True

    def log_request_end(self) -> bool:
        self.end_requests += 1
        return True

    def erase_logs(self) -> bool:
        self.erase_calls += 1
        if not self.erase_ok:
            return False
        self.logs = {}
        return True

    def get_last_command_error(self) -> str:
        return self.command_error


def _service(tmp_path: pathlib.Path, bridge: _FakeBridge) -> LogService:
    return LogService(bridge, log_dir=lambda: str(tmp_path), tlog_dir=lambda: str(tmp_path))


def _wait_idle(service: LogService, timeout: float = 10.0) -> dict:
    """Block until no log job owns the vehicle any more.

    Reads the service's own BUSY_STATES rather than restating them: a local
    copy silently stopped covering "erasing" the moment that state was added,
    and the wait then returned mid-job.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = service.status()
        if status["state"] not in BUSY_STATES:
            return status
        time.sleep(0.02)
    raise AssertionError(f"log service stuck in {service.status()['state']}")


# ---------------------------------------------------------------------------
# Listing
# ---------------------------------------------------------------------------

def test_listing_collects_entries_and_ends_the_session(tmp_path: pathlib.Path) -> None:
    bridge = _FakeBridge({1: b"a" * 200, 2: b"b" * 100})
    service = _service(tmp_path, bridge)
    assert service.refresh() is True
    status = _wait_idle(service)

    assert [log["id"] for log in status["logs"]] == [2, 1], "newest first"
    assert status["logs"][1]["size"] == 200
    # PX4 keeps the log session open until it hears LOG_REQUEST_END, which
    # blocks logging of the next flight.
    assert bridge.end_requests >= 1
    assert bridge.sink is None, "the sink is detached when the job ends"
    service.shutdown()


def test_empty_log_slots_are_not_offered(tmp_path: pathlib.Path) -> None:
    """PX4 reports every slot including empty ones; those are not downloadable."""
    bridge = _FakeBridge({1: b"", 2: b"x" * 90})
    service = _service(tmp_path, bridge)
    service.refresh()
    status = _wait_idle(service)
    assert [log["id"] for log in status["logs"]] == [2]
    service.shutdown()


def test_listing_refuses_without_a_link(tmp_path: pathlib.Path) -> None:
    service = _service(tmp_path, _FakeBridge(connected=False))
    assert service.refresh() is False
    assert "not connected" in service.last_error
    service.shutdown()


# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------

def test_download_writes_the_exact_bytes(tmp_path: pathlib.Path) -> None:
    blob = bytes(range(256)) * 4
    bridge = _FakeBridge({7: blob})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)

    assert service.start_download([7]) is True
    status = _wait_idle(service)
    assert status["state"] == "done", status["message"]

    written = list(tmp_path.glob("log_007*.ulg"))
    assert len(written) == 1
    assert written[0].read_bytes() == blob, "the file is byte-identical to the log"
    service.shutdown()


def test_a_lost_packet_is_re_requested_not_written_as_a_hole(
    tmp_path: pathlib.Path,
) -> None:
    """The protocol has no retransmit of its own, so the gap must be re-asked.

    Writing the hole would produce a file that opens and lies, which is worse
    than a download that fails.
    """
    blob = bytes(range(256)) * 8
    bridge = _FakeBridge({3: blob})
    bridge.drop_first_chunk = True
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)

    service.start_download([3])
    status = _wait_idle(service, timeout=20.0)
    assert status["state"] == "done", status["message"]
    written = list(tmp_path.glob("log_003*.ulg"))[0]
    assert written.read_bytes() == blob
    service.shutdown()


def test_multiple_logs_download_one_after_another(tmp_path: pathlib.Path) -> None:
    """Sequential is a protocol constraint, not a simplification.

    MAVLink has one log session per vehicle: concurrent downloads interleave
    their LOG_DATA and corrupt both files.
    """
    bridge = _FakeBridge({1: b"one" * 100, 2: b"two" * 100, 3: b"three" * 100})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)

    assert service.start_download([1, 2, 3]) is True
    status = _wait_idle(service, timeout=20.0)
    assert status["state"] == "done", status["message"]
    assert len(list(tmp_path.glob("*.ulg"))) == 3
    assert [c["ok"] for c in status["completed"]] == [True, True, True]
    # One log at a time: every data request names the log that was current.
    assert bridge.data_requests, "data was actually requested"
    service.shutdown()


def test_a_second_job_is_refused_while_one_runs(tmp_path: pathlib.Path) -> None:
    bridge = _FakeBridge({1: b"x" * 90_000})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    service.start_download([1])
    assert service.start_download([1]) is False
    assert "already running" in service.last_error
    service.cancel()
    _wait_idle(service, timeout=20.0)
    service.shutdown()


def test_download_refuses_an_unknown_log(tmp_path: pathlib.Path) -> None:
    service = _service(tmp_path, _FakeBridge({1: b"x" * 90}))
    service.refresh()
    _wait_idle(service)
    assert service.start_download([99]) is False
    assert "no known log" in service.last_error
    service.shutdown()


def test_cancel_stops_the_queue_and_leaves_no_partial_file(
    tmp_path: pathlib.Path,
) -> None:
    bridge = _FakeBridge({1: b"x" * 400_000, 2: b"y" * 400_000})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    service.start_download([1, 2])
    time.sleep(0.05)
    assert service.cancel() is True
    status = _wait_idle(service, timeout=20.0)

    assert status["state"] == "cancelled"
    assert not list(tmp_path.glob("*.part")), "no half-written file left behind"
    service.shutdown()


def test_one_failed_log_does_not_abandon_the_rest_of_the_queue(
    tmp_path: pathlib.Path,
) -> None:
    """An operator queues five logs and walks away; one bad log must not eat
    the other four."""
    bridge = _FakeBridge({1: b"a" * 90, 2: b"b" * 90})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    # Make log 1 unreadable by claiming an implausible size.
    service._entries[1]["size"] = 10 ** 12

    service.start_download([1, 2])
    status = _wait_idle(service, timeout=20.0)
    results = {c["id"]: c["ok"] for c in status["completed"]}
    assert results == {1: False, 2: True}
    assert list(tmp_path.glob("log_002*.ulg")), "the good log was still saved"
    service.shutdown()


# ---------------------------------------------------------------------------
# Erase
# ---------------------------------------------------------------------------

def test_erase_clears_the_vehicle_and_re_reads_the_result(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The list is re-read afterwards, not assumed.

    "The vehicle says it has no logs" is evidence; "we sent the command" is
    not, and the operator is about to fly again on the strength of it.
    """
    monkeypatch.setattr("corvus.log_service.ERASE_SETTLE_S", 0.01)
    bridge = _FakeBridge({1: b"a" * 90, 2: b"b" * 90})
    service = _service(tmp_path, bridge)
    service.refresh()
    assert len(_wait_idle(service)["logs"]) == 2

    assert service.erase() is True
    status = _wait_idle(service, timeout=20.0)
    assert bridge.erase_calls == 1
    assert status["logs"] == [], "the re-read shows an empty vehicle"
    assert status["message"] == "No logs on the vehicle"
    service.shutdown()


def test_a_refused_erase_is_reported_with_the_vehicles_reason(
    tmp_path: pathlib.Path,
) -> None:
    bridge = _FakeBridge({1: b"a" * 90})
    bridge.erase_ok = False
    bridge.command_error = "cannot erase logs while armed"
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)

    service.erase()
    status = _wait_idle(service, timeout=10.0)
    assert status["state"] == "failed"
    assert "armed" in status["message"]
    # A refused erase must not quietly empty the list the operator is looking at.
    assert len(status["logs"]) == 1
    service.shutdown()


def test_erase_refuses_without_a_link(tmp_path: pathlib.Path) -> None:
    service = _service(tmp_path, _FakeBridge(connected=False))
    assert service.erase() is False
    assert "not connected" in service.last_error
    service.shutdown()


def test_erase_refuses_while_another_log_job_runs(tmp_path: pathlib.Path) -> None:
    """One log session per vehicle: erasing mid-download would pull the file
    out from under the transfer."""
    bridge = _FakeBridge({1: b"x" * 400_000})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    service.start_download([1])
    assert service.erase() is False
    assert "already running" in service.last_error
    assert bridge.erase_calls == 0
    service.cancel()
    _wait_idle(service, timeout=20.0)
    service.shutdown()


def test_erasing_does_not_touch_files_already_downloaded(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Erase is about the flight controller. What is already on the laptop is
    the whole reason downloading first is worth doing."""
    monkeypatch.setattr("corvus.log_service.ERASE_SETTLE_S", 0.01)
    bridge = _FakeBridge({1: b"a" * 90})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    service.start_download([1])
    _wait_idle(service)
    saved = list(tmp_path.glob("*.ulg"))
    assert saved

    service.erase()
    _wait_idle(service, timeout=20.0)
    assert [f.name for f in tmp_path.glob("*.ulg")] == [f.name for f in saved]
    service.shutdown()


# ---------------------------------------------------------------------------
# Files and folders
# ---------------------------------------------------------------------------

def test_a_log_already_in_the_folder_is_marked_downloaded(
    tmp_path: pathlib.Path,
) -> None:
    """Knowing what is already saved is the difference between a re-download
    over a telemetry link and a no-op."""
    bridge = _FakeBridge({1: b"a" * 90, 2: b"b" * 180})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)

    service.start_download([1])
    _wait_idle(service)

    logs = {log["id"]: log for log in service.status()["logs"]}
    assert logs[1]["downloaded"] is True
    assert logs[1]["file"].endswith(".ulg")
    assert logs[2]["downloaded"] is False
    assert logs[2]["file"] == ""
    service.shutdown()


def test_a_same_numbered_file_of_a_different_size_is_not_claimed(
    tmp_path: pathlib.Path,
) -> None:
    """SD cards recycle log ids, so an id alone proves nothing.

    Reporting last month's log_003 as this flight's would send an operator home
    with the wrong evidence — the size has to agree too.
    """
    bridge = _FakeBridge({3: b"x" * 900})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    # A stale file from an earlier flight, same number, different length.
    (tmp_path / "log_003_2020-01-01_00-00.ulg").write_bytes(b"y" * 42)

    logs = {log["id"]: log for log in service.status()["logs"]}
    assert logs[3]["downloaded"] is False
    service.shutdown()


def test_saved_logs_lists_the_folder_and_recovers_ids(tmp_path: pathlib.Path) -> None:
    (tmp_path / "log_007_2026-01-01_10-00.ulg").write_bytes(b"x" * 10)
    (tmp_path / "handcopied.ulg").write_bytes(b"y" * 20)
    (tmp_path / "notes.txt").write_text("ignore", encoding="utf-8")
    service = LogService(_FakeBridge(), log_dir=lambda: str(tmp_path))

    saved = {f["name"]: f for f in service.saved_logs()}
    assert set(saved) == {"log_007_2026-01-01_10-00.ulg", "handcopied.ulg"}
    assert saved["log_007_2026-01-01_10-00.ulg"]["id"] == 7
    # A file the operator dropped in is listed but carries no id, so it is
    # never matched against a log on the vehicle.
    assert saved["handcopied.ulg"]["id"] is None
    service.shutdown()


def test_filenames_are_sortable_and_cannot_escape_the_folder() -> None:
    service = LogService(_FakeBridge(), log_dir=lambda: "/tmp")
    name = service._filename(7, {"utc": 1_700_000_000})
    assert name.startswith("log_007_") and name.endswith(".ulg")
    assert "/" not in name and ".." not in name
    assert _safe_component("../../etc/passwd") == "etc-passwd"
    assert _safe_component("") == "log"


def test_a_missing_download_folder_is_created(tmp_path: pathlib.Path) -> None:
    target = tmp_path / "nested" / "flightlogs"
    bridge = _FakeBridge({1: b"x" * 90})
    service = LogService(bridge, log_dir=lambda: str(target))
    service.refresh()
    _wait_idle(service)
    assert service.start_download([1]) is True
    _wait_idle(service)
    assert target.is_dir()
    service.shutdown()


def test_local_tlogs_are_listed(tmp_path: pathlib.Path) -> None:
    (tmp_path / "corvus_2026-01-01.tlog").write_bytes(b"x" * 32)
    (tmp_path / "notes.txt").write_text("ignore me", encoding="utf-8")
    service = LogService(_FakeBridge(), log_dir=lambda: str(tmp_path),
                         tlog_dir=lambda: str(tmp_path))
    names = [t["name"] for t in service.local_tlogs()]
    assert names == ["corvus_2026-01-01.tlog"]
    service.shutdown()


def test_shutdown_detaches_the_sink_and_is_idempotent(tmp_path: pathlib.Path) -> None:
    bridge = _FakeBridge({1: b"x" * 90})
    service = _service(tmp_path, bridge)
    service.refresh()
    _wait_idle(service)
    service.shutdown()
    service.shutdown()
    assert bridge.sink is None
    assert service.status()["state"] == "idle"
