"""Minimal ULog reader — enough of PX4's log format to review a flight.

Written against the ULog File Format specification rather than pulled in as a
dependency: the format is small and stable, the app must stay self-contained
for the offline build, and a reader we own cannot break a field release by
changing its parsing rules underneath us. It is validated against the reference
implementation (pyulog) on a real log in tests/test_ulog.py.

What it reads:

* the header and the definitions section (formats, parameters, info keys),
* ``A``/``D`` records for the topics the Flight Review asks for,
* ``L`` and ``C`` records — the log messages PX4 printed during the flight,
* ``O`` dropouts, because a gap in the data is itself a finding.

What it deliberately does not do: appended data (the crash-dump section), and
decoding every topic in the file. A review reads a dozen topics out of a
hundred, and decoding the rest would cost seconds per log for nothing.
"""
from __future__ import annotations

import logging
import struct
from typing import Any, BinaryIO, Iterable

logger = logging.getLogger("corvus.ulog")

MAGIC = b"ULog\x01\x12\x35"
HEADER_BYTES = 16

# Message types in the definitions and data sections.
_FORMAT = ord("F")
_ADD_LOGGED = ord("A")
_REMOVE_LOGGED = ord("R")
_DATA = ord("D")
_INFO = ord("I")
_INFO_MULTI = ord("M")
_PARAMETER = ord("P")
_PARAMETER_DEFAULT = ord("Q")
_LOGGING = ord("L")
_LOGGING_TAGGED = ord("C")
_SYNC = ord("S")
_DROPOUT = ord("O")
_FLAG_BITS = ord("B")

# ULog primitive -> (struct code, byte size). Anything else in a format string
# is a nested message type and is expanded recursively.
_PRIMITIVES: dict[str, tuple[str, int]] = {
    "int8_t": ("b", 1), "uint8_t": ("B", 1),
    "int16_t": ("h", 2), "uint16_t": ("H", 2),
    "int32_t": ("i", 4), "uint32_t": ("I", 4),
    "int64_t": ("q", 8), "uint64_t": ("Q", 8),
    "float": ("f", 4), "double": ("d", 8),
    "bool": ("?", 1), "char": ("c", 1),
}

# PX4 log levels are syslog severities.
_LEVELS = {
    0: "emergency", 1: "alert", 2: "critical", 3: "error",
    4: "warning", 5: "notice", 6: "info", 7: "debug",
}

MAX_FILE_BYTES = 512 * 1024 * 1024


class UlogError(Exception):
    """The file is not a ULog we can read."""


class _Field:
    __slots__ = ("name", "type", "count", "offset", "size")

    def __init__(self, name: str, type_: str, count: int, offset: int, size: int) -> None:
        self.name = name
        self.type = type_
        self.count = count
        self.offset = offset
        self.size = size


def _parse_format(definition: str) -> tuple[str, list[tuple[str, int, str]]]:
    """``"name:uint64_t timestamp;float[3] xyz;"`` -> (name, [(type, count, field)])."""
    name, _, rest = definition.partition(":")
    fields: list[tuple[str, int, str]] = []
    for chunk in rest.split(";"):
        chunk = chunk.strip()
        if not chunk:
            continue
        type_part, _, field_name = chunk.partition(" ")
        count = 1
        if "[" in type_part:
            type_part, _, tail = type_part.partition("[")
            try:
                count = int(tail.rstrip("]"))
            except ValueError:
                count = 1
        fields.append((type_part, count, field_name.strip()))
    return name.strip(), fields


class _Layout:
    """A message format flattened into fixed offsets into the record bytes."""

    def __init__(self, fields: list[_Field], size: int) -> None:
        self.fields = fields
        self.size = size
        self.by_name = {f.name: f for f in fields}


