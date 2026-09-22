"""ArduPilot motor/output schema for the Setup -> Motors page.

The ArduPilot twin of :mod:`corvus.motor_config`, and the one of the four that
could not simply be a second parameter table, because the two stacks disagree
about what a motor *is*.

PX4 describes the airframe: ``CA_ROTOR3_PX``/``PY``/``PZ`` say where motor 3
physically sits and ``CA_ROTOR3_KM`` which way it turns, so the page can draw a
450 mm quad as a 450 mm quad. ArduPilot compiles its mixer in — the layout for a
given ``FRAME_CLASS`` and ``FRAME_TYPE`` is a table in the firmware, not a set of
parameters — so there is nothing on the wire to draw from.

Rather than invent a diagram from a layout table transcribed out of somebody
else's source, this module returns motors with **no positions**, which the
Motors page already handles: it spreads them evenly and rings them with a dashed
line that says the geometry was not measured. A drawn-from-memory motor layout
that is subtly wrong is far worse than an honest ring, because the whole point
of the page is telling an operator which physical motor is Motor 3 — and the
tool that answers that question for real, the motor test, works on both stacks.

What this page *can* do, and what an operator actually changes here:

* the frame class and type, which is what selects the layout in the first place
* which output pin each motor is wired to (``SERVO<n>_FUNCTION``)
* the output protocol and PWM range (``MOT_PWM_*``, ``SERVO_DSHOT_RATE``)
* the spin thresholds — ``MOT_SPIN_ARM``, ``MOT_SPIN_MIN``, ``MOT_THST_HOVER`` —
  which is the group that actually has to be set per airframe
"""
from __future__ import annotations

from typing import Any

from .param_fields import enum, number, present

# SERVO<n>_FUNCTION values for the motors. Motors 1-8 are contiguous from 33;
# motors 9-12 were added later and live at 82-85, which is why this is a table
# rather than a base plus an offset the way PX4's is.
MOTOR_FUNCTIONS: dict[int, int] = {
    1: 33, 2: 34, 3: 35, 4: 36, 5: 37, 6: 38, 7: 39, 8: 40,
    9: 82, 10: 83, 11: 84, 12: 85,
}
MOTOR_BY_FUNCTION: dict[int, int] = {v: k for k, v in MOTOR_FUNCTIONS.items()}

MAX_MOTOR_FUNCTIONS = max(MOTOR_FUNCTIONS)
MAX_ROTORS = MAX_MOTOR_FUNCTIONS

# How many SERVO<n>_FUNCTION parameters to ask about. Boards vary from 8 to 32;
# a parameter the firmware does not carry is simply absent from the answer.
MAX_SERVO_OUTPUTS = 16

# The non-motor functions an operator is likely to meet on an output they were
# about to claim. Named rather than numbered so the refusal reads as "SERVO7
# drives the elevator" instead of "SERVO7 is 19".
SERVO_FUNCTIONS: dict[int, str] = {
    0: "Disabled",
    1: "RC passthrough",
    4: "Aileron",
    19: "Elevator",
    21: "Rudder",
    26: "Ground steering",
    27: "Parachute release",
    28: "Gripper",
    29: "Landing gear",
    51: "RC input 1",
    54: "Throttle",
    56: "Throttle left",
    57: "Throttle right",
    58: "Tilt motor front",
    59: "Tilt motor rear",
    70: "Throttle (scaled)",
    88: "Winch",
}

