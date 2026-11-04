"""``?fresh=1`` on the setup pages that offer "Check values".

After a check the page redraws itself from what the vehicle holds now, so its
read has to skip the parameter cache: the cache also holds values another
station, or the vehicle itself, has since changed. Battery & Power did this
first; Safety & Sensors, Motors, PID Tuning, Radio Control and Remote ID
share the one helper.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.server import CorvusHandler  # noqa: E402


class FreshRecordingBridge:
    """Answers every name, and records whether each read asked for fresh."""

    stack = ""
    vehicle_type_id = 2

    def __init__(self) -> None:
        self.reads: list[dict[str, Any]] = []

    def fetch_params(self, names: list[str], timeout: float = 4.0,
                     fresh: bool = False) -> dict[str, float]:
        self.reads.append({"names": list(names), "fresh": fresh})
        return {n: 0.0 for n in names}

    def get_last_command_error(self) -> str:
        return ""


def _handler(path: str) -> tuple[CorvusHandler, FreshRecordingBridge, list]:
    handler = object.__new__(CorvusHandler)
    bridge = FreshRecordingBridge()
    handler.mavlink = bridge  # type: ignore[assignment]
    handler.store = None  # type: ignore[assignment]
    handler.config = None  # type: ignore[assignment]
    handler.path = path
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, bridge, responses


PAGES = [
    ("/api/safety", "_api_safety"),
    ("/api/motors", "_api_motors"),
    ("/api/tuning", "_api_tuning"),
    ("/api/rc", "_api_rc"),
    ("/api/remoteid", "_api_remote_id"),
]


@pytest.mark.parametrize("path, method", PAGES)
def test_a_fresh_read_skips_the_cache(path: str, method: str) -> None:
    handler, bridge, responses = _handler(path + "?fresh=1")
    getattr(handler, method)()
    assert responses and responses[0][1] == 200
    assert bridge.reads, "the page read its parameters"
    assert all(read["fresh"] for read in bridge.reads)


@pytest.mark.parametrize("path, method", PAGES)
def test_an_ordinary_read_may_use_the_cache(path: str, method: str) -> None:
    handler, bridge, _ = _handler(path)
    getattr(handler, method)()
    assert bridge.reads
    assert not any(read["fresh"] for read in bridge.reads)


def test_a_bridge_without_fresh_still_serves_an_ordinary_read() -> None:
    """Test doubles and plugins predate the flag; an ordinary read never passes it."""
    handler, bridge, responses = _handler("/api/tuning")

    def fetch_params(names: list[str], timeout: float = 4.0) -> dict[str, float]:
        return {}

    bridge.fetch_params = fetch_params  # type: ignore[method-assign]
    handler._api_tuning()
    assert responses[0][1] == 200
