"""ExpressLRS telemetry (corvus/elrs.py), which is code only and not wired.

The last block is the one that matters for now: it fails as soon as anything
in the app imports the module or flips its switch, so wiring it in is a
decision someone makes on purpose, together with its route, its shutdown
hook and its control.
"""
from __future__ import annotations

import math
import re
import struct
import threading
from pathlib import Path

import pytest

from corvus import elrs
from corvus.elrs import (
    ElrsTelemetry,
    ElrsTelemetryReader,
    CrsfParser,
    build_frame,
    crc8_dvb_s2,
    plan_fly_to,
)

ROOT = Path(__file__).resolve().parent.parent


def _gps(lat: float, lon: float, *, kmh: float = 36.0, course: float = 90.0,
         alt: int = 520, sats: int = 14) -> bytes:
    payload = struct.pack(">iiHHHB", int(round(lat * 1e7)), int(round(lon * 1e7)),
                          int(kmh * 10), int(course * 100), alt + 1000, sats)
    return build_frame(elrs.TYPE_GPS, payload)


def _battery(volts: float, amps: float, mah: int, pct: int) -> bytes:
    payload = struct.pack(">HH", int(volts * 10), int(amps * 10)) + mah.to_bytes(3, "big") + bytes([pct])
    return build_frame(elrs.TYPE_BATTERY, payload)


class _Clock:
    def __init__(self) -> None:
        self.now = 100.0

    def __call__(self) -> float:
        return self.now


# --- wire format ----------------------------------------------------------

def test_crc8_matches_the_dvb_s2_check_value():
    assert crc8_dvb_s2(b"123456789") == 0xBC


def test_parser_reads_a_frame_split_across_reads():
    frame = _gps(48.08, 11.64)
    parser = CrsfParser()
    assert parser.feed(frame[:5]) == []
    out = parser.feed(frame[5:])
    assert len(out) == 1 and out[0].type == elrs.TYPE_GPS


def test_parser_resyncs_past_noise_and_a_corrupt_frame():
    good = _gps(48.08, 11.64)
    bad = bytearray(_battery(16.0, 5.0, 300, 80))
    bad[-1] ^= 0xFF
    parser = CrsfParser()
    out = parser.feed(b"\x01\x02\xff" + bytes(bad) + good)
    assert [f.type for f in out] == [elrs.TYPE_GPS]
    assert parser.crc_errors >= 1


def test_build_frame_refuses_an_oversized_payload():
    with pytest.raises(ValueError):
        build_frame(elrs.TYPE_FLIGHT_MODE, b"x" * 62)


# --- decoding into the snapshot -------------------------------------------

def test_gps_frame_becomes_a_position_in_store_units():
    clock = _Clock()
    tel = ElrsTelemetry(clock)
    tel.feed(_gps(48.0801234, 11.6401234, kmh=36.0, course=271.5, alt=520, sats=12))
    snap = tel.snapshot()
    assert snap["position"] == pytest.approx([48.0801234, 11.6401234])
    assert snap["groundspeed"] == pytest.approx(10.0)
    assert snap["heading"] == pytest.approx(271.5)
    assert snap["altitude_amsl"] == pytest.approx(520.0)
    assert snap["satellites"] == 12
    assert snap["source"] == "elrs"
    assert snap["stale"] is False


def test_gps_without_a_fix_keeps_the_last_position():
    tel = ElrsTelemetry(_Clock())
    tel.feed(_gps(48.08, 11.64))
    tel.feed(_gps(0.0, 0.0, sats=0))
    snap = tel.snapshot()
    assert snap["position"] == pytest.approx([48.08, 11.64])
    assert snap["satellites"] == 0


def test_position_goes_stale_when_the_link_stops():
    clock = _Clock()
    tel = ElrsTelemetry(clock)
    assert tel.snapshot()["stale"] is True
    tel.feed(_gps(48.08, 11.64))
    clock.now += elrs.STALE_S + 0.1
    assert tel.snapshot()["stale"] is True


def test_battery_attitude_mode_baro_vario_and_link():
    tel = ElrsTelemetry(_Clock())
    attitude = struct.pack(">hhh", int(0.1 * 10000), int(-0.2 * 10000), int(-math.pi / 2 * 10000))
    link = struct.pack(">BBBbBBBBBb", 70, 75, 100, 9, 0, 6, 3, 80, 98, 5)
    tel.feed(
        _battery(16.4, 12.3, 1234, 67)
        + build_frame(elrs.TYPE_ATTITUDE, attitude)
        + build_frame(elrs.TYPE_FLIGHT_MODE, b"POSCTL\x00")
        + build_frame(elrs.TYPE_BARO_ALTITUDE, struct.pack(">H", 10000 + 253))
        + build_frame(elrs.TYPE_VARIO, struct.pack(">h", -150))
        + build_frame(elrs.TYPE_LINK_STATISTICS, link)
    )
    snap = tel.snapshot()
    assert snap["battery_voltage"] == pytest.approx(16.4)
    assert snap["battery_current"] == pytest.approx(12.3)
    assert snap["battery_used_mah"] == 1234
    assert snap["battery_percent"] == 67
    assert snap["pitch"] == pytest.approx(math.degrees(0.1), abs=0.01)
    assert snap["roll"] == pytest.approx(math.degrees(-0.2), abs=0.01)
    assert snap["yaw"] == pytest.approx(270.0, abs=0.01)
    assert snap["mode_label"] == "POSCTL"
    assert snap["altitude_rel"] == pytest.approx(25.3)
    assert snap["vspeed"] == pytest.approx(-1.5)
    assert snap["link"]["uplink_rssi_1"] == -70
    assert snap["link"]["uplink_lq"] == 100
    assert snap["link"]["downlink_snr"] == 5


