"""Remote ID: the identity a ground station broadcasts for its aircraft.

Remote ID is the one thing on the Setup page that is not a property of the
aircraft. Every other page writes a parameter and the autopilot owns the
answer; here the *operator* owns it. The serial number, the registration the
authority issued, the class the airframe was certified to — none of that is
knowable from the vehicle, and none of it is stored on it either. It is held by
the ground station and pushed down the link, over and over, for as long as the
station is connected. That is not a Corvus decision: ``OPEN_DRONE_ID_BASIC_ID``,
``OPEN_DRONE_ID_OPERATOR_ID``, ``OPEN_DRONE_ID_SELF_ID`` and
``OPEN_DRONE_ID_SYSTEM`` are all GCS -> vehicle messages, and both PX4 and
ArduPilot refuse to arm when the ones they are waiting for stop arriving.

So this module owns three things, and none of them is a parameter:

``settings`` / ``coerce``
    The stored identity, bounded. Everything here reaches the module from a
    browser and leaves it on a radio that a regulator is listening to, so a
    field that is too long, non-ASCII, or out of enum range is corrected on the
    way in rather than sent.

``findings``
    What is wrong with it *as a filing*, which is a different question from
    whether it will encode. A serial number that is 20 valid characters and not
    in ANSI/CTA-2063-A format encodes perfectly and fails an FAA ramp check.
    The rules are named per region (:data:`REGIONS`) and reported as findings
    rather than enforced, because an operator flying under a national exemption
    is not doing anything wrong and must not be locked out of their own page.

``messages``
    The four messages and their exact field values, as ``(name, kwargs)`` pairs
    the bridge sends verbatim. Built here, away from the link, so the encoding —
    degE7 latitudes, the 2019 epoch, the reserved ``id_or_mac``, the -1000 that
    means "not known" — is testable without a vehicle.

``settings``, ``messages``, ``arm_status`` and ``system_timestamp`` are
``corvus_link.remote_id``, in Rust, beside the broadcast that sends them; the
filing checks and the page's option lists stay here.

None of this is legal advice and the module never claims to certify anything.
It reports what the two published broadcast formats ask for; the operator files
the paperwork.
"""

from __future__ import annotations

import datetime
import string
from typing import Any

from corvus_link import remote_id as _wire

# ---------------------------------------------------------------------------
# Enumerations (MAV_ODID_*). Spelled out here rather than imported from
# corvus_link: the values are frozen by the MAVLink standard, this module is
# imported by the HTTP layer with no link open, and a headless install without
# a v2 dialect loaded must still be able to render the page.
# ---------------------------------------------------------------------------

# MAV_ODID_ID_TYPE — what kind of identifier `uas_id` is.
ID_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "None"},
    {"value": 1, "label": "Serial number (ANSI/CTA-2063-A)"},
    {"value": 2, "label": "CAA registration ID"},
    {"value": 3, "label": "UTM-assigned UUID"},
    {"value": 4, "label": "Specific session ID"},
]

# MAV_ODID_UA_TYPE — what the aircraft is. Broadcast as part of the Basic ID.
UA_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Undeclared"},
    {"value": 1, "label": "Aeroplane"},
    {"value": 2, "label": "Helicopter or multirotor"},
    {"value": 3, "label": "Gyroplane"},
    {"value": 4, "label": "Hybrid lift (VTOL)"},
    {"value": 5, "label": "Ornithopter"},
    {"value": 6, "label": "Glider"},
    {"value": 7, "label": "Kite"},
    {"value": 8, "label": "Free balloon"},
    {"value": 9, "label": "Captive balloon"},
    {"value": 10, "label": "Airship"},
    {"value": 11, "label": "Free-fall parachute"},
    {"value": 12, "label": "Rocket"},
    {"value": 13, "label": "Tethered powered aircraft"},
    {"value": 14, "label": "Ground obstacle"},
    {"value": 15, "label": "Other"},
]

