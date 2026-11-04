"""PX4 motor/actuator parameter schema for the Setup -> Motors page.

This module owns the *knowledge* of which PX4 parameters describe a vehicle's
motors, what they mean, and how to present them; the bridge only fetches raw
values and the HTTP layer only serialises. Keeping the schema here is the same
split the parameter editor uses — the frontend renders whatever description it
is handed and never hardcodes a PX4 parameter name.

The page is motor-centric, not pin-centric. PX4 stores the mapping the other
way round (each output pin names the function it drives), but an operator
holding an aircraft thinks "which pin is motor 3 on?", not "what is on pin 7?".
So :func:`build` inverts the mapping into one entry per motor, each carrying its
arm position, its spin direction and the output it is wired to, and a fixed
four-motor list stops being the shape of the page: a quad shows four motors, a
hexa six, and adding one is a write to ``CA_ROTOR_COUNT``.

Version tolerance (AGENTS.md: PX4 v1.16 / v1.17 / v1.18 are the target set, and
a parameter that is absent must never break the page):

* Every field is *candidate* only. :func:`build` emits a field solely when the
  vehicle actually returned that parameter, so a firmware without, say,
  ``PWM_MAIN_TIM3`` simply shows one protocol group fewer instead of an error.
* The same holds for whole output banks: a board with no AUX pins and no
  DroneCAN ESCs answers for neither, so neither is offered.
* Both the modern per-timer protocol parameters (``PWM_MAIN_TIM*``, v1.14+
  control allocation) and the legacy ``DSHOT_CONFIG`` are candidates, so the
  page is correct on whichever the connected firmware exposes.
* The same for output limits: control allocation keeps them per channel
  (``PWM_MAIN_MIN1``, ``PWM_MAIN_DIS1``...), older firmware kept one global
  ``PWM_MAIN_MIN``. The per-channel ones are read only for the pins that
  drive a motor (:func:`output_param_names`), so a quad asks for sixteen
  names rather than a hundred.
* An enum value the schema does not know is preserved verbatim as an extra
  "Unknown (n)" option rather than being snapped to the first choice. Writing a
  silently-changed airframe or output function to a real aircraft would be far
  worse than showing a number.
"""

from __future__ import annotations

from typing import Any

# Control allocation supports at most 12 rotors (CA_ROTOR0..CA_ROTOR11); the
# output functions reach Motor 16, which is why the two caps differ.
MAX_ROTORS = 12
MAX_MOTOR_FUNCTIONS = 16

# PWM_*_FUNCn encodes Motor n as 100 + n and Servo n as 200 + n.
MOTOR_FUNCTION_BASE = 100
SERVO_FUNCTION_BASE = 200

# Timer groups that carry the output protocol (DShot / OneShot / PWM rate).
TIMER_GROUPS = 4

