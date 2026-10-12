"""First setup: telling a fresh controller from a configured one.

Three layers, each tested where it decides:

  * ``corvus.first_setup``: the facts read from parameters, and the one table
    of airframes the wizard may write (``assess``, ``writes_for``).
  * ``/api/first-setup`` and ``/api/first-setup/airframe``: always 200 on a
    read, nothing written that the table did not name, nothing written armed.
  * A board with no firmware: the watcher publishes ``link_bootloader`` and the
    flash service flashes that port without a reboot, but only when no vehicle
    is linked and exactly one board is waiting.
"""
from __future__ import annotations

from typing import Any

import pytest

from corvus import autoconnect as ac
from corvus import first_setup
from corvus.state_store import VehicleStateStore

PX4 = "px4"
ARDUPILOT = "ardupilot"


def _px4_values(**over: float) -> dict[str, float]:
    values = {
        "SYS_AUTOSTART": 4001.0, "CAL_ACC0_ID": 1.0, "CAL_GYRO0_ID": 2.0,
        "CAL_MAG0_ID": 3.0, "SYS_HAS_MAG": 1.0, "SYS_AUTOCONFIG": 0.0,
    }
    values.update(over)
    return values


def _ardupilot_values(**over: float) -> dict[str, float]:
    values = {
        "FRAME_CLASS": 1.0, "FRAME_TYPE": 1.0,
        "INS_ACCOFFS_X": 0.1, "INS_ACCOFFS_Y": -0.2, "INS_ACCOFFS_Z": 0.05,
        "COMPASS_OFS_X": 12.0, "COMPASS_OFS_Y": -3.0, "COMPASS_OFS_Z": 40.0,
        "COMPASS_DEV_ID": 97539.0, "COMPASS_USE": 1.0,
    }
    values.update(over)
    return values


# ---- assess: PX4 ----

def test_a_wiped_px4_board_over_usb_is_fresh_and_prompts() -> None:
    values = _px4_values(SYS_AUTOSTART=0, CAL_ACC0_ID=0, CAL_GYRO0_ID=0, CAL_MAG0_ID=0)
    out = first_setup.assess(PX4, "Copter", "usb", values)

    assert out["needs_setup"] is True
    assert out["prompt"] is True
    assert out["facts"] == {"airframe": "todo", "accel": "todo", "gyro": "todo", "mag": "todo"}
    assert "No airframe is selected." in out["reasons"]
    assert "No sensor has been calibrated." in out["reasons"]


def test_a_configured_px4_board_is_left_alone() -> None:
    out = first_setup.assess(PX4, "Copter", "usb", _px4_values())

    assert out["needs_setup"] is False
    assert out["prompt"] is False
    assert out["preset_current"] == 4001


def test_one_missing_calibration_is_incomplete_not_fresh() -> None:
    """The wizard shows it; an unprompted dialog on every connect would be nagging."""
    out = first_setup.assess(PX4, "Copter", "usb", _px4_values(CAL_MAG0_ID=0))

    assert out["facts"]["mag"] == "todo"
    assert out["needs_setup"] is False


def test_a_simulator_is_never_prompted_even_when_it_looks_fresh() -> None:
    values = _px4_values(SYS_AUTOSTART=0, CAL_ACC0_ID=0, CAL_GYRO0_ID=0, CAL_MAG0_ID=0)
    for transport in ("udp", "tcp", "unknown"):
        out = first_setup.assess(PX4, "Copter", transport, values)
        assert out["needs_setup"] is True
        assert out["prompt"] is False


def test_a_telemetry_radio_may_prompt() -> None:
    values = _px4_values(SYS_AUTOSTART=0)
    assert first_setup.assess(PX4, "Copter", "sik", values)["prompt"] is True


def test_a_board_without_a_compass_does_not_need_one_calibrated() -> None:
    out = first_setup.assess(PX4, "Copter", "usb", _px4_values(SYS_HAS_MAG=0, CAL_MAG0_ID=0))

    assert out["facts"]["mag"] == "not_required"
    assert out["needs_setup"] is False


