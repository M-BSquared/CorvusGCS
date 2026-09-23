"""SiK telemetry-radio schema for the Setup -> Telemetry Radio page.

Same split as :mod:`corvus.safety_config` and :mod:`corvus.rc_config`: this
module owns the *knowledge* of what a SiK radio's settings mean, the service in
:mod:`corvus.sik_service` owns the serial port and the AT dialogue, and the HTTP
layer only serialises. The frontend renders whatever description it is handed
and never hardcodes an S-register number.

What a SiK radio is, and why it is not a parameter page
-------------------------------------------------------
Every other configuration page in Corvus edits the *autopilot* — values that
travel as MAVLink ``PARAM_SET`` and that the vehicle acknowledges. A SiK radio
is not on that path at all. It is the modem the MAVLink stream passes *through*,
it holds its own EEPROM, and it is configured out-of-band with a variant of the
Hayes AT command set: escape into command mode with ``+++``, read the registers
with ``ATI5``, write one with ``ATSn=X``, commit with ``AT&W``, reboot with
``ATZ``. Nothing about that is MAVLink, and the autopilot never learns it
happened.

That has two consequences the rest of this feature is shaped by:

*The port has to be taken.* AT mode means the serial link is no longer carrying
telemetry, so for the radio that *is* the live link the MAVLink bridge must be
stopped for the duration and restarted afterwards. Mission Planner solves this
by refusing to open its radio page unless you disconnect first; the service here
does the stop and the restart itself, and refuses while armed.

*There are two radios.* A link is a pair, and a pair that disagrees is not a
link. Every command has a remote form — ``RT`` in place of ``AT`` — which the
local radio relays over the air to its partner, so both ends can be read and
written from the one USB cable. :data:`MUST_MATCH` is the list of registers
where a disagreement means silence, and it is why the page has a "copy to
remote" action at all rather than two independent forms.

Version tolerance
-----------------
The register set is not fixed. Stock SiK ends at S15; newer builds add
encryption, RFD900-class firmware adds more still, and a clone may answer for
fewer. So :data:`REGISTERS` is a table of what is *known*, and
:func:`describe` renders whatever the radio actually reported: a register in the
table gets its label, its option list and its hint, and one that is not gets a
generic numeric field under its own reported name. Nothing is invented for a
register the radio did not answer for, and nothing the radio did answer for is
hidden — the same rule :mod:`corvus.safety_config` follows for PX4 parameters
that come and go between firmware versions.
"""

from __future__ import annotations

import re
from typing import Any

# The serial speed a SiK radio leaves the factory on, and the one Mission
# Planner's instructions tell every operator to select. It is the first baud a
# session tries, before falling back to the rest of BAUD_CANDIDATES.
DEFAULT_BAUD = 57600

# Bauds to try when the radio does not answer at the first one. A radio whose
# SERIAL_SPEED was changed and then forgotten is the common case this exists
# for: without it the operator is locked out of the very register that locked
# them out. Ordered by how likely each is in the field.
BAUD_CANDIDATES: tuple[int, ...] = (57600, 115200, 38400, 19200, 9600, 230400, 2400, 4800)

# Registers whose value must be identical at both ends of the link or the two
# radios cannot hear each other. Straight from the SiK documentation; this is
# the set the "copy to remote" action copies and the set a mismatch is reported
# for. Firmware version has to match too, but that is not a register — it is
# reported separately and compared in :func:`describe`.
MUST_MATCH: frozenset[str] = frozenset({
    "AIR_SPEED", "MIN_FREQ", "MAX_FREQ", "NUM_CHANNELS",
    "NETID", "ECC", "LBT_RSSI", "MAX_WINDOW",
})

# FORMAT is the EEPROM layout version. The radio writes it; nobody else may.
READ_ONLY: frozenset[str] = frozenset({"FORMAT"})

# The 13 air data rates the radio firmware can actually produce, in kbps. An
# unsupported number is not rejected by the radio — it silently rounds up to the
# next one in this list, which means an operator who typed 100 and got 128 would
# have no way to know. So the page offers the list instead of a free number.
AIR_SPEEDS: tuple[int, ...] = (2, 4, 8, 16, 19, 24, 32, 48, 64, 96, 128, 192, 250)