# FRAME_CLASS -> (label, motor count, which silhouette the page draws).
FRAME_CLASSES: dict[int, tuple[str, int, str]] = {
    0: ("Undefined", 0, "multirotor"),
    1: ("Quad", 4, "multirotor"),
    2: ("Hexa", 6, "multirotor"),
    3: ("Octa", 8, "multirotor"),
    4: ("OctaQuad", 8, "multirotor"),
    5: ("Y6", 6, "multirotor"),
    6: ("Helicopter", 1, "helicopter"),
    7: ("Tri", 3, "multirotor"),
    8: ("Single copter", 1, "helicopter"),
    9: ("Coax copter", 2, "helicopter"),
    10: ("Bicopter", 2, "multirotor"),
    11: ("Dual helicopter", 2, "helicopter"),
    12: ("DodecaHexa", 12, "multirotor"),
    13: ("Quad helicopter", 4, "helicopter"),
    14: ("Deca", 10, "multirotor"),
    15: ("Scripting matrix", 0, "multirotor"),
    16: ("6DoF scripting", 0, "multirotor"),
    17: ("Dynamic scripting matrix", 0, "multirotor"),
}

FRAME_CLASS_OPTIONS: list[dict[str, Any]] = [
    {"value": value, "label": label}
    for value, (label, _count, _family) in sorted(FRAME_CLASSES.items())
]

FRAME_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Plus"},
    {"value": 1, "label": "X"},
    {"value": 2, "label": "V"},
    {"value": 3, "label": "H"},
    {"value": 4, "label": "V-Tail"},
    {"value": 5, "label": "A-Tail"},
    {"value": 10, "label": "Y6B"},
    {"value": 11, "label": "Y6F"},
    {"value": 12, "label": "BetaFlight X"},
    {"value": 13, "label": "DJI X"},
    {"value": 14, "label": "Clockwise X"},
    {"value": 15, "label": "I"},
    {"value": 18, "label": "BetaFlight X reversed"},
    {"value": 19, "label": "Y4"},
]

MOT_PWM_TYPE_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Normal PWM"},
    {"value": 1, "label": "OneShot"},
    {"value": 2, "label": "OneShot125"},
    {"value": 3, "label": "Brushed"},
    {"value": 4, "label": "DShot150"},
    {"value": 5, "label": "DShot300"},
    {"value": 6, "label": "DShot600"},
    {"value": 7, "label": "DShot1200"},
    {"value": 8, "label": "PWM range"},
    {"value": 9, "label": "PWM angle"},
]

ON_OFF_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Disabled"},
    {"value": 1, "label": "Enabled"},
]

# One bank, because ArduPilot has one: SERVO1..n covers every output the board
# has, MAIN and AUX alike. Kept in the same shape as the PX4 module's banks so
# the page and the assign endpoint do not have to care.
BANKS: list[dict[str, Any]] = [
    {"id": "SERVO", "label": "Output", "prefix": "SERVO",
     "pins": MAX_SERVO_OUTPUTS,
     "hint": "ArduPilot numbers every output on the board in one run, so there "
             "is no separate MAIN and AUX here — SERVO9 is usually the first "
             "AUX pin on a Pixhawk."},
]


def motor_function_value(motor: int) -> float | None:
    """The ``SERVO<n>_FUNCTION`` value that means "this is Motor *motor*"."""
    value = MOTOR_FUNCTIONS.get(int(motor))
    return None if value is None else float(value)


def motor_of(value: float) -> int | None:
    """Which motor an output function value drives, or None."""
    return MOTOR_BY_FUNCTION.get(int(round(value)))


def function_label(value: float) -> str:
    """Operator-facing name for one output function value."""
    ivalue = int(round(value))
    motor = MOTOR_BY_FUNCTION.get(ivalue)
    if motor is not None:
        return f"Motor {motor}"
    return SERVO_FUNCTIONS.get(ivalue, f"Function {ivalue}")


def function_param(bank_id: str, pin: int) -> str | None:
    """``SERVO<pin>_FUNCTION`` for a valid bank and pin, else None."""
    if str(bank_id).upper() != "SERVO":
        return None
    try:
        number_ = int(pin)
    except (TypeError, ValueError):
        return None
    if not 1 <= number_ <= MAX_SERVO_OUTPUTS:
        return None
    return f"SERVO{number_}_FUNCTION"


def frame_class(values: dict[str, float]) -> int:
    try:
        return int(round(float(values.get("FRAME_CLASS", 0.0))))
    except (TypeError, ValueError):
        return 0