# MAV_ODID_OPERATOR_ID_TYPE — one member today; kept as a list so a future
# addition is a row here rather than a new control.
OPERATOR_ID_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "CAA-issued operator registration"},
]

# MAV_ODID_DESC_TYPE — what the free-text Self ID field is saying.
DESCRIPTION_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Flight description"},
    {"value": 1, "label": "Emergency"},
    {"value": 2, "label": "Extended status"},
]

# MAV_ODID_OPERATOR_LOCATION_TYPE — where the position in the System message
# comes from. LIVE_GNSS is deliberately absent from the offered list: it means
# "this position is the control station's own receiver, updating", and Corvus
# has no GNSS receiver of its own to make that true. See `location_options`.
OPERATOR_LOCATION_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Take-off location"},
    {"value": 1, "label": "Live GNSS of the control station"},
    {"value": 2, "label": "Fixed position"},
]

# MAV_ODID_CLASSIFICATION_TYPE — which regulatory classification scheme, if any,
# the aircraft is declared under.
CLASSIFICATION_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Undeclared"},
    {"value": 1, "label": "European Union"},
]

# MAV_ODID_CATEGORY_EU — the EU operational category.
CATEGORY_EU_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Undeclared"},
    {"value": 1, "label": "Open"},
    {"value": 2, "label": "Specific"},
    {"value": 3, "label": "Certified"},
]

# MAV_ODID_CLASS_EU — the class mark on the airframe. The enum value and the
# class number differ by one (value 1 is class C0), which is exactly the sort
# of off-by-one that would put a C0 aircraft on the air as a C1; the labels
# carry the real class so nothing has to do that arithmetic twice.
CLASS_EU_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Undeclared"},
    {"value": 1, "label": "Class C0 (under 250 g)"},
    {"value": 2, "label": "Class C1 (under 900 g)"},
    {"value": 3, "label": "Class C2 (under 4 kg)"},
    {"value": 4, "label": "Class C3 (under 25 kg)"},
    {"value": 5, "label": "Class C4 (under 25 kg, no automatic control)"},
    {"value": 6, "label": "Class C5"},
    {"value": 7, "label": "Class C6"},
]

# MAV_ODID_ARM_STATUS, as the vehicle reports it back.
ARM_STATUS_GOOD: int = _wire.ARM_STATUS_GOOD
ARM_STATUS_FAIL: int = _wire.ARM_STATUS_FAIL

# Identifier types that identify the *airframe* rather than a flight. A session
# ID is deliberately not one of them: it is valid under Part 89 and useless for
# telling which aircraft flew yesterday.
ID_TYPE_NONE = 0
ID_TYPE_SERIAL = 1
ID_TYPE_CAA = 2
ID_TYPE_UUID = 3
ID_TYPE_SESSION = 4

LOCATION_TAKEOFF = 0
LOCATION_LIVE_GNSS = 1
LOCATION_FIXED = 2

CLASSIFICATION_UNDECLARED = 0
CLASSIFICATION_EU = 1

# Field widths from the MAVLink message definitions. They are the hard cap on
# what can be sent, not a style preference: a 21st character does not truncate
# politely at the far end, it shifts the field.
UAS_ID_MAX: int = _wire.UAS_ID_MAX
OPERATOR_ID_MAX: int = _wire.OPERATOR_ID_MAX
DESCRIPTION_MAX: int = _wire.DESCRIPTION_MAX

# The ODID "unknown" sentinel for the altitudes and the operational volume.
# -1000 m is below the lowest point on land by a margin no aircraft will ever
# fly at, which is why the standard could spend a real value on it.
ALTITUDE_UNKNOWN: float = _wire.ALTITUDE_UNKNOWN

# OPEN_DRONE_ID_SYSTEM.timestamp counts seconds from this epoch, not from 1970.
ODID_EPOCH = datetime.datetime(2019, 1, 1, tzinfo=datetime.UTC)

