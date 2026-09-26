"""Link robustness: the station speaks first, reboots are noticed, and the
protocol details PX4 reads literally are sent the way it reads them.

Each test pins one fix from the 2026-09-22 MAVLink audit (PLAN.md):

* the GCS heartbeat goes out before the vehicle has been heard, so a far end
  that waits for the station (udpout:, mavlink-router server endpoints, SITL
  in a VM, SYS_USB_AUTO = 1) comes up at all;
* a calibration cancel survives PX4's interim TEMPORARILY_REJECTED;
* fly to points places its takeoff item at the aircraft, never at 0/0;
* a vehicle that reboots under a live link loses its stale parameter state;
* parameters the firmware does not have stop costing every page load;
* a 0 mAh pack and a tab-terminated STATUSTEXT are reported as what they are.
"""
from __future__ import annotations

import time
from types import SimpleNamespace
from typing import Any

import pytest
from pymavlink import mavutil

from corvus.mavlink_bridge import MavlinkBridge
from corvus.state_store import VehicleStateStore

M = mavutil.mavlink


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return 1

    def get_srcComponent(self) -> int:
        return 1


class RecordingMav:
    """The mav surface the bridge sends through, recording every call in order."""

    def __init__(self, bridge: MavlinkBridge | None = None) -> None:
        self.bridge = bridge
        self.calls: list[tuple[str, tuple]] = []
        self.on_command: Any = None
        self.answers: dict[str, float] = {}

    def __getattr__(self, name: str) -> Any:
        if not name.endswith("_send"):
            raise AttributeError(name)

        def record(*args: Any, **kwargs: Any) -> None:
            self.calls.append((name, args))
        return record

    def command_long_send(self, *args: Any) -> None:
        self.calls.append(("command_long_send", args))
        if self.on_command is not None:
            self.on_command(int(args[2]))

    def param_request_read_send(self, _sys: int, _comp: int, name: bytes, _index: int) -> None:
        self.calls.append(("param_request_read_send", (name,)))
        pname = name.decode()
        if self.bridge is not None and pname in self.answers:
            self.bridge._dispatch(param_value(pname, self.answers[pname]))

    def names(self, method: str) -> list[Any]:
        return [args for name, args in self.calls if name == method]


def param_value(name: str, value: float) -> FakeMessage:
    return FakeMessage(
        message_type="PARAM_VALUE", param_id=name.encode(), param_value=value,
        param_type=M.MAV_PARAM_TYPE_REAL32, param_index=0, param_count=0,
    )


def ack(command: int, result: int) -> FakeMessage:
    return FakeMessage(message_type="COMMAND_ACK", command=command, result=result,
                       target_system=0, target_component=0)


def ready_bridge() -> tuple[MavlinkBridge, RecordingMav]:
    store = VehicleStateStore()
    store.update(connected=True)
    store.heartbeat()
    bridge = MavlinkBridge(store)
    mav = RecordingMav(bridge)
    bridge._conn = SimpleNamespace(mav=mav, source_system=254, source_component=190)
    return bridge, mav


# ---------------------------------------------------------------------------
# The station speaks first
# ---------------------------------------------------------------------------

class SilentLink:
    """A link whose far end waits to be spoken to, and so never speaks."""

    def __init__(self) -> None:
        self.order: list[str] = []
        self.mav = SimpleNamespace(heartbeat_send=lambda *a: self.order.append("heartbeat"))
        self.target_system = 0
        self.target_component = 0

    def wait_heartbeat(self, blocking: bool = True, timeout: float = 1.0) -> None:
        self.order.append("wait")
        return None

    def close(self) -> None:
        self.order.append("close")


