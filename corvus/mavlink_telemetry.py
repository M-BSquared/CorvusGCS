"""Telemetry the bridge turns into state: battery, setpoints, RC, link quality.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import collections
import math
import threading
from typing import Any

from pymavlink import mavutil

from . import battery as battery_math
from . import rc_config

# SiK radio RSSI is a 0-255 relative scale; ~150 strong, ~40 weak. Used to
# map RADIO_STATUS.remrssi (the drone's signal as seen by the radio) to 0-100.
_SIK_RSSI_STRONG = 150.0
_SIK_RSSI_WEAK = 40.0


def _rc_rssi_percent(value: Any) -> int:
    """RC_CHANNELS.rssi -> 0-100, or -1 when the receiver does not report it.

    MAVLink reserves 255 for "unknown". Below that the field is ambiguous in
    practice: PX4 publishes a percentage, while several receivers publish the
    raw 0-254 scale the message was originally specified with. A value above
    100 can only be the second, so it is scaled; anything at or below 100 is
    already the percentage it claims to be. Guessing either way beats showing a
    254 % link.
    """
    try:
        rssi = int(value)
    except (TypeError, ValueError):
        return -1
    if rssi < 0 or rssi >= 255:
        return -1
    if rssi <= 100:
        return rssi
    return max(0, min(100, round(rssi * 100 / 254)))


def _quaternion_to_euler_deg(w: float, x: float, y: float, z: float) -> tuple[float, float, float]:
    """Convert a MAVLink attitude quaternion to roll/pitch/yaw in degrees.

    Same convention as the ATTITUDE message this is plotted against (NED body
    frame, roll about the nose axis) so a setpoint trace and a response trace
    share one scale. Pitch is clamped before ``asin`` because a quaternion that
    arrives fractionally un-normalised over a lossy link would otherwise raise
    on a value a hair outside [-1, 1].
    """
    sinr_cosp = 2.0 * (w * x + y * z)
    cosr_cosp = 1.0 - 2.0 * (x * x + y * y)
    roll = math.atan2(sinr_cosp, cosr_cosp)
    sinp = max(-1.0, min(1.0, 2.0 * (w * y - z * x)))
    pitch = math.asin(sinp)
    siny_cosp = 2.0 * (w * z + x * y)
    cosy_cosp = 1.0 - 2.0 * (y * y + z * z)
    yaw = math.atan2(siny_cosp, cosy_cosp)
    return math.degrees(roll), math.degrees(pitch), math.degrees(yaw)



class TelemetryMixin:
    """Handlers for the messages the receive loop dispatches, and on-demand rates."""

    def _init_telemetry_state(self) -> None:
        """Set up link-quality and battery state; called from the bridge's __init__."""
        # Heartbeat inter-arrival timestamps for jitter (A1); rolling window.
        self._hb_times: collections.deque[float] = collections.deque(maxlen=30)
        # Per-connection-cycle radio presence (A3).
        self._radio_status_seen: bool = False
        # Last computed uplink score; kept so heartbeat-only links can leave
        # it at 0 and link_quality can fall back to heartbeat freshness.
        self._uplink_score: int = 0
        # The operator's battery estimator settings, and the cell count they
        # imply for the pack currently plugged in. The settings come from the
        # config file via set_battery_settings; the count is latched per
        # connection cycle (see _battery_fields).
        self._battery_lock = threading.Lock()
        self._battery_settings: dict[str, Any] = battery_math.settings(None)
        self._battery_cells: int = 0

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
        resolved = battery_math.settings(raw)
        with self._battery_lock:
            self._battery_settings = resolved
            self._battery_cells = 0
        return resolved

    def battery_settings(self) -> dict[str, Any]:
        """The estimator settings currently in force, resolved against defaults."""
        with self._battery_lock:
            return dict(self._battery_settings)

    def _forget_battery_cells(self) -> None:
        """Drop the latched cell count (a new link may be a new pack)."""
        with self._battery_lock:
            self._battery_cells = 0

    def _battery_fields(self, voltage: float, current: float,
                        reported: int) -> dict[str, Any]:
        """The battery half of a SYS_STATUS update, both answers included.

        *reported* is the autopilot's own remaining percentage, or -1 when it
        publishes none. The voltage estimate is computed alongside it on every
        frame whether or not the operator has switched to it, because the page
        shows both side by side and an operator deciding which to trust needs
        to see them disagree.

        The cell count is latched. It is detected from the pack voltage, and
        that detection is only unambiguous on a pack that has just been plugged
        in — 21.0 V is a fresh 5S and a tired 6S. Recomputing per frame would
        let the count fall by one somewhere over the field, which moves the
        percentage by thirty points in a single frame and does it while the
        aircraft is flying. So the first reading of a connection decides, and
        the count then only changes when the operator changes it.
        """
        with self._battery_lock:
            resolved = dict(self._battery_settings)
            latched = self._battery_cells
        configured = int(resolved.get("cells") or 0)
        if latched and not configured:
            resolved["cells"] = latched

        est = battery_math.estimate(voltage, current, resolved)
        cells = int(est["cells"])
        if cells and not latched and not configured:
            with self._battery_lock:
                # Only if nothing was latched meanwhile: two frames can race
                # here, and the first one to answer is the one closest to plug-in.
                if not self._battery_cells:
                    self._battery_cells = cells

        estimated = float(est["percent"])
        use_estimate = bool(resolved.get("estimate")) and estimated >= 0
        if use_estimate:
            shown: float = round(estimated)
        else:
            shown = reported if reported >= 0 else 0

        return {
            "battery_voltage": round(voltage, 1),
            "battery_current": round(current, 1),
            "battery_percent": shown,
            "battery_percent_fc": reported,
            "battery_percent_est": estimated,
            "battery_source": "estimate" if use_estimate else "autopilot",
            "battery_cells": cells,
            "battery_cell_voltage": float(est["cell_voltage"]),
        }

    def _handle_battery_status(self, msg: Any) -> None:
        """Publish the detail SYS_STATUS has no room for.

        BATTERY_STATUS is where a pack that actually knows something about
        itself says so: per-cell voltages, how many mAh have left it, its
        temperature, and how long the autopilot thinks it has. An analog power
        module fills in almost none of that, which is the normal case and not
        an error — every field here is published only when the message carries
        a real value, so "not reported" stays distinguishable from zero.

        Only the first battery is published. A second monitor overwriting the
        first would make the top bar read whichever pack sent last.
        """
        if int(getattr(msg, "id", 0) or 0) != 0:
            return

        # UINT16_MAX marks a cell slot the pack does not use; 0 is the same
        # thing in practice, since a cell at 0.000 V is a pack that is not on
        # the aircraft any more.
        cells: list[float] = []
        for raw in list(getattr(msg, "voltages", []) or []):
            value = int(raw)
            if value in (0, 65535):
                continue
            cells.append(round(value / 1000.0, 3))
        for raw in list(getattr(msg, "voltages_ext", []) or []):
            value = int(raw)
            if value in (0, 65535):
                continue
            cells.append(round(value / 1000.0, 3))

        fields: dict[str, Any] = {"battery_cell_voltages": cells}

        # -1 is "not reported"; 0 is a fresh pack, and `or -1` used to turn it
        # into the first.
        raw_consumed = getattr(msg, "current_consumed", -1)
        consumed = -1 if raw_consumed is None else int(raw_consumed)
        if consumed >= 0:
            fields["battery_consumed_mah"] = float(consumed)

        # INT16_MAX is the "unknown" marker; the unit is centidegrees.
        temperature = int(getattr(msg, "temperature", 32767) or 0)
        fields["battery_temperature"] = (
            None if temperature == 32767 else round(temperature / 100.0, 1)
        )

        remaining = int(getattr(msg, "time_remaining", 0) or 0)
        fields["battery_time_remaining"] = max(0, remaining)

        self._store.update(**fields)

    # ------------------------------------------------------------------
    # Controller setpoints (PID tuning)
    # ------------------------------------------------------------------

    def _handle_attitude_target(self, msg: Any) -> None:
        """Publish the attitude and body-rate setpoints the controller is following.

        ATTITUDE_TARGET carries the attitude as a quaternion and the body rates
        as rad/s, and it is the counterpart of ATTITUDE: together they are the
        commanded-versus-achieved pair the PID tuning page plots. Streamed at
        PX4's default rate normally, and raised while the tuning page is open
        (see :meth:`set_tuning_stream`).

        A malformed or short quaternion is dropped rather than guessed at: an
        invented setpoint on a tuning graph is worse than a missing one.
        """
        try:
            quat = list(getattr(msg, "q", None) or [])
            rates = (
                math.degrees(float(msg.body_roll_rate)),
                math.degrees(float(msg.body_pitch_rate)),
                math.degrees(float(msg.body_yaw_rate)),
            )
        except (TypeError, ValueError, AttributeError):
            return
        update: dict[str, Any] = {
            "rollspeed_sp": round(rates[0], 1),
            "pitchspeed_sp": round(rates[1], 1),
            "yawspeed_sp": round(rates[2], 1),
            "setpoints_live": True,
        }
        if len(quat) >= 4:
            try:
                roll, pitch, yaw = _quaternion_to_euler_deg(
                    float(quat[0]), float(quat[1]), float(quat[2]), float(quat[3]))
            except (TypeError, ValueError):
                pass
            else:
                update["roll_sp"] = round(roll, 1)
                update["pitch_sp"] = round(pitch, 1)
                update["yaw_sp"] = round(yaw, 1)
        self._store.update(**update)

    def _handle_position_target(self, msg: Any) -> None:
        """Publish the velocity setpoint the position controller is following.

        POSITION_TARGET_LOCAL_NED carries the whole setpoint triplet; only the
        velocity part is kept, because that is the one the velocity-controller
        gains are read against. The type mask is not consulted: PX4 fills the
        velocity fields with the controller's own setpoint regardless of which
        of them the active mode considers authoritative, and a masked field
        reads as the zero the controller is in fact tracking.
        """
        try:
            self._store.update(
                vx_sp=round(float(msg.vx), 2),
                vy_sp=round(float(msg.vy), 2),
                vz_sp=round(float(msg.vz), 2),
                setpoints_live=True,
            )
        except (TypeError, ValueError, AttributeError):
            return

    # ------------------------------------------------------------------
    # Transmitter channels (Setup -> Radio Control)
    # ------------------------------------------------------------------

    def _handle_rc_channels(self, msg: Any, raw: bool = False) -> None:
        """Publish the raw transmitter channel values, in microseconds.

        The Radio Control page is built on these: the calibration wizard has no
        autopilot-side procedure to follow (PX4 has none for RC), so what the
        operator sweeps on the transmitter reaches the browser through this
        message and nothing else.

        ``RC_CHANNELS`` carries all 18 channels and is what PX4 sends.
        ``RC_CHANNELS_RAW`` carries eight at a time with ``port`` naming the
        bank, and is accepted as a fallback so a non-PX4 autopilot on the same
        link is not silently channel-less; its banks are merged into the
        published list rather than replacing it.

        A channel value of ``UINT16_MAX`` is MAVLink's "not delivered", which is
        published as 0 — the page draws that as an empty channel instead of a
        bar pinned to the top of its range.
        """
        try:
            offset = (int(getattr(msg, "port", 0) or 0) * 8) if raw else 0
            width = 8 if raw else rc_config.MAX_CHANNELS
        except (TypeError, ValueError):
            return
        if offset < 0 or offset >= rc_config.MAX_CHANNELS:
            return

        values: list[int] = []
        for index in range(1, width + 1):
            value = getattr(msg, f"chan{index}_raw", None)
            if value is None:
                break
            try:
                pulse = int(value)
            except (TypeError, ValueError):
                pulse = 0
            values.append(0 if pulse == 65535 else max(0, min(pulse, 65534)))

        if raw:
            # Merge the bank into whatever the other bank already published, so
            # a receiver split across two messages does not flip between eight
            # channels and the other eight on every frame.
            # Clamp the bank to the channels PX4 defines before merging: the
            # offset guard above admits port=2 (channels 17-24), and without
            # this the merged list grows past MAX_CHANNELS and the page draws
            # channels no firmware here has. The RC_CHANNELS path below clamps
            # for the same reason.
            room = rc_config.MAX_CHANNELS - offset
            if room <= 0:
                return
            values = values[:room]
            if not values:
                return
            existing = list(self._store.get_snapshot().get("rc_channels") or [])
            needed = offset + len(values)
            if len(existing) < needed:
                existing.extend([0] * (needed - len(existing)))
            existing[offset:offset + len(values)] = values
            values = existing
            count = len(values)
        else:
            try:
                count = int(getattr(msg, "chancount", 0) or 0)
            except (TypeError, ValueError):
                count = 0
            # chancount is what the receiver actually delivers; trust it over
            # the fixed 18 slots, but never let a bad value grow the list.
            if 0 < count <= len(values):
                values = values[:count]
            else:
                count = len(values)

        self._store.update(
            rc_channels=values,
            rc_channel_count=count,
            rc_rssi=_rc_rssi_percent(getattr(msg, "rssi", 255)),
            rc_live=True,
        )

    # ------------------------------------------------------------------
    # Link-quality tracking (A1)
    # ------------------------------------------------------------------

    def _handle_radio_status(self, msg: Any) -> None:
        """Parse RADIO_STATUS into uplink score and radio metrics (A1).

        SiK radios report RSSI on a 0-255 relative scale; remrssi is the
        drone's signal as seen by the ground radio (the uplink quality
        proxy). txbuf near 100 means the buffer is full (congested).
        rxerrors/fixed are cumulative counters from the radio — stored as-is.
        """
        # RADIO_STATUS also carries rssi, noise and remnoise. They are the
        # *ground* radio's view; the uplink score wants the drone's, which is
        # remrssi + txbuf. They used to be read into three throwaway locals
        # "for completeness" — three getattr + float per RADIO_STATUS on the
        # receive path, to build numbers nothing looked at. A sentence costs
        # less and says the same thing.
        remrssi = float(getattr(msg, "remrssi", 0))
        txbuf = float(getattr(msg, "txbuf", 0))
        rxerrors = int(getattr(msg, "rxerrors", 0))
        fixed = int(getattr(msg, "fixed", 0))
        # 0/255 remrssi on SiK means no remote signal reading → unknown
        # uplink: keep the last score (avoids a momentary 0 flapping the link
        # to "poor" while the radio resyncs). Real signal loss is caught by
        # the heartbeat two-tier path (A3).
        self._radio_status_seen = True
        if remrssi not in (0.0, 255.0):
            remrssi_pct = max(
                0.0, min(100.0, (remrssi - _SIK_RSSI_WEAK)
                         / (_SIK_RSSI_STRONG - _SIK_RSSI_WEAK) * 100.0)
            )
            txbuf_pct = max(0.0, min(100.0, txbuf))
            score = round(0.7 * remrssi_pct + 0.3 * txbuf_pct)
            self._uplink_score = max(0, min(100, score))
        self._store.update(
            uplink=self._uplink_score,
            uplink_rssi=remrssi,
            uplink_rxerrors=rxerrors,
            uplink_fixed=fixed,
        )
        self._publish_link_quality()

    def _publish_heartbeat_jitter(self) -> None:
        """Publish rolling stddev of heartbeat inter-arrival in ms (A1).

        Needs at least two timestamps to compute one interval; below that
        the link is too fresh to characterize and jitter stays 0.
        """
        times = list(self._hb_times)
        if len(times) < 2:
            self._store.update(heartbeat_jitter_ms=0.0)
            return
        intervals_ms = [
            (times[i] - times[i - 1]) * 1000.0 for i in range(1, len(times))
        ]
        mean = sum(intervals_ms) / len(intervals_ms)
        var = sum((v - mean) ** 2 for v in intervals_ms) / len(intervals_ms)
        self._store.update(heartbeat_jitter_ms=round(math.sqrt(var), 1))

    def _current_jitter_ms(self) -> float:
        """Latest published heartbeat jitter (0 before two heartbeats)."""
        times = list(self._hb_times)
        if len(times) < 2:
            return 0.0
        intervals_ms = [
            (times[i] - times[i - 1]) * 1000.0 for i in range(1, len(times))
        ]
        mean = sum(intervals_ms) / len(intervals_ms)
        var = sum((v - mean) ** 2 for v in intervals_ms) / len(intervals_ms)
        return round(math.sqrt(var), 1)

    def _publish_link_quality(self) -> None:
        """Derive the link_quality string from uplink + jitter (A1).

        With RADIO_STATUS: good/fair/poor from uplink score + jitter. Without
        RADIO_STATUS (UDP SITL): heartbeat-freshness-driven — good when fresh,
        left to A3 to mark poor/lost on staleness. 'lost' is owned by A3's drop
        path, not here.
        """
        jitter = self._current_jitter_ms()
        if not self._radio_status_seen:
            # Heartbeat-driven: a fresh heartbeat just arrived (we're in the
            # HEARTBEAT branch) → good. A3 flips to poor/lost on staleness.
            self._store.update(link_quality="good")
            return
        uplink = self._uplink_score
        if uplink >= 70 and jitter < 50.0:
            quality = "good"
        elif uplink >= 40:
            quality = "fair"
        else:
            quality = "poor"
        self._store.update(link_quality=quality)

    def set_vibration_stream(self, enabled: bool, rate_hz: int = 10) -> bool:
        """Enable/disable high-rate VIBRATION streaming from the vehicle.

        Lean — VIBRATION defaults to 0.1 Hz on PX4; the GCS requests ~10 Hz
        only while the vibration plugin is open, then restores the default.
        Safe to call while armed (vibration data is read-only telemetry).
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if not isinstance(rate_hz, int) or not (1 <= rate_hz <= 50):
                    self._set_command_error("vibration rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                # 0 = restore PX4 default rate (lean: high-rate only on demand).
                interval_us = 0
            return self.set_message_interval(
                mavutil.mavlink.MAVLINK_MSG_ID_VIBRATION, interval_us,
            )

    def set_rc_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the RC_CHANNELS rate the Radio Control page needs.

        Lean, exactly like :meth:`set_vibration_stream`: PX4 streams
        RC_CHANNELS at a few hertz by default, which is enough for a channel
        bar and not enough for a calibration — a stick swept through its travel
        in half a second is three samples at 5 Hz, and the endpoint the wizard
        writes is then whatever those three happened to catch.

        Safe while armed, and deliberately so — this is read-only telemetry,
        and the page reads channels in flight to check a switch does what the
        operator thinks it does. Disabling sends interval 0, handing the rate
        back to the firmware's own default rather than to a number this build
        picked.
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if isinstance(rate_hz, bool) or not isinstance(rate_hz, int):
                    self._set_command_error("RC rate must be an integer")
                    return False
                if not 1 <= rate_hz <= 50:
                    self._set_command_error("RC rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                interval_us = 0
            if not self._connection_ready():
                # set_message_interval returns False here without a reason, and
                # the page opened over a dead link is the common case for this
                # route — an unexplained refusal would reach the browser as a
                # conflict rather than as "there is no vehicle".
                self._set_command_error("not connected")
                self._store.update(rc_live=False)
                return False
            ok = self.set_message_interval(
                mavutil.mavlink.MAVLINK_MSG_ID_RC_CHANNELS, interval_us,
            )
            if not enabled:
                # The channel bars stop being live the moment the rate is handed
                # back, so the page stops claiming a reading it is no longer
                # being sent.
                self._store.update(rc_live=False)
            return ok

    # The messages the PID tuning page plots, as commanded/achieved pairs.
    # ATTITUDE is already streamed for the HUD; it is raised here too because a
    # 50 Hz response sampled against a 5 Hz setpoint makes a clean tune look
    # like a lagging one.
    _TUNING_MSG_IDS: tuple[int, ...] = (
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE,
        mavutil.mavlink.MAVLINK_MSG_ID_ATTITUDE_TARGET,
        mavutil.mavlink.MAVLINK_MSG_ID_POSITION_TARGET_LOCAL_NED,
    )

    def set_tuning_stream(self, enabled: bool, rate_hz: int = 20) -> bool:
        """Raise (or restore) the setpoint stream the PID tuning page plots.

        Lean, exactly like :meth:`set_vibration_stream`: PX4 streams
        ATTITUDE_TARGET and POSITION_TARGET_LOCAL_NED slowly by default, the
        GCS asks for a tuning rate only while the page is open, and disabling
        sends interval 0 to hand the rate back to the firmware's own default
        rather than to a number this build picked.

        Safe while armed, and deliberately so — this is read-only telemetry,
        and the page that wants it is used in flight.

        Returns True only if every message was accepted; a firmware that
        refuses one still gets the others, because a missing setpoint trace is
        a degraded graph and not a failure worth aborting the page for.
        """
        with self._operation_lock:
            self._set_command_error("")
            if enabled:
                if isinstance(rate_hz, bool) or not isinstance(rate_hz, int):
                    self._set_command_error("tuning rate must be an integer")
                    return False
                if not 1 <= rate_hz <= 50:
                    self._set_command_error("tuning rate must be between 1 and 50 Hz")
                    return False
                interval_us = max(1, int(1_000_000 / rate_hz))
            else:
                interval_us = 0
            if not self._connection_ready():
                # Same reason set_rc_stream carries this guard: without it
                # set_message_interval clears the command error and returns
                # False without setting one, so the route answers 409 with a
                # generic "request failed" for what is simply no vehicle.
                self._set_command_error("not connected")
                self._store.update(setpoints_live=False)
                return False
            ok = True
            for msg_id in self._TUNING_MSG_IDS:
                if not self.set_message_interval(msg_id, interval_us):
                    ok = False
            if not enabled:
                # The traces stop being live the moment the rate is handed
                # back, so the page stops claiming a setpoint it is no longer
                # being sent.
                self._store.update(setpoints_live=False)
            return ok
