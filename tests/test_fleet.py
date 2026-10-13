"""Several vehicles at once (corvus/fleet.py and its hooks in the bridge).

The first block pins the promise the feature was built around: one vehicle on
one link connects, latches and is flown exactly as before, and the vehicle
list never sends that vehicle anything.
"""
from __future__ import annotations

import time
from types import SimpleNamespace

import pytest
from pymavlink import mavutil

from corvus import fleet as fleet_mod
from corvus.fleet import (
    PALETTE,
    ActiveBridge,
    ActiveStore,
    Fleet,
    FleetTracker,
    RtcmFanout,
    coerce_config,
    parse_vehicle_key,
)
from corvus.mavlink_bridge import (
    MAV_TYPE_MAP,
    MavlinkBridge,
    _VehicleSwitch,
)
from corvus.state_store import VehicleStateStore


def _from(msg, system: int, component: int = 1):
    msg.pack(mavutil.mavlink.MAVLink(None, srcSystem=system, srcComponent=component))
    return msg


def _heartbeat(system: int, *, autopilot=None, vtype=None, armed=False, custom=(3 << 16),
               component: int = 1):
    base_mode = mavutil.mavlink.MAV_MODE_FLAG_CUSTOM_MODE_ENABLED
    if armed:
        base_mode |= mavutil.mavlink.MAV_MODE_FLAG_SAFETY_ARMED
    msg = mavutil.mavlink.MAVLink_heartbeat_message(
        type=mavutil.mavlink.MAV_TYPE_QUADROTOR if vtype is None else vtype,
        autopilot=mavutil.mavlink.MAV_AUTOPILOT_PX4 if autopilot is None else autopilot,
        base_mode=base_mode, custom_mode=custom, system_status=4, mavlink_version=3,
    )
    return _from(msg, system, component)


def _gcs_heartbeat(system: int = 255):
    msg = mavutil.mavlink.MAVLink_heartbeat_message(
        type=mavutil.mavlink.MAV_TYPE_GCS, autopilot=mavutil.mavlink.MAV_AUTOPILOT_INVALID,
        base_mode=0, custom_mode=0, system_status=4, mavlink_version=3,
    )
    return _from(msg, system, 190)


def _position(system: int, lat=48.08, lon=11.64, rel=12.5, hdg=9000, component: int = 1):
    msg = mavutil.mavlink.MAVLink_global_position_int_message(
        time_boot_ms=1000, lat=int(lat * 1e7), lon=int(lon * 1e7), alt=600000,
        relative_alt=int(rel * 1000), vx=300, vy=400, vz=0, hdg=hdg,
    )
    return _from(msg, system, component)


class _SentRecorder:
    """A conn whose mav records what the bridge sends, and to whom."""

    def __init__(self) -> None:
        self.sent: list[tuple] = []
        recorder = self

        class _Mav:
            def command_long_send(self, target_system, target_component, command, *rest):
                recorder.sent.append(("command_long", target_system, command))

            def request_data_stream_send(self, target_system, target_component, stream, rate, on):
                recorder.sent.append(("request_data_stream", target_system, stream))

        self.mav = _Mav()


@pytest.fixture()
def bridge():
    store = VehicleStateStore()
    b = MavlinkBridge(store, "udp:0.0.0.0:14550")
    b._target_system = 1
    b._target_component = 1
    return b


# ---------------------------------------------------------------------------
# One vehicle: nothing changes
# ---------------------------------------------------------------------------

def test_one_vehicle_latches_the_first_autopilot_as_before(bridge) -> None:
    served = [_gcs_heartbeat(), _heartbeat(1)]
    bridge._conn = SimpleNamespace(
        wait_heartbeat=lambda blocking=True, timeout=None: served.pop(0) if served else None,
    )
    hb = bridge._wait_vehicle_heartbeat(5.0)
    assert hb is not None and hb.get_srcSystem() == 1
    assert bridge.preferred_system() is None


def test_the_vehicle_being_flown_is_never_sent_anything_by_the_list(bridge) -> None:
    conn = _SentRecorder()
    bridge._conn = conn
    for _ in range(3):
        bridge._dispatch(_heartbeat(1))
    entry = bridge.fleet_tracker._systems[1]
    entry["first_heartbeat"] -= 60
    for _ in range(5):
        bridge._dispatch(_heartbeat(1))
    assert conn.sent == []


