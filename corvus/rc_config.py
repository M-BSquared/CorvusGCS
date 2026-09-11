"""PX4 radio-control schema for the Setup -> Radio Control page.

Same split as :mod:`corvus.safety_config`, :mod:`corvus.motor_config` and
:mod:`corvus.tuning_config`: this module owns the *knowledge* of which PX4
parameters make up a transmitter setup, the bridge only fetches raw values, and
the HTTP layer only serialises. The frontend renders whatever description it is
handed and never hardcodes a PX4 parameter name.

The page is split the way an operator sets a transmitter up:

``input``     which input the vehicle listens to at all, and what it does when
              that input stops arriving
``sticks``    which channel carries roll, pitch, yaw and throttle
``modes``     the flight-mode switch and the six modes its positions select
``switches``  every other switch and knob PX4 can bind — arm, kill, return,
              the AUX passthrough channels
``channels``  the per-channel calibration numbers themselves

Calibration is not a form
-------------------------
PX4 has no "calibrate RC" command. Unlike the accelerometer or the compass —
where the autopilot runs the procedure and narrates it over STATUSTEXT — an RC
calibration is entirely the ground station's job: watch ``RC_CHANNELS`` while
the operator sweeps every control, then write the endpoints it observed into
``RC<n>_MIN`` / ``RC<n>_MAX`` / ``RC<n>_TRIM`` / ``RC<n>_REV``. That is what
QGroundControl does and it is what the wizard in ``setup-control.js`` does; the
measured result arrives here as :func:`calibration_writes`, which is the one
place that decides what a measurement is allowed to become.

That function is deliberately strict. A transmitter whose throttle channel was
never moved would otherwise be written with ``min == max``, which PX4 accepts
and which turns the stick into a step function at the centre — full throttle
above it, none below. So a channel whose measured travel is implausibly small
is rejected by name rather than silently written.
"""

from __future__ import annotations

from typing import Any

# PX4 supports up to 18 RC channels (RC_CHANNELS carries 18 raw values, and the
# RC<n>_* parameter families are defined for 1..18).
MAX_CHANNELS = 18

# Sane pulse-width bounds for a hobby RC link. Anything outside them is a
# receiver fault or a measurement taken while the receiver was in failsafe, not
# a transmitter endpoint worth writing to the vehicle.
PWM_ABS_MIN = 500
PWM_ABS_MAX = 2500

# The narrowest travel a channel may have and still be treated as calibrated.
# A three-position switch spans ~800 us; a stick spans ~1000. 200 us is far
# below anything real, and well above the jitter of a receiver sitting still.
MIN_TRAVEL_US = 200

# RC<n>_REV is a float that carries only its sign: 1.0 normal, -1.0 reversed.
REV_NORMAL = 1.0
REV_REVERSED = -1.0


# ---------------------------------------------------------------------------
# Option tables
# ---------------------------------------------------------------------------

# COM_RC_IN_MODE — which input the vehicle accepts. Worth naming precisely: the
# difference between "RC and Joystick" and "Stick input disabled" is whether a
# transmitter can take an autonomous flight back.
RC_IN_MODE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "RC transmitter only"},
    {"value": 1, "label": "Joystick only"},
    {"value": 2, "label": "RC and joystick, whichever is present"},
    {"value": 3, "label": "Stick input disabled"},
    {"value": 4, "label": "Stick input with fallback to the other"},
]

# NAV_RCL_ACT — what happens when the transmitter stops being heard.
RC_LOSS_ACTION_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled — keep flying"},
    {"value": 1, "label": "Hold position"},
    {"value": 2, "label": "Return to launch"},
    {"value": 3, "label": "Land at the current position"},
    {"value": 5, "label": "Terminate flight"},
    {"value": 6, "label": "Lockdown (stop the motors)"},
]

# COM_FLTMODE1..6 — the mode each position of the mode switch selects. -1 is
# PX4's "this slot is unused", which is not the same as Manual and must stay a
# distinct choice.
FLIGHT_MODE_OPTIONS: list[dict[str, Any]] = [
    {"value": -1, "label": "Unassigned"},
    {"value": 0, "label": "Manual"},
    {"value": 1, "label": "Altitude"},
    {"value": 2, "label": "Position"},
    {"value": 3, "label": "Mission"},
    {"value": 4, "label": "Hold"},
    {"value": 5, "label": "Return"},
    {"value": 6, "label": "Acro"},
    {"value": 7, "label": "Offboard"},
    {"value": 8, "label": "Stabilized"},
    {"value": 10, "label": "Takeoff"},
    {"value": 11, "label": "Land"},
    {"value": 12, "label": "Follow Me"},
    {"value": 13, "label": "Precision Land"},
]