def test_parameters_the_firmware_lacks_are_unknown_and_never_prompt() -> None:
    out = first_setup.assess(PX4, "Copter", "usb", {})

    assert set(out["facts"].values()) == {"unknown"}
    assert out["needs_setup"] is False
    assert out["prompt"] is False


# ---- assess: ArduPilot ----

def test_a_configured_ardupilot_copter_is_left_alone() -> None:
    out = first_setup.assess(ARDUPILOT, "Copter", "usb", _ardupilot_values())

    assert out["needs_setup"] is False
    assert out["facts"]["gyro"] == "not_required"
    assert out["preset_current"] == 101


def test_an_ardupilot_board_with_zero_offsets_is_fresh() -> None:
    values = _ardupilot_values(
        INS_ACCOFFS_X=0, INS_ACCOFFS_Y=0, INS_ACCOFFS_Z=0,
        COMPASS_OFS_X=0, COMPASS_OFS_Y=0, COMPASS_OFS_Z=0,
    )
    out = first_setup.assess(ARDUPILOT, "Copter", "usb", values)

    assert out["facts"]["accel"] == "todo"
    assert out["facts"]["mag"] == "todo"
    assert out["needs_setup"] is True
    assert out["reasons"] == ["No sensor has been calibrated."]


def test_ardupilot_without_a_compass_judges_the_accelerometer_alone() -> None:
    values = _ardupilot_values(COMPASS_DEV_ID=0, COMPASS_OFS_X=0, COMPASS_OFS_Y=0, COMPASS_OFS_Z=0)
    out = first_setup.assess(ARDUPILOT, "Copter", "usb", values)

    assert out["facts"]["mag"] == "not_required"
    assert out["needs_setup"] is False


def test_the_parameter_names_follow_the_stack() -> None:
    assert "SYS_AUTOSTART" in first_setup.param_names(PX4)
    assert "FRAME_CLASS" in first_setup.param_names(ARDUPILOT)
    assert "FRAME_CLASS" not in first_setup.param_names(PX4)
    # A stack Corvus does not recognise gets PX4's, like every setup page.
    assert first_setup.param_names("generic") == first_setup.param_names(PX4)


# ---- the airframe table ----

def test_px4_offers_the_generic_airframes_and_writes_the_id_then_autoconfig() -> None:
    ids = {p["id"] for p in first_setup.presets(PX4, "Copter")}
    assert {4001, 5001, 6001, 8001, 2100, 3000, 13000, 50000, 51000} <= ids

    assert first_setup.writes_for(PX4, "Copter", 4001) == [
        ("SYS_AUTOSTART", 4001.0), ("SYS_AUTOCONFIG", 1.0),
    ]


def test_px4_refuses_an_id_that_is_not_in_the_table() -> None:
    assert first_setup.writes_for(PX4, "Copter", 4999) is None
    assert first_setup.writes_for(PX4, "Copter", 0) is None


def test_ardupilot_copter_writes_frame_class_and_type() -> None:
    assert first_setup.writes_for(ARDUPILOT, "Copter", 201) == [
        ("FRAME_CLASS", 2.0), ("FRAME_TYPE", 1.0),
    ]
    assert first_setup.writes_for(ARDUPILOT, "Copter", 4001) is None


def test_ardupilot_plane_and_rover_choose_their_type_by_firmware() -> None:
    assert first_setup.presets(ARDUPILOT, "Plane") == []
    assert first_setup.presets(ARDUPILOT, "Rover") == []
    assert first_setup.writes_for(ARDUPILOT, "Plane", 101) is None


def test_the_public_table_never_carries_the_writes() -> None:
    for preset in first_setup.presets(ARDUPILOT, "Copter"):
        assert "writes" not in preset
        assert {"id", "group", "label"} <= set(preset)


# ---- the endpoints ----

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")
from corvus.server import CorvusHandler  # noqa: E402


