"""PX4 Remote ID parameter schema for the Setup -> Remote ID page.

The vehicle half of Remote ID, and it is deliberately small. The identity — the
serial number, the operator registration, the EU class — is not stored on a PX4
aircraft at all: it arrives over the link as ``OPEN_DRONE_ID_*`` messages from
whatever ground station is flying it (see :mod:`corvus.remote_id`). What the
autopilot owns is one decision: whether a Remote ID system that is missing,
silent or unhappy is allowed to stop an arm.

That is ``COM_ARM_ODID``, and on the target firmwares it is the whole schema.
Same contract as every other setup page: the field is a *candidate*, so a build
without the parameter renders one field fewer rather than an error (AGENTS.md:
graceful fallback across PX4 v1.16 / v1.17 / v1.18), and an enum value this
build does not recognise survives as "Unknown (n)" rather than being snapped to
something the aircraft never held.

The field builders come from :mod:`corvus.param_fields`, which was extracted for
the ArduPilot schemas. A fifth private copy of ``enum``/``present`` here would
have been a fifth thing to keep in step with the frontend's field shape.
"""

from __future__ import annotations

from typing import Any

from .param_fields import enum, present, section

# COM_ARM_ODID — how hard the arm check leans on the Remote ID system.
ARM_ODID_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Warn if unavailable"},
    {"value": 2, "label": "Required to arm"},
]

_CANDIDATES: tuple[str, ...] = ("COM_ARM_ODID",)


def param_names() -> list[str]:
    """Every parameter this page may read, in one batch."""
    return list(_CANDIDATES)


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the vehicle half of the Remote ID page.

    Returns ``{"sections": [...], "received": n}`` in the same shape the Safety
    and Motors pages use, so the frontend renders it with the shared
    schema-driven form and never learns which stack built it.
    """
    fields = present([
        enum("COM_ARM_ODID", "Remote ID arming check", values, ARM_ODID_OPTIONS,
             hint="Whether a Remote ID system that is absent or unhealthy blocks "
                  "arming. Required is what a jurisdiction that mandates Remote ID "
                  "expects; Warn lets the aircraft fly and says so."),
    ])
    sections = [s for s in (
        section("vehicle", "On the aircraft", fields,
                "PX4 keeps one Remote ID setting of its own. Everything the aircraft "
                "broadcasts about you and about itself comes from this ground station "
                "over the link."),
    ) if s is not None]
    return {"sections": sections, "received": len(values)}