def channel_options(count: int = MAX_CHANNELS) -> list[dict[str, Any]]:
    """The RC_MAP_* option list: "Unassigned" plus one entry per channel."""
    limit = max(1, min(int(count or MAX_CHANNELS), MAX_CHANNELS))
    options: list[dict[str, Any]] = [{"value": 0, "label": "Unassigned"}]
    options.extend({"value": n, "label": f"Channel {n}"} for n in range(1, limit + 1))
    return options


# ---------------------------------------------------------------------------
# Field tables
#
# One tuple per field: (parameter, label, hint). Kept as data so the four stick
# axes and the switch list stay visibly parallel — a mapping worded differently
# on yaw than on roll is a line you read past.
# ---------------------------------------------------------------------------

STICK_FIELDS: list[tuple[str, str, str]] = [
    ("RC_MAP_ROLL", "Roll", "The channel the right stick's left/right axis sends on."),
    ("RC_MAP_PITCH", "Pitch", "The channel the right stick's forward/back axis sends on."),
    ("RC_MAP_YAW", "Yaw", "The channel the left stick's left/right axis sends on."),
    ("RC_MAP_THROTTLE", "Throttle",
     "The channel the left stick's forward/back axis sends on."),
]

# The mode switch itself. RC_MAP_FLTMODE is the single-channel, six-position
# switch every PX4 since v1.9 uses; the older per-mode switches are in
# SWITCH_FIELDS below and simply do not answer on a modern firmware.
MODE_SWITCH_FIELDS: list[tuple[str, str, str]] = [
    ("RC_MAP_FLTMODE", "Flight mode channel",
     "One channel whose six positions select the six modes below. This is the "
     "modern PX4 mapping; leave it unassigned only if you bind individual mode "
     "switches instead."),
]

MODE_SLOT_FIELDS: list[tuple[str, str, str]] = [
    ("COM_FLTMODE1", "Position 1 (switch fully down)", ""),
    ("COM_FLTMODE2", "Position 2", ""),
    ("COM_FLTMODE3", "Position 3", ""),
    ("COM_FLTMODE4", "Position 4", ""),
    ("COM_FLTMODE5", "Position 5", ""),
    ("COM_FLTMODE6", "Position 6 (switch fully up)", ""),
]

# Everything else PX4 can bind to a channel. The list is a superset across
# v1.16 / v1.17 / v1.18 on purpose: a firmware that dropped a per-mode switch
# simply never answers for it and the field disappears (AGENTS.md).
SWITCH_FIELDS: list[tuple[str, str, str]] = [
    ("RC_MAP_ARM_SW", "Arm / disarm switch",
     "Arms on the high position. PX4 still requires the safety switch and the "
     "prearm checks to pass."),
    ("RC_MAP_KILL_SW", "Kill switch",
     "Stops the motors instantly, in flight, with no landing. Bind it to a "
     "guarded switch or not at all."),
    ("RC_MAP_RETURN_SW", "Return switch", "Triggers return-to-launch."),
    ("RC_MAP_LOITER_SW", "Hold switch", "Holds position where the vehicle is."),
    ("RC_MAP_OFFB_SW", "Offboard switch",
     "Hands control to a companion computer."),
    ("RC_MAP_TRANS_SW", "VTOL transition switch",
     "Transitions between hover and forward flight."),
    ("RC_MAP_GEAR_SW", "Landing gear switch", ""),
    ("RC_MAP_FLAPS", "Flaps channel", ""),
    ("RC_MAP_MAN_SW", "Manual mode switch", "Legacy per-mode switch."),
    ("RC_MAP_ACRO_SW", "Acro mode switch", "Legacy per-mode switch."),
    ("RC_MAP_POSCTL_SW", "Position mode switch", "Legacy per-mode switch."),
    ("RC_MAP_STAB_SW", "Stabilized mode switch", "Legacy per-mode switch."),
    ("RC_MAP_MODE_SW", "Legacy mode switch", "Superseded by the flight mode channel."),
]

