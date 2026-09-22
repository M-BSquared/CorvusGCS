"""Field builders shared by the ArduPilot setup-page schemas.

The PX4 schema modules (:mod:`corvus.safety_config`, :mod:`corvus.tuning_config`,
:mod:`corvus.rc_config`, :mod:`corvus.motor_config`) each grew their own private
copy of these four functions. Rather than add four more copies for the ArduPilot
side, the ArduPilot modules share this one — same output shape, so the frontend
cannot tell which stack built a page.

The shape is the contract: a field is ``{param, label, kind, value, …}`` and the
frontend renders it without knowing what the parameter means. ``None`` means
"this firmware did not answer for that parameter", and a caller drops it —
which is what makes one schema serve several firmware versions.
"""
from __future__ import annotations

from typing import Any


def enum_options(options: list[dict[str, Any]], value: float) -> list[dict[str, Any]]:
    """Options guaranteed to contain *value*.

    A firmware may use an enum member this build has never heard of. Snapping
    such a value to the first option would rewrite a real aircraft's failsafe
    action the moment the operator touched an unrelated field, so the unknown
    number is appended as its own option instead.
    """
    ivalue = int(round(value))
    if any(int(o["value"]) == ivalue for o in options):
        return options
    return options + [{"value": ivalue, "label": f"Unknown ({ivalue})"}]


def enum(name: str, label: str, values: dict[str, float],
         options: list[dict[str, Any]], **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    value = values[name]
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "enum",
        "value": value, "options": enum_options(options, value),
    }
    field.update(extra)
    return field


def number(name: str, label: str, values: dict[str, float],
           **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "number", "value": values[name],
    }
    field.update(extra)
    return field


def bitmask(name: str, label: str, values: dict[str, float],
            bits: list[dict[str, Any]], **extra: Any) -> dict[str, Any] | None:
    """A parameter whose bits are independent switches.

    ArduPilot uses these far more than PX4 does — ``FENCE_TYPE``, ``FS_OPTIONS``
    and ``ARMING_CHECK`` are all bitmasks — and rendering one as a number asks
    an operator to do binary arithmetic on their aircraft's safety settings.
    """
    if name not in values:
        return None
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "bitmask",
        "value": values[name], "bits": bits,
    }
    field.update(extra)
    return field


def present(fields: list[dict[str, Any] | None]) -> list[dict[str, Any]]:
    return [f for f in fields if f is not None]


def option_label(options: list[dict[str, Any]], value: float) -> str:
    ivalue = int(round(value))
    for option in options:
        if int(option["value"]) == ivalue:
            return str(option["label"])
    return f"Unknown ({ivalue})"


def section(section_id: str, title: str, fields: list[dict[str, Any]],
            hint: str = "", **extra: Any) -> dict[str, Any] | None:
    """A plain-form section, or None when nothing in it survived."""
    if not fields:
        return None
    out: dict[str, Any] = {
        "id": section_id, "title": title, "kind": "fields", "fields": fields,
    }
    if hint:
        out["hint"] = hint
    out.update(extra)
    return out