# ANSI/CTA-2063-A alphabet: digits and capitals, with I and O removed because
# they are indistinguishable from 1 and 0 on a printed label.
_CTA_ALPHABET = frozenset(set(string.digits + string.ascii_uppercase) - {"I", "O"})
# The single character that says how long the serial that follows is: hex 1-F.
_CTA_LENGTH_CODES = "123456789ABCDEF"

# The regions whose broadcast rules this module knows how to check.
REGIONS: tuple[str, ...] = ("faa", "eu")

REGION_LABELS: dict[str, str] = {
    "faa": "United States (FAA Part 89)",
    "eu": "European Union (EU 2019/945, EN 4709-002)",
}


# ---------------------------------------------------------------------------
# Coercion
# ---------------------------------------------------------------------------

def settings(raw: Any) -> dict[str, Any]:
    """Return the stored identity, every field bounded and typed.

    Called on the way out of the config file and again on the way in from the
    page, so the two can never disagree about what a stored value means. An
    enum outside the lists above falls back to its default rather than being
    kept: it came from a browser and would be broadcast as a claim.
    """
    return _wire.settings(raw)


def defaults() -> dict[str, Any]:
    """The identity a station that has never been configured holds."""
    return settings(None)


# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------

def serial_number_problem(value: str) -> str:
    """Why *value* is not an ANSI/CTA-2063-A serial number, or "".

    The format is ``MMMM`` (manufacturer code) + ``L`` (one hex digit giving
    the length of what follows) + that many characters. It is checked here
    rather than left to the authority because it is the single most common
    reason a Remote ID filing is rejected, and because nothing downstream will
    ever tell the operator: an ill-formed serial transmits perfectly and is
    only ever discovered by somebody on the ground with a phone.
    """
    text = str(value or "")
    if not text:
        return "no serial number"
    if len(text) < 6:
        return ("too short: a CTA-2063-A serial is a 4-character manufacturer "
                "code, a length character, then the serial itself")
    if len(text) > UAS_ID_MAX:
        return f"longer than the {UAS_ID_MAX} characters the message carries"
    bad = sorted({c for c in text if c not in _CTA_ALPHABET})
    if bad:
        return ("uses " + ", ".join(repr(c) for c in bad)
                + ". CTA-2063-A allows digits and capitals only, without I or O")
    length_code = text[4]
    if length_code not in _CTA_LENGTH_CODES:
        return (f"the 5th character is the length code and must be 1-9 or A-F, "
                f"not {length_code!r}")
    declared = int(length_code, 16)
    actual = len(text) - 5
    if actual != declared:
        return (f"the length character says {declared} characters follow it, "
                f"but {actual} do")
    return ""


def eu_operator_id_problem(value: str) -> str:
    """Why *value* does not look like an EU operator registration, or "".

    EN 4709-002 registration numbers are a 3-character country code, 12
    characters, and a check character — 16 in all — followed by 3 *secret*
    characters that the holder is issued with and that are deliberately not
    part of the broadcast. Operators paste the whole string, because the whole
    string is what the registration letter shows, and the three characters at
    the end are the ones nobody else is supposed to have.

    That is the finding this function exists for. The structural checks around
    it are secondary, and the check character is not verified at all: a
    national register that formats its numbers differently is the operator's
    business, and a false rejection here would leave them unable to file a
    number that is genuinely theirs.
    """
    text = str(value or "")
    if not text:
        return "no operator registration number"
    stripped = text.replace("-", "")
    if len(stripped) > 16:
        return ("the last characters look like the secret part of your "
                "registration. Only the first 16 are broadcast, and the rest "
                "must not be")
    if len(stripped) < 16:
        return (f"{len(stripped)} characters; an EU operator registration is 16, "
                "not counting the secret part")
    if not text[:3].isalpha():
        return "does not start with a 3-letter country code"
    if not stripped.isalnum():
        return "contains characters an operator registration does not use"
    return ""


def _finding(level: str, field: str, text: str) -> dict[str, str]:
    return {"level": level, "field": field, "text": text}


