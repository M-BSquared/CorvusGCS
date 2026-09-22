"""ArduPilot PID tuning schema for the Setup -> PID Tuning page.

The ArduPilot twin of :mod:`corvus.tuning_config`, with the same shape and the
same contract: this owns the knowledge of which parameters make up each loop of
the cascade, the bridge fetches raw values, the HTTP layer serialises, and the
frontend renders whatever it is handed.

The page is split the way the controller is, innermost loop first, because a
vehicle that oscillates in a hover and a vehicle that overshoots a waypoint are
two different loops and a flat list of forty gains gives the operator no way to
tell which one they are touching:

``rate``      the rate controller — ``ATC_RAT_*`` on Copter, ``*_RATE_*`` on Plane
``attitude``  the attitude controller that feeds it rate setpoints
``velocity``  the velocity and acceleration loops (``PSC_*``, Copter)
``position``  the position loop (``PSC_POS*``, Copter)
``autotune``  the tune itself: its preconditions, its settings, its run

Autotune, and why this module says less about it than the PX4 one
-----------------------------------------------------------------
PX4's autotune is a module with an enable parameter and a command, and it
reports progress as a stream of ACKs. ArduPilot's is a **flight mode**: there is
nothing to switch on, nothing to reboot for, and no progress field — the
autopilot narrates the tune over STATUSTEXT, which the console already shows.
So the group here carries the settings that shape the tune (which axes, how
aggressive) and states the procedure, and the start button is a mode change
that :meth:`corvus.mavlink_bridge.MavlinkBridge.autotune` makes on its own.

Every group is *candidate* only, exactly as in the PX4 module: a field whose
parameter the firmware does not answer for is dropped, a section with no
surviving field is dropped, and a group with no surviving section is dropped.
That is what lets one schema serve Copter, Plane and Rover without a switch.
"""
from __future__ import annotations

from typing import Any

from .param_fields import bitmask, enum, number, present

# A field table row is (parameter, label, unit, hint) — same tuple the PX4
# module uses, so the two read the same way side by side.
FieldRow = tuple[str, str, str, str]
SectionTable = list[tuple[str, str, list[FieldRow]]]


def _rate_axis(prefix: str, axis: str) -> list[FieldRow]:
    """The gains of one ArduPilot rate axis (``ATC_RAT_RLL`` and friends)."""
    return [
        (f"{prefix}_P", f"{axis} rate P", "",
         "The main gain: how hard the controller reacts to a rate error. Raise it "
         "until the aircraft feels crisp, back off at the first sign of a twitch."),
        (f"{prefix}_I", f"{axis} rate I", "",
         "Removes steady error — a persistent lean from an off-centre payload or "
         "a bent arm. On ArduPilot this normally tracks P."),
        (f"{prefix}_D", f"{axis} rate D", "",
         "Damps the response. Too much amplifies motor and frame noise into heat."),
        (f"{prefix}_IMAX", f"{axis} integrator limit", "",
         "Ceiling on the integrator's share of the output, as a fraction of full "
         "authority."),
        (f"{prefix}_FF", f"{axis} feed-forward", "",
         "Passes the rate setpoint straight through, ahead of the error loop."),
        (f"{prefix}_FLTT", f"{axis} target filter", "Hz",
         "Low-pass on the setpoint entering the loop."),
        (f"{prefix}_FLTE", f"{axis} error filter", "Hz",
         "Low-pass on the rate error. Lowering it calms a noisy airframe at the "
         "cost of response."),
        (f"{prefix}_FLTD", f"{axis} derivative filter", "Hz",
         "Low-pass on the D term specifically — the usual first thing to lower "
         "when a tune makes the motors hot."),
        (f"{prefix}_SMAX", f"{axis} slew limit", "",
         "Caps how fast the output may move, which is ArduPilot's own guard "
         "against a gain that is too high for the airframe. 0 disables it."),
    ]


# ---------------------------------------------------------------------------
# Copter
# ---------------------------------------------------------------------------

COPTER_RATE_SECTIONS: SectionTable = [
    ("roll", "Roll", _rate_axis("ATC_RAT_RLL", "Roll")),
    ("pitch", "Pitch", _rate_axis("ATC_RAT_PIT", "Pitch")),
    ("yaw", "Yaw", _rate_axis("ATC_RAT_YAW", "Yaw")),
]

