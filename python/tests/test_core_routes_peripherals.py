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
