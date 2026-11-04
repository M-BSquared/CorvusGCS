"""Integer parameter encoding, fresh read-back, and the "Check values" verify.

PX4 copies an integer parameter's four bytes into PARAM_VALUE's float field;
ArduPilot casts the number. Corvus used to read and write every stack the
ArduPilot way, so on PX4 each integer parameter read as 0 and a write of
BAT1_N_CELLS = 4 stored 1082130432 while the echo still said "4.0". These tests
pin the codec, the bridge's use of it, and the verify path built on top.
"""
from __future__ import annotations

import os
import struct
import time
from types import SimpleNamespace

import pytest

os.environ.setdefault("MAVLINK20", "1")
from pymavlink import mavutil  # noqa: E402
from pymavlink.dialects.v20 import common as mavlink2  # noqa: E402

from corvus import autopilot  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402

INT32 = mavutil.mavlink.MAV_PARAM_TYPE_INT32
REAL32 = mavutil.mavlink.MAV_PARAM_TYPE_REAL32
BYTEWISE = autopilot.PARAM_ENCODING_BYTEWISE
C_CAST = autopilot.PARAM_ENCODING_C_CAST


def as_px4_float(value: int) -> float:
    """The float PX4 puts on the wire for the int32 *value*."""
    return struct.unpack("<f", struct.pack("<i", value))[0]


# ---------------------------------------------------------------------------
# The codec
# ---------------------------------------------------------------------------

def test_a_px4_integer_is_read_from_its_bytes_not_its_float_value() -> None:
    assert autopilot.decode_param_value(as_px4_float(4), INT32, BYTEWISE) == 4.0
    assert autopilot.decode_param_value(as_px4_float(1001), INT32, BYTEWISE) == 1001.0


def test_an_integer_whose_bytes_are_a_nan_is_read_from_the_wire_bytes() -> None:
    # Every negative int32 is a NaN as a float; COM_FLTMODE1 = -1 is PX4's
    # stock "unassigned". -5000000 is one a Python float cannot carry intact.
    raw = struct.pack("<i", -5000000)
    assert autopilot.decode_param_value(float("nan"), INT32, BYTEWISE, raw=raw) == -5000000.0
    assert autopilot.decode_param_value(as_px4_float(-1), INT32, BYTEWISE) == -1.0


def test_ardupilot_integers_and_every_float_pass_through_unchanged() -> None:
    assert autopilot.decode_param_value(4.0, INT32, C_CAST) == 4.0
    assert autopilot.decode_param_value(0.15, REAL32, BYTEWISE) == pytest.approx(0.15)


def test_writing_a_px4_integer_packs_its_bytes() -> None:
    wire = autopilot.encode_param_value(4, INT32, BYTEWISE)
    assert struct.unpack("<i", struct.pack("<f", wire))[0] == 4
    assert autopilot.encode_param_value(4, INT32, C_CAST) == 4.0
    assert autopilot.encode_param_value(0.15, REAL32, BYTEWISE) == 0.15


@pytest.mark.parametrize("value", [4.5, 2 ** 31, -(2 ** 31) - 1])
def test_an_integer_parameter_refuses_a_value_it_cannot_hold(value: float) -> None:
    with pytest.raises(ValueError):
        autopilot.encode_param_value(value, INT32, BYTEWISE)


def test_an_integer_that_pymavlink_would_change_on_the_way_out_is_refused() -> None:
    # 0x7F800001 is a signalling NaN; packing it through a Python float quietens it.
    with pytest.raises(ValueError, match="exactly"):
        autopilot.encode_param_value(0x7F800001, INT32, BYTEWISE)


def test_matching_is_exact_for_integers_and_float32_exact_for_floats() -> None:
    assert autopilot.param_values_match(INT32, 4.0, 4)
    assert not autopilot.param_values_match(INT32, 1082130432.0, 4)
    as_float32 = struct.unpack("<f", struct.pack("<f", 0.15))[0]
    assert autopilot.param_values_match(REAL32, as_float32, 0.15)
    assert not autopilot.param_values_match(REAL32, 0.1501, 0.15)