COPTER_ATTITUDE_SECTIONS: SectionTable = [
    ("gains", "Attitude gains", [
        ("ATC_ANG_RLL_P", "Roll angle P", "",
         "Turns a roll angle error into a roll rate setpoint. Higher is sharper; "
         "too high and the rate loop is asked for more than the airframe can give."),
        ("ATC_ANG_PIT_P", "Pitch angle P", "", "Pitch angle error to pitch rate setpoint."),
        ("ATC_ANG_YAW_P", "Yaw angle P", "", "Heading error to yaw rate setpoint."),
        ("ATC_INPUT_TC", "Input time constant", "s",
         "How sharply the aircraft answers the sticks. Smaller is more direct."),
    ]),
    ("limits", "Acceleration limits", [
        ("ATC_ACCEL_R_MAX", "Maximum roll acceleration", "cdeg/s/s",
         "Centidegrees per second squared — ArduPilot's own unit."),
        ("ATC_ACCEL_P_MAX", "Maximum pitch acceleration", "cdeg/s/s", ""),
        ("ATC_ACCEL_Y_MAX", "Maximum yaw acceleration", "cdeg/s/s", ""),
        ("ATC_SLEW_YAW", "Yaw slew rate", "cdeg/s",
         "Fastest heading change an automatic mode may command."),
    ]),
    ("priority", "Throttle and attitude priority", [
        ("ATC_THR_MIX_MIN", "Throttle/attitude mix, minimum", "",
         "How much authority attitude control keeps when the throttle is low. "
         "Raising it helps a heavy aircraft stay upright in a fast descent."),
        ("ATC_THR_MIX_MAN", "Throttle/attitude mix, manual", "", ""),
        ("ATC_THR_MIX_MAX", "Throttle/attitude mix, maximum", "", ""),
    ]),
]

COPTER_VELOCITY_SECTIONS: SectionTable = [
    ("horizontal", "Horizontal velocity", [
        ("PSC_VELXY_P", "Velocity XY P", "", "Velocity error to acceleration demand."),
        ("PSC_VELXY_I", "Velocity XY I", "", ""),
        ("PSC_VELXY_D", "Velocity XY D", "", ""),
        ("PSC_VELXY_IMAX", "Velocity XY integrator limit", "cm/s/s", ""),
        ("PSC_VELXY_FLTE", "Velocity XY error filter", "Hz", ""),
        ("PSC_VELXY_FLTD", "Velocity XY derivative filter", "Hz", ""),
    ]),
    ("vertical", "Vertical velocity", [
        ("PSC_VELZ_P", "Velocity Z P", "", "Climb-rate error to acceleration demand."),
        ("PSC_VELZ_I", "Velocity Z I", "", ""),
        ("PSC_VELZ_D", "Velocity Z D", "", ""),
        ("PSC_VELZ_IMAX", "Velocity Z integrator limit", "cm/s/s", ""),
    ]),
    ("throttle", "Vertical acceleration", [
        ("PSC_ACCZ_P", "Acceleration Z P", "",
         "The throttle loop. On most airframes this is the one gain that has to "
         "change with a large weight difference."),
        ("PSC_ACCZ_I", "Acceleration Z I", "", ""),
        ("PSC_ACCZ_D", "Acceleration Z D", "", ""),
        ("PSC_ACCZ_IMAX", "Acceleration Z integrator limit", "", ""),
        ("PSC_ACCZ_FLTT", "Acceleration Z target filter", "Hz", ""),
        ("PSC_ACCZ_FLTE", "Acceleration Z error filter", "Hz", ""),
        ("PSC_ACCZ_FLTD", "Acceleration Z derivative filter", "Hz", ""),
    ]),
]

COPTER_POSITION_SECTIONS: SectionTable = [
    ("gains", "Position gains", [
        ("PSC_POSXY_P", "Position XY P", "",
         "Position error to velocity demand. Raising it makes the aircraft hold "
         "a point harder and overshoot a waypoint more."),
        ("PSC_POSZ_P", "Position Z P", "", "Altitude error to climb-rate demand."),
    ]),
    ("shaping", "Trajectory shaping", [
        ("PSC_JERK_XY", "Horizontal jerk limit", "m/s/s/s",
         "How abruptly the demanded acceleration may change. Lower is smoother "
         "and slower to react."),
        ("PSC_JERK_Z", "Vertical jerk limit", "m/s/s/s", ""),
        ("WPNAV_SPEED", "Waypoint speed", "cm/s", ""),
        ("WPNAV_ACCEL", "Waypoint acceleration", "cm/s/s", ""),
        ("WPNAV_RADIUS", "Waypoint radius", "cm",
         "How close counts as arrived."),
    ]),
]