# The output banks a motor can be wired to. "MAIN" and "AUX" are the FMU and IO
# PWM headers; "CAN" is DroneCAN ESCs and "SIM" the ESCs of Gazebo SITL, which
# is where a simulated airframe's motors live (``SIM_GZ_EC_FUNC1``, not
# ``PWM_MAIN_FUNC1``). A board without a bank never answers for it, so it
# simply never appears.
#
# ``limits`` are the per-channel standard parameters PX4's actuator generator
# gives each bank (``<prefix>_<suffix><pin>``) with their ranges from the
# bank's module.yaml. DroneCAN ESCs have no disarmed value. The unit is a pulse
# width only on the PWM headers; the other two are raw ESC commands.
_PWM_LIMITS: list[dict[str, Any]] = [
    {"suffix": "MIN", "label": "Minimum", "min": 800, "max": 1400},
    {"suffix": "MAX", "label": "Maximum", "min": 1600, "max": 2200},
    {"suffix": "DIS", "label": "Disarmed", "min": 800, "max": 2200},
    {"suffix": "FAIL", "label": "Failsafe", "min": -1, "max": 2200},
]
BANKS: list[dict[str, Any]] = [
    {"id": "MAIN", "label": "MAIN", "prefix": "PWM_MAIN", "pins": 16,
     "hint": "FMU PWM header", "unit": "us", "limits": _PWM_LIMITS},
    {"id": "AUX", "label": "AUX", "prefix": "PWM_AUX", "pins": 8,
     "hint": "IO / AUX PWM header", "unit": "us", "limits": _PWM_LIMITS},
    {"id": "CAN", "label": "DroneCAN", "prefix": "UAVCAN_EC", "pins": 8,
     "hint": "DroneCAN ESC index", "unit": "", "limits": [
         {"suffix": "MIN", "label": "Minimum", "min": 0, "max": 8191},
         {"suffix": "MAX", "label": "Maximum", "min": 0, "max": 8191},
         {"suffix": "FAIL", "label": "Failsafe", "min": -1, "max": 8191},
     ]},
    {"id": "SIM", "label": "Simulation", "prefix": "SIM_GZ_EC", "pins": 16,
     "hint": "Gazebo SITL ESC", "unit": "", "limits": [
         {"suffix": "MIN", "label": "Minimum", "min": 0, "max": 1000},
         {"suffix": "MAX", "label": "Maximum", "min": 0, "max": 1000},
         {"suffix": "DIS", "label": "Disarmed", "min": 0, "max": 1000},
         {"suffix": "FAIL", "label": "Failsafe", "min": -1, "max": 1000},
     ]},
]

# The drawing family each CA_AIRFRAME value belongs to. The Motors page draws
# the airframe, and a quad, a plane and a helicopter are not the same picture:
# this is the mapping from PX4's airframe class to the silhouette behind the
# motors. Anything unrecognised falls back to "multirotor", which is the arms
# -and-ring drawing that needs no assumption beyond the rotor positions.
AIRFRAME_FAMILIES: dict[int, str] = {
    0: "multirotor",
    1: "wing",
    2: "vtol",
    3: "vtol",
    4: "vtol",
    5: "rover",
    6: "rover",
    7: "multirotor",
    8: "multirotor",
    9: "multirotor",
    10: "helicopter",
    11: "helicopter",
    12: "helicopter",
}
DEFAULT_FAMILY = "multirotor"

# CA_AIRFRAME — the *geometry* class the control allocator solves for. This is
# not SYS_AUTOSTART (the airframe preset); see the note in build().
AIRFRAME_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "Multirotor"},
    {"value": 1, "label": "Fixed wing"},
    {"value": 2, "label": "Standard VTOL"},
    {"value": 3, "label": "Tiltrotor VTOL"},
    {"value": 4, "label": "Tailsitter VTOL"},
    {"value": 5, "label": "Rover (Ackermann)"},
    {"value": 6, "label": "Rover (Differential)"},
    {"value": 7, "label": "Motors (6DOF)"},
    {"value": 8, "label": "Multirotor with tilt"},
    {"value": 9, "label": "Custom"},
    {"value": 10, "label": "Helicopter (tail ESC)"},
    {"value": 11, "label": "Helicopter (tail Servo)"},
    {"value": 12, "label": "Helicopter (Coaxial)"},
]

# PWM_MAIN_TIMn / PWM_AUX_TIMn — negative values select a digital protocol,
# positive values are a plain PWM rate in Hz. The numbers are PX4's
# (src/drivers/pwm_out/module.yaml, the same on v1.16, v1.17 and v1.18): the
# slowest DShot has the most negative value, and there is no DShot1200.
PROTOCOL_OPTIONS: list[dict[str, Any]] = [
    {"value": -5, "label": "DShot150"},
    {"value": -4, "label": "DShot300"},
    {"value": -3, "label": "DShot600"},
    {"value": -1, "label": "OneShot"},
    {"value": 50, "label": "PWM 50 Hz"},
    {"value": 100, "label": "PWM 100 Hz"},
    {"value": 200, "label": "PWM 200 Hz"},
    {"value": 400, "label": "PWM 400 Hz"},
]