def test_a_dispatch_that_the_tracker_cannot_read_still_reaches_the_store(bridge) -> None:
    bridge.fleet_tracker = None  # anything that throws
    bridge._dispatch(_position(1))
    assert bridge._store.get_snapshot()["position"] == [11.64, 48.08]


def test_the_store_names_the_vehicle_it_is_about(bridge, monkeypatch) -> None:
    from corvus import mavlink_bridge as mb

    class FakeConn:
        target_system = 1
        target_component = 1
        mav = SimpleNamespace(
            request_data_stream_send=lambda *a, **k: None,
            command_long_send=lambda *a, **k: None,
        )

        def wait_heartbeat(self, blocking=True, timeout=None):
            return _heartbeat(4)

        def mode_mapping(self):
            return {}

    monkeypatch.setattr(mb.mavutil, "mavlink_connection", lambda *a, **k: FakeConn())
    monkeypatch.setattr(bridge, "_start_gcs_heartbeat", lambda: None)
    monkeypatch.setattr(bridge, "_request_version", lambda: None)
    bridge._connect()
    assert bridge._store.get_snapshot()["vehicle_id"] == "main:4"
    bridge._store.set_disconnected()
    assert bridge._store.get_snapshot()["vehicle_id"] == "main:4", (
        "a dropped link is the same aircraft until another one is heard"
    )


# ---------------------------------------------------------------------------
# Choosing another vehicle on the same link
# ---------------------------------------------------------------------------

def test_a_preferred_system_is_waited_for_past_the_others(bridge) -> None:
    served = [_heartbeat(1), _heartbeat(3), _heartbeat(2)]
    bridge._conn = SimpleNamespace(
        wait_heartbeat=lambda blocking=True, timeout=None: served.pop(0) if served else None,
    )
    bridge._preferred_system = 2
    hb = bridge._wait_vehicle_heartbeat(5.0)
    assert hb is not None and hb.get_srcSystem() == 2
    # ...and the ones it passed over are in the vehicle list.
    assert {s["sysid"] for s in bridge.fleet_tracker.systems()} == {1, 2, 3}


def test_a_preferred_system_that_never_speaks_is_not_replaced_by_another(bridge) -> None:
    bridge._conn = SimpleNamespace(
        wait_heartbeat=lambda blocking=True, timeout=None: _heartbeat(1),
    )
    bridge._preferred_system = 2
    assert bridge._wait_vehicle_heartbeat(0.3) is None


@pytest.mark.parametrize("bad", [0, 256, -1, True, "2", 2.0, None])
def test_select_system_rejects_what_is_not_a_system_id(bridge, bad) -> None:
    with pytest.raises(ValueError):
        bridge.select_system(bad)
    assert bridge.preferred_system() is None


def test_select_system_ends_the_cycle_through_the_receive_loop(bridge) -> None:
    bridge._running.set()
    bridge._store.heartbeat()

    class Conn:
        def recv_match(self, blocking=True, timeout=1):
            return None

    bridge._conn = Conn()
    bridge.select_system(2)
    with pytest.raises(_VehicleSwitch):
        bridge._receive_loop()
    assert not bridge._switch_requested.is_set()


def test_selecting_the_vehicle_already_flown_does_nothing(bridge) -> None:
    bridge._running.set()
    bridge._conn = object()
    bridge._store.heartbeat()
    bridge.select_system(1)
    assert not bridge._switch_requested.is_set()


def test_a_switch_is_not_a_link_failure(bridge, monkeypatch) -> None:
    """No error on the LINK tab and no reconnect backoff for an operator's choice."""
    cycles = []

    def fake_connect():
        cycles.append(time.monotonic())
        if len(cycles) == 1:
            bridge._target_system = 1
            return
        bridge._running.clear()
        raise ConnectionError("done")

    def fake_receive():
        raise _VehicleSwitch("Switching from system 1 to 2")

    slept = []
    monkeypatch.setattr(bridge, "_connect", fake_connect)
    monkeypatch.setattr(bridge, "_receive_loop", fake_receive)
    monkeypatch.setattr(bridge, "_schedule_message_intervals", lambda: None)
    monkeypatch.setattr(bridge, "_interruptible_sleep", lambda s: slept.append(s))
    bridge._running.set()
    bridge._run()
    assert len(cycles) == 2
    assert slept[0] == pytest.approx(0.3)
    assert bridge._reconnect_attempt == 1


