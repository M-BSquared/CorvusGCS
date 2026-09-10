"""Tests for GET/POST /api/forwarding — the LINK tab's "share with another
station" switch.

Two ports run through this endpoint and confusing them breaks the feature
silently, so most of what is pinned here is that they stay apart: ``port`` is
where the OTHER station listens (14550, the UDP link QGroundControl opens by
itself) and ``listen_port`` is Corvus' own socket. See the module docstring in
``corvus/mavlink_forwarder.py`` for why they cannot be the same one.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus.config import _coerce_forwarding
from corvus.mavlink_forwarder import (
    DEFAULT_LISTEN_PORT,
    DEFAULT_PORT,
)
from corvus.server import CorvusHandler


class FakeConfig:
    def __init__(self, forwarding: dict[str, Any] | None = None) -> None:
        self.forwarding = forwarding


def handler(config: FakeConfig, forwarder: Any = None) -> tuple[CorvusHandler, list]:
    h = object.__new__(CorvusHandler)
    h.config = config          # type: ignore[assignment]
    h.forwarder = forwarder    # type: ignore[assignment]
    h.mavlink = None           # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    h._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return h, responses


# ---------------------------------------------------------------------------
# Reading the status back with nothing running
# ---------------------------------------------------------------------------

def test_the_status_names_both_ports_before_anything_is_running() -> None:
    """The LINK tab has to be able to say where the stream will go and where
    Corvus will answer without having started the forwarder first."""
    h, responses = handler(FakeConfig(None))

    h._api_forwarding_status()

    (payload, status), = responses
    assert status == 200
    assert payload["running"] is False
    assert payload["port"] == DEFAULT_PORT
    assert payload["listen_port"] == DEFAULT_LISTEN_PORT
    assert payload["port"] != payload["listen_port"], (
        "one port for both ends is two listeners and no talker"
    )


def test_a_saved_listen_port_of_zero_is_reported_as_zero() -> None:
    """0 means 'bind any free port' and is a real answer. An `or`-default
    turns it back into 14551, which is the one value the operator explicitly
    said they did not want."""
    h, responses = handler(FakeConfig({"listen_port": 0}))

    h._api_forwarding_status()

    assert responses[0][0]["listen_port"] == 0


# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("bad", [-1, 65536, "14551", True, 1.5])
def test_an_unusable_listen_port_is_refused(bad: Any) -> None:
    h, responses = handler(FakeConfig({}))

    h._api_forwarding_set({"listen_port": bad})

    (payload, status), = responses
    assert status == 400
    assert "listen_port" in payload["error"]


def test_zero_is_accepted_as_a_listen_port() -> None:
    """The one value that separates this range from the target port's."""
    config = FakeConfig({})
    h, _ = handler(config)

    h._api_forwarding_set({"listen_port": 0, "enabled": False})

    assert config.forwarding["listen_port"] == 0


@pytest.mark.parametrize("bad", [0, -1, 65536, "14550", True])
def test_an_unusable_target_port_is_refused(bad: Any) -> None:
    """0 is meaningless as a destination — there is nothing to send to."""
    h, responses = handler(FakeConfig({}))

    h._api_forwarding_set({"port": bad})

    assert responses[0][1] == 400


# ---------------------------------------------------------------------------
# What gets persisted
# ---------------------------------------------------------------------------

def test_both_hosts_survive_a_round_trip_through_the_config() -> None:
    config = FakeConfig({})
    h, _ = handler(config)

    h._api_forwarding_set({
        "enabled": False,
        "host": " 192.168.1.50 ",
        "listen_host": " 0.0.0.0 ",
        "port": 14550,
        "listen_port": 14551,
    })

    saved = config.forwarding
    assert saved["host"] == "192.168.1.50", "trimmed, not dropped"
    assert saved["listen_host"] == "0.0.0.0"
    assert _coerce_forwarding(saved) == saved, (
        "what the endpoint writes has to be what the loader reads back"
    )


def test_the_coercer_keeps_a_zero_listen_port_and_drops_a_bad_one() -> None:
    assert _coerce_forwarding({"listen_port": 0}) == {"listen_port": 0}
    assert _coerce_forwarding({"listen_port": -1}) is None
    assert _coerce_forwarding({"listen_port": True}) is None
    assert _coerce_forwarding({"listen_port": "14551"}) is None