# Transmit power in dBm, with the milliwatts each one is, because the legal
# limit an operator is checking against is usually written in one unit and the
# register takes the other. Same rounding-up behaviour as the air rate.
TX_POWERS: tuple[tuple[int, str], ...] = (
    (1, "1 dBm (1.3 mW)"),
    (2, "2 dBm (1.6 mW)"),
    (5, "5 dBm (3.2 mW)"),
    (8, "8 dBm (6.3 mW)"),
    (11, "11 dBm (12.5 mW)"),
    (14, "14 dBm (25 mW)"),
    (17, "17 dBm (50 mW)"),
    (20, "20 dBm (100 mW)"),
)

# Serial speeds in the radio's "one byte form" (see :func:`baud_from_register`),
# paired with the real rate each one stands for. The pairing has to be a table
# rather than arithmetic: the register holds the baud rate with its trailing
# digits *truncated*, not divided, so 57 is 57600 and not 57000 and there is no
# multiplier that recovers both that and 9 -> 9600.
SERIAL_SPEEDS: tuple[tuple[int, int], ...] = (
    (1, 1200),
    (2, 2400),
    (4, 4800),
    (9, 9600),
    (19, 19200),
    (38, 38400),
    (57, 57600),
    (115, 115200),
    (230, 230400),
)

_BAUD_BY_REGISTER: dict[int, int] = {reg: baud for reg, baud in SERIAL_SPEEDS}
_REGISTER_BY_BAUD: dict[int, int] = {baud: reg for reg, baud in SERIAL_SPEEDS}

_OFF_ON: list[dict[str, Any]] = [
    {"value": 0, "label": "Off"},
    {"value": 1, "label": "On"},
]

_MAVLINK_MODES: list[dict[str, Any]] = [
    {"value": 0, "label": "Raw serial, no MAVLink framing"},
    {"value": 1, "label": "MAVLink framing and RSSI reporting"},
    {"value": 2, "label": "Low latency: prioritise RC override packets"},
]

_RTSCTS_MODES: list[dict[str, Any]] = [
    {"value": 0, "label": "Off"},
    {"value": 1, "label": "On"},
    {"value": 2, "label": "Auto"},
]


def _enum(options: tuple[tuple[int, str], ...]) -> list[dict[str, Any]]:
    return [{"value": value, "label": label} for value, label in options]


def _baud_enum() -> list[dict[str, Any]]:
    return [{"value": reg, "label": f"{baud} baud"} for reg, baud in SERIAL_SPEEDS]


# ---------------------------------------------------------------------------
# The register table
# ---------------------------------------------------------------------------

