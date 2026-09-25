"""Parameter defaults from the vehicle: MAVLink FTP, the two metadata formats, the fetch.

The parameter protocol has no field for a default, so Corvus reads the file
each stack keeps on board, the way QGroundControl (PX4) and Mission Planner
(ArduPilot) do. These tests drive the FTP client against a simulated vehicle,
including one that drops part of a burst, and parse both metadata formats from
bytes built here.
"""
from __future__ import annotations

import json
import lzma
import struct
import threading
import time
import zlib
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus import autopilot
from corvus.mavlink_bridge import MavlinkBridge
from corvus.mavlink_ftp import (
    FTP_DATA_MAX,
    FTP_ERR_EOF,
    FTP_ERR_FILE_NOT_FOUND,
    FTP_ERR_NO_SESSIONS,
    FTP_OP_ACK,
    FTP_OP_BURST_READ_FILE,
    FTP_OP_CALC_FILE_CRC32,
    FTP_OP_NAK,
    FTP_OP_OPEN_FILE_RO,
    FTP_OP_RESET_SESSIONS,
    FTP_OP_TERMINATE_SESSION,
    FtpError,
    decode_ftp_payload,
    encode_ftp_payload,
)
from corvus.param_metadata import (
    ParamMetadataCache,
    parse_ardupilot_pck,
    parse_param_metadata,
    parse_px4_metadata,
)
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# A vehicle that answers MAVLink FTP
# ---------------------------------------------------------------------------

class FtpVehicle:
    """Answers FILE_TRANSFER_PROTOCOL requests the way PX4 and ArduPilot do.

    ``drop`` is a set of chunk offsets lost on their first send, which is what
    a radio does to a burst. ``busy`` makes the first open fail for want of a
    session, as it does after a transfer that was never closed.
    """

    def __init__(self, bridge: MavlinkBridge, files: dict[str, bytes], *,
                 drop: set[int] | None = None, busy: bool = False,
                 report_size: bool = True, crc: bool = True) -> None:
        self.bridge = bridge
        self.files = files
        self.drop = set(drop or ())
        self.busy = busy
        self.report_size = report_size
        self.crc = crc
        self.requests: list[Any] = []
        self.open_file: bytes | None = None

    # pymavlink's sender, as the bridge calls it
    def file_transfer_protocol_send(self, network: int, system: int,
                                    component: int, payload: list[int]) -> None:
        request = decode_ftp_payload(payload)
        self.requests.append(request)
        for reply in self._answer(request):
            self.bridge._handle_ftp_message(SimpleNamespace(
                target_system=255, target_component=190, payload=reply,
            ))

    def _reply(self, request: Any, opcode: int, *, seq: int | None = None,
               data: bytes = b"", offset: int = 0, burst_complete: bool = False,
               session: int = 0) -> list[int]:
        payload = encode_ftp_payload(
            request.seq + 1 if seq is None else seq, session, opcode,
            offset=offset, data=data)
        payload[5] = request.opcode
        payload[6] = 1 if burst_complete else 0
        return payload

    def _answer(self, request: Any) -> list[list[int]]:
        if request.opcode == FTP_OP_OPEN_FILE_RO:
            if self.busy:
                self.busy = False
                return [self._reply(request, FTP_OP_NAK, data=bytes([FTP_ERR_NO_SESSIONS]))]
            path = request.data.decode()
            if path not in self.files:
                return [self._reply(request, FTP_OP_NAK, data=bytes([FTP_ERR_FILE_NOT_FOUND]))]
            self.open_file = self.files[path]
            size = len(self.open_file) if self.report_size else 0
            return [self._reply(request, FTP_OP_ACK, data=struct.pack("<I", size))]
        if request.opcode == FTP_OP_BURST_READ_FILE:
            data = self.open_file or b""
            out = []
            seq = request.seq + 1
            offset = request.offset
            while offset < len(data):
                chunk = data[offset:offset + FTP_DATA_MAX]
                if offset in self.drop:
                    self.drop.discard(offset)
                else:
                    out.append(self._reply(request, FTP_OP_ACK, seq=seq, data=chunk, offset=offset))
                seq += 1
                offset += len(chunk)
            out.append(self._reply(request, FTP_OP_NAK, seq=seq,
                                   data=bytes([FTP_ERR_EOF]), offset=len(data)))
            return out
        if request.opcode == FTP_OP_CALC_FILE_CRC32 and self.crc:
            path = request.data.decode()
            if path not in self.files:
                return [self._reply(request, FTP_OP_NAK, data=bytes([FTP_ERR_FILE_NOT_FOUND]))]
            return [self._reply(request, FTP_OP_ACK,
                                data=struct.pack("<I", zlib.crc32(self.files[path])))]
        if request.opcode in (FTP_OP_TERMINATE_SESSION, FTP_OP_RESET_SESSIONS):
            self.open_file = None
            return [self._reply(request, FTP_OP_ACK)]
        return [self._reply(request, FTP_OP_NAK, data=bytes([7]))]


