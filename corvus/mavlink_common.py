"""Pieces every part of the MAVLink bridge shares.

Split out of :mod:`corvus.mavlink_bridge` so the modules the bridge is built
from (:mod:`corvus.mavlink_params`, :mod:`corvus.mavlink_missions` and the
rest) can use them without importing the bridge, which imports them.
"""
from __future__ import annotations

import threading
from dataclasses import dataclass, field

MAV_RESULT_TEXT: dict[int, str] = {
    -2: "DISCONNECTED", -1: "ACK_TIMEOUT",
    0: "ACCEPTED", 1: "TEMPORARILY_REJECTED", 2: "DENIED",
    3: "UNSUPPORTED", 4: "FAILED", 5: "IN_PROGRESS",
    6: "CANCELLED", 7: "COMMAND_LONG_ONLY", 8: "COMMAND_INT_ONLY",
    9: "UNSUPPORTED_FRAME", 10: "NOT_IN_CONTROL",
}

TAKEOFF_ALTITUDE_MIN_M = 1.0
# 120 m, not 50. The old ceiling was an arbitrary round number that refused
# perfectly ordinary survey and inspection heights; 120 m AGL is the actual
# operational ceiling most crews fly to — it is the EU open-category limit and
# matches the equivalent rule in the UK, and is the same number PX4's own
# defaults are written around. A GCS should stop at the number the rules stop
# at, not at one somebody picked because it sounded cautious.
#
# The takeoff slider in src/index.html carries the same maximum; a test pins
# the two together so they cannot drift.
TAKEOFF_ALTITUDE_MAX_M = 120.0


@dataclass
class _PendingAck:
    event: threading.Event = field(default_factory=threading.Event)
    result: int | None = None
    accept_in_progress: bool = False
    # Results that are not the last word: recorded, but the wait goes on for a
    # later ACK, and one of these is returned only if none arrives in time.
    interim: frozenset[int] = frozenset()
    interim_result: int | None = None
