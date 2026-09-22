"""ArduPilot radio-control schema for the Setup -> Radio Control page.

The ArduPilot twin of :mod:`corvus.rc_config`. Most of that module transfers
unchanged — a transmitter is a transmitter — but three things do not, and each
of them fails silently rather than loudly:

``RC<n>_REVERSED`` **not** ``RC<n>_REV``.
    PX4 stores the reversal as the *sign* of a float; ArduPilot stores it as a
    0/1 flag under a different name. Writing PX4's ``-1.0`` to an ArduPilot
    vehicle does not error — the parameter simply does not exist and the write
    is dropped, so a calibration wizard that measured a reversed channel
    reports success and leaves the channel the right way round.

``RCMAP_*`` **not** ``RC_MAP_*``.
    One underscore. The stick assignment half of a calibration went nowhere for
    the same reason.

Flight modes are numbers, not names.
    PX4's ``COM_FLTMODE1..6`` hold a fixed enum. ArduPilot's ``FLTMODE1..6``
    hold its own flat mode numbers, which differ per vehicle — mode 4 is GUIDED
    on a copter and ACRO on a plane — so the option list is built from
    :mod:`corvus.autopilot`'s tables for the connected airframe rather than
    from a constant.

Calibration is still the ground station's job on both stacks: neither autopilot
runs an RC calibration for you, so the wizard watches ``RC_CHANNELS`` while the
operator sweeps every control and writes the endpoints it observed.
:func:`calibration_writes` is the one place that decides what a measurement is
allowed to become, and it is as strict here as its PX4 twin — a channel whose
measured travel is implausibly small is rejected by name rather than written.
"""
from __future__ import annotations

from typing import Any

from . import autopilot
from .param_fields import enum, number, present
from .rc_config import (
    MAX_CHANNELS,
    MIN_TRAVEL_US,
    PWM_ABS_MAX,
    PWM_ABS_MIN,
    CalibrationError,
    validate_channel,
)

__all__ = [
    "MAX_CHANNELS", "MIN_TRAVEL_US", "PWM_ABS_MAX", "PWM_ABS_MIN",
    "CalibrationError", "build", "calibration_writes", "param_names",
]

# ArduPilot's reversal flag: a plain 0/1, not PX4's signed float.
REV_NORMAL = 0.0
REV_REVERSED = 1.0

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Enabled"},
]

CHANNEL_PARAM_SUFFIXES: tuple[str, ...] = ("MIN", "MAX", "TRIM", "DZ", "REVERSED")

# RCMAP_* — the stick assignment. Four parameters, one per axis, and the only
# mapping the calibration wizard has evidence for.
STICK_FIELDS: list[tuple[str, str, str]] = [
    ("RCMAP_ROLL", "Roll", "The channel the right stick's left/right axis sends on."),
    ("RCMAP_PITCH", "Pitch", "The channel the right stick's forward/back axis sends on."),
    ("RCMAP_YAW", "Yaw", "The channel the left stick's left/right axis sends on."),
    ("RCMAP_THROTTLE", "Throttle",
     "The channel the left stick's forward/back axis sends on."),
    ("RCMAP_FORWARD", "Forward", "Sub only."),
    ("RCMAP_LATERAL", "Lateral", "Sub only."),
]

MODE_SWITCH_FIELDS: list[tuple[str, str, str]] = [
    ("FLTMODE_CH", "Flight mode channel",
     "One channel whose six PWM bands select the six modes below. ArduPilot "
     "fixes the bands; only the modes are yours to choose."),
]

MODE_SLOT_FIELDS: list[tuple[str, str, str]] = [
    ("FLTMODE1", "Position 1 (below 1230 us)", ""),
    ("FLTMODE2", "Position 2 (1231-1360 us)", ""),
    ("FLTMODE3", "Position 3 (1361-1490 us)", ""),
    ("FLTMODE4", "Position 4 (1491-1620 us)", ""),
    ("FLTMODE5", "Position 5 (1621-1749 us)", ""),
    ("FLTMODE6", "Position 6 (above 1750 us)", ""),
]

