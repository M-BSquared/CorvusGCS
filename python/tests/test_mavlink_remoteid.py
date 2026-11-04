"""The Remote ID broadcast on the link, and the aircraft's answer to it.

Remote ID is the only thing Corvus sends to the aircraft over and over rather
than once: both stacks time the identity out and refuse to arm when it stops
arriving. That makes the *cadence* part of the feature, so what is pinned here
is where the send lives, what stops it, and what it must never do to the loop
it rides on.

The last point is the sharp one. The identity goes out from the GCS heartbeat
loop, and that loop treats an exception as "the socket was swapped under us" —
it sleeps 100 ms and goes round again without its usual one-second wait. A
Remote ID send that threw would therefore not fail quietly; it would turn a
1 Hz heartbeat into a 10 Hz one for as long as the link stayed up.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("pymavlink")

from corvus import remote_id  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402


class FakeMessage(SimpleNamespace):
    def get_type(self) -> str:
        return self.message_type

    def get_srcSystem(self) -> int:
        return getattr(self, "source_system", 1)

    def get_srcComponent(self) -> int:
        return getattr(self, "source_component", 236)


class RecordingMav:
    """A v2-capable mav that records every Remote ID message it is handed."""

    def __init__(self, raises: Exception | None = None) -> None:
        self.sent: list[tuple[str, dict[str, Any]]] = []
        self.raises = raises

    def _record(self, name: str) -> Any:
        def send(**kwargs: Any) -> None:
            if self.raises is not None:
                raise self.raises
            self.sent.append((name, kwargs))
        return send

    def __getattr__(self, item: str) -> Any:
        if item.startswith("open_drone_id_") and item.endswith("_send"):
            return self._record(item[: -len("_send")])
        raise AttributeError(item)


class LegacyMav:
    """A MAVLink 1 dialect: the Remote ID send methods do not exist on it."""

    def __getattr__(self, item: str) -> Any:
        raise AttributeError(item)


class FakeConnection:
    def __init__(self, mav: Any) -> None:
        self.mav = mav
        self.source_system = 255
        self.source_component = 190


def _bridge(mav: Any = None) -> MavlinkBridge:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = MavlinkBridge(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection(mav if mav is not None else RecordingMav())
    return bridge


def _identity(**over: Any) -> dict[str, Any]:
    base: dict[str, Any] = {
        "enabled": True,
        "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "ABCD3XYZ"},
        "operator_id": {"operator_id_type": 0, "operator_id": "FIN87astrdge12k8"},
        "self_id": {"description_type": 0, "description": "Survey"},
        "system": {"operator_location_type": 0, "classification_type": 1,
                   "category_eu": 1, "class_eu": 2},
    }
    base.update(over)
    return base


# ---------------------------------------------------------------------------
# What is sent, and when it is not
# ---------------------------------------------------------------------------

def test_an_enabled_identity_sends_all_four_messages_per_cycle() -> None:
    bridge = _bridge()
    bridge.set_remote_id(_identity())

    bridge._send_remote_id(bridge._conn)

    assert [name for name, _ in bridge._conn.mav.sent] == [
        "open_drone_id_basic_id", "open_drone_id_operator_id",
        "open_drone_id_self_id", "open_drone_id_system",
    ]


def test_nothing_is_sent_while_the_broadcast_is_off() -> None:
    """A blank or half-filled identity on the air is a false filing, not a small one."""
    bridge = _bridge()
    bridge.set_remote_id(_identity(enabled=False))

    bridge._send_remote_id(bridge._conn)

    assert bridge._conn.mav.sent == []


def test_the_messages_are_addressed_to_the_connected_vehicle() -> None:
    bridge = _bridge()
    bridge._target_system = 9
    bridge._target_component = 4
    bridge.set_remote_id(_identity())

    bridge._send_remote_id(bridge._conn)

    for _name, kwargs in bridge._conn.mav.sent:
        assert kwargs["target_system"] == 9
        assert kwargs["target_component"] == 4


def test_a_mavlink_1_link_is_reported_rather_than_throwing() -> None:
    """The ODID messages are MAVLink 2 only; a v1 vehicle genuinely cannot be told."""
    bridge = _bridge(LegacyMav())
    bridge.set_remote_id(_identity())

    bridge._send_remote_id(bridge._conn)
    status = bridge.remote_id_status()

    assert status["supported"] is False
    assert status["broadcasting"] is False
    assert "MAVLink 1" in status["error"]


def test_a_send_that_fails_never_escapes_into_the_heartbeat_loop() -> None:
    """A throw here would turn the 1 Hz GCS heartbeat into a 10 Hz one."""
    bridge = _bridge(RecordingMav(raises=OSError("socket gone")))
    bridge.set_remote_id(_identity())

    bridge._send_remote_id(bridge._conn)  # must not raise

    assert "socket gone" in bridge.remote_id_status()["error"]


def test_a_successful_send_clears_a_previous_failure() -> None:
    mav = RecordingMav(raises=OSError("socket gone"))
    bridge = _bridge(mav)
    bridge.set_remote_id(_identity())
    bridge._send_remote_id(bridge._conn)

    mav.raises = None
    bridge._send_remote_id(bridge._conn)

    status = bridge.remote_id_status()
    assert status["error"] == ""
    assert status["broadcasting"] is True


# ---------------------------------------------------------------------------
# Installing a new identity
# ---------------------------------------------------------------------------

def test_a_new_identity_takes_effect_on_the_next_cycle_not_the_next_link() -> None:
    bridge = _bridge()
    bridge.set_remote_id(_identity())
    bridge._send_remote_id(bridge._conn)

    bridge.set_remote_id(_identity(basic_id={"id_type": 1, "ua_type": 2,
                                             "uas_id": "WXYZ2AB"}))
    bridge._send_remote_id(bridge._conn)

    last = dict(bridge._conn.mav.sent)["open_drone_id_basic_id"]
    assert last["uas_id"].startswith(b"WXYZ2AB")


def test_installing_a_new_identity_drops_the_old_one_s_arm_verdict() -> None:
    """The aircraft judged the previous identity, not this one."""
    bridge = _bridge()
    bridge.set_remote_id(_identity())
    bridge._dispatch(FakeMessage(message_type="OPEN_DRONE_ID_ARM_STATUS",
                                 status=0, error=b""))
    assert bridge.remote_id_status()["arm_status"] is not None

    bridge.set_remote_id(_identity(enabled=False))

    assert bridge.remote_id_status()["arm_status"] is None


def test_the_settings_handed_back_cannot_reach_into_stored_state() -> None:
    bridge = _bridge()
    bridge.set_remote_id(_identity())

    taken = bridge.remote_id_settings()
    taken["basic_id"]["uas_id"] = "TAMPERED"

    assert bridge.remote_id_settings()["basic_id"]["uas_id"] == "ABCD3XYZ"


# ---------------------------------------------------------------------------
# The aircraft's answer
# ---------------------------------------------------------------------------

def test_an_arm_status_from_the_vehicle_is_latched() -> None:
    bridge = _bridge()
    bridge._dispatch(FakeMessage(message_type="OPEN_DRONE_ID_ARM_STATUS",
                                 status=1, error=b"no transmitter"))

    arm = bridge.remote_id_status()["arm_status"]
    assert arm["ok"] is False
    assert arm["error"] == "no transmitter"


def test_an_arm_status_from_a_remote_id_module_on_the_airframe_is_accepted() -> None:
    """A serial or DroneCAN module speaks under its own component id, and it is
    the part of the system that actually knows."""
    bridge = _bridge()
    bridge._dispatch(FakeMessage(
        message_type="OPEN_DRONE_ID_ARM_STATUS", status=0, error=b"",
        source_system=1, source_component=236))

    assert bridge.remote_id_status()["arm_status"]["ok"] is True


def test_another_aircraft_s_arm_status_is_ignored() -> None:
    """Behind a router the receive loop sees a second vehicle's telemetry too."""
    bridge = _bridge()
    bridge._dispatch(FakeMessage(
        message_type="OPEN_DRONE_ID_ARM_STATUS", status=0, error=b"",
        source_system=2))

    assert bridge.remote_id_status()["arm_status"] is None


