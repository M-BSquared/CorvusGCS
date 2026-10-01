"""A ground station that connects to the aircraft in front of it, by itself.

Auto-connect (``corvus/autoconnect.py``) decides which link to open — USB
flight controller, then SiK radio, then the UDP port a simulator publishes on —
and keeps watching for one afterwards. Two properties matter more than the
convenience, and most of this file is about them:

  * it never steals an active link, and
  * a link the operator chose by hand wins the session.

Everything that decides is a pure function over an injected port list, so none
of this opens a socket, a serial port or a thread. The watcher is exercised
through :meth:`AutoConnectWatcher.tick`, one decision at a time, against a
recording fake of the bridge — so "did it dial" is answered by whether the
sequence stop/set_connection/start actually happened, not by a log line.
"""
from __future__ import annotations

import os
import threading
from typing import Any

import pytest

from corvus import autoconnect as ac
from corvus.state_store import VehicleStateStore


# ---------------------------------------------------------------------------
# Fixtures: ports as list_serial_ports() reports them, and a recording bridge.
# ---------------------------------------------------------------------------

USB_PORT = {
    "device": "/dev/ttyACM0",
    "description": "Pixhawk FMU v6X",
    "hwid": "USB VID:PID=26AC:0032 SER=0001",
}
USB_PORT_2 = {
    "device": "/dev/ttyACM1",
    "description": "Pixhawk FMU v6X",
    "hwid": "USB VID:PID=26AC:0032 SER=0002",
}
SIK_PORT = {
    "device": "/dev/ttyUSB0",
    "description": "FT232R USB UART",
    "hwid": "USB VID:PID=0403:6001",
}
BOOTLOADER_PORT = {
    "device": "/dev/ttyACM0",
    "description": "PX4 BL FMU v6X.x",
    "hwid": "USB VID:PID=26AC:0011",
}
PHANTOM_PORTS = [
    {"device": "/dev/ttyS0", "description": "", "hwid": ""},
    {"device": "/dev/cu.Bluetooth-Incoming-Port", "description": "", "hwid": ""},
    {"device": "/dev/pts/3", "description": "", "hwid": ""},
]


def ports(*rows: dict) -> list[ac.PortInfo]:
    """Classify rows the way the watcher and the resolver both do."""
    return ac.classify_ports(list(rows))


class FakeBridge:
    """Records stop/set_connection/start; validates like the real bridge.

    Deliberately not a MagicMock: the point of most of these tests is the
    *absence* of a teardown, and an assertion that a mock was not called is
    only as good as the name it was spelled with.
    """

    def __init__(self, running: bool = True) -> None:
        self.calls: list[str] = []
        self.connection = "udp:0.0.0.0:14550"
        self.running = running

    def is_running(self) -> bool:
        return self.running

    def validate_connection(self, conn: str) -> None:
        if not isinstance(conn, str) or not conn:
            raise ValueError("connection must be a non-empty string")
        if not conn.startswith(("udp:", "udpin:", "udpout:", "udpbcast:",
                                "tcp:", "tcpin:", "serial:")):
            raise ValueError("bad prefix")

    def connection_string(self) -> str:
        return self.connection

    def stop(self) -> None:
        self.calls.append("stop")

    def set_connection(self, conn: str) -> None:
        self.calls.append(f"set:{conn}")
        self.connection = conn

    def start(self) -> None:
        self.calls.append("start")

    def dialled(self) -> bool:
        return "start" in self.calls


def watcher(
    bridge: FakeBridge,
    store: VehicleStateStore,
    rows: list[dict],
    session: ac.SessionState | None = None,
    stale: bool = True,
) -> ac.AutoConnectWatcher:
    """A watcher that never starts a thread and never enumerates for real."""
    w = ac.AutoConnectWatcher(
        bridge=bridge,
        store=store,
        session=session or ac.SessionState(),
        list_ports_fn=lambda: list(rows),
    )
    if not stale:
        # Fresh heartbeat: the store's own staleness clock says the link is live.
        store.heartbeat()
    return w


# ---------------------------------------------------------------------------
# T1/T2/T4 — the resolution order, which is the whole feature in one table.
# ---------------------------------------------------------------------------

def test_a_flight_controller_on_usb_wins_over_a_radio_and_a_simulator():
    decision = ac.resolve_startup_connection(None, None, ports(SIK_PORT, USB_PORT))
    assert decision.reason == ac.REASON_USB
    assert decision.connection_string == "serial:/dev/ttyACM0:57600"


def test_a_radio_wins_when_there_is_no_flight_controller():
    decision = ac.resolve_startup_connection(None, None, ports(SIK_PORT))
    assert decision.reason == ac.REASON_SIK
    assert decision.connection_string == "serial:/dev/ttyUSB0:57600"


def test_hardware_beats_the_string_the_config_file_was_left_holding():
    """The point of the feature: a cable now outranks a decision from last week."""
    decision = ac.resolve_startup_connection(
        None, "udp:127.0.0.1:14550", ports(USB_PORT),
    )
    assert decision.reason == ac.REASON_USB