def test_each_stack_names_its_encoding() -> None:
    assert autopilot.dialect_for(12).param_encoding == BYTEWISE
    assert autopilot.dialect_for(3).param_encoding == C_CAST
    assert autopilot.dialect_for(0).param_encoding == C_CAST


# ---------------------------------------------------------------------------
# A vehicle on the far end of a fake link
# ---------------------------------------------------------------------------

class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return 1

    def get_srcComponent(self) -> int:
        return 1


class FakeVehicle:
    """Answers the parameter protocol the way the given stack does.

    ``stuck`` names parameters whose writes the vehicle acknowledges with its
    old value, the case a verify exists to catch.
    """

    def __init__(self, bridge: MavlinkBridge, params: dict[str, tuple[float, int]],
                 encoding: str = BYTEWISE) -> None:
        self.bridge = bridge
        self.params = dict(params)
        self.encoding = encoding
        self.stuck: set[str] = set()
        self.silent: set[str] = set()
        self.reads: list[str] = []
        self.sets: list[tuple[str, float, int]] = []

    def _wire(self, value: float, ptype: int) -> float:
        if ptype == INT32 and self.encoding == BYTEWISE:
            return as_px4_float(int(value))
        return float(value)

    def send(self, name: str) -> None:
        value, ptype = self.params[name]
        self.bridge._dispatch(FakeMessage(
            message_type="PARAM_VALUE", param_id=name.encode(),
            param_value=self._wire(value, ptype), param_type=ptype,
            param_index=0, param_count=0,
        ))

    # -- the mav surface the bridge calls --------------------------------
    def param_request_read_send(self, _sys: int, _comp: int, name: bytes, _index: int) -> None:
        pname = name.decode()
        self.reads.append(pname)
        if pname in self.params and pname not in self.silent:
            self.send(pname)

    def param_set_send(self, _sys: int, _comp: int, name: bytes, value: float, ptype: int) -> None:
        pname = name.decode()
        self.sets.append((pname, value, ptype))
        if pname not in self.stuck:
            if ptype == INT32 and self.encoding == BYTEWISE:
                stored = float(struct.unpack("<i", struct.pack("<f", value))[0])
            else:
                stored = float(value)
            self.params[pname] = (stored, ptype)
        self.send(pname)


def make_bridge(params: dict[str, tuple[float, int]], autopilot_id: int = 12,
                encoding: str = BYTEWISE) -> tuple[MavlinkBridge, FakeVehicle]:
    store = VehicleStateStore()
    store.update(connected=True)
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._latch_dialect(autopilot_id, mavutil.mavlink.MAV_TYPE_QUADROTOR)
    vehicle = FakeVehicle(bridge, params, encoding)
    bridge._conn = SimpleNamespace(mav=vehicle, source_system=254, source_component=190)
    return bridge, vehicle


@pytest.fixture(autouse=True)
def _fast_clock(monkeypatch: pytest.MonkeyPatch) -> None:
    """Let waits that nothing will satisfy expire at once."""
    clock = [1000.0]
    monkeypatch.setattr(time, "monotonic", lambda: clock[0])
    monkeypatch.setattr(time, "sleep", lambda s: clock.__setitem__(0, clock[0] + max(s, 0.01)))


# ---------------------------------------------------------------------------
# The bridge reads and writes each stack its own way
# ---------------------------------------------------------------------------

def test_a_px4_integer_parameter_reads_as_its_real_value() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (4, INT32), "COM_FLTMODE1": (-1, INT32)})
    vehicle.send("BAT1_N_CELLS")
    vehicle.send("COM_FLTMODE1")
    assert bridge.get_param("BAT1_N_CELLS")["value"] == 4.0
    assert bridge.get_param("COM_FLTMODE1")["value"] == -1.0, \
        "a -1 travels as a NaN and must not be thrown away as one"


