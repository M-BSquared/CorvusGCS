"""The RTK session, driven end to end against a receiver made of bytes.

The point of this file is that none of it needs a tripod. :class:`FakeReceiver`
speaks enough UBX to be configured — it answers ``MON-VER``, ACKs a
``CFG-VALSET``, reports ``NAV-SVIN`` and starts emitting RTCM once its output
has been switched on — so the whole state machine the operator waits three
minutes on (search, identify, stop the old survey, run the new one, activate,
inject) runs here in well under a second.

What is asserted is the behaviour that costs something when it breaks:

* a base is found and configured without anybody opening the page (the default
  is the feature)
* the vehicle's telemetry port is never opened, whatever is on it
* a receiver that says nothing is listened to rather than written to
* the survey's settings actually reach the receiver, in the order that makes
  them apply
* every correction that arrives reaches the link, and the thread ends cleanly
"""
from __future__ import annotations

import struct
import threading
import time

from corvus import rtk
from corvus.rtk_service import RtkService, STATE_ACTIVE, STATE_ERROR, STATE_SEARCHING


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------


def _rtcm(number: int, body: bytes = b"\x11" * 20) -> bytes:
    """One well-formed RTCM 3 frame carrying *number*."""
    payload = bytes([(number >> 4) & 0xFF, (number & 0x0F) << 4]) + body
    head = b"\xd3" + bytes([(len(payload) >> 8) & 0x03, len(payload) & 0xFF])
    frame = head + payload
    return frame + rtk.crc24q(frame).to_bytes(3, "big")


def _mon_ver(protocol: str = "27.11", model: str = "ZED-F9P") -> bytes:
    payload = b"EXT CORE 1.00 (a1b2c3)".ljust(30, b"\x00")
    payload += b"00190000".ljust(10, b"\x00")
    payload += f"PROTVER={protocol}".encode().ljust(30, b"\x00")
    payload += f"MOD={model}".encode().ljust(30, b"\x00")
    return rtk.ubx_frame(rtk.UBX_CLASS_MON, rtk.UBX_ID_MON_VER, payload)


def _nav_svin(duration: int, accuracy_m: float, valid: bool, active: bool) -> bytes:
    payload = struct.pack(
        "<B3sIIiiibbbbIIBB",
        0, b"\x00" * 3, 0, duration, 1, 2, 3, 0, 0, 0, 0,
        int(accuracy_m * 10000), 100, int(valid), int(active),
    ) + b"\x00\x00"
    return rtk.ubx_frame(rtk.UBX_CLASS_NAV, rtk.UBX_ID_NAV_SVIN, payload)