def test_the_configured_string_is_used_when_no_hardware_is_present():
    decision = ac.resolve_startup_connection(None, "tcp:127.0.0.1:5760", ports())
    assert decision.reason == ac.REASON_CONFIGURED
    assert decision.connection_string == "tcp:127.0.0.1:5760"


def test_nothing_at_all_lands_on_the_ground_station_udp_port():
    decision = ac.resolve_startup_connection(None, None, ports())
    assert decision.reason == ac.REASON_UDP_FALLBACK
    assert decision.connection_string == ac.UDP_FALLBACK_CONNECTION


def test_an_explicit_command_line_argument_outranks_the_cable():
    """Only a human types an argument, so it is the one input that always wins."""
    decision = ac.resolve_startup_connection(
        "udp:10.0.0.4:14550", "udp:127.0.0.1:14550", ports(USB_PORT),
    )
    assert decision.reason == ac.REASON_CLI
    assert decision.connection_string == "udp:10.0.0.4:14550"


def test_a_garbage_argument_falls_through_instead_of_winning():
    """An unusable string is not a decision — it is a typo, and the hardware
    in front of the operator is a better answer than refusing to start."""
    def validate(conn: str) -> None:
        if not conn.startswith(("udp:", "serial:")):
            raise ValueError("bad prefix")

    decision = ac.resolve_startup_connection(
        "wifi:drone", None, ports(USB_PORT), validator=validate,
    )
    assert decision.reason == ac.REASON_USB


def test_two_bad_strings_and_no_hardware_still_start_the_app():
    def validate(conn: str) -> None:
        raise ValueError("nothing is acceptable today")

    decision = ac.resolve_startup_connection(
        "wifi:drone", "also-nonsense", ports(),
        default_connection="udp:127.0.0.1:14550", validator=validate,
    )
    assert decision.reason == ac.REASON_DEFAULT_FALLBACK
    assert decision.connection_string == "udp:127.0.0.1:14550"


def test_the_toggles_take_each_transport_out_of_the_running():
    both = ports(USB_PORT, SIK_PORT)
    assert ac.resolve_startup_connection(
        None, None, both, usb_enabled=False).reason == ac.REASON_SIK
    assert ac.resolve_startup_connection(
        None, None, both, usb_enabled=False, sik_enabled=False,
    ).reason == ac.REASON_UDP_FALLBACK
    assert ac.resolve_startup_connection(
        None, None, ports(), udp_fallback_enabled=False,
    ).reason == ac.REASON_DEFAULT_FALLBACK


# ---------------------------------------------------------------------------
# T6/T11/T12 — what must never be picked.
# ---------------------------------------------------------------------------

def test_a_board_in_its_bootloader_is_left_for_the_flasher():
    """It speaks the bootloader protocol, not MAVLink, and the flash path is
    about to want the port."""
    decision = ac.resolve_startup_connection(None, None, ports(BOOTLOADER_PORT))
    assert decision.reason == ac.REASON_UDP_FALLBACK
    assert ac.pick_usb_serial(ports(BOOTLOADER_PORT)) is None


def test_a_bootloader_does_not_starve_the_radio_behind_it():
    decision = ac.resolve_startup_connection(
        None, None, ports(BOOTLOADER_PORT, SIK_PORT),
    )
    assert decision.reason == ac.REASON_SIK


def test_phantom_and_pseudo_terminal_nodes_are_never_a_vehicle():
    assert ac.classify_ports(PHANTOM_PORTS) == []
    assert ac.pick_usb_serial(ports(*PHANTOM_PORTS)) is None
    assert ac.resolve_startup_connection(
        None, None, ports(*PHANTOM_PORTS),
    ).reason == ac.REASON_UDP_FALLBACK


def test_two_flight_controllers_pick_the_same_one_every_launch():
    """Enumeration order is the USB bus's business. Connecting to a different
    aircraft depending on which port woke up first is not acceptable."""
    forwards = ac.resolve_startup_connection(None, None, ports(USB_PORT, USB_PORT_2))
    backwards = ac.resolve_startup_connection(None, None, ports(USB_PORT_2, USB_PORT))
    assert forwards.connection_string == backwards.connection_string
    assert forwards.connection_string == "serial:/dev/ttyACM0:57600"
    assert "2 candidates" in forwards.detail


def test_a_windows_com_port_is_classified_from_its_descriptor():
    """COM7 is a Pixhawk, a radio or a Bluetooth pairing with equal
    probability — the name says nothing, so the descriptor answers."""
    fc = ports({"device": "COM7", "description": "Pixhawk FMU",
                "hwid": "USB VID:PID=26AC:0032"})
    radio = ports({"device": "COM8", "description": "Silicon Labs CP210x",
                   "hwid": "USB VID:PID=10C4:EA60"})
    assert ac.pick_usb_serial(fc) == ("COM7", 57600, "usb")
    assert ac.pick_usb_serial(radio) == ("COM8", 57600, "sik")


