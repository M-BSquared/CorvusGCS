"""PX4 PID tuning schema for the Setup -> PID Tuning page.

Same split as :mod:`corvus.safety_config` and :mod:`corvus.motor_config`: this
module owns the *knowledge* of which PX4 parameters make up each controller in
the cascade, the bridge only fetches raw values, and the HTTP layer only
serialises. The frontend renders whatever description it is handed and never
hardcodes a PX4 parameter name.

The page is split the way the controller itself is, which is also the way
QGroundControl splits it — one group per loop, innermost first:

``rate``      the rate controller, the loop the autotune actually tunes
``attitude``  the attitude controller that feeds it rate setpoints
``velocity``  the velocity controller (multicopter only)
``position``  the position controller (multicopter only)
``autotune``  the autotune itself: its preconditions, its settings, its run

Splitting it matters for more than layout. A vehicle that oscillates in a
hover and a vehicle that overshoots a waypoint are two different loops, and a
single flat list of forty gains gives the operator no way to tell which one
they are touching.

Every group is *candidate* only, exactly as in :mod:`corvus.safety_config`: a
field whose parameter the connected firmware does not answer for is dropped,
a section with no surviving field is dropped, and a group with no surviving
section is dropped. That is what makes one schema serve a multicopter and a
fixed wing, and PX4 v1.16, v1.17 and v1.18, without a version switch — a
multicopter simply never answers for ``FW_RR_P``.

Autotune, and why this module carries its preconditions
------------------------------------------------------
PX4 runs the autotune **in flight**. The module injects steps into the rate
controller and measures the response, which it can only do on a vehicle that
is armed and airborne; ``mc_autotune_attitude_control`` rejects the command
with ``TEMPORARILY_REJECTED`` when the vehicle is disarmed. A ground station
that refuses to send the command unless the vehicle is *disarmed* — which is
what Corvus did — can therefore never start one. The preconditions travel with
the schema so the page states them before the operator takes off, rather than
after PX4 has refused.
"""

from __future__ import annotations

from typing import Any

# MAV_LANDED_STATE, the autopilot's own answer to "is it flying". 0 means the
# firmware does not publish EXTENDED_SYS_STATE, which is never treated as "on
# the ground" — an unknown state must not block a command PX4 would accept.
LANDED_STATE_ON_GROUND = 1
LANDED_STATE_IN_AIR = 2

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Off"},
    {"value": 1, "label": "On"},
]

# MC_AT_APPLY / FW_AT_APPLY — when the tuned gains reach the controller.
# "After landing and disarming" is the default for a reason: applying new gains
# to a vehicle that is still in the air is the one setting here that can end a
# flight badly.
AUTOTUNE_APPLY_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Never (test only)"},
    {"value": 1, "label": "After landing and disarming"},
    {"value": 2, "label": "Immediately, in flight"},
]

# FW_AT_AXES — the one autotune that does take an axis selection (bitmask:
# 1 roll, 2 pitch, 4 yaw). The multicopter autotune has no equivalent; it
# always tunes all three, which is why this page offers no per-axis button.
FW_AXES_OPTIONS: list[dict[str, Any]] = [
    {"value": 1, "label": "Roll"},
    {"value": 2, "label": "Pitch"},
    {"value": 3, "label": "Roll and pitch"},
    {"value": 4, "label": "Yaw"},
    {"value": 7, "label": "Roll, pitch and yaw"},
]


# ---------------------------------------------------------------------------
# Field tables
#
# One tuple per field: (parameter, label, unit, hint). Kept as data rather than
# as forty _number() calls so the axis triplets stay visibly parallel — a P gain
# that is worded differently on yaw than on roll is a bug you read past.
# ---------------------------------------------------------------------------

_RATE_AXIS_HINTS = {
    "P": "Proportional gain on the rate error. Raise until the axis feels crisp, "
         "back off before it buzzes.",
    "I": "Integral gain. Removes steady-state error (wind, an off-centre payload) "
         "and is what a slow drift back to level comes from.",
    "D": "Derivative gain. Damps overshoot; too much of it amplifies motor and "
         "frame noise into heat.",
    "FF": "Feed-forward from the rate setpoint. Improves tracking without touching "
          "the loop's stability.",
    "K": "Overall gain multiplier for this axis. Scales P, I and D together, so it "
         "is the one knob to turn after a payload change.",
    "INT_LIM": "Ceiling on the integrator, in normalised torque. Stops a long-held "
               "error from winding up into a lurch when the vehicle is released.",
}