# RC<n>_OPTION — ArduPilot binds every auxiliary function through one parameter
# per channel rather than through a parameter per function, which is why this
# page has an auxiliary *channel* list where the PX4 one has a switch list.
# The set below is the subset an operator binds by hand; an unknown number
# renders as "Unknown (n)" rather than being reset.
RC_OPTION_VALUES: list[dict[str, Any]] = [
    {"value": 0, "label": "Do nothing"},
    {"value": 2, "label": "Flip"},
    {"value": 3, "label": "Simple mode"},
    {"value": 4, "label": "RTL"},
    {"value": 7, "label": "Save waypoint"},
    {"value": 9, "label": "Camera trigger"},
    {"value": 10, "label": "Rangefinder enable"},
    {"value": 11, "label": "Fence enable"},
    {"value": 16, "label": "Auto mode"},
    {"value": 17, "label": "Landing gear"},
    {"value": 18, "label": "Land mode"},
    {"value": 19, "label": "Gripper"},
    {"value": 21, "label": "Parachute enable"},
    {"value": 22, "label": "Parachute release"},
    {"value": 24, "label": "Reset mission to first waypoint"},
    {"value": 27, "label": "Retract mount"},
    {"value": 28, "label": "Relay 1"},
    {"value": 29, "label": "Landing gear (alt)"},
    {"value": 31, "label": "Motor emergency stop"},
    {"value": 32, "label": "Motor interlock"},
    {"value": 33, "label": "Brake mode"},
    {"value": 41, "label": "Arm / disarm"},
    {"value": 46, "label": "RC override enable"},
    {"value": 52, "label": "Acro mode"},
    {"value": 55, "label": "Guided mode"},
    {"value": 56, "label": "Loiter mode"},
    {"value": 57, "label": "Follow mode"},
    {"value": 62, "label": "Compass learn"},
    {"value": 65, "label": "GPS disable"},
    {"value": 66, "label": "Relay 5"},
    {"value": 100, "label": "KillIMU1"},
    {"value": 153, "label": "Arm / emergency motor stop"},
]

# Auxiliary channels the page offers an RC<n>_OPTION picker for. Channels 1-4
# are the sticks and are deliberately left out — binding a function to the
# throttle channel is not a configuration, it is an accident.
AUX_CHANNEL_RANGE = range(5, 17)


def channel_options(count: int = MAX_CHANNELS) -> list[dict[str, Any]]:
    """The RCMAP_* option list: "Unassigned" plus one entry per channel."""
    limit = max(1, min(int(count or MAX_CHANNELS), MAX_CHANNELS))
    options: list[dict[str, Any]] = [{"value": 0, "label": "Unassigned"}]
    options.extend({"value": n, "label": f"Channel {n}"} for n in range(1, limit + 1))
    return options


def flight_mode_options(vehicle_type: int = 2) -> list[dict[str, Any]]:
    """The FLTMODE1..6 option list for this airframe.

    Built from the same per-vehicle table the mode selector and the heartbeat
    decoder use, so the number a mode switch writes and the name the status bar
    shows cannot disagree.
    """
    dialect = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    table = dialect.mode_table(vehicle_type)
    numbered = sorted(
        ((cmd.custom_mode, name) for name, cmd in table.items()),
        key=lambda pair: pair[0],
    )
    return [{"value": number, "label": name.replace("_", " ").title()}
            for number, name in numbered]


def param_names(channels: int = MAX_CHANNELS) -> list[str]:
    """Every parameter the Radio Control page may need, in one flat list."""
    limit = max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS))
    names: list[str] = [
        "RC_OPTIONS", "RC_PROTOCOLS", "RC_OVERRIDE_TIME", "RC_FS_TIMEOUT",
        "THR_FAILSAFE", "THR_FS_VALUE", "FS_THR_ENABLE", "FS_THR_VALUE",
    ]
    names.extend(param for param, _l, _h in STICK_FIELDS)
    names.extend(param for param, _l, _h in MODE_SWITCH_FIELDS)
    names.extend(param for param, _l, _h in MODE_SLOT_FIELDS)
    for n in range(1, limit + 1):
        names.extend(f"RC{n}_{suffix}" for suffix in CHANNEL_PARAM_SUFFIXES)
    for n in AUX_CHANNEL_RANGE:
        if n <= limit:
            names.append(f"RC{n}_OPTION")
    return list(dict.fromkeys(names))


