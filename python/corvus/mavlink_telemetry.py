"""Telemetry the bridge turns into state: battery, setpoints, RC, link quality.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.

The handlers are ``corvus_link.telemetry.TelemetryEngine``, in Rust. It reads
the battery estimator (``battery_math``), the channel count (``rc_config``),
the message ids (``mavutil``) and its clock (``time``) from here; this mixin
holds the operation lock around the stream requests.
"""
from __future__ import annotations

import sys
import time  # noqa: F401 - the engine's clock
from typing import Any

from corvus_link import mavutil
from corvus_link import telemetry as _engine

from . import battery as battery_math  # noqa: F401 - the engine's estimator
from . import rc_config  # noqa: F401 - the engine reads MAX_CHANNELS

# GNSS_INTEGRITY (id 441) is in MAVLink's development.xml. A link still on the
# MAVLink 1 view hands it over as an UNKNOWN_441 frame, decoded and CRC checked
# here: an unchecked frame is a guess about jamming. None for anything that is
# not an intact frame of that message.
_decode_gnss_integrity = _engine.decode_gnss_integrity
# RC_CHANNELS.rssi as 0 to 100, or -1 when the receiver does not report it.
_rc_rssi_percent = _engine.rc_rssi_percent
# A MAVLink attitude quaternion as roll, pitch and yaw in degrees.
_quaternion_to_euler_deg = _engine.quaternion_to_euler_deg


def _engine_attribute(name: str, writable: bool = True) -> property:
    if not writable:
        return property(lambda self: getattr(self._telemetry_engine, name))
    return property(
        lambda self: getattr(self._telemetry_engine, name),
        lambda self, value: setattr(self._telemetry_engine, name, value))