# Ordered as the radio reports them, which is also the order they are worth
# reading in: the link's identity first (who am I talking to, how fast), then
# power and protocol, then the regulatory band, then the settings that only
# matter once something is wrong.
#
# ``advanced`` marks the ones Mission Planner hides behind its "Advanced
# Options" checkbox. The distinction is not about danger — NETID is as capable
# of breaking a link as MIN_FREQ is — but about how often a correct value is
# already in place: the band, the channel count and the duty cycle are set by
# where the operator lives and are then never touched again.
REGISTERS: list[dict[str, Any]] = [
    {
        "name": "FORMAT", "label": "EEPROM format",
        "kind": "readonly", "advanced": True,
        "hint": "The layout version of the radio's own settings store. Set by the "
                "firmware and not editable.",
    },
    {
        "name": "SERIAL_SPEED", "label": "Baud rate",
        "kind": "enum", "options": _baud_enum(),
        "hint": "The speed of the wire between this radio and whatever it is plugged "
                "into: the ground station here, the autopilot's telemetry port at the "
                "far end. It does not have to match the other radio, but it does have "
                "to match what is on the other end of its own cable.",
    },
    {
        "name": "AIR_SPEED", "label": "Air data rate",
        "kind": "enum", "options": [
            {"value": v, "label": f"{v} kbps"} for v in AIR_SPEEDS
        ],
        "hint": "The speed the two radios talk to each other at. Lower reaches further "
                "and carries less: 64 kbps is the default and manages a kilometre or "
                "more on the small antennas the radios ship with.",
    },
    {
        "name": "NETID", "label": "Network ID",
        "kind": "number", "min": 0, "max": 499, "step": 1,
        "hint": "Which pair of radios this is. Two pairs on the same ID in the same "
                "field will interfere; give each aircraft its own number. It also seeds "
                "the frequency-hopping pattern, so different IDs hop differently.",
    },
    {
        "name": "TXPOWER", "label": "Transmit power",
        "kind": "enum", "options": _enum(TX_POWERS),
        "hint": "Check this against the limit where you are flying, and remember that "
                "the limit is on radiated power, and antenna gain counts towards it. The "
                "default of 20 dBm is inside the US and Australian 915 MHz allowance "
                "with an antenna below 10 dBi.",
    },
    {
        "name": "ECC", "label": "Error correction",
        "kind": "enum", "options": _OFF_ON,
        "hint": "Golay forward error correction: halves the usable bandwidth and "
                "recovers bit errors that would otherwise drop the packet. Newer radio "
                "chips cannot do it at all and will fail to run with it on, which is "
                "why current guidance is to leave it off.",
    },
    {
        "name": "MAVLINK", "label": "MAVLink framing",
        "kind": "enum", "options": _MAVLINK_MODES,
        "hint": "Aligns radio packets to MAVLink message boundaries so a lost packet "
                "does not deliver half a message. Leave this on MAVLink framing: RSSI "
                "and error reporting only arrive in that mode. Low latency additionally "
                "fast-tracks RC override, which is what a joystick flies on.",
    },
    {
        "name": "OPPRESEND", "label": "Opportunistic resend",
        "kind": "enum", "options": _OFF_ON, "advanced": True,
        "hint": "Resend a packet when the transmit window would otherwise go unused.",
    },
    {
        "name": "MIN_FREQ", "label": "Minimum frequency",
        "kind": "number", "unit": "kHz", "min": 240000, "max": 1000000, "step": 1000,
        "advanced": True,
        "hint": "The bottom of the band the radio hops within. A 433 MHz radio accepts "
                "414000 to 454000 and a 900 MHz one 895000 to 935000; what you may actually "
                "use inside that is set by your national regulator.",
    },
    {
        "name": "MAX_FREQ", "label": "Maximum frequency",
        "kind": "number", "unit": "kHz", "min": 240000, "max": 1000000, "step": 1000,
        "advanced": True,
        "hint": "The top of the hopping band. Must be above the minimum, and the span "
                "between them is divided into the hopping channels.",
    },
    {
        "name": "NUM_CHANNELS", "label": "Hopping channels",
        "kind": "number", "min": 1, "max": 50, "step": 1, "advanced": True,
        "hint": "How many frequencies the band is split into. More than one is what "
                "makes the radio frequency-agile, which several regulators require "
                "alongside listen-before-talk.",
    },
    {
        "name": "DUTY_CYCLE", "label": "Duty cycle",
        "kind": "number", "unit": "%", "min": 0, "max": 100, "step": 1, "advanced": True,
        "hint": "The share of the time this radio is allowed to transmit. 100 unless "
                "your regulator trades duty cycle for band or power. Europe's 433 MHz "
                "allowance below 10% is the usual reason. Zero makes the radio "
                "receive-only.",
    },
    {
        "name": "LBT_RSSI", "label": "Listen before talk",
        "kind": "number", "min": 0, "max": 255, "step": 1, "advanced": True,
        "hint": "Wait for the channel to be quiet before transmitting. 0 disables it; "
                "the lowest working threshold is 25, and each step above that is about "
                "0.5 dB over the receiver's noise floor.",
    },
    {
        "name": "MANCHESTER", "label": "Manchester encoding",
        "kind": "enum", "options": _OFF_ON, "advanced": True,
        "hint": "Alternative line coding. Off unless something specific requires it.",
    },
    {
        "name": "RTSCTS", "label": "Hardware flow control",
        "kind": "enum", "options": _RTSCTS_MODES, "advanced": True,
        "hint": "RTS/CTS on the serial wire. Worth enabling on a Pixhawk TELEM port, "
                "which carries the two extra pins; a radio on a USB cable does not.",
    },
    {
        "name": "MAX_WINDOW", "label": "Maximum transmit window",
        "kind": "number", "unit": "ms", "min": 20, "max": 400, "step": 1,
        "advanced": True,
        "hint": "The longest a radio may hold the air before handing over. 131 is the "
                "default and gives the most bandwidth; 33 bounds how long a command "
                "from the ground can wait, which is what low-latency joystick control "
                "needs.",
    },
    {
        "name": "ENCRYPTION_LEVEL", "label": "Encryption",
        "kind": "enum", "options": _OFF_ON, "advanced": True,
        "hint": "AES link encryption on the firmware builds that carry it. Both ends "
                "need the same level and the same key; the key is set with AT&E and is "
                "not readable back.",
    },
]