def test_empty_and_absent_inputs_mean_absent_not_invalid():
    assert ac.resolve_startup_connection("", None, ports()).reason == ac.REASON_UDP_FALLBACK
    assert ac.resolve_startup_connection("   ", "", ports()).reason == ac.REASON_UDP_FALLBACK


# ---------------------------------------------------------------------------
# T5/T9 — never steal an active link.
# ---------------------------------------------------------------------------

def test_a_device_plugged_in_while_connected_is_offered_and_not_taken(store):
    """The link that is up may be carrying an aircraft that is flying."""
    bridge = FakeBridge()
    store.update(link_status="connected", link_connection="udp:0.0.0.0:14550")
    w = watcher(bridge, store, [USB_PORT], stale=False)

    assert w.tick() == "suggested"
    assert bridge.calls == []
    suggestion = store.get_snapshot()["link_suggestion"]
    assert suggestion["device"] == "/dev/ttyACM0"
    assert suggestion["kind"] == "usb-direct"


def test_a_degraded_link_with_a_fresh_heartbeat_is_still_a_link(store):
    bridge = FakeBridge()
    store.update(link_status="degraded")
    w = watcher(bridge, store, [USB_PORT], stale=False)
    assert w.tick() == "suggested"
    assert not bridge.dialled()


def test_a_connected_link_whose_heartbeat_went_stale_is_the_bridges_problem(store):
    """It is mid-backoff. Tearing it down races the reconnect loop, and the
    loop is the thing that actually knows what happened to the transport."""
    bridge = FakeBridge()
    store.update(link_status="connected")
    w = watcher(bridge, store, [USB_PORT], stale=True)
    assert not w.tick().startswith("dialled")
    assert not bridge.dialled()


def test_the_suggestion_is_published_once_and_not_every_two_seconds(store):
    """Republishing would reopen a row the operator dismissed, forever."""
    bridge = FakeBridge()
    store.update(link_status="connected")
    w = watcher(bridge, store, [USB_PORT], stale=False)
    assert w.tick() == "suggested"

    seen: list[Any] = []
    store.add_listener(lambda snap: seen.append(snap["link_suggestion"]))
    assert w.tick() == "noop: suggestion-unchanged"
    assert seen == []


def test_unplugging_the_offered_device_withdraws_the_offer(store):
    bridge = FakeBridge()
    store.update(link_status="connected")
    rows = [USB_PORT]
    w = ac.AutoConnectWatcher(
        bridge=bridge, store=store, session=ac.SessionState(),
        list_ports_fn=lambda: list(rows),
    )
    store.heartbeat()
    w.tick()
    assert store.get_snapshot()["link_suggestion"] is not None
    rows.clear()
    w.tick()
    assert store.get_snapshot()["link_suggestion"] is None


def test_the_link_already_in_use_is_not_suggested_back(store):
    bridge = FakeBridge()
    store.update(link_status="connected", link_connection="serial:/dev/ttyACM0:57600")
    w = watcher(bridge, store, [USB_PORT], stale=False)
    assert w.tick() == "noop: fresh-heartbeat"
    assert store.get_snapshot()["link_suggestion"] is None


# ---------------------------------------------------------------------------
# T4/AC4 — the one case that may dial: disconnected, stale, nothing to lose.
# ---------------------------------------------------------------------------

def test_plugging_in_while_disconnected_dials_the_new_device(store):
    bridge = FakeBridge()
    store.update(link_status="reconnecting")
    w = watcher(bridge, store, [USB_PORT])

    assert w.tick() == f"dialled: {ac.REASON_USB}"
    assert bridge.calls == ["stop", "set:serial:/dev/ttyACM0:57600", "start"]
    assert store.get_snapshot()["link_auto"]["reason"] == ac.REASON_USB


def test_the_same_device_is_dialled_once_not_on_every_tick(store):
    """A re-dial every two seconds tears the handshake down mid-flight and
    reads, from the outside, as a device that simply never connects."""
    bridge = FakeBridge()
    store.update(link_status="reconnecting")
    w = watcher(bridge, store, [USB_PORT])

    assert w.tick().startswith("dialled")
    assert w.tick() == "noop: already-dialled"
    assert w.tick() == "noop: already-dialled"
    assert bridge.calls.count("start") == 1


def test_unplugging_and_replugging_the_same_cable_is_a_fresh_ask(store):
    """The once-per-device latch is per appearance, not per process. An
    operator who pulls a cable and puts it back is asking again, and a latch
    that outlived the device would answer that with silence."""
    bridge = FakeBridge()
    store.update(link_status="reconnecting")
    rows = [USB_PORT]
    w = ac.AutoConnectWatcher(
        bridge=bridge, store=store, session=ac.SessionState(),
        list_ports_fn=lambda: list(rows),
    )

    assert w.tick().startswith("dialled")
    assert w.tick() == "noop: already-dialled"
    rows.clear()
    assert w.tick() == f"noop: {ac.REASON_NO_CANDIDATE}"
    rows.append(USB_PORT)
    assert w.tick().startswith("dialled")
    assert bridge.calls.count("start") == 2