def ftp_bridge(files: dict[str, bytes], **kwargs: Any) -> tuple[MavlinkBridge, FtpVehicle]:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    vehicle = FtpVehicle(bridge, files, **kwargs)
    bridge._conn = SimpleNamespace(mav=vehicle, source_system=255, source_component=190)
    return bridge, vehicle


# ---------------------------------------------------------------------------
# The FTP client
# ---------------------------------------------------------------------------

def test_payload_round_trips_through_the_wire_layout() -> None:
    payload = encode_ftp_payload(513, 2, FTP_OP_OPEN_FILE_RO, offset=70000, data=b"/etc/x")
    assert len(payload) == 251
    reply = decode_ftp_payload(payload)
    assert (reply.seq, reply.session, reply.opcode, reply.offset) == (513, 2, 4, 70000)
    assert reply.data == b"/etc/x" and reply.size == 6


def test_a_file_is_read_whole_in_one_burst() -> None:
    content = bytes(range(256)) * 9
    bridge, vehicle = ftp_bridge({"/etc/extras/parameters.json.xz": content})
    seen: list[tuple[int, int]] = []
    data = bridge.ftp_read_file("/etc/extras/parameters.json.xz",
                                progress=lambda got, size: seen.append((got, size)))
    assert data == content
    assert seen[-1] == (len(content), len(content))
    opcodes = [r.opcode for r in vehicle.requests]
    assert opcodes[0] == FTP_OP_OPEN_FILE_RO
    assert opcodes.count(FTP_OP_BURST_READ_FILE) == 1
    assert opcodes[-1] == FTP_OP_TERMINATE_SESSION, "the session is closed afterwards"


def test_chunks_a_radio_lost_are_asked_for_again_from_the_first_gap() -> None:
    content = bytes(i % 251 for i in range(FTP_DATA_MAX * 10 + 17))
    lost = {FTP_DATA_MAX * 3, FTP_DATA_MAX * 7}
    bridge, vehicle = ftp_bridge({"/f": content}, drop=lost)
    assert bridge.ftp_read_file("/f") == content
    bursts = [r.offset for r in vehicle.requests if r.opcode == FTP_OP_BURST_READ_FILE]
    assert bursts[0] == 0
    assert bursts[1] == FTP_DATA_MAX * 3, "the second burst starts at the first hole"


def test_a_file_of_unknown_size_ends_at_the_end_of_file_answer() -> None:
    """ArduPilot's generated @PARAM file may not report a size on open."""
    content = b"x" * (FTP_DATA_MAX * 2 + 5)
    bridge, _ = ftp_bridge({"@PARAM/param.pck?withdefaults=1": content}, report_size=False)
    assert bridge.ftp_read_file("@PARAM/param.pck?withdefaults=1") == content