# Bidirectional DShot, v1.18 only. Not offered, since v1.16 and v1.17 have no
# such values and the page cannot tell the versions apart by what answers;
# named when a timer already holds one.
BIDIR_PROTOCOL_OPTIONS: list[dict[str, Any]] = [
    {"value": -8, "label": "Bidirectional DShot150"},
    {"value": -7, "label": "Bidirectional DShot300"},
    {"value": -6, "label": "Bidirectional DShot600"},
]

# DSHOT_CONFIG — the pre-control-allocation way to pick DShot, kept as a
# fallback for older firmware that still carries it.
DSHOT_CONFIG_OPTIONS: list[dict[str, Any]] = [
    {"value": 0, "label": "PWM (DShot off)"},
    {"value": 150, "label": "DShot150"},
    {"value": 300, "label": "DShot300"},
    {"value": 600, "label": "DShot600"},
    {"value": 1200, "label": "DShot1200"},
]


def motor_function_value(motor: int) -> float | None:
    """The output-function value that means "this is Motor *motor*".

    PX4 numbers them contiguously from MOTOR_FUNCTION_BASE. ArduPilot does not,
    which is why the assign endpoint asks for the value instead of doing the
    arithmetic itself — see :func:`corvus.ardupilot_motors.motor_function_value`.
    """
    number = int(motor)
    if not 1 <= number <= MAX_MOTOR_FUNCTIONS:
        return None
    return float(MOTOR_FUNCTION_BASE + number)


def function_param(bank_id: str, pin: int) -> str | None:
    """Name of the parameter that selects what output *pin* of *bank_id* drives."""
    for bank in BANKS:
        if bank["id"] == bank_id:
            if 1 <= pin <= int(bank["pins"]):
                return f"{bank['prefix']}_FUNC{pin}"
            return None
    return None


def function_label(value: float) -> str:
    """Human name for a PWM_*_FUNCn value ("Motor 3", "Servo 1", "Disabled")."""
    ivalue = int(round(value))
    if ivalue == 0:
        return "Disabled"
    motor = ivalue - MOTOR_FUNCTION_BASE
    if 1 <= motor <= MAX_MOTOR_FUNCTIONS:
        return f"Motor {motor}"
    servo = ivalue - SERVO_FUNCTION_BASE
    if 1 <= servo <= 8:
        return f"Servo {servo}"
    return f"Function {ivalue}"


def motor_of(value: float) -> int | None:
    """The 1-based motor a PWM_*_FUNCn value drives, or None if it is not a motor."""
    motor = int(round(value)) - MOTOR_FUNCTION_BASE
    return motor if 1 <= motor <= MAX_MOTOR_FUNCTIONS else None


def param_names() -> list[str]:
    """Every parameter the Motors page may need, in one flat list.

    Handed to the bridge as a single batched read: the page asks for the whole
    superset once and renders what came back, rather than probing name by name.
    """
    names: list[str] = ["SYS_AUTOSTART", "CA_AIRFRAME", "CA_ROTOR_COUNT"]
    for i in range(MAX_ROTORS):
        names += [
            f"CA_ROTOR{i}_PX", f"CA_ROTOR{i}_PY", f"CA_ROTOR{i}_PZ",
            f"CA_ROTOR{i}_KM", f"CA_ROTOR{i}_CT",
            # The thrust axis is what separates a VTOL's lift rotors from its
            # pusher, and a plane's tractor from anything else.
            f"CA_ROTOR{i}_AX", f"CA_ROTOR{i}_AY", f"CA_ROTOR{i}_AZ",
        ]
    for bank in BANKS:
        for pin in range(1, int(bank["pins"]) + 1):
            names.append(f"{bank['prefix']}_FUNC{pin}")
    for suffix in ("MAIN", "AUX"):
        for i in range(TIMER_GROUPS):
            names.append(f"PWM_{suffix}_TIM{i}")
        names += [f"PWM_{suffix}_MIN", f"PWM_{suffix}_MAX", f"PWM_{suffix}_DISARM"]
    names += ["DSHOT_CONFIG", "DSHOT_MIN"]
    return names


