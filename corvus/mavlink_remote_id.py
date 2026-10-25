"""Remote ID: the identity this station broadcasts for the aircraft.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.

The broadcast is ``corvus_link.remote_id.RemoteIdEngine``, in Rust; it reads
its clock from ``time`` here.
"""
from __future__ import annotations

import sys
import time  # noqa: F401 - the engine's clock
from typing import Any

from corvus_link import remote_id as _engine


class RemoteIdMixin:
    """OPEN_DRONE_ID_* out at 1 Hz, OPEN_DRONE_ID_ARM_STATUS back in."""

    # Kept addressable by the names the bridge used. `_remote_id_arm` is None
    # until the vehicle reports an ODID arm status: a stack that never sends
    # one and one that is happy are different facts.
    _remote_id = property(lambda self: self._remote_id_engine.identity)
    _remote_id_sent_at = property(
        lambda self: self._remote_id_engine.sent_at,
        lambda self, value: setattr(self._remote_id_engine, "sent_at", value))
    _remote_id_error = property(
        lambda self: self._remote_id_engine.error,
        lambda self, value: setattr(self._remote_id_engine, "error", value))
    _remote_id_arm = property(lambda self: self._remote_id_engine.arm)

    def _init_remote_id_state(self) -> None:
        """Set up the Remote ID state; called from the bridge's __init__."""
        self._remote_id_engine = _engine.RemoteIdEngine(sys.modules[__name__])

    def set_remote_id(self, raw: Any) -> dict[str, Any]:
        """Install the Remote ID identity; return it resolved.

        Called at startup and again whenever the Remote ID page saves. Takes
        effect on the next 1 Hz cycle rather than on the next connection: an
        operator who has just corrected a mistyped serial number is standing
        beside an aircraft that is still broadcasting the old one.
        """
        return self._remote_id_engine.set(raw)

    def remote_id_settings(self) -> dict[str, Any]:
        """The identity currently in force, resolved against the defaults."""
        return self._remote_id_engine.settings()

    def _forget_remote_id_session(self) -> None:
        """Drop everything Remote ID learned from the link that just ended."""
        self._remote_id_engine.forget()

    def remote_id_supported(self) -> bool:
        """Whether the live link can carry the Remote ID messages at all.

        ``OPEN_DRONE_ID_*`` are MAVLink 2 messages, and corvus_link only grows
        their send methods once the first v2 frame has arrived. So this is a
        fact about the connected vehicle: a v1 only autopilot cannot be told an
        identity, and the page says so.
        """
        return self._remote_id_engine.supported(self)

    def _send_remote_id(self, conn: Any) -> None:
        """Send one round of the Remote ID identity. Never raises.

        Called from the GCS heartbeat loop with ``_send_lock`` already held. A
        throw would land in that loop's reconnect branch and turn the 1 Hz
        heartbeat into a 10 Hz spin, so the reason is recorded for the page
        instead.
        """
        self._remote_id_engine.send(self, conn)

    def _handle_remote_id_arm_status(self, msg: Any) -> None:
        """Latch the vehicle's verdict on the identity it is being sent."""
        self._remote_id_engine.handle_arm_status(msg)

    def remote_id_status(self) -> dict[str, Any]:
        """What the Remote ID broadcast is doing right now.

        ``broadcasting`` means a send landed in the last 5 s, not that the
        switch is on: the switch is the operator's intention, this is what the
        aircraft is actually being told.
        """
        return self._remote_id_engine.status(self)