# ---------------------------------------------------------------------------
# Sections
# ---------------------------------------------------------------------------

def _input_section(values: dict[str, float]) -> dict[str, Any] | None:
    fields = present([
        number("RC_PROTOCOLS", "Receiver protocols", values, step=1,
               hint="A bitmask of the protocols the autopilot will listen for. "
                    "1 accepts all of them and is the usual answer."),
        number("RC_FS_TIMEOUT", "RC loss timeout", values, unit="s", step=0.1, min=0),
        number("RC_OVERRIDE_TIME", "Override timeout", values, unit="s",
               step=0.1, min=0,
               hint="How long a ground-station stick override stays in force "
                    "before the transmitter takes over again."),
        number("RC_OPTIONS", "Receiver options", values, step=1),
        enum("THR_FAILSAFE", "Throttle failsafe", values, ON_OFF_OPTIONS),
        number("THR_FS_VALUE", "Throttle failsafe threshold", values,
               unit="PWM", step=1),
        number("FS_THR_VALUE", "RC-loss throttle threshold", values,
               unit="PWM", step=1,
               hint="A throttle channel below this counts as a lost receiver."),
    ])
    if not fields:
        return None
    return {
        "id": "input", "title": "Receiver", "kind": "fields", "fields": fields,
        "hint": "Which input the vehicle listens to, and what it does when that "
                "input stops arriving.",
    }


def _map_fields(table: list[tuple[str, str, str]], values: dict[str, float],
                channels: int) -> list[dict[str, Any]]:
    options = channel_options(channels)
    return present([enum(param, label, values, options, hint=hint)
                    for param, label, hint in table])


def _sticks_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    fields = _map_fields(STICK_FIELDS, values, channels)
    if not fields:
        return None
    return {
        "id": "sticks", "title": "Sticks", "kind": "fields", "fields": fields,
        "hint": "Which channel carries each stick axis. The calibration wizard "
                "sets these from the stick it asks you to move.",
    }


def _modes_section(values: dict[str, float], channels: int,
                   vehicle_type: int) -> dict[str, Any] | None:
    fields = _map_fields(MODE_SWITCH_FIELDS, values, channels)
    modes = flight_mode_options(vehicle_type)
    fields += present([enum(param, label, values, modes, hint=hint)
                       for param, label, hint in MODE_SLOT_FIELDS])
    if not fields:
        return None
    return {
        "id": "modes", "title": "Flight modes", "kind": "fields", "fields": fields,
        "hint": "The six modes the mode switch selects. ArduPilot fixes the PWM "
                "band of each position; only the mode in it is yours to choose.",
    }


def _aux_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    limit = max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS))
    fields = present([
        enum(f"RC{n}_OPTION", f"Channel {n}", values, RC_OPTION_VALUES)
        for n in AUX_CHANNEL_RANGE if n <= limit
    ])
    if not fields:
        return None
    return {
        "id": "switches", "title": "Auxiliary functions", "kind": "fields",
        "fields": fields,
        "hint": "ArduPilot binds a function to a channel rather than a channel "
                "to a function, so every auxiliary switch is one of these.",
    }