def test_baro_altitude_in_whole_metres_above_the_decimetre_range():
    assert elrs.decode_baro_altitude(struct.pack(">H", 0x8000 | 3000)) == {"altitude_rel": 3000.0}


def test_short_payloads_are_ignored_not_raised():
    tel = ElrsTelemetry(_Clock())
    for frame_type in elrs.DECODERS:
        tel.feed(build_frame(frame_type, b"\x01"))
    assert tel.snapshot()["position"] is None


# --- reader lifecycle -----------------------------------------------------

class _FakeSerial:
    def __init__(self, data: bytes) -> None:
        self._chunks = [data[i:i + 7] for i in range(0, len(data), 7)]
        self.closed = threading.Event()
        self.drained = threading.Event()

    def read(self, size: int = 1) -> bytes:
        if self.closed.is_set():
            raise OSError("closed")
        if self._chunks:
            return self._chunks.pop(0)
        self.drained.set()
        self.closed.wait(0.02)
        return b""

    def close(self) -> None:
        self.closed.set()


def test_reader_refuses_to_start_while_disabled():
    reader = ElrsTelemetryReader(lambda: pytest.fail("source must not be opened"))
    with pytest.raises(RuntimeError):
        reader.start()
    assert not reader.running


def test_reader_reads_and_stops_cleanly(monkeypatch):
    monkeypatch.setattr(elrs, "ENABLED", True)
    port = _FakeSerial(_gps(48.08, 11.64) + _battery(16.0, 1.0, 10, 90))
    updates: list[dict] = []
    reader = ElrsTelemetryReader(lambda: port, on_update=updates.append)
    reader.start()
    assert port.drained.wait(2.0)
    reader.stop()
    assert not reader.running
    assert port.closed.is_set()
    assert updates and updates[-1]["battery_percent"] == 90
    assert reader.telemetry.snapshot()["position"] == pytest.approx([48.08, 11.64])
    reader.stop()
    assert not any(t.name == "corvus-elrs" for t in threading.enumerate())


# --- fly to this point ----------------------------------------------------

def test_fly_to_is_not_available_over_crsf():
    plan = plan_fly_to("crsf", "px4", 2, 48.0, 11.0, 30.0, home_alt_amsl=500.0)
    assert not plan.available
    assert "MAVLink mode" in plan.reason


def test_fly_to_over_mavlink_mode_follows_the_dialect():
    px4 = plan_fly_to("mavlink", "px4", 2, 48.08, 11.64, 30.0, home_alt_amsl=500.0)
    assert px4.available and px4.command == elrs.MAV_CMD_DO_REPOSITION
    assert px4.frame == elrs.MAV_FRAME_GLOBAL and px4.z == pytest.approx(530.0)
    assert (px4.x, px4.y) == (480800000, 116400000)

    ap = plan_fly_to("mavlink", "ardupilot", 2, 48.08, 11.64, 30.0)
    assert ap.available and ap.frame == elrs.MAV_FRAME_GLOBAL_RELATIVE_ALT
    assert ap.z == pytest.approx(30.0)


def test_fly_to_on_px4_needs_a_home_altitude():
    plan = plan_fly_to("mavlink", "px4", 2, 48.08, 11.64, 30.0)
    assert not plan.available and "home altitude" in plan.reason


@pytest.mark.parametrize("lat, lon, alt", [
    (True, 11.0, 30.0), ("x", 11.0, 30.0), (float("nan"), 11.0, 30.0), (91.0, 11.0, 30.0),
])
def test_fly_to_rejects_bad_points(lat, lon, alt):
    assert not plan_fly_to("mavlink", "ardupilot", 2, lat, lon, alt).available


def test_fly_to_reasons_have_no_dashes():
    for plan in (plan_fly_to("crsf", "px4", 2, 0, 0, 0), plan_fly_to("mavlink", "px4", 2, 0, 0, 0),
                 plan_fly_to("bogus", "px4", 2, 0, 0, 0)):
        assert not re.search(r"[–—]| - ", plan.reason)


# --- dormant on purpose ---------------------------------------------------

def test_switch_is_off():
    assert elrs.ENABLED is False


def test_nothing_in_the_app_uses_the_module():
    pattern = re.compile(r"\belrs\b|corvus\.elrs|from \. import elrs|/api/elrs", re.IGNORECASE)
    users = []
    for path in [*ROOT.joinpath("corvus").rglob("*.py"), ROOT / "serve.py",
                 *ROOT.joinpath("src", "js").rglob("*.js"), ROOT / "src" / "index.html"]:
        if path.name == "elrs.py" or not path.exists():
            continue
        if pattern.search(path.read_text(encoding="utf-8", errors="replace")):
            users.append(str(path.relative_to(ROOT)))
    assert users == [], f"corvus/elrs.py is dormant but is referenced from {users}"