def test_a_vehicle_that_never_reports_is_not_treated_as_a_clearance() -> None:
    """PX4 does not send this message at all; "nothing said" is not "good to arm"."""
    bridge = _bridge()
    bridge.set_remote_id(_identity())

    assert bridge.remote_id_status()["arm_status"] is None


def test_a_reconnect_forgets_what_the_old_link_reported() -> None:
    bridge = _bridge()
    bridge.set_remote_id(_identity())
    bridge._send_remote_id(bridge._conn)
    bridge._dispatch(FakeMessage(message_type="OPEN_DRONE_ID_ARM_STATUS",
                                 status=0, error=b""))

    bridge._forget_remote_id_session()

    status = bridge.remote_id_status()
    assert status["arm_status"] is None
    assert status["broadcasting"] is False
    # The identity itself is the operator's and survives the link.
    assert bridge.remote_id_settings()["basic_id"]["uas_id"] == "ABCD3XYZ"


# ---------------------------------------------------------------------------
# The status the page reads
# ---------------------------------------------------------------------------

def test_broadcasting_means_a_send_actually_landed_not_that_the_switch_is_on() -> None:
    bridge = _bridge()
    bridge.set_remote_id(_identity())

    assert bridge.remote_id_status()["enabled"] is True
    assert bridge.remote_id_status()["broadcasting"] is False

    bridge._send_remote_id(bridge._conn)

    assert bridge.remote_id_status()["broadcasting"] is True


def test_a_bridge_with_no_link_reports_the_messages_as_uncarriable() -> None:
    store = VehicleStateStore()
    bridge = MavlinkBridge(store)

    assert bridge.remote_id_status()["supported"] is False


def test_a_bridge_starts_with_the_default_identity_and_the_broadcast_off() -> None:
    bridge = _bridge()

    assert bridge.remote_id_settings() == remote_id.defaults()
    assert bridge.remote_id_status()["enabled"] is False
