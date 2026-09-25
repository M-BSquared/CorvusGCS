"""Parameter metadata from the vehicle: defaults, and on PX4 the documentation.

The parameter protocol carries a name, a value and a type. It has no field for
what the value was out of the box, so a ground station that shows defaults
reads them from a file the autopilot keeps on board, over MAVLink FTP. That is
where QGroundControl (PX4) and Mission Planner (ArduPilot) get them too:

* PX4 builds ``parameters.json.xz`` into its ROMFS: every parameter's default,
  description, unit, range and value labels, for exactly the firmware that is
  running.
* ArduPilot 4.1 and later generate ``@PARAM/param.pck`` on request; with
  ``withdefaults=1`` each entry carries its default beside its value.

Which of the two the connected vehicle has is the dialect's to say
(:mod:`corvus.autopilot`); this module reads the files and runs the fetch.
"""
from __future__ import annotations

import json
import logging
import lzma
import os
import re
import struct
import tempfile
import threading
from pathlib import Path
from typing import Any

from .mavlink_ftp import FTP_ERR_FILE_NOT_FOUND, FTP_ERR_UNKNOWN_COMMAND, FtpError

logger = logging.getLogger("corvus.mavlink")

METADATA_PX4_JSON = "px4-json"
METADATA_ARDUPILOT_PCK = "ardupilot-pck"

# PX4's uncompressed metadata is a few MB; anything far past that is not it.
_MAX_DECOMPRESSED_BYTES = 48 * 1024 * 1024
_XZ_MAGIC = b"\xfd7zXZ\x00"

_PCK_MAGIC = 0x671B
_PCK_MAGIC_DEFAULTS = 0x671C
_PCK_TYPES = {1: "<b", 2: "<h", 3: "<i", 4: "<f"}


def _number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError:
            return None
    return None


