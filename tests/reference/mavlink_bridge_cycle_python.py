"""The bridge's connection cycle and its threads, as Corvus had them in
Python: start and stop, the connect/receive cycle with its backoff, connect,
the vehicle heartbeat wait, the GCS heartbeat loop and the two deferred
workers, all on ``threading.Thread``.

Kept unchanged as the reference tests/test_bridge_cycle_parity.py holds the
Rust engine to; not imported by the app.
"""
from __future__ import annotations

import errno
import logging
import random
import threading
import time
from typing import Any

from corvus_link import mavutil

from corvus.mavlink_bridge import (
    GCS_COMPONENT_ID,
    GCS_SYSTEM_ID,
    HEARTBEAT_WAIT_S,
    MAV_AUTOPILOT_MAP,
    MAV_TYPE_MAP,
    RECONNECT_BASE_S,
    RECONNECT_CAP_S,
    RECONNECT_JITTER,
    RECONNECT_MIN_S,
    _is_vehicle_heartbeat,
)

logger = logging.getLogger("corvus.mavlink")


class LinkCycleMixin:
    """The connection cycle and its threads, as the bridge had them."""

    def _start_locked(self) -> None:
        self._stop_event.clear()
        if self._thread and self._thread.is_alive():
            if self._running.is_set():
                # A healthy MAVLink thread is already running — don't spawn a
                # second one (preserves the original idempotent no-op).
                return
            # stop() was just called but the old thread hasn't fully died yet
            # (its join timed out while _connect() was blocked). Join briefly,
            # then proceed so a fresh start() can reconnect (BUG 3).
            self._thread.join(timeout=1.0)
        self._running.set()
        self._run_generation += 1
        self._thread = threading.Thread(
            target=self._run, args=(self._run_generation,), name="mavlink", daemon=True,
        )
        self._thread.start()

    def _stop_locked(self) -> None:
        self._running.clear()
        self._stop_event.set()
        self._stop_tlog()
        self._store.update(link_status="disconnected")
        # Mark the vehicle disconnected so _connection_ready() returns False and
        # any in-flight command/param writer bails (BUG 3, 7).
        self._store.set_disconnected()
        self._cancel_pending_commands()
        self._abort_parameter_operations()
        # Release exclusive NSH shell mode before the link closes so PX4 frees
        # the shell for other clients. Best-effort; never masks teardown.
        try:
            self.stop_shell()
        except Exception:
            pass
        # Join the batch-upload worker. Its set_param calls were already woken
        # with result=-2 by the _param_set_pending block above, and it re-checks
        # _running each iteration, so it exits promptly; the daemon flag is a
        # backstop. Done before the socket close so a mid-flight send isn't
        # writing on a torn-down link.
        if self._param_upload_thread:
            self._param_upload_thread.join(timeout=3)
            self._param_upload_thread = None
        # Cancels a metadata download mid-transfer: its waits check the
        # cancel event and the stop event every 0.1 s.
        self._stop_param_metadata()
        if self._conn:
            try:
                self._conn.close()
            except Exception:
                pass
            # Clear immediately so _send_command_and_wait's `conn is not
            # self._conn` guard bails on remaining loop iterations (BUG 3).
            self._conn = None
        # Join the deferred daemon workers so stop() never leaves them
        # spinning. _stop_event wakes their interruptible sleeps promptly.
        if self._param_watchdog_thread:
            self._param_watchdog_thread.join(timeout=2)
            self._param_watchdog_thread = None
        if self._version_retry_thread:
            self._version_retry_thread.join(timeout=2)
            self._version_retry_thread = None
        if self._intervals_thread:
            self._intervals_thread.join(timeout=2)
            self._intervals_thread = None
        if self._thread:
            self._thread.join(timeout=3)
        if self._hb_thread:
            self._hb_thread.join(timeout=2)

    def _superseded(self) -> bool:
        """Has a later start() replaced the thread this runs on?

        False on any thread that is not a connect/receive thread (the unit
        tests call _connect directly), so only an orphaned cycle stands down.
        """
        generation = getattr(self._cycle, "generation", None)
        return generation is not None and generation != self._run_generation

    def _run(self, generation: int | None = None) -> None:
        self._cycle.generation = generation
        self._cycle.conn = None
        while self._running.is_set() and not self._superseded():
            cycle_exc: Exception | None = None
            try:
                self._connect()
                self._reconnect_attempt = 1
                # Defer the ACK-confirmed interval requests until the receive
                # loop is draining the socket (BUG 1): _connect() used to run
                # them before _receive_loop() existed, so every COMMAND_ACK
                # timed out and stalled all operator commands issued then.
                self._schedule_message_intervals()
                self._receive_loop()
            except Exception as exc:
                cycle_exc = exc
                if self._running.is_set() and not self._superseded():
                    logger.error("MAVLink error: %s", exc)
                else:
                    # stop() closed the link under the receive loop.
                    logger.debug("MAVLink link closed for shutdown: %s", exc)
            if self._superseded():
                # Another thread owns the link now: close only what this one
                # opened, and leave the shared state to its new owner.
                own = getattr(self._cycle, "conn", None)
                if own is not None and own is not self._conn:
                    try:
                        own.close()
                    except Exception:  # noqa: BLE001 - it is being thrown away
                        pass
                logger.info("MAVLink cycle superseded by a restart; leaving it to the new one")
                return
            self._cancel_pending_commands()
            self._abort_parameter_operations()
            self._reset_shell_state()
            # Close this cycle's tlog so a reconnect opens a fresh file.
            self._stop_tlog()
            conn = self._conn
            self._conn = None
            if conn:
                try:
                    conn.close()
                except Exception:
                    pass
            self._store.set_disconnected()
            if self._running.is_set():
                delay = self._reconnect_delay(self._reconnect_attempt)
                logger.info(
                    "Reconnecting in %.1fs (attempt %d) …",
                    delay, self._reconnect_attempt,
                )
                if cycle_exc is not None:
                    self._store.update(link_status="reconnecting", link_error=str(cycle_exc))
                else:
                    self._store.update(link_status="reconnecting")
                # Interruptible: the backoff reaches 8 s, stop() joins this
                # thread for 3, so a plain sleep meant shutdown returned while
                # the reconnect loop was still parked — and then woke up to
                # reconnect against a store the app had already torn down.
                self._interruptible_sleep(delay)
                self._reconnect_attempt += 1

    def _reconnect_delay(self, attempt: int) -> float:
        """Exponential backoff with jitter between reconnect cycles (A4).

        Unified for serial and UDP — the old linear serial backoff (2s/attempt,
        cap 5s) was too aggressive on lossy 57 kbps radios, hammering the link
        while it was still recovering. base*2^(n-1), capped, +/- 25% jitter.
        """
        raw = min(RECONNECT_BASE_S * (2 ** max(0, attempt - 1)), RECONNECT_CAP_S)
        delayed = raw * (1.0 + random.uniform(-RECONNECT_JITTER, RECONNECT_JITTER))
        return max(RECONNECT_MIN_S, delayed)

    def _connect(self) -> None:
        logger.info("Connecting to %s …", self._conn_str)
        self._reset_shell_state()
        self._reset_parameter_cache()
        self._reset_param_metadata()
        self._request_sent = False
        self._param_encoding_declared = ""
        self._firmware_version = None
        self._last_boot_ms = None
        self._clear_prearm_reasons()
        self._prearm_requested_at = 0.0
        self._kill_evidence_at = 0.0
        self._prearm_last = None
        self._prearm_report_started_at = 0.0
        self._prearm_report_kill = False
        self._home_alt_amsl = None
        self._position_home_alt_amsl = None
        # And forget the DISPLAYED home with it. The bridge already dropped
        # its own altitude reference here; leaving the map's marker behind
        # meant a session that reconnected — to the same aircraft moved between
        # flights, or to a different one — drew last session's launch point as
        # this one's, and the "centre on vehicle" fallback flew there. A home
        # marker has to be something the aircraft said in this session or
        # nothing at all; _request_home() below asks for it immediately so the
        # gap is one round trip rather than the stream's own 0.5 Hz.
        self._store.update(home=[0.0, 0.0])
        # Per-connection-cycle state (A1/A3): fresh jitter window + flags.
        self._forget_link_quality()
        self._degraded_warned = False
        # A new link may be a new aircraft with a different pack, so the
        # latched cell count goes with the old connection.
        self._forget_battery_cells()
        self._forget_gnss_integrity()
        # Same reasoning for Remote ID: the arm status and the "we are sending"
        # timestamp both describe the link that just went away.
        self._forget_remote_id_session()
        # A new link may be a new aircraft, and even the same one may have had
        # its mission replaced while nobody was listening.
        self._forget_vehicle_mission()
        self._vehicle_mission_id = None
        self._vehicle_fence = None
        self._store.update(mission_state="", mission_seq=-1, mission_reached=-1,
                           mission_total=-1)
        self._store.update(
            link_status="connecting",
            link_connection=self._conn_str,
            link_error="",
        )
        try:
            if self._is_serial():
                # mavserial takes the bare device path + a baud kwarg; the
                # ``serial:`` prefix is only a user-facing convention here.
                device, baud = self._parse_serial(self._conn_str)
                opened = mavutil.mavlink_connection(
                    device,
                    baud=baud,
                    source_system=GCS_SYSTEM_ID,
                    source_component=GCS_COMPONENT_ID,
                )
            else:
                opened = mavutil.mavlink_connection(
                    self._conn_str,
                    timeout=2,
                    source_system=GCS_SYSTEM_ID,
                    source_component=GCS_COMPONENT_ID,
                )
            # Opening can block (a TCP dial, a USB port that is slow to come
            # up), long enough for stop() to give up on this thread and a new
            # start() to begin. Then this connection is nobody's.
            if self._superseded():
                try:
                    opened.close()
                except Exception:  # noqa: BLE001 - it is being thrown away
                    pass
                raise ConnectionError("superseded by a restart")
            self._conn = opened
            self._cycle.conn = opened
        except (OSError, FileNotFoundError) as exc:
            if self._is_serial():
                message = self._serial_open_error(exc)
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            if getattr(exc, "errno", None) == errno.EADDRINUSE:
                # "[Errno 48] Address already in use" is true and useless. The
                # cause is nearly always another program already listening on
                # the port — QGroundControl, MAVROS, a second Corvus — and the
                # fix is to pick a different one, which the message should say.
                message = (
                    f"{self._conn_str}: port already in use by another program "
                    f"(QGroundControl, MAVROS/MAVSDK or a second Corvus). "
                    f"Close it or connect on a different port"
                )
                self._store.update(link_error=message)
                raise ConnectionError(message) from exc
            raise
        if self._is_serial():
            self._claim_serial_exclusive(self._parse_serial(self._conn_str)[0])
        else:
            self._warn_if_onboard_port()
        self._apply_signing()
        # Speak first, then listen. A far end that waits to hear from a station
        # before it sends anything (a mavlink-router server endpoint, SITL in a
        # VM reached with udpout:, a USB board with SYS_USB_AUTO = 1) and a
        # station that waits for the vehicle first would each wait for the
        # other until the heartbeat wait below gave up, on every reconnect.
        self._send_gcs_heartbeat(self._conn)
        self._start_gcs_heartbeat()
        logger.info("Waiting for heartbeat …")
        hb = self._wait_vehicle_heartbeat(HEARTBEAT_WAIT_S)
        if hb is None:
            raise ConnectionError("No heartbeat received")
        # PX4 autopilot is comp 1; comp 0 = broadcast (not addressable).
        src_sys = getattr(hb, "get_srcSystem", lambda: 0)() or self._conn.target_system
        src_comp = getattr(hb, "get_srcComponent", lambda: 0)() or self._conn.target_component
        self._target_system = src_sys or 1
        self._target_component = src_comp or 1
        self._ignored_systems = set()
        # The vehicle list is this one vehicle from now on; whatever the link
        # carried before (another aircraft on the same radio) is gone.
        try:
            self.fleet_tracker.bind(self._target_system, self._target_component)
            self.fleet_tracker.observe(hb, vehicle_heartbeat=True)
        except Exception:  # noqa: BLE001 - the vehicle list never costs the link
            logger.debug("fleet tracker failed", exc_info=True)
        # Tell corvus_link which node we picked. It does its own latching in
        # post_message — first heartbeat its `probably_vehicle_heartbeat`
        # accepts wins — and that is not always the one _wait_vehicle_heartbeat
        # chose: behind a router the two can land on different nodes, and
        # mode_mapping() then reads the mode table of whatever corvus_link
        # latched onto instead of the aircraft Corvus is flying. Everything
        # this module sends already addresses _target_system explicitly; this
        # aligns the parts of mavutil that do not.
        try:
            self._conn.target_system = self._target_system
            self._conn.target_component = self._target_component
            # mode_mapping() reads the per-system state of mavutil's own
            # latched sysid, not target_system, so that has to follow too.
            if self._target_system in (getattr(self._conn, "sysid_state", None) or {}):
                self._conn.sysid = self._target_system
        except Exception as exc:  # noqa: BLE001 - a mavutil that will not be told is not fatal
            logger.debug("could not pin mavutil target: %s", exc)
        vtype = MAV_TYPE_MAP.get(hb.type, f"TYPE_{hb.type}")
        autopilot = MAV_AUTOPILOT_MAP.get(hb.autopilot, f"AP_{hb.autopilot}")
        # Before anything decodes a mode. The mode word means different things
        # on different stacks, so reading it with the previous session's
        # dialect is how a reconnect from a PX4 bench to an ArduPilot aircraft
        # used to show PX4 mode names for the first second of the flight.
        self._latch_dialect(hb.autopilot, hb.type)
        armed = bool(hb.base_mode & mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED)
        mode = self._decode_mode(hb)
        vehicle_id = f"{self.fleet_link_id}:{self._target_system}"
        previous_id = self._store.get_snapshot().get("vehicle_id", "")
        if previous_id and previous_id != vehicle_id:
            # Another aircraft than the one this store described. The same
            # one reconnecting keeps its last state, as it always has.
            self._store.forget_vehicle()
        logger.info("Heartbeat: %s / %s (%s) mode=%s (sys=%d comp=%d)",
                    vtype, autopilot, self._dialect.label, mode,
                    self._target_system, self._target_component)
        self._store.update(
            connected=True, vehicle_type=vtype, autopilot=autopilot,
            armed=armed, mode=mode, mode_label=self._dialect.mode_label(mode),
            link_status="connected", link_error="",
            autopilot_stack=self._dialect.stack,
            vehicle_id=vehicle_id,
        )
        self._store.heartbeat()
        self._build_mode_mapping()
        # Per-message SET_MESSAGE_INTERVAL (A5) is what every supported target
        # (v1.16-v1.18) wants; the ACK-confirmed requests are deferred to
        # _run() so the receive loop exists to dispatch their COMMAND_ACKs
        # (BUG 1). The deprecated REQUEST_DATA_STREAM is only the fallback for
        # stacks that do not answer it — ArduPilot above all, where it is the
        # classic mechanism behind the SRx_ parameters. It is NOT a fallback
        # for old PX4: PX4 carries no handler for the message in any release
        # from v1.11 to today, so on a PX4 target it is simply ignored. On UDP
        # it no longer goes out blind here: it waits
        # until those per-message requests prove it is needed — see
        # _request_message_intervals(). A UDP link is rarely the autopilot's
        # only client (MAVROS, a companion computer, mavlink-router), and a
        # blanket legacy rate request is the loudest thing a GCS can say to a
        # stack it did not set up. A serial link still asks up front: it is
        # point to point, there is nobody else to disturb, and a 57 kbps radio
        # must not hold the HUD empty for a command round trip first.
        if self._is_serial():
            self._request_streams()
        self._request_version()
        self._request_home()
        # Start a fresh tlog for this flight session. Gated on _running so the
        # direct-_connect() unit tests (which never start() the bridge) do not
        # open files or spawn writer threads in ~/.corvus/logs; in production
        # _run only calls _connect while _running is set.
        self._tlog_armed = None
        self._start_tlog()

    def _wait_vehicle_heartbeat(self, timeout: float) -> Any:
        """The first HEARTBEAT from an actual autopilot, within *timeout*.

        ``mavutil.wait_heartbeat`` matches on message type alone, so on a link
        that carries more than one node — anything behind mavlink-router — it
        hands back whichever heartbeat happens to arrive first. If that is
        QGroundControl's, ``_target_system`` becomes 255 and every command this
        session sends is addressed to a ground station: arming, mode changes,
        parameter reads and mission uploads all time out, with a connected
        green dot the whole time.

        So keep reading until the aircraft identifies itself, inside the same
        wall-clock budget a single wait would have had.
        """
        deadline = time.monotonic() + timeout
        while True:
            # stop() during connect: bail rather than sit out the rest of the
            # budget. Closing the socket would end the wait too, but only by
            # raising through _run's error path, which then logs a shutdown as
            # a link failure.
            if self._stop_event.is_set() or self._superseded():
                return None
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            conn = self._conn
            if conn is None:
                return None
            # Each wait is capped so the stop check above is reached within a
            # second even on a link where nothing arrives at all; the deadline
            # above still bounds the whole thing to the caller's budget.
            wait_for = min(1.0, remaining)
            waited_from = time.monotonic()
            hb = conn.wait_heartbeat(blocking=True, timeout=wait_for)
            if hb is None:
                # Retry only if that wait really blocked for its timeout. A
                # transport that answers "nothing" the instant it is asked is
                # not going to start answering differently, and retrying it
                # would burn the whole budget at full tilt — so an immediate
                # None is taken at face value, exactly as a single
                # full-length wait used to take it.
                if time.monotonic() - waited_from < wait_for / 2:
                    return None
                continue
            if _is_vehicle_heartbeat(hb):
                return hb
            logger.info(
                "ignoring heartbeat from sys=%s comp=%s: not an autopilot",
                getattr(hb, "get_srcSystem", lambda: "?")(),
                getattr(hb, "get_srcComponent", lambda: "?")(),
            )

    def _start_gcs_heartbeat(self) -> None:
        if self._hb_thread and self._hb_thread.is_alive():
            return
        hb_thread = threading.Thread(target=self._gcs_hb_loop, name="gcs-hb", daemon=True)
        hb_thread.start()
        self._hb_thread = hb_thread

    def _send_gcs_heartbeat(self, conn: Any) -> bool:
        """Send one GCS heartbeat on *conn* now. Never raises.

        A binding UDP socket that has not heard from anybody yet has nowhere to
        send to; corvus_link drops the frame, which is the right outcome there.
        """
        if conn is None:
            return False
        try:
            with self._send_lock:
                if conn is not self._conn:
                    return False
                conn.mav.heartbeat_send(
                    mavutil.mavlink.MAV_TYPE_GCS,
                    mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                    0, 0, 0,
                )
            return True
        except Exception as exc:  # noqa: BLE001 - a first frame that fails is retried at 1 Hz
            logger.debug("GCS heartbeat send failed: %s", exc)
            return False

    def _gcs_hb_loop(self) -> None:
        # Runs while _running is set regardless of link_status: a degraded
        # link (A3) must NOT stop the GCS heartbeat, or the autopilot drops us
        # while we wait for its heartbeat to return (A7).
        me = threading.current_thread()
        while self._running.is_set() and not self._stop_event.is_set():
            conn = self._conn
            if not conn:
                self._interruptible_sleep(1)
                continue
            # Read outside the send lock. The heartbeat starts before the
            # vehicle is found; the Remote ID identity is addressed to it.
            vehicle_known = bool(self._store.get_snapshot().get("connected"))
            try:
                with self._send_lock:
                    # Re-check under the lock: stop()/reconnect can clear
                    # _conn or _running while we waited for the lock.
                    if not self._running.is_set():
                        break
                    if conn is not self._conn:
                        continue
                    conn.mav.heartbeat_send(
                        mavutil.mavlink.MAV_TYPE_GCS,
                        mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                        0, 0, 0,
                    )
                    # Remote ID rides the heartbeat rather than getting a thread
                    # of its own. Both stacks want the identity at about 1 Hz and
                    # fail the pre-arm check when it stops, which is exactly this
                    # loop's cadence and exactly its lifetime — and a second
                    # 1 Hz worker would be a second thing to join on shutdown
                    # (AGENTS.md: every thread the change introduces is torn down).
                    if vehicle_known:
                        self._send_remote_id(conn)
            except Exception:
                # A reconnect swaps/closes the socket under this persistent
                # worker. Stay alive so the next connection gets heartbeats
                # even when it became ready before this thread observed the
                # old socket failure.
                self._interruptible_sleep(0.1)
                continue
            self._interruptible_sleep(1)
        # Allow _start_gcs_heartbeat to restart us on the next connect — but
        # only if nobody already has. A reconnect can call
        # _start_gcs_heartbeat() while this loop is on its way out; clearing
        # the field unconditionally then orphaned the *new* thread, so stop()
        # never joined it and the next connect started a second heartbeat
        # alongside it — two GCS heartbeats at 1 Hz on the same link.
        if self._hb_thread is me:
            self._hb_thread = None

    def _interruptible_sleep(self, seconds: float) -> None:
        """Sleep that returns early when _stop_event is set.

        Sleeps in small slices via time.sleep so tests that monkeypatch
        ``corvus.mavlink_bridge.time.sleep`` still short-circuit the whole
        delay, while stop() can wake the daemon worker within one slice
        (~0.1 s). Used by the deferred interval worker and the version retry
        (BUG 1, 12).
        """
        slept = 0.0
        while slept < seconds:
            if self._stop_event.is_set():
                return
            chunk = min(0.1, seconds - slept)
            time.sleep(chunk)
            slept += chunk

    def _schedule_message_intervals(self) -> None:
        """Defer _request_message_intervals until the receive loop is running.

        MAV_CMD_SET_MESSAGE_INTERVAL is ACK-confirmed and the ACK is
        dispatched by _receive_loop(). Requesting intervals from _connect()
        (before the loop exists) made every ACK time out. This spawns a
        one-shot daemon that fires once the loop is draining the socket, so
        COMMAND_ACKs are actually delivered (BUG 1).
        """
        prior = self._intervals_thread
        if prior is not None and prior.is_alive():
            return

        def _runner() -> None:
            # Let the receive loop start draining the socket first.
            self._interruptible_sleep(0.05)
            if not self._running.is_set() or not self._conn:
                return
            if self._stop_event.is_set():
                return
            try:
                self._request_message_intervals()
            except Exception as exc:
                logger.debug("deferred message intervals failed: %s", exc)
            try:
                self._check_gcs_authority()
            except Exception as exc:
                logger.debug("GCS authority check failed: %s", exc)

        # Started before it is published, here and for every worker stop()
        # joins: stop() runs on another thread, and join() on a thread that is
        # published but not yet started raises, which aborted the teardown.
        intervals_thread = threading.Thread(
            target=_runner, name="mavlink-intervals", daemon=True,
        )
        intervals_thread.start()
        self._intervals_thread = intervals_thread

    def _schedule_version_retry(self) -> None:
        """Send one delayed AUTOPILOT_VERSION retry if the first got no reply.

        PX4 v1.18.0-alpha1 SITL ACKs the capabilities command with
        MAV_RESULT_UNSUPPORTED, so AUTOPILOT_VERSION never arrives; a single
        retry ~4s after connect covers slow/lossy responders without looping
        or spamming the link. Daemon thread; checks _running/_conn/_stop_event
        so it stays silent after shutdown, and the 4s wait uses
        _interruptible_sleep so stop() can wake it and join it promptly (BUG 12).
        """
        if not self._running.is_set():
            return
        prior = self._version_retry_thread
        if prior is not None and prior.is_alive():
            return

        def _retry() -> None:
            self._interruptible_sleep(4.0)
            if not self._running.is_set() or not self._conn:
                return
            if self._store.get_snapshot().get("px4_version"):
                return
            # Separate try blocks on purpose: one request failing must not
            # skip the others, which is exactly what a shared try would do.
            nan = float("nan")
            try:
                with self._send_lock:
                    if not self._running.is_set() or not self._conn:
                        return
                    self._conn.mav.command_long_send(
                        self._target_system, self._target_component,
                        mavutil.mavlink.MAV_CMD_REQUEST_MESSAGE, 0,
                        float(self._MSG_ID_AUTOPILOT_VERSION),
                        nan, nan, nan, nan, nan, nan,
                    )
            except Exception:
                pass
            try:
                with self._send_lock:
                    if not self._running.is_set() or not self._conn:
                        return
                    self._conn.mav.autopilot_version_request_send(
                        self._target_system, self._target_component,
                    )
            except Exception:
                pass

        version_retry_thread = threading.Thread(
            target=_retry, name="px4-ver-retry", daemon=True,
        )
        version_retry_thread.start()
        self._version_retry_thread = version_retry_thread
