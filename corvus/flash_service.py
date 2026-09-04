"""Firmware-flash orchestration service.

Gate + flow for flashing PX4 firmware over a **direct USB** link only. SiK
radio, UDP, and TCP links are refused at the gate (the uploader must own the
serial device, and flashing over a telemetry radio is unsafe).

Flow (all on a single daemon worker thread — the uploader itself runs
synchronously on that thread):

  1. ``MavlinkBridge.reboot_to_bootloader()`` — reboot the FC into its USB
     bootloader (refused while armed).
  2. ``MavlinkBridge.stop()`` — release the MAVLink bridge so the uploader
     can own the serial device.
  3. :class:`FirmwareUploader.run` — erase / program / verify / reset over
     pyserial on the bootloader device.
  4. ``MavlinkBridge.set_connection`` + ``start()`` — reconnect the GCS to the
     (newly flashed) FC. Skipped during shutdown so a tear-down never respawns
     the bridge.
"""
from __future__ import annotations

import logging
import threading
from typing import Any, Callable, TYPE_CHECKING

from .firmware_catalog import FirmwareCatalog
from .firmware_uploader import FirmwareUploader, parse_firmware

if TYPE_CHECKING:  # avoid an import cycle at runtime
    from .mavlink_bridge import MavlinkBridge
    from .state_store import VehicleStateStore

logger = logging.getLogger("corvus.flash")

# The HTTP ``/api/firmware/status`` gate message returned when the link is not a
# direct FC USB connection. Kept here as a constant so the gate text is single-
# sourced and the GUI can rely on its exact wording.
NON_USB_GATE_MESSAGE = (
    "Firmware flash requires a direct USB connection to the flight controller "
    "(current link: {transport}). SiK radio, UDP, and TCP are not permitted."
)

# States in which the service is busy and must refuse a second job. Downloading
# counts: the operator has committed to a flash, and starting another one would
# race the first onto the same serial device.
BUSY_STATES = frozenset({"downloading", "flashing"})