def output_param_names(values: dict[str, float]) -> list[str]:
    """The per-channel limits of every pin that drives a motor.

    The second read of the Motors page, made once the first has said which
    pins are wired. Asking for the limits of all 48 pins up front would triple
    the burst, and on a telemetry radio most of it would be for pins nothing
    is plugged into.
    """
    names: list[str] = []
    for entry in outputs(values):
        if entry["motor"] is None:
            continue
        bank = _bank(entry["bank"])
        for limit in bank["limits"]:
            names.append(f"{bank['prefix']}_{limit['suffix']}{entry['pin']}")
    return names


def _bank(bank_id: str) -> dict[str, Any]:
    return next(b for b in BANKS if b["id"] == bank_id)


def _output_fields(values: dict[str, float],
                   entry: dict[str, Any] | None) -> list[dict[str, Any]]:
    """The limits of the pin a motor is wired to, as far as the vehicle has them."""
    if entry is None:
        return []
    bank = _bank(entry["bank"])
    fields: list[dict[str, Any]] = []
    for limit in bank["limits"]:
        extra: dict[str, Any] = {"step": 1, "min": limit["min"], "max": limit["max"]}
        if bank["unit"]:
            extra["unit"] = bank["unit"]
        if limit["suffix"] == "FAIL":
            extra["hint"] = "-1 keeps the default for the output's function."
        field = _number(f"{bank['prefix']}_{limit['suffix']}{entry['pin']}",
                        limit["label"], values, **extra)
        if field is not None:
            fields.append(field)
    return fields