def _text(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def _decompress(raw: bytes) -> bytes:
    if not raw.startswith(_XZ_MAGIC):
        return raw
    decoder = lzma.LZMADecompressor()
    out = decoder.decompress(raw, max_length=_MAX_DECOMPRESSED_BYTES)
    if not decoder.eof:
        raise ValueError("the metadata file is truncated or too large")
    return out


def parse_px4_metadata(raw: bytes) -> dict[str, dict[str, Any]]:
    """PX4 ``parameters.json(.xz)`` -> ``{name: {default, short_desc, ...}}``.

    Only the keys a parameter actually carries are set, so the editor can
    tell "no unit" from an empty one.
    """
    doc = json.loads(_decompress(raw).decode("utf-8"))
    entries = doc.get("parameters") if isinstance(doc, dict) else None
    if not isinstance(entries, list):
        raise ValueError("not a PX4 parameter metadata file")
    out: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        name = _text(entry.get("name"))
        if not name:
            continue
        meta: dict[str, Any] = {}
        for src, dst in (("default", "default"), ("min", "min"),
                         ("max", "max"), ("increment", "increment")):
            number = _number(entry.get(src))
            if number is not None:
                meta[dst] = number
        for src, dst in (("shortDesc", "short_desc"), ("longDesc", "long_desc"),
                         ("units", "units"), ("group", "group")):
            text = _text(entry.get(src))
            if text:
                meta[dst] = text
        decimals = entry.get("decimalPlaces")
        if isinstance(decimals, int) and not isinstance(decimals, bool) and 0 <= decimals <= 9:
            meta["decimals"] = decimals
        if entry.get("rebootRequired") is True:
            meta["reboot_required"] = True
        values = []
        for item in entry.get("values") or []:
            if isinstance(item, dict):
                number = _number(item.get("value"))
                if number is not None:
                    values.append([number, _text(item.get("description"))])
        if values:
            meta["values"] = values
        bits = []
        for item in entry.get("bitmask") or []:
            if isinstance(item, dict) and isinstance(item.get("index"), int):
                bits.append([item["index"], _text(item.get("description"))])
        if bits:
            meta["bitmask"] = bits
        out[name] = meta
    return out


def parse_ardupilot_pck(raw: bytes) -> dict[str, dict[str, Any]]:
    """ArduPilot ``@PARAM/param.pck`` -> ``{name: {"default": value}}``.

    The pack is a 6 byte header (magic, parameters in this file, total) and
    then one entry per parameter: a byte of type (low nibble) and flags (high
    nibble), a byte of name length and the prefix it shares with the previous
    name, the rest of the name, the value, and the default when flag 1 is set.
    An entry without that flag is at its default. Zero bytes between entries
    are padding. A pack without the defaults magic carries no defaults, which
    is an empty answer, not an error.
    """
    if len(raw) < 6:
        raise ValueError("the parameter pack is truncated")
    magic, count, _total = struct.unpack_from("<HHH", raw, 0)
    if magic not in (_PCK_MAGIC, _PCK_MAGIC_DEFAULTS):
        raise ValueError("not an ArduPilot parameter pack")
    with_defaults = magic == _PCK_MAGIC_DEFAULTS
    out: dict[str, dict[str, Any]] = {}
    pos = 6
    last = b""
    seen = 0
    while pos < len(raw):
        if raw[pos] == 0:
            pos += 1
            continue
        if pos + 2 > len(raw):
            raise ValueError("the parameter pack is truncated")
        type_byte, len_byte = raw[pos], raw[pos + 1]
        fmt = _PCK_TYPES.get(type_byte & 0x0F)
        if fmt is None:
            raise ValueError(f"unknown parameter type {type_byte & 0x0F} in the pack")
        width = struct.calcsize(fmt)
        name_len = (len_byte >> 4) + 1
        common = len_byte & 0x0F
        has_default = with_defaults and bool((type_byte >> 4) & 1)
        end = pos + 2 + name_len + width * (2 if has_default else 1)
        if end > len(raw) or common > len(last):
            raise ValueError("the parameter pack is truncated")
        name_bytes = last[:common] + raw[pos + 2:pos + 2 + name_len]
        value_at = pos + 2 + name_len
        value = struct.unpack_from(fmt, raw, value_at)[0]
        default = struct.unpack_from(fmt, raw, value_at + width)[0] if has_default else value
        last = name_bytes
        pos = end
        seen += 1
        name = name_bytes.decode("ascii", "replace")
        out[name] = {"default": float(default)} if with_defaults else {}
    if seen != count:
        raise ValueError(f"the parameter pack lists {count} parameters but holds {seen}")
    return out


def parse_param_metadata(fmt: str, raw: bytes) -> dict[str, dict[str, Any]]:
    """Parse a metadata file in the format the dialect named."""
    if fmt == METADATA_PX4_JSON:
        return parse_px4_metadata(raw)
    if fmt == METADATA_ARDUPILOT_PCK:
        return parse_ardupilot_pck(raw)
    raise ValueError(f"unknown metadata format {fmt!r}")


def _fetch_error_text(exc: FtpError) -> str:
    if exc.code == FTP_ERR_FILE_NOT_FOUND:
        return "this firmware was built without parameter metadata"
    if exc.code == FTP_ERR_UNKNOWN_COMMAND:
        return "the autopilot does not support MAVLink FTP"
    return str(exc)


def _mtime(path: Path) -> float:
    try:
        return path.stat().st_mtime
    except OSError:
        return 0.0


class ParamMetadataCache:
    """Metadata files kept on the ground station, one per vehicle checksum.

    Opt-in (``parameters.cache_defaults`` in the config), and only for a
    stack whose file is fixed when the firmware is built (PX4). A copy is
    used only when the vehicle reports, over MAVLink FTP, the same CRC32 for
    its file that it reported when the copy was made; that check costs one
    round trip where the download costs the whole file, which on a telemetry
    radio is the difference between an instant and half a minute.

    What this cannot know is written on the switch that turns it on: a CRC32
    is 32 bits, so two different files can share one, and a copy that matches
    is then the wrong firmware's. The copies are raw files as the vehicle sent
    them and are parsed exactly like a download; a copy that no longer parses
    is deleted.
    """

    MAX_FILES = 12
    _NAME = re.compile(r"^[a-z0-9-]+-[0-9a-f]{8}\.bin$")

    def __init__(self, directory: str | os.PathLike[str]) -> None:
        self.directory = Path(directory)

    def _path(self, fmt: str, crc: int) -> Path:
        tag = re.sub(r"[^a-z0-9-]", "", fmt.lower()) or "metadata"
        return self.directory / f"{tag}-{crc & 0xFFFFFFFF:08x}.bin"

    def load(self, fmt: str, crc: int) -> bytes | None:
        """The copy for *crc*, or None."""
        path = self._path(fmt, crc)
        try:
            raw = path.read_bytes()
        except OSError:
            return None
        try:
            os.utime(path)   # most recently used survives the pruning
        except OSError:
            pass
        return raw or None

    def store(self, fmt: str, crc: int, raw: bytes) -> None:
        """Keep *raw* for *crc*, atomically; the oldest copies beyond MAX_FILES go."""
        path = self._path(fmt, crc)
        try:
            self.directory.mkdir(parents=True, exist_ok=True)
            fd, tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp",
                                       dir=str(self.directory))
            try:
                with os.fdopen(fd, "wb") as handle:
                    handle.write(raw)
                os.replace(tmp, path)
            except BaseException:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass
                raise
        except OSError as exc:
            logger.warning("could not keep a copy of the parameter metadata: %s", exc)
            return
        files = sorted(self._files(), key=_mtime, reverse=True)
        for old in files[self.MAX_FILES:]:
            try:
                old.unlink()
            except OSError:
                pass

    def discard(self, fmt: str, crc: int) -> None:
        try:
            self._path(fmt, crc).unlink()
        except OSError:
            pass

    def _files(self) -> list[Path]:
        try:
            return [p for p in self.directory.iterdir()
                    if p.is_file() and self._NAME.match(p.name)]
        except OSError:
            return []

    def info(self) -> dict[str, Any]:
        """``{"dir","files","bytes"}`` for the settings page."""
        files = self._files()
        size = 0
        for p in files:
            try:
                size += p.stat().st_size
            except OSError:
                pass
        return {"dir": str(self.directory), "files": len(files), "bytes": size}

    def clear(self) -> int:
        """Delete every copy; returns how many went."""
        removed = 0
        for p in self._files():
            try:
                p.unlink()
                removed += 1
            except OSError:
                pass
        return removed