def test_a_real_frame_is_decoded_from_its_bytes() -> None:
    """-5000000 is a signalling NaN as a float, which a Python float quietens.

    pymavlink cannot even build this frame without changing it, so the bytes
    are written in by hand, the way PX4 puts them on the wire.
    """
    bridge, _vehicle = make_bridge({})
    mav = mavlink2.MAVLink(None, srcSystem=1, srcComponent=1)
    message = mav.param_value_encode(b"COM_FLTMODE2", 0.0, INT32, 10, 3)
    frame = bytearray(message.pack(mav))
    frame[10:14] = struct.pack("<i", -5000000)
    crc = mavutil.x25crc(bytes(frame[1:-2]))
    crc.accumulate(bytes([message.crc_extra]))
    frame[-2:] = struct.pack("<H", crc.crc)
    decoded = mavlink2.MAVLink(None).decode(frame)
    bridge._dispatch(decoded)
    assert bridge.get_param("COM_FLTMODE2")["value"] == -5000000.0


def test_ardupilot_integers_are_still_read_as_cast() -> None:
    bridge, vehicle = make_bridge({"FS_THR_ENABLE": (1, INT32)}, autopilot_id=3, encoding=C_CAST)
    vehicle.send("FS_THR_ENABLE")
    assert bridge.get_param("FS_THR_ENABLE")["value"] == 1.0


def test_autopilot_version_capabilities_override_the_dialect_default() -> None:
    bridge, _vehicle = make_bridge({}, autopilot_id=0)
    assert bridge._param_encoding() == C_CAST
    bridge._dispatch(FakeMessage(
        message_type="AUTOPILOT_VERSION", flight_sw_version=0, vendor_id=0,
        product_id=0, capabilities=16, flight_custom_version=b""))
    assert bridge._param_encoding() == BYTEWISE


def test_writing_a_px4_integer_stores_the_number_on_the_vehicle() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (6, INT32)})
    vehicle.send("BAT1_N_CELLS")
    assert bridge.set_param("BAT1_N_CELLS", 4) is True
    assert vehicle.params["BAT1_N_CELLS"] == (4.0, INT32), \
        "the vehicle must hold 4, not 1082130432"


def test_a_fraction_for_an_integer_parameter_never_leaves_the_station() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (6, INT32)})
    vehicle.send("BAT1_N_CELLS")
    assert bridge.set_param("BAT1_N_CELLS", 4.5) is False
    assert "whole numbers" in bridge.get_last_command_error()
    assert vehicle.sets == []


def test_the_hash_check_pseudo_parameter_is_not_a_parameter() -> None:
    bridge, _vehicle = make_bridge({})
    bridge._param_download_state = "downloading"
    bridge._param_count = 2
    bridge._dispatch(FakeMessage(
        message_type="PARAM_VALUE", param_id=b"_HASH_CHECK", param_value=1.5,
        param_type=mavutil.mavlink.MAV_PARAM_TYPE_UINT32, param_index=-1, param_count=2))
    assert bridge.get_param("_HASH_CHECK") is None
    assert bridge.param_status()["received"] == 0


# ---------------------------------------------------------------------------
# Fresh reads
# ---------------------------------------------------------------------------

def test_a_fresh_read_asks_again_for_what_the_cache_already_holds() -> None:
    bridge, vehicle = make_bridge({"BAT1_V_EMPTY": (3.5, REAL32)})
    vehicle.send("BAT1_V_EMPTY")
    vehicle.params["BAT1_V_EMPTY"] = (3.6, REAL32)   # changed behind our back

    assert bridge.fetch_params(["BAT1_V_EMPTY"]) == {"BAT1_V_EMPTY": 3.5}
    assert vehicle.reads == [], "a plain read serves the cache"

    fresh = bridge.fetch_params(["BAT1_V_EMPTY"], fresh=True)
    assert fresh["BAT1_V_EMPTY"] == pytest.approx(3.6)
    assert vehicle.reads == ["BAT1_V_EMPTY"]