def rotor_count(values: dict[str, float]) -> int:
    """How many motors this frame class has.

    Derived, not read: ArduPilot has no motor-count parameter, because the
    count is a property of the compiled-in layout.
    """
    return FRAME_CLASSES.get(frame_class(values), ("", 0, ""))[1]


def airframe_family(values: dict[str, float]) -> str:
    return FRAME_CLASSES.get(frame_class(values), ("", 0, "multirotor"))[2]


def param_names() -> list[str]:
    """Every parameter the Motors page may need, in one flat list."""
    names: list[str] = [
        "FRAME_CLASS", "FRAME_TYPE",
        "MOT_PWM_TYPE", "MOT_PWM_MIN", "MOT_PWM_MAX",
        "MOT_SPIN_ARM", "MOT_SPIN_MIN", "MOT_SPIN_MAX",
        "MOT_THST_HOVER", "MOT_THST_EXPO",
        "MOT_BAT_VOLT_MIN", "MOT_BAT_VOLT_MAX", "MOT_SAFE_DISARM",
        "SERVO_DSHOT_RATE", "SERVO_DSHOT_ESC", "SERVO_BLH_AUTO", "SERVO_BLH_MASK",
    ]
    for pin in range(1, MAX_SERVO_OUTPUTS + 1):
        names.append(f"SERVO{pin}_FUNCTION")
        names.append(f"SERVO{pin}_MIN")
        names.append(f"SERVO{pin}_MAX")
    return names


def outputs(values: dict[str, float]) -> list[dict[str, Any]]:
    """Every output pin the vehicle answered for, with what it currently drives."""
    entries: list[dict[str, Any]] = []
    for bank in BANKS:
        for pin in range(1, int(bank["pins"]) + 1):
            param = f"{bank['prefix']}{pin}_FUNCTION"
            if param not in values:
                continue
            value = values[param]
            entries.append({
                "bank": bank["id"], "pin": pin, "param": param,
                "label": f"{bank['label']} {pin}",
                "value": value,
                "function": function_label(value),
                "motor": motor_of(value),
            })
    return entries