def test_pointing_the_link_elsewhere_forgets_the_choice(bridge) -> None:
    bridge._preferred_system = 3
    bridge.fleet_tracker.observe(_heartbeat(3), vehicle_heartbeat=True)
    bridge.set_connection("udp:0.0.0.0:14550")
    assert bridge.preferred_system() == 3, "the same link keeps it"
    bridge.set_connection("udp:0.0.0.0:14560")
    assert bridge.preferred_system() is None
    assert bridge.fleet_tracker.systems() == []


def test_a_quiet_ardupilot_on_the_same_link_is_asked_for_its_position(bridge) -> None:
    conn = _SentRecorder()
    bridge._conn = conn
    ardupilot = mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA
    bridge._dispatch(_heartbeat(2, autopilot=ardupilot, custom=0))
    bridge.fleet_tracker._systems[2]["first_heartbeat"] -= 10
    bridge._dispatch(_heartbeat(2, autopilot=ardupilot, custom=0))
    targets = {entry[1] for entry in conn.sent}
    assert targets == {2}
    kinds = [entry[0] for entry in conn.sent]
    assert "command_long" in kinds and "request_data_stream" in kinds
    # Bounded: a vehicle without GPS is not asked forever.
    for _ in range(20):
        bridge.fleet_tracker._systems[2]["last_nudge"] -= 60
        bridge._dispatch(_heartbeat(2, autopilot=ardupilot, custom=0))
    assert len([e for e in conn.sent if e[0] == "command_long"]) == fleet_mod.NUDGE_MAX


def test_a_vehicle_that_streams_its_position_is_not_asked(bridge) -> None:
    conn = _SentRecorder()
    bridge._conn = conn
    bridge._dispatch(_heartbeat(2))
    bridge._dispatch(_position(2))
    bridge.fleet_tracker._systems[2]["first_heartbeat"] -= 10
    bridge._dispatch(_heartbeat(2))
    assert conn.sent == []


# ---------------------------------------------------------------------------
# The tracker
# ---------------------------------------------------------------------------

def test_tracker_lists_every_autopilot_and_ignores_the_rest() -> None:
    t = FleetTracker(MAV_TYPE_MAP)
    t.observe(_heartbeat(1, armed=True), vehicle_heartbeat=True)
    t.observe(_heartbeat(2), vehicle_heartbeat=True)
    t.observe(_gcs_heartbeat(), vehicle_heartbeat=False)
    t.observe(_position(1))
    t.observe(_position(3))               # never heartbeated: not a vehicle
    t.observe(_position(2, component=191))  # a companion, not the autopilot
    systems = {s["sysid"]: s for s in t.systems()}
    assert set(systems) == {1, 2}
    one = systems[1]
    assert one["armed"] is True
    assert one["type"] == "QUADROTOR"
    assert one["stack"] == "px4"
    assert one["mode"]
    assert one["position"] == [11.64, 48.08]
    assert one["altitude_rel"] == 12.5
    assert one["heading"] == 90.0
    assert one["groundspeed"] == 5.0
    assert systems[2]["position"] is None


def test_tracker_marks_silent_vehicles_lost_then_forgets_them() -> None:
    t = FleetTracker()
    t.observe(_heartbeat(1), vehicle_heartbeat=True, now=100.0)
    assert t.systems(now=101.0)[0]["stale"] is False
    assert t.systems(now=100.0 + fleet_mod.STALE_S + 1)[0]["stale"] is True
    assert t.systems(now=100.0 + fleet_mod.FORGET_S + 1) == []


def test_tracker_keeps_the_flight_controller_over_a_second_autopilot_looking_node() -> None:
    t = FleetTracker()
    t.observe(_heartbeat(1, armed=True), vehicle_heartbeat=True, now=10.0)
    t.observe(_heartbeat(1, armed=False, component=191), vehicle_heartbeat=True, now=10.5)
    assert t.systems(now=11.0)[0]["armed"] is True


def test_tracker_is_bounded() -> None:
    t = FleetTracker()
    for system in range(1, 200):
        t.observe(_heartbeat(system), vehicle_heartbeat=True)
    assert len(t.systems()) == fleet_mod.MAX_SYSTEMS_PER_LINK


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

