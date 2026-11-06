#!/usr/bin/env python3
"""Dump how the Python parameter engine behaves into fixtures for the Rust one.

    python tools/dump_protocol_cases.py [--check]

Drives ``corvus_link.params.ParamEngine`` (the reference) through scripted
scenarios against a stub bridge and a simulated vehicle, on a fake clock, and
records everything it did: what it sent, what it told the listeners, the
errors it set, what it returned and what its cache holds. The scenarios and
their results are written to ``src/core/tests/data/params_cases.json``; the
Rust engine in ``corvus_core::protocol::params`` plays the same scenarios and
must end up in the same place.
"""
from __future__ import annotations

import json
import pathlib
import struct
import sys
import time
from unittest import mock

from corvus_link import params as P

OUT = pathlib.Path(__file__).resolve().parent.parent.parent / "src" / "core" / "tests" / "data" / "params_cases.json"


def f32(x: float) -> float:
    return struct.unpack("<f", struct.pack("<f", x))[0]


def bits_to_float(bits: int) -> float:
    return struct.unpack("<f", struct.pack("<I", bits & 0xFFFFFFFF))[0]


class Clock:
    t = 1000.0


class Flag:
    def __init__(self, value: bool) -> None:
        self.value = value

    def is_set(self) -> bool:
        return self.value


class Store:
    def __init__(self, armed: bool) -> None:
        self.armed = armed

    def get_snapshot(self) -> dict:
        return {"armed": self.armed}


class Msg:
    def __init__(self, name: str, value: float, ptype: int, index: int, count: int, raw_bits: int | None) -> None:
        self.param_id = name.encode().ljust(16, b"\0")
        self.param_value = value
        self.param_type = ptype
        self.param_index = index
        self.param_count = count
        bits = raw_bits if raw_bits is not None else struct.unpack("<I", struct.pack("<f", value))[0]
        self._buf = b"\xfd" + bytes(9) + struct.pack("<I", bits)

    def get_msgbuf(self) -> bytes:
        return self._buf


class Bridge:
    """A stand-in for MavlinkBridge: the methods the engine calls on it."""

    def __init__(self, sc: dict) -> None:
        self.sc = sc
        self.engine = P.ParamEngine()
        self._conn = object()
        self._store = Store(sc.get("armed", False))
        self._running = Flag(True)
        self._stop_event = Flag(False)
        self.sent: list = []
        self.statuses: list = []
        self.errors: list = []
        self.console: list = []
        self.last_error = ""
        self.watchdog_started = 0
        self.vehicle = [dict(p) for p in sc["vehicle"]["params"]]

    # --- what the engine asks of the bridge ---
    def _connection_ready(self):
        return self.sc.get("ready", True)

    def _set_command_error(self, text):
        self.last_error = text
        self.errors.append(text)

    def get_last_command_error(self):
        return self.last_error

    def _console_publish(self, name, text, level):
        self.console.append([name, text, level])

    def _start_param_watchdog(self):
        self.watchdog_started += 1

    def _interruptible_sleep(self, seconds):
        self.ticks = getattr(self, "ticks", 0) + 1
        if self.ticks >= self.sc.get("max_ticks", 400):
            self._running.value = False
        time.sleep(seconds)

    def _param_download_budget_s(self):
        return 180.0 if self.sc.get("slow") else 30.0

    def _is_slow_link(self):
        return bool(self.sc.get("slow"))

    def _param_encoding(self):
        return self.sc.get("encoding", "bytewise")

    def _notify_param_listeners(self, status):
        self.statuses.append(dict(status))

    def request_param(self, name):
        return self.engine.request_param(self, name)

    def _wait_for_param(self, name, timeout=2.0):
        return self.engine.wait_for_param(name, timeout)

    def fetch_params(self, names, timeout=4.0, fresh=False):
        return self.engine.fetch_params(self, names, timeout, fresh)

    def set_param(self, name, value):
        return self.engine.set_param(self, name, value)

    # --- the link and the simulated vehicle on it ---
    def _link_send(self, conn, method, *args):
        if conn is not self._conn:
            return False
        if self.sc.get("send_fails") == method:
            raise OSError("write failed")
        shown = [a.decode() if isinstance(a, bytes) else a for a in args]
        self.sent.append([method, [s.rstrip("\0") if isinstance(s, str) else s for s in shown]])
        self.react(method, args)
        return True

    def deliver(self, p: dict, index: int, count: int) -> None:
        value = bits_to_float(p["bits"]) if "bits" in p else f32(p["value"])
        self.engine.handle_param_value(self, Msg(p["name"], value, p["type"], index, count, p.get("bits")))

    def react(self, method, args):
        veh = self.sc["vehicle"]
        count = veh.get("count", len(self.vehicle))
        if method == "param_request_list_send":
            for i, p in enumerate(self.vehicle):
                if i not in veh.get("drop", []):
                    self.deliver(p, i, count)
        elif method == "param_request_read_send" and veh.get("answer_reads", True):
            name = args[0].decode().rstrip("\0") if args[0] else ""
            index = args[1]
            for i, p in enumerate(self.vehicle):
                if (name and p["name"] == name) or (not name and i == index):
                    if i not in veh.get("never", []):
                        self.deliver(p, i, count)
        elif method == "param_set_send":
            name = args[0].decode().rstrip("\0")
            mode = veh.get("echo", "ok")
            for i, p in enumerate(self.vehicle):
                if p["name"] == name and mode != "none":
                    if mode == "ok":
                        p["bits"] = struct.unpack("<I", struct.pack("<f", args[1]))[0] if self.sc.get("encoding", "bytewise") == "bytewise" and p["type"] <= 6 else None
                        if p["bits"] is None:
                            del p["bits"]
                            p["value"] = args[1]
                    self.deliver(p, i, count)


