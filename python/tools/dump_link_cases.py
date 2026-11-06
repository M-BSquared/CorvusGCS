#!/usr/bin/env python3
"""Dump pymavlink's frames into fixtures for the plain Rust MAVLink layer.

    python tools/dump_link_cases.py [--check]

Writes ``src/link/tests/data/plain_frames.json``: for a set of messages, the
field values, and the exact bytes pymavlink frames them as (MAVLink 1 and 2,
with trailing zeros trimmed in 2, and signed). The Rust tests of
``corvus_link::plain`` hold the codec, the framing and the signing to it.
"""
from __future__ import annotations

import hashlib
import json
import pathlib
import sys

from pymavlink.dialects.v10 import all as v10
from pymavlink.dialects.v20 import all as v20

OUT = pathlib.Path(__file__).resolve().parent.parent.parent / "src" / "link" / "tests" / "data" / "plain_frames.json"

MESSAGES = ["HEARTBEAT", "SYS_STATUS", "ATTITUDE", "GLOBAL_POSITION_INT", "PARAM_VALUE", "STATUSTEXT",
            "COMMAND_LONG", "MISSION_ITEM_INT", "BATTERY_STATUS", "OPEN_DRONE_ID_BASIC_ID", "RC_CHANNELS",
            "SERIAL_CONTROL", "COMMAND_ACK", "SYSTEM_TIME", "HOME_POSITION", "AUTOPILOT_VERSION"]


def values_for(cls: type, seed: int, zero: bool = False) -> list:
    """One value per declared field, deterministic, exact in the field's type."""
    out = []
    lengths = dict(zip(cls.ordered_fieldnames, cls.array_lengths))
    for i, (name, ftype) in enumerate(zip(cls.fieldnames, cls.fieldtypes)):
        alen = lengths[name]
        base = ftype.split("[")[0]
        k = seed + i
        if base == "char":
            out.append(b"" if zero else ("name" + str(k)).encode()[:max(alen - 1, 1)])
            continue
        if zero:
            one = 0.0 if base in ("float", "double") else 0
        elif base in ("float", "double"):
            one = 0.25 * (k % 40 + 1)
        elif base.startswith("uint8"):
            one = k % 200 + 1
        elif base.startswith("int8"):
            one = -(k % 100) - 1
        elif base.startswith("uint16"):
            one = (k * 257) % 60000 + 1
        elif base.startswith("int16"):
            one = -((k * 131) % 30000) - 1
        elif base.startswith("uint32"):
            one = (k * 65537) % 4000000000 + 1
        elif base.startswith("int32"):
            one = -((k * 100003) % 2000000000) - 1
        elif base.startswith("uint64"):
            one = (k * 4294967311) % (1 << 62) + 1
        else:
            one = -((k * 4294967311) % (1 << 62)) - 1
        out.append([one + (j if not zero else 0) if base not in ("float", "double") or not zero else 0 for j in range(alen)]
                   if alen else one)
    return out


def jsonable(names: list[str], vals: list) -> dict:
    return {n: (v.decode() if isinstance(v, bytes) else v) for n, v in zip(names, vals)}


def hexs(b: bytes) -> str:
    return b.hex()


def cases() -> dict:
    out: list[dict] = []
    for dialect, proto in ((v10, 1), (v20, 2)):
        mav = dialect.MAVLink(None, srcSystem=7, srcComponent=9)
        mav.robust_parsing = True
        for name in MESSAGES:
            cls = getattr(dialect, f"MAVLink_{name.lower()}_message", None)
            if cls is None:
                continue
            for variant, (seed, zero) in {"values": (3, False), "zeros": (0, True)}.items():
                vals = values_for(cls, seed, zero)
                msg = cls(*vals)
                mav.seq = 41
                frame = msg.pack(mav)
                out.append({
                    "message": name, "proto": proto, "variant": variant, "sysid": 7, "compid": 9, "seq": 41,
                    "fields": jsonable(cls.fieldnames, vals), "frame": hexs(bytes(frame)),
                })
    # signed MAVLink 2
    key = hashlib.sha256(b"corvus test secret").digest()
    mav = v20.MAVLink(None, srcSystem=7, srcComponent=9)
    mav.signing.secret_key = key
    mav.signing.link_id = 3
    mav.signing.timestamp = 123456789012
    mav.signing.sign_outgoing = True
    cls = v20.MAVLink_attitude_message
    vals = values_for(cls, 5)
    mav.seq = 5
    signed = bytes(cls(*vals).pack(mav))
    return {
        "frames": out,
        "signed": {"message": "ATTITUDE", "fields": jsonable(cls.fieldnames, vals), "frame": hexs(signed),
                   "secret_key": hexs(key), "link_id": 3, "timestamp": 123456789012, "sysid": 7, "compid": 9, "seq": 5},
    }


def connection_errors() -> dict:
    """What ``mavlink_connection`` says about strings that are not links."""
    from corvus_link import mavutil

    bad = ["udpin:1.2.3.4", "tcp:host:x", "udpout:1.2.3.4:70000", "tcp:1:2:3", "uds:", "/dev/ttyUSB0,fast",
           "udpin:1.2.3.4:x:y", "tcpin:1.2.3.4", "udpbcast:a", "mcast:239.1.2.3:x", "udp:a:b:c", "udp:host:-1"]
    out = {}
    for text in bad:
        try:
            mavutil.mavlink_connection(text).close()
            out[text] = None
        except Exception as exc:  # noqa: BLE001 - the message is the fixture
            out[text] = f"{type(exc).__name__}: {exc}"
    return out


def main(argv: list[str]) -> int:
    data = cases()
    data["connection_errors"] = connection_errors()
    text = json.dumps(data, indent=1) + "\n"
    if "--check" in argv:
        if not OUT.is_file() or OUT.read_text() != text:
            print("stale: run tools/dump_link_cases.py", file=sys.stderr)
            return 1
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