def test_config_block_is_bounded_and_typed() -> None:
    raw = {
        "links": [
            {"id": "link-2", "connection": "serial:/dev/ttyUSB1:57600"},
            {"id": "link-2", "connection": "udp:0.0.0.0:14560"},     # duplicate id
            {"id": "link-3", "connection": "serial:/dev/ttyUSB1:57600"},  # duplicate port
            {"id": "evil", "connection": "udp:0.0.0.0:1"},
            {"id": "link-4"},
            "nonsense",
        ],
        "vehicles": {
            "main:1": {"name": "  Scout   one ", "color": "#2f7df6"},
            "main:2": {"name": "x" * 200, "color": "red"},
            "main:999": {"name": "nope"},
            "other": {"name": "nope"},
            "main:3": {},
        },
        "theme_vehicle": "main:1",
    }
    out = coerce_config(raw)
    assert out["links"] == [{"id": "link-2", "connection": "serial:/dev/ttyUSB1:57600"}]
    assert out["vehicles"]["main:1"] == {"name": "Scout one", "color": "#2F7DF6"}
    assert out["vehicles"]["main:2"] == {"name": "x" * fleet_mod.MAX_NAME_CHARS}
    assert set(out["vehicles"]) == {"main:1", "main:2"}
    assert out["theme_vehicle"] == "main:1"
    assert coerce_config(None) is None
    assert coerce_config({"links": "x", "vehicles": []}) is None


def test_config_round_trips_through_the_file(tmp_path) -> None:
    from corvus.config import load_config, save_config, CorvusConfig
    path = tmp_path / "config.json"
    cfg = CorvusConfig()
    cfg.fleet = {"vehicles": {"main:2": {"name": "Two"}}}
    save_config(cfg, str(path))
    assert load_config(str(path)).fleet == {"vehicles": {"main:2": {"name": "Two"}}}


@pytest.mark.parametrize("key,expected", [
    ("main:1", ("main", 1)),
    ("link-12:255", ("link-12", 255)),
    ("main:0", None),
    ("main:256", None),
    ("link:1", None),
    (7, None),
])
def test_vehicle_keys(key, expected) -> None:
    assert parse_vehicle_key(key) == expected


# ---------------------------------------------------------------------------
# The proxies the HTTP layer talks through
# ---------------------------------------------------------------------------

def test_active_store_moves_its_listeners_and_never_reuses_a_version() -> None:
    a, b = VehicleStateStore(), VehicleStateStore()
    proxy = ActiveStore(a)
    seen: list[dict] = []
    proxy.add_listener(seen.append)
    a.update(battery_voltage=12.0)
    assert seen and seen[-1]["battery_voltage"] == 12.0
    before = proxy.version()
    proxy.retarget(b)
    assert seen[-1]["battery_voltage"] == 0.0, "the new store's state is pushed at once"
    assert proxy.version() != before
    count = len(seen)
    a.update(battery_voltage=11.0)
    assert len(seen) == count, "the old store no longer feeds the stream"
    b.update(battery_voltage=15.0)
    assert seen[-1]["battery_voltage"] == 15.0
    assert proxy.get_snapshot() is b.get_snapshot()
    proxy.remove_listener(seen.append)
    b.update(battery_voltage=16.0)
    assert seen[-1]["battery_voltage"] == 15.0


def test_active_bridge_moves_the_stream_subscriptions() -> None:
    class Fake:
        def __init__(self, name):
            self.name = name
            self.subs = {"console": [], "shell": [], "params": []}

        def add_console_sub(self, fn): self.subs["console"].append(fn)
        def remove_console_sub(self, fn): self.subs["console"].remove(fn)
        def add_shell_sub(self, fn): self.subs["shell"].append(fn); return self.name
        def remove_shell_sub(self, fn): self.subs["shell"].remove(fn)
        def add_param_listener(self, fn): self.subs["params"].append(fn)
        def remove_param_listener(self, fn): self.subs["params"].remove(fn)
        def arm(self, on): return (self.name, on)

    one, two = Fake("one"), Fake("two")
    proxy = ActiveBridge(one)
    fn = lambda e: None  # noqa: E731
    proxy.add_console_sub(fn)
    assert proxy.add_shell_sub(fn) == "one"
    proxy.add_param_listener(fn)
    assert proxy.arm(True) == ("one", True)
    proxy.retarget(two)
    assert one.subs == {"console": [], "shell": [], "params": []}
    assert two.subs == {"console": [fn], "shell": [fn], "params": [fn]}
    assert proxy.arm(False) == ("two", False)
    proxy.remove_console_sub(fn)
    assert two.subs["console"] == []