def entry_json(e) -> dict:
    return {"name": e.name, "value": e.value, "type": e.type, "index": e.index, "count": e.count}


def clean(x):
    """JSON-safe: NaN becomes the text "nan", tuples become lists."""
    if isinstance(x, float) and x != x:
        return "nan"
    if isinstance(x, dict):
        return {k: clean(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [clean(v) for v in x]
    return x


def unclean(x):
    if x == "nan":
        return float("nan")
    if isinstance(x, dict):
        return {k: unclean(v) for k, v in x.items()}
    if isinstance(x, list):
        return [unclean(v) for v in x]
    return x


def play(sc: dict) -> dict:
    sc = unclean(sc)
    clock = Clock()
    br = Bridge(sc)
    results = []
    with mock.patch.object(time, "monotonic", lambda: clock.t), \
            mock.patch.object(time, "sleep", lambda s: setattr(clock, "t", clock.t + s)):
        for step in sc["steps"]:
            op = step["op"]
            before = (len(br.sent), len(br.statuses), len(br.errors), len(br.console), clock.t)
            result = None
            eng = br.engine
            if op == "request_list":
                result = eng.request_param_list(br)
            elif op == "request":
                result = eng.request_param(br, step["name"])
            elif op == "deliver":
                p = step["param"]
                eng.handle_param_value(br, Msg(p["name"], p.get("value", 0.0), p["type"], p["index"], p["count"], p.get("bits")))
            elif op == "watchdog":
                eng.run_watchdog(br)
            elif op == "wait_for":
                e = eng.wait_for_param(step["name"], step.get("timeout", 2.0))
                result = None if e is None else entry_json(e)
            elif op == "set":
                result = eng.set_param(br, step["name"], step["value"])
            elif op == "fetch":
                result = eng.fetch_params(br, step["names"], step.get("timeout", 4.0), step.get("fresh", False))
            elif op == "verify":
                result = eng.verify_params(br, step["targets"], step.get("names"), step.get("rewrite", True), step.get("timeout", 4.0))
            elif op == "upload":
                ok = eng.begin_upload(br, step["params"])
                result = {"begun": ok}
                if ok:
                    eng.run_upload(br, step["params"])
                    result["result"] = eng.upload_result()
            elif op == "abort":
                eng.abort()
            elif op == "reset":
                eng.reset()
            elif op == "set_state":
                eng.state = step["state"]
            else:
                raise SystemExit(op)
            results.append({
                "result": result,
                "sent": br.sent[before[0]:],
                "statuses": br.statuses[before[1]:],
                "errors": br.errors[before[2]:],
                "console": br.console[before[3]:],
                "elapsed": round(clock.t - before[4], 6),
            })
    eng = br.engine
    return clean({
        "steps": results,
        "final": {
            "state": eng.state, "count": eng.count, "received": eng.received, "status": eng.status(),
            "params": eng.get_params(), "watchdog_started": br.watchdog_started,
            "last_error": br.last_error, "cache": {k: entry_json(v) for k, v in eng.cache.items()},
            "vehicle": br.vehicle,
        },
    })


def P_(name, value=0.0, ptype=9, **kw):
    return {"name": name, "value": value, "type": ptype, **kw}


BASE6 = [P_("A", 1.5), P_("B", 2.5), P_("C", 3.5), P_("D", 4.5), P_("E", 5.5), P_("F", 6.5)]
INTS = [P_("MC_ROLL_P", 6.5), P_("SYS_AUTOSTART", 0.0, 6, bits=4001), P_("SDLOG_MODE", 0.0, 6, bits=0xFFFFFFFF),
        P_("SENS_FLAGS", 0.0, 2, bits=0x000000FE), P_("COM_RC_LOSS_T", 0.5)]


def scenarios() -> list[dict]:
    veh = lambda params, **kw: {"params": params, **kw}  # noqa: E731
    return [
        {"name": "download_complete", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"}, {"op": "request_list"}]},
        {"name": "download_not_connected", "ready": False, "vehicle": veh(BASE6), "steps": [{"op": "request_list"}]},
        {"name": "download_send_fails", "send_fails": "param_request_list_send", "vehicle": veh(BASE6),
         "steps": [{"op": "request_list"}]},
        {"name": "download_with_loss_recovered", "vehicle": veh(BASE6, drop=[1, 4]), "steps": [
            {"op": "request_list"}, {"op": "watchdog"}]},
        {"name": "download_with_loss_recovered_slow_link", "slow": True, "vehicle": veh(BASE6, drop=[2]), "steps": [
            {"op": "request_list"}, {"op": "watchdog"}]},
        {"name": "download_gives_up_incomplete", "vehicle": veh(BASE6, drop=[0, 5], answer_reads=False), "steps": [
            {"op": "request_list"}, {"op": "watchdog"}, {"op": "request_list"}]},
        {"name": "download_quiet_vehicle_times_out", "max_ticks": 70, "vehicle": veh([], count=4), "steps": [
            {"op": "request_list"}, {"op": "watchdog"}]},
        {"name": "values_are_validated", "vehicle": veh([]), "steps": [
            {"op": "deliver", "param": {"name": "", "value": 1.0, "type": 9, "index": 0, "count": 3}},
            {"op": "deliver", "param": {"name": "BAD_TYPE", "value": 1.0, "type": 0, "index": 0, "count": 3}},
            {"op": "deliver", "param": {"name": "BAD_TYPE2", "value": 1.0, "type": 11, "index": 0, "count": 3}},
            {"op": "deliver", "param": {"name": "BAD_INDEX", "value": 1.0, "type": 9, "index": 5, "count": 3}},
            {"op": "deliver", "param": {"name": "BAD_COUNT", "value": 1.0, "type": 9, "index": 0, "count": 20000}},
            {"op": "deliver", "param": {"name": "NAN", "value": float("nan"), "type": 9, "index": 0, "count": 3}},
            {"op": "deliver", "param": {"name": "_HASH_CHECK", "value": 1.0, "type": 6, "index": 65535, "count": 3}},
            {"op": "deliver", "param": {"name": "OK_NO_INDEX", "value": 2.5, "type": 9, "index": 65535, "count": 0}},
            {"op": "deliver", "param": {"name": "OK_1", "value": 2.5, "type": 9, "index": 0, "count": 2}},
            {"op": "deliver", "param": {"name": "OK_1", "value": 3.5, "type": 9, "index": 0, "count": 2}},
            {"op": "deliver", "param": {"name": "OK_2", "value": 4.5, "type": 9, "index": 1, "count": 2}}]},
        {"name": "px4_integers_arrive_as_their_bytes", "encoding": "bytewise", "vehicle": veh(INTS), "steps": [
            {"op": "request_list"}]},
        {"name": "ardupilot_integers_arrive_as_numbers", "encoding": "c_cast",
         "vehicle": veh([P_("SERIAL1_BAUD", 57.0, 4), P_("BATT_CAPACITY", 5000.0, 6), P_("ANGLE_MAX", 3000.0, 4)]),
         "steps": [{"op": "request_list"}]},
        {"name": "named_read_and_wait", "vehicle": veh(BASE6), "steps": [
            {"op": "request", "name": "C"}, {"op": "wait_for", "name": "C"}, {"op": "wait_for", "name": "ZZ", "timeout": 0.1},
            {"op": "request", "name": "x" * 17}, {"op": "request", "name": ""}]},
        {"name": "set_confirmed", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"}, {"op": "set", "name": "B", "value": 9.25}]},
        {"name": "set_integer_px4", "encoding": "bytewise", "vehicle": veh(INTS), "steps": [
            {"op": "request_list"}, {"op": "set", "name": "SYS_AUTOSTART", "value": 4002.0},
            {"op": "set", "name": "SDLOG_MODE", "value": -1.0}, {"op": "set", "name": "SYS_AUTOSTART", "value": 4002.5},
            {"op": "set", "name": "SENS_FLAGS", "value": 300.0}]},
        {"name": "set_refusals", "vehicle": veh(BASE6), "steps": [
            {"op": "set", "name": "A", "value": float("nan")}, {"op": "set", "name": "y" * 20, "value": 1.0},
            {"op": "set", "name": "NOPE", "value": 1.0}]},
        {"name": "set_while_armed", "armed": True, "vehicle": veh(BASE6), "steps": [
            {"op": "set", "name": "A", "value": 1.0}]},
        {"name": "set_not_connected", "ready": False, "vehicle": veh(BASE6), "steps": [
            {"op": "set", "name": "A", "value": 1.0}]},
        {"name": "set_echo_differs", "vehicle": veh(BASE6, echo="wrong"), "steps": [
            {"op": "request_list"}, {"op": "set", "name": "A", "value": 3.0}]},
        {"name": "set_unknown_parameter_is_requested_first", "vehicle": veh(BASE6), "steps": [
            {"op": "set", "name": "D", "value": 7.5}]},
        {"name": "fetch_a_few", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"}, {"op": "fetch", "names": ["A", "C"]}, {"op": "fetch", "names": ["A", "ZZ"], "fresh": True},
            {"op": "fetch", "names": []}]},
        {"name": "fetch_without_a_download", "vehicle": veh(BASE6, never=[2]), "steps": [
            {"op": "fetch", "names": ["B", "C", "E"], "timeout": 1.0},
            {"op": "fetch", "names": ["B", "C", "E"], "timeout": 1.0, "fresh": True},
            {"op": "fetch", "names": ["C"], "timeout": 1.0}]},
        {"name": "verify_rewrites_what_differs", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"},
            {"op": "verify", "targets": [{"name": "A", "value": 1.5}, {"name": "B", "value": 8.0}], "names": ["C"]}]},
        {"name": "verify_refusals", "vehicle": veh(BASE6), "steps": [
            {"op": "verify", "targets": [], "names": []},
            {"op": "verify", "targets": [{"name": "A", "value": 1.0}], "rewrite": False},
            {"op": "verify", "targets": [{"name": "ZZ", "value": 1.0}], "rewrite": False, "timeout": 1.0}]},
        {"name": "verify_armed_is_not_rewritten", "armed": True, "vehicle": veh(BASE6), "steps": [
            {"op": "verify", "targets": [{"name": "A", "value": 99.0}]}]},
        {"name": "upload_all_confirmed", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"},
            {"op": "upload", "params": [{"name": "A", "value": 10.0}, {"name": "B", "value": 20.0}]}]},
        {"name": "upload_refusals", "vehicle": veh(BASE6), "steps": [
            {"op": "upload", "params": []}, {"op": "set_state", "state": "downloading"},
            {"op": "upload", "params": [{"name": "A", "value": 1.0}]}]},
        {"name": "abort_and_reset", "vehicle": veh(BASE6), "steps": [
            {"op": "request_list"}, {"op": "abort"}, {"op": "reset"}]},
    ]


def cases() -> list[dict]:
    out = []
    for sc in scenarios():
        print("scenario", sc["name"], file=sys.stderr, flush=True)
        sc = clean(sc)
        out.append({"scenario": sc, "expected": play(sc)})
    return out


def main(argv: list[str]) -> int:
    text = json.dumps({"constants": {k: getattr(P, k) for k in dir(P) if k.startswith("PARAM_")}, "cases": cases()},
                      indent=1, allow_nan=False) + "\n"
    if "--check" in argv:
        if not OUT.is_file() or OUT.read_text() != text:
            print("stale: run tools/dump_protocol_cases.py", file=sys.stderr)
            return 1
        return 0
    OUT.write_text(text)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
