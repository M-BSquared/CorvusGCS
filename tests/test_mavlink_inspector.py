"""Live Messages: per-message statistics."""
from __future__ import annotations

import json
import threading

from corvus.mavlink_inspector import MessageScope, key_for


def _awake() -> MessageScope:
    scope = MessageScope()
    scope.snapshot()
    return scope


class FakeMsg:
    def __init__(self, name="ATTITUDE", system=1, component=1, msg_id=30, **fields):
        self._name = name
        self._system = system
        self._component = component
        self._id = msg_id
        self._fields = fields
        self.fieldunits_by_name = {k: "rad" for k in fields}
        for key, value in fields.items():
            setattr(self, key, value)

    def get_type(self): return self._name
    def get_srcSystem(self): return self._system
    def get_srcComponent(self): return self._component
    def get_msgId(self): return self._id
    def get_msgbuf(self): return b"\x00" * 12
    def get_fieldnames(self): return list(self._fields)


def test_counts_and_rate_per_message_type() -> None:
    scope = _awake()
    for i in range(21):
        scope.record(FakeMsg(roll=0.1), now=100.0 + i * 0.1)
    scope.record(FakeMsg("HEARTBEAT", msg_id=0), now=100.0)
    snap = scope.snapshot(now=102.2)
    by_name = {r["name"]: r for r in snap["rows"]}
    assert by_name["ATTITUDE"]["count"] == 21
    assert 9.0 <= by_name["ATTITUDE"]["rate"] <= 11.0
    assert by_name["HEARTBEAT"]["count"] == 1
    assert snap["total"] == 22


def test_rows_are_split_by_system_and_component() -> None:
    scope = _awake()
    scope.record(FakeMsg(system=1, component=1), now=1.0)
    scope.record(FakeMsg(system=1, component=100), now=1.0)
    scope.record(FakeMsg(system=2, component=1), now=1.0)
    keys = [r["key"] for r in scope.snapshot(now=1.0)["rows"]]
    assert keys == [key_for(1, 1, "ATTITUDE"), key_for(1, 100, "ATTITUDE"),
                    key_for(2, 1, "ATTITUDE")]


def test_silent_message_reports_zero_rate() -> None:
    scope = _awake()
    for i in range(30):
        scope.record(FakeMsg(), now=10.0 + i * 0.1)
    row = scope.snapshot(now=60.0)["rows"][0]
    assert row["rate"] == 0.0
    assert row["age"] > 40


def test_fields_only_for_selected_row_and_json_safe() -> None:
    scope = _awake()
    scope.record(FakeMsg(roll=float("nan"), text=b"abc\x00\x00", arr=[1, 2, 3]), now=1.0)
    scope.record(FakeMsg("HEARTBEAT", msg_id=0, mode=4), now=1.0)
    plain = scope.snapshot(now=1.0)
    assert plain["fields"] is None
    snap = scope.snapshot(key_for(1, 1, "ATTITUDE"), now=1.0)
    json.dumps(snap)
    values = {f["name"]: f["value"] for f in snap["fields"]}
    assert values == {"roll": None, "text": "abc", "arr": [1, 2, 3]}
    assert snap["selected"] == key_for(1, 1, "ATTITUDE")


def test_unknown_selection_gives_no_fields() -> None:
    scope = _awake()
    scope.record(FakeMsg(), now=1.0)
    snap = scope.snapshot("9:9:NOPE", now=1.0)
    assert snap["fields"] is None and snap["selected"] == ""


def test_clear_and_bad_data_and_table_cap(monkeypatch) -> None:
    import corvus.mavlink_inspector as mod
    monkeypatch.setattr(mod, "MAX_KEYS", 3)
    scope = _awake()
    scope.record(FakeMsg("BAD_DATA"), now=1.0)
    assert scope.snapshot(now=1.0)["rows"] == []
    for n in range(5):
        scope.record(FakeMsg(f"M{n}"), now=1.0)
    snap = scope.snapshot(now=1.0)
    assert len(snap["rows"]) == 3 and snap["dropped"] == 2
    scope.clear()
    snap = scope.snapshot(now=1.0)
    assert snap["rows"] == [] and snap["dropped"] == 0 and snap["total"] == 0


def test_record_never_raises_on_odd_message() -> None:
    scope = _awake()
    scope.record(object())
    assert scope.snapshot()["rows"] == []


def test_concurrent_recording_and_snapshot() -> None:
    scope = _awake()
    stop = threading.Event()

    def writer() -> None:
        while not stop.is_set():
            scope.record(FakeMsg())

    t = threading.Thread(target=writer)
    t.start()
    try:
        for _ in range(200):
            scope.snapshot(key_for(1, 1, "ATTITUDE"))
    finally:
        stop.set()
        t.join()
    assert scope.snapshot()["total"] > 0


def test_nothing_is_counted_until_somebody_asks() -> None:
    scope = MessageScope()
    scope.record(FakeMsg(), now=1.0)
    assert scope.snapshot()["rows"] == []
    scope.record(FakeMsg(), now=1.0)
    assert scope.snapshot()["total"] == 1


def test_counter_sleeps_and_forgets_when_the_page_stops_asking(monkeypatch) -> None:
    import corvus.mavlink_inspector as mod
    clock = [1000.0]
    monkeypatch.setattr(mod.time, "monotonic", lambda: clock[0])
    scope = MessageScope()
    scope.snapshot()
    scope.record(FakeMsg())
    assert scope.snapshot()["total"] == 1
    clock[0] += mod.ACTIVE_FOR_S + 1
    scope.record(FakeMsg())
    clock[0] += 0.1
    assert scope.snapshot()["rows"] == []
    scope.record(FakeMsg())
    assert scope.snapshot()["total"] == 1
