"""The MAVLink parameter protocol: download, read, write, verify.

Mixed into :class:`corvus.mavlink_bridge.MavlinkBridge`. The protocol itself
(the cache, the download state machine, PARAM_VALUE checks and decoding, the
lossy-link watchdog, named reads, confirmed writes, verify and the batch
upload) is ``corvus_link.params.ParamEngine``, in Rust. This mixin gives it
the bridge's locks, link, threads and listeners, and keeps the methods and
attributes the rest of Corvus and the tests have always used.
Nothing here imports the bridge, so the bridge can import this.
"""
from __future__ import annotations

import logging
import threading
from typing import Any
from collections.abc import Callable

from corvus_link import params as _engine

logger = logging.getLogger("corvus.mavlink")

# The protocol's limits live with the engine; these are its numbers.
#
# Lossy-link parameter-download recovery (BUG 5). After the PARAM_VALUE burst
# settles, re-request missing indices (bounded rounds), then give up to a
# terminal "incomplete" state so the editor is never stuck at complete:false.
# Over a telemetry radio the budget is 180 s rather than 30: PX4's ~1400
# parameters are ~35 kB, which a 57 kbps link cannot carry in 30 s.
PARAM_DOWNLOAD_INACTIVITY_S = _engine.PARAM_DOWNLOAD_INACTIVITY_S
PARAM_DOWNLOAD_MAX_ROUNDS = _engine.PARAM_DOWNLOAD_MAX_ROUNDS
PARAM_DOWNLOAD_TIMEOUT_S = _engine.PARAM_DOWNLOAD_TIMEOUT_S
PARAM_DOWNLOAD_TIMEOUT_S_SERIAL = _engine.PARAM_DOWNLOAD_TIMEOUT_S_SERIAL
PARAM_WATCHDOG_TICK_S = _engine.PARAM_WATCHDOG_TICK_S
# Retransmit pacing: a round asks for at most this many lost values, spaced
# so the replies interleave with telemetry instead of saturating a SiK radio.
PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL = _engine.PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL
PARAM_RETRANSMIT_MAX_PER_ROUND_UDP = _engine.PARAM_RETRANSMIT_MAX_PER_ROUND_UDP
PARAM_RETRANSMIT_GAP_S_SERIAL = _engine.PARAM_RETRANSMIT_GAP_S_SERIAL
PARAM_RETRANSMIT_GAP_S_UDP = _engine.PARAM_RETRANSMIT_GAP_S_UDP
# Bounds the cache and the watchdog against an invalid or hostile count.
PARAM_CACHE_MAX_ENTRIES = _engine.PARAM_CACHE_MAX_ENTRIES
# PARAM_VALUE.param_index for a value that names no index (uint16 -1).
PARAM_INDEX_NONE = _engine.PARAM_INDEX_NONE

# AUTOPILOT_VERSION.capabilities bits that say how integer parameters travel.
# Numbers rather than names: older dialects call the first one PARAM_UNION and
# lack the second.
_CAP_PARAM_ENCODE_BYTEWISE = 16
_CAP_PARAM_ENCODE_C_CAST = 131072

# PX4's pseudo parameter at index -1 (the hash of all values); not counted.
_PARAM_HASH_CHECK = _engine.PARAM_HASH_CHECK

# How many parameters one verify request may name, and how often one that did
# not stick is written again before the check gives up on it.
PARAM_VERIFY_MAX_NAMES = _engine.PARAM_VERIFY_MAX_NAMES
PARAM_VERIFY_REWRITES = _engine.PARAM_VERIFY_REWRITES

# Named reads a parameter may go unanswered in, while the vehicle answers
# others, before it counts as absent on this firmware for the session.
PARAM_ABSENT_AFTER_MISSES = _engine.PARAM_ABSENT_AFTER_MISSES

ParamEntry = _engine.ParamEntry


def _engine_attribute(name: str) -> property:
    """A bridge attribute the parameter state used to be, now the engine's."""
    def get(self: Any) -> Any:
        return getattr(self._param_engine, name)

    def set_(self: Any, value: Any) -> None:
        setattr(self._param_engine, name, value)

    return property(get, set_)