def test_a_session_left_open_is_reset_once() -> None:
    bridge, vehicle = ftp_bridge({"/f": b"hello"}, busy=True)
    assert bridge.ftp_read_file("/f") == b"hello"
    opcodes = [r.opcode for r in vehicle.requests]
    assert opcodes[:3] == [FTP_OP_OPEN_FILE_RO, FTP_OP_RESET_SESSIONS, FTP_OP_OPEN_FILE_RO]


def test_a_missing_file_is_reported_with_its_code() -> None:
    bridge, _ = ftp_bridge({})
    with pytest.raises(FtpError) as err:
        bridge.ftp_read_file("/etc/extras/parameters.json.xz")
    assert err.value.code == FTP_ERR_FILE_NOT_FOUND


def test_a_silent_vehicle_times_out(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("corvus.mavlink_ftp.FTP_REPLY_TIMEOUT_S", 0.05)
    bridge, vehicle = ftp_bridge({})
    vehicle._answer = lambda request: []   # type: ignore[method-assign]
    with pytest.raises(FtpError, match="no answer"):
        bridge.ftp_read_file("/f")


def test_replies_addressed_to_another_station_are_ignored() -> None:
    bridge, _ = ftp_bridge({})
    bridge._ftp_listening = True
    bridge._handle_ftp_message(SimpleNamespace(
        target_system=254, target_component=190,
        payload=encode_ftp_payload(1, 0, FTP_OP_ACK)))
    assert not bridge._ftp_inbox


def test_stop_cancels_a_transfer_that_is_waiting() -> None:
    bridge, vehicle = ftp_bridge({})
    vehicle._answer = lambda request: []   # type: ignore[method-assign]
    errors: list[Exception] = []

    def run() -> None:
        try:
            bridge.ftp_read_file("/f")
        except FtpError as exc:
            errors.append(exc)

    worker = threading.Thread(target=run)
    worker.start()
    time.sleep(0.05)
    bridge.abort_ftp()
    worker.join(timeout=1)
    assert not worker.is_alive()
    assert errors and "cancelled" in str(errors[0])


def test_one_transfer_at_a_time() -> None:
    bridge, _ = ftp_bridge({})
    bridge._ftp_lock.acquire()
    try:
        with pytest.raises(FtpError, match="in progress"):
            bridge.ftp_read_file("/f")
    finally:
        bridge._ftp_lock.release()


def test_the_bridge_routes_ftp_replies_from_the_autopilot() -> None:
    bridge, _ = ftp_bridge({})
    bridge._ftp_listening = True
    msg = SimpleNamespace(
        get_type=lambda: "FILE_TRANSFER_PROTOCOL", get_srcSystem=lambda: 1,
        get_srcComponent=lambda: 1, target_system=255, target_component=190,
        payload=encode_ftp_payload(9, 0, FTP_OP_ACK),
    )
    bridge._dispatch(msg)
    assert len(bridge._ftp_inbox) == 1


# ---------------------------------------------------------------------------
# PX4 parameters.json
# ---------------------------------------------------------------------------

PX4_METADATA = {
    "version": 1,
    "parameters": [
        {"name": "MPC_XY_VEL_MAX", "type": "Float", "default": 12.0,
         "shortDesc": "Maximum horizontal velocity", "longDesc": "Long text.",
         "units": "m/s", "min": 0, "max": 20, "decimalPlaces": 1,
         "increment": 1, "group": "Multicopter Position Control"},
        {"name": "COM_RC_IN_MODE", "type": "Int32", "default": 3,
         "shortDesc": "RC control input mode", "rebootRequired": True,
         "values": [{"value": 0, "description": "RC Transmitter only"},
                    {"value": 1, "description": "Joystick only"}]},
        {"name": "SDLOG_PROFILE", "type": "Int32", "default": 1,
         "bitmask": [{"index": 0, "description": "Default set"}]},
        {"name": "", "default": 1},
    ],
}


def test_px4_metadata_is_read_from_the_compressed_file() -> None:
    raw = lzma.compress(json.dumps(PX4_METADATA).encode(), format=lzma.FORMAT_XZ)
    meta = parse_px4_metadata(raw)
    vel = meta["MPC_XY_VEL_MAX"]
    assert vel["default"] == 12.0
    assert vel["short_desc"] == "Maximum horizontal velocity"
    assert vel["units"] == "m/s"
    assert (vel["min"], vel["max"], vel["decimals"]) == (0.0, 20.0, 1)
    assert "reboot_required" not in vel
    mode = meta["COM_RC_IN_MODE"]
    assert mode["reboot_required"] is True
    assert mode["values"] == [[0.0, "RC Transmitter only"], [1.0, "Joystick only"]]
    assert meta["SDLOG_PROFILE"]["bitmask"] == [[0, "Default set"]]
    assert "" not in meta


def test_px4_metadata_also_reads_uncompressed() -> None:
    assert parse_px4_metadata(json.dumps(PX4_METADATA).encode())["SDLOG_PROFILE"]["default"] == 1.0


def test_something_that_is_not_px4_metadata_is_refused() -> None:
    with pytest.raises(ValueError):
        parse_px4_metadata(b'{"hello": 1}')


# ---------------------------------------------------------------------------
# ArduPilot param.pck
# ---------------------------------------------------------------------------

_PCK_FMT = {1: "<b", 2: "<h", 3: "<i", 4: "<f"}


def build_pck(entries: list[tuple[str, int, float, float | None]], *, defaults: bool = True,
              pad_after: int | None = None) -> bytes:
    """Pack (name, type, value, default-or-None) the way ArduPilot does."""
    out = bytearray(struct.pack("<HHH", 0x671C if defaults else 0x671B, len(entries), len(entries)))
    last = b""
    for index, (name, ptype, value, default) in enumerate(entries):
        encoded = name.encode()
        common = 0
        while common < min(len(last), len(encoded), 15) and last[common] == encoded[common]:
            common += 1
        suffix = encoded[common:]
        flags = 1 if (defaults and default is not None) else 0
        out += bytes([ptype | (flags << 4), ((len(suffix) - 1) << 4) | common]) + suffix
        out += struct.pack(_PCK_FMT[ptype], value)
        if flags:
            out += struct.pack(_PCK_FMT[ptype], default)
        if pad_after == index:
            out += b"\x00\x00\x00"
        last = encoded
    return bytes(out)


def test_ardupilot_pack_gives_each_parameter_its_default() -> None:
    raw = build_pck([
        ("ARMING_CHECK", 3, 0, 1),        # changed from its default
        ("ARMING_REQUIRE", 1, 1, None),   # at its default: no second value
        ("ATC_RAT_RLL_P", 4, 0.25, 0.135),
    ], pad_after=1)
    meta = parse_ardupilot_pck(raw)
    assert meta["ARMING_CHECK"] == {"default": 1.0}
    assert meta["ARMING_REQUIRE"] == {"default": 1.0}
    assert meta["ATC_RAT_RLL_P"]["default"] == pytest.approx(0.135, rel=1e-6)


def test_an_ardupilot_pack_without_defaults_is_empty_not_an_error() -> None:
    meta = parse_ardupilot_pck(build_pck([("ARMING_CHECK", 3, 0, None)], defaults=False))
    assert meta == {"ARMING_CHECK": {}}


def test_a_truncated_pack_is_refused() -> None:
    raw = build_pck([("ARMING_CHECK", 3, 0, 1), ("ARMING_REQUIRE", 1, 1, None)])
    with pytest.raises(ValueError):
        parse_ardupilot_pck(raw[:-3])


def test_unknown_metadata_format_is_refused() -> None:
    with pytest.raises(ValueError):
        parse_param_metadata("xml", b"")


# ---------------------------------------------------------------------------
# The fetch, per dialect
# ---------------------------------------------------------------------------

def wait_for_state(bridge: MavlinkBridge, *states: str) -> dict:
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        status = bridge.param_metadata_status(include_params=True)
        if status["state"] in states:
            return status
        time.sleep(0.01)
    raise AssertionError(f"metadata stuck at {bridge.param_metadata_status()}")


def test_px4_defaults_are_fetched_from_the_path_the_dialect_names() -> None:
    raw = lzma.compress(json.dumps(PX4_METADATA).encode(), format=lzma.FORMAT_XZ)
    bridge, vehicle = ftp_bridge({"/etc/extras/parameters.json.xz": raw})
    assert bridge.start_param_metadata()["state"] == "loading"
    status = wait_for_state(bridge, "ready", "unavailable")
    assert status["state"] == "ready", status
    assert status["params"]["MPC_XY_VEL_MAX"]["default"] == 12.0
    assert status["count"] == 3
    # A second request while ready does not download again.
    before = len(vehicle.requests)
    assert bridge.start_param_metadata()["state"] == "ready"
    assert len(vehicle.requests) == before
    bridge.stop()


def test_ardupilot_defaults_come_from_the_generated_pack() -> None:
    raw = build_pck([("ARMING_CHECK", 3, 0, 1)])
    bridge, vehicle = ftp_bridge({"@PARAM/param.pck?withdefaults=1": raw})
    bridge._dialect = autopilot.dialect_for(mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    bridge.start_param_metadata()
    status = wait_for_state(bridge, "ready", "unavailable")
    assert status["params"] == {"ARMING_CHECK": {"default": 1.0}}
    assert vehicle.requests[0].data == b"@PARAM/param.pck?withdefaults=1"
    bridge.stop()


def test_firmware_built_without_metadata_is_reported_not_hidden() -> None:
    bridge, _ = ftp_bridge({})
    bridge.start_param_metadata()
    status = wait_for_state(bridge, "unavailable")
    assert status["error"] == "this firmware was built without parameter metadata"
    bridge.stop()


def test_a_stack_without_metadata_is_unavailable_at_once() -> None:
    bridge, vehicle = ftp_bridge({})
    bridge._dialect = autopilot.dialect_for_stack(autopilot.STACK_GENERIC)
    status = bridge.start_param_metadata()
    assert status["state"] == "unavailable"
    assert "does not publish parameter defaults" in status["error"]
    assert vehicle.requests == []


def test_a_new_link_forgets_the_metadata() -> None:
    raw = lzma.compress(json.dumps(PX4_METADATA).encode(), format=lzma.FORMAT_XZ)
    bridge, _ = ftp_bridge({"/etc/extras/parameters.json.xz": raw})
    bridge.start_param_metadata()
    wait_for_state(bridge, "ready")
    bridge._reset_param_metadata()
    status = bridge.param_metadata_status(include_params=True)
    assert status["state"] == "idle" and "params" not in status
    bridge.stop()


def test_not_connected_starts_nothing() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    status = bridge.start_param_metadata()
    assert status["state"] == "idle" and status["error"] == "not connected"
    assert bridge._param_meta_thread is None


def test_dialects_name_their_metadata_and_file_format() -> None:
    px4 = autopilot.dialect_for_stack(autopilot.STACK_PX4)
    ardupilot = autopilot.dialect_for_stack(autopilot.STACK_ARDUPILOT)
    assert px4.param_file_format == "qgc"
    assert ardupilot.param_file_format == "mission-planner"
    assert px4.capabilities()["param_defaults"] is True
    assert autopilot.dialect_for_stack(autopilot.STACK_GENERIC).capabilities()["param_defaults"] is False


# ---------------------------------------------------------------------------
# The opt-in copy on this machine
# ---------------------------------------------------------------------------

PX4_PATH = "/etc/extras/parameters.json.xz"


def px4_raw(default: float = 12.0) -> bytes:
    doc = {"parameters": [{"name": "MPC_XY_VEL_MAX", "type": "Float", "default": default}]}
    return lzma.compress(json.dumps(doc).encode(), format=lzma.FORMAT_XZ)


def fetch(bridge: MavlinkBridge, cache: ParamMetadataCache | None) -> dict:
    bridge.start_param_metadata(cache=cache)
    status = wait_for_state(bridge, "ready", "unavailable")
    bridge.stop()
    return status


def test_a_copy_is_made_and_then_used_instead_of_the_download(tmp_path) -> None:
    cache = ParamMetadataCache(tmp_path)
    bridge, vehicle = ftp_bridge({PX4_PATH: px4_raw()})
    first = fetch(bridge, cache)
    assert first["source"] == "vehicle"
    assert cache.info()["files"] == 1, "the download is kept"

    bridge, vehicle = ftp_bridge({PX4_PATH: px4_raw()})
    second = fetch(bridge, cache)
    assert second["source"] == "cache"
    assert second["params"]["MPC_XY_VEL_MAX"]["default"] == 12.0
    opcodes = [r.opcode for r in vehicle.requests]
    assert FTP_OP_CALC_FILE_CRC32 in opcodes
    assert FTP_OP_OPEN_FILE_RO not in opcodes, "one checksum question, no download"


def test_a_different_file_on_the_vehicle_is_downloaded_again(tmp_path) -> None:
    cache = ParamMetadataCache(tmp_path)
    fetch(ftp_bridge({PX4_PATH: px4_raw(12.0)})[0], cache)
    bridge, vehicle = ftp_bridge({PX4_PATH: px4_raw(15.0)})
    status = fetch(bridge, cache)
    assert status["source"] == "vehicle"
    assert status["params"]["MPC_XY_VEL_MAX"]["default"] == 15.0
    assert cache.info()["files"] == 2


def test_without_a_checksum_nothing_is_kept_or_trusted(tmp_path) -> None:
    cache = ParamMetadataCache(tmp_path)
    bridge, _ = ftp_bridge({PX4_PATH: px4_raw()}, crc=False)
    status = fetch(bridge, cache)
    assert status["state"] == "ready" and status["source"] == "vehicle"
    assert cache.info()["files"] == 0


def test_an_unreadable_copy_is_thrown_away_and_downloaded(tmp_path) -> None:
    cache = ParamMetadataCache(tmp_path)
    raw = px4_raw()
    cache.store("px4-json", zlib.crc32(raw), b"not xz")
    bridge, _ = ftp_bridge({PX4_PATH: raw})
    status = fetch(bridge, cache)
    assert status["source"] == "vehicle"
    assert cache.load("px4-json", zlib.crc32(raw)) == raw, "replaced by the real file"


def test_ardupilot_never_uses_the_copy(tmp_path) -> None:
    """Its defaults follow the frame the vehicle is set up as."""
    cache = ParamMetadataCache(tmp_path)
    bridge, vehicle = ftp_bridge({"@PARAM/param.pck?withdefaults=1": build_pck([("ARMING_CHECK", 3, 0, 1)])})
    bridge._dialect = autopilot.dialect_for(mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA)
    status = fetch(bridge, cache)
    assert status["source"] == "vehicle"
    assert FTP_OP_CALC_FILE_CRC32 not in [r.opcode for r in vehicle.requests]
    assert cache.info()["files"] == 0


def test_the_copies_are_bounded_and_can_be_cleared(tmp_path) -> None:
    cache = ParamMetadataCache(tmp_path)
    for n in range(ParamMetadataCache.MAX_FILES + 3):
        cache.store("px4-json", n + 1, b"x")
    assert cache.info()["files"] == ParamMetadataCache.MAX_FILES
    assert cache.clear() == ParamMetadataCache.MAX_FILES
    assert cache.info()["files"] == 0


def test_the_switch_is_a_real_boolean_in_the_config() -> None:
    from corvus.config import _build_config, _config_to_dict
    cfg = _build_config({"parameters": {"cache_defaults": True}})
    assert cfg.parameters == {"cache_defaults": True}
    assert _config_to_dict(cfg)["parameters"] == {"cache_defaults": True}
    assert _build_config({"parameters": {"cache_defaults": "true"}}).parameters is None
    assert _build_config({}).parameters is None