_BY_NAME: dict[str, dict[str, Any]] = {r["name"]: r for r in REGISTERS}

# ``S1: SERIAL_SPEED=57``, and the variants different firmware builds print:
# no space after the colon, no colon at all, trailing carriage return.
_ATI5_RE = re.compile(r"^\s*S(\d+)\s*[:=]?\s*([A-Za-z0-9_]+)\s*=\s*(-?\d+)\s*$")

# ``ATI7`` — the link-quality report. Both radios' signal and noise in one line:
#   L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20 txe=0 rxe=0 ...
_RSSI_RE = re.compile(
    r"L/R\s+RSSI:\s*(\d+)\s*/\s*(\d+).*?L/R\s+noise:\s*(\d+)\s*/\s*(\d+)",
    re.IGNORECASE | re.DOTALL,
)


class SikConfigError(ValueError):
    """A requested write is not one this radio will accept.

    Raised before anything is sent, and carries the operator-facing reason: the
    HTTP layer turns it straight into a 400 rather than letting a bad value
    reach a radio that would round it, refuse it, or take it and stop talking.
    """


# ---------------------------------------------------------------------------
# The "one byte form"
# ---------------------------------------------------------------------------

def baud_from_register(value: int) -> int:
    """Turn a SERIAL_SPEED / AIR_SPEED register into an actual rate.

    The radio stores it in the same compressed form ArduPilot uses for its own
    ``SERIALn_BAUD``: above 2000 the number *is* the baud rate, and below it the
    number is the rate with its trailing digits truncated. Truncated, not
    divided — 57 is 57600 — so the small numbers come from
    :data:`SERIAL_SPEEDS` rather than from a multiplication, and a value that is
    in neither falls back to kbps, which is the reading that at least keeps the
    magnitude right.
    """
    number = int(value)
    if number > 2000:
        return number
    return _BAUD_BY_REGISTER.get(number, number * 1000)


def register_from_baud(baud: int) -> int:
    """The inverse of :func:`baud_from_register`, for a real baud rate."""
    number = int(baud)
    if number in _REGISTER_BY_BAUD:
        return _REGISTER_BY_BAUD[number]
    return number // 1000 if number >= 2000 else number


def rssi_to_dbm(rssi: float) -> float:
    """Convert a raw SiK RSSI count to dBm.

    ``signal_dBm = (RSSI / 1.9) - 127``, the approximation the SiK documentation
    gives for the Si1000's RSSI curve. Good enough to answer the question an
    operator actually has — is this margin comfortable — and not good enough to
    quote in a regulatory filing.
    """
    return (float(rssi) / 1.9) - 127.0


# ---------------------------------------------------------------------------
# Parsing what the radio says
# ---------------------------------------------------------------------------