class ULog:
    """A parsed ULog. Construct with :func:`read`."""

    def __init__(self) -> None:
        self.info: dict[str, Any] = {}
        self.params: dict[str, Any] = {}
        self.formats: dict[str, list[tuple[str, int, str]]] = {}
        self.messages: list[dict[str, Any]] = []
        self.dropouts: list[dict[str, Any]] = []
        self.start_timestamp: int = 0
        # topic -> {multi_id -> {field -> [values]}}
        self.data: dict[str, dict[int, dict[str, list]]] = {}
        self.truncated = False

    # ------------------------------------------------------------------

    def series(self, topic: str, field: str, multi_id: int = 0) -> list:
        """One field of one topic instance, or [] when it is not in the log."""
        return self.data.get(topic, {}).get(multi_id, {}).get(field, [])

    def instances(self, topic: str) -> list[int]:
        """Multi-instance ids present for *topic* (e.g. three IMUs)."""
        return sorted(self.data.get(topic, {}))

    def has(self, topic: str) -> bool:
        return bool(self.data.get(topic))


def _build_layout(
    fields: list[tuple[str, int, str]],
    formats: dict[str, list[tuple[str, int, str]]],
    prefix: str = "",
    depth: int = 0,
) -> tuple[list[_Field], int]:
    """Flatten a format (expanding nested types) into offset/size fields."""
    out: list[_Field] = []
    offset = 0
    if depth > 6:      # nested types are shallow in practice; stop runaway
        return out, offset
    for type_, count, name in fields:
        full = f"{prefix}{name}"
        primitive = _PRIMITIVES.get(type_)
        if primitive is not None:
            code, size = primitive
            out.append(_Field(full, type_, count, offset, size))
            offset += size * count
            continue
        nested = formats.get(type_)
        if nested is None:
            # An unknown type makes every following offset a guess, so the
            # whole message is abandoned rather than silently misread.
            raise UlogError(f"unknown field type {type_!r}")
        for index in range(count):
            sub_prefix = f"{full}." if count == 1 else f"{full}[{index}]."
            sub_fields, sub_size = _build_layout(nested, formats, sub_prefix, depth + 1)
            for field in sub_fields:
                field.offset += offset
                out.append(field)
            offset += sub_size
    return out, offset


def _decode(layout: _Layout, raw: bytes) -> dict[str, Any]:
    """Pull every non-padding field out of one data record."""
    values: dict[str, Any] = {}
    for field in layout.fields:
        if field.name.startswith("_padding") or ".._padding" in field.name:
            continue
        code, size = _PRIMITIVES[field.type]
        end = field.offset + size * field.count
        if end > len(raw):
            continue
        if field.type == "char":
            values[field.name] = raw[field.offset:end].split(b"\x00", 1)[0].decode(
                "utf-8", errors="replace")
            continue
        if field.count == 1:
            values[field.name] = struct.unpack_from("<" + code, raw, field.offset)[0]
        else:
            values[field.name] = list(
                struct.unpack_from(f"<{field.count}{code}", raw, field.offset))
    return values


def _read_value(type_: str, raw: bytes) -> Any:
    """Decode an info/parameter value from its declared type."""
    if type_.startswith("char["):
        return raw.split(b"\x00", 1)[0].decode("utf-8", errors="replace")
    primitive = _PRIMITIVES.get(type_)
    if primitive is None:
        return None
    code, size = primitive
    if len(raw) < size:
        return None
    return struct.unpack_from("<" + code, raw, 0)[0]