# ---------------------------------------------------------------------------
# The fleet
# ---------------------------------------------------------------------------

class FakeBridge:
    def __init__(self, store, connection):
        self.store = store
        self.conn = connection
        self.fleet_tracker = FleetTracker(MAV_TYPE_MAP)
        self.fleet_link_id = "main"
        self.fleet_shared_link = True
        self.started = self.stopped = 0
        self.selected: list[int] = []
        self.controlled = None
        self.preferred = None
        self.rtcm: list[bytes] = []

    def connection_string(self): return self.conn
    def serial_device(self):
        return self.conn.split(":")[1] if self.conn.startswith("serial:") else ""
    def validate_connection(self, conn):
        if not conn.startswith(("udp:", "serial:", "tcp:")):
            raise ValueError("connection must start with one of: udp:, serial:, tcp:")
    def start(self): self.started += 1
    def stop(self): self.stopped += 1
    def controlled_system(self): return self.controlled
    def preferred_system(self): return self.preferred
    def select_system(self, sysid):
        self.selected.append(sysid)
        self.preferred = sysid
    def is_connected(self): return self.controlled is not None
    def inject_rtcm(self, frame): self.rtcm.append(frame); return True


def _fleet(saved=None):
    main_store = VehicleStateStore()
    main = FakeBridge(main_store, "udp:0.0.0.0:14550")
    main.controlled = 1
    persisted: list = []
    f = Fleet(
        main, main_store,
        bridge_factory=lambda store, conn: FakeBridge(store, conn),
        store_factory=VehicleStateStore,
        persist=persisted.append,
    )
    f.load(saved)
    return f, main, persisted


def test_one_vehicle_keeps_the_theme_colour_and_writes_nothing() -> None:
    f, main, persisted = _fleet()
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    snap = f.snapshot()
    assert [v["id"] for v in snap["vehicles"]] == ["main:1"]
    assert snap["vehicles"][0]["color"] == ""
    assert snap["vehicles"][0]["name"] == "Vehicle 1"
    assert snap["vehicles"][0]["active"] is True
    assert snap["active"] == "main:1"
    assert persisted == []
    assert f.bridge.target is main and f.store.target is main.store


def test_a_second_vehicle_gets_a_colour_of_its_own_that_sticks() -> None:
    f, main, persisted = _fleet()
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    main.fleet_tracker.observe(_heartbeat(2), vehicle_heartbeat=True)
    snap = f.snapshot()
    colors = {v["id"]: v["color"] for v in snap["vehicles"]}
    assert colors == {"main:1": "", "main:2": PALETTE[0]}
    assert persisted[-1]["theme_vehicle"] == "main:1"
    assert persisted[-1]["vehicles"]["main:2"]["color"] == PALETTE[0]
    count = len(persisted)
    f.snapshot()
    assert len(persisted) == count, "assigned once, not on every snapshot"


def test_colours_wait_for_the_vehicle_being_flown() -> None:
    """At start-up several vehicles can be heard before the link latches one;
    the one it latches is the one that keeps the theme's colour."""
    f, main, persisted = _fleet()
    main.controlled = None
    main.fleet_tracker.observe(_heartbeat(2), vehicle_heartbeat=True)
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    f.snapshot()
    assert persisted == []
    main.controlled = 1
    colors = {v["id"]: v["color"] for v in f.snapshot()["vehicles"]}
    assert colors == {"main:1": "", "main:2": PALETTE[0]}


def test_naming_and_colouring_a_vehicle() -> None:
    f, main, persisted = _fleet()
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    assert f.set_meta("main:1", name="  Scout ", color="#00ff00") == ""
    row = f.snapshot()["vehicles"][0]
    assert (row["name"], row["color"], row["custom_name"]) == ("Scout", "#00FF00", True)
    assert persisted[-1]["vehicles"]["main:1"] == {"name": "Scout", "color": "#00FF00"}
    assert f.set_meta("main:1", color="blue") != ""
    assert f.set_meta("main:1", name="", color="") == ""
    row = f.snapshot()["vehicles"][0]
    assert (row["name"], row["color"]) == ("Vehicle 1", "")
    assert f.set_meta("bogus", name="x") != ""