def parse_registers(text: str) -> dict[str, dict[str, Any]]:
    """Parse an ``ATI5`` / ``RTI5`` report into ``{name: {number, value}}``.

    Tolerant on purpose. The reply arrives mixed with the echo of the command
    that asked for it, with whatever the other radio was mid-sentence about when
    the escape landed, and with line endings that differ between firmware
    builds. Anything that does not look like a register line is simply not one.
    """
    out: dict[str, dict[str, Any]] = {}
    for line in str(text or "").replace("\r", "\n").split("\n"):
        match = _ATI5_RE.match(line)
        if match is None:
            continue
        number, name, value = match.group(1), match.group(2).upper(), match.group(3)
        try:
            out[name] = {"number": int(number), "value": int(value)}
        except ValueError:  # pragma: no cover - the regex already constrains both
            continue
    return out


def parse_rssi(text: str) -> dict[str, Any] | None:
    """Parse an ``ATI7`` link report into local/remote signal and noise.

    Returns ``None`` when the text carries no report — an older firmware, or a
    radio that has not yet found its partner. Both raw counts and their dBm
    equivalents come back, plus the fade margin, because the margin is the
    number that answers "will this reach": the SiK guidance is that range
    doubles for every 6 dB of it.
    """
    match = _RSSI_RE.search(str(text or ""))
    if match is None:
        return None
    local_rssi, remote_rssi, local_noise, remote_noise = (
        int(match.group(i)) for i in (1, 2, 3, 4)
    )
    return {
        "local_rssi": local_rssi,
        "remote_rssi": remote_rssi,
        "local_noise": local_noise,
        "remote_noise": remote_noise,
        "local_dbm": round(rssi_to_dbm(local_rssi), 1),
        "remote_dbm": round(rssi_to_dbm(remote_rssi), 1),
        # (signal - noise) / 2 is the SiK rule of thumb for fade margin in dB.
        "local_margin_db": round((local_rssi - local_noise) / 2.0, 1),
        "remote_margin_db": round((remote_rssi - remote_noise) / 2.0, 1),
    }


def parse_version(text: str) -> str:
    """Pull the firmware banner out of an ``ATI`` reply.

    The banner is the line that names the firmware — ``SiK 2.0 on HM-TRP``,
    ``RFD SiK 2.7 on RFD900P``. Echoed commands and bare ``OK`` acknowledgements
    are not it.
    """
    for line in str(text or "").replace("\r", "\n").split("\n"):
        candidate = line.strip()
        if not candidate or candidate.upper() in {"OK", "ERROR"}:
            continue
        if candidate.upper().startswith(("AT", "RT")):
            continue
        return candidate
    return ""


# ---------------------------------------------------------------------------
# Describing a radio for the page
# ---------------------------------------------------------------------------

def _option_values(field: dict[str, Any]) -> list[int]:
    return [int(o["value"]) for o in field.get("options", [])]


def describe_radio(registers: dict[str, dict[str, Any]],
                   version: str = "",
                   board: str = "") -> dict[str, Any]:
    """Turn parsed registers into the field list for one end of the link.

    Every register the radio reported becomes a field, in the table's order
    first and then whatever the radio had that the table does not know about.
    An enum whose current value is not in its option list gets that value
    appended as its own option, for the same reason
    :mod:`corvus.safety_config` does it: snapping an unrecognised setting to the
    first option would rewrite a working radio the moment the operator touched
    an unrelated field.
    """
    fields: list[dict[str, Any]] = []
    seen: set[str] = set()

    for spec in REGISTERS:
        name = spec["name"]
        entry = registers.get(name)
        if entry is None:
            continue
        seen.add(name)
        fields.append(_field(spec, name, entry))

    for name, entry in sorted(registers.items(), key=lambda kv: kv[1]["number"]):
        if name in seen:
            continue
        # A register this build has never heard of: render it as a plain number
        # under the radio's own name rather than dropping it. The operator can
        # still read it, and a firmware that grew a setting does not become
        # invisible until Corvus is updated.
        fields.append(_field(
            {"name": name, "label": name.replace("_", " ").title(),
             "kind": "number", "advanced": True,
             "hint": "Reported by this radio's firmware; not described by Corvus."},
            name, entry,
        ))

    return {
        "version": version,
        "board": board,
        "fields": fields,
        "values": {name: entry["value"] for name, entry in registers.items()},
    }


