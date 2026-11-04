"""The Rust Remote ID engine against the Python Remote ID it replaced.

``tests/reference/remote_id_python.py`` and ``mavlink_remote_id_python.py``
keep the identity coercion, the four OPEN_DRONE_ID messages, the arm status
and the 1 Hz broadcast as they were in Python. The tests feed both the same
identities (hand picked edge cases and a few thousand random ones), times,
arm statuses and links, and compare what comes out: the resolved identity,
every message and field, the errors, and the broadcast status.
"""
from __future__ import annotations

import collections
import datetime
import decimal
import fractions
import random
from types import SimpleNamespace
from typing import Any

import pytest

pytest.importorskip("corvus_link")

from corvus import mavlink_remote_id as rust_side  # noqa: E402
from corvus import remote_id  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402
from corvus.state_store import VehicleStateStore  # noqa: E402
from corvus_link import remote_id as wire  # noqa: E402
from tests.reference import mavlink_remote_id_python as reference_mixin  # noqa: E402
from tests.reference import remote_id_python as reference  # noqa: E402
from tests.test_mavlink_remoteid import FakeConnection, LegacyMav, RecordingMav  # noqa: E402


class ReferenceBridge(reference_mixin.RemoteIdMixin, MavlinkBridge):
    """A bridge whose Remote ID is the Python reference."""


for _name in ("_remote_id", "_remote_id_sent_at", "_remote_id_error", "_remote_id_arm"):
    setattr(ReferenceBridge, _name, None)