def test_nothing_plugged_in_is_a_quiet_no_op(store):
    bridge = FakeBridge()
    store.update(link_status="disconnected")
    w = watcher(bridge, store, [])
    assert w.tick() == f"noop: {ac.REASON_NO_CANDIDATE}"
    assert bridge.calls == []


def test_a_bridge_the_operator_stopped_stays_stopped(store):
    bridge = FakeBridge(running=False)
    store.update(link_status="disconnected")
    w = watcher(bridge, store, [USB_PORT])
    assert w.tick() == "noop: bridge-stopped"
    assert not bridge.dialled()


# ---------------------------------------------------------------------------
# T8 — manual wins the session.
# ---------------------------------------------------------------------------

def test_a_link_chosen_by_hand_is_not_dialled_away_from(store):
    bridge = FakeBridge()
    store.update(link_status="disconnected")
    session = ac.SessionState()
    session.note_manual_connect()
    w = watcher(bridge, store, [USB_PORT], session=session)

    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


def test_an_overridden_session_stops_nagging_as_well_as_dialling(store):
    session = ac.SessionState()
    session.note_manual_connect()
    assert ac.suggest_connection({"link_status": "connected"}, ports(USB_PORT), session) is None


def test_the_override_is_never_written_to_the_config_file():
    """It is the operator's intent for one session. Surviving a restart would
    make it fight the USB cable at the next field launch."""
    from corvus.config import DEFAULT_AUTOCONNECT, _coerce_autoconnect
    assert "manual_override" not in DEFAULT_AUTOCONNECT
    assert _coerce_autoconnect({"manual_override": True}) is None


def test_turning_auto_connect_off_stops_every_decision(store):
    bridge = FakeBridge()
    store.update(link_status="disconnected")
    session = ac.SessionState()
    session.apply_config({"enabled": False})
    w = watcher(bridge, store, [USB_PORT], session=session)

    assert w.tick() == "disabled"
    assert bridge.calls == []


# ---------------------------------------------------------------------------
# T14 — the config block.
# ---------------------------------------------------------------------------

def test_a_config_file_from_before_this_feature_loads_unchanged():
    from corvus.config import CorvusConfig, autoconnect_settings
    cfg = CorvusConfig()
    assert cfg.autoconnect is None
    assert autoconnect_settings(cfg) == {
        "enabled": True, "usb": True, "sik": True, "udp_fallback": True,
    }


def test_only_a_genuine_boolean_can_turn_auto_connect_off():
    """A file carrying the string "false" must read as "not set". The switch
    decides whether the station dials an aircraft on its own."""
    from corvus.config import _coerce_autoconnect
    assert _coerce_autoconnect({"enabled": "false"}) is None
    assert _coerce_autoconnect({"enabled": 0}) is None
    assert _coerce_autoconnect({"enabled": False}) == {"enabled": False}


def test_unknown_keys_inside_the_block_are_ignored():
    from corvus.config import _coerce_autoconnect
    assert _coerce_autoconnect({"usb": False, "bluetooth": True}) == {"usb": False}


def test_the_block_survives_a_save_and_load_round_trip(tmp_path):
    from corvus.config import CorvusConfig, load_config, save_config
    path = str(tmp_path / "config.json")
    save_config(CorvusConfig(autoconnect={"enabled": False, "sik": False}), path)
    loaded = load_config(path)
    assert loaded.autoconnect == {"enabled": False, "sik": False}


# ---------------------------------------------------------------------------
# T15 — the reason strings are an API, not prose.
# ---------------------------------------------------------------------------

def test_every_reason_the_resolver_can_return_is_in_the_published_set():
    cases = [
        ac.resolve_startup_connection("udp:1.2.3.4:14550", None, ports()),
        ac.resolve_startup_connection(None, None, ports(USB_PORT)),
        ac.resolve_startup_connection(None, None, ports(SIK_PORT)),
        ac.resolve_startup_connection(None, "udp:127.0.0.1:14550", ports()),
        ac.resolve_startup_connection(None, None, ports()),
        ac.resolve_startup_connection(
            None, None, ports(), udp_fallback_enabled=False),
    ]
    assert {d.reason for d in cases} <= ac.REASONS
    assert len({d.reason for d in cases}) == len(cases)


def test_the_udp_fallback_listens_on_every_interface():
    """0.0.0.0 is the superset bind: it takes a simulator on loopback AND one
    on a bridged interface, while 127.0.0.1 drops the second silently and
    looks exactly like a simulator that is not running."""
    assert ac.UDP_FALLBACK_CONNECTION == "udp:0.0.0.0:14550"


# ---------------------------------------------------------------------------
# Lifecycle — one thread, and it goes away.
# ---------------------------------------------------------------------------