def test_the_gcs_heartbeat_goes_out_before_the_vehicle_is_heard(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    link = SilentLink()
    monkeypatch.setattr("corvus.mavlink_bridge.mavutil.mavlink_connection",
                        lambda *a, **k: link)
    bridge = MavlinkBridge(VehicleStateStore(), "udpout:127.0.0.1:18570")

    with pytest.raises(ConnectionError):
        bridge._connect()

    assert link.order[:2] == ["heartbeat", "wait"], \
        "a far end that waits for the station must hear it before we wait for it"


def test_remote_id_waits_until_the_vehicle_is_known() -> None:
    """The heartbeat now runs before the vehicle is found; the identity is
    addressed to the vehicle, so it waits."""
    bridge, mav = ready_bridge()
    bridge._store.update(connected=False)
    sent: list[Any] = []
    bridge._send_remote_id = lambda conn: sent.append(conn)  # type: ignore[method-assign]
    bridge._running.set()
    bridge._interruptible_sleep = lambda s: bridge._running.clear()  # type: ignore[method-assign]

    bridge._gcs_hb_loop()
    assert mav.names("heartbeat_send"), "the heartbeat itself goes out"
    assert sent == []

    bridge._store.update(connected=True)
    bridge._running.set()
    bridge._gcs_hb_loop()
    assert len(sent) == 1


# ---------------------------------------------------------------------------
# Calibration cancel
# ---------------------------------------------------------------------------

def test_a_cancel_is_confirmed_by_the_calibrations_own_ack() -> None:
    """PX4's Commander answers TEMPORARILY_REJECTED at once while its worker is
    busy; the calibration routine answers ACCEPTED when it sees the cancel."""
    bridge, mav = ready_bridge()

    def answer(command: int) -> None:
        bridge._dispatch(ack(command, M.MAV_RESULT_TEMPORARILY_REJECTED))
        bridge._dispatch(ack(command, M.MAV_RESULT_ACCEPTED))

    mav.on_command = answer
    assert bridge.cancel_calibration() is True
    assert len(mav.names("command_long_send")) == 1


def test_a_cancel_that_only_gets_the_interim_answer_still_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bridge, mav = ready_bridge()
    monkeypatch.setattr(MavlinkBridge, "CALIBRATION_CANCEL_ACK_S", 0.1)
    mav.on_command = lambda command: bridge._dispatch(
        ack(command, M.MAV_RESULT_TEMPORARILY_REJECTED))

    assert bridge.cancel_calibration() is False
    assert "TEMPORARILY_REJECTED" in bridge.get_last_command_error()
    assert len(mav.names("command_long_send")) == 1, "heard, so not sent again"


# ---------------------------------------------------------------------------
# Fly to points: the takeoff item
# ---------------------------------------------------------------------------

def _capture_fly_to(bridge: MavlinkBridge) -> list[dict[str, Any]]:
    uploaded: list[dict[str, Any]] = []

    def upload(items: list[dict[str, Any]]) -> int:
        uploaded.extend(items)
        return M.MAV_MISSION_ACCEPTED

    bridge._upload_mission = upload  # type: ignore[method-assign]
    bridge._send_command_and_wait = lambda *a, **k: M.MAV_RESULT_ACCEPTED  # type: ignore[method-assign]
    bridge._enter_mission_mode = lambda action: True  # type: ignore[method-assign]
    bridge.arm = lambda arm=True: True  # type: ignore[method-assign]
    return uploaded


POINTS = [{"lat": 48.1, "lon": 11.6, "alt_agl": 30.0}]


def test_the_takeoff_item_is_placed_at_the_aircraft() -> None:
    bridge, _mav = ready_bridge()
    bridge._store.update(position=[11.5, 48.05], home=[11.4, 48.0], landed_state=1)
    uploaded = _capture_fly_to(bridge)

    assert bridge.fly_to_points(POINTS) is True
    takeoff = uploaded[0]
    assert takeoff["command"] == M.MAV_CMD_NAV_TAKEOFF
    assert (takeoff["x_int"], takeoff["y_int"]) == (int(48.05 * 1e7), int(11.5 * 1e7))


def test_without_a_position_the_takeoff_falls_back_to_home_then_the_first_point() -> None:
    bridge, _mav = ready_bridge()
    bridge._store.update(position=[0.0, 0.0], home=[11.4, 48.0], landed_state=1)
    uploaded = _capture_fly_to(bridge)
    assert bridge.fly_to_points(POINTS) is True
    assert uploaded[0]["x_int"] == int(48.0 * 1e7)

    bridge, _mav = ready_bridge()
    bridge._store.update(position=[0.0, 0.0], home=[0.0, 0.0], landed_state=1)
    uploaded = _capture_fly_to(bridge)
    assert bridge.fly_to_points(POINTS) is True
    assert (uploaded[0]["x_int"], uploaded[0]["y_int"]) != (0, 0), "never 0/0"
    assert uploaded[0]["x_int"] == int(round(48.1 * 1e7))


# ---------------------------------------------------------------------------
# A vehicle that reboots under a live link
# ---------------------------------------------------------------------------

def system_time(boot_ms: int) -> FakeMessage:
    return FakeMessage(message_type="SYSTEM_TIME", time_unix_usec=0, time_boot_ms=boot_ms)


def test_a_reboot_under_a_live_link_drops_the_stale_parameter_state() -> None:
    bridge, mav = ready_bridge()
    console: list[dict[str, Any]] = []
    bridge.add_console_sub(console.append)
    bridge._dispatch(system_time(600_000))
    bridge._dispatch(param_value("BAT1_V_EMPTY", 3.5))
    bridge._param_download_state = "complete"

    bridge._dispatch(system_time(1_500))

    assert bridge.get_param("BAT1_V_EMPTY") is None
    assert bridge.param_status()["state"] == "idle"
    assert any("rebooted" in e["text"] for e in console)
    assert mav.names("command_long_send"), "version and home are asked for again"


def test_frames_out_of_order_are_not_a_reboot() -> None:
    bridge, _mav = ready_bridge()
    bridge._dispatch(system_time(600_000))
    bridge._dispatch(param_value("BAT1_V_EMPTY", 3.5))
    bridge._dispatch(system_time(599_000))
    assert bridge.get_param("BAT1_V_EMPTY") is not None


# ---------------------------------------------------------------------------
# Parameters the firmware does not have
# ---------------------------------------------------------------------------

@pytest.fixture
def fast_clock(monkeypatch: pytest.MonkeyPatch) -> None:
    clock = [1000.0]
    monkeypatch.setattr(time, "monotonic", lambda: clock[0])
    monkeypatch.setattr(time, "sleep", lambda s: clock.__setitem__(0, clock[0] + max(s, 0.01)))


def _reads(mav: RecordingMav) -> list[bytes]:
    return [args[0] for args in mav.names("param_request_read_send")]


def test_a_name_that_twice_goes_unanswered_is_not_asked_for_again(fast_clock: None) -> None:
    bridge, mav = ready_bridge()
    mav.answers = {"BAT1_V_EMPTY": 3.5}
    names = ["BAT1_V_EMPTY", "BAT_N_CELLS"]

    bridge.fetch_params(names, timeout=0.5)
    bridge.fetch_params(names, timeout=0.5)
    mav.calls.clear()
    result = bridge.fetch_params(names, timeout=0.5, fresh=True)

    assert result == {"BAT1_V_EMPTY": 3.5}
    assert b"BAT_N_CELLS" not in _reads(mav)
    assert b"BAT1_V_EMPTY" in _reads(mav), "fresh still reads what exists"


def test_a_read_nothing_answered_proves_nothing(fast_clock: None) -> None:
    """A dead link is not a firmware without parameters."""
    bridge, mav = ready_bridge()
    for _ in range(3):
        bridge.fetch_params(["BAT1_V_EMPTY"], timeout=0.5)
    mav.calls.clear()
    bridge.fetch_params(["BAT1_V_EMPTY"], timeout=0.5)
    assert b"BAT1_V_EMPTY" in _reads(mav)


def test_after_a_complete_download_a_missing_name_is_known_absent(fast_clock: None) -> None:
    bridge, mav = ready_bridge()
    bridge._dispatch(param_value("BAT1_V_EMPTY", 3.5))
    bridge._param_download_state = "complete"
    assert bridge.fetch_params(["BAT1_V_EMPTY", "BAT_N_CELLS"], timeout=0.5) == {
        "BAT1_V_EMPTY": 3.5}
    assert _reads(mav) == []


def test_a_name_the_firmware_lacks_does_not_cost_the_read_its_budget(
    fast_clock: None,
) -> None:
    """The first open of a setup page used to wait out all 4 s for it."""
    bridge, mav = ready_bridge()
    mav.answers = {"BAT1_V_EMPTY": 3.5, "BAT1_V_CHARGED": 4.2}
    start = time.monotonic()

    result = bridge.fetch_params(["BAT1_V_EMPTY", "BAT1_V_CHARGED", "BAT_N_CELLS"])

    assert result == {"BAT1_V_EMPTY": 3.5, "BAT1_V_CHARGED": 4.2}
    assert time.monotonic() - start < 0.5
    assert _reads(mav).count(b"BAT_N_CELLS") == 2, "the retransmit round still runs"
    assert bridge._param_misses == {"BAT_N_CELLS": 1}


def test_a_reply_lost_before_the_barrier_is_asked_for_again(fast_clock: None) -> None:
    bridge, mav = ready_bridge()
    mav.answers = {"BAT1_V_EMPTY": 3.5, "BAT1_V_CHARGED": 4.2}
    original = mav.param_request_read_send
    dropped: list[bytes] = []

    def lossy(sys_id: int, comp_id: int, name: bytes, index: int) -> None:
        if name == b"BAT1_V_CHARGED" and not dropped:
            dropped.append(name)
            mav.calls.append(("param_request_read_send", (name,)))
            return
        original(sys_id, comp_id, name, index)

    mav.param_request_read_send = lossy  # type: ignore[method-assign]

    assert bridge.fetch_params(["BAT1_V_EMPTY", "BAT1_V_CHARGED"]) == {
        "BAT1_V_EMPTY": 3.5, "BAT1_V_CHARGED": 4.2}
    assert bridge._param_misses == {}


def test_no_barrier_is_trusted_while_a_full_download_streams(fast_clock: None) -> None:
    """The burst carries every name, so a barrier's answer proves nothing."""
    bridge, mav = ready_bridge()
    mav.answers = {"BAT1_V_EMPTY": 3.5}
    bridge._param_download_state = "downloading"
    start = time.monotonic()

    bridge.fetch_params(["BAT1_V_EMPTY", "BAT_N_CELLS"], timeout=2.0)

    assert time.monotonic() - start >= 2.0
    assert _reads(mav) == [b"BAT1_V_EMPTY", b"BAT_N_CELLS", b"BAT_N_CELLS"]


def test_a_name_that_turns_up_after_all_is_believed(fast_clock: None) -> None:
    bridge, mav = ready_bridge()
    mav.answers = {"BAT1_V_EMPTY": 3.5}
    for _ in range(2):
        bridge.fetch_params(["BAT1_V_EMPTY", "SENS_EN_SF1XX"], timeout=0.5)
    bridge._dispatch(param_value("SENS_EN_SF1XX", 1.0))
    mav.answers["SENS_EN_SF1XX"] = 1.0
    assert bridge.fetch_params(["SENS_EN_SF1XX"], timeout=0.5, fresh=True) == {
        "SENS_EN_SF1XX": 1.0}


# ---------------------------------------------------------------------------
# Small things reported as what they are
# ---------------------------------------------------------------------------

def test_a_fresh_pack_reports_zero_mah_not_nothing() -> None:
    bridge, _mav = ready_bridge()
    bridge._dispatch(FakeMessage(
        message_type="BATTERY_STATUS", id=0, voltages=[65535] * 10, voltages_ext=[],
        current_consumed=0, temperature=32767, time_remaining=0))
    assert bridge._store.get_snapshot().get("battery_consumed_mah") == 0.0


def test_a_statustext_that_doubles_an_event_loses_its_tab() -> None:
    bridge, _mav = ready_bridge()
    console: list[dict[str, Any]] = []
    bridge.add_console_sub(console.append)
    bridge._dispatch(FakeMessage(
        message_type="STATUSTEXT", text="Arming denied: Resolve system health failures first\t",
        severity=M.MAV_SEVERITY_CRITICAL, id=0, chunk_seq=0))
    texts = [e["text"] for e in console if e["name"] == "STATUSTEXT"]
    assert texts == ["Arming denied: Resolve system health failures first"]
