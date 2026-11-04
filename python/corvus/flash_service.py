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

A controller with no firmware (a new board, or one whose flash was interrupted)
has no MAVLink to reboot, but it is already sitting in its bootloader. When no
vehicle is linked and exactly one board shows up in its bootloader, step 1 is
skipped and that port is flashed directly. Everything else is unchanged.
"""
from __future__ import annotations

import logging
import sys
import threading
from typing import Any, TYPE_CHECKING
from collections.abc import Callable

from corvus_link import flash as _flash

from .firmware_catalog import FirmwareCatalog
from .firmware_uploader import FirmwareUploader, parse_firmware_image  # noqa: F401 - read by the Rust engine
from .serial_ports import is_bootloader_port  # noqa: F401 - read by the Rust engine

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
        mavlink: MavlinkBridge,
        store: VehicleStateStore,
        catalog: FirmwareCatalog | None = None,
        list_ports_fn: Callable[[], list[dict[str, Any]]] | None = None,
    ) -> None:
        self.mavlink = mavlink
        self._store = store
        self.catalog = catalog
        self._list_ports = list_ports_fn
        self._lock = threading.Lock()
        self._state = "idle"
        self._percent = 0
        self._message = ""
        # Set on every gate-failure branch of start() so the HTTP layer can
        # surface the exact refusal reason on the 409 response.
        self.last_error: str = ""
        self._cancel = threading.Event()
        self._worker: threading.Thread | None = None
        self._uploader: FirmwareUploader | None = None
        self._listeners: list[Callable[[dict[str, Any]], None]] = []
        # Raised by shutdown() so the worker's finally does NOT reconnect the
        # bridge while the app is tearing down.
        self._shutting_down = False

    # ------------------------------------------------------------------
    # A board that is already in its bootloader
    # ------------------------------------------------------------------

    def bootloader_ports(self) -> list[dict[str, Any]]:
        """Serial ports whose descriptor says "flight controller in its bootloader"."""
        return _engine.bootloader_ports(self)

    def bootloader_device(self) -> str:
        """The one bootloader port that may be flashed without a link, or "".

        Only while no vehicle is linked (a linked vehicle takes the normal
        path, and its own bootloader reboot is what makes the port appear), and
        only when there is exactly one: with two boards waiting, guessing is
        how the wrong one gets erased.
        """
        return _engine.bootloader_device(self)

    # ------------------------------------------------------------------
    # Status (HTTP GET /api/firmware/status)
    # ------------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Live status computed from the bridge + the flash state."""
        mavlink = self.mavlink
        transport = mavlink.transport() if mavlink is not None else "unknown"
        device = mavlink.serial_device() if mavlink is not None else ""
        armed = bool(self._store.get_snapshot().get("armed"))
        bootloader = self.bootloader_device()
        with self._lock:
            state = self._state
            percent = self._percent
            message = self._message
        return {
            "state": state,
            "transport": transport,
            "device": device,
            # can_flash is the conjunction: USB link AND idle AND disarmed.
            "can_flash": bool(
                mavlink is not None and state not in BUSY_STATES
                and (bool(bootloader) or (mavlink.is_direct_usb() and not armed))),
            # The port of a board waiting in its bootloader, or "".
            "bootloader": bootloader,
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
        return _engine.gate(self)

    def start(self, firmware_bytes: bytes) -> bool:
        """Synchronous gate check, then spawn the worker. Returns False on refusal.

        Every refusal sets :attr:`last_error` so the HTTP handler can return the
        exact reason. Firmware is parsed here (in the calling thread) so a bad
        archive yields a 409 immediately, not a mid-flash failure.
        """
        return _engine.start(self, firmware_bytes)

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
        return _engine.start_release(self, release_tag, board_name)

    def _run_release(self, conn: str, entry: dict[str, Any], bootloader: str = "") -> None:
        """Download worker: fetch (or reuse the cache), then hand off to _run."""
        _engine.run_release(self, conn, entry, bootloader)

    def _gate_for_resume(self, bootloader: str = "") -> str | None:
        """The gate, minus the busy check — this job IS the busy one.

        A flash that started on a board in its bootloader resumes only while
        that same board is still the one waiting there.
        """
        return _engine.gate_for_resume(self, bootloader)

    # ------------------------------------------------------------------
    # Worker
    # ------------------------------------------------------------------

    def _run(self, conn: str, image: bytes, board_id: int | None = None,
             bootloader: str = "") -> None:
        """Upload worker: reboot -> stop bridge -> flash -> (re)connect bridge.

        *board_id* is the board the image was built for, or None for a raw
        binary; the uploader checks it against the bootloader before erasing.
        *bootloader* is the port of a board that is already in its bootloader:
        there is nothing to reboot, so the reboot is skipped and that port is
        flashed.
        """
        _engine.run(self, conn, image, board_id, bootloader)

    def _reconnect(self, conn: str) -> None:
        """Restart the MAVLink bridge so the GCS reconnects to the flashed FC.

        Skipped during shutdown (the bridge is being torn down anyway; respawning
        it would race the stop sequence).
        """
        _engine.reconnect(self, conn)

    # ------------------------------------------------------------------
    # Cancel / shutdown
    # ------------------------------------------------------------------

    def cancel(self) -> bool:
        """Cancel a running download or flash. False when nothing is running."""
        return _engine.cancel(self)

    def shutdown(self) -> None:
        """Idempotent teardown: cancel the worker, join it, reset state. Never raises."""
        _engine.shutdown(self)

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


_engine = _flash.FlashEngine(sys.modules[__name__])