def test_selecting_on_the_same_link_asks_the_bridge_and_keeps_the_proxies() -> None:
    f, main, _ = _fleet()
    main.fleet_tracker.observe(_heartbeat(2), vehicle_heartbeat=True)
    assert f.select("main:2") == ""
    assert main.selected == [2]
    assert f.bridge.target is main


def test_extra_links_open_switch_and_close() -> None:
    f, main, persisted = _fleet()
    link_id, error = f.add_link("serial:/dev/ttyUSB1:57600")
    assert (link_id, error) == ("link-2", "")
    extra = f.link("link-2")
    assert extra.bridge.started == 1
    assert extra.bridge.fleet_link_id == "link-2"
    assert persisted[-1]["links"] == [{"id": "link-2", "connection": "serial:/dev/ttyUSB1:57600"}]

    # The same port twice would hand each bridge half the frames.
    assert f.add_link("serial:/dev/ttyUSB1:57600")[0] is None
    assert f.add_link("udp:0.0.0.0:14550")[0] is None
    assert f.add_link("bogus")[0] is None

    extra.bridge.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    assert f.select("link-2:1") == ""
    assert f.bridge.target is extra.bridge
    assert f.store.target is extra.store
    assert not f.active_is_main()
    snap = f.snapshot()
    assert snap["active"] == "link-2:1"
    assert {v["id"] for v in snap["vehicles"]} == {"link-2:1"}
    assert snap["vehicles"][0]["name"] == "Vehicle 1 (Link 2)"

    assert f.remove_link("main") != ""
    assert f.remove_link("link-2") == ""
    assert extra.bridge.stopped == 1
    assert f.bridge.target is main, "removing the flown link falls back to the main one"
    assert f.active_is_main()
    assert persisted[-1] is None or "links" not in persisted[-1]


def test_configured_links_open_on_start_and_close_on_stop() -> None:
    f, main, _ = _fleet({"links": [{"id": "link-3", "connection": "udp:0.0.0.0:14560"}]})
    f.start()
    try:
        extra = f.link("link-3")
        assert extra is not None and extra.bridge.started == 1
    finally:
        f.stop()
    assert extra.bridge.stopped == 1
    assert main.stopped == 0, "the main link is not the fleet's to close"
    assert f._thread is None


def test_rtk_corrections_go_to_every_link() -> None:
    f, main, _ = _fleet()
    f.add_link("udp:0.0.0.0:14560")
    extra = f.link("link-2").bridge
    extra.controlled = 1
    fan = RtcmFanout(main, f)
    assert fan.inject_rtcm(b"rtcm") is True
    assert main.rtcm == [b"rtcm"] and extra.rtcm == [b"rtcm"]
    assert fan.connection_string() == "udp:0.0.0.0:14550"


def test_publishing_reaches_listeners_only_on_change() -> None:
    f, main, _ = _fleet()
    got: list = []
    f.add_listener(got.append)
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    f._publish()
    f._publish()
    assert len(got) == 1
    f.publish_now()
    assert len(got) == 2
    f.remove_listener(got.append)
    f.publish_now()
    assert len(got) == 2


# ---------------------------------------------------------------------------
# What the vehicles not being flown say
# ---------------------------------------------------------------------------

def _statustext(system: int, text: str, severity: int = 4, component: int = 1):
    msg = mavutil.mavlink.MAVLink_statustext_message(severity=severity, text=text.encode())
    return _from(msg, system, component)


def test_warnings_from_vehicles_not_being_flown_become_notifications() -> None:
    f, main, _ = _fleet()
    t = main.fleet_tracker
    t.observe(_heartbeat(1), vehicle_heartbeat=True)
    t.observe(_heartbeat(2), vehicle_heartbeat=True)
    f.collect_events(now=100.0)            # the cursor starts here: history is not news
    t.observe(_statustext(2, "Low battery level!", severity=4))
    t.observe(_statustext(2, "Failsafe enabled: no RC", severity=2))
    t.observe(_statustext(2, "Takeoff detected", severity=6))   # info: not a notification
    t.observe(_statustext(1, "Low battery level!", severity=4))  # the flown one has its own path
    f.collect_events(now=101.0)
    events = f.snapshot()["events"]
    assert [(e["vehicle"], e["level"], e["text"]) for e in events] == [
        ("main:2", "warning", "Low battery level!"),
        ("main:2", "critical", "Failsafe enabled: no RC"),
    ]
    # Repeated within the window: told once.
    t.observe(_statustext(2, "Low battery level!", severity=4))
    f.collect_events(now=110.0)
    assert len(f.snapshot()["events"]) == 2
    t.observe(_statustext(2, "Low battery level!", severity=4))
    f.collect_events(now=140.0)
    assert len(f.snapshot()["events"]) == 3