class FakeReceiver:
    """A u-blox base station, as far as a serial port can tell.

    ``writes`` keeps every UBX message it was sent, which is how the tests
    assert on what was configured rather than only on what came back.
    """

    def __init__(self, *, identify: bool = True, survey_polls: int = 2,
                 protocol: str = "27.11") -> None:
        self.device = "/dev/ttyACM9"
        self.baud = 0
        self._identify = identify
        self._protocol = protocol
        self._survey_polls = survey_polls
        self._out = bytearray()
        self._parser = rtk.UbxParser()
        self._lock = threading.Lock()
        self.writes: list[tuple[int, int, bytes]] = []
        self.closed = False
        self.streaming = False
        self.survey_running = False
        self.svin_polls = 0
        self.mode = 0

    # -- the port interface the transport uses --------------------------

    @property
    def in_waiting(self) -> int:
        with self._lock:
            return len(self._out)

    def read(self, size: int = 1) -> bytes:
        with self._lock:
            if self.streaming and len(self._out) < 64:
                self._out.extend(_rtcm(1005))
                self._out.extend(_rtcm(1074))
            if not self._out:
                return b""
            take = min(size, len(self._out))
            out = bytes(self._out[:take])
            del self._out[:take]
            return out

    def write(self, data: bytes) -> None:
        if self.closed:
            raise OSError("port is closed")
        for msg_class, msg_id, payload in self._parser.feed(data):
            self.writes.append((msg_class, msg_id, payload))
            self._react(msg_class, msg_id, payload)

    def flush(self) -> None:
        return None

    def close(self) -> None:
        self.closed = True

    # -- behaviour -------------------------------------------------------

    def _react(self, msg_class: int, msg_id: int, payload: bytes) -> None:
        if msg_class == rtk.UBX_CLASS_MON and msg_id == rtk.UBX_ID_MON_VER and not payload:
            if self._identify:
                self._queue(_mon_ver(self._protocol))
            return
        if msg_class == rtk.UBX_CLASS_NAV and msg_id == rtk.UBX_ID_NAV_SVIN and not payload:
            self.svin_polls += 1
            if not self.survey_running:
                self._queue(_nav_svin(0, 0.0, False, False))
            elif self.svin_polls <= self._survey_polls:
                self._queue(_nav_svin(self.svin_polls * 10, 6.0, False, True))
            else:
                self._queue(_nav_svin(200, 1.2, True, False))
            return
        if msg_class == rtk.UBX_CLASS_CFG and msg_id == rtk.UBX_ID_CFG_VALSET:
            self._apply_valset(payload)
            self._queue(rtk.ubx_frame(rtk.UBX_CLASS_ACK, rtk.UBX_ID_ACK_ACK,
                                      bytes((msg_class, msg_id))))

    def _apply_valset(self, payload: bytes) -> None:
        offset = 4
        while offset + 4 <= len(payload):
            key = struct.unpack_from("<I", payload, offset)[0]
            offset += 4
            size_field = (key >> 28) & 0x07
            width = 1 if size_field <= 2 else 2 if size_field == 3 else 4
            if offset + width > len(payload):
                return
            value = int.from_bytes(payload[offset:offset + width], "little")
            offset += width
            if key == rtk.KEY_TMODE_MODE:
                self.mode = value
                self.survey_running = value == 1
                if value != 1:
                    self.svin_polls = 0
            # The 1005 output key on USB is what "start streaming" means here.
            if key == rtk.KEY_MSGOUT_RTCM_1005_I2C + 3:
                self.streaming = value > 0

    def _queue(self, data: bytes) -> None:
        with self._lock:
            self._out.extend(data)

    def valset_items(self) -> dict[int, int]:
        """Every key this receiver was ever set to, flattened."""
        items: dict[int, int] = {}
        for msg_class, msg_id, payload in self.writes:
            if msg_class != rtk.UBX_CLASS_CFG or msg_id != rtk.UBX_ID_CFG_VALSET:
                continue
            offset = 4
            while offset + 4 <= len(payload):
                key = struct.unpack_from("<I", payload, offset)[0]
                offset += 4
                size_field = (key >> 28) & 0x07
                width = 1 if size_field <= 2 else 2 if size_field == 3 else 4
                items[key] = int.from_bytes(payload[offset:offset + width], "little")
                offset += width
        return items


class FakeBridge:
    """The half of MavlinkBridge the service touches."""

    def __init__(self, connection: str = "udp:0.0.0.0:14550", connected: bool = True) -> None:
        self._connection = connection
        self._connected = connected
        self.injected: list[bytes] = []
        self.forgotten = 0

    def inject_rtcm(self, frame: bytes) -> bool:
        self.injected.append(frame)
        return True

    def rtcm_stats(self) -> dict:
        return {"bytes": sum(len(f) for f in self.injected),
                "messages": len(self.injected), "dropped": 0, "age": 0.0}

    def forget_rtcm_session(self) -> None:
        self.forgotten += 1

    def connection_string(self) -> str:
        return self._connection

    def is_connected(self) -> bool:
        return self._connected


class FakeStore:
    def __init__(self) -> None:
        self.updates: list[dict] = []
        self._snapshot = {"gps_fix": "RTK_FIXED", "gps_satellites": 22, "gps_hdop": 0.7}

    def update(self, **kwargs) -> None:
        self.updates.append(kwargs)

    def get_snapshot(self) -> dict:
        return dict(self._snapshot)


def _service(receiver: FakeReceiver | None, bridge: FakeBridge | None = None,
             ports: list[dict] | None = None, settings: dict | None = None):
    bridge = bridge or FakeBridge()
    store = FakeStore()
    opened: list[tuple[str, int]] = []

    def open_port(device: str, baud: int):
        opened.append((device, baud))
        if receiver is None:
            raise OSError("no such device")
        receiver.baud = baud
        receiver.closed = False
        return receiver

    default_ports = [{
        "device": receiver.device if receiver else "/dev/ttyACM9",
        "description": "u-blox GNSS receiver",
        "hwid": "USB VID:PID=1546:01A9",
    }]
    service = RtkService(
        bridge, store, settings,
        open_port=open_port,
        list_ports=lambda: (default_ports if ports is None else ports),
    )
    return service, bridge, store, opened