def banks(output_entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The banks that actually exist on this board."""
    seen = {e["bank"] for e in output_entries}
    return [{"id": b["id"], "label": b["label"], "hint": b["hint"]}
            for b in BANKS if b["id"] in seen]


def _motors(values: dict[str, float],
            output_entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """One entry per motor: which output drives it, and its endpoints.

    No position and no spin direction, deliberately — see the module docstring.
    The frontend draws motors with no geometry on an evenly spread ring inside
    a dashed circle, which is the truthful rendering of "the firmware did not
    tell us where these are".
    """
    by_motor = {e["motor"]: e for e in output_entries if e["motor"] is not None}
    motors: list[dict[str, Any]] = []
    for number_ in range(1, rotor_count(values) + 1):
        assigned = by_motor.get(number_)
        fields: list[dict[str, Any]] = []
        if assigned is not None:
            pin = assigned["pin"]
            fields = present([
                number(f"SERVO{pin}_MIN", "Output minimum", values, unit="us", step=1),
                number(f"SERVO{pin}_MAX", "Output maximum", values, unit="us", step=1),
            ])
        motors.append({
            "index": number_ - 1,
            "number": number_,
            "label": f"Motor {number_}",
            # Zero, and the page's dashed ring says so: ArduPilot publishes no
            # motor geometry at all.
            "x": 0.0, "y": 0.0, "z": 0.0,
            "axis": None,
            "thrust": None,
            "spin": None,
            "fields": fields,
            "output": None if assigned is None else {
                "bank": assigned["bank"], "pin": assigned["pin"],
                "param": assigned["param"], "label": assigned["label"],
            },
        })
    return motors


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Motors page description.

    Same return shape as :func:`corvus.motor_config.build`, so the page renders
    both stacks with one code path: ``sections``, ``geometry``, ``motors``,
    ``outputs``, ``banks``, ``airframe_family`` and the counts.
    """
    geometry = present([
        enum("FRAME_CLASS", "Frame class", values, FRAME_CLASS_OPTIONS,
             reload=True,
             hint="Which layout the firmware mixes for. Changing it redefines "
                  "what Motor 3 means and takes effect after a reboot."),
        enum("FRAME_TYPE", "Frame type", values, FRAME_TYPE_OPTIONS, reload=True,
             hint="The arrangement within the class — X or Plus on a quad."),
    ])

    sections: list[dict[str, Any]] = []
    protocol = present([
        enum("MOT_PWM_TYPE", "Output protocol", values, MOT_PWM_TYPE_OPTIONS,
             hint="A protocol change takes effect after a reboot of the autopilot."),
        number("MOT_PWM_MIN", "PWM minimum", values, unit="us", step=1),
        number("MOT_PWM_MAX", "PWM maximum", values, unit="us", step=1),
        number("SERVO_DSHOT_RATE", "DShot rate", values, step=1),
        enum("SERVO_BLH_AUTO", "BLHeli passthrough", values, ON_OFF_OPTIONS,
             hint="Lets a configurator reach the ESCs through the autopilot."),
        number("SERVO_BLH_MASK", "BLHeli channel mask", values, step=1),
    ])
    if protocol:
        sections.append({
            "id": "protocol", "title": "Output protocol", "fields": protocol,
            "hint": "ArduPilot sets the protocol for all motor outputs at once, "
                    "not per timer group.",
        })

    spin = present([
        number("MOT_SPIN_ARM", "Spin when armed", values, step=0.01, min=0, max=1,
               hint="Throttle, 0-1, that the motors turn at the moment the "
                    "vehicle arms. Just enough that every motor is visibly "
                    "turning — this is a safety cue as much as a setting."),
        number("MOT_SPIN_MIN", "Minimum flight throttle", values,
               step=0.01, min=0, max=1,
               hint="The lowest throttle used in flight. Must be above the "
                    "arming spin, or a descent can stop a motor."),
        number("MOT_SPIN_MAX", "Maximum throttle", values, step=0.01, min=0, max=1),
        number("MOT_THST_HOVER", "Hover throttle", values, step=0.01, min=0, max=1,
               hint="Learned in flight. A value near 0.2 or near 0.8 means the "
                    "aircraft is badly over- or under-powered."),
        number("MOT_THST_EXPO", "Thrust curve expo", values, step=0.01,
               hint="Linearises the ESC's thrust curve. 0.55 suits most "
                    "hobby ESCs; smaller propellers want less."),
        number("MOT_BAT_VOLT_MIN", "Battery voltage, minimum", values,
               unit="V", step=0.1, min=0),
        number("MOT_BAT_VOLT_MAX", "Battery voltage, maximum", values,
               unit="V", step=0.1, min=0),
        enum("MOT_SAFE_DISARM", "Stop PWM when disarmed", values, ON_OFF_OPTIONS),
    ])
    if spin:
        sections.append({
            "id": "spin", "title": "Throttle and thrust", "fields": spin,
            "hint": "The group that genuinely has to be set per airframe. Every "
                    "value is a fraction of full throttle, 0 to 1.",
        })

    output_entries = outputs(values)
    label = FRAME_CLASSES.get(frame_class(values), ("", 0, ""))[0]
    return {
        "stack": "ardupilot",
        "sections": sections,
        "geometry": geometry,
        "motors": _motors(values, output_entries),
        "outputs": output_entries,
        "banks": banks(output_entries),
        "airframe_family": airframe_family(values),
        "airframe_label": label,
        "airframe_preset": None,
        "rotor_count": rotor_count(values),
        "max_motors": MAX_ROTORS,
        # ArduPilot's motor count follows FRAME_CLASS, so the page's add/remove
        # motor buttons have nothing to write.
        "fixed_motor_count": True,
        "received": len(values),
    }
