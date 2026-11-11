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