def outcome(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return ("ok", call(*args, **kwargs))
    except Exception as exc:  # noqa: BLE001 - compared below
        return ("error", type(exc).__name__, str(exc))


class Text:
    def __str__(self) -> str:
        return " \x1c Survey ö\x00 north \x1f"


class Number:
    def __int__(self) -> int:
        return 2

    def __float__(self) -> float:
        return 12.75


ODD = [
    None, True, False, 0, 1, -1, 2, 3, 7, 15, 16, 2.7, -0.0, "3", " 2 ", "2.5", "x", "", "１",
    10**30, -(10**30), float("nan"), float("inf"), float("-inf"), 1e308, -91.5, 95.25, 181, 40000,
    65535.9, 70000, "1e3", b"AB\x00CD", bytearray(b"\xffAB C"), b"\xe2\x82\xac9", "Böck survey",
    "\x1c abc \x1f", " spaced ", "a" * 40, "x\x00y", [1], {"a": 1}, (2,), Text(), Number(),
    decimal.Decimal("2.5"), fractions.Fraction(7, 2), "FAA", "Eu ", "eu", "faa",
]


def _section(rng: random.Random, keys: list[str]) -> Any:
    roll = rng.random()
    if roll < 0.05:
        return rng.choice(ODD)
    part: dict[str, Any] = collections.OrderedDict() if roll < 0.1 else {}
    for key in keys:
        if rng.random() < 0.8:
            part[key] = rng.choice(ODD)
    return part


SECTIONS = {
    "basic_id": ["id_type", "ua_type", "uas_id"],
    "operator_id": ["operator_id_type", "operator_id"],
    "self_id": ["description_type", "description"],
    "system": ["operator_location_type", "operator_latitude", "operator_longitude",
               "operator_altitude_geo", "classification_type", "category_eu", "class_eu",
               "area_count", "area_radius", "area_ceiling", "area_floor"],
}


def random_identity(seed: int) -> Any:
    rng = random.Random(seed)
    if rng.random() < 0.03:
        return rng.choice(ODD)
    out: dict[str, Any] = {}
    for key in ("enabled", "region"):
        if rng.random() < 0.7:
            out[key] = rng.choice(ODD)
    for name, keys in SECTIONS.items():
        if rng.random() < 0.9:
            out[name] = _section(rng, keys)
    return out


def _plain(value: Any) -> Any:
    """NaN compares unequal to itself; everything else is compared as is."""
    if isinstance(value, float) and value != value:
        return "nan"
    if isinstance(value, dict):
        return {k: _plain(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return type(value)(_plain(v) for v in value)
    return value


def test_the_enum_values_are_the_page_options() -> None:
    lists = {
        "id_type": remote_id.ID_TYPE_OPTIONS, "ua_type": remote_id.UA_TYPE_OPTIONS,
        "operator_id_type": remote_id.OPERATOR_ID_TYPE_OPTIONS,
        "description_type": remote_id.DESCRIPTION_TYPE_OPTIONS,
        "operator_location_type": remote_id.OPERATOR_LOCATION_TYPE_OPTIONS,
        "classification_type": remote_id.CLASSIFICATION_TYPE_OPTIONS,
        "category_eu": remote_id.CATEGORY_EU_OPTIONS, "class_eu": remote_id.CLASS_EU_OPTIONS,
    }
    assert wire.ENUMS == {name: [o["value"] for o in options] for name, options in lists.items()}
    for name in ("UAS_ID_MAX", "OPERATOR_ID_MAX", "DESCRIPTION_MAX", "ALTITUDE_UNKNOWN",
                 "ARM_STATUS_GOOD", "ARM_STATUS_FAIL"):
        assert getattr(remote_id, name) == getattr(reference, name)


@pytest.mark.parametrize("seed", range(40))
def test_settings_resolve_identically(seed: int) -> None:
    for n in range(100):
        raw = random_identity(seed * 100 + n)
        assert _plain(outcome(wire.settings, raw)) == _plain(outcome(reference.settings, raw)), raw


@pytest.mark.parametrize("value", ODD, ids=repr)
def test_every_odd_value_in_every_field_resolves_identically(value: Any) -> None:
    for section, keys in SECTIONS.items():
        for key in keys:
            raw = {"enabled": value, "region": value, section: {key: value}}
            assert _plain(outcome(wire.settings, raw)) == _plain(outcome(reference.settings, raw))
    assert _plain(outcome(wire.settings, value)) == _plain(outcome(reference.settings, value))


def test_an_unconvertible_number_raises_alike() -> None:
    for raw in ({"system": {"area_count": float("inf")}}, {"system": {"area_radius": "-inf"}},
                {"system": {"operator_latitude": 10**400}}, {"basic_id": {"id_type": float("inf")}}):
        mine, theirs = outcome(wire.settings, raw), outcome(reference.settings, raw)
        assert mine == theirs
        assert mine[0] == "error"


MOMENTS = [
    None,
    datetime.datetime(2019, 1, 2, tzinfo=datetime.UTC),
    datetime.datetime(2019, 1, 2),
    datetime.datetime(2018, 6, 1, tzinfo=datetime.UTC),
    datetime.datetime(2019, 1, 1, 0, 0, 0, 999999, tzinfo=datetime.UTC),
    datetime.datetime(2030, 5, 5, 12, 30, tzinfo=datetime.timezone(datetime.timedelta(hours=-7))),
    datetime.datetime(2200, 1, 1, tzinfo=datetime.UTC),
]


@pytest.mark.parametrize("moment", MOMENTS, ids=repr)
def test_timestamps_match(moment: Any) -> None:
    mine, theirs = wire.system_timestamp(moment), reference.system_timestamp(moment)
    if moment is None:
        assert abs(mine - theirs) <= 1
    else:
        assert mine == theirs


@pytest.mark.parametrize("seed", range(20))
def test_messages_match_field_for_field(seed: int) -> None:
    moment = datetime.datetime(2026, 10, 25, 12, 0, tzinfo=datetime.UTC)
    for n in range(100):
        raw = random_identity(seed * 100 + n)
        targets = {"target_system": random.Random(n).choice([1, 255, True, 2.9, "7"]),
                   "target_component": 1}
        mine = outcome(wire.messages, raw, now=moment, **targets)
        theirs = outcome(reference.messages, raw, now=moment, **targets)
        assert _plain(mine) == _plain(theirs), raw
        if mine[0] == "ok":
            for (_, a), (_, b) in zip(mine[1], theirs[1], strict=True):
                assert [type(v) for v in a.values()] == [type(v) for v in b.values()]


def test_a_fixed_position_rounds_alike() -> None:
    for lat in (48.0800005, -33.12345675, 0.00000005, 89.99999995, 12.34567845):
        raw = {"system": {"operator_location_type": 2, "operator_latitude": lat,
                          "operator_longitude": -lat * 2, "operator_altitude_geo": 512.5}}
        mine = wire.messages(raw, target_system=1, target_component=1, now=MOMENTS[1])
        assert mine == reference.messages(raw, target_system=1, target_component=1, now=MOMENTS[1])


@pytest.mark.parametrize("status", [0, 1, 2, None, "0", "x", 1.9, True, 10**25, float("nan")], ids=repr)
@pytest.mark.parametrize("error", [b"", b"no transmitter\x00junk", "UTF-8 ö fail", None, b"x" * 80, 5])
def test_arm_statuses_match(status: Any, error: Any) -> None:
    assert outcome(wire.arm_status, status, error) == outcome(reference.arm_status, status, error)


# ------------------------------------------------------------- broadcast ----

class Clock:
    def __init__(self) -> None:
        self.now = 100.0

    def monotonic(self) -> float:
        return self.now


class FailingMav(RecordingMav):
    def __init__(self, exc: BaseException) -> None:
        super().__init__(raises=exc)  # type: ignore[arg-type]


def _without_timestamps(sent: list) -> list:
    return [(name, {k: v for k, v in kwargs.items() if k != "timestamp"}) for name, kwargs in sent]


@pytest.fixture
def clock(monkeypatch: pytest.MonkeyPatch) -> Clock:
    fake = Clock()
    for module in (reference_mixin, rust_side):
        monkeypatch.setattr(module, "time", fake)
    return fake


def make(kind: type, mav: Any) -> Any:
    store = VehicleStateStore()
    store.heartbeat()
    bridge = kind(store)
    bridge._target_system = 1
    bridge._target_component = 1
    bridge._conn = FakeConnection(mav)
    return bridge


MAVS = {
    "v2": RecordingMav,
    "v1": LegacyMav,
    "none": lambda: None,
    "raises": lambda: FailingMav(OSError("radio gone")),
    "raises-empty": lambda: FailingMav(RuntimeError()),
    "raises-value": lambda: FailingMav(ValueError("field too long")),
}


@pytest.mark.parametrize("mav", list(MAVS))
def test_a_broadcast_session_matches(mav: str, clock: Clock) -> None:
    results = []
    for kind in (ReferenceBridge, MavlinkBridge):
        clock.now = 100.0
        link = MAVS[mav]()
        bridge = make(kind, link)
        if mav == "none":
            bridge._conn = SimpleNamespace(mav=None)
        steps: list[Any] = [bridge.remote_id_status(), bridge.remote_id_supported()]
        steps.append(bridge.set_remote_id({"enabled": True, "basic_id": {"uas_id": "ABCD3XYZ"}}))
        bridge._send_remote_id(bridge._conn)
        clock.now = 102.26
        steps.append(bridge.remote_id_status())
        bridge._handle_remote_id_arm_status(SimpleNamespace(status=1, error=b"no operator id\x00"))
        steps.append(bridge.remote_id_status())
        clock.now = 107.5
        steps.append(bridge.remote_id_status())
        bridge._forget_remote_id_session()
        steps.append(bridge.remote_id_status())
        bridge._handle_remote_id_arm_status(SimpleNamespace())
        steps.append(bridge.remote_id_status())
        steps.append(bridge.set_remote_id({"enabled": False}))
        bridge._send_remote_id(bridge._conn)
        steps.append(bridge.remote_id_status())
        steps.append(bridge.remote_id_settings())
        sent = _without_timestamps(getattr(link, "sent", []))
        results.append((steps, sent, bridge._remote_id_error, bridge._remote_id_sent_at))
    assert results[1] == results[0]


def test_a_keyboard_interrupt_in_a_send_is_not_swallowed(clock: Clock) -> None:
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge = make(kind, FailingMav(KeyboardInterrupt()))
        bridge.set_remote_id({"enabled": True})
        with pytest.raises(KeyboardInterrupt):
            bridge._send_remote_id(bridge._conn)


def test_the_settings_handed_out_are_a_fresh_copy(clock: Clock) -> None:
    for kind in (ReferenceBridge, MavlinkBridge):
        bridge = make(kind, RecordingMav())
        bridge.set_remote_id({"enabled": True, "basic_id": {"uas_id": "ABCD3XYZ"}})
        handed = bridge.remote_id_settings()
        handed["basic_id"]["uas_id"] = "changed"
        assert bridge.remote_id_settings()["basic_id"]["uas_id"] == "ABCD3XYZ"
