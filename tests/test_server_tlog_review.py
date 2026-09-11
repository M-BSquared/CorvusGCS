"""GET /api/logs/tlog-review — one recorded tlog, read as plots.

The route is confined to the tlog folder exactly the way the ULog review is
confined to the download folder: the request names a *file*, never a path, and
the resolved target is checked with realpath before anything is opened. These
tests are mostly about that confinement, because it is the property that would
turn a log viewer into an arbitrary-file reader if it ever slipped.
"""
from __future__ import annotations

import json
import pathlib
from typing import Any

import pytest

from corvus import tlog_review
from corvus.server import CorvusHandler

mavutil = pytest.importorskip("pymavlink.mavutil")

_MAGIC = b"#CORVUS-TLOG "


class _Logs:
    """Just enough LogService for the route: where the recordings live."""

    def __init__(self, directory: str) -> None:
        self._directory = directory

    def resolve_tlog_dir(self) -> str:
        return self._directory


def _handler(path: str, logs: Any) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.path = path
    handler.logs = logs
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _tlog(seconds: int = 5) -> bytes:
    link = mavutil.mavlink.MAVLink(None, srcSystem=1, srcComponent=1)
    frames = [
        mavutil.mavlink.MAVLink_heartbeat_message(2, 12, 128, 4 << 16, 4, 3).pack(link),
    ]
    for step in range(seconds * 5):
        boot = 1000 + step * 200
        frames.append(mavutil.mavlink.MAVLink_global_position_int_message(
            boot, 481000000, 115000000, 100000, 100000, 500, 0, 0, 0).pack(link))
    header = _MAGIC + json.dumps({"product": "Corvus GCS"}).encode() + b"\n"
    return header + b"".join(frames)


@pytest.fixture(autouse=True)
def _clean_cache() -> Any:
    tlog_review.clear_cache()
    yield
    tlog_review.clear_cache()


@pytest.fixture
def folder(tmp_path: pathlib.Path) -> pathlib.Path:
    (tmp_path / "20260910-143205-000000.tlog").write_bytes(_tlog())
    return tmp_path


def test_a_recording_in_the_folder_is_reviewed(folder: pathlib.Path) -> None:
    handler, out = _handler(
        "/api/logs/tlog-review?file=20260910-143205-000000.tlog", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status == 200 and data["ok"] is True
    assert data["summary"]["name"] == "20260910-143205-000000.tlog"
    assert any(p["id"] == "altitude" for p in data["plots"])


@pytest.mark.parametrize("name", [
    "../../../etc/passwd",
    "../secrets.tlog",
    "/etc/passwd",
    "..%2F..%2Fetc%2Fpasswd",
])
def test_no_query_can_walk_out_of_the_tlog_folder(
    folder: pathlib.Path, tmp_path: pathlib.Path, name: str,
) -> None:
    """The basename is taken and resolved inside the folder, and the result is
    checked with realpath. A viewer that could be talked into reading any path
    on the machine is a different program."""
    (tmp_path.parent / "secrets.tlog").write_bytes(_tlog())
    handler, out = _handler(f"/api/logs/tlog-review?file={name}", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status in (400, 404)
    assert data["ok"] is False
    assert "summary" not in data


def test_only_tlogs_are_opened(folder: pathlib.Path) -> None:
    """The folder is the operator's; a config or a key sitting in it is not a
    recording and must not be parsed as one."""
    (folder / "config.json").write_text('{"token": "hunter2"}', encoding="utf-8")
    handler, out = _handler("/api/logs/tlog-review?file=config.json", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status == 404 and data["ok"] is False


def test_a_missing_recording_is_a_404_not_a_crash(folder: pathlib.Path) -> None:
    handler, out = _handler("/api/logs/tlog-review?file=gone.tlog", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    assert out[0][1] == 404


def test_the_file_parameter_is_required(folder: pathlib.Path) -> None:
    handler, out = _handler("/api/logs/tlog-review", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status == 400 and "file is required" in data["error"]


def test_an_unreadable_recording_reports_the_reason_rather_than_500ing(
    folder: pathlib.Path,
) -> None:
    """A corrupt file in the folder is an ordinary thing. It must come back as
    a sentence the operator can read, not as a stack trace."""
    (folder / "broken.tlog").write_bytes(b"\x00" * 200)
    handler, out = _handler("/api/logs/tlog-review?file=broken.tlog", _Logs(str(folder)))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status == 400 and data["ok"] is False
    assert "no MAVLink frames" in data["error"]


def test_no_log_service_is_a_503_not_an_exception() -> None:
    handler, out = _handler("/api/logs/tlog-review?file=x.tlog", None)
    handler._api_logs_tlog_review()
    assert out[0][1] == 503


def test_an_unconfigured_folder_refuses_rather_than_resolving_against_root() -> None:
    """With no tlog dir, realpath("") is the working directory. Reviewing
    anything relative to that would read files from wherever Corvus was
    started."""
    handler, out = _handler("/api/logs/tlog-review?file=x.tlog", _Logs(""))
    handler._api_logs_tlog_review()
    data, status = out[0]
    assert status == 400 and data["ok"] is False
    assert "no recording folder configured" in data["error"]