AUX_FIELDS: list[tuple[str, str, str]] = [
    ("RC_MAP_AUX1", "AUX 1", "Passed straight through to an actuator or a payload."),
    ("RC_MAP_AUX2", "AUX 2", ""),
    ("RC_MAP_AUX3", "AUX 3", ""),
    ("RC_MAP_AUX4", "AUX 4", ""),
    ("RC_MAP_AUX5", "AUX 5", ""),
    ("RC_MAP_AUX6", "AUX 6", ""),
    ("RC_MAP_PARAM1", "Tuning knob 1", "Adjusts the parameter named by RC_PARAM_MAP."),
    ("RC_MAP_PARAM2", "Tuning knob 2", ""),
    ("RC_MAP_PARAM3", "Tuning knob 3", ""),
]

# Every RC_MAP_* this module knows about, in one list, so the "which channel is
# already taken" check the frontend does has a single source.
MAP_FIELDS: list[tuple[str, str, str]] = (
    STICK_FIELDS + MODE_SWITCH_FIELDS + SWITCH_FIELDS + AUX_FIELDS
)

# Per-channel calibration parameters, as (suffix, label, unit, hint).
CHANNEL_PARAM_SUFFIXES = ("MIN", "MAX", "TRIM", "DZ", "REV")

# Input mode, loss handling and the stick-override behaviour that decides
# whether a transmitter can interrupt an autonomous flight at all.
INPUT_NUMBER_FIELDS: list[tuple[str, str, str, str]] = [
    ("COM_RC_LOSS_T", "RC loss timeout", "s",
     "How long the transmitter may go unheard before the loss action runs."),
    ("COM_RC_STICK_OV", "Stick override threshold", "%",
     "How far a stick must move to take an autonomous flight back into "
     "position mode. 0 disables the override entirely."),
    ("RC_CHAN_CNT", "Channel count", "",
     "How many channels the receiver delivers. Written by the calibration."),
    ("RC_RSSI_PWM_CHAN", "RSSI channel", "",
     "Channel carrying link strength as a pulse width. 0 if the receiver "
     "reports RSSI over the protocol instead."),
]


# ---------------------------------------------------------------------------
# Parameter list
# ---------------------------------------------------------------------------

def channel_params(channel: int) -> list[str]:
    """The five calibration parameters of one channel, in write order."""
    return [f"RC{int(channel)}_{suffix}" for suffix in CHANNEL_PARAM_SUFFIXES]


def param_names(channels: int = MAX_CHANNELS) -> list[str]:
    """Every parameter the Radio Control page may need, in one flat list.

    Handed to the bridge as a single batched read: the page asks for the whole
    superset once and renders what came back, rather than probing name by name.
    """
    names: list[str] = [
        "COM_RC_IN_MODE", "NAV_RCL_ACT", "RC_MAP_FAILSAFE",
    ]
    names.extend(param for param, _l, _u, _h in INPUT_NUMBER_FIELDS)
    names.extend(param for param, _l, _h in MAP_FIELDS)
    names.extend(param for param, _l, _h in MODE_SLOT_FIELDS)
    limit = max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS))
    for channel in range(1, limit + 1):
        names.extend(channel_params(channel))
    seen: set[str] = set()
    unique: list[str] = []
    for name in names:
        if name not in seen:
            seen.add(name)
            unique.append(name)
    return unique


# ---------------------------------------------------------------------------
# Field builders (mirrors safety_config's, deliberately)
# ---------------------------------------------------------------------------

def _enum_options(options: list[dict[str, Any]], value: float) -> list[dict[str, Any]]:
    """Options list guaranteed to contain *value*.

    An enum member this build has never heard of is appended as its own option
    rather than snapped to the first one, so touching an unrelated field never
    silently rewrites it.
    """
    ivalue = int(round(value))
    if any(int(o["value"]) == ivalue for o in options):
        return options
    return options + [{"value": ivalue, "label": f"Unknown ({ivalue})"}]


def _enum(name: str, label: str, values: dict[str, float],
          options: list[dict[str, Any]], **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    value = values[name]
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "enum",
        "value": value, "options": _enum_options(options, value),
    }
    field.update(extra)
    return field


