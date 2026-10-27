"""The bridge's own requests to the vehicle and MAVLink signing, as Corvus had
them in Python: the stream rate tables and requests, the GCS authority check,
the home and version requests, the interval command and applying signing.

Kept unchanged as the reference tests/test_bridge_requests_parity.py holds the
Rust engine to; not imported by the app.
"""
from __future__ import annotations

import logging

from corvus_link import mavutil

from corvus import autopilot as autopilot_dialect
from corvus.mavlink_bridge import GCS_SYSTEM_ID, INTERVAL_REQUEST_SILENT_LIMIT
from corvus.mavlink_common import MAV_RESULT_TEXT
from tests.reference.mavlink_signing_python import _allow_unsigned_signed_link, _load_signing_key

logger = logging.getLogger("corvus.mavlink")


class RequestMixin:
    """The bridge's own requests and signing, as the bridge had them."""

    def _apply_signing(self) -> None:
        """Turn on MAVLink 2 signing when the operator has configured a key.

        Opt-in, and off unless ``CORVUS_MAVLINK_SIGNING_KEY_FILE`` names a
        32-byte key the autopilot has been given as well. With it on, every
        frame Corvus sends is signed and unsigned frames are rejected — which
        is the only thing that stops somebody within radio range of a SiK link
        from injecting a disarm, because the radio itself authenticates
        nothing and will relay whatever it hears on its frequency.

        Three messages stay exempt, and deliberately: RADIO_STATUS comes from
        the SiK modem, which has no key and cannot sign; ADSB_VEHICLE and
        COLLISION are traffic warnings from transponders outside the vehicle.
        None of the three can command anything, and dropping them would cost
        the link-quality readout and collision alerts for no gain.

        A key that cannot be loaded fails the connection rather than falling
        back to an unsigned link. Anything else would mean a typo in a path
        silently downgrades the security the operator asked for, and they would
        have no way to tell the difference from the outside.
        """
        try:
            key = _load_signing_key()
        except (OSError, ValueError, PermissionError) as exc:
            message = f"MAVLink signing key unusable: {exc}"
            logger.error("%s", message)
            self._store.update(link_error=message)
            raise ConnectionError(message) from exc
        if key is None:
            return
        try:
            # Signing only exists in MAVLink 2, and the key lives on conn.mav.
            # A connection that starts out as MAVLink 1 swaps conn.mav for a
            # fresh object when the first MAVLink 2 frame arrives
            # (mavutil.auto_mavlink_version), and the key goes with the old
            # one: the link then says "signing enabled" and sends every frame
            # unsigned. Switching to MAVLink 2 before the key is installed
            # leaves nothing for that swap to do.
            if getattr(self._conn, "WIRE_PROTOCOL_VERSION", "2.0") != "2.0":
                self._conn.auto_mavlink_version(b"\xfd")
            self._conn.setup_signing(
                key,
                sign_outgoing=True,
                allow_unsigned_callback=_allow_unsigned_signed_link,
            )
        except Exception as exc:  # noqa: BLE001 - a dialect without signing support
            message = f"MAVLink signing could not be enabled: {exc}"
            logger.error("%s", message)
            self._store.update(link_error=message)
            raise ConnectionError(message) from exc
        logger.info("MAVLink 2 signing enabled for %s", self._conn_str)
        self._console_publish("LINK", "MAVLink signing enabled", "success")

    def _stream_rates(self) -> dict[int, int]:
        """Per-stream REQUEST_DATA_STREAM rates, throttled on radio links.

        A SiK Telemetry Radio V3 tunnels MAVLink over a ~57 kbps serial link,
        so the default 50 Hz attitude/RC streams would saturate it. Keep
        MAV_DATA_STREAM_* constants: they are what ArduPilot honours, and the
        reason this path exists at all. PX4 ignores the message entirely, so
        these rates only ever reach a non-PX4 stack.
        """
        # EXTENDED_STATUS is the group that carries SYS_STATUS, GPS_RAW_INT
        # and MISSION_CURRENT on ArduPilot. Without it a stack that needs this
        # fallback had no battery, no GPS fix and no mission progress at all.
        if self._is_slow_link():
            return {
                mavutil.mavlink.MAV_DATA_STREAM_EXTENDED_STATUS: 2,
                mavutil.mavlink.MAV_DATA_STREAM_POSITION: 5,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA1: 10,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA2: 5,
                mavutil.mavlink.MAV_DATA_STREAM_EXTRA3: 2,
                mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS: 0,
                mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS: 0,
            }
        return {
            mavutil.mavlink.MAV_DATA_STREAM_EXTENDED_STATUS: 2,
            mavutil.mavlink.MAV_DATA_STREAM_POSITION: 10,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA1: 50,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA2: 10,
            mavutil.mavlink.MAV_DATA_STREAM_EXTRA3: 5,
            mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS: 5,
            mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS: 5,
        }

    def _request_streams(self) -> None:
        """Send the deprecated REQUEST_DATA_STREAM set, at most once per link.

        The legacy path. On serial it runs from _connect() so telemetry starts
        without waiting on a command round trip; on UDP it runs only if
        _request_message_intervals() finds the modern mechanism missing. The
        _request_sent latch is what lets that second caller retry per message
        without sending the whole set again, and _connect() clears it so the
        next connection makes the decision afresh.
        """
        conn = self._conn
        if self._request_sent or conn is None:
            return
        for stream, rate in self._stream_rates().items():
            if rate == 0:
                continue
            try:
                with self._send_lock:
                    if conn is not self._conn:
                        return
                    conn.mav.request_data_stream_send(
                        self._target_system, self._target_component, stream, rate, 1,
                    )
            except Exception as exc:  # noqa: BLE001 - a fallback request is best-effort
                logger.debug("REQUEST_DATA_STREAM %s failed: %s", stream, exc)
        self._request_sent = True

    def _message_intervals(self) -> dict[int, int]:
        """Per-message MAV_CMD_SET_MESSAGE_INTERVAL targets in microseconds.

        Modern PX4 (v1.16-v1.18) rate-control, and the only mechanism PX4 has
        ever answered; REQUEST_DATA_STREAM (above) is the fallback for stacks
        that do not, which in practice means ArduPilot. Radio links are
        throttled to fit a 57 kbps SiK radio; a USB cable is not. VIBRATION
        is on-demand only (not listed here).
        """
        m = mavutil.mavlink
        if self._is_slow_link():
            return {
                m.MAVLINK_MSG_ID_ATTITUDE: 100000,            # 10 Hz
                m.MAVLINK_MSG_ID_GLOBAL_POSITION_INT: 200000,  # 5 Hz
                m.MAVLINK_MSG_ID_VFR_HUD: 200000,             # 5 Hz
                m.MAVLINK_MSG_ID_SYS_STATUS: 1_000_000,       # 1 Hz
                m.MAVLINK_MSG_ID_GPS_RAW_INT: 1_000_000,      # 1 Hz
                # 0.2 Hz, because home is where RTL ends and a stale one on
                # the map is a lie about where the aircraft will come back to.
                # Cheap at this rate — HOME_POSITION is 60 bytes — and it is
                # the one field the serial set used to leave entirely to PX4's
                # own defaults, which not every firmware publishes.
                m.MAVLINK_MSG_ID_HOME_POSITION: 5_000_000,    # 0.2 Hz
                # 1 Hz even on a 57 kbps radio: the message is two bytes of
                # payload, and it is what tells the bar the aircraft is flying.
                m.MAVLINK_MSG_ID_EXTENDED_SYS_STATE: 1_000_000,  # 1 Hz
                # 0.5 Hz: the pack's own detail — per-cell volts, consumed mAh,
                # temperature. SYS_STATUS carries none of it, and on a smart or
                # DroneCAN battery it is the difference between a pack voltage
                # and knowing which cell is dragging the pack down. 41 bytes
                # twice a minute of a 57 kbps budget.
                m.MAVLINK_MSG_ID_BATTERY_STATUS: 2_000_000,   # 0.5 Hz
                # Mission progress, and the uptime that tells a reboot from a
                # dropped link. ArduPilot streams neither unless asked: its
                # SRx rates default to 0, and on a link where this batch is
                # accepted REQUEST_DATA_STREAM is never sent.
                m.MAVLINK_MSG_ID_MISSION_CURRENT: 1_000_000,  # 1 Hz
                m.MAVLINK_MSG_ID_SYSTEM_TIME: 2_000_000,      # 0.5 Hz
            }
        # No HEARTBEAT: it is 1 Hz on every stack by definition, and PX4 v1.16
        # to v1.18 answer an interval request for it with FAILED
        # (MavlinkReceiver::set_message_interval), on every connect.
        return {
            m.MAVLINK_MSG_ID_ATTITUDE: 20_000,                # 50 Hz
            m.MAVLINK_MSG_ID_GLOBAL_POSITION_INT: 100_000,    # 10 Hz
            m.MAVLINK_MSG_ID_VFR_HUD: 100_000,               # 10 Hz
            m.MAVLINK_MSG_ID_SYS_STATUS: 1_000_000,          # 1 Hz
            m.MAVLINK_MSG_ID_GPS_RAW_INT: 1_000_000,         # 1 Hz
            m.MAVLINK_MSG_ID_HOME_POSITION: 1_000_000,       # 1 Hz
            m.MAVLINK_MSG_ID_EXTENDED_SYS_STATE: 1_000_000,  # 1 Hz
            # 5 Hz, matching what MAV_DATA_STREAM_RC_CHANNELS used to ask for.
            # That request is no longer sent on UDP, and RC_CHANNELS is the one
            # message Corvus displays that the legacy set covered and this one
            # did not — leaving it out would trade a stream nobody asked to
            # lose for the fix. Off the serial list still, where the radio's
            # 57 kbps buys attitude and position instead.
            m.MAVLINK_MSG_ID_RC_CHANNELS: 200_000,           # 5 Hz
            # See the serial list: the pack detail SYS_STATUS has no room for.
            # Asked for explicitly rather than left to the firmware's defaults,
            # which publish it on some builds and not others.
            m.MAVLINK_MSG_ID_BATTERY_STATUS: 1_000_000,      # 1 Hz
            # See the serial list: nothing else asks ArduPilot for these.
            m.MAVLINK_MSG_ID_MISSION_CURRENT: 1_000_000,     # 1 Hz
            m.MAVLINK_MSG_ID_SYSTEM_TIME: 1_000_000,         # 1 Hz
        }

    def _check_gcs_authority(self) -> None:
        """Warn when this ArduPilot vehicle will ignore our stick input.

        ArduPilot accepts ``MANUAL_CONTROL`` and ``RC_CHANNELS_OVERRIDE`` only
        from the system id in ``SYSID_MYGCS`` (``MAV_GCS_SYSID`` from 4.5), and
        it *silently drops* everything else — there is no NAK, no STATUSTEXT and
        no ACK to miss. Corvus announces itself as system 254 rather than the
        conventional 255, deliberately, so that it can share a link with
        QGroundControl without the two fighting over one id. On a stock
        ArduPilot vehicle that means the joystick does nothing at all, and does
        it without a single sign that anything is wrong.

        So it is checked once per connection and reported with the fix in it.
        Corvus does not write the parameter itself: which ground station is
        allowed to take an aircraft's sticks is the operator's decision, not a
        thing to change behind their back.

        PX4 has no equivalent gate — it accepts MANUAL_CONTROL from any GCS —
        so this runs on an ArduPilot link only.
        """
        if self._dialect.stack != autopilot_dialect.STACK_ARDUPILOT:
            return
        if not self._connection_ready():
            return
        values = self.fetch_params(list(self._GCS_AUTHORITY_PARAMS), timeout=3.0)
        name = next((n for n in self._GCS_AUTHORITY_PARAMS if n in values), "")
        if not name:
            # An ArduPilot that answers for neither is old enough or odd enough
            # that guessing would be worse than saying nothing.
            return
        current = int(round(values[name]))
        if current == GCS_SYSTEM_ID:
            return
        text = (
            f"This vehicle only accepts stick input from ground station "
            f"{current} ({name}), and Corvus is {GCS_SYSTEM_ID}, so the joystick "
            f"will be ignored until {name} is set to {GCS_SYSTEM_ID} in "
            f"Parameters. Flight commands and mode changes are unaffected."
        )
        logger.info("%s", text)
        self._console_publish("JOYSTICK", text, "warning")
        self._store_update_warning(text, "warning")

    def _request_message_intervals(self) -> None:
        """Ask PX4 for per-message stream intervals (A5).

        Best-effort: a firmware that DENIES one message must not block the
        rest. Each call goes through _send_command_and_wait (ACK-confirmed).

        This is also where the legacy REQUEST_DATA_STREAM fallback is decided
        on UDP, because the ACKs are the only honest evidence of which
        mechanism the autopilot speaks — the firmware version cannot be used:
        AUTOPILOT_VERSION arrives later than this batch, and on the builds
        that answer the capabilities command with MAV_RESULT_UNSUPPORTED it
        may not arrive at all. So the three answers are read for what they
        each mean. MAV_RESULT_UNSUPPORTED is the firmware saying it has never
        heard of the command: that is an older ArduPilot, or something that is
        not PX4 at all, the fallback is the whole point, and the rest of the
        list would only spend another round trip each proving the same thing.
        (It is never an old PX4 — every PX4 release answers this command, and
        none of them answer the fallback.) A missing ACK is the link saying nothing
        at all — send the fallback once as insurance so the operator is not
        left with a connected link and an empty HUD, but keep going, since the
        next message may well be answered. Anything else — an accept, or a
        DENIED for one message this build does not carry — is proof the modern
        path works, and REQUEST_DATA_STREAM is then never sent.

        Quietly, too: a refusal here is an answer the loop acts on, not a
        failure the operator did anything to cause. Reported the way an
        operator's own request is, every connect to a PX4 put a critical
        "Set message interval failed" on the status bar for MISSION_CURRENT,
        which PX4 sends from its mission manager rather than as a stream.
        """
        silent = 0
        for msg_id, interval_us in self._message_intervals().items():
            try:
                _ok, result = self._set_message_interval(msg_id, interval_us, quiet=True)
            except Exception as exc:
                logger.debug("SET_MESSAGE_INTERVAL msg %d failed: %s", msg_id, exc)
                continue
            if result == -2:
                # Link torn down under us; the next connect starts over.
                return
            if result == mavutil.mavlink.MAV_RESULT_UNSUPPORTED:
                logger.info(
                    "SET_MESSAGE_INTERVAL unsupported (msg %d), falling back "
                    "to REQUEST_DATA_STREAM for this connection", msg_id,
                )
                self._request_streams()
                return
            if result == -1:
                self._request_streams()
                silent += 1
                # Each unanswered request costs two full ACK timeouts, and it
                # takes _operation_lock for both — so on a link that is not
                # answering at all, walking the whole list spent the better
                # part of a minute with every operator command queued behind a
                # batch that was already known to be going nowhere. A few in a
                # row is that answer; the fallback has been sent, and the
                # remaining messages would only prove the same thing again.
                if silent >= INTERVAL_REQUEST_SILENT_LIMIT:
                    logger.info(
                        "no COMMAND_ACK for %d interval requests, leaving the "
                        "rest to REQUEST_DATA_STREAM for this connection",
                        silent,
                    )
                    return
            else:
                silent = 0

    def _request_home(self) -> None:
        """Ask the autopilot to send HOME_POSITION now.

        Home arrives on its own — PX4 publishes it at 0.5 Hz and again whenever
        it changes — so this is not what makes the marker appear. It is what
        makes it appear *promptly*, and the two moments where the wait is worst
        are the two that call it: straight after connect, where the map would
        otherwise hold an empty home for up to two seconds while the operator
        looks for the launch point, and straight after ``set_home``, where the
        operator has just clicked a spot and is waiting to see the marker move
        there.

        Fire-and-forget, both times. Whether it worked is judged by
        HOME_POSITION arriving, and the periodic stream is the backstop if it
        did not; waiting for an ACK here would stall the connect path (the
        receive loop is not dispatching yet) and would put a round trip in
        front of set_home's own reply.
        """
        conn = self._conn
        if conn is None:
            return
        nan = float("nan")
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return
                conn.mav.command_long_send(
                    self._target_system, self._target_component,
                    mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE, 0,
                    float(self._MSG_ID_HOME_POSITION),
                    nan, nan, nan, nan, nan, nan,
                )
        except Exception as exc:  # noqa: BLE001 - a nudge that fails costs nothing
            logger.debug("HOME_POSITION request failed: %s", exc)

    def _request_version(self) -> None:
        """Ask the autopilot for AUTOPILOT_VERSION — the firmware version.

        Three requests, because no single one covers the supported firmware
        range. PX4 answers MAV_CMD_REQUEST_MESSAGE (v1.16-v1.18, and the only
        one v1.18 accepts); older builds answer the now-deprecated
        MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES; the bare
        AUTOPILOT_VERSION_REQUEST message is an ArduPilot-era legacy that PX4
        does not handle at all — which is why sending only that one left the
        firmware version blank on every supported target.

        Fire-and-forget: this runs inside _connect(), before the receive loop
        is dispatching COMMAND_ACKs, so waiting for an ACK here would stall the
        connect path. Whether it worked is judged by the answer arriving, and
        _schedule_version_retry() covers the case where it did not.
        """
        if self._conn:
            nan = float("nan")
            requests = (
                (mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE,
                 [float(self._MSG_ID_AUTOPILOT_VERSION), nan, nan, nan, nan, nan, nan]),
                (mavutil.mavlink.MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES,
                 [1.0, nan, nan, nan, nan, nan, nan]),
            )
            for command, params in requests:
                try:
                    with self._send_lock:
                        self._conn.mav.command_long_send(
                            self._target_system, self._target_component,
                            command, 0, *params,
                        )
                except Exception:
                    pass
            try:
                with self._send_lock:
                    self._conn.mav.autopilot_version_request_send(
                        self._target_system, self._target_component,
                    )
            except Exception:
                pass
        self._schedule_version_retry()

    def _set_message_interval(
        self, msg_id: int, interval_us: int, quiet: bool = False,
    ) -> tuple[bool, int]:
        """The same request, with the raw COMMAND_ACK result beside the verdict.

        The connect-time batch needs the result code and not just the bool:
        it is what separates a firmware that has never heard of
        MAV_CMD_SET_MESSAGE_INTERVAL (MAV_RESULT_UNSUPPORTED) from one that
        refuses a single message it does not carry (MAV_RESULT_DENIED), and
        only the first is grounds for falling back to REQUEST_DATA_STREAM.
        -1 is no ACK at all, -2 no usable link. *quiet* keeps the outcome off
        the console and the status bar; the reason is still the last error.
        """
        with self._operation_lock:
            self._set_command_error("")
            if not self._connection_ready():
                return False, -2
            nan = float("nan")
            result = self._send_command_and_wait(
                mavutil.mavlink.MAV_CMD_SET_MESSAGE_INTERVAL,
                [float(msg_id), float(interval_us), nan, nan, nan, nan, nan],
                timeout=3.0, retries=1,
            )
            if result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                if quiet:
                    self._set_command_error(
                        "Set message interval failed: "
                        + MAV_RESULT_TEXT.get(result, f"RESULT_{result}"))
                    logger.debug("SET_MESSAGE_INTERVAL msg %d: result %s", msg_id, result)
                    return False, result
                return self._command_failure("Set message interval", result), result
            if quiet:
                return True, result
            self._console_publish(
                "STREAM",
                f"Message {msg_id} interval set to {interval_us} us", "success",
            )
            return True, result