def _channel_row(channel: int, values: dict[str, float]) -> dict[str, Any] | None:
    """One channel's calibration numbers, or None when the firmware has none.

    ``calibrated`` is the honest half of the row: a channel whose endpoints are
    still the 1100/1900 defaults, or whose travel is implausibly narrow, has
    never actually been calibrated, and the page says so rather than drawing a
    bar that looks configured.
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
        "reversed": bool(float(fields.get("reversed", REV_NORMAL)) != 0.0),
        "has_rev": "reversed" in fields,
        "travel": travel,
        "calibrated": bool(travel is not None and travel >= MIN_TRAVEL_US),
        "params": {suffix.lower(): f"RC{channel}_{suffix}"
                   for suffix in CHANNEL_PARAM_SUFFIXES
                   if f"RC{channel}_{suffix}" in values},
    }


def _channels_section(values: dict[str, float], channels: int) -> dict[str, Any] | None:
    limit = max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS))
    rows = [r for r in (_channel_row(n, values) for n in range(1, limit + 1))
            if r is not None]
    if not rows:
        return None
    return {
        "id": "channels", "title": "Channel calibration", "kind": "channels",
        "hint": "The endpoints and centre the calibration measured. Every value "
                "here is editable, but the wizard is the reliable way to set them.",
        "rows": rows,
    }


def _assignments(values: dict[str, float]) -> dict[str, int]:
    """``{param: channel}`` for every RCMAP_* the vehicle answered for.

    The frontend uses it to flag a channel bound to two things at once, which
    ArduPilot permits and which is a genuine way to lose an aircraft.
    """
    out: dict[str, int] = {}
    for param, _label, _hint in STICK_FIELDS + MODE_SWITCH_FIELDS:
        if param in values:
            out[param] = int(round(float(values[param])))
    return out


def build(values: dict[str, float], channels: int = MAX_CHANNELS,
          vehicle_type: int = 2) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Radio Control page description.

    Same return shape as :func:`corvus.rc_config.build`. *vehicle_type* is the
    MAV_TYPE, and it is needed because ArduPilot's flight-mode numbers mean
    different things on different airframes.
    """
    sections = [s for s in (
        _input_section(values),
        _sticks_section(values, channels),
        _modes_section(values, channels, vehicle_type),
        _aux_section(values, channels),
        _channels_section(values, channels),
    ) if s is not None]
    return {
        "stack": "ardupilot",
        "sections": sections,
        "assignments": _assignments(values),
        "channel_limit": max(1, min(int(channels or MAX_CHANNELS), MAX_CHANNELS)),
        "received": len(values),
    }


# ---------------------------------------------------------------------------
# Calibration
# ---------------------------------------------------------------------------

CALIBRATABLE_MAPS: frozenset[str] = frozenset(
    param for param, _l, _h in STICK_FIELDS
)


def _validate_mapping(mapping: Any, known: set[str] | None) -> list[dict[str, Any]]:
    """Validate the ``{RCMAP_*: channel}`` half of a measured calibration."""
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

    The ArduPilot twin of :func:`corvus.rc_config.calibration_writes`, with the
    same validation — it shares the channel validator, because what makes a
    measurement implausible is a fact about receivers, not about firmware — and
    two differences on the wire: the reversal goes to ``RC<n>_REVERSED`` as a
    0/1 flag, and there is no channel-count parameter to write, because
    ArduPilot does not have one.

    Raises :class:`CalibrationError` — never returns a partial list — when a
    measurement cannot be trusted. A rejected calibration leaves the vehicle
    exactly as it was, which is the only safe outcome: a half-written endpoint
    set is worse than an uncalibrated radio, because it looks done.
    """
    if not isinstance(channels, list) or not channels:
        raise CalibrationError("no channels were measured")
    if len(channels) > MAX_CHANNELS:
        raise CalibrationError(f"at most {MAX_CHANNELS} channels can be calibrated")

    validated = [validate_channel(entry) for entry in channels]
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
                f"RC{channel}_REVERSED",
                REV_REVERSED if entry["reversed"] else REV_NORMAL,
            ))
        writes.extend(
            {"name": name, "value": value} for name, value in pairs if allowed(name)
        )

    # `count` is accepted and ignored so the two stacks share one call site.
    # ArduPilot has no RC_CHAN_CNT: it uses whatever the receiver delivers.
    del count

    # Endpoints first, mapping last: a channel pointed at a stick before its
    # travel is known is briefly a stick with the firmware's defaults on the
    # wrong channel, and a link that dies between the two writes leaves exactly
    # that.
    writes.extend(map_writes)

    if not writes:
        raise CalibrationError(
            "the connected firmware has none of the calibration parameters"
        )
    return writes