def _number(name: str, label: str, values: dict[str, float],
            unit: str = "", hint: str = "", **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "number", "value": values[name],
    }
    if unit:
        field["unit"] = unit
    if hint:
        field["hint"] = hint
    field.update(extra)
    return field


def _present(fields: list[dict[str, Any] | None]) -> list[dict[str, Any]]:
    return [f for f in fields if f is not None]


def _map_fields(table: list[tuple[str, str, str]], values: dict[str, float],
                options: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Channel-picker fields for one RC_MAP_* table.

    Every one of these carries ``role: "channel"`` so the frontend knows it may
    offer "Detect" on it — the button that watches the live channels and picks
    the one the operator just moved.
    """
    built = [
        _enum(param, label, values, options, hint=hint or "", role="channel")
        for param, label, hint in table
    ]
    return _present(built)


def _section(section_id: str, title: str, hint: str,
             fields: list[dict[str, Any]], **extra: Any) -> dict[str, Any] | None:
    if not fields:
        return None
    section: dict[str, Any] = {
        "id": section_id, "title": title, "hint": hint,
        "kind": "fields", "fields": fields,
    }
    section.update(extra)
    return section


# ---------------------------------------------------------------------------
# Sections
# ---------------------------------------------------------------------------

def _input_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = _present([
        _enum("COM_RC_IN_MODE", "Accepted input", values, RC_IN_MODE_OPTIONS,
              hint="Which manual input the vehicle listens to at all."),
        _enum("NAV_RCL_ACT", "Action on RC loss", values, RC_LOSS_ACTION_OPTIONS,
              hint="What the vehicle does when the transmitter goes unheard for "
                   "longer than the timeout below."),
    ])
    fields.extend(_present([
        _number(param, label, values, unit, hint)
        for param, label, unit, hint in INPUT_NUMBER_FIELDS
    ]))
    return _section(
        "input", "Input and failsafe",
        "What the vehicle accepts from a transmitter, and what it does when the "
        "transmitter stops arriving.",
        fields,
    )


def _sticks_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    options = channel_options(channels)
    return _section(
        "sticks", "Stick channels",
        "Which channel carries each axis. The calibration sets these for you; "
        "change one here only to correct a transmitter you re-ordered.",
        _map_fields(STICK_FIELDS, values, options),
    )


def _modes_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    options = channel_options(channels)
    fields = _map_fields(MODE_SWITCH_FIELDS, values, options)
    fields.extend(_present([
        _enum(param, label, values, FLIGHT_MODE_OPTIONS, hint=hint or "", role="mode")
        for param, label, hint in MODE_SLOT_FIELDS
    ]))
    return _section(
        "modes", "Flight mode switch",
        "One channel, six positions, one flight mode each. A position left "
        "unassigned keeps whatever mode the vehicle was already in.",
        fields,
    )


def _switches_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    options = channel_options(channels)
    return _section(
        "switches", "Switches",
        "Every other action PX4 can bind to a switch. Unassigned means the "
        "action is unavailable from the transmitter.",
        _map_fields(SWITCH_FIELDS, values, options),
    )


def _aux_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    options = channel_options(channels)
    return _section(
        "aux", "Auxiliary channels and knobs",
        "Channels passed through to an actuator, a payload, or a tuning knob.",
        _map_fields(AUX_FIELDS, values, options),
    )


def _channel_row(channel: int, values: dict[str, float]) -> dict[str, Any] | None:
    """One channel's calibration numbers, or None when the firmware has none.

    ``calibrated`` is the honest half of this row: a channel whose endpoints
    are still PX4's 1000/2000 defaults, or whose travel is implausibly narrow,
    has never actually been calibrated, and the page says so rather than
    drawing a bar that looks configured.
    """
    fields: dict[str, Any] = {}
    for suffix in CHANNEL_PARAM_SUFFIXES:
        name = f"RC{channel}_{suffix}"
        if name in values:
            fields[suffix.lower()] = values[name]
    if not fields:
        return None
    minimum = fields.get("min")
    maximum = fields.get("max")
    travel = None
    if minimum is not None and maximum is not None:
        travel = abs(float(maximum) - float(minimum))
    return {
        "channel": channel,
        "min": minimum,
        "max": maximum,
        "trim": fields.get("trim"),
        "dz": fields.get("dz"),
        "reversed": bool(float(fields.get("rev", REV_NORMAL)) < 0),
        "has_rev": "rev" in fields,
        "travel": travel,
        "calibrated": bool(travel is not None and travel >= MIN_TRAVEL_US),
        "params": {suffix.lower(): f"RC{channel}_{suffix}"
                   for suffix in CHANNEL_PARAM_SUFFIXES
                   if f"RC{channel}_{suffix}" in values},
    }


def _channels_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    limit = max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS))
    rows = _present([_channel_row(n, values) for n in range(1, limit + 1)])
    if not rows:
        return None
    return {
        "id": "channels", "title": "Channel calibration", "kind": "channels",
        "hint": "The endpoints and centre the calibration measured. Every value "
                "here is editable, but the wizard is the reliable way to set them.",
        "rows": rows,
    }


def _assignments(values: dict[str, float]) -> dict[str, int]:
    """``{param: channel}`` for every RC_MAP_* the vehicle answered for.

    The frontend uses it to flag a channel bound to two things at once, which
    PX4 permits and which is a genuine way to lose an aircraft: a kill switch
    sharing a channel with the mode switch fires on a mode change.
    """
    out: dict[str, int] = {}
    for param, _label, _hint in MAP_FIELDS:
        if param in values:
            out[param] = int(round(float(values[param])))
    return out


# ---------------------------------------------------------------------------
# Calibration
# ---------------------------------------------------------------------------

class CalibrationError(ValueError):
    """A measured calibration that must not be written to the vehicle."""


def _validate_channel(entry: Any) -> dict[str, Any]:
    """Validate one measured channel, returning it normalised."""
    if not isinstance(entry, dict):
        raise CalibrationError("each channel must be an object")
    try:
        channel = int(entry["channel"])
        minimum = int(round(float(entry["min"])))
        maximum = int(round(float(entry["max"])))
        trim = int(round(float(entry["trim"])))
    except (KeyError, TypeError, ValueError) as exc:
        raise CalibrationError(
            "each channel needs numeric channel, min, max and trim"
        ) from exc
    if not 1 <= channel <= MAX_CHANNELS:
        raise CalibrationError(f"channel {channel} is outside 1-{MAX_CHANNELS}")
    for label, value in (("min", minimum), ("max", maximum), ("trim", trim)):
        if not PWM_ABS_MIN <= value <= PWM_ABS_MAX:
            raise CalibrationError(
                f"channel {channel} {label} of {value} us is outside "
                f"{PWM_ABS_MIN}-{PWM_ABS_MAX} us"
            )
    if maximum - minimum < MIN_TRAVEL_US:
        raise CalibrationError(
            f"channel {channel} moved only {maximum - minimum} us — move every "
            f"stick and switch through its full travel and measure again"
        )
    if not minimum <= trim <= maximum:
        raise CalibrationError(
            f"channel {channel} centre of {trim} us is outside its own "
            f"{minimum}-{maximum} us travel"
        )
    return {
        "channel": channel, "min": minimum, "max": maximum, "trim": trim,
        "reversed": bool(entry.get("reversed", False)),
        "set_reverse": bool(entry.get("set_reverse", "reversed" in entry)),
    }


# The RC_MAP_* parameters the calibration wizard is allowed to write. The
# wizard learns these from the stick the operator was asked to move, which is
# the only mapping it has any evidence for; a kill switch is bound by hand.
CALIBRATABLE_MAPS: frozenset[str] = frozenset(
    param for param, _l, _h in STICK_FIELDS
)


def _validate_mapping(mapping: Any, known: set[str] | None) -> list[dict[str, Any]]:
    """Validate the ``{RC_MAP_*: channel}`` half of a measured calibration."""
    if mapping is None:
        return []
    if not isinstance(mapping, dict):
        raise CalibrationError("mapping must be an object")
    writes: list[dict[str, Any]] = []
    for name in sorted(mapping):
        if name not in CALIBRATABLE_MAPS:
            raise CalibrationError(f"{name} is not set by the radio calibration")
        value = mapping[name]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise CalibrationError(f"{name} must be a channel number")
        channel = int(value)
        if channel != value or not 1 <= channel <= MAX_CHANNELS:
            raise CalibrationError(
                f"{name} channel {value} is outside 1-{MAX_CHANNELS}")
        if known is None or name in known:
            writes.append({"name": name, "value": float(channel)})
    return writes


def calibration_writes(channels: list[Any], count: int | None = None,
                       known: set[str] | None = None,
                       mapping: dict[str, Any] | None = None) -> list[dict[str, Any]]:
    """Turn a measured calibration into the ordered parameter write list.

    *channels* is what the wizard measured: one ``{channel, min, max, trim,
    reversed}`` per channel the receiver actually delivered. *count* is written
    to ``RC_CHAN_CNT`` when given. *mapping* is the stick assignment the wizard
    learned from watching which channel moved when it asked for a named stick,
    limited to :data:`CALIBRATABLE_MAPS`. *known*, when given, is the set of
    parameter names the connected firmware answered for; a write to a parameter
    outside it is dropped rather than sent, which is the same version-tolerance
    rule the rest of this module follows (AGENTS.md).

    Raises :class:`CalibrationError` — never returns a partial list — when a
    measurement cannot be trusted. A rejected calibration leaves the vehicle
    exactly as it was, which is the only safe outcome: a half-written endpoint
    set is worse than an uncalibrated radio, because it looks done.
    """
    if not isinstance(channels, list) or not channels:
        raise CalibrationError("no channels were measured")
    if len(channels) > MAX_CHANNELS:
        raise CalibrationError(f"at most {MAX_CHANNELS} channels can be calibrated")

    validated = [_validate_channel(entry) for entry in channels]
    map_writes = _validate_mapping(mapping, known)
    seen: set[int] = set()
    for entry in validated:
        if entry["channel"] in seen:
            raise CalibrationError(f"channel {entry['channel']} was measured twice")
        seen.add(entry["channel"])

    def allowed(name: str) -> bool:
        return known is None or name in known

    writes: list[dict[str, Any]] = []
    for entry in sorted(validated, key=lambda e: e["channel"]):
        channel = entry["channel"]
        pairs: list[tuple[str, float]] = [
            (f"RC{channel}_MIN", float(entry["min"])),
            (f"RC{channel}_MAX", float(entry["max"])),
            (f"RC{channel}_TRIM", float(entry["trim"])),
        ]
        if entry["set_reverse"]:
            pairs.append((
                f"RC{channel}_REV",
                REV_REVERSED if entry["reversed"] else REV_NORMAL,
            ))
        writes.extend(
            {"name": name, "value": value} for name, value in pairs if allowed(name)
        )

    if count is not None:
        try:
            resolved = int(count)
        except (TypeError, ValueError) as exc:
            raise CalibrationError("channel count must be a number") from exc
        if not 1 <= resolved <= MAX_CHANNELS:
            raise CalibrationError(
                f"channel count {resolved} is outside 1-{MAX_CHANNELS}"
            )
        if allowed("RC_CHAN_CNT"):
            writes.append({"name": "RC_CHAN_CNT", "value": float(resolved)})

    # Endpoints first, mapping last: a channel pointed at a stick before its
    # travel is known is briefly a stick with PX4's defaults on the wrong
    # channel, and a link that dies between the two writes leaves exactly that.
    writes.extend(map_writes)

    if not writes:
        raise CalibrationError(
            "the connected firmware has none of the calibration parameters"
        )
    return writes


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def build(values: dict[str, float], channels: int = MAX_CHANNELS) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Radio Control page description.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them.
    *channels* caps the channel pickers and the calibration table at what the
    receiver actually delivers, so an 8-channel radio does not present eighteen
    empty rows.

    The return shape is what ``GET /api/rc`` serialises:

    ``sections`` — ordered ``{id, title, hint, kind, fields|rows}``. ``kind`` is
    ``"fields"`` for a plain schema-driven form (rendered by the shared
    ``paramFieldGrid``) and ``"channels"`` for the calibration table. Each field
    carries the PX4 parameter name it writes, so the frontend applies edits
    through the existing ``POST /api/params/set``.

    ``assignments`` — ``{RC_MAP_*: channel}``, so the page can flag a channel
    bound to two actions at once.
    """
    sections = [s for s in (
        _input_section(values),
        _sticks_section(values, channels),
        _modes_section(values, channels),
        _switches_section(values, channels),
        _aux_section(values, channels),
        _channels_section(values, channels),
    ) if s is not None]
    return {
        "sections": sections,
        "assignments": _assignments(values),
        "channel_limit": max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS)),
        "received": len(values),
    }