class ParamMetadataMixin:
    """Fetch the connected vehicle's parameter metadata once per link cycle."""

    def _init_param_metadata_state(self) -> None:
        """Set up the metadata state; called from the bridge's __init__."""
        self._param_meta_lock = threading.Lock()
        self._param_meta_state = "idle"      # idle | loading | ready | unavailable
        self._param_meta_error = ""
        self._param_meta: dict[str, dict[str, Any]] = {}
        self._param_meta_progress = (0, 0)
        self._param_meta_source = ""          # "vehicle" | "cache" once ready
        self._param_meta_thread: threading.Thread | None = None
        self._param_meta_cancel = threading.Event()
        # Bumped by every reset, so a fetch that outlived its link cycle
        # cannot publish metadata for a vehicle that is no longer there.
        self._param_meta_generation = 0

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

    def param_metadata_status(self, include_params: bool = False) -> dict[str, Any]:
        """``{"state","error","received","size","count"}``, plus ``params`` when asked and ready."""
        with self._param_meta_lock:
            return self._param_metadata_status_locked(include_params)

    def _param_metadata_status_locked(self, include_params: bool) -> dict[str, Any]:
        received, size = self._param_meta_progress
        status: dict[str, Any] = {
            "state": self._param_meta_state,
            "error": self._param_meta_error,
            "received": received,
            "size": size,
            "count": len(self._param_meta),
            "source": self._param_meta_source,
        }
        if include_params and self._param_meta_state == "ready":
            status["params"] = self._param_meta
        return status

    def _reset_param_metadata(self) -> None:
        """Forget the metadata and cancel a fetch; a new link may be a new vehicle."""
        with self._param_meta_lock:
            self._param_meta_generation += 1
            self._param_meta_state = "idle"
            self._param_meta_error = ""
            self._param_meta = {}
            self._param_meta_progress = (0, 0)
            self._param_meta_source = ""
            self._param_meta_cancel.set()
        self.abort_ftp()

    def _stop_param_metadata(self) -> None:
        """Cancel and join the fetch thread; part of the bridge's stop()."""
        self._reset_param_metadata()
        thread = self._param_meta_thread
        if thread is not None:
            thread.join(timeout=2)
            self._param_meta_thread = None

    def _param_metadata_worker(
        self, generation: int, path: str, fmt: str, cancel: threading.Event,
        cache: ParamMetadataCache | None = None,
    ) -> None:
        def progress(received: int, size: int) -> None:
            with self._param_meta_lock:
                if generation == self._param_meta_generation:
                    self._param_meta_progress = (received, size)

        meta: dict[str, dict[str, Any]] = {}
        error = ""
        source = "vehicle"
        try:
            crc = self._param_metadata_checksum(path, cancel) if cache is not None else None
            raw = cache.load(fmt, crc) if cache is not None and crc else None
            if raw is not None:
                try:
                    meta = parse_param_metadata(fmt, raw)
                    source = "cache"
                except (ValueError, lzma.LZMAError, UnicodeDecodeError):
                    logger.info("discarding an unreadable parameter metadata copy")
                    cache.discard(fmt, crc)
                    raw = None
            if raw is None:
                raw = self.ftp_read_file(path, progress=progress, cancel=cancel)
                meta = parse_param_metadata(fmt, raw)
                if cache is not None and crc:
                    cache.store(fmt, crc, raw)
            if not any("default" in m for m in meta.values()):
                error = "the vehicle reports no parameter defaults"
        except FtpError as exc:
            error = _fetch_error_text(exc)
        except (ValueError, lzma.LZMAError, UnicodeDecodeError) as exc:
            error = f"could not read the parameter metadata: {exc}"
        except Exception as exc:  # noqa: BLE001 - a worker must end in a state, never die silently
            logger.exception("parameter metadata fetch failed")
            error = f"could not read the parameter metadata: {exc}"
        with self._param_meta_lock:
            if generation != self._param_meta_generation:
                return
            if error:
                self._param_meta_state = "unavailable"
                self._param_meta_error = error
                self._param_meta = {}
            else:
                self._param_meta_state = "ready"
                self._param_meta = meta
                self._param_meta_source = source
        if error:
            logger.info("parameter metadata unavailable: %s", error)
        else:
            logger.info("parameter metadata: %d parameters from %s (%s)",
                        len(meta), path, source)

    def _param_metadata_checksum(self, path: str, cancel: threading.Event) -> int | None:
        """The vehicle's CRC32 of its metadata file, or None to skip the copies.

        A missing file is re-raised: it is the same answer the download would
        give, and asking twice would only double the wait. Anything else (a
        firmware without the checksum command, a lost reply) means no copy can
        be trusted, so the file is downloaded as if the option were off. Zero
        is refused as a key: a firmware that answers it for every file would
        make every copy match.
        """
        try:
            crc = self.ftp_file_crc32(path, cancel=cancel)
        except FtpError as exc:
            if exc.code == FTP_ERR_FILE_NOT_FOUND or str(exc) == "cancelled":
                raise
            logger.info("no checksum for the parameter metadata (%s); downloading it", exc)
            return None
        return crc or None