def test_the_watcher_is_one_daemon_thread_that_joins_on_stop(store):
    before = {t.name for t in threading.enumerate()}
    w = ac.AutoConnectWatcher(
        bridge=FakeBridge(), store=store, session=ac.SessionState(),
        list_ports_fn=list, poll_s=0.05,
    )
    w.start()
    try:
        assert w.is_alive()
        added = {t.name for t in threading.enumerate()} - before
        assert added == {"autoconnect-watcher"}
        thread = next(t for t in threading.enumerate() if t.name == "autoconnect-watcher")
        assert thread.daemon
    finally:
        w.stop()
    assert not w.is_alive()
    assert "autoconnect-watcher" not in {t.name for t in threading.enumerate()}


def test_a_disabled_watcher_starts_no_thread_at_all(store):
    session = ac.SessionState()
    session.apply_config({"enabled": False})
    w = ac.AutoConnectWatcher(bridge=FakeBridge(), store=store, session=session)
    w.start()
    assert not w.is_alive()
    w.stop()


def test_stopping_twice_is_not_an_error(store):
    w = ac.AutoConnectWatcher(
        bridge=FakeBridge(), store=store, session=ac.SessionState(), poll_s=0.05,
    )
    w.start()
    w.stop()
    w.stop()
    assert not w.is_alive()


def test_an_enumeration_that_blows_up_does_not_kill_the_watcher(store):
    """A watcher is a convenience. It may never be the reason the ground
    station goes down."""
    def boom() -> list[dict]:
        raise OSError("the USB subsystem is having a day")

    store.update(link_status="disconnected")
    w = ac.AutoConnectWatcher(
        bridge=FakeBridge(), store=store, session=ac.SessionState(),
        list_ports_fn=boom, poll_s=0.02,
    )
    w.start()
    try:
        with pytest.raises(OSError):
            w.tick()
        assert w.is_alive()
    finally:
        w.stop()


def test_a_dial_that_fails_is_logged_and_survived(store, caplog):
    class Broken(FakeBridge):
        def start(self) -> None:
            raise OSError("address already in use")

    bridge = Broken()
    store.update(link_status="disconnected")
    w = watcher(bridge, store, [USB_PORT])
    w._bridge = bridge
    assert w.tick() == "dial-failed"


# ---------------------------------------------------------------------------
# The classification this builds on is the flasher's, not a second copy.
# ---------------------------------------------------------------------------

def test_auto_connect_and_the_firmware_flasher_agree_on_what_a_pixhawk_is():
    """A second VID:PID table would drift, and the direction it drifts in
    decides whether the app offers to flash firmware down a radio."""
    pytest.importorskip("pymavlink")
    from corvus.mavlink_bridge import MavlinkBridge, classify_serial_device
    from corvus.state_store import VehicleStateStore as Store

    bridge = MavlinkBridge(Store(), "serial:/dev/ttyACM0:57600")
    assert bridge.transport() == classify_serial_device("/dev/ttyACM0")
    bridge.set_connection("serial:/dev/ttyUSB0:57600")
    assert bridge.transport() == classify_serial_device("/dev/ttyUSB0")


# ---------------------------------------------------------------------------
# The HTTP surface: the endpoints that set and read the session state.
# ---------------------------------------------------------------------------