def findings(identity: dict[str, Any], region: str | None = None) -> list[dict[str, str]]:
    """What is missing or wrong in *identity* for *region*.

    ``level`` is ``"error"`` for something the region's broadcast format asks
    for and does not have, and ``"warning"`` for something that will transmit
    but is probably not what the operator meant. Nothing here blocks a save:
    an aircraft flown under an exemption, or a station being set up before the
    registration has come back, are both legitimate and both incomplete.
    """
    resolved = settings(identity)
    area = (region or resolved["region"] or "eu").lower()
    if area not in REGIONS:
        area = "eu"
    basic = resolved["basic_id"]
    operator = resolved["operator_id"]
    system = resolved["system"]
    out: list[dict[str, str]] = []

    # --- Basic ID, both regions --------------------------------------------
    if not basic["uas_id"]:
        out.append(_finding("error", "uas_id",
                            "No aircraft ID. Nothing identifies the airframe."))
    elif basic["id_type"] == ID_TYPE_SERIAL:
        problem = serial_number_problem(basic["uas_id"])
        if problem:
            out.append(_finding("error", "uas_id", f"Serial number {problem}."))
    if basic["id_type"] == ID_TYPE_NONE and basic["uas_id"]:
        out.append(_finding("error", "id_type",
                            "The ID type is None, so the ID is broadcast as "
                            "meaning nothing."))
    if basic["ua_type"] == 0:
        out.append(_finding("warning", "ua_type",
                            "The aircraft type is undeclared."))

    # --- Region-specific ----------------------------------------------------
    if area == "faa":
        if basic["id_type"] not in (ID_TYPE_SERIAL, ID_TYPE_SESSION):
            out.append(_finding(
                "error", "id_type",
                "Part 89 broadcasts either a CTA-2063-A serial number or a "
                "session ID; this is neither."))
        if system["operator_location_type"] == LOCATION_TAKEOFF:
            out.append(_finding(
                "error", "operator_location_type",
                "Part 89 broadcasts the control station's position. The "
                "take-off point is not it."))
    else:
        if not operator["operator_id"]:
            out.append(_finding(
                "error", "operator_id",
                "No operator registration number. EU direct remote ID "
                "broadcasts the operator, not only the aircraft."))
        else:
            problem = eu_operator_id_problem(operator["operator_id"])
            if problem:
                level = "error" if "secret" in problem else "warning"
                out.append(_finding(level, "operator_id",
                                    f"Operator registration {problem}."))
        if basic["id_type"] != ID_TYPE_SERIAL:
            out.append(_finding(
                "warning", "id_type",
                "EU direct remote ID expects the UAS serial number as the "
                "aircraft ID."))
        if system["classification_type"] != CLASSIFICATION_EU:
            out.append(_finding(
                "warning", "classification_type",
                "The EU classification is undeclared, so no category or class "
                "is broadcast."))
        else:
            if system["category_eu"] == 0:
                out.append(_finding("error", "category_eu",
                                    "EU classification is declared but the "
                                    "operational category is not."))
            if system["class_eu"] == 0 and system["category_eu"] == 1:
                out.append(_finding("warning", "class_eu",
                                    "An Open-category aircraft normally carries "
                                    "a class mark (C0-C6)."))

    # --- Fixed operator position -------------------------------------------
    if system["operator_location_type"] == LOCATION_FIXED:
        if not system["operator_latitude"] and not system["operator_longitude"]:
            out.append(_finding("error", "operator_latitude",
                                "The operator position is fixed but sits at "
                                "0°N 0°E."))
    if system["operator_location_type"] == LOCATION_LIVE_GNSS:
        out.append(_finding(
            "error", "operator_location_type",
            "Live GNSS means the control station's own receiver, and Corvus "
            "has none. Use a fixed position or the take-off point."))
    return out