def _rate_axis(prefix: str, imax: str, axis: str) -> list[tuple[str, str, str, str]]:
    """The six rate-controller fields of one multicopter axis."""
    return [
        (f"{prefix}_P", f"{axis} rate P", "", _RATE_AXIS_HINTS["P"]),
        (f"{prefix}_I", f"{axis} rate I", "", _RATE_AXIS_HINTS["I"]),
        (f"{prefix}_D", f"{axis} rate D", "", _RATE_AXIS_HINTS["D"]),
        (f"{prefix}_FF", f"{axis} rate feed-forward", "", _RATE_AXIS_HINTS["FF"]),
        (f"{prefix}_K", f"{axis} rate gain", "", _RATE_AXIS_HINTS["K"]),
        (imax, f"{axis} integrator limit", "", _RATE_AXIS_HINTS["INT_LIM"]),
    ]


def _fw_rate_axis(prefix: str, axis: str) -> list[tuple[str, str, str, str]]:
    """The five rate-controller fields of one fixed-wing axis."""
    return [
        (f"{prefix}_P", f"{axis} rate P", "", _RATE_AXIS_HINTS["P"]),
        (f"{prefix}_I", f"{axis} rate I", "", _RATE_AXIS_HINTS["I"]),
        (f"{prefix}_D", f"{axis} rate D", "", _RATE_AXIS_HINTS["D"]),
        (f"{prefix}_FF", f"{axis} rate feed-forward", "", _RATE_AXIS_HINTS["FF"]),
        (f"{prefix}_IMAX", f"{axis} integrator limit", "", _RATE_AXIS_HINTS["INT_LIM"]),
    ]


MC_RATE_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("roll", "Roll", _rate_axis("MC_ROLLRATE", "MC_RR_INT_LIM", "Roll")),
    ("pitch", "Pitch", _rate_axis("MC_PITCHRATE", "MC_PR_INT_LIM", "Pitch")),
    ("yaw", "Yaw", _rate_axis("MC_YAWRATE", "MC_YR_INT_LIM", "Yaw")),
]

MC_ATTITUDE_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("gains", "Attitude gains", [
        ("MC_ROLL_P", "Roll P", "",
         "Turns a roll angle error into a roll rate setpoint. Higher is sharper; "
         "too high and the rate loop is asked for more than the airframe can give."),
        ("MC_PITCH_P", "Pitch P", "", "Pitch angle error to pitch rate setpoint."),
        ("MC_YAW_P", "Yaw P", "", "Heading error to yaw rate setpoint."),
        ("MC_YAW_WEIGHT", "Yaw weight", "",
         "How much yaw authority may be taken from roll and pitch when the "
         "mixer saturates. Lower keeps the vehicle upright at the cost of heading."),
    ]),
    ("limits", "Rate limits", [
        ("MC_ROLLRATE_MAX", "Maximum roll rate", "deg/s",
         "Ceiling on the rate setpoint the attitude loop may request."),
        ("MC_PITCHRATE_MAX", "Maximum pitch rate", "deg/s", ""),
        ("MC_YAWRATE_MAX", "Maximum yaw rate", "deg/s", ""),
    ]),
]

MC_VELOCITY_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("horizontal", "Horizontal velocity", [
        ("MPC_XY_VEL_P_ACC", "Horizontal velocity P", "",
         "Horizontal velocity error to acceleration setpoint."),
        ("MPC_XY_VEL_I_ACC", "Horizontal velocity I", "",
         "Trims out a steady wind. Too high and the vehicle circles its hold point."),
        ("MPC_XY_VEL_D_ACC", "Horizontal velocity D", "",
         "Damps the velocity loop. Sensitive to position noise, so raise it slowly."),
        ("MPC_XY_VEL_MAX", "Maximum horizontal speed", "m/s", ""),
    ]),
    ("vertical", "Vertical velocity", [
        ("MPC_Z_VEL_P_ACC", "Vertical velocity P", "",
         "Vertical velocity error to acceleration setpoint."),
        ("MPC_Z_VEL_I_ACC", "Vertical velocity I", "",
         "Carries the hover thrust. This is the gain that holds altitude when the "
         "battery sags."),
        ("MPC_Z_VEL_D_ACC", "Vertical velocity D", "", ""),
        ("MPC_Z_VEL_MAX_UP", "Maximum climb rate", "m/s", ""),
        ("MPC_Z_VEL_MAX_DN", "Maximum descent rate", "m/s", ""),
    ]),
    ("envelope", "Envelope", [
        ("MPC_TILTMAX_AIR", "Maximum tilt in flight", "deg",
         "Caps the lean angle the velocity loop may ask for, and with it the "
         "horizontal acceleration the vehicle can produce."),
        ("MPC_THR_HOVER", "Hover throttle", "",
         "The throttle that holds a hover. A wrong value makes every altitude "
         "change start with a lurch."),
    ]),
]