def _field(spec: dict[str, Any], name: str, entry: dict[str, Any]) -> dict[str, Any]:
    value = int(entry["value"])
    field: dict[str, Any] = {
        "name": name,
        "register": int(entry["number"]),
        "label": spec.get("label", name),
        "kind": spec.get("kind", "number"),
        "value": value,
        "advanced": bool(spec.get("advanced")),
        "must_match": name in MUST_MATCH,
        "read_only": name in READ_ONLY or spec.get("kind") == "readonly",
    }
    for key in ("unit", "min", "max", "step", "hint"):
        if key in spec:
            field[key] = spec[key]
    if spec.get("kind") == "enum":
        options = list(spec.get("options", []))
        if value not in _option_values({"options": options}):
            options = options + [{"value": value, "label": f"Unknown ({value})"}]
        field["options"] = options
    return field


def compare(local: dict[str, Any], remote: dict[str, Any] | None) -> list[dict[str, Any]]:
    """Registers that must match at both ends and do not.

    This is the whole reason the page reads both radios rather than just the one
    on the cable. A link that is up is proof the pair currently agrees; a link
    that is down gives the operator no way to tell *which* of eight settings
    drifted, and changing the wrong one on the reachable radio makes it worse.

    Returns an empty list when there is no remote to compare against — an
    unreachable partner is reported by the caller as its own condition, not as
    eight simultaneous mismatches.
    """
    if not remote:
        return []
    local_values = local.get("values") or {}
    remote_values = remote.get("values") or {}
    out: list[dict[str, Any]] = []
    for spec in REGISTERS:
        name = spec["name"]
        if name not in MUST_MATCH:
            continue
        if name not in local_values or name not in remote_values:
            continue
        if int(local_values[name]) == int(remote_values[name]):
            continue
        out.append({
            "name": name,
            "label": spec.get("label", name),
            "local": int(local_values[name]),
            "remote": int(remote_values[name]),
        })
    return out


def copy_to_remote(local: dict[str, Any], remote: dict[str, Any] | None) -> dict[str, int]:
    """The register values that would make the remote radio agree with the local one.

    Only :data:`MUST_MATCH` registers, and only those the remote actually has —
    the same set Mission Planner's "copy required items to remote" copies.
    Everything else is deliberately left alone: transmit power and flow control
    are properties of where each radio is installed, not of the link, and an
    aircraft radio on a Pixhawk TELEM port has good reason to differ from the
    one on the USB cable.
    """
    if not remote:
        return {}
    local_values = local.get("values") or {}
    remote_values = remote.get("values") or {}
    return {
        name: int(local_values[name])
        for name in sorted(MUST_MATCH)
        if name in local_values and name in remote_values
        and int(local_values[name]) != int(remote_values[name])
    }


# ---------------------------------------------------------------------------
# Validating a write
# ---------------------------------------------------------------------------