# ---------------------------------------------------------------------------
# Plane
# ---------------------------------------------------------------------------

PLANE_RATE_SECTIONS: SectionTable = [
    ("roll", "Roll", [
        ("RLL_RATE_P", "Roll rate P", "", "Roll rate error to aileron."),
        ("RLL_RATE_I", "Roll rate I", "", ""),
        ("RLL_RATE_D", "Roll rate D", "", ""),
        ("RLL_RATE_FF", "Roll rate feed-forward", "",
         "The dominant term on a fixed wing: control surfaces are fast enough "
         "that most of the demand can go straight through."),
        ("RLL_RATE_IMAX", "Roll rate integrator limit", "", ""),
    ]),
    ("pitch", "Pitch", [
        ("PTCH_RATE_P", "Pitch rate P", "", "Pitch rate error to elevator."),
        ("PTCH_RATE_I", "Pitch rate I", "", ""),
        ("PTCH_RATE_D", "Pitch rate D", "", ""),
        ("PTCH_RATE_FF", "Pitch rate feed-forward", "", ""),
        ("PTCH_RATE_IMAX", "Pitch rate integrator limit", "", ""),
    ]),
    ("yaw", "Yaw", [
        ("YAW_RATE_P", "Yaw rate P", "", ""),
        ("YAW_RATE_I", "Yaw rate I", "", ""),
        ("YAW_RATE_D", "Yaw rate D", "", ""),
        ("YAW_RATE_FF", "Yaw rate feed-forward", "", ""),
        ("KFF_RDDRMIX", "Rudder mix", "",
         "How much rudder follows aileron, to keep a turn coordinated."),
    ]),
]

PLANE_ATTITUDE_SECTIONS: SectionTable = [
    ("limits", "Attitude limits", [
        ("LIM_ROLL_CD", "Maximum bank angle", "cdeg",
         "Centidegrees — ArduPilot's own unit. 4500 is 45°."),
        ("LIM_PITCH_MAX", "Maximum pitch up", "cdeg", ""),
        ("LIM_PITCH_MIN", "Maximum pitch down", "cdeg", ""),
        ("ACRO_ROLL_RATE", "Acro roll rate", "deg/s", ""),
        ("ACRO_PITCH_RATE", "Acro pitch rate", "deg/s", ""),
    ]),
    ("navigation", "Navigation", [
        ("NAVL1_PERIOD", "L1 period", "s",
         "How tightly the aircraft tracks a line between waypoints. Lower turns "
         "harder and can weave; higher is smooth and cuts corners."),
        ("NAVL1_DAMPING", "L1 damping", "", ""),
        ("WP_RADIUS", "Waypoint radius", "m", ""),
        ("WP_LOITER_RAD", "Loiter radius", "m", ""),
    ]),
    ("speed_height", "Speed and height (TECS)", [
        ("TECS_TIME_CONST", "TECS time constant", "s",
         "How quickly the total-energy controller trades speed for height."),
        ("TECS_PTCH_DAMP", "Pitch damping", "", ""),
        ("TECS_THR_DAMP", "Throttle damping", "", ""),
        ("TECS_CLMB_MAX", "Maximum climb rate", "m/s", ""),
        ("TECS_SINK_MAX", "Maximum sink rate", "m/s", ""),
        ("TRIM_ARSPD_CM", "Cruise airspeed", "cm/s", ""),
    ]),
]

# ---------------------------------------------------------------------------
# Rover
# ---------------------------------------------------------------------------

ROVER_RATE_SECTIONS: SectionTable = [
    ("steering", "Steering rate", [
        ("ATC_STR_RAT_P", "Steering rate P", "", ""),
        ("ATC_STR_RAT_I", "Steering rate I", "", ""),
        ("ATC_STR_RAT_D", "Steering rate D", "", ""),
        ("ATC_STR_RAT_FF", "Steering rate feed-forward", "", ""),
        ("ATC_STR_RAT_IMAX", "Steering rate integrator limit", "", ""),
        ("ATC_STR_RAT_MAX", "Maximum turn rate", "deg/s", ""),
    ]),
    ("speed", "Speed", [
        ("ATC_SPEED_P", "Speed P", "", ""),
        ("ATC_SPEED_I", "Speed I", "", ""),
        ("ATC_SPEED_D", "Speed D", "", ""),
        ("ATC_SPEED_IMAX", "Speed integrator limit", "", ""),
        ("ATC_ACCEL_MAX", "Maximum acceleration", "m/s/s", ""),
        ("ATC_DECEL_MAX", "Maximum deceleration", "m/s/s", ""),
    ]),
]

