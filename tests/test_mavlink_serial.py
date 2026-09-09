"""Serial-link (SiK Telemetry Radio V3) behaviour for the MAVLink bridge.

Covers serial port enumeration, connection-type detection, conservative
stream rates, longer heartbeat timeout, escalating reconnect backoff, and
the link_status / link_connection / link_error transitions written into the
Vehicle State Store. The store gains these fields from the backend agent; the
tests seed them locally so the bridge's writes are observable before that
integration lands.
"""
from __future__ import annotations

import sys
import time
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore


def seed_link_fields(store: VehicleStateStore) -> None:
    """Inject the link_status/link_connection/link_error keys the bridge writes.

    Mirrors the fields the backend agent will add to ``VehicleStateStore``.
    """
    with store._lock:
        store._data.setdefault("link_status", "disconnected")
        store._data.setdefault("link_connection", "")
        store._data.setdefault("link_error", "")


def make_heartbeat(armed: bool = False) -> SimpleNamespace:
    base_mode = mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED if armed else 0
    return SimpleNamespace(
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4,
        base_mode=base_mode,
        custom_mode=(3 << 16),  # POSCTL
    )


class _FakeMav:
    def __init__(self) -> None:
        self.streams: list[tuple[int, int]] = []
        self.version_requested = False
        self.heartbeats: list[tuple] = []

    def request_data_stream_send(
        self, target_system: int, target_component: int, stream: int, rate: int, start: int
    ) -> None:
        self.streams.append((stream, rate))

    def autopilot_version_request_send(self, target_system: int, target_component: int) -> None:
        self.version_requested = True

    def heartbeat_send(self, *args: Any) -> None:
        self.heartbeats.append(args)


class FakeConnectConnection:
    """Minimal pymavlink connection stand-in for a successful _connect()."""

    def __init__(self) -> None:
        self.mav = _FakeMav()
        self.target_system = 1
        self.target_component = 1
        self._heartbeat = make_heartbeat(armed=False)

    def wait_heartbeat(self, blocking: bool = True, timeout: float = 10) -> SimpleNamespace:
        return self._heartbeat

    def mode_mapping(self) -> dict[str, tuple[int, ...]] | None:
        return None

    def close(self) -> None:
        pass


# ---------------------------------------------------------------------------
# list_serial_ports
# ---------------------------------------------------------------------------

def test_list_serial_ports_uses_pyserial_and_sorts_by_device(monkeypatch: pytest.MonkeyPatch) -> None:
    import serial.tools.list_ports as list_ports

    infos = [
        SimpleNamespace(device="/dev/ttyUSB1", description="SiK Telemetry Radio V3",
                        hwid="USB VID:PID=26AC:0011 SNR=1"),
        SimpleNamespace(device="/dev/ttyUSB0", description="FTDI FT232R",
                        hwid="USB VID:PID=0403:6001"),
    ]
    monkeypatch.setattr(list_ports, "comports", lambda: infos)

    ports = MavlinkBridge.list_serial_ports()

    assert [p["device"] for p in ports] == ["/dev/ttyUSB0", "/dev/ttyUSB1"]
    assert all(set(p.keys()) == {"device", "description", "hwid"} for p in ports)
    assert ports[0]["description"] == "FTDI FT232R"
    assert ports[0]["hwid"] == "USB VID:PID=0403:6001"
    assert ports[1]["hwid"] == "USB VID:PID=26AC:0011 SNR=1"


def test_list_serial_ports_falls_back_to_glob_when_pyserial_import_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setitem(sys.modules, "serial", None)
    monkeypatch.setitem(sys.modules, "serial.tools", None)
    monkeypatch.setitem(sys.modules, "serial.tools.list_ports", None)

    import glob

    monkeypatch.setattr(glob, "glob", lambda pattern: ["/dev/ttyUSB1", "/dev/ttyUSB0"])

    ports = MavlinkBridge.list_serial_ports()

    assert [p["device"] for p in ports] == ["/dev/ttyUSB0", "/dev/ttyUSB1"]
    assert all(p["description"] == "" and p["hwid"] == "" for p in ports)


