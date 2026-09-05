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
DATA_STALL_TIMEOUT_S = 4.0            # no LOG_DATA for this long = re-request
DOWNLOAD_GIVE_UP_S = 60.0             # no progress at all for this long = fail
MAX_LOG_BYTES = 2 * 1024 * 1024 * 1024
# States in which the service owns the vehicle's single log session and must
# refuse a second job.
BUSY_STATES = frozenset({"listing", "downloading", "erasing"})
# The vehicle needs a moment to clear the SD card before a re-list is honest.
ERASE_SETTLE_S = 2.0


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

    def local_tlogs(self) -> list[dict[str, Any]]:
        """tlogs Corvus already recorded on this laptop."""
        out: list[dict[str, Any]] = []
        directory = ""
        try:
            directory = (self._tlog_dir() if self._tlog_dir else "") or ""
        except Exception:  # noqa: BLE001
            return out
        if not directory:
            return out
        try:
            names = sorted(os.listdir(directory), reverse=True)
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
                "mtime": stat.st_mtime, "path": path,
            })
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
        last_seen = time.monotonic()

        def sink(msg: Any) -> None:
            nonlocal last_seen
            if msg.get_type() != "LOG_ENTRY":
                return
            last_seen = time.monotonic()
            try:
                size = int(msg.size)
            except (TypeError, ValueError):
                return
            # PX4 reports every slot including empty ones; an empty log is not
            # something the operator can do anything with.
            if size <= 0:
                return
            seen[int(msg.id)] = {
                "id": int(msg.id),
                "size": size,
                "utc": int(getattr(msg, "time_utc", 0) or 0),
                "num_logs": int(getattr(msg, "num_logs", 0) or 0),
            }

        try:
            self.mavlink.set_log_sink(sink)
            if not self.mavlink.request_log_list():
                self._set("failed", 0, "could not ask the vehicle for its logs")
                return
            while not self._cancel.is_set():
                if time.monotonic() - last_seen > LIST_TIMEOUT_S:
                    break
                if seen:
                    expected = next(iter(seen.values())).get("num_logs", 0)
                    if expected and len(seen) >= expected:
                        break
                time.sleep(0.1)
        except Exception:  # noqa: BLE001 - the worker must never propagate
            logger.exception("log listing crashed")
            self._set("failed", 0, "log listing failed unexpectedly")
            return
        finally:
            self.mavlink.set_log_sink(None)
            self.mavlink.log_request_end()

        with self._lock:
            self._entries = seen
            self._listed_at = time.time()
        if self._cancel.is_set():
            self._set("cancelled", 0, "Listing cancelled")
        elif not seen:
            self._set("idle", 0, "No logs on the vehicle")
        else:
            self._set("idle", 0, f"{len(seen)} log(s) on the vehicle")

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
        received = bytearray(size)          # 1 per byte received, for gap detection
        got_any = threading.Event()

        def sink(msg: Any) -> None:
            if msg.get_type() != "LOG_DATA" or int(msg.id) != log_id:
                return
            offset = int(msg.ofs)
            data = bytes(msg.data)[:int(msg.count)]
            end = min(offset + len(data), size)
            if offset >= size or end <= offset:
                return
            self._buffer[offset:end] = data[:end - offset]
            for i in range(offset, end):
                received[i] = 1
            got_any.set()
            self._got_data.set()

        try:
            self.mavlink.set_log_sink(sink)
            started = time.monotonic()
            last_progress = started
            done_bytes = 0
            offset = 0
            while offset < size:
                if self._cancel.is_set():
                    return False, "cancelled"
                self._got_data.clear()
                span = min(REQUEST_SPAN_BYTES, size - offset)
                if not self.mavlink.request_log_data(log_id, offset, span):
                    return False, "link lost"
                # Wait for the requested span to arrive, then advance past every
                # byte we now hold — re-requesting only the first real gap.
                deadline = time.monotonic() + DATA_STALL_TIMEOUT_S
                while time.monotonic() < deadline:
                    if self._cancel.is_set():
                        return False, "cancelled"
                    if all(received[i] for i in range(offset, offset + span)):
                        break
                    self._got_data.wait(0.2)
                    self._got_data.clear()
                nxt = offset
                while nxt < size and received[nxt]:
                    nxt += 1
                if nxt > offset:
                    offset = nxt
                    done_bytes = offset
                    last_progress = time.monotonic()
                    total = size or 1
                    self._set("downloading", int(done_bytes * 100 / total),
                              f"{name} — {done_bytes // 1024}/{size // 1024} KB"
                              + (f" (+{remaining} queued)" if remaining else ""))
                elif time.monotonic() - last_progress > DOWNLOAD_GIVE_UP_S:
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
        """``log_<id>_<UTC date>.ulg`` — sortable, and readable a month later."""
        stamp = ""
        utc = int(entry.get("utc") or 0)
        if utc > 0:
            try:
                stamp = time.strftime("_%Y-%m-%d_%H-%M", time.gmtime(utc))
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
