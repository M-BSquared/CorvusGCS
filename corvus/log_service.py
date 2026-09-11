"""Flight-log download: on-board ULogs over MAVLink, plus the local tlogs.

Two kinds of log matter after a flight and they live in different places:

* **ULog** — PX4's own high-rate log, on the flight controller's SD card. It
  comes off the vehicle over the MAVLink ``LOG_*`` protocol, which is a
  request/response chunk protocol with no ACK and no built-in retry.
* **tlog** — the MAVLink stream as Corvus recorded it, already on this laptop
  under ``tlog_dir``. Nothing to download; it is copied into the same folder as
  the ULogs so one flight's evidence ends up in one place.

Downloads are **sequential by design**. The MAVLink log protocol has one
session per vehicle: two concurrent downloads interleave their LOG_DATA and
corrupt both files. So a multi-select becomes a queue on one worker thread,
which is also what the operator wants — start it, walk away, come back to a
folder of logs.

Lifecycle: one daemon worker, cancellable, joined by :meth:`shutdown`. The
MAVLink sink is attached only while a listing or download is running and is
always detached in a ``finally``, so a torn-down service never keeps the
receive loop calling into it.
"""
from __future__ import annotations

import errno
import logging
import os
import re
import threading
import time
from typing import Any, Callable, TYPE_CHECKING

if TYPE_CHECKING:  # avoid an import cycle at runtime
    from .mavlink_bridge import MavlinkBridge

logger = logging.getLogger("corvus.logs")

# One LOG_DATA carries 90 bytes. Asking for a large span and letting the
# vehicle stream it is far faster than a chunk-per-request round trip.
LOG_CHUNK_BYTES = 90
REQUEST_SPAN_BYTES = 90 * 80          # ~7 kB in flight per request
LIST_TIMEOUT_S = 6.0                  # no LOG_ENTRY for this long = list done
LIST_RETRY_S = 1.5                    # still nothing: ask again
LIST_ATTEMPTS = 4                     # LOG_REQUEST_LIST has no ACK to wait on
DATA_STALL_TIMEOUT_S = 4.0            # no LOG_DATA at all = re-request
DATA_QUIET_S = 0.7                    # stream went quiet mid-span = re-request
DOWNLOAD_GIVE_UP_S = 60.0             # no progress at all for this long = fail
STALL_REPRIME_AFTER = 2               # dead spans before re-opening the session
PRIME_TIMEOUT_S = 3.0                 # wait for the session-opening LOG_ENTRY
MAX_LOG_BYTES = 2 * 1024 * 1024 * 1024
# States in which the service owns the vehicle's single log session and must
# refuse a second job.
BUSY_STATES = frozenset({"listing", "downloading", "erasing"})
# The vehicle needs a moment to clear the SD card before a re-list is honest.
ERASE_SETTLE_S = 2.0


# ``20260910-143205-123456`` (plus an optional ``_02`` de-collision suffix) —
# the name MavlinkBridge._next_tlog_path writes, in this machine's LOCAL time.
_TLOG_NAME_RE = re.compile(
    r"^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})(?:-(\d{1,6}))?"
)


def _tlog_started(name: str, fallback: float) -> float:
    """When a tlog's recording opened, as an epoch timestamp.

    The stamp in the name is local wall-clock, so it is turned back into an
    epoch through ``mktime`` rather than read as UTC — off by the machine's
    offset otherwise, which is exactly the confusion this is meant to end.
    A DST fold makes one hour a year ambiguous; ``mktime`` picks one, and an
    hour's slip on a sort key is not worth carrying a tz database for.
    """
    match = _TLOG_NAME_RE.match(name)
    if not match:
        return float(fallback)
    year, month, day, hour, minute, second = (int(g) for g in match.groups()[:6])
    try:
        stamp = time.mktime(
            (year, month, day, hour, minute, second, 0, 1, -1)
        )
    except (OverflowError, ValueError):
        return float(fallback)
    micro = match.group(7)
    if micro:
        stamp += int(micro.ljust(6, "0")) / 1_000_000.0
    return stamp


def _safe_component(value: Any) -> str:
    """A filename fragment that cannot escape the target directory."""
    text = "".join(c if (c.isalnum() or c in "-_.") else "-" for c in str(value or ""))
    return text.strip("-.") or "log"