def _handler(bridge: Any, store: VehicleStateStore, session: Any):
    """A CorvusHandler with no socket, capturing what it would have sent."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.server import CorvusHandler

    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge
    handler.store = store
    handler.autoconnect_session = session
    sent: list[tuple[dict, int]] = []
    handler._send_json = lambda data, status=200: sent.append((data, status))
    return handler, sent


def test_connecting_by_hand_takes_the_session_from_auto_connect(store):
    bridge = FakeBridge()
    session = ac.SessionState()
    handler, sent = _handler(bridge, store, session)
    store.update(link_suggestion={"connection_string": "serial:/dev/ttyACM0:57600"})

    handler._api_mavlink_connect({"connection": "udpout:127.0.0.1:14550"})

    assert sent[0][1] == 200
    assert session.manual_override is True
    assert store.get_snapshot()["link_suggestion"] is None
    assert store.get_snapshot()["link_auto"]["reason"] == "manual"


def test_a_connect_the_bridge_refused_is_a_typo_and_not_a_choice(store):
    bridge = FakeBridge()
    session = ac.SessionState()
    handler, sent = _handler(bridge, store, session)

    handler._api_mavlink_connect({"connection": "wifi:drone"})

    assert sent[0][1] == 400
    assert session.manual_override is False


def test_disconnect_means_disconnected_and_nothing_dials_it_back_open(store):
    """The operator freed the radio — to swap a cable, to hand the aircraft to
    another station. A watcher that reopens it two seconds later has taken a
    decision that was not its to take."""
    bridge = FakeBridge()
    session = ac.SessionState()
    handler, sent = _handler(bridge, store, session)

    handler._api_mavlink_disconnect({})

    assert sent[0] == ({"ok": True}, 200)
    assert session.manual_override is True
    assert store.get_snapshot()["link_suggestion"] is None


def test_the_auto_endpoint_reads_and_decides_nothing(store):
    bridge = FakeBridge()
    session = ac.SessionState()
    session.note_decision(ac.REASON_USB, "serial:/dev/ttyACM0:57600")
    handler, sent = _handler(bridge, store, session)

    handler._api_mavlink_auto()

    body, status = sent[0]
    assert status == 200
    assert body["auto"]["reason"] == ac.REASON_USB
    assert body["auto"]["winner"] == "serial:/dev/ttyACM0:57600"
    assert body["suggestion"] is None
    assert bridge.calls == []


def test_a_server_without_a_session_still_connects(store):
    """The unit-test fixtures wire a bridge and a store and nothing else. A
    connect must not depend on auto-connect having been set up."""
    bridge = FakeBridge()
    handler, sent = _handler(bridge, store, None)
    handler._api_mavlink_connect({"connection": "udp:0.0.0.0:14550"})
    assert sent[0][1] == 200


def test_saving_settings_never_dials_and_never_tears_a_link_down(tmp_path, store):
    """POST /api/config has never dialled for mavlink_connection, and the
    auto-connect toggles do not change that: a settings save is not a link
    action."""
    pytest.importorskip("pymavlink")
    pytest.importorskip("paramiko")
    from corvus.config import CorvusConfig
    from corvus.server import CorvusHandler

    bridge = FakeBridge()
    session = ac.SessionState()
    handler = object.__new__(CorvusHandler)
    handler.mavlink = bridge
    handler.store = store
    handler.autoconnect_session = session
    handler.config = CorvusConfig()
    handler.config_path = str(tmp_path / "config.json")

    public, error = handler._apply_config_partial({"autoconnect": {"sik": False}})

    assert error is None
    assert public["autoconnect"] == {"sik": False}
    assert bridge.calls == []
    # The live watcher feels the switch now, not only at the next launch.
    assert session.sik is False
    assert session.usb is True


def test_the_startup_resolver_publishes_what_it_picked(store, monkeypatch):
    pytest.importorskip("pymavlink")
    from corvus import server as srv
    from corvus.mavlink_bridge import MavlinkBridge

    bridge = MavlinkBridge(store, "udp:0.0.0.0:14550")
    monkeypatch.setattr(
        MavlinkBridge, "list_serial_ports", staticmethod(lambda: [USB_PORT]),
    )
    session = ac.SessionState()

    chosen = srv.apply_startup_connection(
        bridge, None, configured="udp:127.0.0.1:14550",
        session=session, store=store,
    )

    assert chosen == "serial:/dev/ttyACM0:57600"
    auto = store.get_snapshot()["link_auto"]
    assert auto["reason"] == ac.REASON_USB
    assert auto["winner"] == chosen


def test_startup_without_a_session_behaves_exactly_as_it_always_did(store):
    """The old two-argument call is still the old two-argument call: set the
    string, fall back to the default when it is unusable."""
    pytest.importorskip("pymavlink")
    from corvus.server import DEFAULT_MAVLINK_CONNECTION, apply_startup_connection
    from corvus.mavlink_bridge import MavlinkBridge

    bridge = MavlinkBridge(store, "udp:0.0.0.0:14550")
    assert apply_startup_connection(bridge, "tcp:127.0.0.1:5760") == "tcp:127.0.0.1:5760"
    assert apply_startup_connection(bridge, "wifi:drone") == DEFAULT_MAVLINK_CONNECTION


# ---------------------------------------------------------------------------
# Bootloaders: the product string decides, never a PX4 id.
# ---------------------------------------------------------------------------

def test_a_running_pixhawk_1_is_not_mistaken_for_its_bootloader():
    """PX4's FMUv2/v3 firmware enumerates as 26AC:0011, the same id its
    bootloader uses. Matching the id skipped every Pixhawk 1, Pixhawk 2.4.8 and
    Cube Black running PX4, so auto-connect never dialled them."""
    from corvus.serial_ports import is_bootloader_port

    running = {
        "device": "/dev/ttyACM0",
        "description": "PX4 FMU v2.x",
        "hwid": "USB VID:PID=26AC:0011 SER=0 LOCATION=1-1:1.0",
    }
    assert not is_bootloader_port(running["device"], running["hwid"], running["description"])
    decision = ac.resolve_startup_connection(None, None, ports(running))
    assert decision.reason == ac.REASON_USB
    assert decision.connection_string == "serial:/dev/ttyACM0:57600"


@pytest.mark.parametrize("description, hwid, expected", [
    ("PX4 BL FMU v2.x", "USB VID:PID=26AC:0011 SER=0", True),
    ("PX4 BL FMU v6X.x", "USB VID:PID=3185:0035 SER=0", True),
    ("CubeOrange-BL", "USB VID:PID=2DAE:1016 SER=2A0035", True),
    ("STM32 BOOTLOADER", "USB VID:PID=0483:DF11", True),
    ("CubeOrange", "USB VID:PID=2DAE:1016 SER=2A0035", False),
    ("PX4 FMU v6X.x", "USB VID:PID=3185:0035 SER=0", False),
    # A serial number is not a word: "DFU" inside one is not DFU mode.
    ("FT232R USB UART", "USB VID:PID=0403:6001 SER=A9DFU3XY", False),
    ("USB BLE dongle", "USB VID:PID=1A86:7523", False),
])
def test_bootloaders_are_told_apart_by_their_product_string(description, hwid, expected):
    from corvus.serial_ports import is_bootloader_port
    assert is_bootloader_port("/dev/ttyACM0", hwid, description) is expected


# ---------------------------------------------------------------------------
# Baud rates the operator already chose are kept.
# ---------------------------------------------------------------------------

def test_a_radio_saved_at_115200_is_dialled_at_115200_on_its_own_port():
    decision = ac.resolve_startup_connection(
        None, "serial:/dev/ttyUSB0:115200", ports(SIK_PORT),
    )
    assert decision.reason == ac.REASON_SIK
    assert decision.connection_string == "serial:/dev/ttyUSB0:115200"


def test_a_saved_rate_for_another_port_does_not_leak_onto_this_one():
    decision = ac.resolve_startup_connection(
        None, "serial:/dev/ttyUSB3:115200", ports(SIK_PORT),
    )
    assert decision.connection_string == "serial:/dev/ttyUSB0:57600"


def test_the_radio_the_link_is_on_is_not_offered_back_at_another_rate():
    session = ac.SessionState()
    snapshot = {"link_status": "connected", "link_connection": "serial:/dev/ttyUSB0:115200"}
    assert ac.suggest_connection(snapshot, ports(SIK_PORT), session) is None


def test_a_redial_of_the_same_port_keeps_the_link_rate(store):
    bridge = FakeBridge()
    bridge.connection = "serial:/dev/ttyACM0:115200"
    store.update(link_status="reconnecting")
    w = watcher(bridge, store, [USB_PORT])
    assert w.tick() == f"dialled: {ac.REASON_USB}"
    assert "set:serial:/dev/ttyACM0:115200" in bridge.calls


# ---------------------------------------------------------------------------
# The link's own device, back under another name.
# ---------------------------------------------------------------------------

# Node names no test host has, so presence comes from the injected enumeration.
OLD_NODE = "/dev/ttyACM41"
NEW_NODE = "/dev/ttyACM42"


def fc(device: str, serial: str = "3A0029", description: str = "Pixhawk FMU v6X") -> dict:
    return {
        "device": device,
        "description": description,
        "hwid": f"USB VID:PID=3185:0035 SER={serial} LOCATION=1-2",
    }


def manual_link(store, rows: list[dict], baud: int = 115200):
    """A link the operator chose by hand, seen once by the watcher while it was there."""
    bridge = FakeBridge()
    bridge.connection = f"serial:{OLD_NODE}:{baud}"
    session = ac.SessionState()
    session.note_manual_connect()
    store.update(link_status="connected", link_connection=bridge.connection)
    store.heartbeat()
    w = watcher(bridge, store, rows, session=session)
    assert w.tick() == "noop: manual-override"
    return bridge, session, w


def test_a_hand_chosen_link_follows_its_device_to_a_new_name(store):
    """Plugged into another socket, or replugged on Linux while the old node
    was still open: the same board comes back as a new node. The bridge would
    retry the vanished name for the rest of the session."""
    rows = [fc(OLD_NODE)]
    bridge, session, w = manual_link(store, rows)

    rows[:] = [fc(NEW_NODE)]
    store.update(link_status="reconnecting")
    assert w.tick() == "followed"

    assert bridge.calls == ["stop", f"set:serial:{NEW_NODE}:115200", "start"]
    auto = store.get_snapshot()["link_auto"]
    assert auto["manual_override"] is True
    assert auto["reason"] == ac.REASON_MANUAL
    assert auto["winner"] == f"serial:{NEW_NODE}:115200"
    assert store.get_snapshot()["link_suggestion"] is None


def test_nothing_moves_until_the_bridge_has_given_the_old_name_up(store):
    """About two seconds pass between pulling the cable and the bridge noticing.
    Acting then would offer the same board back as if it were a second one."""
    rows = [fc(OLD_NODE)]
    bridge, _, w = manual_link(store, rows)
    rows[:] = [fc(NEW_NODE)]
    assert w.tick() == "noop: link-device-gone"
    assert bridge.calls == []
    assert store.get_snapshot()["link_suggestion"] is None


def test_a_different_board_is_not_followed(store):
    rows = [fc(OLD_NODE, serial="AAAA")]
    bridge, _, w = manual_link(store, rows)
    rows[:] = [fc(NEW_NODE, serial="BBBB")]
    store.update(link_status="reconnecting")
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


def test_two_boards_with_the_same_identity_are_no_answer(store):
    rows = [fc(OLD_NODE, serial="")]
    bridge, _, w = manual_link(store, rows)
    rows[:] = [fc(NEW_NODE, serial=""), fc("/dev/ttyACM43", serial="")]
    store.update(link_status="reconnecting")
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


def test_the_old_name_still_being_there_is_not_a_move(store):
    rows = [fc(OLD_NODE, serial="")]
    bridge, _, w = manual_link(store, rows)
    rows.append(fc(NEW_NODE, serial=""))
    store.update(link_status="reconnecting")
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


def test_a_disconnect_by_hand_is_not_followed(store):
    rows = [fc(OLD_NODE)]
    bridge, _, w = manual_link(store, rows)
    bridge.running = False
    rows[:] = [fc(NEW_NODE)]
    store.update(link_status="disconnected")
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


def test_the_bootloader_is_waited_out_and_the_firmware_followed(store):
    """A PX4 board comes back in its bootloader first, with the firmware's
    vendor and product id, and re-enumerates once the firmware boots."""
    rows = [fc(OLD_NODE)]
    bridge, _, w = manual_link(store, rows)
    store.update(link_status="reconnecting")

    rows[:] = [fc(NEW_NODE, description="PX4 BL FMU v6X.x")]
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []

    rows[:] = [fc("/dev/ttyACM43")]
    assert w.tick() == "followed"
    assert "set:serial:/dev/ttyACM43:115200" in bridge.calls


def test_a_bootloader_on_the_old_name_does_not_replace_what_was_remembered(store):
    rows = [fc(OLD_NODE)]
    bridge, _, w = manual_link(store, rows)
    rows[:] = [fc(OLD_NODE, serial="BL", description="PX4 BL FMU v6X.x")]
    w.tick()
    rows[:] = [fc(NEW_NODE)]
    store.update(link_status="reconnecting")
    assert w.tick() == "followed"


def test_an_automatic_link_follows_at_once_rather_than_after_eight_seconds(store):
    rows = [fc(OLD_NODE)]
    bridge = FakeBridge()
    bridge.connection = f"serial:{OLD_NODE}:57600"
    store.update(link_status="connected", link_connection=bridge.connection)
    store.heartbeat()
    w = watcher(bridge, store, rows)
    w.tick()

    rows[:] = [fc(NEW_NODE)]
    store.update(link_status="reconnecting")
    assert not store.is_stale(ac.STALE_AFTER_S), "the heartbeat is still recent"
    assert w.tick() == "followed"
    assert store.get_snapshot()["link_auto"]["reason"] == ac.REASON_USB
    assert store.get_snapshot()["link_auto"]["manual_override"] is False


def test_an_identity_learned_on_another_link_is_not_used(store):
    rows = [fc(OLD_NODE)]
    bridge, _, w = manual_link(store, rows)
    bridge.connection = "serial:/dev/ttyACM44:115200"
    rows[:] = [fc(NEW_NODE)]
    store.update(link_status="reconnecting")
    assert w.tick() == "noop: manual-override"
    assert bridge.calls == []


# ---------------------------------------------------------------------------
# serial_ports helpers the watcher stands on.
# ---------------------------------------------------------------------------

def test_usb_identity_reads_ids_and_serial_number():
    from corvus.serial_ports import usb_identity
    assert usb_identity("USB VID:PID=26ac:0032 SER=0001 LOCATION=20-1") == "26AC:0032/0001"
    assert usb_identity("USB VID:PID=1A86:7523 LOCATION=1-1") == "1A86:7523/"
    assert usb_identity("n/a") is None
    assert usb_identity("") is None


def test_one_node_has_many_spellings(tmp_path):
    from corvus.serial_ports import same_port
    assert same_port("/dev/cu.usbmodem01", "/dev/tty.usbmodem01")
    assert same_port("COM3", "\\\\.\\COM3")
    assert same_port("com3", "COM3")
    assert not same_port("COM3", "COM13")


@pytest.mark.skipif(os.name == "nt", reason="a Windows serial port is a COM name, never a file link")
def test_a_by_id_link_is_the_node_it_points_at(tmp_path):
    from corvus.serial_ports import same_port
    target = tmp_path / "ttyACM0"
    target.write_text("")
    link = tmp_path / "usb-Holybro_PX4_FMU_v6C.x_0-if00"
    link.symlink_to(target)
    assert same_port(str(link), str(target))


def test_a_port_is_present_when_enumerated_or_on_disk(tmp_path):
    from corvus.serial_ports import port_present
    node = tmp_path / "ttyACM0"
    assert not port_present(str(node), [])
    node.write_text("")
    assert port_present(str(node), [])
    assert port_present("COM7", ["COM7"])
    assert not port_present("COM7", ["COM8"])
    assert not port_present("", ["COM7"])


@pytest.mark.parametrize("conn, expected", [
    ("serial:/dev/ttyUSB0:115200", ("/dev/ttyUSB0", 115200)),
    ("serial:/dev/ttyUSB0", ("/dev/ttyUSB0", 57600)),
    ("serial:COM3:921600", ("COM3", 921600)),
    ("serial:/dev/serial/by-path/pci-0000:00:14.0-usb-0:2:1.0:57600",
     ("/dev/serial/by-path/pci-0000:00:14.0-usb-0:2:1.0", 57600)),
])
def test_serial_connection_strings_split_on_the_last_numeric_field(conn, expected):
    from corvus.serial_ports import split_serial_connection
    assert split_serial_connection(conn) == expected