MC_POSITION_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("gains", "Position gains", [
        ("MPC_XY_P", "Horizontal position P", "",
         "Horizontal position error to velocity setpoint. Raise for a tighter "
         "hold, lower if the vehicle hunts around its hold point."),
        ("MPC_Z_P", "Vertical position P", "",
         "Altitude error to climb-rate setpoint."),
    ]),
    ("smoothness", "Acceleration and jerk", [
        ("MPC_ACC_HOR", "Horizontal acceleration", "m/s²", ""),
        ("MPC_ACC_UP_MAX", "Maximum upward acceleration", "m/s²", ""),
        ("MPC_ACC_DOWN_MAX", "Maximum downward acceleration", "m/s²", ""),
        ("MPC_JERK_AUTO", "Jerk limit (missions)", "m/s³",
         "How abruptly acceleration may change. Lower is smoother and gentler on "
         "a camera; higher tracks a mission leg more tightly."),
        ("MPC_JERK_MAX", "Jerk limit (manual)", "m/s³", ""),
    ]),
]

FW_RATE_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("roll", "Roll", _fw_rate_axis("FW_RR", "Roll")),
    ("pitch", "Pitch", _fw_rate_axis("FW_PR", "Pitch")),
    ("yaw", "Yaw", _fw_rate_axis("FW_YR", "Yaw")),
]

FW_ATTITUDE_SECTIONS: list[tuple[str, str, list[tuple[str, str, str, str]]]] = [
    ("response", "Attitude response", [
        ("FW_R_TC", "Roll time constant", "s",
         "How quickly the aircraft is asked to reach a commanded roll angle. "
         "Smaller is sharper; a large airframe needs a larger value."),
        ("FW_P_TC", "Pitch time constant", "s", ""),
    ]),
    ("limits", "Rate limits", [
        ("FW_R_RMAX", "Maximum roll rate", "deg/s", ""),
        ("FW_P_RMAX_POS", "Maximum pitch-up rate", "deg/s", ""),
        ("FW_P_RMAX_NEG", "Maximum pitch-down rate", "deg/s", ""),
        ("FW_Y_RMAX", "Maximum yaw rate", "deg/s", ""),
    ]),
]

# ---------------------------------------------------------------------------
# Charts
#
# Every chart is a setpoint trace and the response that followed it, because
# that pair is what a tuning decision is actually read off: a response that
# lags is a P that is too low, one that rings past the setpoint is a P too high
# or a D too low. A lone response trace shows neither.
# ---------------------------------------------------------------------------

MC_RATE_CHARTS: list[dict[str, Any]] = [
    {"id": "rollrate", "title": "Roll rate", "unit": "deg/s",
     "actual": "rollspeed", "setpoint": "rollspeed_sp"},
    {"id": "pitchrate", "title": "Pitch rate", "unit": "deg/s",
     "actual": "pitchspeed", "setpoint": "pitchspeed_sp"},
    {"id": "yawrate", "title": "Yaw rate", "unit": "deg/s",
     "actual": "yawspeed", "setpoint": "yawspeed_sp"},
]

ATTITUDE_CHARTS: list[dict[str, Any]] = [
    {"id": "roll", "title": "Roll angle", "unit": "deg",
     "actual": "roll", "setpoint": "roll_sp"},
    {"id": "pitch", "title": "Pitch angle", "unit": "deg",
     "actual": "pitch", "setpoint": "pitch_sp"},
]

VELOCITY_CHARTS: list[dict[str, Any]] = [
    {"id": "vx", "title": "Velocity north", "unit": "m/s",
     "actual": "vx", "setpoint": "vx_sp"},
    {"id": "vy", "title": "Velocity east", "unit": "m/s",
     "actual": "vy", "setpoint": "vy_sp"},
    {"id": "vz", "title": "Velocity down", "unit": "m/s",
     "actual": "vz", "setpoint": "vz_sp"},
]

POSITION_CHARTS: list[dict[str, Any]] = [
    {"id": "alt", "title": "Height above home", "unit": "m",
     "actual": "altitude_agl", "setpoint": None},
    {"id": "groundspeed", "title": "Ground speed", "unit": "m/s",
     "actual": "groundspeed", "setpoint": None},
]

# ---------------------------------------------------------------------------
# Autotune
# ---------------------------------------------------------------------------