class FakeBridge:
    def __init__(self, values: dict[str, float] | None = None, stack: str = PX4,
                 vehicle_type_id: int = 2, transport: str = "usb",
                 set_results: list[bool] | None = None, error: str = "",
                 raises: bool = False) -> None:
        self.values = dict(values or {})
        self.stack = stack
        self.vehicle_type_id = vehicle_type_id
        self._transport = transport
        self.set_results = list(set_results or [])
        self.error = error
        self.raises = raises
        self.set_calls: list[tuple[str, float]] = []

    def transport(self) -> str:
        return self._transport

    def fetch_params(self, names: list[str], **_kw: Any) -> dict[str, float]:
        if self.raises:
            raise RuntimeError("link dropped")
        return {n: v for n, v in self.values.items() if n in set(names)}

    def board_identity(self) -> dict[str, Any]:
        return {"device": "/dev/ttyACM0", "description": "Pixhawk",
                "hwid": "USB VID:PID=26AC:0032 SER=0001", "vendor_id": 9900, "product_id": 50}

    def set_param(self, name: str, value: float) -> bool:
        self.set_calls.append((name, value))
        ok = self.set_results.pop(0) if self.set_results else True
        if ok:
            self.values[name] = value
        return ok

    def get_last_command_error(self) -> str:
        return self.error


def _handler(bridge: Any, armed: bool = False) -> tuple[CorvusHandler, list[tuple[dict, int]]]:
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge  # type: ignore[assignment]
    store = VehicleStateStore()
    store.update(armed=armed)
    handler.store = store  # type: ignore[assignment]
    responses: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: responses.append((data, status))  # type: ignore[method-assign]
    return handler, responses


def test_the_read_reports_a_fresh_board_with_a_fingerprint() -> None:
    bridge = FakeBridge(_px4_values(SYS_AUTOSTART=0, CAL_ACC0_ID=0, CAL_GYRO0_ID=0, CAL_MAG0_ID=0))
    handler, responses = _handler(bridge)

    handler._api_first_setup()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is True
    assert payload["prompt"] is True
    assert payload["fingerprint"].startswith("px4|9900|50|26AC:0032/0001")
    assert any(p["id"] == 4001 for p in payload["presets"])


def test_a_vehicle_that_answers_nothing_is_not_connected_and_not_a_prompt() -> None:
    handler, responses = _handler(FakeBridge({}))

    handler._api_first_setup()

    payload, status = responses[0]
    assert status == 200
    assert payload["connected"] is False
    assert payload["prompt"] is False
    assert payload["needs_setup"] is False


def test_a_read_that_raises_still_answers_200() -> None:
    handler, responses = _handler(FakeBridge({}, raises=True))

    handler._api_first_setup()

    assert responses[0][1] == 200
    assert responses[0][0]["connected"] is False


def test_with_no_bridge_the_read_answers_quietly() -> None:
    handler, responses = _handler(None)

    handler._api_first_setup()

    assert responses[0] == ({"connected": False, "needs_setup": False, "prompt": False}, 200)


def test_choosing_a_px4_airframe_writes_the_id_and_autoconfig() -> None:
    bridge = FakeBridge(_px4_values(SYS_AUTOSTART=0))
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 4001})

    assert bridge.set_calls == [("SYS_AUTOSTART", 4001.0), ("SYS_AUTOCONFIG", 1.0)]
    payload, status = responses[0]
    assert status == 200
    assert payload == {"ok": True, "written": ["SYS_AUTOSTART", "SYS_AUTOCONFIG"], "reboot": True}


def test_a_failed_autoconfig_is_a_warning_and_keeps_the_airframe() -> None:
    bridge = FakeBridge(_px4_values(SYS_AUTOSTART=0), set_results=[True, False],
                        error="parameter not found")
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 4001})

    payload, status = responses[0]
    assert status == 200
    assert payload["ok"] is True
    assert payload["written"] == ["SYS_AUTOSTART"]
    assert payload["warning"] == "parameter not found"


def test_a_firmware_without_autoconfig_just_gets_the_airframe() -> None:
    values = _px4_values(SYS_AUTOSTART=0)
    del values["SYS_AUTOCONFIG"]
    bridge = FakeBridge(values)
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 4001})

    assert bridge.set_calls == [("SYS_AUTOSTART", 4001.0)]
    assert responses[0][0] == {"ok": True, "written": ["SYS_AUTOSTART"], "reboot": True}