class ParamProtocolMixin:
    """PARAM_REQUEST_LIST / _READ / PARAM_SET / PARAM_VALUE, and the checks on top."""

    # PARAM_VALUE / PARAM_SET carry the id in a fixed char[16].
    PARAM_ID_MAX_BYTES = _engine.PARAM_ID_MAX_BYTES

    # The state the bridge used to hold itself, kept addressable by its old
    # names: the HTTP layer, the shutdown path and the tests read them.
    _params = property(lambda self: self._param_engine.cache)
    _param_download_state = _engine_attribute("state")
    _param_count = _engine_attribute("count")
    _param_received = _engine_attribute("received")
    _param_seen_indices = _engine_attribute("seen_indices")
    _param_rx_seq = _engine_attribute("rx_seq")
    _param_misses = _engine_attribute("misses")
    _param_download_started_at = _engine_attribute("download_started_at")
    _param_retransmit_round = _engine_attribute("retransmit_round")
    _param_last_value_at = _engine_attribute("last_value_at")
    _param_upload_total = _engine_attribute("upload_total")
    _param_upload_done = _engine_attribute("upload_done")
    _param_state_before_upload = _engine_attribute("state_before_upload")

    def _init_param_state(self) -> None:
        """Set up the parameter state; called from the bridge's __init__."""
        # Parameter encoding the vehicle declared in AUTOPILOT_VERSION, or ""
        # until it does; the dialect's default applies meanwhile.
        self._param_encoding_declared = ""
        self._param_engine = _engine.ParamEngine()
        self._param_watchdog_thread: threading.Thread | None = None
        self._param_lock = threading.Lock()
        self._param_listeners: list[Callable[[dict[str, Any]], None]] = []
        self._param_upload_thread: threading.Thread | None = None

    def _abort_parameter_operations(self) -> None:
        """Wake parameter waiters and prevent an upload crossing a reconnect.

        The download state goes back to idle whatever it was: "complete"
        means *this vehicle has given us its parameters*, which stops being a
        claim Corvus can make the moment the link is gone.
        """
        self._param_engine.abort()

    def _reset_parameter_cache(self) -> None:
        """Discard vehicle-specific parameter metadata for a new link cycle."""
        self._param_engine.reset()

    # ------------------------------------------------------------------
    # Parameter protocol
    # ------------------------------------------------------------------

    def request_param_list(self) -> bool:
        """Start a full parameter download from the vehicle.

        Lazy, operator-triggered. Returns False when disconnected or another
        parameter operation is in progress.
        """
        with self._operation_lock:
            return self._param_engine.request_param_list(self)

    @classmethod
    def _encode_param_name(cls, name: Any) -> bytes | None:
        """Encode *name* for the wire, or None when it cannot be one.

        Rejects instead of truncating: a shortened name addresses a
        *different* parameter.
        """
        return _engine.encode_param_name(name)

    def request_param(self, name: str) -> bool:
        """Request a single parameter by name (param_index=-1)."""
        with self._operation_lock:
            return self._param_engine.request_param(self, name)

    def fetch_params(
        self, names: list[str], timeout: float = 4.0, fresh: bool = False,
    ) -> dict[str, float]:
        """Read a named set of parameters without a full parameter download.

        Asks for the missing names in one burst (one retransmit round for the
        stragglers) and returns ``{name: value}`` for whatever arrived before
        *timeout*. A name the vehicle never answers is a missing key, never an
        error. *fresh* counts only values that arrive after the call started.
        A name known absent on this vehicle (not in a complete download, or
        unanswered in :data:`PARAM_ABSENT_AFTER_MISSES` partly answered reads)
        is not asked for again. A round ends at a barrier read of a known
        parameter, once the vehicle has visibly worked through the burst.
        """
        return self._param_engine.fetch_params(self, names, timeout, fresh)

    def _wait_for_param(self, name: str, timeout: float = 2.0) -> ParamEntry | None:
        """Poll the param cache until *name* appears or *timeout* expires."""
        return self._param_engine.wait_for_param(name, timeout)

    def verify_params(
        self, targets: list[dict[str, Any]], names: list[str] | None = None,
        rewrite: bool = True, timeout: float = 4.0,
    ) -> dict[str, Any] | None:
        """Check that the vehicle holds what was written, and write it again if not.

        *targets* are ``{"name", "value"}`` pairs the operator set; *names* are
        further parameters to read back without judging them. Every name is
        read fresh. A target that differs is written again (at most
        :data:`PARAM_VERIFY_REWRITES` times, never while armed) and read back.

        Returns ``{"results", "values", "missing", "confirmed", "failed",
        "all_confirmed"}``, or None when the request itself is unusable; the
        reason is the last command error.
        """
        return self._param_engine.verify_params(self, targets, names, rewrite, timeout)

    def _param_matches(self, name: str, got: float, wanted: float) -> bool:
        """Does *got* (a value read back) equal *wanted*, at the parameter's own precision?"""
        return self._param_engine.matches(name, got, wanted)

    def set_param(self, name: str, value: float) -> bool:
        """Write a parameter and confirm the echoed PARAM_VALUE matches."""
        with self._operation_lock:
            return self._param_engine.set_param(self, name, value)

    @staticmethod
    def _echo_confirms(echoed: ParamEntry, value: float) -> bool:
        """Does an echoed PARAM_VALUE carry the value that was written?"""
        return _engine.ParamEngine.echo_confirms(echoed, value)

    def start_param_upload(self, params: list[dict]) -> bool:
        """Apply a saved parameter file to the vehicle as a background upload.

        Validates the list, flips the protocol state to ``"uploading"`` and
        starts one daemon worker that writes each parameter through
        :meth:`set_param`. Progress goes to the param listeners; the tally is
        :meth:`get_param_upload_result`.
        """
        with self._operation_lock:
            self._set_command_error("")
            prior_upload = self._param_upload_thread
            if prior_upload is not None and prior_upload.is_alive():
                self._set_command_error("another parameter operation in progress")
                return False
            if not self._param_engine.begin_upload(self, params):
                return False
            # set_param takes _operation_lock itself, on the worker thread; it
            # is not held across the loop, which would block every command.
            param_upload_thread = threading.Thread(
                target=self.set_params_batch, args=(params,),
                name="param-upload", daemon=True,
            )
            param_upload_thread.start()
            self._param_upload_thread = param_upload_thread
            return True

    def set_params_batch(self, params: list[dict]) -> None:
        """Worker target: write each parameter via the confirmed-write path.

        The terminal ``"upload_complete"`` (or ``"interrupted"``) status is
        always announced, so the UI never waits on a stuck ``"uploading"``.
        """
        self._param_engine.run_upload(self, params)

    def get_param_upload_result(self) -> dict[str, Any]:
        """Return the final tally of the last batch upload.

        ``{"state","written","failed","errors"}``, idle before any upload ran.
        """
        return self._param_engine.upload_result()

    def get_params(self) -> list[dict[str, Any]]:
        """Return cached parameters as sorted ``{"name","value","type"}`` dicts."""
        return self._param_engine.get_params()

    def get_param(self, name: str) -> dict[str, Any] | None:
        """Return a single cached parameter dict or None."""
        return self._param_engine.get_param(name)

    def param_file_info(self) -> dict[str, Any]:
        """What a parameter file written for this vehicle needs to say about it.

        ``format`` is the file the connected stack's users exchange (see
        :mod:`corvus.param_files`); the rest fills QGroundControl's header and
        the system and component columns it checks on import.
        """
        snapshot = self._store.get_snapshot()
        return {
            "format": self._dialect.param_file_format,
            "system_id": int(self._target_system or 1),
            "component_id": int(self._target_component or 1),
            "stack": self._dialect.label,
            "vehicle": str(snapshot.get("vehicle_type") or ""),
            "version": str(snapshot.get("px4_version") or ""),
            "git_hash": str(snapshot.get("px4_version_detail") or ""),
        }

    def _param_encoding(self) -> str:
        """How this vehicle packs integer parameters: what it declared, else its dialect's."""
        return self._param_encoding_declared or self._dialect.param_encoding

    def param_status(self) -> dict[str, Any]:
        """Return ``{"state","count","received"}`` for the download progress.

        During an upload the two numbers are the upload's own.
        """
        return self._param_engine.status()

    def _param_progress_locked(self) -> tuple[int, int]:
        return self._param_engine.progress_tuple()

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
        self._param_engine.handle_param_value(self, msg)

    # ------------------------------------------------------------------
    # Lossy-link parameter-download recovery (BUG 5)
    # ------------------------------------------------------------------

    def _start_param_watchdog(self) -> None:
        """Spawn the download watchdog (only while the bridge is running)."""
        if not self._running.is_set():
            return
        if self._param_watchdog_thread and self._param_watchdog_thread.is_alive():
            return
        param_watchdog_thread = threading.Thread(
            target=self._param_watchdog, name="param-watchdog", daemon=True,
        )
        param_watchdog_thread.start()
        self._param_watchdog_thread = param_watchdog_thread

    def _finish_param_download_incomplete(self) -> None:
        """Flip a stuck download to a terminal "incomplete" (partial params kept)."""
        self._param_engine.finish_download_incomplete(self)

    def _param_download_budget_s(self) -> float:
        """How long a full download may take before it is called incomplete."""
        if self._is_slow_link():
            return PARAM_DOWNLOAD_TIMEOUT_S_SERIAL
        return PARAM_DOWNLOAD_TIMEOUT_S

    def _param_watchdog(self) -> None:
        """Re-request lost PARAM_VALUEs and time out a stuck download (BUG 5).

        Once the burst has settled (no PARAM_VALUE for
        PARAM_DOWNLOAD_INACTIVITY_S) the missing indices are asked for again,
        capped and paced per round. After PARAM_DOWNLOAD_MAX_ROUNDS or the
        download budget the state becomes a terminal "incomplete".
        """
        self._param_engine.run_watchdog(self)