# What the operator has to have done before the command can be accepted. PX4's
# own refusal ("TEMPORARILY_REJECTED") says none of this, so the page does.
AUTOTUNE_STEPS: list[str] = [
    "Fly somewhere open, with room to drift, and keep a hand on the sticks.",
    "Take off and hold a stable hover in Position or Altitude mode, 3-10 m up.",
    "Start the tune and let go of the sticks — the autopilot injects its own "
    "steps and needs a clean response to measure.",
    "Expect roughly a minute of visible twitching, one axis at a time.",
    "Land and disarm to apply the gains, then test them gently before flying "
    "the aircraft normally.",
]

MC_AUTOTUNE_FIELDS: list[tuple[str, str, str, str]] = [
    ("MC_AT_SYSID_AMP", "Excitation amplitude", "",
     "How hard the autotune shakes the vehicle. Raise it on a large or heavy "
     "airframe whose response is otherwise lost in the noise; lower it if the "
     "aircraft feels alarming during the tune."),
    ("MC_AT_RISE_TIME", "Target rise time", "s",
     "The closed-loop response the tune aims for. Smaller means sharper and "
     "hotter motors."),
]

FW_AUTOTUNE_FIELDS: list[tuple[str, str, str, str]] = [
    ("FW_AT_SYSID_AMP", "Excitation amplitude", "",
     "How hard the autotune excites each axis."),
    ("FW_AT_MAN_AUX", "Start from an aux switch", "",
     "Lets the tune be triggered from the transmitter instead of the ground "
     "station."),
]


def _autotune_family(values: dict[str, float]) -> tuple[str, str] | None:
    """Which autotune the connected firmware has, as (prefix, label).

    Decided by which enable parameter answered rather than by the vehicle type
    string: a firmware built without the autotune module has neither, and a
    VTOL has both.
    """
    if "MC_AT_EN" in values:
        return ("MC", "Multicopter autotune")
    if "FW_AT_EN" in values:
        return ("FW", "Fixed-wing autotune")
    return None


# ---------------------------------------------------------------------------
# Parameter list
# ---------------------------------------------------------------------------

def param_names() -> list[str]:
    """Every parameter the PID Tuning page may need, in one flat list.

    Handed to the bridge as a single batched read: the page asks for the whole
    superset once and renders what came back, rather than probing name by name.
    """
    names: list[str] = []
    for group in (MC_RATE_SECTIONS, MC_ATTITUDE_SECTIONS, MC_VELOCITY_SECTIONS,
                  MC_POSITION_SECTIONS, FW_RATE_SECTIONS, FW_ATTITUDE_SECTIONS):
        for _id, _title, fields in group:
            names.extend(param for param, _l, _u, _h in fields)
    names.extend(param for param, _l, _u, _h in MC_AUTOTUNE_FIELDS)
    names.extend(param for param, _l, _u, _h in FW_AUTOTUNE_FIELDS)
    names.extend([
        "MC_AT_EN", "MC_AT_APPLY",
        "FW_AT_EN", "FW_AT_APPLY", "FW_AT_AXES",
    ])
    # Deduplicated, order preserved: the batched read must not ask twice for a
    # parameter two groups happen to share.
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
            unit: str = "", hint: str = "") -> dict[str, Any] | None:
    if name not in values:
        return None
    field: dict[str, Any] = {
        "param": name, "label": label, "kind": "number",
        "value": values[name], "step": 0.001, "min": 0,
    }
    if unit:
        field["unit"] = unit
    if hint:
        field["hint"] = hint
    return field


def _present(fields: list[dict[str, Any] | None]) -> list[dict[str, Any]]:
    return [f for f in fields if f is not None]


def _fields(table: list[tuple[str, str, str, str]],
            values: dict[str, float]) -> list[dict[str, Any]]:
    built = [_number(p, label, values, unit, hint) for p, label, unit, hint in table]
    return [f for f in built if f is not None]