def test_a_fresh_read_does_not_count_a_cached_value_as_an_answer() -> None:
    bridge, vehicle = make_bridge({"BAT1_V_EMPTY": (3.5, REAL32)})
    vehicle.send("BAT1_V_EMPTY")
    vehicle.silent.add("BAT1_V_EMPTY")
    assert bridge.fetch_params(["BAT1_V_EMPTY"], timeout=0.5, fresh=True) == {}


# ---------------------------------------------------------------------------
# verify_params: the "Check values" button
# ---------------------------------------------------------------------------

def test_a_value_the_vehicle_holds_is_confirmed_without_writing() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (4, INT32), "BAT1_V_EMPTY": (3.5, REAL32)})
    result = bridge.verify_params(
        [{"name": "BAT1_N_CELLS", "value": 4}], names=["BAT1_V_EMPTY"])
    assert result["all_confirmed"] is True
    assert result["results"][0]["ok"] is True
    assert result["results"][0]["rewritten"] is False
    assert vehicle.sets == []
    assert result["values"] == {"BAT1_N_CELLS": 4.0, "BAT1_V_EMPTY": 3.5}


def test_a_value_that_did_not_stick_is_written_again_and_confirmed() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (6, INT32)})
    result = bridge.verify_params([{"name": "BAT1_N_CELLS", "value": 4}])
    row = result["results"][0]
    assert row["ok"] is True and row["rewritten"] is True
    assert row["before"] == 6.0 and row["after"] == 4.0
    assert vehicle.params["BAT1_N_CELLS"] == (4.0, INT32)
    assert result["values"]["BAT1_N_CELLS"] == 4.0


def test_a_value_the_vehicle_keeps_refusing_is_reported_not_confirmed() -> None:
    bridge, vehicle = make_bridge({"BAT1_V_EMPTY": (3.5, REAL32)})
    vehicle.stuck.add("BAT1_V_EMPTY")
    result = bridge.verify_params([{"name": "BAT1_V_EMPTY", "value": 3.6}])
    row = result["results"][0]
    assert row["ok"] is False and row["rewritten"] is True
    assert row["after"] == 3.5
    assert row["error"]
    assert result["all_confirmed"] is False
    assert len(vehicle.sets) == 2, "written again a bounded number of times"


def test_nothing_is_written_again_while_armed() -> None:
    bridge, vehicle = make_bridge({"BAT1_N_CELLS": (6, INT32)})
    bridge._store.update(armed=True)
    result = bridge.verify_params([{"name": "BAT1_N_CELLS", "value": 4}])
    assert result["results"][0]["ok"] is False
    assert "armed" in result["results"][0]["error"]
    assert vehicle.sets == []


def test_a_parameter_the_vehicle_does_not_answer_for_is_named() -> None:
    bridge, _vehicle = make_bridge({"BAT1_N_CELLS": (4, INT32)})
    result = bridge.verify_params([{"name": "BAT9_N_CELLS", "value": 4}], timeout=0.5)
    assert result["results"][0]["ok"] is False
    assert result["results"][0]["before"] is None
    assert result["missing"] == ["BAT9_N_CELLS"]


@pytest.mark.parametrize("targets", [
    "nope", [{"name": "", "value": 1}], [{"name": "A", "value": True}],
    [{"name": "A", "value": float("nan")}], [{"name": "X" * 17, "value": 1}],
])
def test_an_unusable_request_is_refused(targets: object) -> None:
    bridge, _vehicle = make_bridge({})
    assert bridge.verify_params(targets) is None  # type: ignore[arg-type]
    assert bridge.get_last_command_error()


def test_nothing_is_checked_without_a_link() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge.verify_params([{"name": "BAT1_N_CELLS", "value": 4}]) is None
    assert bridge.get_last_command_error() == "not connected"