def read(source: BinaryIO | bytes, topics: Iterable[str] | None = None) -> ULog:
    """Parse a ULog.

    `topics` limits which ones are decoded — a review reads a dozen out of a
    hundred, and decoding the rest costs seconds per log for nothing. None
    decodes everything.

    Raises :class:`UlogError` on a file that is not a readable ULog. A file that
    is merely *truncated* (the aircraft lost power mid-write, which is exactly
    when the log matters most) is not an error: everything up to the cut is
    returned with ``truncated`` set.
    """
    blob = source if isinstance(source, (bytes, bytearray)) else source.read(MAX_FILE_BYTES + 1)
    if len(blob) > MAX_FILE_BYTES:
        raise UlogError("log is too large to review")
    if len(blob) < HEADER_BYTES or blob[:7] != MAGIC:
        raise UlogError("not a ULog file")

    log = ULog()
    log.start_timestamp = struct.unpack_from("<Q", blob, 8)[0]
    wanted = set(topics) if topics is not None else None

    layouts: dict[str, _Layout] = {}          # topic -> layout
    subscriptions: dict[int, tuple[str, int]] = {}   # msg_id -> (topic, multi_id)
    offset = HEADER_BYTES
    total = len(blob)

    while offset + 3 <= total:
        size, msg_type = struct.unpack_from("<HB", blob, offset)
        body_start = offset + 3
        body_end = body_start + size
        if body_end > total:
            # Truncated tail: a power loss mid-write is the case where the log
            # matters most, so keep what was read rather than throwing it away.
            log.truncated = True
            break
        body = blob[body_start:body_end]
        offset = body_end

        try:
            if msg_type == _FORMAT:
                name, fields = _parse_format(body.decode("utf-8", errors="replace"))
                if name:
                    log.formats[name] = fields

            elif msg_type == _ADD_LOGGED:
                multi_id = body[0]
                msg_id = struct.unpack_from("<H", body, 1)[0]
                topic = body[3:].decode("utf-8", errors="replace")
                if wanted is not None and topic not in wanted:
                    continue
                if topic not in layouts:
                    fields = log.formats.get(topic)
                    if fields is None:
                        continue
                    flat, layout_size = _build_layout(fields, log.formats)
                    layouts[topic] = _Layout(flat, layout_size)
                subscriptions[msg_id] = (topic, multi_id)
                log.data.setdefault(topic, {}).setdefault(multi_id, {})

            elif msg_type == _REMOVE_LOGGED:
                subscriptions.pop(struct.unpack_from("<H", body, 0)[0], None)

            elif msg_type == _DATA:
                msg_id = struct.unpack_from("<H", body, 0)[0]
                subscription = subscriptions.get(msg_id)
                if subscription is None:
                    continue
                topic, multi_id = subscription
                values = _decode(layouts[topic], body[2:])
                bucket = log.data[topic][multi_id]
                for key, value in values.items():
                    bucket.setdefault(key, []).append(value)

            elif msg_type in (_INFO, _PARAMETER, _PARAMETER_DEFAULT):
                start = 1
                if msg_type == _PARAMETER_DEFAULT:
                    start = 2          # leading default_types byte
                key_len = body[start - 1] if msg_type != _PARAMETER_DEFAULT else body[1]
                key_start = start if msg_type != _PARAMETER_DEFAULT else 2
                key = body[key_start:key_start + key_len].decode("utf-8", errors="replace")
                type_, _, field_name = key.partition(" ")
                value = _read_value(type_, body[key_start + key_len:])
                target = log.params if msg_type != _INFO else log.info
                if field_name:
                    target[field_name] = value

            elif msg_type == _INFO_MULTI:
                key_len = body[1]
                key = body[2:2 + key_len].decode("utf-8", errors="replace")
                type_, _, field_name = key.partition(" ")
                value = _read_value(type_, body[2 + key_len:])
                if field_name:
                    log.info.setdefault(field_name, value)

            elif msg_type in (_LOGGING, _LOGGING_TAGGED):
                head = 9 if msg_type == _LOGGING else 11
                level = body[0] if msg_type == _LOGGING else body[2]
                stamp = struct.unpack_from("<Q", body, 1 if msg_type == _LOGGING else 3)[0]
                text = body[head:].decode("utf-8", errors="replace").rstrip("\x00")
                log.messages.append({
                    "t": stamp, "level": _LEVELS.get(level, "info"), "text": text,
                })

            elif msg_type == _DROPOUT:
                log.dropouts.append({"duration_ms": struct.unpack_from("<H", body, 0)[0]})

            elif msg_type in (_SYNC, _FLAG_BITS):
                continue

        except (struct.error, IndexError, UnicodeDecodeError, UlogError) as exc:
            # One malformed record must not cost the rest of the flight.
            logger.debug("skipping ULog record type %s: %s", chr(msg_type), exc)
            continue

    return log