class TelemetryMixin:
    """Handlers for the messages the receive loop dispatches, and on-demand rates."""

    # Kept addressable by the names the bridge used: the tests look at them.
    _hb_times = _engine_attribute("hb_times", writable=False)
    _radio_status_seen = _engine_attribute("radio_status_seen")
    _uplink_score = _engine_attribute("uplink_score")
    _battery_cells = _engine_attribute("battery_cells")
    _drain_samples = _engine_attribute("drain_samples", writable=False)
    _drain_source = _engine_attribute("drain_source", writable=False)

    def _init_telemetry_state(self) -> None:
        """Set up link-quality and battery state; called from the bridge's __init__."""
        self._telemetry_engine = _engine.TelemetryEngine(sys.modules[__name__])

    def _note_heartbeat(self, stamp: float) -> None:
        """A heartbeat landed at *stamp*: the jitter window's newest entry."""
        self._telemetry_engine.note_heartbeat(stamp)

    def _forget_link_quality(self) -> None:
        """A fresh jitter window and radio view for a new link."""
        self._telemetry_engine.forget_link()

    # ------------------------------------------------------------------
    # Battery
    # ------------------------------------------------------------------

    def set_battery_settings(self, raw: Any) -> dict[str, Any]:
        """Install the operator's battery estimator settings; return them resolved.

        Called at startup and again whenever the Battery & Power page saves.
        The latched cell count is dropped with the old settings: a 6S typed in
        place of an auto-detected 5S must take effect on the next frame, not on
        the next connection.
        """
        return self._telemetry_engine.set_battery_settings(raw)

    def battery_settings(self) -> dict[str, Any]:
        """The estimator settings currently in force, resolved against defaults."""
        return self._telemetry_engine.battery_settings()

    def _forget_battery_cells(self) -> None:
        """Drop the latched cell count (a new link may be a new pack)."""
        self._telemetry_engine.forget_battery_cells()

    def _battery_fields(self, voltage: float, current: float,
                        reported: int) -> dict[str, Any]:
        """The battery half of a SYS_STATUS update, both answers included.

        *reported* is the autopilot's own remaining percentage, or -1 when it
        publishes none. The cell count is latched on the first reading of a
        connection: detection from the voltage is only unambiguous on a pack
        just plugged in (21.0 V is a fresh 5S and a tired 6S), and a count that
        fell by one in flight would move the percentage by thirty points.
        """
        return self._telemetry_engine.battery_fields(voltage, current, reported)

    def _battery_endurance_fields(self, percent: float | None, source: str,
                                  armed: bool, now: float | None = None) -> dict[str, Any]:
        """Corvus's own time-to-empty, as the ``battery_endurance_est`` field.

        Only a fallback for the autopilot's BATTERY_STATUS.time_remaining: the
        drain rate of the percentage being flown by, kept only while armed and
        restarted when that percentage's source changes.
        """
        return self._telemetry_engine.battery_endurance_fields(percent, source, armed, now)

    def _handle_battery_status(self, msg: Any) -> None:
        """Publish the detail SYS_STATUS has no room for: cell voltages,
        consumed mAh, temperature and time remaining, first battery only, each
        only when the message carries a real value."""
        self._telemetry_engine.handle_battery_status(self, msg)

    # ------------------------------------------------------------------
    # GNSS integrity (jamming, spoofing)
    # ------------------------------------------------------------------

    def _handle_gnss_integrity(self, msg: Any) -> None:
        """Publish the receiver's own jamming and spoofing verdicts, first
        receiver of this aircraft only."""
        self._telemetry_engine.handle_gnss_integrity(self, msg)

    def _forget_gnss_integrity(self) -> None:
        """Clear the integrity verdicts (a new link may be a new receiver)."""
        self._telemetry_engine.forget_gnss_integrity(self)

    # ------------------------------------------------------------------
    # Controller setpoints (PID tuning)
    # ------------------------------------------------------------------

    def _handle_attitude_target(self, msg: Any) -> None:
        """Publish the attitude and body rate setpoints the controller follows,
        the commanded half of what the PID tuning page plots."""
        self._telemetry_engine.handle_attitude_target(self, msg)

    def _handle_position_target(self, msg: Any) -> None:
        """Publish the velocity setpoint the position controller follows."""
        self._telemetry_engine.handle_position_target(self, msg)

    # ------------------------------------------------------------------
    # Transmitter channels (Setup -> Radio Control)
    # ------------------------------------------------------------------

    def _handle_rc_channels(self, msg: Any, raw: bool = False) -> None:
        """Publish the raw transmitter channel values, in microseconds.

        ``RC_CHANNELS`` carries all 18; ``RC_CHANNELS_RAW`` (*raw*) eight at a
        time with ``port`` naming the bank, merged into the published list.
        ``UINT16_MAX`` ("not delivered") is published as 0.
        """
        self._telemetry_engine.handle_rc_channels(self, msg, raw)

    # ------------------------------------------------------------------
    # Link-quality tracking (A1)
    # ------------------------------------------------------------------

    def _handle_radio_status(self, msg: Any) -> None:
        """Parse RADIO_STATUS into the uplink score and radio metrics (A1)."""
        self._telemetry_engine.handle_radio_status(self, msg)

    def _publish_heartbeat_jitter(self) -> None:
        """Publish the rolling stddev of heartbeat inter-arrival in ms (A1)."""
        self._telemetry_engine.publish_heartbeat_jitter(self)

    def _current_jitter_ms(self) -> float:
        """Latest heartbeat jitter (0 before two heartbeats)."""
        return self._telemetry_engine.current_jitter_ms()

    def _publish_link_quality(self) -> None:
        """Derive the link_quality string from uplink + jitter (A1). 'lost' is
        owned by the drop path, not here."""
        self._telemetry_engine.publish_link_quality(self)

    def _stream_interval_us(self, msg_id: int, requested_us: int | None) -> int:
        """The interval a page's stream request should really send for *msg_id*.

        Never slower than the rate the session asked for at connect, and that
        rate back (or 0, the firmware's default) when the page is done.
        """
        return self._telemetry_engine.stream_interval_us(self, msg_id, requested_us)

    def set_vibration_stream(self, enabled: bool, rate_hz: int = 10) -> bool:
        """Enable/disable high-rate VIBRATION streaming from the vehicle.

        Requested only while the vibration plugin is open, then handed back to
        the firmware's default. Safe while armed (read-only telemetry).
        """
        with self._operation_lock:
            return self._telemetry_engine.vibration_stream(self, enabled, rate_hz)

    def set_rangefinder_stream(self, enabled: bool) -> bool:
        """Ask for (or hand back) the DISTANCE_SENSOR stream the top bar shows."""
        with self._operation_lock:
            return self._telemetry_engine.rangefinder_stream(self, enabled)

    def set_rc_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the RC_CHANNELS rate the Radio Control page needs.

        A stick swept in half a second is three samples at PX4's default 5 Hz.
        Safe while armed, and deliberately so: the page checks switches in
        flight.
        """
        with self._operation_lock:
            return self._telemetry_engine.rc_stream(self, enabled, rate_hz)

    # The messages the PID tuning page plots, as commanded/achieved pairs.
    # ATTITUDE is raised too: a 50 Hz response against a 5 Hz setpoint makes a
    # clean tune look like a lagging one.
    _TUNING_MSG_IDS: tuple[int, ...] = (
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE,
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE_TARGET,
        mavutil.mavlink.MAVLINK_MSG_ID_POSITION_TARGET_LOCAL_NED,
    )

    def set_tuning_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the setpoint stream the PID tuning page plots.

        True only if every message was accepted; a refused one still leaves
        the others raised, since a missing trace is a degraded graph rather
        than a failure.
        """
        with self._operation_lock:
            return self._telemetry_engine.tuning_stream(self, enabled, rate_hz)
