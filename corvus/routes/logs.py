"""On-board logs: listing, download, erase.

Route handlers moved out of corvus/server.py. CorvusHandler inherits
LogRoutes, and the @route registry wires them exactly as before.
"""
from __future__ import annotations

import logging
import errno
import os

from urllib.parse import parse_qs, urlparse
from ..config import default_config_path, save_config
from ..http_routes import _config_write_lock, route

logger = logging.getLogger("corvus.server")


class LogRoutes:
    # ---- Flight logs (on-board ULogs + local tlogs) ----
    @route("GET", "/api/logs/status")
    def _api_logs_status(self) -> None:
        """Everything the Analysis page renders: logs, queue, progress, folder."""
        if self.logs is None:
            self._send_json({
                "state": "idle", "message": "", "percent": 0, "current": None,
                "queued": [], "completed": [], "logs": [], "tlogs": [],
                "dir": "", "connected": False,
            })
            return
        self._send_json(self.logs.status())

    @route("POST", "/api/logs/refresh")
    def _api_logs_refresh(self, payload: dict) -> None:
        """Ask the vehicle to enumerate its on-board logs."""
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.refresh():
            self._send_json({"ok": True, "state": "listing"})
            return
        error = getattr(self.logs, "last_error", "") or "could not list logs"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    @route("POST", "/api/logs/download")
    def _api_logs_download(self, payload: dict) -> None:
        """Queue one or more on-board logs for sequential download."""
        raw = payload.get("ids")
        if not isinstance(raw, list) or not raw:
            self._send_json({"ok": False, "error": "ids must be a non-empty list"}, 400)
            return
        ids: list[int] = []
        for item in raw:
            if isinstance(item, bool) or not isinstance(item, (int, float)):
                self._send_json({"ok": False, "error": "ids must be numbers"}, 400)
                return
            ids.append(int(item))
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.start_download(ids):
            self._send_json({"ok": True, "state": "downloading", "queued": len(ids)})
            return
        error = getattr(self.logs, "last_error", "") or "download refused"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    def _review_sensitivity(self) -> str:
        """The Flight Review sensitivity the operator set in Settings.

        Read per request from the live config, so a change on the Settings
        page applies to the next review opened without a restart. Never
        raises: an unreadable setting reviews at the default.
        """
        from ..flight_review import normalize_sensitivity
        try:
            block = getattr(self._live_config(), "review", None) or {}
            return normalize_sensitivity(block.get("sensitivity"))
        except Exception:  # noqa: BLE001 - a setting must not fail a review
            return normalize_sensitivity(None)

    @route("GET", "/api/logs/review")
    def _api_logs_review(self) -> None:
        """Flight Review for one ULog in the download folder.

        The file is named, never pathed: the name is resolved inside the
        download folder and the result checked with realpath, so no query can
        walk out of it.
        """
        params = parse_qs(urlparse(self.path).query)
        name = (params.get("file", [""])[0] or "").strip()
        if not name:
            self._send_json({"ok": False, "error": "file is required"}, 400)
            return
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        # Emptiness is tested BEFORE realpath: realpath("") is the process's
        # working directory, so the commonpath guard below would happily pass
        # and an unconfigured folder would resolve names against wherever
        # Corvus was started from.
        configured = self.logs.status().get("dir") or ""
        if not configured:
            self._send_json({"ok": False, "error": "no download folder configured"}, 400)
            return
        directory = os.path.realpath(configured)
        target = os.path.realpath(os.path.join(directory, os.path.basename(name)))
        if os.path.commonpath([directory, target]) != directory:
            self._send_json({"ok": False, "error": "unknown log file"}, 400)
            return
        # .bin is accepted here and refused by the parser with a sentence that
        # says what the file actually is. "Unknown log file" for a log Corvus
        # downloaded itself, under a name it chose itself, is the worst of both.
        suffixes = getattr(self.logs, "LOG_SUFFIXES", (".ulg",))
        if not target.endswith(suffixes) or not os.path.isfile(target):
            self._send_json({"ok": False, "error": "unknown log file"}, 404)
            return
        try:
            from ..flight_review import review_file
            from ..ulog import UlogError
            data = review_file(target, os.path.basename(target),
                               self._review_sensitivity())
        except UlogError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except (OSError, MemoryError) as exc:
            self._send_json({"ok": False, "error": f"could not read the log ({exc})"}, 400)
            return
        except Exception:  # noqa: BLE001 - a bad log must not 500 the app
            logger.exception("flight review failed for %s", target)
            self._send_json({"ok": False, "error": "could not analyse this log"}, 400)
            return
        # Spread, not mutated: the result may be a cached one that another
        # request is about to send.
        self._send_json({**data, "ok": True})

    @route("GET", "/api/logs/tlog-review")
    def _api_logs_tlog_review(self) -> None:
        """Telemetry Review for one recorded tlog.

        Confined to the tlog folder exactly the way the ULog review is confined
        to the download folder: the request names a file, never a path, and the
        resolved target is checked with realpath before it is opened.
        """
        params = parse_qs(urlparse(self.path).query)
        name = (params.get("file", [""])[0] or "").strip()
        if not name:
            self._send_json({"ok": False, "error": "file is required"}, 400)
            return
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        # Emptiness is tested BEFORE realpath, not after: realpath("") is the
        # process's working directory, which is always a real path, so the
        # guard below would pass and an unconfigured folder would resolve
        # names against wherever Corvus happened to be started.
        configured = self.logs.resolve_tlog_dir() or ""
        if not configured:
            self._send_json({"ok": False, "error": "no recording folder configured"}, 400)
            return
        directory = os.path.realpath(configured)
        target = os.path.realpath(os.path.join(directory, os.path.basename(name)))
        if os.path.commonpath([directory, target]) != directory:
            self._send_json({"ok": False, "error": "unknown recording"}, 400)
            return
        if not target.endswith(".tlog") or not os.path.isfile(target):
            self._send_json({"ok": False, "error": "unknown recording"}, 404)
            return
        try:
            from ..tlog_review import TlogError, review_file
            data = review_file(target, os.path.basename(target),
                               self._review_sensitivity())
        except TlogError as exc:
            self._send_json({"ok": False, "error": str(exc)}, 400)
            return
        except (OSError, MemoryError) as exc:
            self._send_json({"ok": False,
                             "error": f"could not read that recording ({exc})"}, 400)
            return
        except Exception:  # noqa: BLE001 - a bad recording must not 500 the app
            logger.exception("telemetry review failed for %s", target)
            self._send_json({"ok": False,
                             "error": "could not analyse that recording"}, 400)
            return
        # Spread, not mutated: the result may be a cached one that another
        # request is about to send.
        self._send_json({**data, "ok": True})

    @route("POST", "/api/logs/erase")
    def _api_logs_erase(self, payload: dict) -> None:
        """Erase every on-board log.

        MAVLink has no per-log delete, so this is all-or-nothing by protocol.
        The confirm lives in the UI; the backend refuses while armed and while
        another log job owns the vehicle's single log session.
        """
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.erase():
            self._send_json({"ok": True, "state": "erasing"})
            return
        error = getattr(self.logs, "last_error", "") or "erase refused"
        self._send_json({"ok": False, "error": error},
                        503 if "not connected" in error else 409)

    @route("POST", "/api/logs/cancel")
    def _api_logs_cancel(self, payload: dict) -> None:
        del payload
        if self.logs is None:
            self._send_json({"ok": False, "error": "log service unavailable"}, 503)
            return
        if self.logs.cancel():
            self._send_json({"ok": True})
        else:
            self._send_json({"ok": False, "error": "no log operation in progress"})

    @route("POST", "/api/logs/dir")
    def _api_logs_dir(self, payload: dict) -> None:
        """Set the download folder and persist it for future connections.

        Its own endpoint rather than a general config write because this is the
        one setting the Analysis page owns, and it has to be verified as
        writable before the operator queues an hour of downloads into it.
        """
        value = payload.get("dir")
        if not isinstance(value, str) or not value.strip():
            self._send_json({"ok": False, "error": "dir must be a non-empty string"}, 400)
            return
        path = os.path.expanduser(value.strip())
        try:
            os.makedirs(path, exist_ok=True)
            if not os.access(path, os.W_OK):
                raise OSError(errno.EACCES, "not writable")
        except OSError as exc:
            self._send_json({"ok": False,
                             "error": f"cannot use {path} ({exc.strerror or exc})"}, 400)
            return
        if self.config is not None:
            with _config_write_lock:
                self.config.log_download_dir = path
                try:
                    save_config(self.config, self.config_path or default_config_path())
                except Exception:  # noqa: BLE001 - the folder still works this session
                    logger.exception("could not persist log_download_dir")
                    self._send_json({"ok": True, "dir": path,
                                     "warning": "folder set for this session but not saved"})
                    return
        self._send_json({"ok": True, "dir": path})