def _enum_options(options: list[dict[str, Any]], value: float) -> list[dict[str, Any]]:
    """Options list guaranteed to contain *value*.

    A firmware may use an enum member this build has never heard of. Snapping
    such a value to the first option would rewrite a real aircraft's airframe or
    motor assignment the moment the operator touched an unrelated field, so the
    unknown number is appended as its own option instead.
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
    field = {
        "param": name, "label": label, "kind": "enum",
        "value": value, "options": _enum_options(options, value),
    }
    field.update(extra)
    return field


def _number(name: str, label: str, values: dict[str, float], **extra: Any) -> dict[str, Any] | None:
    if name not in values:
        return None
    field = {"param": name, "label": label, "kind": "number", "value": values[name]}
    field.update(extra)
    return field


def _sign(name: str, label: str, values: dict[str, float]) -> dict[str, Any] | None:
    """A CW/CCW choice backed by the *sign* of a coefficient parameter.

    ``CA_ROTORn_KM`` encodes the spin direction in its sign and the moment
    strength in its magnitude, so this field is matched on the sign and written
    back as ``-value``/``+value`` — flipping the propeller direction must never
    quietly rewrite the tuned magnitude to 1.
    """
    if name not in values:
        return None
    value = values[name]
    return {
        "param": name, "label": label, "kind": "sign", "value": value,
        "options": [{"value": 1, "label": "CCW"}, {"value": -1, "label": "CW"}],
    }


def airframe_family(values: dict[str, float]) -> str:
    """Which silhouette the page should draw behind the motors.

    An airframe class this build has never heard of falls back to the
    multirotor drawing, which assumes nothing beyond the rotor positions —
    better a plain arms-and-ring picture than a wing that is not there.
    """
    raw = values.get("CA_AIRFRAME")
    if raw is None:
        return DEFAULT_FAMILY
    return AIRFRAME_FAMILIES.get(int(round(raw)), DEFAULT_FAMILY)


def _axis(values: dict[str, float], index: int) -> dict[str, Any] | None:
    """A rotor's thrust axis, or None when this firmware does not report one.

    PX4's default is (0, 0, -1) — straight up, since body Z points down. A VTOL
    pusher is (1, 0, 0) and a tailsitter's rotors are too, which is exactly the
    distinction the drawing needs to show a propeller edge-on instead of as a
    lift disc.
    """
    names = [f"CA_ROTOR{index}_A{a}" for a in ("X", "Y", "Z")]
    if not any(n in values for n in names):
        return None
    x, y, z = (float(values.get(n, 0.0)) for n in names)
    length = (x * x + y * y + z * z) ** 0.5
    if length < 1e-6:
        return None
    return {
        "x": x / length, "y": y / length, "z": z / length,
        # "lift" when the axis is mostly vertical, "horizontal" when it mostly
        # points along the airframe — the two cases the drawing renders apart.
        "kind": "lift" if abs(z) >= max(abs(x), abs(y)) else "horizontal",
    }


def rotor_count(values: dict[str, float]) -> int:
    """How many motors the geometry has.

    Prefers the vehicle's CA_ROTOR_COUNT and falls back to counting the
    ``CA_ROTOR*_PX`` parameters that actually arrived, so a firmware without the
    count parameter still shows its motors instead of none.
    """
    raw = values.get("CA_ROTOR_COUNT")
    if raw is not None:
        return max(0, min(MAX_ROTORS, int(round(raw))))
    return sum(1 for i in range(MAX_ROTORS) if f"CA_ROTOR{i}_PX" in values)


def outputs(values: dict[str, float]) -> list[dict[str, Any]]:
    """Every output pin the vehicle answered for, with what it currently drives.

    This is the catalogue the assignment dropdown is built from and the table
    the assign endpoint consults to find a motor's current pin. A bank the board
    does not have contributes nothing.
    """
    entries: list[dict[str, Any]] = []
    for bank in BANKS:
        for pin in range(1, int(bank["pins"]) + 1):
            param = f"{bank['prefix']}_FUNC{pin}"
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
    """The banks that actually exist on this board, in BANKS order."""
    present = {e["bank"] for e in output_entries}
    return [
        {"id": b["id"], "label": b["label"], "hint": b["hint"]}
        for b in BANKS if b["id"] in present
    ]


def _motors(values: dict[str, float],
            output_entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """One entry per motor: where it sits, which way it turns, what drives it.

    The position triple is the motor's offset from the centre of gravity in
    metres — the distances the airframe diagram is drawn from, so a 450 mm quad
    looks like one and a 250 mm one does not. ``CA_ROTORn_KM`` carries the spin
    direction in its *sign* (negative = clockwise).
    """
    by_motor = {e["motor"]: e for e in output_entries if e["motor"] is not None}
    motors: list[dict[str, Any]] = []
    for i in range(rotor_count(values)):
        number = i + 1
        km = values.get(f"CA_ROTOR{i}_KM")
        fields = [f for f in (
            _number(f"CA_ROTOR{i}_PX", "X", values, unit="m", step=0.001),
            _number(f"CA_ROTOR{i}_PY", "Y", values, unit="m", step=0.001),
            _number(f"CA_ROTOR{i}_PZ", "Z", values, unit="m", step=0.001),
            _sign(f"CA_ROTOR{i}_KM", "Spin", values),
            _number(f"CA_ROTOR{i}_CT", "Thrust coef.", values, step=0.1),
        ) if f is not None]
        assigned = by_motor.get(number)
        axis = _axis(values, i)
        position = {a.lower(): f"CA_ROTOR{i}_P{a}" for a in ("X", "Y")
                    if f"CA_ROTOR{i}_P{a}" in values}
        motors.append({
            "index": i,
            "number": number,
            "label": f"Motor {number}",
            "x": float(values.get(f"CA_ROTOR{i}_PX", 0.0)),
            "y": float(values.get(f"CA_ROTOR{i}_PY", 0.0)),
            "z": float(values.get(f"CA_ROTOR{i}_PZ", 0.0)),
            "axis": axis,
            "thrust": axis["kind"] if axis else None,
            # The parameters behind x and y, for the frame size control, which
            # rewrites every lift rotor's position at once.
            "position_params": position if len(position) == 2 else None,
            "spin": None if km is None else ("CW" if km < 0 else "CCW"),
            "fields": fields,
            "output_fields": _output_fields(values, assigned),
            "output": None if assigned is None else {
                "bank": assigned["bank"], "pin": assigned["pin"],
                "param": assigned["param"], "label": assigned["label"],
            },
        })
    return motors


# Below this a frame has no measurable size: its motor positions are not set.
MIN_FRAME_M = 1e-3

FRAME_HINT = ("Scales the motor positions around the middle of the frame, and keeps "
              "where the centre of gravity sits relative to that middle. The diagonal "
              "is the distance between opposite motors that frames are sold by, 450 mm "
              "on a 450 frame. Length and width change one axis alone, for an H or a "
              "stretched frame. PX4 normalises the mix, so the proportions and the "
              "centre of gravity matter more than the overall size.")


def _metres(value: float) -> float:
    """A distance to a tenth of a millimetre: finer than anyone measures an arm."""
    return round(float(value), 4)


def frame(motors: list[dict[str, Any]]) -> dict[str, Any]:
    """The frame's measured size, and the rotors a size change moves.

    Only lift rotors count. A quadplane's pusher sits on the tail boom, and
    stretching the lift frame must not move it, nor let its position decide how
    long the frame is. ``length`` is the extent along X (front to back),
    ``width`` along Y, ``diagonal`` the largest distance between two rotors. An
    axis along which every rotor sits in one line has no extent to scale, and
    comes back as None.
    """
    lift = [m for m in motors
            if m.get("thrust") != "horizontal" and m.get("position_params")]
    if len(lift) < 2:
        return {"adjustable": False,
                "note": "The frame size needs at least two lift rotors with positions. "
                        "Set each motor's position below instead."}
    xs = [float(m["x"]) for m in lift]
    ys = [float(m["y"]) for m in lift]
    diagonal = max(
        ((a["x"] - b["x"]) ** 2 + (a["y"] - b["y"]) ** 2) ** 0.5
        for i, a in enumerate(lift) for b in lift[i + 1:])
    if diagonal < MIN_FRAME_M:
        return {"adjustable": False,
                "note": "No motor positions are set yet, so there is no size to scale. "
                        "Set each motor's position below, or pick an airframe preset."}
    length = max(xs) - min(xs)
    width = max(ys) - min(ys)
    return {
        "adjustable": True,
        "length": _metres(length) if length >= MIN_FRAME_M else None,
        "width": _metres(width) if width >= MIN_FRAME_M else None,
        "diagonal": _metres(diagonal),
        "center": {"x": (max(xs) + min(xs)) / 2, "y": (max(ys) + min(ys)) / 2},
        "motors": [{
            "number": m["number"], "x": float(m["x"]), "y": float(m["y"]),
            "x_param": m["position_params"]["x"], "y_param": m["position_params"]["y"],
        } for m in lift],
        "hint": FRAME_HINT,
    }


def _protocol_options(values: dict[str, float], name: str) -> list[dict[str, Any]]:
    held = values.get(name)
    if held is None:
        return PROTOCOL_OPTIONS
    return PROTOCOL_OPTIONS + [
        o for o in BIDIR_PROTOCOL_OPTIONS if o["value"] == int(round(held))]


def _protocol(values: dict[str, float], bank: str) -> list[dict[str, Any]]:
    """Output protocol + endpoints for one bank (DShot / OneShot / PWM rate)."""
    fields = []
    for i in range(TIMER_GROUPS):
        name = f"PWM_{bank}_TIM{i}"
        field = _enum(name, f"Timer group {i}", values, _protocol_options(values, name))
        if field is not None:
            fields.append(field)
    # One limit for the whole bank: firmware before control allocation only.
    # From v1.14 the limits are per pin and shown on the motor they drive.
    for suffix, label in (("MIN", "PWM minimum"), ("MAX", "PWM maximum"),
                          ("DISARM", "PWM disarmed")):
        field = _number(f"PWM_{bank}_{suffix}", label, values, unit="us", step=1)
        if field is not None:
            fields.append(field)
    return fields


def build(values: dict[str, float]) -> dict[str, Any]:
    """Turn raw ``{param: value}`` into the Motors page description.

    *values* holds only the parameters the vehicle actually answered for, so
    every section shrinks or disappears on a firmware that lacks them. The
    return shape is what ``GET /api/motors`` serialises:

    ``sections``  ordered generic forms (output protocol per bank), each
                  ``{id, title, hint, fields}``. Every field names the PX4
                  parameter it writes, so the frontend applies edits through the
                  existing ``POST /api/params/set``, and a field marked
                  ``reload`` tells the page to re-read the whole configuration
                  after a successful write — changing the airframe or the motor
                  count changes which fields exist at all.
    ``geometry``  the airframe class and motor count, as the same kind of
                  fields, to be rendered on the airframe card beside the
                  drawing they change.
    ``airframe_family``  which silhouette to draw: multirotor, wing, vtol,
                  rover or helicopter.
    ``motors``    one entry per motor: position, spin, per-motor parameter
                  fields, the output it is wired to and that output's limits
                  (``output_fields``, present once :func:`output_param_names`
                  has been read too). Drives the airframe diagram and the
                  click-to-select detail panel.
    ``frame``     the frame's size (length, width, diagonal) and the lift
                  rotors a size change rewrites; see :func:`frame`.
    ``outputs``   every output pin that exists, and what it currently drives.
    ``banks``     the output banks this board actually has.
    """
    sections: list[dict[str, Any]] = []

    # --- Geometry. CA_AIRFRAME is the airframe *class* the allocator solves
    # for; it defines what "Motor 3" even means and which silhouette the page
    # draws, so it is returned beside the drawing rather than in the generic
    # form sections — the airframe is chosen on the airframe card, next to the
    # picture it changes. SYS_AUTOSTART — the airframe *preset* — is a
    # different act: it rewrites a whole config bundle and needs a reboot, so it
    # is reported as read-only context and left to the parameter editor.
    geometry = [f for f in (
        _enum("CA_AIRFRAME", "Airframe type", values, AIRFRAME_OPTIONS,
              reload=True,
              hint="The geometry the control allocator solves for. Changing it "
                   "redefines the motor layout."),
        _number("CA_ROTOR_COUNT", "Motor count", values, step=1, min=0, max=MAX_ROTORS,
                reload=True),
    ) if f is not None]

    main_protocol = _protocol(values, "MAIN")
    dshot = _enum("DSHOT_CONFIG", "DShot mode", values, DSHOT_CONFIG_OPTIONS,
                  hint="Legacy DShot selector. Newer firmware uses the timer groups above.")
    if dshot is not None:
        main_protocol.append(dshot)
    dshot_min = _number("DSHOT_MIN", "DShot minimum output", values,
                        step=0.01, min=0, max=1,
                        hint="The lowest command a DShot ESC gets while armed, as a "
                             "fraction of full. High enough that every motor keeps "
                             "turning.")
    if dshot_min is not None:
        main_protocol.append(dshot_min)
    if main_protocol:
        sections.append({
            "id": "protocol_main", "title": "Output protocol: MAIN",
            "fields": main_protocol,
            "hint": "Timer groups are shared by several outputs. A protocol change takes "
                    "effect after a reboot of the autopilot.",
        })
    aux_protocol = _protocol(values, "AUX")
    if aux_protocol:
        sections.append({
            "id": "protocol_aux", "title": "Output protocol: AUX",
            "fields": aux_protocol,
        })

    output_entries = outputs(values)
    autostart = values.get("SYS_AUTOSTART")
    airframe = values.get("CA_AIRFRAME")
    motors = _motors(values, output_entries)
    return {
        "sections": sections,
        "geometry": geometry,
        "motors": motors,
        "frame": frame(motors),
        "outputs": output_entries,
        "banks": banks(output_entries),
        "airframe_family": airframe_family(values),
        "airframe_label": next(
            (o["label"] for o in AIRFRAME_OPTIONS
             if airframe is not None and int(o["value"]) == int(round(airframe))), ""),
        "airframe_preset": int(round(autostart)) if autostart is not None else None,
        "rotor_count": rotor_count(values),
        "max_motors": MAX_ROTORS,
        "received": len(values),
    }