def location_options(gnss_available: bool = False) -> list[dict[str, Any]]:
    """The operator-location list, with what cannot be honoured marked.

    LIVE_GNSS stays in the list rather than being dropped (AGENTS.md: a
    capability that is missing is reported, not hidden) — an operator who knows
    the standard has that option needs to see why it is not offered rather than
    wonder whether Corvus forgot it.
    """
    out = []
    for option in OPERATOR_LOCATION_TYPE_OPTIONS:
        row = dict(option)
        if option["value"] == LOCATION_LIVE_GNSS and not gnss_available:
            row["disabled"] = True
            row["reason"] = "Corvus has no GNSS receiver of its own"
        out.append(row)
    return out


def schema(gnss_available: bool = False) -> dict[str, Any]:
    """Everything the page needs to render the form without knowing the standard."""
    return {
        "id_types": ID_TYPE_OPTIONS,
        "ua_types": UA_TYPE_OPTIONS,
        "operator_id_types": OPERATOR_ID_TYPE_OPTIONS,
        "description_types": DESCRIPTION_TYPE_OPTIONS,
        "location_types": location_options(gnss_available),
        "classification_types": CLASSIFICATION_TYPE_OPTIONS,
        "categories_eu": CATEGORY_EU_OPTIONS,
        "classes_eu": CLASS_EU_OPTIONS,
        "regions": [{"value": r, "label": REGION_LABELS[r]} for r in REGIONS],
        "limits": {
            "uas_id": UAS_ID_MAX,
            "operator_id": OPERATOR_ID_MAX,
            "description": DESCRIPTION_MAX,
        },
        "altitude_unknown": ALTITUDE_UNKNOWN,
    }


def ua_type_for_vehicle(mav_type: int) -> int:
    """The MAV_ODID_UA_TYPE that matches a MAV_TYPE, or 0 when none does.

    Offered to the operator as a suggestion, never written on their behalf:
    what the airframe *is* and what it was *registered as* are two different
    facts, and only the second one is legally interesting.
    """
    # MAV_TYPE values, spelled out for the same reason the ODID enums are.
    mapping = {
        1: 1,    # FIXED_WING            -> Aeroplane
        2: 2,    # QUADROTOR             -> Helicopter or multirotor
        3: 2,    # COAXIAL
        4: 2,    # HELICOPTER
        13: 2,   # HEXAROTOR
        14: 2,   # OCTOROTOR
        15: 2,   # TRICOPTER
        7: 10,   # AIRSHIP               -> Airship
        8: 8,    # FREE_BALLOON          -> Free balloon
        9: 12,   # ROCKET                -> Rocket
        16: 5,   # FLAPPING_WING         -> Ornithopter
        19: 4,   # VTOL_TAILSITTER_DUOROTOR
        20: 4,   # VTOL_TAILSITTER_QUADROTOR
        21: 4,   # VTOL_TILTROTOR
        22: 4,   # VTOL_FIXEDROTOR
        23: 4,   # VTOL_TAILSITTER
        24: 4,   # VTOL_TILTWING
        25: 4,   # VTOL_RESERVED5
    }
    try:
        return int(mapping.get(int(mav_type), 0))
    except (TypeError, ValueError):
        return 0


# ---------------------------------------------------------------------------
# Message building
# ---------------------------------------------------------------------------

def system_timestamp(now: datetime.datetime | None = None) -> int:
    """Seconds since the ODID epoch (2019-01-01 UTC), clamped to uint32."""
    return _wire.system_timestamp(now)


def messages(identity: dict[str, Any], *, target_system: int, target_component: int,
             now: datetime.datetime | None = None) -> list[tuple[str, dict[str, Any]]]:
    """The Remote ID messages to send, as ``(message name, kwargs)`` pairs.

    All four go out together on every cycle: both stacks time the last one
    out and fail the pre-arm check when the stream stops. ``id_or_mac`` is 20
    reserved zero bytes, the operator position goes out only when it is a
    fixed one, and the EU category and class only under the EU
    classification.
    """
    return _wire.messages(identity, target_system=target_system,
                          target_component=target_component, now=now)


def arm_status(status: Any, error: Any) -> dict[str, Any]:
    """Normalise an ``OPEN_DRONE_ID_ARM_STATUS`` into something renderable."""
    return _wire.arm_status(status, error)
