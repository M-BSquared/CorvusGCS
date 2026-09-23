"""Remote ID: the identity this station broadcasts for the aircraft.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import threading
import time
from typing import Any

from . import remote_id as remote_id_model


class RemoteIdMixin:
    """OPEN_DRONE_ID_* out at 1 Hz, OPEN_DRONE_ID_ARM_STATUS back in."""

    def _init_remote_id_state(self) -> None:
        """Set up the Remote ID state; called from the bridge's __init__."""
        # The Remote ID identity this station broadcasts for the aircraft, and
        # what has come back about it. The identity arrives from the config file
        # via set_remote_id; the rest is per-connection-cycle. `_remote_id_arm`
        # is None until the vehicle actually reports an ODID arm status — a
        # stack that never sends one and one that is happy are different facts.
        self._remote_id_lock = threading.Lock()
        self._remote_id: dict[str, Any] = remote_id_model.defaults()
        self._remote_id_sent_at: float = 0.0
        self._remote_id_error: str = ""
        self._remote_id_arm: dict[str, Any] | None = None

    # ------------------------------------------------------------------
    # Remote ID — the identity this station broadcasts for the aircraft
    # ------------------------------------------------------------------

    def set_remote_id(self, raw: Any) -> dict[str, Any]:
        """Install the Remote ID identity; return it resolved.

        Called at startup and again whenever the Remote ID page saves. Takes
        effect on the next 1 Hz cycle rather than on the next connection: an
        operator who has just corrected a mistyped serial number is standing
        beside an aircraft that is still broadcasting the old one.
        """
        resolved = remote_id_model.settings(raw)
        with self._remote_id_lock:
            self._remote_id = resolved
            # The old identity's send history says nothing about the new one,
            # and the vehicle has not judged it yet.
            self._remote_id_sent_at = 0.0
            self._remote_id_error = ""
            self._remote_id_arm = None
        return resolved

    def remote_id_settings(self) -> dict[str, Any]:
        """The identity currently in force, resolved against the defaults."""
        # Re-resolved rather than deep-copied: settings() rebuilds the nested
        # dicts from scratch, so the caller cannot reach into stored state.
        with self._remote_id_lock:
            return remote_id_model.settings(self._remote_id)

    def _forget_remote_id_session(self) -> None:
        """Drop everything Remote ID learned from the link that just ended."""
        with self._remote_id_lock:
            self._remote_id_sent_at = 0.0
            self._remote_id_error = ""
            self._remote_id_arm = None

    def remote_id_supported(self) -> bool:
        """Whether the live link can carry the Remote ID messages at all.

        ``OPEN_DRONE_ID_*`` are MAVLink 2 messages, and pymavlink only grows
        the send methods for them once the dialect has been upgraded — which
        happens when the first v2 frame arrives. So this is a fact about the
        connected vehicle, not about this build: a v1-only autopilot genuinely
        cannot be told an identity, and the page says so rather than showing a
        broadcast that never leaves.
        """
        conn = self._conn
        mav = getattr(conn, "mav", None)
        return bool(mav is not None and hasattr(mav, "open_drone_id_basic_id_send"))

    def _send_remote_id(self, conn: Any) -> None:
        """Send one round of the Remote ID identity. Never raises.

        Called from the GCS heartbeat loop with ``_send_lock`` already held. It
        swallows everything because of where it is called from: a throw here
        would land in the heartbeat loop's reconnect branch and turn a link that
        cannot carry ODID into a 10 Hz heartbeat spin. The reason is recorded
        for the page instead.
        """
        with self._remote_id_lock:
            identity = self._remote_id
            enabled = bool(identity.get("enabled"))
        if not enabled:
            return
        mav = getattr(conn, "mav", None)
        if mav is None or not hasattr(mav, "open_drone_id_basic_id_send"):
            with self._remote_id_lock:
                self._remote_id_error = (
                    "this link is MAVLink 1, and the Remote ID messages are MAVLink 2 only"
                )
            return
        try:
            for name, kwargs in remote_id_model.messages(
                identity,
                target_system=self._target_system,
                target_component=self._target_component,
            ):
                getattr(mav, name + "_send")(**kwargs)
        except Exception as exc:  # noqa: BLE001 - see the docstring
            with self._remote_id_lock:
                self._remote_id_error = str(exc) or exc.__class__.__name__
            return
        with self._remote_id_lock:
            self._remote_id_sent_at = time.monotonic()
            self._remote_id_error = ""

    def _handle_remote_id_arm_status(self, msg: Any) -> None:
        """Latch the vehicle's verdict on the identity it is being sent.

        ``OPEN_DRONE_ID_ARM_STATUS`` is the only feedback a ground station gets:
        the aircraft says whether its Remote ID system would let it arm, and
        gives the reason in plain text when it would not. Without it the page
        could only report what it *sent*, which is the half of the exchange
        that is never the problem.
        """
        status = remote_id_model.arm_status(
            getattr(msg, "status", None), getattr(msg, "error", b""))
        with self._remote_id_lock:
            self._remote_id_arm = status

    def remote_id_status(self) -> dict[str, Any]:
        """What the Remote ID broadcast is doing right now.

        ``broadcasting`` is deliberately "a send landed in the last few
        seconds" rather than "enabled is true": the switch is the operator's
        intention and this is what the aircraft is actually being told.
        """
        with self._remote_id_lock:
            enabled = bool(self._remote_id.get("enabled"))
            sent_at = self._remote_id_sent_at
            error = self._remote_id_error
            arm = dict(self._remote_id_arm) if self._remote_id_arm else None
        age = (time.monotonic() - sent_at) if sent_at else None
        return {
            "enabled": enabled,
            "supported": self.remote_id_supported(),
            "broadcasting": bool(enabled and age is not None and age < 5.0),
            "last_sent_age": round(age, 1) if age is not None else None,
            "error": error,
            "arm_status": arm,
        }