class LogService:
    """Lists and downloads flight logs. One job at a time, always cancellable."""

    def __init__(
        self,
        mavlink: "MavlinkBridge",
        log_dir: Callable[[], str],
        tlog_dir: Callable[[], str] | None = None,
    ) -> None:
        self.mavlink = mavlink
        self._log_dir = log_dir
        self._tlog_dir = tlog_dir
        self._lock = threading.Lock()
        self._state = "idle"          # idle | listing | downloading | erasing
                                      # | done | failed | cancelled
        self._message = ""
        self._percent = 0
        self._entries: dict[int, dict[str, Any]] = {}
        self._listed_at = 0.0
        self._queue: list[int] = []
        self._current: int | None = None
        self._completed: list[dict[str, Any]] = []
        self._cancel = threading.Event()
        self._worker: threading.Thread | None = None
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        self._shutting_down = False
        # Download assembly state, owned by the worker and written by the sink.
        self._buffer = bytearray()
        self._got_data = threading.Event()
        self._last_offset = 0
        self.last_error = ""

    # ------------------------------------------------------------------
    # Status
    # ------------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Everything the Analysis page renders, in one snapshot."""
        with self._lock:
            entries = [dict(e) for e in self._entries.values()]
            state = self._state
            message = self._message
            percent = self._percent
            current = self._current
            queued = list(self._queue)
            completed = [dict(c) for c in self._completed]
        entries.sort(key=lambda e: e.get("id", 0), reverse=True)
        saved = self.saved_logs()
        by_id = {int(f["id"]): f for f in saved if f.get("id") is not None}
        for entry in entries:
            match = by_id.get(int(entry.get("id", -1)))
            # Same id AND same size. An id alone is not enough: the SD card
            # recycles them, so last month's log_003 can share a number with
            # today's. Claiming a stale file is this flight's log would send an
            # operator home with the wrong evidence.
            entry["downloaded"] = bool(match and match["size"] == entry.get("size"))
            entry["file"] = match["path"] if entry["downloaded"] else ""
            # The bare name as well as the path: it is what the review endpoint
            # takes, and deriving it in the browser would mean guessing at this
            # machine's path separator.
            entry["file_name"] = match["name"] if entry["downloaded"] else ""
        return {
            "state": state,
            "message": message,
            "percent": percent,
            "current": current,
            "queued": queued,
            "completed": completed,
            "logs": entries,
            "saved": saved,
            "tlogs": self.local_tlogs(),
            "dir": self._resolve_dir(),
            "connected": bool(self.mavlink is not None and self.mavlink.is_connected()),
        }

    def saved_logs(self) -> list[dict[str, Any]]:
        """ULogs already in the download folder, newest first.

        The id is recovered from the ``log_<id>_<date>.ulg`` name this service
        writes; a file that does not follow it is still listed (an operator may
        have dropped one in) but carries no id, so it is never matched against
        a log on the vehicle.
        """
        out: list[dict[str, Any]] = []
        directory = self._resolve_dir()
        if not directory:
            return out
        try:
            names = os.listdir(directory)
        except OSError:
            return out
        for name in names:
            if not name.endswith(".ulg"):
                continue
            path = os.path.join(directory, name)
            try:
                stat = os.stat(path)
            except OSError:
                continue
            match = re.match(r"^log_(\d+)", name)
            out.append({
                "name": name,
                "id": int(match.group(1)) if match else None,
                "size": stat.st_size,
                "mtime": stat.st_mtime,
                "path": path,
            })
        out.sort(key=lambda f: f["mtime"], reverse=True)
        return out

    def _resolve_dir(self) -> str:
        try:
            return self._log_dir() or ""
        except Exception:  # noqa: BLE001
            return ""

    def resolve_tlog_dir(self) -> str:
        """Where the recordings live, or "" when nothing is configured.

        Public because the review endpoint has to resolve a filename inside
        this folder and check it did not escape — the same confinement the
        ULog review does against the download folder.
        """
        try:
            return (self._tlog_dir() if self._tlog_dir else "") or ""
        except Exception:  # noqa: BLE001
            return ""

    def local_tlogs(self) -> list[dict[str, Any]]:
        """tlogs Corvus already recorded on this laptop, newest session first.

        Each entry carries both ends of the session: ``started`` is when the
        recording opened, ``mtime`` when the last frame was written. The start
        is the one an operator sorts and searches by — "the flight at half two"
        is when it took off, not when the link finally dropped — so it is read
        from the recorder's own filename rather than from the file's mtime.
        A file that does not follow that name (one dropped into the folder by
        hand) falls back to its mtime, which is the only honest answer left.
        """
        out: list[dict[str, Any]] = []
        directory = self.resolve_tlog_dir()
        if not directory:
            return out
        try:
            names = os.listdir(directory)
        except OSError:
            return out
        for name in names:
            if not name.endswith(".tlog"):
                continue
            path = os.path.join(directory, name)
            try:
                stat = os.stat(path)
            except OSError:
                continue
            out.append({
                "name": name, "size": stat.st_size,
                "started": _tlog_started(name, stat.st_mtime),
                "mtime": stat.st_mtime, "path": path,
            })
        out.sort(key=lambda f: f["started"], reverse=True)
        return out

    # ------------------------------------------------------------------
    # Listing
    # ------------------------------------------------------------------

    def refresh(self) -> bool:
        """Ask the vehicle what logs it has. False on refusal."""
        if self.mavlink is None or not self.mavlink.is_connected():
            self.last_error = "not connected"
            return False
        with self._lock:
            if self._state in BUSY_STATES:
                self.last_error = "a log operation is already running"
                return False
        self._cancel.clear()
        self._set("listing", 0, "Asking the vehicle for its logs…")
        self._worker = threading.Thread(target=self._run_list, name="log-list", daemon=True)
        self._worker.start()
        return True

    def _run_list(self) -> None:
        seen: dict[int, dict[str, Any]] = {}
        # Every id the vehicle named, empty slots included. The completion test
        # has to count those too: PX4's num_logs covers the whole log directory,
        # so measuring it against the non-empty entries alone never adds up and
        # every listing sat out the full timeout instead of finishing.
        heard: set[int] = set()
        expected = 0
        last_seen = 0.0               # 0.0 = the vehicle has not said anything
        # Wakes the retry wait the instant the vehicle answers, so a listing
        # costs one round trip rather than a full retry interval.
        answered = threading.Event()

        def sink(msg: Any) -> None:
            nonlocal last_seen, expected
            if msg.get_type() != "LOG_ENTRY":
                return
            last_seen = time.monotonic()
            answered.set()
            try:
                log_id = int(msg.id)
                size = int(msg.size)
            except (TypeError, ValueError):
                return
            heard.add(log_id)
            expected = int(getattr(msg, "num_logs", 0) or 0) or expected
            # PX4 reports every slot including empty ones; an empty log is not
            # something the operator can do anything with.
            if size <= 0:
                return
            seen[log_id] = {
                "id": log_id,
                "size": size,
                "utc": int(getattr(msg, "time_utc", 0) or 0),
                "num_logs": expected,
            }

        attempts = 0
        try:
            self.mavlink.set_log_sink(sink)
            while not self._cancel.is_set():
                if last_seen == 0.0:
                    # LOG_REQUEST_LIST is unacknowledged, so a single dropped
                    # request is indistinguishable from an empty SD card. Ask
                    # again rather than report "no logs" on one lost packet —
                    # that answer sends an operator home without the flight.
                    if attempts >= LIST_ATTEMPTS:
                        break
                    if not self.mavlink.request_log_list():
                        self._set("failed", 0, "could not ask the vehicle for its logs")
                        return
                    attempts += 1
                    deadline = time.monotonic() + LIST_RETRY_S
                    while time.monotonic() < deadline and not self._cancel.is_set():
                        if answered.wait(0.05):
                            break
                    continue
                if expected and len(heard) >= expected:
                    break
                if time.monotonic() - last_seen > LIST_TIMEOUT_S:
                    break
                self._cancel.wait(0.1)
        except Exception:  # noqa: BLE001 - the worker must never propagate
            logger.exception("log listing crashed")
            self._set("failed", 0, "log listing failed unexpectedly")
            return
        finally:
            self.mavlink.set_log_sink(None)
            # Deliberately no log_request_end() here. LOG_REQUEST_END closes the
            # vehicle's log session, and PX4 answers LOG_REQUEST_DATA only while
            # one is open (v1.15 drops to Inactive and ignores the request
            # outright). Ending the session the moment the list arrived is what
            # made every subsequent download return silence. The session is
            # closed when the downloads finish, or on shutdown.

        with self._lock:
            self._entries = seen
            self._listed_at = time.time()
        if self._cancel.is_set():
            self._set("cancelled", 0, "Listing cancelled")
            return
        if last_seen == 0.0:
            # Silence is genuinely ambiguous: PX4 sends nothing at all for an
            # empty SD card, and nothing at all for a request that never
            # arrived. The two are indistinguishable on the wire, which is why
            # the answer is the retry above rather than a cleverer message —
            # after several unanswered requests, empty is the honest reading,
            # and calling it a failure would put a fault on the page every time
            # an operator checks a vehicle whose card really is empty. The
            # count goes to the log so a field diagnosis still has it.
            logger.info("no LOG_ENTRY after %d requests; reporting an empty vehicle",
                        attempts)
        if not seen:
            self._set("idle", 0, "No logs on the vehicle")
        else:
            self._set("idle", 0, f"{len(seen)} log(s) on the vehicle")

    def _prime_session(self) -> bool:
        """Re-open the vehicle's log session, and confirm it with a LOG_ENTRY.

        PX4 serves LOG_REQUEST_DATA only after a LOG_REQUEST_LIST has put its
        log handler into a listed state, and it falls out of that state on
        LOG_REQUEST_END, on a reboot, and on a link cycle. It says nothing when
        it does — a data request against a closed session is dropped in silence.
        So a download re-opens the session itself instead of trusting the one
        the last listing left behind, which is the difference between a
        download that runs and one that waits for bytes that will never come.
        """
        heard = threading.Event()

        def sink(msg: Any) -> None:
            if msg.get_type() == "LOG_ENTRY":
                heard.set()

        self.mavlink.set_log_sink(sink)
        try:
            for _ in range(LIST_ATTEMPTS):
                if self._cancel.is_set():
                    return False
                if not self.mavlink.request_log_list():
                    return False
                deadline = time.monotonic() + PRIME_TIMEOUT_S
                while time.monotonic() < deadline:
                    if heard.is_set():
                        return True
                    if self._cancel.is_set():
                        return False
                    heard.wait(0.1)
            return heard.is_set()
        finally:
            self.mavlink.set_log_sink(None)

    # ------------------------------------------------------------------
    # Download
    # ------------------------------------------------------------------

    def start_download(self, ids: list[int]) -> bool:
        """Queue one or more logs and download them one after another.

        Sequential, not parallel: the MAVLink log protocol has a single session
        per vehicle, so concurrent downloads interleave their LOG_DATA and
        corrupt each other.
        """
        if self.mavlink is None or not self.mavlink.is_connected():
            self.last_error = "not connected"
            return False
        with self._lock:
            if self._state in BUSY_STATES:
                self.last_error = "a log operation is already running"
                return False
            wanted = [int(i) for i in ids if int(i) in self._entries]
            if not wanted:
                self.last_error = "no known log selected"
                return False
            self._queue = wanted
            self._completed = []
        directory = self._resolve_dir()
        if not directory:
            self.last_error = "no download folder configured"
            return False
        try:
            os.makedirs(directory, exist_ok=True)
        except OSError as exc:
            self.last_error = f"cannot use the download folder ({exc.strerror or exc})"
            return False
        self._cancel.clear()
        self._set("downloading", 0, f"Downloading {len(wanted)} log(s)…")
        self._worker = threading.Thread(target=self._run_download, name="log-download", daemon=True)
        self._worker.start()
        return True

    def _run_download(self) -> None:
        directory = self._resolve_dir()
        try:
            # Open the session before asking for a single byte. The listing
            # that filled the picker may be minutes old, and the vehicle keeps
            # no session across a reboot or a link cycle.
            if not self._prime_session():
                if not self._cancel.is_set():
                    self._set("failed", 0, "the vehicle would not open a log session")
                return
            while not self._cancel.is_set():
                with self._lock:
                    if not self._queue:
                        break
                    log_id = self._queue.pop(0)
                    self._current = log_id
                    entry = dict(self._entries.get(log_id) or {})
                    remaining = len(self._queue)
                if not entry:
                    continue
                ok, detail = self._download_one(log_id, entry, directory, remaining)
                with self._lock:
                    self._completed.append({
                        "id": log_id, "ok": ok, "detail": detail,
                    })
                if not ok and not self._cancel.is_set():
                    # Keep going: one unreadable log should not abandon the rest
                    # of a queue the operator left running.
                    logger.info("log %d failed: %s", log_id, detail)
        except Exception:  # noqa: BLE001
            logger.exception("log download worker crashed")
            self._set("failed", self._percent, "log download failed unexpectedly")
            return
        finally:
            self.mavlink.set_log_sink(None)
            self.mavlink.log_request_end()
            with self._lock:
                self._current = None

        with self._lock:
            done = [c for c in self._completed if c["ok"]]
            failed = [c for c in self._completed if not c["ok"]]
        if self._cancel.is_set():
            self._set("cancelled", self._percent,
                      f"Cancelled — {len(done)} log(s) saved")
        elif failed:
            self._set("failed", 100,
                      f"{len(done)} saved, {len(failed)} failed")
        else:
            self._set("done", 100, f"{len(done)} log(s) saved to {directory}")

    def _download_one(
        self, log_id: int, entry: dict[str, Any], directory: str, remaining: int,
    ) -> tuple[bool, str]:
        """Pull one log off the vehicle and write it. Returns (ok, detail)."""
        size = int(entry.get("size") or 0)
        if size <= 0 or size > MAX_LOG_BYTES:
            return False, "implausible log size"
        name = self._filename(log_id, entry)
        path = os.path.join(directory, name)

        self._buffer = bytearray(size)
        received = bytearray(size)          # 0 = still missing, 1 = held
        # Written by the receive thread, read by this one. A bare assignment is
        # atomic enough under the GIL, and a lock here would sit in the path of
        # every 90-byte packet on the link.
        flow = {"at": 0.0}

        def sink(msg: Any) -> None:
            if msg.get_type() != "LOG_DATA" or int(msg.id) != log_id:
                return
            offset = int(msg.ofs)
            data = bytes(msg.data)[:int(msg.count)]
            end = min(offset + len(data), size)
            if offset >= size or end <= offset:
                return
            self._buffer[offset:end] = data[:end - offset]
            # Slice-assign the ledger too. Marking it a byte at a time ran a
            # 90-iteration Python loop on the receive thread for every packet,
            # which on a log of any real size starved the whole link.
            received[offset:end] = b"\x01" * (end - offset)
            flow["at"] = time.monotonic()
            self._got_data.set()

        try:
            self.mavlink.set_log_sink(sink)
            last_progress = time.monotonic()
            stalls = 0
            reprimes_left = 1
            offset = 0
            while offset < size:
                if self._cancel.is_set():
                    return False, "cancelled"
                self._got_data.clear()
                span = min(REQUEST_SPAN_BYTES, size - offset)
                before = flow["at"]
                if not self.mavlink.request_log_data(log_id, offset, span):
                    return False, "link lost"
                # Wait for the requested span to arrive, then advance past every
                # byte we now hold — re-requesting only the first real gap.
                deadline = time.monotonic() + DATA_STALL_TIMEOUT_S
                while time.monotonic() < deadline:
                    if self._cancel.is_set():
                        return False, "cancelled"
                    if received.find(b"\x00", offset, offset + span) < 0:
                        break
                    self._got_data.wait(0.1)
                    self._got_data.clear()
                    # The vehicle streams a span in one burst, so once packets
                    # stop arriving the missing ones are lost, not late. Sitting
                    # out the full stall timeout for each dropped packet is what
                    # made a lossy link take minutes per megabyte.
                    quiet = flow["at"]
                    if quiet > before and time.monotonic() - quiet > DATA_QUIET_S:
                        break
                nxt = received.find(b"\x00", offset)
                if nxt < 0:
                    nxt = size
                if nxt > offset:
                    offset = nxt
                    stalls = 0
                    last_progress = time.monotonic()
                    total = size or 1
                    self._set("downloading", int(offset * 100 / total),
                              f"{name} — {offset // 1024}/{size // 1024} KB"
                              + (f" (+{remaining} queued)" if remaining else ""))
                    continue
                stalls += 1
                # Not one byte came back. Either the request was lost, or the
                # vehicle dropped the log session under us — a reboot, a link
                # cycle, another station ending it. The second failure is
                # silent and permanent, so re-open the session once before
                # spending a minute waiting for data that cannot arrive.
                if stalls >= STALL_REPRIME_AFTER and reprimes_left:
                    reprimes_left -= 1
                    self.mavlink.set_log_sink(None)
                    reopened = self._prime_session()
                    self.mavlink.set_log_sink(sink)
                    if not reopened:
                        return False, "the vehicle closed its log session"
                    stalls = 0
                    last_progress = time.monotonic()
                    continue
                if time.monotonic() - last_progress > DOWNLOAD_GIVE_UP_S:
                    return False, "the vehicle stopped sending log data"
            payload = bytes(self._buffer)
        except Exception as exc:  # noqa: BLE001
            logger.exception("log %d download failed", log_id)
            return False, str(exc)
        finally:
            self.mavlink.set_log_sink(None)
            self._buffer = bytearray()

        try:
            tmp = path + ".part"
            with open(tmp, "wb") as handle:
                handle.write(payload)
            os.replace(tmp, path)
        except OSError as exc:
            if exc.errno == errno.ENOSPC:
                return False, "the download folder is full"
            return False, f"could not write {name} ({exc.strerror or exc})"
        return True, path

    def _filename(self, log_id: int, entry: dict[str, Any]) -> str:
        """``log_<id>_<local date>.ulg`` — sortable, and readable a month later.

        The vehicle reports the log's time as UTC, and the name used to keep it
        that way. Nobody flies in UTC: the operator remembers the flight by the
        clock on the wall, and a name an hour or two off that is a name they
        have to convert before they can trust it. So the stamp is this
        machine's local time, matching both the tlog names and every time this
        page shows. Files downloaded before this change keep their UTC names —
        harmless, because a log is matched to the vehicle by id and size, never
        by the stamp in its name.
        """
        stamp = ""
        utc = int(entry.get("utc") or 0)
        if utc > 0:
            try:
                stamp = time.strftime("_%Y-%m-%d_%H-%M", time.localtime(utc))
            except (ValueError, OSError):
                stamp = ""
        return _safe_component(f"log_{log_id:03d}{stamp}") + ".ulg"

    # ------------------------------------------------------------------
    # Erase
    # ------------------------------------------------------------------

    def erase(self) -> bool:
        """Erase every on-board log. False on refusal.

        MAVLink has no per-log delete — LOG_ERASE clears the whole log
        directory — so this is all-or-nothing by protocol, not by choice. The
        UI says so before it asks.

        The erased list is re-read afterwards rather than assumed: "the vehicle
        says it has no logs" is evidence, "we sent the command" is not.
        """
        if self.mavlink is None or not self.mavlink.is_connected():
            self.last_error = "not connected"
            return False
        with self._lock:
            if self._state in BUSY_STATES:
                self.last_error = "a log operation is already running"
                return False
        self._cancel.clear()
        self._set("erasing", 0, "Erasing all logs on the vehicle…")
        self._worker = threading.Thread(target=self._run_erase, name="log-erase", daemon=True)
        self._worker.start()
        return True

    def _run_erase(self) -> None:
        try:
            if not self.mavlink.erase_logs():
                error = ""
                try:
                    error = self.mavlink.get_last_command_error() or ""
                except Exception:  # noqa: BLE001
                    error = ""
                self._set("failed", 0, error or "the vehicle refused to erase its logs")
                return
        except Exception:  # noqa: BLE001 - the worker must never propagate
            logger.exception("log erase crashed")
            self._set("failed", 0, "log erase failed unexpectedly")
            return
        with self._lock:
            self._entries = {}
            self._completed = []
        # Give the SD card time to actually clear, then ask again — the answer
        # is what the operator sees, not our optimism.
        self._cancel.wait(ERASE_SETTLE_S)
        with self._lock:
            self._state = "listing"
        self._run_list()

    # ------------------------------------------------------------------
    # Cancel / shutdown / listeners
    # ------------------------------------------------------------------

    def cancel(self) -> bool:
        with self._lock:
            if self._state not in BUSY_STATES:
                return False
            self._queue = []
        self._cancel.set()
        self._got_data.set()
        return True

    def shutdown(self) -> None:
        """Idempotent teardown. Never raises, never leaves the sink attached."""
        self._shutting_down = True
        self._cancel.set()
        self._got_data.set()
        worker = self._worker
        if worker is not None:
            try:
                worker.join(timeout=3.0)
            except Exception:  # noqa: BLE001
                pass
            self._worker = None
        try:
            if self.mavlink is not None:
                self.mavlink.set_log_sink(None)
        except Exception:  # noqa: BLE001
            pass
        with self._lock:
            self._state = "idle"

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            self._listeners.append(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass

    def _set(self, state: str, percent: int, message: str) -> None:
        with self._lock:
            self._state = state
            self._percent = percent
            self._message = message
            listeners = list(self._listeners)
            current = self._current
            queued = len(self._queue)
        entry = {"state": state, "percent": percent, "message": message,
                 "current": current, "queued": queued}
        for fn in listeners:
            try:
                fn(entry)
            except Exception:  # noqa: BLE001 - a bad subscriber must not kill the job
                logger.exception("log listener raised")
