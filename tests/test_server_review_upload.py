"""POST /api/logs/review/upload — a ULog from outside the download folder.

The sibling GET route resolves a *name* inside the download folder and checks
the result with realpath, so no query can walk out of it. Opening a log that is
simply sitting somewhere on the operator's machine could have been done by
relaxing that into a path parameter, and deliberately was not: the bytes are
posted instead, so the backend never gains the ability to read an arbitrary
path off this machine on request.
"""
from __future__ import annotations

import struct
from typing import Any

import pytest

from corvus import flight_review
from corvus.server import MAX_ULOG_BODY_BYTES, CorvusHandler
from corvus.ulog import MAGIC


class _Headers:
    def __init__(self, length: Any) -> None:
        self._length = length

    def get(self, name: str, default: Any = None) -> Any:
        return self._length if name == "Content-Length" else default


class _Reader:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def read(self, n: int) -> bytes:
        return self._data[:n]


def _handler(body: bytes, *, path: str = "/api/logs/review/upload",
             length: Any = None) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.path = path
    handler.headers = _Headers(str(len(body)) if length is None else length)
    handler.rfile = _Reader(body)
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def _msg(msg_type: str, body: bytes) -> bytes:
    return struct.pack("<HB", len(body), ord(msg_type)) + body


def _ulog() -> bytes:
    """A short but reviewable flight."""
    return (
        MAGIC + b"\x01" + struct.pack("<Q", 0)
        + _msg("F", b"vehicle_local_position:uint64_t timestamp;float x;float y;")
        + _msg("A", bytes([0]) + struct.pack("<H", 1) + b"vehicle_local_position")
        + _msg("D", struct.pack("<H", 1) + struct.pack("<Qff", 0, 0.0, 0.0))
        + _msg("D", struct.pack("<H", 1) + struct.pack("<Qff", 1_000_000, 40.0, 25.0))
    )


@pytest.fixture(autouse=True)
def _clean_cache() -> Any:
    flight_review.clear_cache()
    yield
    flight_review.clear_cache()


def test_an_uploaded_log_is_reviewed_and_named_by_its_own_filename() -> None:
    handler, responses = _handler(
        _ulog(), path="/api/logs/review/upload?name=yesterday.ulg")
    handler.do_POST()
    payload, status = responses[0]
    assert status == 200 and payload["ok"] is True
    assert payload["summary"]["name"] == "yesterday.ulg"
    assert any(p["id"] == "track" for p in payload["plots"])


def test_the_name_is_a_label_and_never_opens_anything() -> None:
    """It is echoed straight back onto the page, so a name that tries to walk
    out of a directory must arrive as the harmless string it is."""
    handler, responses = _handler(
        _ulog(), path="/api/logs/review/upload?name=../../etc/passwd")
    handler.do_POST()
    payload, _ = responses[0]
    assert payload["summary"]["name"] == "passwd"


def test_an_unnamed_upload_still_gets_a_title() -> None:
    handler, responses = _handler(_ulog())
    handler.do_POST()
    payload, status = responses[0]
    assert status == 200
    assert payload["summary"]["name"] == "log.ulg"


def test_a_file_that_is_not_a_ulog_is_refused_with_the_reason() -> None:
    handler, responses = _handler(b"this is a text file, not a flight log")
    handler.do_POST()
    payload, status = responses[0]
    assert status == 400 and payload["ok"] is False
    assert "not a ULog" in payload["error"]


def test_the_body_is_not_json_parsed_on_the_way_in() -> None:
    """The dispatcher must hand this route the raw bytes: a ULog is binary and
    would 400 as malformed JSON before it was ever read."""
    handler, responses = _handler(_ulog())
    handler.do_POST()
    assert responses[0][1] == 200


def test_an_oversized_upload_is_refused_before_it_is_read() -> None:
    handler, responses = _handler(b"", length=str(MAX_ULOG_BODY_BYTES + 1))
    handler.do_POST()
    payload, status = responses[0]
    assert status == 413 and "too large" in payload["error"]


def test_a_non_numeric_content_length_is_a_400_not_a_dropped_connection() -> None:
    handler, responses = _handler(b"", length="not-a-number")
    handler.do_POST()
    assert responses[0][1] == 400


def test_an_empty_body_is_refused() -> None:
    handler, responses = _handler(b"")
    handler.do_POST()
    payload, status = responses[0]
    assert status == 400 and "empty" in payload["error"]


def test_a_truncated_upload_is_reported_rather_than_reviewed() -> None:
    """The client promised more bytes than it sent. Reviewing what arrived
    would silently present half a flight as the whole one."""
    blob = _ulog()
    handler, responses = _handler(blob[:20], length=str(len(blob)))
    handler.do_POST()
    payload, status = responses[0]
    assert status == 400 and "cut short" in payload["error"]


def test_the_same_bytes_are_only_parsed_once(monkeypatch: pytest.MonkeyPatch) -> None:
    """There is no path to stat, so the cache is keyed on the content itself."""
    reads: list[int] = []
    real = flight_review.read
    monkeypatch.setattr(flight_review, "read",
                        lambda *a, **k: (reads.append(1), real(*a, **k))[1])
    blob = _ulog()
    first = flight_review.review_bytes(blob, "a.ulg")
    second = flight_review.review_bytes(blob, "a.ulg")
    assert len(reads) == 1
    assert first is second