def _sections(table: list[tuple[str, str, list[tuple[str, str, str, str]]]],
              values: dict[str, float]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for section_id, title, table_fields in table:
        fields = _fields(table_fields, values)
        if fields:
            out.append({"id": section_id, "title": title, "fields": fields})
    return out


def _charts(charts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Copies, so a caller mutating the returned description cannot edit the
    module-level tables the next request would be built from."""
    return [dict(c) for c in charts]


def _group(group_id: str, title: str, hint: str,
           sections: list[dict[str, Any]],
           charts: list[dict[str, Any]]) -> dict[str, Any] | None:
    if not sections:
        return None
    return {
        "id": group_id, "title": title, "hint": hint, "kind": "fields",
        "sections": sections, "charts": _charts(charts),
    }


# ---------------------------------------------------------------------------
# Group builders
# ---------------------------------------------------------------------------

def _rate_group(values: dict[str, float]) -> dict[str, Any] | None:
    sections = _sections(MC_RATE_SECTIONS, values) or _sections(FW_RATE_SECTIONS, values)
    return _group(
        "rate", "Rate Controller",
        "The innermost loop: it turns a rate setpoint into motor torque, and it "
        "is the loop responsible for how the aircraft feels. Tune it first — "
        "every outer loop is built on top of it, and this is the one the "
        "autotune tunes.",
        sections, MC_RATE_CHARTS,
    )


def _attitude_group(values: dict[str, float]) -> dict[str, Any] | None:
    sections = (_sections(MC_ATTITUDE_SECTIONS, values)
                or _sections(FW_ATTITUDE_SECTIONS, values))
    return _group(
        "attitude", "Attitude Controller",
        "Turns an angle error into the rate setpoint the loop above feeds to the "
        "rate controller. Only worth touching once the rate loop tracks cleanly.",
        sections, ATTITUDE_CHARTS,
    )


def _velocity_group(values: dict[str, float]) -> dict[str, Any] | None:
    return _group(
        "velocity", "Velocity Controller",
        "Holds a commanded speed. This is the loop behind a hover that drifts in "
        "wind or an altitude that sags as the battery does. PX4 has no autotune "
        "for it — these gains are set by hand.",
        _sections(MC_VELOCITY_SECTIONS, values), VELOCITY_CHARTS,
    )


def _position_group(values: dict[str, float]) -> dict[str, Any] | None:
    return _group(
        "position", "Position Controller",
        "The outermost loop: position error to velocity setpoint, plus the "
        "acceleration and jerk limits that decide how abrupt the vehicle is.",
        _sections(MC_POSITION_SECTIONS, values), POSITION_CHARTS,
    )


def _autotune_group(values: dict[str, float]) -> dict[str, Any] | None:
    """The autotune group: its preconditions, its settings, and its run.

    Returns ``None`` when the firmware carries no autotune module at all, which
    is a real configuration (it can be compiled out) and must show as "this
    vehicle has no autotune" rather than as a button that always fails.
    """
    family = _autotune_family(values)
    if family is None:
        return None
    prefix, label = family
    enable_param = f"{prefix}_AT_EN"
    enabled = int(round(values.get(enable_param, 0.0))) == 1

    settings = _present([
        _enum(enable_param, "Autotune module", values, ON_OFF_OPTIONS,
              hint="The module has to be running before it can be commanded. "
                   "Switching this on takes effect after the autopilot reboots."),
        _enum(f"{prefix}_AT_APPLY", "Apply the tuned gains", values,
              AUTOTUNE_APPLY_OPTIONS,
              hint="When the result reaches the controller. Applying in flight is "
                   "supported but leaves you flying gains nobody has checked."),
    ])
    if prefix == "FW":
        axes = _enum("FW_AT_AXES", "Axes to tune", values, FW_AXES_OPTIONS,
                     hint="The fixed-wing autotune takes an axis selection; the "
                          "multicopter one always tunes all three.")
        if axes is not None:
            settings.append(axes)
    table = MC_AUTOTUNE_FIELDS if prefix == "MC" else FW_AUTOTUNE_FIELDS
    settings.extend(_fields(table, values))

    return {
        "id": "autotune", "title": "Autotune", "kind": "autotune",
        "hint": "PX4 tunes the rate and attitude controllers together, in flight, "
                "by injecting steps and measuring what came back.",
        "label": label,
        "family": prefix,
        "enabled": enabled,
        "enable_param": enable_param if enable_param in values else None,
        "steps": list(AUTOTUNE_STEPS),
        "sections": ([{"id": "settings", "title": "Autotune settings",
                       "fields": settings}] if settings else []),
        "charts": _charts(MC_RATE_CHARTS),
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the PID Tuning page description.

    *values* holds only the parameters the vehicle actually answered for, so
    every group shrinks or disappears on a firmware that lacks them. The return
    shape is what ``GET /api/tuning`` serialises:

    ``groups`` — ordered list of ``{id, title, hint, kind, sections, charts}``,
    innermost control loop first. ``kind`` is ``"fields"`` for a controller and
    ``"autotune"`` for the autotune group, which carries its preconditions and
    its own settings alongside its fields. Each field carries the PX4 parameter
    name it writes, so the frontend applies edits through the existing
    ``POST /api/params/set``.
    """
    groups = [g for g in (
        _rate_group(values),
        _attitude_group(values),
        _velocity_group(values),
        _position_group(values),
        _autotune_group(values),
    ) if g is not None]
    return {"groups": groups, "received": len(values)}