def _wait_for(predicate, timeout: float = 6.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return False


# ---------------------------------------------------------------------------
# Plug and play
# ---------------------------------------------------------------------------


def test_a_base_plugged_in_is_surveyed_and_streamed_without_being_asked():
    """The default is the feature: nobody opened the page in this test."""
    receiver = FakeReceiver()
    service, bridge, store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE), \
            f"never went active: {service.status()['state']} {service.status()['message']}"
        assert _wait_for(lambda: len(bridge.injected) >= 2)
    finally:
        service.stop()

    # The corrections that arrived are the ones that were forwarded, intact.
    assert all(frame.startswith(b"\xd3") for frame in bridge.injected)
    assert {rtk.message_number(f) for f in bridge.injected} == {1005, 1074}

    status = service.status()
    assert status["messages"]["1005"] > 0
    assert status["receiver"]["model"] == "ZED-F9P"
    assert status["survey"]["valid"] is True


def test_the_survey_the_operator_asked_for_is_the_one_the_receiver_is_given():
    """Accuracy and duration have to survive the trip to the wire.

    They are set in metres and seconds and stored in 0.1 mm and seconds, which
    is exactly the kind of conversion that is wrong by a factor of ten for a
    year before anybody notices — the survey still completes, just at the
    wrong limit.
    """
    receiver = FakeReceiver()
    service, _bridge, _store, _opened = _service(
        receiver, settings={"survey_accuracy": 1.5, "survey_duration": 90},
    )
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
    finally:
        service.stop()

    items = receiver.valset_items()
    assert items[rtk.KEY_TMODE_SVIN_ACC_LIMIT] == 15000   # 1.5 m in 0.1 mm
    assert items[rtk.KEY_TMODE_SVIN_MIN_DUR] == 90


def test_the_old_survey_is_stopped_before_the_new_one_is_configured():
    """Survey-in over survey-in is a no-op on a u-blox receiver.

    So the mode must reach 0 before it reaches 1, or the settings above are
    quietly ignored and the page reports the previous survey's progress.
    """
    receiver = FakeReceiver()
    service, _bridge, _store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
    finally:
        service.stop()

    modes = []
    for msg_class, msg_id, payload in receiver.writes:
        if msg_class != rtk.UBX_CLASS_CFG or msg_id != rtk.UBX_ID_CFG_VALSET:
            continue
        offset = 4
        while offset + 4 <= len(payload):
            key = struct.unpack_from("<I", payload, offset)[0]
            offset += 4
            width = 1 if ((key >> 28) & 0x07) <= 2 else 2 if ((key >> 28) & 0x07) == 3 else 4
            if key == rtk.KEY_TMODE_MODE:
                modes.append(int.from_bytes(payload[offset:offset + width], "little"))
            offset += width
    assert modes[:2] == [0, 1], f"mode sequence was {modes}"


def test_progress_is_published_to_the_store_while_the_survey_runs():
    """The HUD and the page both read the store, so the survey has to be in it."""
    receiver = FakeReceiver(survey_polls=4)
    service, _bridge, store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: any(
            u.get("rtk_state") == "surveying" for u in store.updates
        ))
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
    finally:
        service.stop()

    states = [u["rtk_state"] for u in store.updates if "rtk_state" in u]
    assert "surveying" in states and "active" in states
    assert all(0 <= u["rtk_progress"] <= 100 for u in store.updates if "rtk_progress" in u)


# ---------------------------------------------------------------------------
# What it refuses to touch
# ---------------------------------------------------------------------------


def test_the_vehicles_own_serial_link_is_never_opened():
    """Two readers on one port is half a telemetry link and no RTK.

    The port is excluded by device name rather than by kind, because the kind
    is exactly what a mislabelled descriptor would get wrong.
    """
    receiver = FakeReceiver()
    bridge = FakeBridge(connection="serial:/dev/ttyACM9:57600")
    service, _bridge, _store, opened = _service(receiver, bridge=bridge)
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_SEARCHING)
    finally:
        service.stop()
    assert opened == []
    assert receiver.writes == []