def test_a_failed_airframe_write_is_refused_and_nothing_else_is_tried() -> None:
    bridge = FakeBridge(_px4_values(), set_results=[False], error="timeout")
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 4001})

    assert bridge.set_calls == [("SYS_AUTOSTART", 4001.0)]
    assert responses[0] == ({"ok": False, "error": "timeout"}, 409)


def test_an_airframe_is_not_written_while_armed() -> None:
    bridge = FakeBridge(_px4_values())
    handler, responses = _handler(bridge, armed=True)

    handler._api_first_setup_airframe({"preset": 4001})

    assert bridge.set_calls == []
    assert responses[0][1] == 409


@pytest.mark.parametrize("preset", ["4001", 4001.5, None, True, 4999, 0, -1])
def test_a_preset_the_table_does_not_name_writes_nothing(preset: Any) -> None:
    bridge = FakeBridge(_px4_values())
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": preset})

    assert bridge.set_calls == []
    assert responses[0][1] == 400


def test_an_ardupilot_copter_frame_is_written_as_class_and_type() -> None:
    bridge = FakeBridge(_ardupilot_values(), stack=ARDUPILOT, vehicle_type_id=2)
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 301})

    assert bridge.set_calls == [("FRAME_CLASS", 3.0), ("FRAME_TYPE", 1.0)]
    assert responses[0][0]["ok"] is True


def test_an_ardupilot_plane_has_no_frame_to_write() -> None:
    bridge = FakeBridge(_ardupilot_values(), stack=ARDUPILOT, vehicle_type_id=1)
    handler, responses = _handler(bridge)

    handler._api_first_setup_airframe({"preset": 101})

    assert bridge.set_calls == []
    assert responses[0][1] == 400


def test_writing_with_no_bridge_is_a_503() -> None:
    handler, responses = _handler(None)

    handler._api_first_setup_airframe({"preset": 4001})

    assert responses[0][1] == 503


# ---- a board with no firmware ----

BOOTLOADER_PORT = {
    "device": "/dev/ttyACM0", "description": "PX4 BL FMU v6X.x",
    "hwid": "USB VID:PID=26AC:0011",
}
SECOND_BOOTLOADER = {
    "device": "/dev/ttyACM1", "description": "PX4 BL FMU v5.x",
    "hwid": "USB VID:PID=26AC:0011 SER=2",
}
FC_PORT = {
    "device": "/dev/ttyACM0", "description": "Pixhawk FMU v6X",
    "hwid": "USB VID:PID=26AC:0032 SER=0001",
}


class _Bridge:
    def is_running(self) -> bool:
        return True

    def connection_string(self) -> str:
        return "udp:0.0.0.0:14550"

    def stop(self) -> None: ...
    def set_connection(self, conn: str) -> None: ...
    def start(self) -> None: ...


def _watcher(store: VehicleStateStore, rows: list[dict]) -> ac.AutoConnectWatcher:
    store.heartbeat()
    return ac.AutoConnectWatcher(
        bridge=_Bridge(),  # type: ignore[arg-type]
        store=store, session=ac.SessionState(), list_ports_fn=lambda: list(rows),
    )


def test_the_watcher_publishes_a_board_in_its_bootloader() -> None:
    store = VehicleStateStore()
    watcher = _watcher(store, [BOOTLOADER_PORT])

    watcher.tick()

    assert store.get_snapshot()["link_bootloader"] == {
        "device": "/dev/ttyACM0", "description": "PX4 BL FMU v6X.x",
    }


def test_the_bootloader_is_cleared_when_the_board_leaves() -> None:
    store = VehicleStateStore()
    rows = [BOOTLOADER_PORT]
    watcher = ac.AutoConnectWatcher(
        bridge=_Bridge(),  # type: ignore[arg-type]
        store=store, session=ac.SessionState(), list_ports_fn=lambda: list(rows),
    )
    store.heartbeat()
    watcher.tick()
    rows[:] = [FC_PORT]
    watcher.tick()

    assert store.get_snapshot()["link_bootloader"] is None


