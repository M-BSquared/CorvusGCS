"""The MAVLink parameter protocol: download, read, write, verify.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`: the methods run on
the bridge and use its link, locks and state (``_conn``, ``_send_lock``,
``_operation_lock``, ``_store``), which the bridge's ``__init__`` sets up.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
import math
import struct
import threading
import time
from dataclasses import dataclass
from typing import Any
from collections.abc import Callable

from pymavlink import mavutil

from . import autopilot as autopilot_dialect
from .mavlink_common import _PendingAck

logger = logging.getLogger("corvus.mavlink")

# Lossy-link parameter-download recovery (BUG 5). After the PARAM_VALUE burst
# settles, re-request missing indices (bounded rounds), then give up to a
# terminal "incomplete" state so the editor is never stuck at complete:false.
PARAM_DOWNLOAD_INACTIVITY_S = 1.0
PARAM_DOWNLOAD_MAX_ROUNDS = 3
PARAM_DOWNLOAD_TIMEOUT_S = 30.0
# The same budget over a telemetry radio. PX4 exposes on the order of 1400
# parameters, and a PARAM_VALUE is 25 bytes on the wire: the burst alone is
# ~35 kB, which at 57 kbps cannot finish inside 30 s even before the radio
# shares the link with attitude and position. The old single ceiling therefore
# marked every serial download "incomplete" at 30 s while it was still
# arriving normally — a timeout reporting nothing but its own tightness.
PARAM_DOWNLOAD_TIMEOUT_S_SERIAL = 180.0
PARAM_WATCHDOG_TICK_S = 0.5
# Retransmit pacing. The watchdog re-requests the PARAM_VALUEs a lossy link
# lost, and it used to ask for every one of them back to back. On the link
# where that actually happens — a 57 kbps SiK radio, which is the whole reason
# a parameter goes missing — several hundred PARAM_REQUEST_READs is a solid
# block of uplink, and the vehicle answers each one. The radio saturates, the
# GCS heartbeat behind it cannot get out on time, and the recovery makes the
# loss it is recovering from worse.
#
# So a round is bounded and paced: at most this many requests, spaced far
# enough apart that the replies interleave with telemetry instead of competing
# with it. Whatever is still missing is picked up by the next round — the
# rounds are what the retransmit budget was always expressed in.
PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL = 120
PARAM_RETRANSMIT_MAX_PER_ROUND_UDP = 250
PARAM_RETRANSMIT_GAP_S_SERIAL = 0.02
PARAM_RETRANSMIT_GAP_S_UDP = 0.002
# PX4 currently exposes far fewer parameters than this on every supported
# target.  The bound protects the retransmit watchdog from an invalid or
# hostile PARAM_VALUE count causing tens of thousands of requests and a large
# in-memory cache on a shared UDP link.
PARAM_CACHE_MAX_ENTRIES = 10_000

# AUTOPILOT_VERSION.capabilities bits that say how integer parameters travel.
# Numbers rather than names: older pymavlink builds call the first one
# PARAM_UNION and lack the second.
_CAP_PARAM_ENCODE_BYTEWISE = 16
_CAP_PARAM_ENCODE_C_CAST = 131072

# PX4 opens every full parameter list with this pseudo parameter (index -1, the
# hash of all values, for a GCS that caches). It is not a parameter: counting
# it declared a download complete one real parameter early.
_PARAM_HASH_CHECK = "_HASH_CHECK"

# How many parameters one verify request may name, and how often one that did
# not stick is written again before the check gives up on it.
PARAM_VERIFY_MAX_NAMES = 200
PARAM_VERIFY_REWRITES = 2

# Named reads a parameter may go unanswered in, while the vehicle answers
# others in the same read, before it counts as absent on this firmware. The
# setup pages list candidates for several firmware versions; without this every
# page open re-asked for the ones this vehicle never had and sat out the whole
# read budget waiting for them. Two, not one, so a single reply lost on a radio
# link does not hide a real parameter for the rest of the session.
PARAM_ABSENT_AFTER_MISSES = 2


def _param_value_wire_bytes(msg: Any) -> bytes | None:
    """The four bytes PARAM_VALUE.param_value arrived as, or None.

    Read from the frame rather than from the decoded float because a NaN bit
    pattern does not survive pymavlink's float conversion, and a PX4 integer
    parameter can be one. param_value is the first payload field in both
    MAVLink versions; the header is 10 bytes on v2 (0xFD) and 6 on v1 (0xFE).
    """
    get_buf = getattr(msg, "get_msgbuf", None)
    if not callable(get_buf):
        return None
    try:
        buf = bytes(get_buf() or b"")
    except Exception:  # noqa: BLE001 - a message without a frame falls back
        return None
    if not buf:
        return None
    header = {0xFD: 10, 0xFE: 6}.get(buf[0])
    if header is None or len(buf) < header + 4:
        return None
    return buf[header:header + 4]


@dataclass
class ParamEntry:
    name: str
    value: float
    type: int        # MAV_PARAM_TYPE_* from the PARAM_VALUE message
    index: int
    count: int
    # Arrival order of the PARAM_VALUE this entry came from, so a read can tell
    # a value the vehicle just sent from one the cache has held for an hour. A
    # counter rather than a clock: two frames can land in one clock tick.
    seq: int = 0



class ParamProtocolMixin:
    """PARAM_REQUEST_LIST / _READ / PARAM_SET / PARAM_VALUE, and the checks on top."""

    def _init_param_state(self) -> None:
        """Set up the parameter state; called from the bridge's __init__."""
        # Parameter encoding the vehicle declared in AUTOPILOT_VERSION, or ""
        # until it does; the dialect's default applies meanwhile.
        self._param_encoding_declared = ""
        self._params: dict[str, ParamEntry] = {}
        self._param_count: int = -1
        self._param_received: int = 0
        self._param_download_state: str = "idle"
        self._param_seen_indices: set[int] = set()
        # Bumped for every PARAM_VALUE cached; see ParamEntry.seq.
        self._param_rx_seq: int = 0
        # name -> named reads it went unanswered in; see PARAM_ABSENT_AFTER_MISSES.
        self._param_misses: dict[str, int] = {}
        # Lossy-link download recovery (BUG 5): the watchdog re-requests lost
        # PARAM_VALUEs once the burst settles, then flips to a terminal
        # "incomplete" so the editor never waits forever at complete:false.
        self._param_download_started_at: float = 0.0
        self._param_retransmit_round: int = 0
        self._param_last_value_at: float = 0.0
        self._param_watchdog_thread: threading.Thread | None = None
        self._param_lock = threading.Lock()
        self._param_listeners: list[Callable[[dict[str, Any]], None]] = []
        self._param_set_pending: dict[str, _PendingAck] = {}
        # Background batch-upload state. The worker thread owns the loop; the
        # fields are touched under _param_lock so the SSE/HTTP readers see a
        # consistent snapshot. _param_upload_failed is reset per start_param_upload
        # and copied out into _param_upload_result at completion.
        self._param_upload_thread: threading.Thread | None = None
        self._param_upload_cancel = threading.Event()
        self._param_upload_failed: list[dict[str, Any]] = []
        self._param_upload_result: dict[str, Any] = {}
        # An upload's own progress. Kept apart from _param_count/_received,
        # which describe the cached parameter set: the write echoes land in
        # that cache, and counting them twice once showed "3 / 1400" for a
        # five-parameter file.
        self._param_upload_total: int = 0
        self._param_upload_done: int = 0
        # What the set was before the upload, and so what it is again after:
        # a complete set that had values written into it is still complete.
        self._param_state_before_upload: str = "idle"

    def _abort_parameter_operations(self) -> None:
        """Wake parameter waiters and prevent an upload crossing a reconnect.

        The download state goes back to idle whatever it was, not only when
        something was in flight. "complete" means *this vehicle has given us
        its parameters*, and the moment the link is gone that is no longer a
        claim Corvus can make — the editor would otherwise sit at complete,
        offering writes, against an aircraft that is not there. Guarding the
        reset on an in-progress state left exactly that behind.
        """
        self._param_upload_cancel.set()
        with self._param_lock:
            self._param_download_state = "idle"
            for pending in self._param_set_pending.values():
                pending.result = -2
                pending.event.set()
            self._param_set_pending.clear()

    def _reset_parameter_cache(self) -> None:
        """Discard vehicle-specific parameter metadata for a new link cycle."""
        self._abort_parameter_operations()
        with self._param_lock:
            self._params.clear()
            self._param_count = -1
            self._param_received = 0
            self._param_download_state = "idle"
            self._param_seen_indices.clear()
            self._param_download_started_at = 0.0
            self._param_retransmit_round = 0
            self._param_last_value_at = 0.0
            self._param_misses.clear()

    # ------------------------------------------------------------------
    # Parameter protocol
    # ------------------------------------------------------------------

    def request_param_list(self) -> bool:
        """Start a full parameter download from the vehicle.

        Lazy, operator-triggered — keeps the GCS lean; only GPS/telemetry
        streams until requested. Returns False when disconnected or another
        parameter operation is in progress.
        """
        with self._operation_lock:
            if not self._connection_ready():
                # Surface a fresh error rather than a stale thread-local one
                # (BUG 11).
                self._set_command_error("not connected")
                return False
            with self._param_lock:
                # Guard against a concurrent upload/download (BUG 4): the upload
                # worker reads self._params for MAV_PARAM_TYPE, so clobbering it
                # here would corrupt an in-flight upload.
                if self._param_download_state in ("downloading", "uploading"):
                    self._set_command_error("another parameter operation in progress")
                    return False
                self._param_download_state = "downloading"
                self._params.clear()
                self._param_received = 0
                self._param_count = -1
                self._param_seen_indices = set()
                # Lossy-link recovery bookkeeping (BUG 5).
                now = time.monotonic()
                self._param_download_started_at = now
                self._param_last_value_at = now
                self._param_retransmit_round = 0
            conn = self._conn
            try:
                with self._send_lock:
                    if conn is None or conn is not self._conn:
                        raise ConnectionError("link closed")
                    conn.mav.param_request_list_send(
                        self._target_system, self._target_component,
                    )
            except Exception as exc:  # noqa: BLE001 - a torn-down link is not a 500
                # Roll the state machine back: leaving it at "downloading" with
                # nothing on the wire would block every later parameter
                # operation behind an "in progress" that never progresses.
                with self._param_lock:
                    if self._param_download_state == "downloading":
                        self._param_download_state = "idle"
                self._set_command_error(f"parameter list request failed: {exc}")
                return False
            self._start_param_watchdog()
            return True

    # PARAM_VALUE / PARAM_SET carry the id in a fixed char[16] with no
    # terminator when it is exactly full, so anything longer is not a
    # parameter this vehicle could ever have — and pymavlink packs it by
    # raising rather than truncating.
    PARAM_ID_MAX_BYTES = 16

    @classmethod
    def _encode_param_name(cls, name: Any) -> bytes | None:
        """Encode *name* for the wire, or None when it cannot be one.

        Rejects instead of truncating: a silently shortened name addresses a
        *different* parameter, which on a parameter write is the difference
        between setting a rate gain and setting something else entirely.
        """
        if not isinstance(name, str) or not name:
            return None
        try:
            encoded = name.encode("ascii")
        except UnicodeEncodeError:
            return None
        if len(encoded) > cls.PARAM_ID_MAX_BYTES:
            return None
        return encoded

    def request_param(self, name: str) -> bool:
        """Request a single parameter by name (param_index=-1)."""
        with self._operation_lock:
            if not self._connection_ready():
                return False
            encoded = self._encode_param_name(name)
            if encoded is None:
                self._set_command_error("invalid parameter name")
                return False
            conn = self._conn
            try:
                with self._send_lock:
                    if conn is None or conn is not self._conn:
                        return False
                    conn.mav.param_request_read_send(
                        self._target_system, self._target_component,
                        encoded, -1,
                    )
            except Exception as exc:  # noqa: BLE001 - a torn-down link is not a 500
                logger.debug("param read request failed (%s): %s", name, exc)
                return False
            return True

    def fetch_params(
        self, names: list[str], timeout: float = 4.0, fresh: bool = False,
    ) -> dict[str, float]:
        """Read a named set of parameters without a full parameter download.

        The Motors page needs ~100 specific parameters, not the ~1300 a full
        download pulls. This asks for the missing ones in one burst of
        ``PARAM_REQUEST_READ`` (one retransmit round for the stragglers, since a
        lossy link drops individual replies) and returns ``{name: value}`` for
        whatever arrived before *timeout*.

        Names the vehicle never answers for are simply absent from the result —
        that is the version-tolerance contract: a parameter this firmware does
        not have is a missing key, never an error. Nothing here mutates the
        download state machine, so it is safe to call while the parameter
        editor's full download is idle *or* running.

        *fresh* ignores the cache and asks the vehicle for every name: only a
        value that arrives after the call started counts. That is the read a
        "was it applied" check needs, because the cache also holds values the
        vehicle has since changed on its own or through another station.

        A name this vehicle is known not to have is not asked for again: after
        a complete download anything not in it, and otherwise a name that went
        unanswered in :data:`PARAM_ABSENT_AFTER_MISSES` reads the vehicle
        answered others in. Both hold until the next connection.
        """
        wanted = [n for n in names if isinstance(n, str) and n]
        if not wanted:
            return {}
        if not self._connection_ready():
            self._set_command_error("not connected")
            return {}
        with self._param_lock:
            since: int | None = self._param_rx_seq if fresh else None
            complete = self._param_download_state == "complete"
            known_absent = {
                n for n in wanted
                if n not in self._params and (
                    complete
                    or self._param_misses.get(n, 0) >= PARAM_ABSENT_AFTER_MISSES
                )
            }

        def snapshot() -> dict[str, float]:
            with self._param_lock:
                return {
                    n: self._params[n].value for n in wanted
                    if n in self._params
                    and (since is None or self._params[n].seq > since)
                }

        def still_missing(have: dict[str, float]) -> list[str]:
            return [n for n in wanted if n not in have and n not in known_absent]

        deadline = time.monotonic() + max(0.5, timeout)
        have = snapshot()
        missing = still_missing(have)
        # Two rounds: the initial burst, then one retransmit of whatever is
        # still outstanding halfway through the budget.
        for round_index in range(2):
            if not missing:
                break
            for name in missing:
                # stop() marks the vehicle disconnected before it tears the
                # socket down, so this is also the shutdown bail-out.
                if self._stop_event.is_set() or not self._connection_ready():
                    return snapshot()
                self.request_param(name)
            round_deadline = deadline if round_index else (
                time.monotonic() + max(0.25, (deadline - time.monotonic()) / 2))
            while time.monotonic() < min(round_deadline, deadline):
                have = snapshot()
                missing = still_missing(have)
                if not missing:
                    break
                if self._stop_event.is_set():
                    return have
                time.sleep(0.05)
            have = snapshot()
            missing = still_missing(have)
        # Only a read the vehicle answered in part says anything about the
        # names it did not answer; a read nothing came back from says the link
        # is down, not that the firmware lacks every parameter.
        if missing and any(n in have for n in wanted):
            with self._param_lock:
                for name in missing:
                    self._param_misses[name] = self._param_misses.get(name, 0) + 1
        return have

    def _wait_for_param(self, name: str, timeout: float = 2.0) -> ParamEntry | None:
        """Poll the param cache until *name* appears or *timeout* expires."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self._param_lock:
                entry = self._params.get(name)
            if entry is not None:
                return entry
            time.sleep(0.02)
        return None

    def verify_params(
        self, targets: list[dict[str, Any]], names: list[str] | None = None,
        rewrite: bool = True, timeout: float = 4.0,
    ) -> dict[str, Any] | None:
        """Check that the vehicle holds what was written, and write it again if not.

        *targets* are ``{"name", "value"}`` pairs the operator set; *names* are
        further parameters to read back without judging them (the rest of a
        page, so it can be redrawn from what the vehicle really holds). Every
        name is read fresh from the vehicle, never from the cache. A target
        that differs is written again (at most :data:`PARAM_VERIFY_REWRITES`
        times, never while armed) and read back once more.

        Returns ``{"results", "values", "missing", "confirmed", "failed",
        "all_confirmed"}``: one result per target with ``wanted``, ``before``
        (what the first read found), ``after`` (what the vehicle holds now),
        ``rewritten`` and ``ok``, plus the fresh value of every name that
        answered. None when the request itself is unusable; the reason is the
        last command error.
        """
        self._set_command_error("")
        if not isinstance(targets, list) or (
            names is not None and not isinstance(names, list)
        ):
            self._set_command_error("invalid parameter list")
            return None
        wanted: dict[str, float] = {}
        for entry in targets:
            if not isinstance(entry, dict):
                self._set_command_error("invalid parameter list")
                return None
            name = entry.get("name")
            value = entry.get("value")
            if self._encode_param_name(name) is None:
                self._set_command_error("invalid parameter name")
                return None
            if isinstance(value, bool) or not isinstance(value, (int, float)) \
                    or not math.isfinite(float(value)):
                self._set_command_error(f"{name}: value must be a finite number")
                return None
            wanted[name] = float(value)
        read_names = list(wanted)
        for name in names or []:
            if self._encode_param_name(name) is None:
                self._set_command_error("invalid parameter name")
                return None
            if name not in read_names:
                read_names.append(name)
        if not read_names:
            self._set_command_error("nothing to check")
            return None
        if len(read_names) > PARAM_VERIFY_MAX_NAMES:
            self._set_command_error(
                f"too many parameters ({len(read_names)}); "
                f"the maximum is {PARAM_VERIFY_MAX_NAMES}")
            return None
        if not self._connection_ready():
            self._set_command_error("not connected")
            return None

        values = self.fetch_params(read_names, timeout=timeout, fresh=True)
        results: list[dict[str, Any]] = []
        for name, value in wanted.items():
            before = values.get(name)
            row: dict[str, Any] = {
                "name": name, "wanted": value, "before": before, "after": before,
                "rewritten": False, "ok": False, "error": "",
            }
            results.append(row)
            if before is None:
                row["error"] = "the vehicle did not answer for this parameter"
                continue
            if self._param_matches(name, before, value):
                row["ok"] = True
                continue
            if not rewrite:
                row["error"] = "the vehicle holds a different value"
                continue
            if self._store.get_snapshot().get("armed"):
                row["error"] = "not written again: the vehicle is armed"
                continue
            for _attempt in range(PARAM_VERIFY_REWRITES):
                row["rewritten"] = True
                row["error"] = ""
                if not self.set_param(name, value):
                    row["error"] = self.get_last_command_error() or "write failed"
                    if "not connected" in row["error"]:
                        break
                after = self.fetch_params([name], timeout=2.0, fresh=True).get(name)
                if after is not None:
                    row["after"] = after
                    values[name] = after
                if after is not None and self._param_matches(name, after, value):
                    row["ok"] = True
                    row["error"] = ""
                    break
                if not row["error"]:
                    row["error"] = (
                        "no answer when read back" if after is None
                        else "the vehicle kept a different value")
        confirmed = sum(1 for row in results if row["ok"])
        self._set_command_error("")
        return {
            "results": results,
            "values": values,
            "missing": [n for n in read_names if n not in values],
            "confirmed": confirmed,
            "failed": len(results) - confirmed,
            "all_confirmed": confirmed == len(results),
        }

    def _param_matches(self, name: str, got: float, wanted: float) -> bool:
        """Does *got* (a value read back) equal *wanted*, at the parameter's own precision?"""
        with self._param_lock:
            entry = self._params.get(name)
        ptype = entry.type if entry is not None else mavutil.mavlink.MAV_PARAM_TYPE_REAL32
        return autopilot_dialect.param_values_match(ptype, got, wanted)

    def set_param(self, name: str, value: float) -> bool:
        """Write a parameter and confirm the echoed PARAM_VALUE matches."""
        with self._operation_lock:
            self._set_command_error("")
            if self._encode_param_name(name) is None:
                self._set_command_error("invalid parameter name")
                return False
            # bool is an int in Python: True would be written as 1.0.
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                self._set_command_error("parameter value must be a number")
                return False
            value = float(value)
            if not math.isfinite(value):
                # NaN/Inf reaches PX4 as a valid float32 and is stored as one.
                # The echo check below could never confirm it either (NaN is
                # not equal to itself), so it would report an unconfirmed write
                # for a value that did land on the vehicle.
                self._set_command_error("parameter value must be finite")
                return False
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            # Defense-in-depth: refuse while armed (PX4 also rejects).
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot set parameter while armed")
                return False
            with self._param_lock:
                entry = self._params.get(name)
            if entry is None:
                # Ad-hoc fetch: request then wait for the value to arrive.
                self.request_param(name)
                entry = self._wait_for_param(name, timeout=2.0)
                if entry is None:
                    self._set_command_error("parameter not found on vehicle")
                    return False
            try:
                wire_value = autopilot_dialect.encode_param_value(
                    value, entry.type, self._param_encoding())
            except ValueError as exc:
                self._set_command_error(f"{name}: {exc}")
                return False
            pending = _PendingAck()
            with self._param_lock:
                self._param_set_pending[name] = pending
            try:
                for _attempt in range(3):
                    if not self._connection_ready():
                        self._set_command_error("not connected")
                        return False
                    conn = self._conn
                    try:
                        with self._send_lock:
                            if conn is None or conn is not self._conn:
                                self._set_command_error("not connected")
                                return False
                            conn.mav.param_set_send(
                                self._target_system, self._target_component,
                                name.encode("ascii"), wire_value, entry.type,
                            )
                    except Exception as exc:  # noqa: BLE001 - report, never raise
                        self._set_command_error(f"parameter write failed: {exc}")
                        return False
                    if pending.event.wait(timeout=1.0):
                        # The waiter is woken by an echoed PARAM_VALUE
                        # (result 0) and by a link teardown alike (result -2,
                        # set by _abort_parameter_operations). Only the first
                        # is a write. Treating the second as one meant a cable
                        # pulled mid-write reported the parameter as set
                        # whenever the stale cache still held the value being
                        # written — which is exactly the case where nothing
                        # went out at all.
                        if pending.result == -2:
                            self._set_command_error("not connected")
                            return False
                        break
                else:
                    self._set_command_error("parameter write not confirmed (timeout)")
                    return False
            finally:
                with self._param_lock:
                    if self._param_set_pending.get(name) is pending:
                        self._param_set_pending.pop(name, None)
            with self._param_lock:
                echoed = self._params.get(name)
            if echoed is None:
                self._set_command_error("parameter write not confirmed (no echo)")
                return False
            if autopilot_dialect.param_type_is_integer(echoed.type):
                confirmed = autopilot_dialect.param_values_match(
                    echoed.type, echoed.value, value)
            else:
                confirmed = abs(echoed.value - value) <= max(1e-4, 1e-3 * abs(value))
            if confirmed:
                self._console_publish("PARAM", f"{name} set to {value}", "success")
                return True
            text = (f"parameter write not confirmed (got {echoed.value} expected {value})")
            self._set_command_error(text)
            return False

    def start_param_upload(self, params: list[dict]) -> bool:
        """Apply a saved parameter file to the vehicle as a background upload.

        Validates the list up front, flips the protocol state to ``"uploading"``,
        and spawns one daemon worker that walks the list calling :meth:`set_param`
        (the confirmed-write path). Progress is published over the existing
        param listeners so ``GET /api/params/progress`` emits per-param updates;
        the final tally is read via :meth:`get_param_upload_result`. Mirrors
        :meth:`request_param_list`: the validation+start run under
        ``_operation_lock`` so they cannot race with ``set_param``/``stop``.
        """
        with self._operation_lock:
            self._set_command_error("")
            prior_upload = self._param_upload_thread
            if prior_upload is not None and prior_upload.is_alive():
                self._set_command_error("another parameter operation in progress")
                return False
            if not isinstance(params, list) or not params:
                self._set_command_error("invalid parameter list")
                return False
            for entry in params:
                if not isinstance(entry, dict):
                    self._set_command_error("invalid parameter list")
                    return False
                name = entry.get("name")
                value = entry.get("value")
                if not isinstance(name, str) or not name:
                    self._set_command_error("invalid parameter list")
                    return False
                # bool is a subclass of int — reject it so True is never coerced
                # to 1.0 and silently written to the autopilot.
                if isinstance(value, bool) or not isinstance(value, (int, float)):
                    self._set_command_error("invalid parameter list")
                    return False
            if not self._connection_ready():
                self._set_command_error("not connected")
                return False
            # Defense-in-depth: PX4 also rejects param writes while armed, but
            # refuse up front so the operator gets a clear error instead of N
            # per-param failures from the worker.
            if self._store.get_snapshot().get("armed"):
                self._set_command_error("cannot upload parameters while armed")
                return False
            with self._param_lock:
                if self._param_download_state in ("downloading", "uploading"):
                    self._set_command_error("another parameter operation in progress")
                    return False
                self._param_state_before_upload = self._param_download_state
                self._param_download_state = "uploading"
                self._param_upload_total = len(params)
                self._param_upload_done = 0
                self._param_upload_failed = []
                self._param_upload_result = {}
                self._param_upload_cancel.clear()
            # set_param acquires _operation_lock internally; since this runs on
            # the worker thread (not this one) there is no recursive-lock
            # issue, and we deliberately do NOT hold _operation_lock across
            # the loop — that would block every other command for the whole
            # upload.
            self._param_upload_thread = threading.Thread(
                target=self.set_params_batch, args=(params,),
                name="param-upload", daemon=True,
            )
            self._param_upload_thread.start()
            return True

    def set_params_batch(self, params: list[dict]) -> None:
        """Worker target: write each parameter via the confirmed-write path.

        Runs on the ``param-upload`` daemon thread. Reuses :meth:`set_param`
        so each write inherits the armed-check, cache-lookup, and PARAM_VALUE
        echo confirmation. Per-param progress is pushed to the param listeners
        (the SSE handler reads only ``state``/``count``/``received``); the
        terminal ``"upload_complete"`` status is always emitted, even on error,
        so the UI never waits on a stuck ``"uploading"`` state.
        """
        failed: list[dict[str, Any]] = []
        try:
            for p in params:
                if not self._running.is_set() or self._param_upload_cancel.is_set():
                    break
                name = p["name"]
                value = float(p["value"])
                ok = self.set_param(name, value)
                with self._param_lock:
                    if ok:
                        self._param_upload_done += 1
                    else:
                        # Read the error on this worker thread (set_param sets
                        # the thread-local _command_context here); copy it out
                        # under the lock so the result is self-consistent.
                        failed.append({
                            "name": name,
                            "error": self.get_last_command_error() or "write failed",
                        })
                    status = {
                        "state": self._param_download_state,
                        "count": self._param_upload_total,
                        "received": self._param_upload_done,
                        "name": name,
                        "value": value,
                    }
                self._notify_param_listeners(status)
        except Exception as exc:
            logger.error("param upload failed: %s", exc)
            self._finish_param_upload(failed, interrupted=False)
            return
        interrupted = self._param_upload_cancel.is_set() or not self._running.is_set()
        self._finish_param_upload(failed, interrupted)

    def _finish_param_upload(self, failed: list[dict[str, Any]], interrupted: bool) -> None:
        """Record the tally, restore the set's state, and announce the end.

        The announcement is always sent, and says ``upload_complete`` (or
        ``interrupted``): it is what the editor waits for. The *state* goes
        back to what it was before, because the confirmed writes went into the
        cache: a set that was complete is complete, with the new values, and
        ``GET /api/params`` goes on serving it rather than an empty list until
        the next full download.
        """
        with self._param_lock:
            outcome = "interrupted" if interrupted else "upload_complete"
            before = self._param_state_before_upload
            if interrupted or before not in ("complete", "incomplete"):
                self._param_download_state = "idle"
            else:
                self._param_download_state = before
            self._param_upload_result = {
                "state": outcome,
                "written": self._param_upload_done,
                "failed": len(failed),
                "errors": list(failed),
            }
            final = {
                "state": outcome,
                "count": self._param_upload_total,
                "received": self._param_upload_done,
            }
        self._notify_param_listeners(final)

    def get_param_upload_result(self) -> dict[str, Any]:
        """Return the final tally of the last batch upload.

        ``{"state","written","failed","errors"}`` — idle default before any
        upload has run so the HTTP endpoint can render before a vehicle is
        connected. Copied under ``_param_lock`` so concurrent readers see a
        stable snapshot.
        """
        with self._param_lock:
            if not self._param_upload_result:
                return {"state": "idle", "written": 0, "failed": 0, "errors": []}
            return dict(self._param_upload_result)

    def get_params(self) -> list[dict[str, Any]]:
        """Return cached parameters as sorted ``{"name","value","type"}`` dicts."""
        with self._param_lock:
            return [
                {"name": e.name, "value": e.value, "type": e.type}
                for _, e in sorted(self._params.items())
            ]

    def get_param(self, name: str) -> dict[str, Any] | None:
        """Return a single cached parameter dict or None."""
        with self._param_lock:
            entry = self._params.get(name)
            if entry is None:
                return None
            return {"name": entry.name, "value": entry.value, "type": entry.type}

    def _param_encoding(self) -> str:
        """How this vehicle packs integer parameters: what it declared, else its dialect's."""
        return self._param_encoding_declared or self._dialect.param_encoding

    def param_status(self) -> dict[str, Any]:
        """Return ``{"state","count","received"}`` for the download progress.

        During an upload the two numbers are the upload's own.
        """
        with self._param_lock:
            count, received = self._param_progress_locked()
            return {
                "state": self._param_download_state,
                "count": count,
                "received": received,
            }

    def _param_progress_locked(self) -> tuple[int, int]:
        if self._param_download_state == "uploading":
            return self._param_upload_total, self._param_upload_done
        return self._param_count, self._param_received

    def add_param_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._param_lock:
            self._param_listeners.append(fn)

    def remove_param_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._param_lock:
            try:
                self._param_listeners.remove(fn)
            except ValueError:
                pass

    def _notify_param_listeners(self, status: dict[str, Any]) -> None:
        with self._param_lock:
            listeners = list(self._param_listeners)
        for fn in listeners:
            try:
                fn(status)
            except Exception:
                pass

    def _handle_param_value(self, msg: Any) -> None:
        """Cache a received PARAM_VALUE and notify listeners."""
        try:
            raw_id = msg.param_id
            if isinstance(raw_id, bytes):
                pname = raw_id.split(b"\x00", 1)[0].decode("ascii")
            else:
                pname = str(raw_id).split("\x00", 1)[0]
            ptype = int(msg.param_type)
            pval = autopilot_dialect.decode_param_value(
                float(msg.param_value), ptype, self._param_encoding(),
                raw=_param_value_wire_bytes(msg),
            )
            pindex = int(msg.param_index)
            pcount = int(msg.param_count)
        except (AttributeError, TypeError, ValueError, UnicodeError, struct.error):
            logger.debug("discarding malformed PARAM_VALUE")
            return
        if pname == _PARAM_HASH_CHECK:
            return
        if (
            self._encode_param_name(pname) is None
            or not math.isfinite(pval)
            or not 1 <= ptype <= 10
            or not -1 <= pindex < PARAM_CACHE_MAX_ENTRIES
            or not 0 <= pcount <= PARAM_CACHE_MAX_ENTRIES
            or (pcount > 0 and pindex >= pcount)
        ):
            logger.warning(
                "discarding invalid PARAM_VALUE id=%r index=%s count=%s type=%s",
                pname, pindex, pcount, ptype,
            )
            return
        with self._param_lock:
            is_new = pname not in self._params
            self._param_misses.pop(pname, None)
            if is_new and len(self._params) >= PARAM_CACHE_MAX_ENTRIES:
                logger.warning("parameter cache limit reached; dropping %s", pname)
                return
            if is_new:
                self._param_received += 1
            if pindex >= 0:
                self._param_seen_indices.add(pindex)
            # Last-arrival timestamp feeds the watchdog's inactivity retransmit
            # (BUG 5).
            self._param_last_value_at = time.monotonic()
            self._param_rx_seq += 1
            self._params[pname] = ParamEntry(
                pname, pval, ptype, pindex, pcount, self._param_rx_seq)
            if pcount > 0:
                self._param_count = max(self._param_count, pcount)
            # Never while uploading: the upload's own end decides the state,
            # and the echoes of a whole-file upload onto an empty cache would
            # otherwise declare the set complete halfway through the writes.
            if (self._param_download_state != "uploading"
                    and self._param_count > 0
                    and self._param_received >= self._param_count):
                self._param_download_state = "complete"
            count, received = self._param_progress_locked()
            status = {
                "state": self._param_download_state,
                "count": count,
                "received": received,
                "name": pname,
                "value": pval,
                "type": ptype,
            }
            # Wake a pending set_param waiter if the echoed name matches.
            pending = self._param_set_pending.get(pname)
            if pending is not None:
                pending.result = 0
                pending.event.set()
        self._notify_param_listeners(status)

    # ------------------------------------------------------------------
    # Lossy-link parameter-download recovery (BUG 5)
    # ------------------------------------------------------------------

    def _start_param_watchdog(self) -> None:
        """Spawn the download watchdog (only while the bridge is running)."""
        if not self._running.is_set():
            return
        if self._param_watchdog_thread and self._param_watchdog_thread.is_alive():
            return
        self._param_watchdog_thread = threading.Thread(
            target=self._param_watchdog, name="param-watchdog", daemon=True,
        )
        self._param_watchdog_thread.start()

    def _finish_param_download_incomplete(self) -> None:
        """Flip a stuck download to a terminal "incomplete" (partial params kept)."""
        with self._param_lock:
            if self._param_download_state != "downloading":
                return
            self._param_download_state = "incomplete"
            status = {
                "state": "incomplete",
                "count": self._param_count,
                "received": self._param_received,
            }
        self._notify_param_listeners(status)

    def _param_download_budget_s(self) -> float:
        """How long a full download may take before it is called incomplete."""
        if self._is_slow_link():
            return PARAM_DOWNLOAD_TIMEOUT_S_SERIAL
        return PARAM_DOWNLOAD_TIMEOUT_S

    def _param_watchdog(self) -> None:
        """Re-request lost PARAM_VALUEs and time out a stuck download (BUG 5).

        PX4 bursts the full parameter list on PARAM_REQUEST_LIST; on a lossy
        link some PARAM_VALUEs are lost and _param_received never reaches
        _param_count, leaving the editor stuck at complete:false. This
        watchdog wakes periodically and, once the burst has settled (no new
        PARAM_VALUE for PARAM_DOWNLOAD_INACTIVITY_S), re-requests the missing
        indices via param_request_read_send. After PARAM_DOWNLOAD_MAX_ROUNDS
        or PARAM_DOWNLOAD_TIMEOUT_S it flips the state to a terminal
        "incomplete" (partial params kept) so the UI never waits forever.
        Bounded: never loops past the rounds/timeout caps.
        """
        while self._running.is_set() and not self._stop_event.is_set():
            self._interruptible_sleep(PARAM_WATCHDOG_TICK_S)
            with self._param_lock:
                state = self._param_download_state
                if state != "downloading":
                    return
                count = self._param_count
                received = self._param_received
                seen = set(self._param_seen_indices)
                started_at = self._param_download_started_at
                last_value = self._param_last_value_at
                round_ = self._param_retransmit_round
            if count <= 0:
                continue
            if received >= count:
                return
            now = time.monotonic()
            if now - started_at > self._param_download_budget_s():
                self._finish_param_download_incomplete()
                return
            if now - last_value < PARAM_DOWNLOAD_INACTIVITY_S:
                continue
            if round_ >= PARAM_DOWNLOAD_MAX_ROUNDS:
                self._finish_param_download_incomplete()
                return
            missing = set(range(count)) - seen
            if not missing:
                return
            with self._param_lock:
                self._param_retransmit_round = round_ + 1
            conn = self._conn
            if conn is None or conn is not self._conn:
                return
            # Re-request the missing indices by index (name=b"", index=idx),
            # capped and paced so the recovery cannot saturate the very link
            # whose losses it is recovering from. The inactivity gate above
            # means this round's replies have to arrive (or not) before
            # another round is scheduled, so what is left over is not lost.
            if self._is_slow_link():
                budget = PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL
                gap = PARAM_RETRANSMIT_GAP_S_SERIAL
            else:
                budget = PARAM_RETRANSMIT_MAX_PER_ROUND_UDP
                gap = PARAM_RETRANSMIT_GAP_S_UDP
            for idx in sorted(missing)[:budget]:
                if self._stop_event.is_set():
                    return
                try:
                    with self._send_lock:
                        if conn is not self._conn:
                            return
                        conn.mav.param_request_read_send(
                            self._target_system, self._target_component,
                            b"", idx,
                        )
                except Exception as exc:
                    logger.debug("param retransmit idx=%d failed: %s", idx, exc)
                # Outside the send lock: the point is to leave the link free
                # between requests, not to hold it while waiting.
                self._interruptible_sleep(gap)