def test_a_port_that_is_not_a_receiver_is_not_a_candidate():
    """A flight controller on /dev/ttyACM0 must not be surveyed."""
    receiver = FakeReceiver()
    service, _bridge, _store, opened = _service(receiver, ports=[{
        "device": "/dev/ttyACM0",
        "description": "PX4 FMU v6",
        "hwid": "USB VID:PID=26AC:0032",
    }])
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_SEARCHING)
    finally:
        service.stop()
    assert opened == []


def test_a_receiver_that_does_not_identify_is_listened_to_but_not_configured():
    """An already-configured base is the common case this protects.

    Somebody set their base up in u-center, or it is a make this module has
    never heard of. It streams RTCM the moment it is powered, and forwarding
    that needs no knowledge of it at all — so it is read, and nothing is
    written to it beyond the one poll that asked what it was.
    """
    receiver = FakeReceiver(identify=False)
    receiver.streaming = True
    service, bridge, _store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: len(bridge.injected) >= 2, timeout=8.0)
        status = service.status()
    finally:
        service.stop()

    assert status["state"] == STATE_ACTIVE
    assert status["receiver"] is None
    configured = [
        w for w in receiver.writes
        if w[0] == rtk.UBX_CLASS_CFG
    ]
    assert configured == [], f"an unidentified receiver was configured: {configured}"


def test_a_silent_source_is_reported_rather_than_left_looking_alive():
    """A base with no power looks exactly like one that is working, until this."""
    receiver = FakeReceiver(identify=False)
    receiver.streaming = False
    # The baud is pinned so this exercises the silence timeout rather than the
    # sweep of every candidate rate, which is a different (and slower) path.
    service, _bridge, _store, _opened = _service(receiver, settings={"baud": 38400})
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ERROR, timeout=15.0)
        status = service.status()
    finally:
        service.stop()
    assert "no corrections" in status["error"]


# ---------------------------------------------------------------------------
# Settings, lifecycle
# ---------------------------------------------------------------------------


def test_switching_rtk_off_stops_touching_the_hardware():
    receiver = FakeReceiver()
    service, _bridge, _store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
        service.apply_settings({"enabled": False})
        assert _wait_for(lambda: service.status()["state"] == "off")
        before = len(receiver.writes)
        time.sleep(0.4)
        assert len(receiver.writes) == before
    finally:
        service.stop()


def test_the_ntrip_password_never_leaves_through_the_status_endpoint():
    """The page builds its form from status(), so a password in it is a leak."""
    service, _bridge, _store, _opened = _service(None, settings={
        "source": "ntrip",
        "ntrip": {"host": "caster.example", "mountpoint": "MSM4", "password": "hunter2"},
    })
    status = service.status()
    assert status["settings"]["ntrip"]["password"] == ""
    assert status["settings"]["ntrip"]["has_password"] is True
    assert "hunter2" not in repr(status)
    # It is still held, and still what the caster would be given.
    assert service.settings()["ntrip"]["password"] == "hunter2"


def test_stop_joins_the_thread_and_releases_the_port():
    receiver = FakeReceiver()
    service, _bridge, _store, _opened = _service(receiver)
    service.start()
    assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
    service.stop(timeout=5.0)
    assert not service.is_alive()
    assert receiver.closed
    assert service.status()["state"] == "off"
    # Idempotent: a second stop on a stopped service is not an error.
    service.stop()


def test_a_receiver_is_taken_back_out_of_base_mode_when_the_session_ends():
    """A receiver left in base mode computes no position of its own.

    That is not a state to hand back to whoever plugs it in next, so the
    session undoes itself when the settings change under it.
    """
    receiver = FakeReceiver()
    service, _bridge, _store, _opened = _service(receiver)
    service.start()
    try:
        assert _wait_for(lambda: service.status()["state"] == STATE_ACTIVE)
        service.apply_settings({"enabled": False})
        assert _wait_for(lambda: service.status()["state"] == "off")
    finally:
        service.stop()
    assert receiver.mode == 0, "the receiver was left in base mode"