def test_an_ordinary_flight_controller_is_not_a_bootloader() -> None:
    store = VehicleStateStore()
    _watcher(store, [FC_PORT]).tick()

    assert store.get_snapshot()["link_bootloader"] is None


def test_the_bootloader_is_published_once_per_appearance() -> None:
    store = VehicleStateStore()
    published: list[Any] = []
    real_update = store.update

    def counting(**kwargs: Any) -> None:
        if "link_bootloader" in kwargs:
            published.append(kwargs["link_bootloader"])
        real_update(**kwargs)

    store.update = counting  # type: ignore[method-assign]
    watcher = _watcher(store, [BOOTLOADER_PORT])

    watcher.tick()
    watcher.tick()
    watcher.tick()

    assert len(published) == 1


class _FlashMavlink:
    def __init__(self) -> None:
        self.reboots = 0
        self.stopped = False
        self.started = False
        self.connection_set_to: str | None = None

    def transport(self) -> str:
        return "udp"

    def is_direct_usb(self) -> bool:
        return False

    def serial_device(self) -> str:
        return ""

    def connection_string(self) -> str:
        return "udp:0.0.0.0:14550"

    def reboot_to_bootloader(self) -> bool:
        self.reboots += 1
        return True

    def stop(self) -> None:
        self.stopped = True

    def set_connection(self, conn: str) -> None:
        self.connection_set_to = conn

    def start(self) -> None:
        self.started = True


class _Uploader:
    ran: list[tuple[str, int, Any]] = []

    def __init__(self, on_progress: Any = None, cancel: Any = None) -> None: ...

    def run(self, device: str, image: bytes, board_id: Any = None) -> bool:
        _Uploader.ran.append((device, len(image), board_id))
        return True

    def shutdown(self) -> None: ...


def _flash(rows: list[dict], connected: bool = False, mav: Any = None):
    from corvus.flash_service import FlashService
    store = VehicleStateStore()
    store.update(connected=connected)
    return FlashService(mav or _FlashMavlink(), store, list_ports_fn=lambda: list(rows))


def test_a_lone_bootloader_board_with_no_link_can_be_flashed() -> None:
    fs = _flash([BOOTLOADER_PORT])

    status = fs.status()

    assert status["can_flash"] is True
    assert status["bootloader"] == "/dev/ttyACM0"


def test_no_bootloader_board_means_the_usual_gate() -> None:
    fs = _flash([])

    assert fs.status()["can_flash"] is False
    assert fs.status()["bootloader"] == ""
    assert fs.start(b"\x00" * 64) is False
    assert "direct USB" in fs.last_error


def test_two_boards_in_their_bootloaders_are_never_guessed_between() -> None:
    fs = _flash([BOOTLOADER_PORT, SECOND_BOOTLOADER])

    assert fs.status()["bootloader"] == ""
    assert fs.status()["can_flash"] is False


def test_a_linked_vehicle_takes_the_normal_path_even_with_a_bootloader_port() -> None:
    fs = _flash([BOOTLOADER_PORT], connected=True)

    assert fs.status()["bootloader"] == ""
    assert fs.status()["can_flash"] is False


def test_a_blank_board_is_flashed_without_a_reboot(monkeypatch: pytest.MonkeyPatch) -> None:
    import corvus.flash_service as flash_service
    _Uploader.ran = []
    monkeypatch.setattr(flash_service, "FirmwareUploader", _Uploader)
    mav = _FlashMavlink()
    fs = _flash([BOOTLOADER_PORT], mav=mav)

    assert fs.start(b"\x00" * 64) is True
    fs._worker.join(timeout=5)

    assert mav.reboots == 0
    assert mav.stopped is True
    assert _Uploader.ran == [("/dev/ttyACM0", 64, None)]
    assert fs.status()["state"] == "done"
    assert mav.started is True
    fs.shutdown()


def test_a_flash_resumes_only_while_the_same_board_still_waits() -> None:
    fs = _flash([BOOTLOADER_PORT])

    assert fs._gate_for_resume("/dev/ttyACM0") is None
    assert fs._gate_for_resume("/dev/ttyACM7") == "the controller left its bootloader"