def validate_writes(settings: dict[str, Any],
                    present: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    """Check requested values against the radio in front of us, ordered for writing.

    *present* is what the radio just reported, so this rejects a write to a
    register this firmware does not have rather than sending ``ATS16=`` to a
    radio whose registers stop at 15 and reading back an ``ERROR`` the operator
    cannot interpret.

    Returns ``[{name, register, value}]`` with unchanged registers dropped — a
    save that would rewrite every register to what it already holds costs a
    reboot for nothing. Raises :class:`SikConfigError` on the first problem,
    naming the setting, because a partly-validated batch is not worth sending:
    the radio commits the ones it accepted and reboots into a configuration
    nobody asked for.
    """
    if not isinstance(settings, dict):
        raise SikConfigError("settings must be an object")

    writes: list[dict[str, Any]] = []
    for name, raw in settings.items():
        key = str(name).upper()
        entry = present.get(key)
        if entry is None:
            raise SikConfigError(f"this radio has no {key} setting")
        if key in READ_ONLY:
            raise SikConfigError(f"{key} is set by the radio firmware and cannot be written")

        if isinstance(raw, bool) or not isinstance(raw, (int, float)):
            raise SikConfigError(f"{key} must be a number")
        value = int(raw)
        if value != raw:
            raise SikConfigError(f"{key} must be a whole number")

        spec = _BY_NAME.get(key)
        if spec is not None:
            _check_against_spec(spec, key, value)

        if value != int(entry["value"]):
            writes.append({"name": key, "register": int(entry["number"]), "value": value})

    _check_band(settings, present)
    # Ascending register order so a log of the session reads the way ATI5 does.
    writes.sort(key=lambda w: w["register"])
    return writes


def _check_against_spec(spec: dict[str, Any], key: str, value: int) -> None:
    kind = spec.get("kind")
    if kind == "enum":
        allowed = _option_values(spec)
        if value not in allowed:
            # Named rather than listed when the list is long: an operator who
            # typed 100 for the air rate needs to know 96 and 128 exist, not to
            # read all thirteen back.
            raise SikConfigError(
                f"{spec.get('label', key)} does not accept {value}; "
                f"allowed values are {', '.join(str(v) for v in allowed)}"
            )
        return
    low, high = spec.get("min"), spec.get("max")
    if low is not None and value < low:
        raise SikConfigError(f"{spec.get('label', key)} must be at least {low}")
    if high is not None and value > high:
        raise SikConfigError(f"{spec.get('label', key)} must be at most {high}")


def _check_band(settings: dict[str, Any], present: dict[str, dict[str, Any]]) -> None:
    """Refuse a frequency band that is inverted or empty.

    Checked across the pair rather than per field because either half may be
    absent from the request: an operator lowering only the maximum is editing
    the band just as surely as one who sends both. A radio given max <= min
    stops transmitting entirely and has to be recovered over a cable.
    """
    def resolve(name: str) -> int | None:
        if name in settings:
            try:
                return int(settings[name])
            except (TypeError, ValueError):
                return None
        entry = present.get(name)
        return None if entry is None else int(entry["value"])

    low, high = resolve("MIN_FREQ"), resolve("MAX_FREQ")
    if low is None or high is None:
        return
    if high <= low:
        raise SikConfigError(
            "the maximum frequency must be above the minimum frequency"
        )


def write_plan(writes: list[dict[str, Any]], remote: bool) -> list[str]:
    """The AT command sequence that commits *writes*, in the order it must run.

    ``ATSn=X`` for each register, then ``AT&W`` to move them from working memory
    into EEPROM, then ``ATZ`` to reboot into them — with ``RT`` in place of
    ``AT`` for the far radio. Every step matters and the order is not free:
    without the write the settings evaporate at the next power cycle, and
    without the reboot most of them are not in effect at all (transmit power is
    the exception, which takes hold immediately and then reverts unless it was
    written).
    """
    prefix = "RT" if remote else "AT"
    commands = [f"{prefix}S{w['register']}={w['value']}" for w in writes]
    if not commands:
        return []
    commands.append(f"{prefix}&W")
    commands.append(f"{prefix}Z")
    return commands


def schema() -> dict[str, Any]:
    """The static half of the page description: the table, without a radio.

    Served so the frontend can render its form, its option lists and its help
    text before any radio has been read — and so the labels and hints stay in
    one place rather than being duplicated in JavaScript.
    """
    return {
        "registers": [
            {
                "name": spec["name"],
                "label": spec.get("label", spec["name"]),
                "kind": spec.get("kind", "number"),
                "advanced": bool(spec.get("advanced")),
                "must_match": spec["name"] in MUST_MATCH,
                "read_only": spec["name"] in READ_ONLY or spec.get("kind") == "readonly",
                **{k: spec[k] for k in ("unit", "min", "max", "step", "hint", "options")
                   if k in spec},
            }
            for spec in REGISTERS
        ],
        "must_match": sorted(MUST_MATCH),
        "bauds": list(BAUD_CANDIDATES),
        "default_baud": DEFAULT_BAUD,
    }
