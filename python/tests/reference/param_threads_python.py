"""How the parameter workers were started in Python, on threading.Thread: the
batch upload, the download watchdog and the metadata download.

Kept unchanged as the reference tests/test_param_threads_parity.py holds the
Rust threads to; not imported by the app.
"""
from __future__ import annotations

import threading
from typing import Any

from corvus.param_metadata import ParamMetadataCache


class ParamThreadsMixin:
    """The parameter worker starts, as ParamProtocolMixin and ParamMetadataMixin had them."""

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

    def start_param_metadata(self, cache: ParamMetadataCache | None = None) -> dict[str, Any]:
        """Start reading the vehicle's parameter metadata in the background.

        A no-op while a fetch runs or once it has succeeded for this link. A
        fetch that failed is tried again. *cache* is the operator's opt-in
        copy store; it is ignored for a stack whose metadata is not fixed per
        firmware build. Returns the status.
        """
        dialect = self._dialect
        with self._param_meta_lock:
            if self._param_meta_state in ("loading", "ready"):
                return self._param_metadata_status_locked(False)
        previous = self._param_meta_thread
        if previous is not None and previous.is_alive():
            # A fetch cancelled by a link reset; it unwinds within one wait slice.
            previous.join(timeout=1.0)
        with self._param_meta_lock:
            if self._param_meta_state in ("loading", "ready"):
                return self._param_metadata_status_locked(False)
            if not dialect.param_metadata_path:
                self._param_meta_state = "unavailable"
                self._param_meta_error = "this autopilot does not publish parameter defaults"
                return self._param_metadata_status_locked(False)
            if not self._connection_ready():
                self._param_meta_error = "not connected"
                return self._param_metadata_status_locked(False)
            if previous is not None and previous.is_alive():
                self._param_meta_error = "the previous transfer is still closing"
                return self._param_metadata_status_locked(False)
            self._param_meta_generation += 1
            generation = self._param_meta_generation
            self._param_meta_state = "loading"
            self._param_meta_error = ""
            self._param_meta_progress = (0, 0)
            self._param_meta_source = ""
            self._param_meta_cancel = threading.Event()
            cancel = self._param_meta_cancel
            use_cache = cache if dialect.param_metadata_cacheable else None
            self._param_meta_thread = threading.Thread(
                target=self._param_metadata_worker,
                args=(generation, dialect.param_metadata_path,
                      dialect.param_metadata_format, cancel, use_cache),
                name="param-metadata", daemon=True,
            )
            self._param_meta_thread.start()
            return self._param_metadata_status_locked(False)