def test_a_vehicle_not_being_flown_that_goes_quiet_is_reported_lost() -> None:
    f, main, _ = _fleet()
    t = main.fleet_tracker
    t.observe(_heartbeat(1), vehicle_heartbeat=True)
    t.observe(_heartbeat(2), vehicle_heartbeat=True)
    f.collect_events(now=1.0)
    t._systems[2]["last_heartbeat"] -= 10
    t._systems[1]["last_heartbeat"] -= 10
    f.collect_events(now=2.0)
    events = f.snapshot()["events"]
    assert [(e["vehicle"], e["level"]) for e in events] == [("main:2", "critical")], (
        "the flown vehicle's own loss is the status bar's to show"
    )
    t.observe(_heartbeat(2), vehicle_heartbeat=True)
    f.collect_events(now=3.0)
    assert f.snapshot()["events"][-1]["text"] == "heard again"


def test_vehicles_on_other_links_are_reported_too() -> None:
    f, main, _ = _fleet()
    f.add_link("serial:/dev/ttyUSB1:57600")
    extra = f.link("link-2").bridge
    extra.controlled = 1
    extra.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    f.collect_events(now=1.0)
    extra.fleet_tracker.observe(_statustext(1, "Low battery level!"))
    f.collect_events(now=2.0)
    assert f.snapshot()["events"][-1]["vehicle"] == "link-2:1"


# ---------------------------------------------------------------------------
# Several vehicles on one link, switched off
# ---------------------------------------------------------------------------

def test_one_vehicle_per_link_lists_and_asks_nothing_of_the_others(bridge) -> None:
    conn = _SentRecorder()
    bridge._conn = conn
    bridge.fleet_shared_link = False
    ardupilot = mavutil.mavlink.MAV_AUTOPILOT_ARDUPILOTMEGA
    bridge._dispatch(_heartbeat(1))
    bridge._dispatch(_heartbeat(2, autopilot=ardupilot, custom=0))
    assert [s["sysid"] for s in bridge.fleet_tracker.systems()] == [1]
    assert conn.sent == []


def test_switching_shared_links_off_trims_the_list_and_refuses_another_system() -> None:
    f, main, persisted = _fleet()
    main.fleet_tracker.observe(_heartbeat(1), vehicle_heartbeat=True)
    main.fleet_tracker.observe(_heartbeat(2), vehicle_heartbeat=True)
    f.set_shared_links(False)
    assert main.fleet_shared_link is False
    assert [v["id"] for v in f.snapshot()["vehicles"]] == ["main:1"]
    assert persisted[-1]["shared_links"] is False
    assert f.select("main:2") != ""
    assert main.selected == []
    f.set_shared_links(True)
    assert "shared_links" not in (persisted[-1] or {}), "on is the default and is not written"


def test_a_serial_port_is_the_same_port_at_any_baud() -> None:
    f, main, _ = _fleet()
    f.add_link("serial:/dev/ttyUSB1:57600")
    assert f.add_link("serial:/dev/ttyUSB1:115200")[0] is None
    assert f.connection_in_use("serial:/dev/ttyUSB1:115200") == "Link 2"
    assert f.connection_in_use("serial:/dev/ttyUSB1:115200", except_link="link-2") == ""
    assert f.extra_serial_devices() == ["/dev/ttyUSB1"]


def test_opening_the_offered_radio_withdraws_the_offer() -> None:
    f, main, _ = _fleet()
    main.store.update(link_suggestion={
        "connection_string": "serial:/dev/ttyUSB1:57600", "kind": "sik", "device": "/dev/ttyUSB1",
    })
    assert f.snapshot()["suggestion"]["device"] == "/dev/ttyUSB1"
    f.add_link("serial:/dev/ttyUSB1:57600")
    assert main.store.get_snapshot()["link_suggestion"] is None