def test_list_serial_ports_falls_back_to_glob_when_comports_empty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import serial.tools.list_ports as list_ports

    monkeypatch.setattr(list_ports, "comports", lambda: [])

    import glob

    monkeypatch.setattr(glob, "glob", lambda pattern: ["/dev/ttyACM0"])

    ports = MavlinkBridge.list_serial_ports()

    assert [p["device"] for p in ports] == ["/dev/ttyACM0"]
    assert ports[0]["description"] == ""
    assert ports[0]["hwid"] == ""


def test_list_serial_ports_never_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    import serial.tools.list_ports as list_ports

    def boom_comports() -> list[Any]:
        raise OSError("comports broken")

    monkeypatch.setattr(list_ports, "comports", boom_comports)

    import glob

    def boom_glob(pattern: str) -> list[str]:
        raise OSError("glob broken")

    monkeypatch.setattr(glob, "glob", boom_glob)

    assert MavlinkBridge.list_serial_ports() == []


# ---------------------------------------------------------------------------
# _is_serial
# ---------------------------------------------------------------------------

def test_is_serial_detects_serial_prefix() -> None:
    assert MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")._is_serial() is True
    assert MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")._is_serial() is False
    assert MavlinkBridge(VehicleStateStore(), "tcp:1.2.3.4:5760")._is_serial() is False


# ---------------------------------------------------------------------------
# _stream_rates
# ---------------------------------------------------------------------------

def test_stream_rates_conservative_for_serial() -> None:
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    rates = bridge._stream_rates()
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_EXTRA1] == 10
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_POSITION] == 5
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_EXTRA2] == 5
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_EXTRA3] == 2
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS] == 0
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS] == 0


def test_stream_rates_high_for_udp() -> None:
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    rates = bridge._stream_rates()
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_EXTRA1] == 50
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_POSITION] == 10
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS] == 5
    assert rates[mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS] == 5


def test_request_streams_skips_zero_rate_streams_for_serial() -> None:
    store = VehicleStateStore()
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnectConnection()

    bridge._request_streams()

    requested_streams = {stream for stream, _ in bridge._conn.mav.streams}
    assert mavutil.mavlink.MAV_DATA_STREAM_RAW_SENSORS not in requested_streams
    assert mavutil.mavlink.MAV_DATA_STREAM_RC_CHANNELS not in requested_streams
    assert mavutil.mavlink.MAV_DATA_STREAM_EXTRA1 in requested_streams
    rates_by_stream = dict(bridge._conn.mav.streams)
    assert rates_by_stream[mavutil.mavlink.MAV_DATA_STREAM_EXTRA1] == 10


# ---------------------------------------------------------------------------
# _heartbeat_timeout
# ---------------------------------------------------------------------------

def test_heartbeat_timeout_longer_for_serial() -> None:
    assert MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")._heartbeat_timeout() == 10.0
    assert MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")._heartbeat_timeout() == 5.0


# ---------------------------------------------------------------------------
# _reconnect_delay (A4: exponential backoff + jitter, unified serial/udp)
# ---------------------------------------------------------------------------

def test_reconnect_delay_grows_then_caps(monkeypatch: pytest.MonkeyPatch) -> None:
    # Deterministic jitter (zero) so the exponential curve is exact.
    monkeypatch.setattr(
        "corvus.mavlink_bridge.random.uniform", lambda a, b: 0.0,
    )
    bridge = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    # base=0.5, cap=8.0: 0.5, 1.0, 2.0, 4.0, 8.0, 8.0, 8.0 …
    assert [bridge._reconnect_delay(a) for a in (1, 2, 3, 4, 5, 6, 99)] == [
        0.5, 1.0, 2.0, 4.0, 8.0, 8.0, 8.0,
    ]