class FlashService:
    """Orchestrates firmware flashing over a direct USB link only.

    Gate: refuses unless ``MavlinkBridge.is_direct_usb()``. The synchronous gate
    runs in the calling (HTTP) thread so a refusal returns before any worker is
    spawned; the upload itself runs on a daemon worker thread.
    """

    def __init__(
        self,
        mavlink: "MavlinkBridge",
        store: "VehicleStateStore",
        catalog: "FirmwareCatalog | None" = None,
    ) -> None:
        self.mavlink = mavlink
        self._store = store
        self.catalog = catalog
        self._lock = threading.Lock()
        self._state = "idle"
        self._percent = 0
        self._message = ""
        # Set on every gate-failure branch of start() so the HTTP layer can
        # surface the exact refusal reason on the 409 response.
        self.last_error: str = ""
        self._cancel = threading.Event()
        self._worker: threading.Thread | None = None
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        # Raised by shutdown() so the worker's finally does NOT reconnect the
        # bridge while the app is tearing down.
        self._shutting_down = False

    # ------------------------------------------------------------------
    # Status (HTTP GET /api/firmware/status)
    # ------------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Live status computed from the bridge + the flash state."""
        mavlink = self.mavlink
        transport = mavlink.transport() if mavlink is not None else "unknown"
        device = mavlink.serial_device() if mavlink is not None else ""
        armed = bool(self._store.get_snapshot().get("armed"))
        with self._lock:
            state = self._state
            percent = self._percent
            message = self._message
        return {
            "state": state,
            "transport": transport,
            "device": device,
            # can_flash is the conjunction: USB link AND idle AND disarmed.
            "can_flash": bool(mavlink is not None and mavlink.is_direct_usb()
                              and state not in BUSY_STATES and not armed),
            "armed": armed,
            "progress": percent,
            "message": message,
        }

    # ------------------------------------------------------------------
    # Gate + start (HTTP POST /api/firmware/upload)
    # ------------------------------------------------------------------

    def _gate(self) -> str | None:
        """Refusal reason, or None when a flash may start.

        Shared by every entry point so a download-then-flash is refused on
        exactly the same terms as a direct upload — the gate is a safety
        property, not a per-endpoint detail.
        """
        if self.mavlink is None:
            return "no autopilot connection"
        if not self.mavlink.is_direct_usb():
            return NON_USB_GATE_MESSAGE.format(transport=self.mavlink.transport())
        if self._store.get_snapshot().get("armed"):
            return "cannot flash while armed"
        with self._lock:
            if self._state in BUSY_STATES:
                return "flash already in progress"
        return None

    def start(self, firmware_bytes: bytes) -> bool:
        """Synchronous gate check, then spawn the worker. Returns False on refusal.

        Every refusal sets :attr:`last_error` so the HTTP handler can return the
        exact reason. Firmware is parsed here (in the calling thread) so a bad
        archive yields a 409 immediately, not a mid-flash failure.
        """
        refusal = self._gate()
        if refusal:
            self.last_error = refusal
            return False
        with self._lock:
            conn = self.mavlink.connection_string()
        # Parse now so an invalid archive is rejected before we reboot the FC.
        try:
            image = parse_firmware(firmware_bytes)
        except ValueError as exc:
            self.last_error = str(exc)
            return False
        if not image:
            self.last_error = "empty firmware image"
            return False
        self._cancel.clear()
        with self._lock:
            self._state = "flashing"
            self._percent = 0
            self._message = "Preparing to flash…"
        self._notify_listeners({"state": "flashing", "percent": 0, "message": "Preparing to flash…"})
        self._worker = threading.Thread(
            target=self._run, args=(conn, image), name="firmware-flash", daemon=True,
        )
        self._worker.start()
        return True

    def start_release(self, release_tag: str, board_name: str) -> bool:
        """Download one catalogue image, then flash it. False on refusal.

        The client names a release and a board, never a URL: the download target
        is resolved from the backend's own catalogue and re-derived from a fixed
        template, so the browser cannot point the fetch anywhere.

        Download and flash are one job on one worker with one progress stream
        and one cancel, because to the operator they are one action — and
        because a download that finished into a vehicle that has since armed
        must not flash. The gate is therefore re-checked after the download.
        """
        if self.catalog is None:
            self.last_error = "firmware catalogue unavailable"
            return False
        refusal = self._gate()
        if refusal:
            self.last_error = refusal
            return False
        entry = self.catalog.resolve(release_tag, board_name)
        if entry is None:
            self.last_error = "unknown firmware release or board"
            return False
        with self._lock:
            conn = self.mavlink.connection_string()
        self._cancel.clear()
        self._set("downloading", 0, f"Downloading {entry.get('name', '')}…")
        self._worker = threading.Thread(
            target=self._run_release, args=(conn, entry),
            name="firmware-download", daemon=True,
        )
        self._worker.start()
        return True

    def _run_release(self, conn: str, entry: dict[str, Any]) -> None:
        """Download worker: fetch (or reuse the cache), then hand off to _run."""
        name = str(entry.get("name", ""))
        try:
            cached = self.catalog.cached_path(name) is not None
            if cached:
                self._set("downloading", 100, f"Using cached {name}")

            def on_progress(done: int, total: int) -> None:
                percent = int(done * 100 / total) if total > 0 else 0
                self._set("downloading", min(99, percent),
                          f"Downloading {name} — {done // 1024} KB")

            raw = self.catalog.download(
                entry, on_progress=None if cached else on_progress, cancel=self._cancel,
            )
            if self._cancel.is_set():
                self._set("cancelled", 0, "Download cancelled")
                return
            image = parse_firmware(raw)
            if not image:
                self._set("failed", 0, "empty firmware image")
                return
        except ValueError as exc:
            self._set("failed", 0, str(exc))
            return
        except Exception:  # noqa: BLE001 - the worker must never propagate
            logger.exception("firmware download worker crashed")
            self._set("failed", 0, "firmware download failed unexpectedly")
            return

        # Re-check the gate: the download took time, and the vehicle may have
        # armed or the cable may have been pulled since the operator pressed go.
        refusal = self._gate_for_resume()
        if refusal:
            self._set("failed", 0, refusal)
            return
        self._set("flashing", 0, "Preparing to flash…")
        self._run(conn, image)

    def _gate_for_resume(self) -> str | None:
        """The gate, minus the busy check — this job IS the busy one."""
        if self.mavlink is None:
            return "no autopilot connection"
        if not self.mavlink.is_direct_usb():
            return NON_USB_GATE_MESSAGE.format(transport=self.mavlink.transport())
        if self._store.get_snapshot().get("armed"):
            return "cannot flash while armed"
        return None

    # ------------------------------------------------------------------
    # Worker
    # ------------------------------------------------------------------

    def _run(self, conn: str, image: bytes) -> None:
        """Upload worker: reboot -> stop bridge -> flash -> (re)connect bridge."""
        self._set("flashing", 0, "Rebooting autopilot into bootloader…")
        uploader: FirmwareUploader | None = None
        try:
            if not self.mavlink.reboot_to_bootloader():
                self._set("failed", 0, "Reboot to bootloader rejected — is the vehicle armed?")
                return
            # Release the bridge so the uploader can own the serial device.
            try:
                self.mavlink.stop()
            except Exception:  # noqa: BLE001 - bridge may already be tearing down
                logger.debug("mavlink.stop() during flash raised", exc_info=True)
            device = self.mavlink.serial_device()
            if not device:
                self._set("failed", 0, "no serial device for bootloader")
                return
            uploader = FirmwareUploader(on_progress=self._on_uploader_progress, cancel=self._cancel)
            ok = uploader.run(device, image)
            if self._cancel.is_set():
                self._set("cancelled", self._percent, "Flash cancelled")
            elif ok:
                self._set("done", 100, "Firmware flashed successfully")
            else:
                self._set("failed", self._percent, self._message or "flash failed")
        except Exception:  # noqa: BLE001 - the worker must never propagate
            logger.exception("firmware flash worker crashed")
            self._set("failed", self._percent, "flash failed unexpectedly")
        finally:
            if uploader is not None:
                try:
                    uploader.shutdown()
                except Exception:  # noqa: BLE001
                    pass
            self._reconnect(conn)

    def _reconnect(self, conn: str) -> None:
        """Restart the MAVLink bridge so the GCS reconnects to the flashed FC.

        Skipped during shutdown (the bridge is being torn down anyway; respawning
        it would race the stop sequence).
        """
        if self._shutting_down or not conn or self.mavlink is None:
            return
        try:
            self.mavlink.set_connection(conn)
            self.mavlink.start()
        except Exception:  # noqa: BLE001 - best-effort reconnect
            logger.debug("mavlink reconnect after flash failed", exc_info=True)

    # ------------------------------------------------------------------
    # Cancel / shutdown
    # ------------------------------------------------------------------

    def cancel(self) -> bool:
        """Cancel a running download or flash. False when nothing is running."""
        with self._lock:
            if self._state not in BUSY_STATES:
                return False
        self._cancel.set()
        return True

    def shutdown(self) -> None:
        """Idempotent teardown: cancel the worker, join it, reset state. Never raises."""
        self._shutting_down = True
        self._cancel.set()
        worker = self._worker
        if worker is not None:
            try:
                worker.join(timeout=3.0)
            except Exception:  # noqa: BLE001
                pass
            self._worker = None
        with self._lock:
            self._state = "idle"

    # ------------------------------------------------------------------
    # Listeners (SSE fan-out)
    # ------------------------------------------------------------------

    def add_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            self._listeners.append(fn)

    def remove_listener(self, fn: Callable[[dict[str, Any]], None]) -> None:
        with self._lock:
            try:
                self._listeners.remove(fn)
            except ValueError:
                pass

    def _notify_listeners(self, entry: dict[str, Any]) -> None:
        with self._lock:
            listeners = list(self._listeners)
        for fn in listeners:
            try:
                fn(entry)
            except Exception:  # noqa: BLE001 - a bad subscriber must not kill the flash
                logger.exception("flash listener raised")

    def _set(self, state: str, percent: int, message: str) -> None:
        """Update the flash-level state and notify listeners (flash-level state)."""
        with self._lock:
            self._state = state
            self._percent = percent
            self._message = message
        self._notify_listeners({"state": state, "percent": percent, "message": message})

    def _on_uploader_progress(self, p: dict[str, Any]) -> None:
        """Forward granular uploader progress (connecting/erasing/...) to SSE.

        Updates the percent/message used by status() but does NOT change the
        flash-level state, which stays ``flashing`` until done/failed/cancelled.
        """
        percent = int(p.get("percent", 0))
        message = str(p.get("message", ""))
        with self._lock:
            self._percent = percent
            self._message = message
        self._notify_listeners({
            "state": p.get("state"),
            "percent": percent,
            "message": message,
        })