ROVER_NAV_SECTIONS: SectionTable = [
    ("cruise", "Cruise", [
        ("CRUISE_SPEED", "Cruise speed", "m/s", ""),
        ("CRUISE_THROTTLE", "Cruise throttle", "%", ""),
        ("WP_SPEED", "Waypoint speed", "m/s", ""),
        ("WP_RADIUS", "Waypoint radius", "m", ""),
        ("WP_OVERSHOOT", "Allowed overshoot", "m", ""),
    ]),
]

# ---------------------------------------------------------------------------
# Live charts — the telemetry field names the frontend plots against
# ---------------------------------------------------------------------------

RATE_CHARTS: list[dict[str, Any]] = [
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

# ---------------------------------------------------------------------------
# Autotune
# ---------------------------------------------------------------------------

AUTOTUNE_AXES_BITS: list[dict[str, Any]] = [
    {"bit": 0, "label": "Roll"},
    {"bit": 1, "label": "Pitch"},
    {"bit": 2, "label": "Yaw"},
    {"bit": 3, "label": "Yaw D"},
]

COPTER_AUTOTUNE_FIELDS: list[FieldRow] = [
    ("AUTOTUNE_AGGR", "Aggressiveness", "",
     "How much bounce-back the tune accepts before it stops raising a gain. "
     "0.05 is gentle, 0.10 is sharp; 0.075 is the default and suits most "
     "airframes."),
    ("AUTOTUNE_MIN_D", "Minimum D", "",
     "Floor on the D term, so the tune cannot leave an axis undamped."),
]

PLANE_AUTOTUNE_FIELDS: list[FieldRow] = [
    ("AUTOTUNE_LEVEL", "Tuning level", "",
     "1 is very soft, 10 is very sharp. 6 is the default and is a reasonable "
     "first flight for most airframes."),
]

COPTER_AUTOTUNE_STEPS: list[str] = [
    "Fly somewhere open and calm — the tune needs room to drift and is thrown "
    "off by wind.",
    "Take off and hold a stable hover in Loiter or AltHold, 5-10 m up.",
    "Start the tune. Corvus switches the vehicle into AUTOTUNE mode; ArduPilot "
    "takes the sticks from there.",
    "Let go of the sticks. Expect visible twitching, one axis at a time, for "
    "several minutes — nudging a stick pauses the tune, it does not break it.",
    "When ArduPilot reports the tune is complete, land with the throttle down "
    "and leave the mode switch in AUTOTUNE to keep the new gains, then disarm.",
    "Switching out of AUTOTUNE before landing discards the result; test the "
    "new gains gently before flying the aircraft normally.",
]

PLANE_AUTOTUNE_STEPS: list[str] = [
    "Fly somewhere open, in calm air, with plenty of altitude.",
    "Trim the aircraft in FBWA so it flies straight and level hands-off.",
    "Start the tune. Corvus switches the vehicle into AUTOTUNE mode.",
    "Fly it: the plane tunes from your own stick inputs, so give it full "
    "aileron and elevator rolls and pitches for a few minutes.",
    "Switch out of AUTOTUNE when the response feels right — the gains are "
    "saved as they are learned.",
]


def param_names() -> list[str]:
    """Every parameter this page may need, across Copter, Plane and Rover."""
    names: list[str] = []
    for table in (COPTER_RATE_SECTIONS, COPTER_ATTITUDE_SECTIONS,
                  COPTER_VELOCITY_SECTIONS, COPTER_POSITION_SECTIONS,
                  PLANE_RATE_SECTIONS, PLANE_ATTITUDE_SECTIONS,
                  ROVER_RATE_SECTIONS, ROVER_NAV_SECTIONS):
        for _id, _title, fields in table:
            names.extend(param for param, _l, _u, _h in fields)
    names.extend(param for param, _l, _u, _h in COPTER_AUTOTUNE_FIELDS)
    names.extend(param for param, _l, _u, _h in PLANE_AUTOTUNE_FIELDS)
    names.extend(["AUTOTUNE_AXES", "AUTOTUNE_OPTIONS"])
    return list(dict.fromkeys(names))


# ---------------------------------------------------------------------------
# Assembly
# ---------------------------------------------------------------------------

def _fields(table: list[FieldRow], values: dict[str, float]) -> list[dict[str, Any]]:
    return present([number(p, label, values, unit=unit, hint=hint)
                    for p, label, unit, hint in table])


def _sections(table: SectionTable, values: dict[str, float]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for section_id, title, fields in table:
        built = _fields(fields, values)
        if built:
            out.append({"id": section_id, "title": title, "fields": built})
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


def _rate_group(values: dict[str, float]) -> dict[str, Any] | None:
    sections = (_sections(COPTER_RATE_SECTIONS, values)
                or _sections(PLANE_RATE_SECTIONS, values)
                or _sections(ROVER_RATE_SECTIONS, values))
    return _group(
        "rate", "Rate Controller",
        "The innermost loop: it turns a rate setpoint into control output, and "
        "it is the loop responsible for how the aircraft feels. Tune it first — "
        "every outer loop is built on top of it, and this is the one the "
        "autotune tunes.",
        sections, RATE_CHARTS,
    )


def _attitude_group(values: dict[str, float]) -> dict[str, Any] | None:
    sections = (_sections(COPTER_ATTITUDE_SECTIONS, values)
                or _sections(PLANE_ATTITUDE_SECTIONS, values)
                or _sections(ROVER_NAV_SECTIONS, values))
    return _group(
        "attitude", "Attitude Controller",
        "The loop above the rate controller: it turns an attitude error into "
        "the rate setpoints the loop below chases.",
        sections, ATTITUDE_CHARTS,
    )


def _velocity_group(values: dict[str, float]) -> dict[str, Any] | None:
    return _group(
        "velocity", "Velocity Controller",
        "Turns a velocity demand into an attitude and a throttle. Multicopter "
        "only — a fixed wing does this through TECS instead.",
        _sections(COPTER_VELOCITY_SECTIONS, values), VELOCITY_CHARTS,
    )


def _position_group(values: dict[str, float]) -> dict[str, Any] | None:
    return _group(
        "position", "Position Controller",
        "The outermost loop: it turns a position error into a velocity demand, "
        "and it is what holds a point and flies a waypoint.",
        _sections(COPTER_POSITION_SECTIONS, values), VELOCITY_CHARTS,
    )


def _autotune_group(values: dict[str, float],
                    vehicle: str) -> dict[str, Any] | None:
    """The autotune group: its settings and its procedure.

    Returns ``None`` for a vehicle whose firmware has no autotune — Rover and
    Sub — so the page says "this vehicle has no autotune" rather than offering
    a button that always fails.
    """
    if vehicle == "plane":
        fields, steps, label = PLANE_AUTOTUNE_FIELDS, PLANE_AUTOTUNE_STEPS, "AUTOTUNE mode"
    elif vehicle == "copter":
        fields, steps, label = COPTER_AUTOTUNE_FIELDS, COPTER_AUTOTUNE_STEPS, "AUTOTUNE mode"
    else:
        return None

    settings = present([
        bitmask("AUTOTUNE_AXES", "Axes to tune", values, AUTOTUNE_AXES_BITS,
                hint="One axis at a time takes longer but is far easier to "
                     "abort if the aircraft misbehaves."),
        enum("AUTOTUNE_OPTIONS", "Options", values, [
            {"value": 0, "label": "Default"},
            {"value": 1, "label": "Re-tune after a failed attempt"},
        ]),
    ])
    settings.extend(_fields(fields, values))

    return {
        "id": "autotune", "title": "Autotune", "kind": "autotune",
        "hint": "ArduPilot's autotune is a flight mode, not a command: starting "
                "it switches the vehicle into AUTOTUNE and the autopilot takes "
                "over from there. It narrates its progress over the console.",
        "label": label,
        "family": "ARDUPILOT",
        # There is no module to switch on and no reboot to wait for, so the
        # "module is off" warning the PX4 page shows never applies here.
        "enabled": True,
        "enable_param": None,
        "steps": list(steps),
        "sections": ([{"id": "settings", "title": "Autotune settings",
                       "fields": settings}] if settings else []),
        "charts": _charts(RATE_CHARTS),
    }


def build(values: dict[str, float], vehicle: str = "copter") -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the PID Tuning page description.

    Same return shape as :func:`corvus.tuning_config.build` — ``groups`` of
    ``{id, title, hint, kind, sections, charts}``, innermost loop first — so the
    frontend renders an ArduPilot aircraft with the same code it renders a PX4
    one with.
    """
    groups = [g for g in (
        _rate_group(values),
        _attitude_group(values),
        _velocity_group(values),
        _position_group(values),
        _autotune_group(values, vehicle),
    ) if g is not None]
    return {"groups": groups, "received": len(values), "stack": "ardupilot"}