def test_reconnect_delay_applies_jitter_within_bounds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Jitter of +/-25% must keep the delay within [base*0.75, base*1.25].
    monkeypatch.setattr(
        "corvus.mavlink_bridge.random.uniform", lambda a, b: b,
    )
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    # attempt 1: base 0.5 * (1 + 0.25) = 0.625
    assert bridge._reconnect_delay(1) == pytest.approx(0.625)
    # attempt 5: capped at 8.0 * 1.25 = 10.0
    assert bridge._reconnect_delay(5) == pytest.approx(10.0)


def test_reconnect_delay_never_below_floor(monkeypatch: pytest.MonkeyPatch) -> None:
    # Worst-case negative jitter still respects the 0.1s floor.
    monkeypatch.setattr(
        "corvus.mavlink_bridge.random.uniform", lambda a, b: a,
    )
    bridge = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    assert bridge._reconnect_delay(1) == pytest.approx(0.5 * 0.75)
    assert bridge._reconnect_delay(1) >= 0.1


def test_reconnect_delay_is_unified_across_serial_and_udp(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A4 removed the serial/udp split — both use the same exponential curve.
    monkeypatch.setattr(
        "corvus.mavlink_bridge.random.uniform", lambda a, b: 0.0,
    )
    serial = MavlinkBridge(VehicleStateStore(), "serial:/dev/ttyUSB0:57600")
    udp = MavlinkBridge(VehicleStateStore(), "udp:0.0.0.0:14540")
    for attempt in (1, 2, 3, 4, 5):
        assert serial._reconnect_delay(attempt) == udp._reconnect_delay(attempt)


# ---------------------------------------------------------------------------
# link-status transitions
# ---------------------------------------------------------------------------

def test_connect_failure_for_missing_serial_sets_link_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")

    def boom(*args: Any, **kwargs: Any) -> Any:
        raise FileNotFoundError(2, "No such file or directory")

    monkeypatch.setattr(mavutil, "mavlink_connection", boom)

    with pytest.raises(ConnectionError, match="serial device unavailable"):
        bridge._connect()

    snap = store.get_snapshot()
    assert snap["link_status"] in {"connecting", "reconnecting"}
    assert snap["link_error"]
    assert "serial device unavailable" in snap["link_error"]
    assert snap["link_connection"] == "serial:/dev/ttyUSB0:57600"


def test_connect_success_sets_connected_status(monkeypatch: pytest.MonkeyPatch) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")
    fake_conn = FakeConnectConnection()

    monkeypatch.setattr(mavutil, "mavlink_connection", lambda *a, **k: fake_conn)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    snap = store.get_snapshot()
    assert snap["link_status"] == "connected"
    assert snap["link_error"] == ""
    assert snap["link_connection"] == "udp:0.0.0.0:14540"
    assert snap["connected"] is True
    # High-rate (non-serial) streams were actually requested.
    requested = dict(fake_conn.mav.streams)
    assert requested[mavutil.mavlink.MAV_DATA_STREAM_EXTRA1] == 50


def test_stop_marks_link_disconnected() -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")
    bridge._running.set()
    store.update(link_status="reconnecting", link_error="radio dropped")

    bridge.stop()

    assert store.get_snapshot()["link_status"] == "disconnected"


# ---------------------------------------------------------------------------
# _run reconnect cycle on a disappearing serial device
# ---------------------------------------------------------------------------

def test_run_sets_reconnecting_and_retries_on_serial_disconnect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")
    bridge._running.set()

    connect_calls = {"n": 0}

    def fake_connect() -> None:
        connect_calls["n"] += 1
        raise ConnectionError("serial device unavailable: serial:/dev/ttyUSB0:57600")

    sleep_calls = {"n": 0}

    def fake_sleep(seconds: float) -> None:
        sleep_calls["n"] += 1
        # Stop the retry loop after two reconnect cycles.
        if sleep_calls["n"] >= 2:
            bridge._running.clear()

    monkeypatch.setattr(bridge, "_connect", fake_connect)
    monkeypatch.setattr(time, "sleep", fake_sleep)

    bridge._run()

    assert connect_calls["n"] == 2  # retried at least once after the first failure
    snap = store.get_snapshot()
    assert snap["link_status"] == "reconnecting"
    assert "serial device unavailable" in snap["link_error"]
    # Serial backoff escalated on the second cycle (attempt 2 -> 4.0s).
    assert sleep_calls["n"] == 2


def test_run_resets_reconnect_attempt_after_successful_connect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")
    bridge._running.set()
    bridge._reconnect_attempt = 4  # pretend several failed attempts already

    def fake_connect_success() -> None:
        # Successful connect path resets the attempt counter.
        bridge._reconnect_attempt = 1
        # Then the receive loop immediately bails out (running cleared).
        bridge._running.clear()

    monkeypatch.setattr(bridge, "_connect", fake_connect_success)
    monkeypatch.setattr(bridge, "_receive_loop", lambda: None)

    bridge._run()

    assert bridge._reconnect_attempt == 1


# ---------------------------------------------------------------------------
# _parse_serial (Bug 1: serial: prefix must be translated for mavserial)
# ---------------------------------------------------------------------------

def test_parse_serial_parses_device_and_baud() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    assert bridge._parse_serial("serial:/dev/ttyUSB0:57600") == ("/dev/ttyUSB0", 57600)
    assert bridge._parse_serial("serial:/dev/ttyUSB0") == ("/dev/ttyUSB0", 57600)
    assert bridge._parse_serial("serial:COM3:115200") == ("COM3", 115200)


def test_parse_serial_falls_back_to_default_baud_on_non_numeric_suffix() -> None:
    bridge = MavlinkBridge(VehicleStateStore())
    device, baud = bridge._parse_serial("serial:weird:notanumber")
    assert device == "weird:notanumber"
    assert baud == 57600


def test_connect_calls_mavserial_with_translated_device_and_baud(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Serial ``serial:`` strings must reach mavserial, not mavudp."""
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "serial:/dev/ttyUSB0:57600")

    captured: dict[str, Any] = {}

    def fake_mavlink_connection(device: str, **kwargs: Any) -> Any:
        captured["device"] = device
        captured["kwargs"] = kwargs
        return FakeConnectConnection()

    monkeypatch.setattr(mavutil, "mavlink_connection", fake_mavlink_connection)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    assert captured["device"] == "/dev/ttyUSB0"
    assert captured["kwargs"] == {
        "baud": 57600,
        "source_system": 254,
        "source_component": 190,
    }
    # ``timeout=`` must NOT be passed to mavserial.
    assert "timeout" not in captured["kwargs"]


def test_connect_non_serial_keeps_timeout_kwarg(monkeypatch: pytest.MonkeyPatch) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")

    captured: dict[str, Any] = {}

    def fake_mavlink_connection(device: str, **kwargs: Any) -> Any:
        captured["device"] = device
        captured["kwargs"] = kwargs
        return FakeConnectConnection()

    monkeypatch.setattr(mavutil, "mavlink_connection", fake_mavlink_connection)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    assert captured["device"] == "udp:0.0.0.0:14540"
    assert captured["kwargs"] == {
        "timeout": 2,
        "source_system": 254,
        "source_component": 190,
    }


# ---------------------------------------------------------------------------
# list_serial_ports filters phantom 8250 platform ports (Bug 2)
# ---------------------------------------------------------------------------

def test_list_serial_ports_filters_phantom_ttyS_ports(monkeypatch: pytest.MonkeyPatch) -> None:
    import serial.tools.list_ports as list_ports

    infos = [
        SimpleNamespace(device=f"/dev/ttyS{n}", description="n/a", hwid="n/a")
        for n in range(32)
    ]
    infos.append(SimpleNamespace(
        device="/dev/ttyUSB0", description="Holybro SiK Telemetry Radio V3",
        hwid="USB VID:PID=0403:6015",
    ))
    infos.append(SimpleNamespace(
        device="/dev/ttyACM0", description="Pixhawk FMU", hwid="USB VID:PID=26AC:0011",
    ))
    monkeypatch.setattr(list_ports, "comports", lambda: infos)

    ports = MavlinkBridge.list_serial_ports()

    devices = [p["device"] for p in ports]
    assert "/dev/ttyUSB0" in devices
    assert "/dev/ttyACM0" in devices
    assert not any(d.startswith("/dev/ttyS") for d in devices)
    assert len(ports) == 2
    assert devices == sorted(devices)
    assert ports[0]["description"] == "Pixhawk FMU"  # /dev/ttyACM0 sorts first
    assert ports[1]["hwid"] == "USB VID:PID=0403:6015"


def test_list_serial_ports_keeps_real_platform_ports_like_ama_and_ths(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import serial.tools.list_ports as list_ports

    infos = [
        SimpleNamespace(device="/dev/ttyAMA0", description="UART0", hwid=""),
        SimpleNamespace(device="/dev/ttyTHS0", description="Tegra HS", hwid=""),
        SimpleNamespace(device="/dev/ttyS0", description="n/a", hwid="n/a"),
    ]
    monkeypatch.setattr(list_ports, "comports", lambda: infos)

    ports = MavlinkBridge.list_serial_ports()

    devices = [p["device"] for p in ports]
    assert devices == ["/dev/ttyAMA0", "/dev/ttyTHS0"]


# ---------------------------------------------------------------------------
# target_component / target_system from heartbeat (Bug 3)
# ---------------------------------------------------------------------------

def _hb_with_source_ids(src_sys: int, src_comp: int) -> SimpleNamespace:
    """Heartbeat stand-in exposing get_srcSystem/get_srcComponent like pymavlink."""
    hb = make_heartbeat(armed=False)

    def get_srcSystem() -> int:
        return src_sys

    def get_srcComponent() -> int:
        return src_comp

    hb.get_srcSystem = get_srcSystem  # type: ignore[attr-defined]
    hb.get_srcComponent = get_srcComponent  # type: ignore[attr-defined]
    return hb


def test_connect_uses_heartbeat_source_ids_for_targets(monkeypatch: pytest.MonkeyPatch) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")

    fake_conn = FakeConnectConnection()
    fake_conn.target_component = 0  # pymavlink leaves this 0 until addressed
    fake_conn.target_system = 0
    fake_conn._heartbeat = _hb_with_source_ids(src_sys=1, src_comp=1)

    monkeypatch.setattr(mavutil, "mavlink_connection", lambda *a, **k: fake_conn)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    assert bridge._target_system == 1
    assert bridge._target_component == 1


def test_connect_defaults_target_component_to_one_when_heartbeat_reports_zero(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")

    fake_conn = FakeConnectConnection()
    fake_conn.target_component = 0
    fake_conn.target_system = 0
    fake_conn._heartbeat = _hb_with_source_ids(src_sys=0, src_comp=0)

    monkeypatch.setattr(mavutil, "mavlink_connection", lambda *a, **k: fake_conn)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    assert bridge._target_system == 1
    assert bridge._target_component == 1


def test_connect_handles_heartbeat_without_get_srcComponent_method(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Test fakes (and some pymavlink builds) may omit get_srcComponent."""
    store = VehicleStateStore()
    seed_link_fields(store)
    bridge = MavlinkBridge(store, "udp:0.0.0.0:14540")

    fake_conn = FakeConnectConnection()
    fake_conn.target_component = 1
    fake_conn.target_system = 1
    # Heartbeat without get_srcSystem/get_srcComponent → defensive getattr.
    fake_conn._heartbeat = make_heartbeat(armed=False)

    monkeypatch.setattr(mavutil, "mavlink_connection", lambda *a, **k: fake_conn)
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)

    bridge._connect()

    # Falls back to conn.target_* (both 1 here) then ``or 1`` keeps it 1.
    assert bridge._target_system == 1
    assert bridge._target_component == 1
