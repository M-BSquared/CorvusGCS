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
import threading
import sys
import time
from unittest import mock

from corvus_link import params as P

DATA = pathlib.Path(__file__).resolve().parent.parent.parent / "src" / "core" / "tests" / "data"
OUT = DATA / "params_cases.json"


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
    if x == float("inf") and isinstance(x, float):
        return "inf"
    if x == float("-inf") and isinstance(x, float):
        return "-inf"
    if isinstance(x, dict):
        return {k: clean(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [clean(v) for v in x]
    return x


def unclean(x):
    if x == "nan":
        return float("nan")
    if x == "inf":
        return float("inf")
    if x == "-inf":
        return float("-inf")
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




# ---------------------------------------------------------------- logs ----

class SendLock:
    def acquire(self):
        pass

    def release(self):
        pass


class Mav:
    """``bridge._conn.mav``: records every ``*_send`` call, may raise."""

    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def __getattr__(self, method):
        def call(*args):
            if self.bridge.sc.get("send_fails") == method:
                raise OSError("boom")
            self.bridge.sent.append([method, [a for a in args[2:]] if len(args) >= 2 else []])
        return call


class SimpleBridge:
    """What the small request protocols ask of a bridge."""

    def __init__(self, sc: dict) -> None:
        self.sc = sc
        self.sent: list = []
        self.errors: list = []
        self.console: list = []
        self.last_error = ""
        self._send_lock = SendLock()
        self._target_system = 1
        self._target_component = 1
        self._conn = type("Conn", (), {})()
        self._conn.mav = Mav(self)
        self._store = Store(sc.get("armed", False))

    def _connection_ready(self):
        return self.sc.get("ready", True)

    def _set_command_error(self, text):
        self.last_error = text
        self.errors.append(text)

    def get_last_command_error(self):
        return self.last_error

    def _console_publish(self, name, text, level):
        self.console.append([name, text, level])


def play_simple(sc: dict, engine, ops: dict) -> dict:
    br = SimpleBridge(sc)
    results = []
    for step in sc["steps"]:
        before = (len(br.sent), len(br.errors), len(br.console))
        result = ops[step["op"]](engine, br, step)
        results.append({"result": result, "sent": br.sent[before[0]:], "errors": br.errors[before[1]:],
                        "console": br.console[before[2]:]})
    return clean({"steps": results, "final": {"last_error": br.last_error}})


def logs_scenarios() -> list[dict]:
    return [
        {"name": "list_ok", "steps": [{"op": "list", "start": 0, "end": 65535}, {"op": "list", "start": 3, "end": 9}]},
        {"name": "list_not_connected", "ready": False, "steps": [{"op": "list", "start": 0, "end": 5}]},
        {"name": "list_send_fails", "send_fails": "log_request_list_send", "steps": [{"op": "list", "start": 0, "end": 5}]},
        {"name": "data_ok", "steps": [{"op": "data", "id": 2, "offset": 1000, "count": 90}]},
        {"name": "data_not_connected", "ready": False, "steps": [{"op": "data", "id": 2, "offset": 0, "count": 90}]},
        {"name": "data_send_fails", "send_fails": "log_request_data_send",
         "steps": [{"op": "data", "id": 2, "offset": 0, "count": 90}]},
        {"name": "erase_ok", "steps": [{"op": "erase"}]},
        {"name": "erase_armed", "armed": True, "steps": [{"op": "erase"}]},
        {"name": "erase_not_connected", "ready": False, "steps": [{"op": "erase"}]},
        {"name": "erase_send_fails", "send_fails": "log_erase_send", "steps": [{"op": "erase"}]},
        {"name": "end_ok_and_best_effort", "steps": [{"op": "end"}]},
        {"name": "end_not_connected", "ready": False, "steps": [{"op": "end"}]},
        {"name": "end_send_fails", "send_fails": "log_request_end_send", "steps": [{"op": "end"}]},
    ]


def logs_cases() -> list[dict]:
    from corvus_link import logs as L

    ops = {
        "list": lambda e, b, st: e.request_list(b, st["start"], st["end"]),
        "data": lambda e, b, st: e.request_data(b, st["id"], st["offset"], st["count"]),
        "erase": lambda e, b, st: e.erase(b),
        "end": lambda e, b, st: e.request_end(b),
    }
    return [{"scenario": sc, "expected": play_simple(sc, L.LogEngine(), ops)} for sc in logs_scenarios()]


# --------------------------------------------------------------- shell ----

class ShellMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def serial_control_send(self, *args):
        if self.bridge.sc.get("send_fails") == "serial_control_send":
            raise OSError("boom")
        device, flags, timeout, baud, count, data = args[:6]
        row = ["serial_control_send", [device, flags, timeout, baud, count, bytes(data).hex()] + list(args[6:])]
        self.bridge.sent.append(row)


class Dialect:
    def __init__(self, sc) -> None:
        self.label = sc.get("label", "PX4")
        self.supports_shell = sc.get("supports_shell", True)


class ShellBridge(SimpleBridge):
    def __init__(self, sc: dict) -> None:
        super().__init__(sc)
        self._shell_lock = SendLock()
        self._dialect = Dialect(sc)
        self._conn = type("Conn", (), {})()
        self._conn.mav = ShellMav(self)
        self.entries: list = []
        self.failures: list = []

    def _command_failure(self, action, result):
        self.failures.append([action, result])
        return False

    def _ack_is_for_us(self, msg):
        return self.sc.get("ack_ours", True)


class SerialMsg:
    def __init__(self, device, count, data) -> None:
        self.device = device
        self.count = count
        self.data = list(data) + [0] * (70 - len(data))


def shell_scenarios() -> list[dict]:
    def text(n):  # a multi chunk command
        return ("x" * n)
    return [
        {"name": "command_ok", "steps": [{"op": "command", "text": "ls"}, {"op": "command", "text": "top\n"}]},
        {"name": "command_chunks", "steps": [{"op": "command", "text": text(150)}]},
        {"name": "command_multibyte", "steps": [{"op": "command", "text": "echo €ä🚀"}]},
        {"name": "command_unsupported", "supports_shell": False, "label": "ArduPilot", "steps": [{"op": "command", "text": "ls"}]},
        {"name": "command_not_ready", "ready": False, "steps": [{"op": "command", "text": "ls"}]},
        {"name": "command_send_fails", "send_fails": "serial_control_send", "steps": [
            {"op": "command", "text": "ls"}, {"op": "state"}]},
        {"name": "write_raw", "steps": [{"op": "write", "text": "abc"}, {"op": "write", "text": "def"}, {"op": "state"}]},
        {"name": "write_refusals", "limits": {"write_max": 10}, "steps": [
            {"op": "write", "text": ""}, {"op": "write", "text": "x" * 11}]},
        {"name": "write_unsupported", "supports_shell": False, "label": "ArduPilot", "steps": [{"op": "write", "text": "a"}]},
        {"name": "write_not_ready", "ready": False, "steps": [{"op": "write", "text": "a"}]},
        {"name": "write_send_fails", "send_fails": "serial_control_send", "steps": [{"op": "write", "text": "a"}]},
        {"name": "lines_are_assembled", "steps": [
            {"op": "command", "text": "ls"},
            {"op": "recv", "device": 10, "data": "ab\r\ncd"}, {"op": "recv", "device": 10, "data": "ef\n\nnsh> "},
            {"op": "recv", "device": 10, "data": "t\x08\x08op\n"}, {"op": "state"}]},
        {"name": "line_cap_flushes", "limits": {"line_max": 10}, "steps": [
            {"op": "command", "text": "ls"}, {"op": "recv", "device": 10, "data": "y" * 12}, {"op": "state"}]},
        {"name": "utf8_split_across_chunks", "steps": [
            {"op": "command", "text": "ls"},
            {"op": "recv_bytes", "device": 10, "data": [0xE2, 0x82]}, {"op": "recv_bytes", "device": 10, "data": [0xAC, 0x0A]},
            {"op": "recv_bytes", "device": 10, "data": [0x6F, 0x6B, 0xFF, 0x21, 0x0A]}]},
        {"name": "ignored_input", "steps": [
            {"op": "recv", "device": 10, "data": "nobody asked\n"},
            {"op": "command", "text": "ls"}, {"op": "recv", "device": 3, "data": "other device\n"},
            {"op": "recv_count", "device": 10, "count": 0}, {"op": "recv_count", "device": 10, "count": 71},
            {"op": "state"}]},
        {"name": "not_our_ack", "ack_ours": False, "steps": [
            {"op": "command", "text": "ls"}, {"op": "recv", "device": 10, "data": "x\n"}]},
        {"name": "raw_stream_with_backlog_and_subscribers", "limits": {"backlog": 20}, "steps": [
            {"op": "add_sub"}, {"op": "write", "text": "a"}, {"op": "recv", "device": 10, "data": "line one\nline two\n"},
            {"op": "recv", "device": 10, "data": "line three\n"}, {"op": "add_sub"}, {"op": "recv", "device": 10, "data": "!"},
            {"op": "remove_sub", "which": 0}, {"op": "recv", "device": 10, "data": "?"}, {"op": "state"}]},
        {"name": "stop_closes_the_session", "steps": [
            {"op": "write", "text": "a"}, {"op": "recv", "device": 10, "data": "out\n"}, {"op": "stop"}, {"op": "state"}]},
        {"name": "reset_forgets_partial_state", "steps": [
            {"op": "command", "text": "ls"}, {"op": "recv", "device": 10, "data": "part"}, {"op": "reset"}, {"op": "state"}]},
    ]


def shell_cases() -> list[dict]:
    from types import SimpleNamespace

    from corvus_link import shell as S

    out = []
    for sc in shell_scenarios():
        lim = sc.get("limits", {})
        env = SimpleNamespace(SHELL_LINE_MAX_CHARS=lim.get("line_max", 8192), SHELL_BACKLOG_CHARS=lim.get("backlog", 65536),
                              SHELL_WRITE_MAX_CHARS=lim.get("write_max", 4096))
        eng = S.ShellEngine(env)
        br = ShellBridge(sc)
        subs: list = []
        results = []
        for step in sc["steps"]:
            before = (len(br.sent), len(br.errors), len(br.console), len(br.failures), len(br.entries))
            op = step["op"]
            result = None
            if op == "command":
                result = eng.send_command(br, step["text"])
            elif op == "write":
                result = eng.write(br, step["text"])
            elif op == "recv":
                eng.handle(br, SerialMsg(step["device"], len(step["data"].encode()), step["data"].encode()))
            elif op == "recv_bytes":
                eng.handle(br, SerialMsg(step["device"], len(step["data"]), bytes(step["data"])))
            elif op == "recv_count":
                eng.handle(br, SerialMsg(step["device"], step["count"], b"abc"))
            elif op == "add_sub":
                idx = len(subs)
                fn = (lambda e, idx=idx: br.entries.append([idx, e["text"]]))
                subs.append(fn)
                result = eng.add_sub(br, fn)
            elif op == "remove_sub":
                eng.remove_sub(br, subs[step["which"]])
            elif op == "stop":
                eng.stop(br)
            elif op == "reset":
                eng.reset(br)
            elif op == "state":
                result = {"active": eng.active, "raw": eng.raw, "buffer": eng.buffer, "backlog": eng.backlog}
            else:
                raise SystemExit(op)
            results.append({"result": result, "sent": br.sent[before[0]:], "errors": br.errors[before[1]:],
                            "console": br.console[before[2]:], "failures": br.failures[before[3]:],
                            "entries": br.entries[before[4]:]})
        out.append({"scenario": sc, "expected": clean({"steps": results, "final": {
            "active": eng.active, "raw": eng.raw, "buffer": eng.buffer, "backlog": eng.backlog,
            "last_error": br.last_error}})})
    return out


# ----------------------------------------------------------------- ftp ----

class FtpError(Exception):
    def __init__(self, message: str, code: int = 0) -> None:
        super().__init__(message)
        self.message = message
        self.code = code


class FtpLock:
    def __init__(self) -> None:
        self.held = False

    def acquire(self, blocking=True):
        if self.held:
            return False
        self.held = True
        return True

    def release(self):
        self.held = False


class FtpMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def file_transfer_protocol_send(self, network, ts, tc, payload):
        self.bridge.on_send(bytes(payload))


class FtpBridge(SimpleBridge):
    def __init__(self, sc: dict, eng) -> None:
        super().__init__(sc)
        self.engine = eng
        self._ftp_lock = FtpLock()
        self._stop_event = Flag(False)
        self._conn = None if sc.get("not_connected") else type("Conn", (), {})()
        if self._conn is not None:
            self._conn.mav = FtpMav(self)
            self._conn.source_system = 255
            self._conn.source_component = 0
        self.frames: list = []
        self.opens = 0
        self.burst_count = 0
        self.files = {k: bytes.fromhex(v) if isinstance(v, str) else v for k, v in sc.get("files", {}).items()}

    def _is_slow_link(self):
        return bool(self.sc.get("slow"))

    def on_send(self, p: bytes):
        seq = p[0] | p[1] << 8
        session, opcode, size = p[2], p[3], p[4]
        offset = int.from_bytes(p[8:12], "little")
        data = p[12:12 + size]
        self.frames.append([seq, session, opcode, size, offset, data.hex()])
        self.vehicle(seq, session, opcode, size, offset, data)

    def reply(self, seq, session, req_op, opcode, offset=0, data=b"", burst_complete=False):
        buf = bytearray(251)
        buf[0:2] = ((seq + 1) & 0xFFFF).to_bytes(2, "little")
        buf[2] = session
        buf[3] = opcode
        buf[4] = len(data)
        buf[5] = req_op
        buf[6] = 1 if burst_complete else 0
        buf[8:12] = offset.to_bytes(4, "little")
        buf[12:12 + len(data)] = data
        msg = type("M", (), {})()
        msg.target_system = 255
        msg.target_component = 0
        msg.payload = list(buf)
        self.engine.handle_message(self, msg)

    def nak(self, seq, session, req_op, code, offset=0):
        self.reply(seq, session, req_op, 129, offset, bytes([code]))

    def vehicle(self, seq, session, opcode, size, offset, data):
        veh = self.sc.get("vehicle", {})
        if veh.get("silent"):
            return
        if opcode == 4:  # open
            self.opens += 1
            path = data.decode()
            if veh.get("no_sessions_once") and self.opens == 1:
                return self.nak(seq, 0, 4, 5)
            if path not in self.files:
                return self.nak(seq, 0, 4, 10)
            n = veh.get("report_size", len(self.files[path]))
            self.current = self.files[path]
            return self.reply(seq, 1, 4, 128, 0, n.to_bytes(4, "little"))
        if opcode in (1, 2):
            return self.reply(seq, session, opcode, 128)
        if opcode == 14:
            path = data.decode()
            if path not in self.files:
                return self.nak(seq, 0, 14, 10)
            if veh.get("short_crc"):
                return self.reply(seq, 0, 14, 128, 0, b"\x01")
            import zlib
            return self.reply(seq, 0, 14, 128, 0, zlib.crc32(self.files[path]).to_bytes(4, "little"))
        if opcode == 15:  # burst read
            self.burst_count += 1
            chunk, f = 239, self.current
            per_burst = veh.get("chunks_per_burst", 1000)
            drop = veh.get("drop_first_burst", [])
            pos, n = offset, 0
            while True:
                if pos >= len(f):
                    return self.nak(seq, session, 15, 6, len(f))
                if n >= per_burst:
                    return
                part = f[pos:pos + chunk]
                done = pos + len(part) >= len(f)
                if not (self.burst_count == 1 and n in drop):
                    self.reply(seq, session, 15, 128, pos, part, burst_complete=done and not veh.get("no_eof_flag"))
                pos += len(part)
                n += 1
                if done:
                    if veh.get("eof_after"):
                        self.nak(seq, session, 15, 6, len(f))
                    return


def ftp_scenarios() -> list[dict]:
    body = lambda n: bytes((i * 7 + 3) % 251 for i in range(n)).hex()  # noqa: E731
    big = {"/fs/a.bin": body(100), "/fs/b.bin": body(1000), "/fs/c.bin": body(700), "/fs/d.bin": body(5000)}
    return [
        {"name": "read_small", "files": big, "steps": [{"op": "read", "path": "/fs/a.bin"}]},
        {"name": "read_multichunk_with_progress", "files": big, "steps": [{"op": "read", "path": "/fs/b.bin"}]},
        {"name": "read_loss_recovers", "files": big, "vehicle": {"drop_first_burst": [1]}, "steps": [
            {"op": "read", "path": "/fs/c.bin"}]},
        {"name": "read_loss_slow_link", "slow": True, "files": big, "vehicle": {"drop_first_burst": [0, 2]}, "steps": [
            {"op": "read", "path": "/fs/c.bin"}]},
        {"name": "read_in_short_bursts", "files": big, "vehicle": {"chunks_per_burst": 2, "no_eof_flag": True}, "steps": [
            {"op": "read", "path": "/fs/b.bin"}]},
        {"name": "read_too_large", "files": big, "limits": {"max_file": 1000}, "steps": [{"op": "read", "path": "/fs/d.bin"}]},
        {"name": "read_not_found", "files": big, "steps": [{"op": "read", "path": "/fs/none"}]},
        {"name": "read_no_sessions_then_reset", "files": big, "vehicle": {"no_sessions_once": True}, "steps": [
            {"op": "read", "path": "/fs/a.bin"}]},
        {"name": "read_unknown_size_ends_at_eof", "files": big, "vehicle": {"report_size": 0, "eof_after": True,
                                                                           "no_eof_flag": True}, "steps": [
            {"op": "read", "path": "/fs/b.bin"}]},
        {"name": "read_path_too_long", "files": big, "steps": [{"op": "read", "path": "/" + "p" * 300}]},
        {"name": "read_not_connected", "not_connected": True, "files": big, "steps": [{"op": "read", "path": "/fs/a.bin"}]},
        {"name": "read_cancelled", "files": big, "steps": [{"op": "read", "path": "/fs/a.bin", "cancel": True}]},
        {"name": "read_stopped", "files": big, "stopped": True, "steps": [{"op": "read", "path": "/fs/a.bin"}]},
        {"name": "read_no_answer", "files": big, "vehicle": {"silent": True}, "limits": {"retries": 2}, "steps": [
            {"op": "read", "path": "/fs/a.bin"}]},
        {"name": "crc_ok", "files": big, "steps": [{"op": "crc", "path": "/fs/b.bin"}]},
        {"name": "crc_not_found", "files": big, "steps": [{"op": "crc", "path": "/fs/none"}]},
        {"name": "crc_short_reply", "files": big, "vehicle": {"short_crc": True}, "steps": [{"op": "crc", "path": "/fs/a.bin"}]},
        {"name": "crc_path_too_long", "files": big, "steps": [{"op": "crc", "path": "/" + "p" * 300}]},
        {"name": "two_reads_in_a_row_reuse_the_engine", "files": big, "steps": [
            {"op": "read", "path": "/fs/a.bin"}, {"op": "read", "path": "/fs/c.bin"}, {"op": "crc", "path": "/fs/a.bin"}]},
    ]


def ftp_cases() -> list[dict]:
    from types import SimpleNamespace

    from corvus_link import ftp as F

    out = []
    for sc in ftp_scenarios():
        lim = sc.get("limits", {})
        clock = Clock()

        def mono(clock=clock):
            clock.t += 0.05
            return clock.t

        env = SimpleNamespace(FtpError=FtpError, time=SimpleNamespace(monotonic=mono), FTP_REPLY_TIMEOUT_S=1.0,
                              FTP_REPLY_TIMEOUT_S_SLOW=3.0, FTP_MAX_FILE_BYTES=lim.get("max_file", 8 * 1024 * 1024),
                              FTP_BURST_ROUNDS=25, FTP_REQUEST_RETRIES=lim.get("retries", 3))
        eng = F.FtpEngine(env)
        br = FtpBridge(sc, eng)
        br._stop_event.value = bool(sc.get("stopped"))
        results = []
        for step in sc["steps"]:
            before = len(br.frames)
            progress: list = []
            cancel = Flag(True) if step.get("cancel") else None
            if cancel is not None:
                cancel.set = lambda: None
            try:
                if step["op"] == "read":
                    cb = (lambda got, total, progress=progress: progress.append([got, total]))
                    if cancel is not None:
                        import threading
                        ev = threading.Event()
                        ev.set()
                        data = eng.read_file(br, step["path"], cb, ev)
                    else:
                        data = eng.read_file(br, step["path"], cb)
                    result = {"ok": bytes(data).hex()}
                else:
                    result = {"ok": eng.file_crc32(br, step["path"])}
            except FtpError as exc:
                result = {"error": [exc.message, exc.code]}
            results.append({"result": result, "frames": br.frames[before:], "progress": progress})
        out.append({"scenario": sc, "expected": clean({"steps": results, "final": {
            "listening": eng.listening, "inbox": len(eng.inbox), "busy": br._ftp_lock.held}})})
    return out


# -------------------------------------------------------------- flight ----

class FlightLock:
    """The bridge's command lock as the flight code sees it: contexts, a mark
    and a count of the aborts other threads registered."""

    def __init__(self) -> None:
        self.aborts = 0

    def priority(self):
        return self

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def abort_mark(self) -> int:
        return self.aborts

    def aborted_since(self, mark: int) -> bool:
        return self.aborts > mark


class FlightStop:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def is_set(self) -> bool:
        stop_at = self.bridge.sc.get("stop_at")
        return stop_at is not None and self.bridge.clock.t >= stop_at


class FlightStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def get_snapshot(self) -> dict:
        self.bridge.tick()
        state = self.bridge.state
        snap = {"connected": state["connected"], "armed": state["armed"], "mode": state["mode"]}
        if state["landed_state"] is not None:
            snap["landed_state"] = state["landed_state"]
        if state["altitude_agl"] is not None:
            snap["altitude_agl"] = state["altitude_agl"]
        return snap

    def is_stale(self, timeout: float = 2.0) -> bool:
        self.bridge.log.append(["stale_check", timeout])
        return bool(self.bridge.sc.get("stale", False))


class FlightMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def set_position_target_local_ned_send(self, *args):
        if self.bridge.sc.get("send_fails"):
            raise OSError("boom")
        self.bridge.log.append(["sent", "set_position_target_local_ned_send", list(args)])


class FlightSendLock:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def __enter__(self):
        if self.bridge.sc.get("link_lost"):
            self.bridge._conn = type("Conn", (), {})()
        return self

    def __exit__(self, *exc):
        return False


def _flight_spec(seq, command, lat, lon, alt, p1, p2, p3, p4, frame=None):
    from corvus_link import mavutil

    return {
        "frame": (mavutil.mavlink.MAV_FRAME_GLOBAL_RELATIVE_ALT if frame is None else int(frame)),
        "command": command, "current": 1 if seq == 0 else 0, "autocontinue": 1,
        "param1": float(p1), "param2": float(p2), "param3": float(p3), "param4": float(p4),
        "x_int": int(round(lat * 1e7)), "y_int": int(round(lon * 1e7)),
        "x_f": float(lat), "y_f": float(lon), "z": float(alt),
    }


def flight_bridge_class():
    """The reference flight code (tests/reference) on a scripted bridge."""
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from corvus_link import mavutil
    from tests.reference import mavlink_flight_python as ref

    class FlightBridge(ref.FlightCommandsMixin, ref.MissionFlightMixin):
        def __init__(self, sc: dict, clock: Clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self.last_error = ""
            self._operation_lock = FlightLock()
            self._stop_event = FlightStop(self)
            self._store = FlightStore(self)
            self._send_lock = FlightSendLock(self)
            self._conn = type("Conn", (), {})()
            self._conn.mav = FlightMav(self)
            self._target_system = 1
            self._target_component = 1
            self._mav_type_id = sc.get("mav_type", 2)
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            mode = sc.get("modes", "dialect")
            table = self._dialect.mode_table(self._mav_type_id, (1, 18, 0))
            self._mode_values = {} if mode == "empty" else {
                n: (c.base_mode, c.custom_mode, c.custom_sub) for n, c in table.items()}
            self._modes_unsupported = bool(sc.get("modes_unsupported", False))
            init = sc.get("initial", {})
            self.state = {"connected": True, "armed": False, "mode": "", "landed_state": None,
                          "altitude_agl": 0.0, **init}
            self._home = sc.get("home_amsl")
            self._pos_home = sc.get("position_home_amsl")
            self.pending: list = []
            self.counts: dict = {}
            self.aborted_on: set = set()

        def tick(self) -> None:
            due = [p for p in self.pending if p[0] <= self.clock.t]
            self.pending = [p for p in self.pending if p[0] > self.clock.t]
            for _, field, value in sorted(due, key=lambda p: p[0]):
                if field in ("home_amsl", "position_home_amsl"):
                    if field == "home_amsl":
                        self._home = value
                    else:
                        self._pos_home = value
                else:
                    self.state[field] = value

        @property
        def _home_alt_amsl(self):
            self.tick()
            return self._home

        @property
        def _position_home_alt_amsl(self):
            self.tick()
            return self._pos_home

        def _connection_ready(self) -> bool:
            until = self.sc.get("ready_until")
            if until is not None and self.clock.t >= until:
                return False
            return self.sc.get("ready", True)

        def _set_command_error(self, text: str) -> None:
            self.last_error = text
            self.log.append(["error", text])

        def get_last_command_error(self) -> str:
            return self.last_error

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])
            for prefix in self.sc.get("abort_on_console", []):
                if text.startswith(prefix) and prefix not in self.aborted_on:
                    self.aborted_on.add(prefix)
                    self._operation_lock.aborts += 1

        def _store_update_warning(self, text, level) -> None:
            self.log.append(["warn", text, level])

        def _command_failure(self, action, result) -> bool:
            self.log.append(["failure", action, result])
            return False

        def _request_home(self) -> None:
            self.log.append(["request_home"])
            reply = self.sc.get("home_reply")
            if reply:
                self.pending.append((self.clock.t + reply["after"], "home_amsl", reply["value"]))

        def _result_for(self, command: int, params) -> int:
            self.clock.t += self.sc.get("cost", 0.1)
            results = self.sc.get("results", {})
            plan = results.get(str(command), self.sc.get("default_result", 0))
            if isinstance(plan, list):
                n = self.counts.get(command, 0)
                self.counts[command] = n + 1
                result = plan[min(n, len(plan) - 1)]
            else:
                result = plan
            if result == 0:
                for rule in self.sc.get("reactions", []):
                    if rule["cmd"] != command:
                        continue
                    if "param" in rule:
                        index, value = rule["param"]
                        if not params[index] == value:
                            continue
                    for field, value in rule["set"].items():
                        self.pending.append((self.clock.t + rule.get("after", 0.0), field, value))
            return result

        def _send_command_and_wait(self, command, params=None, timeout=3.0, retries=2,
                                   accept_in_progress=False, interim=frozenset()):
            self.log.append(["cmd", command, list(params), timeout, retries, accept_in_progress])
            return self._result_for(command, params)

        def _send_command_int_and_wait(self, command, frame, params=None, x=0, y=0, z=0.0,
                                       timeout=3.0, retries=2):
            self.log.append(["cmdint", command, frame, list(params), x, y, z, timeout, retries])
            return self._result_for(command, params)

        def _validate_fly_to_points(self, points):
            self.log.append(["validate_points", points])
            if self.sc.get("points_error"):
                self._set_command_error(self.sc["points_error"])
                return None
            return [tuple(p) for p in points]

        @staticmethod
        def _fly_to_point_number(value):
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                return None
            return float(value)

        def _takeoff_item_position(self, first):
            return first[0], first[1]

        _build_mission_item_spec = staticmethod(_flight_spec)

        def _with_mission_home_slot(self, specs):
            if not specs or not self._dialect.mission_seq0_is_home:
                return specs
            home = self.sc.get("home", [0.0, 0.0])
            home_alt = self._home or 0.0
            placeholder = self._build_mission_item_spec(
                0, mavutil.mavlink.MAV_CMD_NAV_WAYPOINT, home[1], home[0], home_alt, 0.0, 0.0, 0.0, 0.0,
                frame=mavutil.mavlink.MAV_FRAME_GLOBAL)
            return [placeholder] + [dict(item, current=0) for item in specs]

        def _upload_mission(self, items):
            self.log.append(["upload", items])
            self.clock.t += self.sc.get("cost", 0.1)
            return self.sc.get("upload_ack", 0)

        @staticmethod
        def _mission_ack_to_result(ack_type):
            if ack_type == mavutil.mavlink.MAV_MISSION_ACCEPTED:
                return mavutil.mavlink.MAV_RESULT_ACCEPTED
            if ack_type == mavutil.mavlink.MAV_MISSION_DENIED:
                return mavutil.mavlink.MAV_RESULT_DENIED
            if ack_type in (2, 3):
                return mavutil.mavlink.MAV_RESULT_UNSUPPORTED
            return mavutil.mavlink.MAV_RESULT_FAILED

        def _note_vehicle_mission(self, plan_index) -> None:
            self.log.append(["note_mission", plan_index])

        def _mission_start_range(self, count):
            return self._dialect.mission_start_params(count)

    return FlightBridge


def flight_scenarios() -> list[dict]:
    GUIDED = [{"cmd": 176, "param": [1, 4.0], "set": {"mode": "GUIDED"}, "after": 0.3}]
    ARM = [{"cmd": 400, "param": [0, 1.0], "set": {"armed": True}, "after": 0.5},
           {"cmd": 400, "param": [0, 0.0], "set": {"armed": False}, "after": 0.5}]
    AP = {"stack": "ardupilot", "mav_type": 2}
    air = {"landed_state": 2, "altitude_agl": 30.0, "armed": True}
    pts = [[48.1, 11.5, 30.0]]
    two = [[48.1, 11.5, 30.0], [48.2, 11.6, 40.0]]
    return [
        {"name": "mode_ok", "initial": {}, "steps": [{"op": "set_mode", "mode": "LOITER"}]},
        {"name": "mode_not_ready", "ready": False, "steps": [{"op": "set_mode", "mode": "LOITER"}]},
        {"name": "mode_unknown", "steps": [{"op": "set_mode", "mode": "WARP"}]},
        {"name": "mode_unsupported_autopilot", "stack": "generic", "modes_unsupported": True,
         "steps": [{"op": "set_mode", "mode": "LOITER"}]},
        {"name": "mode_generic_unknown", "stack": "generic", "steps": [{"op": "set_mode", "mode": "LOITER"}]},
        {"name": "mode_refused", "results": {"176": 4}, "steps": [{"op": "set_mode", "mode": "RTL"}]},
        {"name": "mode_ardupilot", **AP, "steps": [{"op": "set_mode", "mode": "LOITER"}]},
        {"name": "arm_confirmed", "reactions": ARM, "steps": [{"op": "arm", "arm": True}, {"op": "arm", "arm": False}]},
        {"name": "arm_never_confirmed", "steps": [{"op": "arm", "arm": True}]},
        {"name": "arm_confirmed_late", "reactions": [
            {"cmd": 400, "param": [0, 1.0], "set": {"armed": True}, "after": 5.0}], "steps": [{"op": "arm", "arm": True}]},
        {"name": "arm_refused", "results": {"400": 2}, "steps": [{"op": "arm", "arm": True}]},
        {"name": "arm_no_ack", "results": {"400": -1}, "initial": {"armed": True}, "steps": [{"op": "arm", "arm": True}]},
        {"name": "arm_already_armed_failed", "results": {"400": 4}, "initial": {"armed": True},
         "steps": [{"op": "arm", "arm": True}, {"op": "arm", "arm": False}]},
        {"name": "arm_already_armed_stale", "results": {"400": 4}, "initial": {"armed": True}, "stale": True,
         "steps": [{"op": "arm", "arm": True}]},
        {"name": "arm_not_ready", "ready": False, "steps": [{"op": "arm", "arm": True}, {"op": "arm", "arm": False}]},
        {"name": "airborne_states", "steps": [{"op": "state_airborne"}]},
        {"name": "airborne_landed_in_air", "initial": {"landed_state": 2}, "steps": [{"op": "airborne"}]},
        {"name": "airborne_landed_on_ground", "initial": {"landed_state": 1, "altitude_agl": 40.0},
         "steps": [{"op": "airborne"}]},
        {"name": "airborne_by_height", "initial": {"altitude_agl": 3.0}, "steps": [{"op": "airborne"}]},
        {"name": "airborne_by_low_height", "initial": {"altitude_agl": 1.0}, "steps": [{"op": "airborne"}]},
        {"name": "amsl_and_reference", "home_amsl": 500.0, "steps": [
            {"op": "amsl", "agl": 12.5}, {"op": "await_reference", "agl": 1.0}]},
        {"name": "reference_from_position", "position_home_amsl": 400.0, "steps": [{"op": "amsl", "agl": 10.0}]},
        {"name": "reference_arrives", "home_reply": {"after": 0.4, "value": 480.0},
         "steps": [{"op": "await_reference", "agl": 10.0}]},
        {"name": "reference_never_arrives", "steps": [{"op": "await_reference", "agl": 10.0}]},
        {"name": "reference_stopped", "stop_at": 1000.0, "steps": [{"op": "await_reference", "agl": 10.0}]},
        {"name": "reference_link_lost", "ready_until": 1000.0, "steps": [{"op": "await_reference", "agl": 10.0}]},
        {"name": "wait_for_mode", "reactions": [], "initial": {"mode": "LOITER"}, "steps": [
            {"op": "wait_mode", "mode": "LOITER"}, {"op": "wait_mode", "mode": "AUTO"}]},
        {"name": "wait_for_mode_arrives", "steps": [{"op": "pend", "field": "mode", "value": "AUTO", "after": 0.6},
                                                    {"op": "wait_mode", "mode": "AUTO"}]},
        {"name": "takeoff_px4", "home_amsl": 500.0, "reactions": ARM, "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_px4_waits_for_reference", "home_reply": {"after": 0.4, "value": 520.0}, "reactions": ARM,
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_px4_without_reference", "reactions": ARM, "steps": [{"op": "takeoff", "agl": 25.0}]},
        {"name": "takeoff_px4_refused", "home_amsl": 500.0, "results": {"22": 2}, "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_px4_arm_refused", "home_amsl": 500.0, "results": {"400": 2},
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_px4_interrupted_after_command", "home_amsl": 500.0, "reactions": ARM,
         "abort_on_console": ["Takeoff target accepted"], "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_px4_interrupted_before_command", "home_amsl": 500.0, "reactions": ARM,
         "abort_on_console": ["Requesting"], "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_checks", "home_amsl": 500.0, "steps": [
            {"op": "takeoff", "agl": float("nan")}, {"op": "takeoff", "agl": float("inf")},
            {"op": "takeoff", "agl": 0.5}, {"op": "takeoff", "agl": 500.0}]},
        {"name": "takeoff_rover", "mav_type": 10, "stack": "ardupilot", "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_not_ready", "ready": False, "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot", **AP, "reactions": GUIDED + ARM, "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_already_guided", **AP, "initial": {"mode": "GUIDED"}, "reactions": ARM,
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_mode_late", **AP, "reactions": [
            {"cmd": 176, "param": [1, 4.0], "set": {"mode": "GUIDED"}, "after": 9.0}] + ARM,
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_mode_refused", **AP, "results": {"176": 2}, "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_arm_refused", **AP, "reactions": GUIDED, "results": {"400": 4},
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_refused_disarms", **AP, "reactions": GUIDED + ARM, "results": {"22": 3},
         "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "takeoff_ardupilot_interrupted_after_arm", **AP, "reactions": GUIDED + ARM,
         "abort_on_console": ["Armed successfully"], "steps": [{"op": "takeoff", "agl": 10.0}]},
        {"name": "land_rtl", "steps": [{"op": "land"}, {"op": "rtl"}]},
        {"name": "land_rtl_refused", "results": {"21": 4, "20": 2}, "steps": [{"op": "land"}, {"op": "rtl"}]},
        {"name": "land_rover", "mav_type": 10, "stack": "ardupilot", "steps": [{"op": "land"}, {"op": "rtl"}]},
        {"name": "lat_lon", "steps": [
            {"op": "lat_lon", "lat": 48.1, "lon": 11.5}, {"op": "lat_lon", "lat": 91.0, "lon": 11.5},
            {"op": "lat_lon", "lat": 48.1, "lon": -181.0}, {"op": "lat_lon", "lat": float("nan"), "lon": 0.0},
            {"op": "lat_lon", "lat": 0.0, "lon": float("inf")}]},
        {"name": "set_home", "home_amsl": 500.0, "steps": [{"op": "set_home", "lat": 48.1234567, "lon": 11.7654321}]},
        {"name": "set_home_from_reference_wait", "home_reply": {"after": 0.4, "value": 480.0},
         "steps": [{"op": "set_home", "lat": 48.0, "lon": 11.0}]},
        {"name": "set_home_no_reference", "steps": [{"op": "set_home", "lat": 48.0, "lon": 11.0}]},
        {"name": "set_home_refused", "home_amsl": 500.0, "results": {"179": 4},
         "steps": [{"op": "set_home", "lat": 48.0, "lon": 11.0}]},
        {"name": "set_home_bad_coordinates", "home_amsl": 500.0, "steps": [{"op": "set_home", "lat": 99.0, "lon": 11.0}]},
        {"name": "hold", "steps": [{"op": "hold"}]},
        {"name": "hold_rover", "mav_type": 10, "stack": "ardupilot", "steps": [{"op": "hold"}]},
        {"name": "hold_generic", "stack": "generic", "steps": [{"op": "hold"}]},
        {"name": "plugin_plain", "steps": [
            {"op": "plugin", "command": 178, "params": [1.0, 5.0]},
            {"op": "plugin", "command": 178}, {"op": "plugin", "command": 178, "params": [1, 2, 3, 4, 5, 6, 7, 8, 9]}]},
        {"name": "plugin_in_progress_and_failure", "results": {"178": [5, 2]}, "steps": [
            {"op": "plugin", "command": 178, "params": [1.0]}, {"op": "plugin", "command": 178, "params": [1.0]}]},
        {"name": "plugin_not_in_table", "steps": [
            {"op": "plugin", "command": 400}, {"op": "plugin", "command": 99999},
            {"op": "plugin", "command": 183, "params": [1.0]}]},
        {"name": "plugin_ardupilot_table", **AP, "steps": [
            {"op": "plugin", "command": 183, "params": [1.0, 1500.0]}, {"op": "plugin", "command": 5000}]},
        {"name": "plugin_position_checks", "home_amsl": 500.0, "steps": [
            {"op": "plugin", "command": 192}, {"op": "plugin", "command": 178, "position": [48.0, 11.0, 30.0, "amsl"]},
            {"op": "plugin", "command": 192, "position": [95.0, 11.0, 30.0, "amsl"]}]},
        {"name": "plugin_position_px4", "home_amsl": 500.0, "steps": [
            {"op": "plugin", "command": 192, "params": [-1.0, 1.0], "position": [48.1234567, 11.5, 30.0, "amsl"]},
            {"op": "plugin", "command": 192, "params": [-1.0, 1.0, 3.0, 4.0, 5.0], "position": [48.1, 11.5, 30.0, "relative"]}]},
        {"name": "plugin_position_px4_waits_for_home", "home_reply": {"after": 0.4, "value": 480.0}, "steps": [
            {"op": "plugin", "command": 192, "position": [48.1, 11.5, 30.0, "relative"]}]},
        {"name": "plugin_position_px4_without_home", "steps": [
            {"op": "plugin", "command": 192, "position": [48.1, 11.5, 30.0, "relative"]}]},
        {"name": "plugin_position_ardupilot", **AP, "steps": [
            {"op": "plugin", "command": 192, "params": [-1.0], "position": [48.1, 11.5, 30.0, "relative"]},
            {"op": "plugin", "command": 192, "position": [48.1, 11.5, 30.0, "amsl"]}]},
        {"name": "plugin_position_refused", "home_amsl": 500.0, "results": {"192": 3}, "steps": [
            {"op": "plugin", "command": 192, "position": [48.1, 11.5, 30.0, "amsl"]}]},
        {"name": "velocity_px4", "steps": [
            {"op": "velocity", "vx": 1.0, "vy": -2.0, "vz": 0.5}, {"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0, "yaw": 45.0},
            {"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0, "yaw": -10.0, "body": True}]},
        {"name": "velocity_ardupilot", **AP, "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "velocity_plane", "mav_type": 1, "stack": "px4", "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "velocity_generic", "stack": "generic", "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "velocity_checks", "steps": [
            {"op": "velocity", "vx": float("nan"), "vy": 0.0, "vz": 0.0}, {"op": "velocity", "vx": 0.0, "vy": float("inf"), "vz": 0.0},
            {"op": "velocity", "vx": 0.0, "vy": 0.0, "vz": 0.0, "yaw": float("nan")},
            {"op": "velocity", "vx": 31.0, "vy": 0.0, "vz": 0.0}, {"op": "velocity", "vx": 0.0, "vy": 0.0, "vz": -30.5},
            {"op": "velocity", "vx": 0.0, "vy": 0.0, "vz": 0.0, "yaw": 181.0}, {"op": "velocity", "vx": 30.0, "vy": -30.0, "vz": 0.0, "yaw": 180.0}]},
        {"name": "velocity_not_ready", "ready": False, "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "velocity_send_fails", "send_fails": True, "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "velocity_link_lost", "link_lost": True, "steps": [{"op": "velocity", "vx": 1.0, "vy": 0.0, "vz": 0.0}]},
        {"name": "fly_one_point_airborne_px4", "home_amsl": 500.0, "initial": air, "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_one_point_airborne_px4_no_reference_falls_back_to_mission", "initial": air, "reactions": ARM,
         "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_one_point_unsupported_falls_back_to_mission", "home_amsl": 500.0, "initial": air,
         "results": {"192": 3}, "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_one_point_refused", "home_amsl": 500.0, "initial": air, "results": {"192": 2},
         "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_one_point_ardupilot_airborne", **AP, "initial": {**air, "mode": "LOITER"}, "reactions": GUIDED,
         "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_one_point_ardupilot_guided_refused", **AP, "initial": air, "results": {"176": 2},
         "steps": [{"op": "fly", "points": pts}]},
        {"name": "fly_two_points_px4_on_ground", "home_amsl": 500.0, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_two_points_px4_airborne", "home_amsl": 500.0, "initial": air, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_ardupilot_on_ground", **AP, "home": [11.0, 48.0], "home_amsl": 520.0,
         "reactions": GUIDED + ARM, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_ardupilot_start_refused_disarms", **AP, "reactions": GUIDED + ARM, "results": {"300": 4},
         "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_upload_refused", "upload_ack": 1, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_upload_unsupported", "upload_ack": 3, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_upload_cancelled", "upload_ack": -3, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_start_refused", "results": {"300": 2}, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_invalid", "points_error": "points must be a non-empty list", "steps": [{"op": "fly", "points": []}]},
        {"name": "fly_points_not_ready", "ready": False, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_px4_mode_confirmed", "reactions": [
            {"cmd": 300, "set": {"mode": "MISSION", "armed": True}, "after": 0.4}], "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_px4_mode_mismatch", "initial": {"mode": "LOITER"}, "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_px4_no_mode", "steps": [{"op": "fly", "points": two}]},
        {"name": "fly_points_interrupted_while_uploading", "abort_on_console": ["Mission accepted"],
         "steps": [{"op": "fly", "points": two}]},
        {"name": "drive_points_rover", "mav_type": 10, "stack": "ardupilot", "initial": {"armed": True},
         "reactions": GUIDED, "steps": [{"op": "fly", "points": [[48.1, 11.5, 0.0]]}, {"op": "fly", "points": two}]},
        {"name": "drive_points_rover_disarmed", "mav_type": 10, "stack": "ardupilot", "reactions": GUIDED + ARM,
         "steps": [{"op": "fly", "points": [[48.1, 11.5, 0.0]]}]},
        {"name": "start_mission_px4", "reactions": [{"cmd": 300, "set": {"mode": "MISSION"}, "after": 0.2}],
         "steps": [{"op": "start", "count": 4}]},
        {"name": "start_mission_checks", "steps": [{"op": "start", "count": 0}, {"op": "start", "count": -3}]},
        {"name": "start_mission_not_ready", "ready": False, "steps": [{"op": "start", "count": 3}]},
        {"name": "start_mission_ardupilot_copter", **AP, "reactions": GUIDED + ARM, "steps": [{"op": "start", "count": 4}]},
        {"name": "start_mission_ardupilot_plane", "stack": "ardupilot", "mav_type": 1, "reactions": [
            {"cmd": 176, "param": [1, 10.0], "set": {"mode": "AUTO"}, "after": 0.3}] + ARM,
         "steps": [{"op": "start", "count": 4}]},
        {"name": "start_mission_ardupilot_already_armed", **AP, "initial": {"armed": True}, "steps": [{"op": "start", "count": 2}]},
        {"name": "start_mission_ardupilot_mode_refused", **AP, "results": {"176": 4}, "steps": [{"op": "start", "count": 2}]},
        {"name": "start_mission_refused", "results": {"300": 2}, "steps": [{"op": "start", "count": 2}]},
    ]


def flight_cases() -> list[dict]:
    import logging

    FlightBridge = flight_bridge_class()
    out = []
    for sc in flight_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = FlightBridge(sc, clock)
        records: list = []

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)

        def monotonic(clock=clock):
            return clock.t

        def sleep(seconds, clock=clock):
            clock.t += seconds

        results = []
        try:
            with mock.patch("time.monotonic", monotonic), mock.patch("time.sleep", sleep):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    if op == "set_mode":
                        result = br.set_mode(step["mode"])
                    elif op == "arm":
                        result = br.arm(step["arm"])
                    elif op == "airborne":
                        result = br._is_airborne()
                    elif op == "state_airborne":
                        result = []
                        for landed in (None, 0, 1, 2, 3, 4, 5):
                            for agl in (0.0, 1.5, 1.6):
                                br.state["landed_state"], br.state["altitude_agl"] = landed, agl
                                result.append(br._is_airborne())
                    elif op == "amsl":
                        result = br._takeoff_altitude_amsl(step["agl"])
                    elif op == "await_reference":
                        result = br._await_takeoff_reference(step["agl"])
                    elif op == "wait_mode":
                        result = br._wait_for_mode(step["mode"])
                    elif op == "pend":
                        br.pending.append((clock.t + step["after"], step["field"], step["value"]))
                        result = None
                    elif op == "takeoff":
                        result = br.takeoff(step["agl"])
                    elif op == "land":
                        result = br.land()
                    elif op == "rtl":
                        result = br.rtl()
                    elif op == "lat_lon":
                        result = br._validate_lat_lon(step["lat"], step["lon"], "Check")
                    elif op == "set_home":
                        result = br.set_home(step["lat"], step["lon"])
                    elif op == "hold":
                        result = br.hold()
                    elif op == "plugin":
                        position = tuple(step["position"]) if "position" in step else None
                        result = br.plugin_command(step["command"], step.get("params"), position)
                    elif op == "velocity":
                        result = br.velocity_setpoint(step["vx"], step["vy"], step["vz"], step.get("yaw"),
                                                      step.get("body", False))
                    elif op == "fly":
                        result = br.fly_to_points(step["points"])
                    elif op == "start":
                        result = br.start_mission(step["count"])
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:], "t": round(clock.t - 1000.0, 6)})
        finally:
            logger.removeHandler(handler)
            logger.setLevel(old_level)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {
            "last_error": br.last_error, "armed": br.state["armed"], "mode": br.state["mode"],
            "aborts": br._operation_lock.aborts}})})
    return out


# --------------------------------------------------------------- setup ----

class SetupStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def get_snapshot(self) -> dict:
        return {k: v for k, v in self.bridge.snap.items() if v is not None}

    def update(self, **fields) -> None:
        self.bridge.log.append(["update", fields])
        self.bridge.snap.update(fields)

    def merge_warning(self, text, level) -> None:
        self.bridge.log.append(["merge_warning", text, level])


def setup_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from tests.reference import mavlink_setup_python as ref

    class SetupBridge(ref.VehicleSetupMixin):
        def __init__(self, sc: dict) -> None:
            self.sc = sc
            self.log: list = []
            self.last_error = ""
            self._operation_lock = FlightLock()
            self._mav_type_id = sc.get("mav_type", 2)
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            self.snap = {"armed": False, "mode": "", "landed_state": None, "autotune_state": None,
                         **sc.get("snapshot", {})}
            self._store = SetupStore(self)
            self._mode_values = {n: (c.base_mode, c.custom_mode, c.custom_sub)
                                 for n, c in self._dialect.mode_table(self._mav_type_id, (1, 18, 0)).items()}
            if "mode_values" in sc:
                self._mode_values = {n: (1, 2, 3) for n in sc["mode_values"]}
            self.counts: dict = {}
            self._init_setup_state()

        def _connection_ready(self) -> bool:
            return self.sc.get("ready", True)

        def _set_command_error(self, text: str) -> None:
            self.last_error = text
            self.log.append(["error", text])

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])

        def _command_failure(self, action, result) -> bool:
            self.log.append(["failure", action, result])
            return False

        def _send_command_and_wait(self, command, params=None, timeout=3.0, retries=2,
                                   accept_in_progress=False, interim=frozenset()):
            self.log.append(["cmd", command, list(params), timeout, retries, accept_in_progress, sorted(interim)])
            results = self.sc.get("results", {})
            plan = results.get(str(command), self.sc.get("default_result", 0))
            if isinstance(plan, list):
                n = self.counts.get(command, 0)
                self.counts[command] = n + 1
                return plan[min(n, len(plan) - 1)]
            return plan

        def set_mode(self, mode) -> bool:
            self.log.append(["set_mode", mode])
            results = self.sc.get("set_mode_results", {})
            return results.get(mode, self.sc.get("set_mode_ok", True))

        def _reset_parameter_cache(self) -> None:
            self.log.append(["reset_cache"])

    return SetupBridge


def setup_scenarios() -> list[dict]:
    AP = {"stack": "ardupilot", "mav_type": 2}
    FLYING = {"armed": True, "landed_state": 2, "mode": "LOITER"}
    return [
        {"name": "available_calibrations", "steps": [{"op": "available"}]},
        {"name": "available_ardupilot", **AP, "steps": [{"op": "available"}]},
        {"name": "available_generic", "stack": "generic", "steps": [{"op": "available"}]},
        {"name": "calibrate_every_sensor_px4", "steps": [
            {"op": "calibrate", "sensor": s} for s in
            ["gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed", "motor", "bogus", ""]]},
        {"name": "calibrate_every_sensor_ardupilot", **AP, "steps": [
            {"op": "calibrate", "sensor": s} for s in
            ["gyro", "compass", "baro", "accel", "level", "accel_quick", "airspeed", "motor", "bogus"]]},
        {"name": "calibrate_every_sensor_generic", "stack": "generic", "steps": [
            {"op": "calibrate", "sensor": s} for s in ["gyro", "compass", "baro", "accel", "motor", "bogus"]]},
        {"name": "calibrate_armed", "snapshot": {"armed": True}, "steps": [{"op": "calibrate", "sensor": "gyro"}]},
        {"name": "calibrate_not_ready", "ready": False, "steps": [{"op": "calibrate", "sensor": "gyro"}]},
        {"name": "calibrate_refused", "results": {"241": 4}, "steps": [
            {"op": "calibrate", "sensor": "gyro"}, {"op": "state"}]},
        {"name": "cancel_px4", "steps": [{"op": "calibrate", "sensor": "gyro"}, {"op": "cancel"}, {"op": "state"}]},
        {"name": "cancel_ardupilot_compass", **AP, "steps": [
            {"op": "calibrate", "sensor": "compass"}, {"op": "cancel"}, {"op": "state"}]},
        {"name": "cancel_ardupilot_other", **AP, "steps": [
            {"op": "calibrate", "sensor": "gyro"}, {"op": "cancel"}]},
        {"name": "cancel_nothing_running", "steps": [{"op": "cancel"}]},
        {"name": "cancel_refused_keeps_calibration", "results": {"241": [0, 4]}, "steps": [
            {"op": "calibrate", "sensor": "gyro"}, {"op": "cancel"}, {"op": "state"}]},
        {"name": "cancel_not_ready", "ready": False, "steps": [{"op": "cancel"}]},
        {"name": "cancel_armed_is_fine", "snapshot": {"armed": True}, "steps": [{"op": "cancel"}]},
        {"name": "accel_position_px4", "steps": [{"op": "accel_position", "position": "level"}]},
        {"name": "accel_position_ardupilot", **AP, "steps": [
            {"op": "accel_position", "position": p} for p in ["level", "LEFT", "nosedown", "noseup", "back", "right", "bogus", ""]]},
        {"name": "accel_position_not_ready", **AP, "ready": False, "steps": [{"op": "accel_position", "position": "level"}]},
        {"name": "accel_position_refused", **AP, "results": {"42429": 4}, "steps": [{"op": "accel_position", "position": "level"}]},
        {"name": "motor_test_px4", "steps": [
            {"op": "motor_test", "motor": 1, "throttle": 50.0, "duration": 2.0},
            {"op": "motor_test", "motor": 12, "throttle": 12.5, "duration": 0.5},
            {"op": "motor_test", "motor": 1, "throttle": 100.0, "duration": 3.5}, {"op": "state"}]},
        {"name": "motor_test_ardupilot", **AP, "steps": [{"op": "motor_test", "motor": 3, "throttle": 20.0, "duration": 1.0}]},
        {"name": "motor_test_generic", "stack": "generic", "steps": [{"op": "motor_test", "motor": 3, "throttle": 20.0, "duration": 1.0}]},
        {"name": "motor_test_checks", "steps": [
            {"op": "motor_test", "motor": 0, "throttle": 10.0, "duration": 1.0},
            {"op": "motor_test", "motor": 17, "throttle": 10.0, "duration": 1.0},
            {"op": "motor_test", "motor": 1, "throttle": 101.0, "duration": 1.0},
            {"op": "motor_test", "motor": 1, "throttle": -1.0, "duration": 1.0},
            {"op": "motor_test", "motor": 1, "throttle": "nan", "duration": 1.0}]},
        {"name": "motor_test_duration_is_bounded", "steps": [
            {"op": "motor_test", "motor": 1, "throttle": 10.0, "duration": 99.0},
            {"op": "motor_test", "motor": 2, "throttle": 10.0, "duration": -4.0},
            {"op": "motor_test", "motor": 3, "throttle": 10.0, "duration": "nan"},
            {"op": "motor_test", "motor": 4, "throttle": 0.00001234, "duration": 0.0001},
            {"op": "motor_test", "motor": 5, "throttle": 100.0, "duration": "inf"}]},
        {"name": "motor_test_armed", "snapshot": {"armed": True}, "steps": [
            {"op": "motor_test", "motor": 1, "throttle": 10.0, "duration": 1.0}]},
        {"name": "motor_test_not_ready", "ready": False, "steps": [{"op": "motor_test", "motor": 1, "throttle": 10.0, "duration": 1.0}]},
        {"name": "motor_test_refused", "results": {"310": 2}, "steps": [
            {"op": "motor_test", "motor": 1, "throttle": 10.0, "duration": 1.0}, {"op": "state"}]},
        {"name": "motor_stop_order", "steps": [
            {"op": "motor_test", "motor": 3, "throttle": 10.0, "duration": 1.0},
            {"op": "motor_test", "motor": 12, "throttle": 10.0, "duration": 1.0}, {"op": "stop_motor"}]},
        {"name": "motor_stop_unconfirmed", "results": {"310": [0, 4, 0, 2]}, "steps": [{"op": "stop_motor"}]},
        {"name": "motor_stop_ardupilot", **AP, "steps": [{"op": "stop_motor"}]},
        {"name": "motor_stop_not_ready", "ready": False, "steps": [{"op": "stop_motor"}]},
        {"name": "motor_stop_armed_is_fine", "snapshot": {"armed": True}, "steps": [{"op": "stop_motor"}]},
        {"name": "autotune_px4_start_stop", "snapshot": FLYING, "results": {"212": [0, 5, 0]}, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}, {"op": "autotune", "axis": "all", "enable": True},
            {"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_px4_axis", "snapshot": FLYING, "steps": [{"op": "autotune", "axis": "roll", "enable": True}]},
        {"name": "autotune_px4_preconditions", "steps": [
            {"op": "autotune", "axis": "all", "enable": True},
            {"op": "set_snapshot", "snapshot": {"armed": True, "landed_state": 1}},
            {"op": "autotune", "axis": "all", "enable": True},
            {"op": "set_snapshot", "snapshot": {"armed": True, "landed_state": 0}},
            {"op": "autotune", "axis": "all", "enable": True},
            {"op": "set_snapshot", "snapshot": {"armed": True, "landed_state": None}},
            {"op": "autotune", "axis": "all", "enable": True}]},
        {"name": "autotune_px4_stop_needs_nothing", "steps": [{"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_px4_refused", "snapshot": FLYING, "results": {"212": 1}, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}, {"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_px4_not_ready", "snapshot": FLYING, "ready": False, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}]},
        {"name": "autotune_generic", "stack": "generic", "snapshot": FLYING, "steps": [{"op": "autotune", "axis": "all", "enable": True}]},
        {"name": "autotune_ardupilot_start_stop", **AP, "snapshot": FLYING, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}, {"op": "state"},
            {"op": "autotune", "axis": "all", "enable": False}, {"op": "state"}]},
        {"name": "autotune_ardupilot_already_in_mode", **AP, "snapshot": {**FLYING, "mode": "AUTOTUNE"}, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}, {"op": "state"}, {"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_ardupilot_no_airframe_mode", "stack": "ardupilot", "mav_type": 10, "snapshot": FLYING, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}]},
        {"name": "autotune_ardupilot_plane", "stack": "ardupilot", "mav_type": 1, "snapshot": {**FLYING, "mode": "FBWA"},
         "steps": [{"op": "autotune", "axis": "all", "enable": True}, {"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_ardupilot_preconditions", **AP, "steps": [
            {"op": "autotune", "axis": "all", "enable": True},
            {"op": "set_snapshot", "snapshot": {"armed": True, "landed_state": 1}},
            {"op": "autotune", "axis": "all", "enable": True}]},
        {"name": "autotune_ardupilot_mode_refused", **AP, "snapshot": FLYING, "set_mode_ok": False, "steps": [
            {"op": "autotune", "axis": "all", "enable": True}, {"op": "state"}]},
        {"name": "autotune_ardupilot_stop_without_return_mode", **AP, "snapshot": FLYING, "steps": [
            {"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_ardupilot_stop_nothing_to_return_to", **AP, "snapshot": FLYING, "mode_values": ["AUTOTUNE", "RTL"],
         "steps": [{"op": "autotune", "axis": "all", "enable": False}]},
        {"name": "autotune_ardupilot_stop_refused", **AP, "snapshot": FLYING, "set_mode_results": {"LOITER": False},
         "steps": [{"op": "autotune", "axis": "all", "enable": True}, {"op": "set_snapshot", "snapshot": {"mode": "AUTOTUNE"}},
                   {"op": "autotune", "axis": "all", "enable": False}, {"op": "state"}]},
        {"name": "autotune_ack_stream", "steps": [
            {"op": "ack", "result": 5, "progress": 10}, {"op": "ack", "result": 5, "progress": 250},
            {"op": "ack", "result": 5, "progress": -4}, {"op": "ack", "result": 5, "progress": 0},
            {"op": "ack", "result": 0, "progress": 0}, {"op": "ack", "result": 0, "progress": 0}]},
        {"name": "autotune_ack_accepted_without_a_run", "steps": [{"op": "ack", "result": 0, "progress": 0}]},
        {"name": "autotune_ack_refusals", "steps": [
            {"op": "ack", "result": 4, "progress": 0}, {"op": "ack", "result": 1, "progress": 0},
            {"op": "ack", "result": 77, "progress": 0}, {"op": "ack", "result": -1, "progress": 0}]},
        {"name": "reboot", "steps": [{"op": "reboot"}, {"op": "bootloader"}]},
        {"name": "reboot_armed", "snapshot": {"armed": True}, "steps": [{"op": "reboot"}, {"op": "bootloader"}]},
        {"name": "reboot_not_ready", "ready": False, "steps": [{"op": "reboot"}, {"op": "bootloader"}]},
        {"name": "reboot_refused", "results": {"246": 2}, "steps": [{"op": "reboot"}, {"op": "bootloader"}]},
    ]


def setup_cases() -> list[dict]:
    SetupBridge = setup_bridge_class()
    out = []
    for sc in setup_scenarios():
        br = SetupBridge(sc)
        results = []
        for step in sc["steps"]:
            before = len(br.log)
            op = step["op"]
            num = lambda v: float(v) if isinstance(v, str) else v
            if op == "available":
                result = br.available_calibrations()
            elif op == "calibrate":
                result = br.calibrate(step["sensor"])
            elif op == "cancel":
                result = br.cancel_calibration()
            elif op == "accel_position":
                result = br.accel_calibration_position(step["position"])
            elif op == "motor_test":
                result = br.motor_test(step["motor"], num(step["throttle"]), num(step["duration"]))
            elif op == "stop_motor":
                result = br.stop_motor_test()
            elif op == "autotune":
                result = br.autotune(step["axis"], step["enable"])
            elif op == "ack":
                msg = type("Ack", (), {})()
                msg.result, msg.progress = step["result"], step["progress"]
                br._handle_autotune_ack(msg)
                result = None
            elif op == "reboot":
                result = br.reboot_autopilot()
            elif op == "bootloader":
                result = br.reboot_to_bootloader()
            elif op == "set_snapshot":
                br.snap.update(step["snapshot"])
                result = None
            elif op == "state":
                result = {"active": br._active_calibration, "return_mode": br._autotune_return_mode,
                          "tested": list(getattr(br, "_motors_tested", None) or [])}
            else:
                raise SystemExit(op)
            results.append({"result": result, "events": br.log[before:]})
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {
            "last_error": br.last_error, "active": br._active_calibration, "return_mode": br._autotune_return_mode,
            "tested": list(getattr(br, "_motors_tested", None) or []), "snapshot": br.snap}})})
    return out


# ----------------------------------------------------------- telemetry ----

def telemetry_message(name: str, fields: dict, system: int = 1):
    """A message with every field of the MAVLink 2 view, as a decoded one has."""
    import corvus_link
    from types import SimpleNamespace

    cls = getattr(corvus_link.dialect_v2, f"MAVLink_{name.lower()}_message")
    ns = SimpleNamespace()
    lengths = dict(zip(cls.ordered_fieldnames, cls.array_lengths))
    for fname, ftype in zip(cls.fieldnames, cls.fieldtypes):
        zero = 0.0 if ftype in ("float", "double") else 0
        length = lengths[fname]
        setattr(ns, fname, [zero] * length if length else zero)
    for key, value in fields.items():
        setattr(ns, key, list(value) if isinstance(value, list) else value)
    ns.get_type = lambda name=name: name
    ns.get_srcSystem = lambda system=system: system
    return ns


def gnss_frame(system: int = 1, receiver: int = 0, jamming: int = 1, spoofing: int = 1, signal: int = 5,
               mode: str = "ok", length: int | None = None) -> str:
    from corvus_link import x25crc

    payload = struct.pack("<IHHBBBBBBBBB", 0, 0, 0, receiver, 0, jamming, spoofing, 0, 0, 0, signal, 0)
    payload = payload.rstrip(b"\0") or b"\0"
    if length is not None:
        payload = (payload + b"\0" * 40)[:length]
    magic = 0xFE if mode == "mav1" else 0xFD
    msgid = 440 if mode == "other_id" else 441
    header = bytes([magic, len(payload), 0, 0, 7, system, 1, msgid & 0xFF, (msgid >> 8) & 0xFF, 0])
    body = header + payload
    crc = x25crc(body[1:])
    crc.accumulate(bytes([169]))
    value = crc.crc ^ 1 if mode == "bad_crc" else crc.crc
    frame = body + value.to_bytes(2, "little")
    if mode == "short":
        frame = frame[:-3]
    return frame.hex()


class TelemetryStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def update(self, **fields) -> None:
        self.bridge.log.append(["update", fields])
        if "rc_channels" in fields:
            self.bridge.rc = list(fields["rc_channels"])

    def get_snapshot(self) -> dict:
        return {"rc_channels": list(self.bridge.rc)} if self.bridge.rc else {}


def telemetry_bridge_class():
    import collections

    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import mavlink_telemetry_python as ref

    class TelemetryBridge(ref.TelemetryMixin):
        def __init__(self, sc: dict) -> None:
            self.sc = sc
            self.log: list = []
            self.rc: list = list(sc.get("rc", []))
            self.last_error = ""
            self._operation_lock = FlightLock()
            self._store = TelemetryStore(self)
            self._target_system = sc.get("target_system", 1)
            self._init_telemetry_state()

        @property
        def _rangefinder_seen(self):
            return None

        @_rangefinder_seen.setter
        def _rangefinder_seen(self, value) -> None:
            self.log.append(["forget_rangefinder", value])

        def _connection_ready(self) -> bool:
            return self.sc.get("ready", True)

        def _set_command_error(self, text: str) -> None:
            self.last_error = text
            self.log.append(["error", text])

        def _is_slow_link(self) -> bool:
            return self.sc.get("slow_link", False)

        def _message_intervals(self) -> dict:
            return {int(k): v for k, v in self.sc.get("intervals", {}).items()}

        def set_message_interval(self, msg_id, interval_us) -> bool:
            self.log.append(["interval", msg_id, interval_us])
            return self.sc.get("interval_results", {}).get(str(msg_id), True)

    return TelemetryBridge


def telemetry_scenarios() -> list[dict]:
    RC = {f"chan{i}_raw": 1000 + i * 10 for i in range(1, 19)}
    bs = lambda **kw: {"op": "battery_status", "fields": kw}
    return [
        {"name": "battery_settings", "steps": [
            {"op": "battery"}, {"op": "set_battery", "raw": {"estimate": True, "cells": 6}}, {"op": "battery"},
            {"op": "set_battery", "raw": {"chemistry": "liion", "resistance": "9"}}, {"op": "set_battery", "raw": None},
            {"op": "set_battery", "raw": 7}, {"op": "state"}]},
        {"name": "battery_fields_autopilot", "steps": [
            {"op": "fields", "v": 25.2, "a": 3.0, "reported": 80}, {"op": "fields", "v": 24.0, "a": 30.0, "reported": -1},
            {"op": "fields", "v": 0.0, "a": 0.0, "reported": 100}, {"op": "fields", "v": 3.0, "a": 0.0, "reported": 0},
            {"op": "state"}]},
        {"name": "battery_cells_are_latched", "steps": [
            {"op": "fields", "v": 25.2, "a": 3.0, "reported": 80}, {"op": "fields", "v": 21.5, "a": 3.0, "reported": 80},
            {"op": "state"}, {"op": "forget_cells"}, {"op": "fields", "v": 21.5, "a": 3.0, "reported": 80}, {"op": "state"}]},
        {"name": "battery_cells_configured", "steps": [
            {"op": "set_battery", "raw": {"cells": 4}}, {"op": "fields", "v": 16.0, "a": 0.0, "reported": 50},
            {"op": "state"}]},
        {"name": "battery_estimate_shown", "steps": [
            {"op": "set_battery", "raw": {"estimate": True}}, {"op": "fields", "v": 25.2, "a": 0.0, "reported": 40},
            {"op": "fields", "v": 22.2, "a": 20.0, "reported": 40}, {"op": "fields", "v": 0.0, "a": 0.0, "reported": 40},
            {"op": "fields", "v": 22.2, "a": 20.0, "reported": -1}]},
        {"name": "battery_settings_reset_the_latch", "steps": [
            {"op": "fields", "v": 25.2, "a": 3.0, "reported": 80}, {"op": "set_battery", "raw": {"estimate": True}},
            {"op": "state"}]},
        {"name": "battery_endurance", "steps": [
            {"op": "endurance", "percent": 80.0, "source": "autopilot", "armed": False, "now": 0.0},
            *[{"op": "endurance", "percent": 80.0 - i * 0.5, "source": "autopilot", "armed": True, "now": 10.0 * i}
              for i in range(14)],
            {"op": "state"},
            {"op": "endurance", "percent": 60.0, "source": "estimate", "armed": True, "now": 200.0}, {"op": "state"},
            {"op": "endurance", "percent": None, "source": "estimate", "armed": True, "now": 210.0},
            {"op": "endurance", "percent": 59.0, "source": "estimate", "armed": False, "now": 220.0}, {"op": "state"}]},
        {"name": "battery_endurance_window", "steps": [
            *[{"op": "endurance", "percent": 100.0 - i * 0.1, "source": "autopilot", "armed": True, "now": 1.0 * i}
              for i in range(450)], {"op": "state_len"}]},
        {"name": "battery_status_cells", "steps": [
            bs(id=0, voltages=[4100, 4120, 4110, 4090, 65535, 65535, 65535, 65535, 65535, 65535], current_consumed=1200,
               temperature=2550, time_remaining=900),
            bs(id=0, voltages=[0] * 10, current_consumed=0, temperature=32767, time_remaining=-5),
            bs(id=0, voltages=[16800] + [65535] * 9, current_consumed=-1, temperature=0, time_remaining=0),
            bs(id=0, voltages=[65534] + [65535] * 9, voltages_ext=[5000, 65535, 65535, 65535], current_consumed=10),
            bs(id=0, voltages=[65534, 65535, 65535, 65535, 65535, 65535, 65535, 65535, 65535, 65535],
               voltages_ext=[5000, 6000, 65535, 65535]),
            bs(id=0, voltages=[3700] * 10, voltages_ext=[3700, 3700, 0, 65535], temperature=-1234),
            bs(id=1, voltages=[3700] * 10), bs(id=0, voltages=[3701, 3702, 3703, 0, 0, 0, 0, 0, 0, 0])]},
        {"name": "gnss_integrity_decoded", "target_system": 1, "steps": [
            {"op": "gnss", "fields": {"id": 0, "jamming_state": 1, "spoofing_state": 3, "gnss_signal_quality": 7}, "system": 1},
            {"op": "gnss", "fields": {"id": 0, "jamming_state": 2, "spoofing_state": 2, "gnss_signal_quality": 11}, "system": 1},
            {"op": "gnss", "fields": {"id": 0, "jamming_state": 9, "spoofing_state": 0, "gnss_signal_quality": 255}, "system": 0},
            {"op": "gnss", "fields": {"id": 0, "jamming_state": 3, "spoofing_state": 1, "gnss_signal_quality": 0}, "system": 5},
            {"op": "gnss", "fields": {"id": 1, "jamming_state": 3, "spoofing_state": 1, "gnss_signal_quality": 4}, "system": 1},
            {"op": "forget_gnss"}]},
        {"name": "gnss_integrity_unknown_target", "target_system": 0, "steps": [
            {"op": "gnss", "fields": {"id": 0, "jamming_state": 3, "spoofing_state": 3, "gnss_signal_quality": 10}, "system": 9}]},
        {"name": "gnss_integrity_frames", "target_system": 1, "steps": [
            {"op": "gnss_frame", "frame": gnss_frame()},
            {"op": "gnss_frame", "frame": gnss_frame(jamming=3, spoofing=2, signal=0)},
            {"op": "gnss_frame", "frame": gnss_frame(jamming=0, spoofing=0, signal=0)},
            {"op": "gnss_frame", "frame": gnss_frame(system=4)},
            {"op": "gnss_frame", "frame": gnss_frame(receiver=2)},
            {"op": "gnss_frame", "frame": gnss_frame(mode="bad_crc")},
            {"op": "gnss_frame", "frame": gnss_frame(mode="mav1")},
            {"op": "gnss_frame", "frame": gnss_frame(mode="short")},
            {"op": "gnss_frame", "frame": gnss_frame(mode="other_id")},
            {"op": "gnss_frame", "frame": gnss_frame(length=18)},
            {"op": "gnss_frame", "frame": ""}, {"op": "gnss_frame", "frame": "fd0100"}]},
        {"name": "attitude_target", "steps": [
            {"op": "att", "fields": {"q": [1.0, 0.0, 0.0, 0.0], "body_roll_rate": 0.1, "body_pitch_rate": -0.2, "body_yaw_rate": 0.0}},
            {"op": "att", "fields": {"q": [0.9238795, 0.0, 0.3826834, 0.0], "body_roll_rate": 1.5, "body_pitch_rate": 0.0, "body_yaw_rate": -3.0}},
            {"op": "att", "fields": {"q": [0.7071068, 0.7071068, 0.0, 0.0]}},
            {"op": "att", "fields": {"q": [0.5, 0.5, 0.5, 0.5]}},
            {"op": "att", "fields": {"q": [1.2, 0.0, 0.9, 0.0]}},
            {"op": "att", "fields": {"q": [0.0, 0.0, 0.0, 0.0], "body_yaw_rate": 0.05}},
            {"op": "att", "fields": {"q": [0.7, 0.0, 0.7, 0.1], "body_roll_rate": 0.123456}}]},
        {"name": "position_target", "steps": [
            {"op": "pos", "fields": {"vx": 1.234, "vy": -2.5, "vz": 0.005}},
            {"op": "pos", "fields": {"vx": 0.0, "vy": 0.0, "vz": 0.0}},
            {"op": "pos", "fields": {"vx": 10.125, "vy": 0.015, "vz": -0.025}}]},
        {"name": "rc_channels_full", "steps": [
            {"op": "rc", "fields": {**RC, "chancount": 8, "rssi": 90}},
            {"op": "rc", "fields": {**RC, "chancount": 18, "rssi": 254}},
            {"op": "rc", "fields": {**RC, "chancount": 0, "rssi": 255}},
            {"op": "rc", "fields": {**RC, "chancount": 40, "rssi": 101}},
            {"op": "rc", "fields": {**RC, "chancount": 3, "rssi": 100, "chan1_raw": 65535, "chan2_raw": 0, "chan3_raw": 65534}},
            *[{"op": "rc", "fields": {**RC, "chancount": 4, "rssi": r}} for r in (0, 50, 100, 101, 127, 128, 200, 253, 254, 255)]]},
        {"name": "rc_channels_raw_banks", "steps": [
            {"op": "rc_raw", "fields": {"port": 0, **{f"chan{i}_raw": 1100 + i for i in range(1, 9)}, "rssi": 80}},
            {"op": "rc_raw", "fields": {"port": 1, **{f"chan{i}_raw": 1200 + i for i in range(1, 9)}, "rssi": 80}},
            {"op": "rc_raw", "fields": {"port": 2, **{f"chan{i}_raw": 1300 + i for i in range(1, 9)}, "rssi": 255}},
            {"op": "rc_raw", "fields": {"port": 3, **{f"chan{i}_raw": 1400 + i for i in range(1, 9)}, "rssi": 80}},
            {"op": "rc_raw", "fields": {"port": 0, "chan1_raw": 65535, "chan2_raw": 2000}},
            {"op": "rc", "fields": {**RC, "chancount": 18, "rssi": 20}},
            {"op": "rc_raw", "fields": {"port": 1, **{f"chan{i}_raw": 1500 + i for i in range(1, 9)}}}]},
        {"name": "radio_status_and_link_quality", "steps": [
            {"op": "link_quality"},
            *[{"op": "hb", "t": 100.0 + i * 1.0} for i in range(5)], {"op": "jitter"}, {"op": "link_quality"},
            {"op": "radio", "fields": {"remrssi": 150, "txbuf": 100, "rxerrors": 3, "fixed": 1}},
            {"op": "radio", "fields": {"remrssi": 255, "txbuf": 100, "rxerrors": 4, "fixed": 2}},
            {"op": "radio", "fields": {"remrssi": 0, "txbuf": 0}},
            {"op": "radio", "fields": {"remrssi": 90, "txbuf": 40}},
            {"op": "radio", "fields": {"remrssi": 60, "txbuf": 10}},
            {"op": "radio", "fields": {"remrssi": 41, "txbuf": 0}},
            {"op": "radio", "fields": {"remrssi": 200, "txbuf": 200}},
            {"op": "state"}]},
        {"name": "link_quality_with_jitter", "steps": [
            {"op": "hb", "t": 0.0}, {"op": "hb", "t": 1.0}, {"op": "hb", "t": 3.5}, {"op": "hb", "t": 4.0}, {"op": "hb", "t": 7.0},
            {"op": "jitter"}, {"op": "current_jitter"},
            {"op": "radio", "fields": {"remrssi": 150, "txbuf": 100}}, {"op": "radio", "fields": {"remrssi": 120, "txbuf": 60}},
            {"op": "state"}]},
        {"name": "heartbeat_window", "steps": [
            *[{"op": "hb", "t": float(i) * 0.9973} for i in range(40)], {"op": "jitter"}, {"op": "state"},
            {"op": "forget_link"}, {"op": "state"}, {"op": "jitter"}]},
        {"name": "vibration_stream", "steps": [
            {"op": "vibration", "enabled": True, "rate": 10}, {"op": "vibration", "enabled": True, "rate": 1},
            {"op": "vibration", "enabled": True, "rate": 50}, {"op": "vibration", "enabled": True, "rate": 7},
            {"op": "vibration", "enabled": True, "rate": 0}, {"op": "vibration", "enabled": True, "rate": 51},
            {"op": "vibration", "enabled": False, "rate": 10}]},
        {"name": "vibration_stream_refused", "interval_results": {"241": False}, "steps": [
            {"op": "vibration", "enabled": True, "rate": 10}]},
        {"name": "rangefinder_stream", "intervals": {"132": 250000}, "steps": [
            {"op": "rangefinder", "enabled": True}, {"op": "rangefinder", "enabled": False}]},
        {"name": "rangefinder_stream_slow", "slow_link": True, "steps": [
            {"op": "rangefinder", "enabled": True}, {"op": "rangefinder", "enabled": False}]},
        {"name": "rangefinder_stream_not_ready", "ready": False, "steps": [
            {"op": "rangefinder", "enabled": True}, {"op": "rangefinder", "enabled": False}]},
        {"name": "rangefinder_stream_refused", "interval_results": {"132": False}, "steps": [
            {"op": "rangefinder", "enabled": False}]},
        {"name": "rc_stream", "intervals": {"65": 200000}, "steps": [
            {"op": "rc_stream", "enabled": True, "rate": 20}, {"op": "rc_stream", "enabled": True, "rate": 2},
            {"op": "rc_stream", "enabled": True, "rate": 0}, {"op": "rc_stream", "enabled": True, "rate": 99},
            {"op": "rc_stream", "enabled": False, "rate": 20}]},
        {"name": "rc_stream_without_baseline", "steps": [
            {"op": "rc_stream", "enabled": True, "rate": 20}, {"op": "rc_stream", "enabled": False, "rate": 20}]},
        {"name": "rc_stream_not_ready", "ready": False, "steps": [
            {"op": "rc_stream", "enabled": True, "rate": 20}, {"op": "rc_stream", "enabled": False, "rate": 20}]},
        {"name": "rc_stream_refused", "interval_results": {"65": False}, "steps": [
            {"op": "rc_stream", "enabled": True, "rate": 20}, {"op": "rc_stream", "enabled": False, "rate": 20}]},
        {"name": "tuning_stream", "intervals": {"30": 20000, "83": 0}, "steps": [
            {"op": "tuning", "enabled": True, "rate": 20}, {"op": "tuning", "enabled": True, "rate": 80},
            {"op": "tuning", "enabled": True, "rate": 0}, {"op": "tuning", "enabled": False, "rate": 20}]},
        {"name": "tuning_stream_partial", "interval_results": {"83": False}, "steps": [
            {"op": "tuning", "enabled": True, "rate": 10}, {"op": "tuning", "enabled": False, "rate": 10}]},
        {"name": "tuning_stream_not_ready", "ready": False, "steps": [
            {"op": "tuning", "enabled": True, "rate": 10}, {"op": "tuning", "enabled": False, "rate": 10}]},
        {"name": "stream_intervals", "intervals": {"30": 20000, "65": 200000}, "steps": [
            {"op": "interval", "id": 30, "requested": 10000}, {"op": "interval", "id": 30, "requested": 50000},
            {"op": "interval", "id": 30, "requested": None}, {"op": "interval", "id": 83, "requested": 40000},
            {"op": "interval", "id": 83, "requested": None}]},
    ]


def telemetry_cases() -> list[dict]:
    TelemetryBridge = telemetry_bridge_class()
    out = []
    for sc in telemetry_scenarios():
        br = TelemetryBridge(sc)
        results = []
        for step in sc["steps"]:
            before = len(br.log)
            op = step["op"]
            result = None
            if op == "battery":
                result = br.battery_settings()
            elif op == "set_battery":
                result = br.set_battery_settings(step["raw"])
            elif op == "forget_cells":
                br._forget_battery_cells()
            elif op == "fields":
                result = br._battery_fields(step["v"], step["a"], step["reported"])
            elif op == "endurance":
                result = br._battery_endurance_fields(step["percent"], step["source"], step["armed"], step["now"])
            elif op == "battery_status":
                br._handle_battery_status(telemetry_message("BATTERY_STATUS", step["fields"]))
            elif op == "gnss":
                br._handle_gnss_integrity(telemetry_message("GNSS_INTEGRITY", step["fields"], step["system"]))
            elif op == "gnss_frame":
                msg = type("Unknown", (), {})()
                msg.get_type = lambda: "UNKNOWN_441"
                msg.data = bytes.fromhex(step["frame"])
                br._handle_gnss_integrity(msg)
            elif op == "forget_gnss":
                br._forget_gnss_integrity()
            elif op == "att":
                br._handle_attitude_target(telemetry_message("ATTITUDE_TARGET", step["fields"]))
            elif op == "pos":
                br._handle_position_target(telemetry_message("POSITION_TARGET_LOCAL_NED", step["fields"]))
            elif op == "rc":
                br._handle_rc_channels(telemetry_message("RC_CHANNELS", step["fields"]))
            elif op == "rc_raw":
                br._handle_rc_channels(telemetry_message("RC_CHANNELS_RAW", step["fields"]), raw=True)
            elif op == "radio":
                br._handle_radio_status(telemetry_message("RADIO_STATUS", step["fields"]))
            elif op == "hb":
                br._hb_times.append(step["t"])
            elif op == "forget_link":
                br._hb_times.clear()
                br._radio_status_seen = False
                br._uplink_score = 0
            elif op == "jitter":
                br._publish_heartbeat_jitter()
            elif op == "current_jitter":
                result = br._current_jitter_ms()
            elif op == "link_quality":
                br._publish_link_quality()
            elif op == "vibration":
                result = br.set_vibration_stream(step["enabled"], step["rate"])
            elif op == "rangefinder":
                result = br.set_rangefinder_stream(step["enabled"])
            elif op == "rc_stream":
                result = br.set_rc_stream(step["enabled"], step["rate"])
            elif op == "tuning":
                result = br.set_tuning_stream(step["enabled"], step["rate"])
            elif op == "interval":
                result = br._stream_interval_us(step["id"], step["requested"])
            elif op == "state":
                result = {"hb": list(br._hb_times), "radio_seen": br._radio_status_seen, "uplink": br._uplink_score,
                          "cells": br._battery_cells, "drain": [list(x) for x in br._drain_samples],
                          "source": br._drain_source}
            elif op == "state_len":
                result = len(br._drain_samples)
            else:
                raise SystemExit(op)
            results.append({"result": result, "events": br.log[before:]})
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {
            "last_error": br.last_error, "rc": br.rc}})})
    return out


# ------------------------------------------------------------- missions ----

class FakeEvent:
    """threading.Event on the fake clock: waiting runs the scripted vehicle."""

    sim = None

    def __init__(self) -> None:
        self._flag = False

    def set(self) -> None:
        self._flag = True

    def clear(self) -> None:
        self._flag = False

    def is_set(self) -> bool:
        return self._flag

    def wait(self, timeout=None) -> bool:
        return FakeEvent.sim.wait(self, timeout)


class FakePendingAck:
    def __init__(self) -> None:
        self.event = FakeEvent()
        self.result = None
        self.accept_in_progress = False


class MissionLock(FlightLock):
    def __init__(self, bridge) -> None:
        super().__init__()
        self.bridge = bridge

    def abort_waiting(self) -> bool:
        at = self.bridge.sc.get("abort_at")
        return at is not None and self.bridge.clock.t >= at


class MissionMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def __getattr__(self, method):
        def call(*args):
            bridge = self.bridge
            if bridge.sc.get("send_fails") == method:
                raise OSError("boom")
            bridge.log.append(["send", method, list(args)])
            bridge.sim.on_send(method, list(args))
        return call


class MissionSendLock:
    def __init__(self, bridge) -> None:
        self.bridge = bridge
        self.swapped = False

    def __enter__(self):
        at = self.bridge.sc.get("link_lost_at")
        if at is not None and not self.swapped and self.bridge.clock.t >= at:
            self.swapped = True
            self.bridge._conn = type("Conn", (), {})()
            self.bridge._conn.mav = MissionMav(self.bridge)
        return self

    def __exit__(self, *exc):
        return False


class MissionSim:
    """The vehicle's side of the mission protocol, scripted by the scenario."""

    def __init__(self, bridge) -> None:
        self.bridge = bridge
        self.v = bridge.sc.get("vehicle", {})
        self.queue: list = []
        self.n = 0
        self.upload: dict = {}
        self.count_sends = 0
        self.list_requests = 0
        self.item_requests: dict = {}

    def schedule(self, delay: float, kind: str, payload) -> None:
        self.n += 1
        self.queue.append((self.bridge.clock.t + delay, self.n, kind, payload))

    def on_send(self, name: str, args: list) -> None:
        v = self.v
        if name == "mission_count_send":
            self.count_sends += 1
            if self.count_sends <= v.get("drop_counts", 0):
                return
            count = args[2]
            self.upload = {"count": count, "type": args[3] if len(args) > 3 else 0}
            if count == 0:
                self.schedule(v.get("ack_delay", 0.05), "ack", self.upload["type"])
            else:
                self.schedule(v.get("req_delay", 0.05), "request", 0)
        elif name in ("mission_item_int_send", "mission_item_send"):
            seq = args[2]
            nxt = seq + 1
            if nxt >= self.upload.get("count", 0):
                self.schedule(v.get("ack_delay", 0.05), "ack", self.upload.get("type", 0))
            elif v.get("stall_after") is not None and seq >= v["stall_after"]:
                pass
            else:
                self.schedule(v.get("item_delay", 0.05), "request", nxt)
        elif name == "mission_request_list_send":
            self.list_requests += 1
            if self.list_requests <= v.get("drop_lists", 0):
                return
            count = len(v.get("mission", [])) + (1 if v.get("count_changes") and self.list_requests >= 2 else 0)
            self.schedule(v.get("count_delay", 0.05), "count", v.get("count_override", count))
        elif name == "mission_request_int_send":
            seq = args[2]
            self.item_requests[seq] = self.item_requests.get(seq, 0) + 1
            if self.item_requests[seq] <= v.get("drop_items", {}).get(str(seq), 0):
                if v.get("unexpected"):
                    self.schedule(0.01, "console", {"name": "STATUSTEXT", "text": "Req. WP was unexpected", "level": "warning"})
                return
            self.schedule(v.get("item_delay", 0.05), "item", seq)

    def deliver(self, entry) -> None:
        _, _, kind, payload = entry
        b = self.bridge
        v = self.v
        as_int = v.get("request_as_int", True)
        if kind == "request":
            name = "MISSION_REQUEST_INT" if as_int else "MISSION_REQUEST"
            b._handle_mission_request(telemetry_message(name, {"seq": payload, "mission_type": self.upload.get("type", 0)}), as_int)
        elif kind == "ack":
            b._handle_mission_ack(telemetry_message("MISSION_ACK", {"type": v.get("ack_type", 0), "mission_type": payload}))
            current = v.get("current_after_ack")
            if current:
                self.schedule(current["delay"], "current", current["fields"])
            for line in v.get("status_after_ack", []):
                self.schedule(line["delay"], "console", {"name": "STATUSTEXT", "text": line["text"], "level": line["level"]})
        elif kind == "count":
            b._handle_mission_count(telemetry_message("MISSION_COUNT", {"count": payload, "mission_type": 0}))
        elif kind == "item":
            item = v["mission"][payload]
            fields = {"seq": payload, "frame": item["frame"], "command": item["command"], "current": 0,
                      "autocontinue": item.get("autocontinue", 1), "z": item.get("z", 0.0), "mission_type": 0}
            for n, value in enumerate(item.get("params", [0.0] * 4), start=1):
                fields[f"param{n}"] = unclean(value)
            if as_int:
                fields["x"], fields["y"] = item["x"], item["y"]
                b._handle_mission_item(telemetry_message("MISSION_ITEM_INT", fields), True)
            else:
                fields["x"], fields["y"] = item["x"] / 1e7, item["y"] / 1e7
                b._handle_mission_item(telemetry_message("MISSION_ITEM", fields), False)
        elif kind == "current":
            b._handle_mission_current(telemetry_message("MISSION_CURRENT", unclean(payload)))
        elif kind == "console":
            b.publish_to_subs(payload)

    def wait(self, event, timeout) -> bool:
        clock = self.bridge.clock
        deadline = clock.t + timeout
        while True:
            if event._flag:
                return True
            if not self.queue:
                clock.t = deadline
                return event._flag
            nxt = min(self.queue, key=lambda e: (e[0], e[1]))
            if nxt[0] > deadline:
                clock.t = deadline
                return event._flag
            self.queue.remove(nxt)
            if nxt[0] > clock.t:
                clock.t = nxt[0]
            self.deliver(nxt)


class MissionStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def update(self, **fields) -> None:
        self.bridge.log.append(["update", fields])

    def get_snapshot(self) -> dict:
        sc = self.bridge.sc
        snap = {}
        if "home" in sc:
            snap["home"] = sc["home"]
        if "position" in sc:
            snap["position"] = sc["position"]
        return snap


def mission_bridge_class():
    import types

    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from tests.reference import mavlink_missions_python as ref

    class MissionBridge(ref.MissionProtocolMixin):
        def __init__(self, sc: dict, clock: Clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self.last_error = ""
            self.subs: list = []
            self.sim = MissionSim(self)
            self._operation_lock = MissionLock(self)
            self._stop_event = FlightStop(self)
            self._store = MissionStore(self)
            self._send_lock = MissionSendLock(self)
            self._conn = type("Conn", (), {})()
            self._conn.mav = MissionMav(self)
            self._target_system = 1
            self._target_component = 1
            self._mav_type_id = sc.get("mav_type", 2)
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            self._home_alt_amsl = sc.get("home_amsl")
            self._init_mission_state()

        def _connection_ready(self) -> bool:
            until = self.sc.get("ready_until")
            if until is not None and self.clock.t >= until:
                return False
            return self.sc.get("ready", True)

        def _ack_is_for_us(self, msg) -> bool:
            return self.sc.get("ours", True)

        def _is_slow_link(self) -> bool:
            return self.sc.get("slow_link", False)

        def _set_command_error(self, text: str) -> None:
            self.last_error = text
            self.log.append(["error", text])

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])
            self.publish_to_subs({"name": name, "text": text, "level": level})

        def publish_to_subs(self, entry) -> None:
            for fn in list(self.subs):
                fn(entry)

        def add_console_sub(self, fn) -> None:
            self.subs.append(fn)

        def remove_console_sub(self, fn) -> None:
            if fn in self.subs:
                self.subs.remove(fn)

        def _command_failure(self, action, result) -> bool:
            self.log.append(["failure", action, result])
            return False

    return MissionBridge, ref


def mission_spec(raw: dict) -> dict:
    spec = unclean(raw)
    return spec


def mission_scenarios() -> list[dict]:
    def spec(seq, lat=48.1, lon=11.5, alt=30.0, command=16, p=(0.0, 0.0, 0.0, 0.0), frame=3):
        return {"frame": frame, "command": command, "current": 1 if seq == 0 else 0, "autocontinue": 1,
                "param1": p[0], "param2": p[1], "param3": p[2], "param4": p[3],
                "x_int": round(lat * 1e7), "y_int": round(lon * 1e7), "x_f": lat, "y_f": lon, "z": alt}

    three = [spec(0), spec(1, 48.2, 11.6, 40.0), spec(2, 48.3, 11.7, 50.0, p=(1.0, "nan", 0.0, "nan"))]
    plan = [{"command": 22, "lat": 48.1, "lon": 11.5, "alt": 20.0, "params": [0.0, 0.0, 0.0, "nan"], "item": 0},
            {"command": 16, "lat": 48.2, "lon": 11.6, "alt": 30.0, "params": [0.0, 0.0, 0.0, "nan"], "item": 1},
            {"command": 178, "lat": 0.0, "lon": 0.0, "alt": 0.0, "params": [1.0, 5.0, -1.0, 0.0], "item": 2},
            {"command": 20, "lat": 0.0, "lon": 0.0, "alt": 0.0, "params": [], "item": 3}]
    stored = [
        {"command": 16, "frame": 3, "x": 481000000, "y": 115000000, "z": 30.0, "params": [0.0, 0.0, 0.0, "nan"]},
        {"command": 22, "frame": 3, "x": 481100000, "y": 115100000, "z": 0.0, "params": [15.0, 0.0, 0.0, 90.0]},
        {"command": 21, "frame": 0, "x": 0, "y": 0, "z": 120.5, "params": [0.0, 0.0, 0.0, 0.0], "autocontinue": 0},
    ]
    polygon = [[11.0, 48.0], [11.1, 48.0], [11.1, 48.1], [11.0, 48.1]]
    AP = {"stack": "ardupilot", "mav_type": 2}
    cur = lambda **kw: {"op": "current", "fields": kw}
    return [
        {"name": "capabilities", "steps": [{"op": "capabilities"}]},
        {"name": "capabilities_rover", "stack": "ardupilot", "mav_type": 10, "steps": [{"op": "capabilities"}]},
        {"name": "upload_ok", "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_int_and_plain_requests", "vehicle": {"request_as_int": False}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_rejected", "vehicle": {"ack_type": 14}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_unsupported_frame", "vehicle": {"ack_type": 2}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_count_lost_then_retried", "vehicle": {"drop_counts": 2}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_count_never_answered", "vehicle": {"drop_counts": 9}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_count_never_answered_slow_link", "slow_link": True, "vehicle": {"drop_counts": 9},
         "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_stalls", "vehicle": {"stall_after": 0}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_slow_vehicle", "vehicle": {"req_delay": 1.0, "item_delay": 0.8, "ack_delay": 0.9},
         "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_send_fails", "send_fails": "mission_count_send", "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_link_lost", "link_lost_at": 0.0, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_item_send_fails", "send_fails": "mission_item_int_send", "vehicle": {"stall_after": 5},
         "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_aborted", "abort_at": 1000.3, "vehicle": {"req_delay": 1.0}, "steps": [{"op": "upload", "items": three}]},
        {"name": "upload_fence", "steps": [{"op": "upload", "items": three, "type": 1}]},
        {"name": "upload_clear", "steps": [{"op": "upload", "items": []}, {"op": "upload", "items": [], "type": 1}]},
        {"name": "upload_not_ours", "ours": False, "steps": [{"op": "upload", "items": three}]},
        {"name": "clear_mission", "steps": [{"op": "clear"}, {"op": "state"}]},
        {"name": "clear_mission_refused", "vehicle": {"ack_type": 5}, "steps": [{"op": "clear"}]},
        {"name": "clear_mission_timeout", "vehicle": {"drop_counts": 9}, "steps": [{"op": "clear"}]},
        {"name": "clear_mission_not_ready", "ready": False, "steps": [{"op": "clear"}]},
        {"name": "plan_upload_px4", "steps": [{"op": "upload_plan", "items": plan}, {"op": "state"}]},
        {"name": "plan_upload_ardupilot", **AP, "home": [11.5, 48.1], "home_amsl": 520.0,
         "steps": [{"op": "upload_plan", "items": plan[:3]}, {"op": "state"}]},
        {"name": "plan_lowers_circles_on_px4", "steps": [{"op": "upload_plan", "items": [
            {"command": 18, "lat": 48.0, "lon": 11.0, "alt": 40.0, "params": [2.0, 0.0, 50.0, 0.0], "item": 0},
            {"command": 16, "lat": 48.1, "lon": 11.1, "alt": 40.0, "params": [0.0, 0.0, 0.0, 0.0], "item": 1}]}]},
        {"name": "plan_keeps_circles_on_ardupilot", **AP, "steps": [{"op": "upload_plan", "items": [
            {"command": 18, "lat": 48.0, "lon": 11.0, "alt": 40.0, "params": [2.0, 0.0, 50.0, 0.0], "item": 0}]}]},
        {"name": "plan_without_item_numbers", "steps": [{"op": "upload_plan", "items": [
            {"command": 16, "lat": 48.0, "lon": 11.0, "alt": 40.0, "params": [0.0, 0.0, 0.0, 0.0]}]}, {"op": "state"}]},
        {"name": "plan_empty", "steps": [{"op": "upload_plan", "items": []}]},
        {"name": "plan_too_many", "steps": [{"op": "upload_plan", "items": [
            {"command": 16, "lat": 48.0, "lon": 11.0, "alt": 40.0, "params": [0.0] * 4, "item": i} for i in range(256)]}]},
        {"name": "plan_too_many_after_lowering", "steps": [{"op": "upload_plan", "items": [
            {"command": 18, "lat": 48.0, "lon": 11.0, "alt": 40.0, "params": [1.0] * 4, "item": i} for i in range(130)]}]},
        {"name": "plan_not_ready", "ready": False, "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_refused_for_a_rover", "stack": "ardupilot", "mav_type": 10, "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_refused_without_item_numbers", "stack": "ardupilot", "mav_type": 10, "steps": [{"op": "upload_plan", "items": [
            {"command": 16, "lat": 48.0, "lon": 11.0, "alt": 0.0, "params": [0.0] * 4},
            {"command": 21, "lat": 48.0, "lon": 11.0, "alt": 0.0, "params": [0.0] * 4}]}]},
        {"name": "plan_upload_rejected", "vehicle": {"ack_type": 3}, "steps": [{"op": "upload_plan", "items": plan}, {"op": "state"}]},
        {"name": "plan_verdict_no_mission_with_reason", "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}},
            "status_after_ack": [{"delay": 0.2, "text": "Mission rejected: Takeoff required", "level": "critical"},
                                 {"delay": 0.25, "text": "Other critical thing", "level": "critical"}]},
         "home": [11.5, 48.1], "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_no_mission_other_reason", "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}},
            "status_after_ack": [{"delay": 0.2, "text": "Landing too steep", "level": "critical"}]},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_no_home", "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_no_home_zero_home", "home": [0.0, 0.0], "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_with_home_gives_the_hint", "home": [11.5, 48.1], "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_accepted_states", "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 2}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_too_early", "vehicle": {
            "current_after_ack": {"delay": 0.1, "fields": {"seq": 0, "total": 4, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_for_another_mission", "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 9, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_never_comes", "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_interrupted", "abort_at": 1000.0, "steps": [{"op": "upload_plan", "items": plan}]},
        {"name": "plan_verdict_not_used_on_ardupilot", **AP, "vehicle": {
            "current_after_ack": {"delay": 0.5, "fields": {"seq": 0, "total": 4, "mission_state": 1}}},
         "steps": [{"op": "upload_plan", "items": plan[:2]}]},
        {"name": "fence_upload_and_clear", "steps": [{"op": "fence", "polygon": polygon}, {"op": "state"},
                                                    {"op": "clear_fence"}, {"op": "state"}]},
        {"name": "fence_checks", "steps": [{"op": "fence", "polygon": polygon[:2]}]},
        {"name": "fence_not_ready", "ready": False, "steps": [{"op": "fence", "polygon": polygon}, {"op": "clear_fence"}]},
        {"name": "fence_rejected", "vehicle": {"ack_type": 4}, "steps": [{"op": "fence", "polygon": polygon}, {"op": "clear_fence"},
                                                                         {"op": "state"}]},
        {"name": "download_ok", "vehicle": {"mission": stored}, "home": [11.5, 48.1], "home_amsl": 510.0,
         "steps": [{"op": "download"}, {"op": "state"}]},
        {"name": "download_plain_item_replies", "vehicle": {"mission": stored, "request_as_int": False}, "steps": [{"op": "download"}]},
        {"name": "download_ardupilot_skips_home_slot", **AP, "vehicle": {"mission": stored}, "steps": [{"op": "download"}]},
        {"name": "download_empty", "vehicle": {"mission": []}, "steps": [{"op": "download"}, {"op": "state"}]},
        {"name": "download_count_lost_then_answered", "vehicle": {"mission": stored, "drop_lists": 2}, "steps": [{"op": "download"}]},
        {"name": "download_count_never_answered", "vehicle": {"mission": stored, "drop_lists": 99}, "steps": [{"op": "download"}]},
        {"name": "download_item_lost_then_restarted", "vehicle": {"mission": stored, "drop_items": {"1": 1}}, "steps": [{"op": "download"}]},
        {"name": "download_item_never_arrives", "vehicle": {"mission": stored, "drop_items": {"1": 99}}, "steps": [{"op": "download"}]},
        {"name": "download_item_unexpected_request", "vehicle": {"mission": stored, "drop_items": {"2": 99}, "unexpected": True},
         "steps": [{"op": "download"}]},
        {"name": "download_count_changes_on_restart", "vehicle": {"mission": stored, "drop_items": {"1": 1}, "count_changes": True},
         "steps": [{"op": "download"}]},
        {"name": "download_too_long", "vehicle": {"mission": stored, "count_override": 400}, "steps": [{"op": "download"}]},
        {"name": "download_interrupted_by_abort", "abort_at": 1000.2, "vehicle": {"mission": stored, "count_delay": 1.0},
         "steps": [{"op": "download"}]},
        {"name": "download_not_ready", "ready": False, "steps": [{"op": "download"}]},
        {"name": "download_link_lost", "link_lost_at": 0.0, "vehicle": {"mission": stored}, "steps": [{"op": "download"}]},
        {"name": "download_send_fails", "send_fails": "mission_request_list_send", "vehicle": {"mission": stored},
         "steps": [{"op": "download"}]},
        {"name": "download_stopped", "stop_at": 1000.0, "vehicle": {"mission": stored}, "steps": [{"op": "download"}]},
        {"name": "download_slow_link", "slow_link": True, "vehicle": {"mission": stored, "count_delay": 2.0}, "steps": [{"op": "download"}]},
        {"name": "download_items_with_nan_and_junk", "vehicle": {"mission": [
            {"command": 16, "frame": 3, "x": 1, "y": -1, "z": "nan", "params": ["nan", 1.0, "nan", 0.0]}]}, "steps": [{"op": "download"}]},
        {"name": "mission_current_follows_the_plan", "steps": [
            {"op": "note", "index": [0, 1, None, 2]},
            cur(seq=1, total=4, mission_state=3), cur(seq=2, total=4), cur(seq=9, total=4), cur(seq=0, total=65535, mission_state=5),
            cur(seq=3, total=0, mission_state=9), {"op": "reached", "fields": {"seq": 3}}, {"op": "reached", "fields": {"seq": 1}},
            {"op": "state"}]},
        {"name": "mission_current_ardupilot_offset", **AP, "steps": [
            {"op": "note", "index": [0, 1]}, cur(seq=0, total=3, mission_state=3), cur(seq=1, total=3), cur(seq=2, total=3),
            {"op": "reached", "fields": {"seq": 0}}, {"op": "reached", "fields": {"seq": 2}}]},
        {"name": "mission_current_without_a_plan", "steps": [cur(seq=2, total=4, mission_state=3), {"op": "state"}]},
        {"name": "mission_changed_by_count_after_grace", "steps": [
            {"op": "note", "index": [0, 1, 2]}, cur(seq=0, total=7), {"op": "advance", "seconds": 6.0}, cur(seq=0, total=7), {"op": "state"}]},
        {"name": "mission_count_confirmed_then_changed", "steps": [
            {"op": "note", "index": [0, 1, 2]}, cur(seq=0, total=3), cur(seq=0, total=3), cur(seq=0, total=5), {"op": "state"}]},
        {"name": "mission_changed_by_id", "steps": [
            {"op": "note", "index": [0, 1, 2]}, cur(seq=0, total=3, mission_id=77), cur(seq=1, total=3, mission_id=77),
            cur(seq=1, total=3, mission_id=78), cur(seq=1, total=3, mission_id=78), {"op": "state"}]},
        {"name": "mission_id_adopted_after_a_transfer", "steps": [
            cur(seq=0, total=3, mission_id=5), {"op": "note", "index": [0, 1, 2]}, cur(seq=0, total=3, mission_id=9),
            cur(seq=0, total=3, mission_id=9), {"op": "state"}]},
        {"name": "forget_and_note", "steps": [
            {"op": "note", "index": [0, 1]}, {"op": "forget", "reason": "the link restarted"}, {"op": "forget", "reason": "again"},
            {"op": "forget", "reason": ""}, {"op": "note", "index": None}, {"op": "state"}]},
        {"name": "request_served_directly", "steps": [
            {"op": "serve", "items": three, "seq": 1, "as_int": True}, {"op": "serve", "items": three, "seq": 2, "as_int": False},
            {"op": "serve", "items": three, "seq": 3, "as_int": True}, {"op": "serve", "items": three, "seq": -1, "as_int": True},
            {"op": "serve", "items": three, "seq": 1, "as_int": True, "mission_type": 1}]},
        {"name": "request_for_a_fence_upload", "steps": [
            {"op": "serve", "items": three, "seq": 1, "as_int": True, "type": 1, "mission_type": 1}]},
        {"name": "request_not_ours", "ours": False, "steps": [{"op": "serve", "items": three, "seq": 1, "as_int": True}]},
        {"name": "request_send_fails", "send_fails": "mission_item_int_send", "steps": [
            {"op": "serve", "items": three, "seq": 1, "as_int": True}]},
        {"name": "request_link_lost", "link_lost_at": 0.0, "steps": [{"op": "serve", "items": three, "seq": 1, "as_int": True}]},
        {"name": "stray_messages", "steps": [
            {"op": "stray_count", "count": 3}, {"op": "stray_item", "seq": 0}, {"op": "stray_ack", "type": 0}, {"op": "stray_ack", "type": 14}]},
        {"name": "helper_specs", "steps": [{"op": "spec", "args": [0, 22, 48.1234567, 11.7654321, 30.0, "nan", "nan", "nan", "nan"]},
                                          {"op": "spec", "args": [3, 16, -0.00000005, 0.00000015, -5.5, 1.0, 2.0, 3.0, 4.0], "frame": 0}]},
        {"name": "helper_home_slot", "steps": [{"op": "home_slot", "items": [spec(0), spec(1)]}, {"op": "home_slot", "items": []}]},
        {"name": "helper_home_slot_ardupilot", **AP, "home": [11.5, 48.1], "home_amsl": 500.0,
         "steps": [{"op": "home_slot", "items": [spec(0), spec(1)]}, {"op": "home_slot", "items": []}]},
        {"name": "helper_home_slot_ardupilot_no_home", **AP, "steps": [{"op": "home_slot", "items": [spec(0)]}]},
        {"name": "helper_takeoff_position", "steps": [
            {"op": "takeoff_pos", "first": [48.0, 11.0, 30.0]}]},
        {"name": "helper_takeoff_position_from_the_vehicle", "position": [11.9, 48.9], "home": [11.5, 48.1],
         "steps": [{"op": "takeoff_pos", "first": [48.0, 11.0, 30.0]}]},
        {"name": "helper_takeoff_position_from_home", "position": [0.0, 0.0], "home": [11.5, 48.1],
         "steps": [{"op": "takeoff_pos", "first": [48.0, 11.0, 30.0]}]},
        {"name": "helper_takeoff_position_junk", "position": [200.0, 48.9], "home": [11.5, 95.0],
         "steps": [{"op": "takeoff_pos", "first": [48.0, 11.0, 30.0]}]},
        {"name": "helper_acks", "steps": [{"op": "ack_result", "ack": n} for n in range(-3, 17)]},
        {"name": "helper_validate_points", "steps": [
            {"op": "validate", "points": [{"lat": 48.1, "lon": 11.5, "alt_agl": 30}]},
            {"op": "validate", "points": []}, {"op": "validate", "points": "x"}, {"op": "validate", "points": None},
            {"op": "validate", "points": [1]}, {"op": "validate", "points": [{"lon": 11.5, "alt_agl": 30}]},
            {"op": "validate", "points": [{"lat": True, "lon": 11.5, "alt_agl": 30}]},
            {"op": "validate", "points": [{"lat": 91, "lon": 11.5, "alt_agl": 30}]},
            {"op": "validate", "points": [{"lat": 48, "lon": "x", "alt_agl": 30}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 181, "alt_agl": 30}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": "30"}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 0.5}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 120}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 121}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 30}] * 255},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 30}] * 256},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 30}, {"lat": 49, "lon": 12, "alt_agl": 99.5}]}]},
        {"name": "helper_validate_points_rover", "stack": "ardupilot", "mav_type": 10, "steps": [
            {"op": "validate", "points": [{"lat": 48, "lon": 11}]}, {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": 500}]},
            {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": None}]}, {"op": "validate", "points": [{"lat": 48, "lon": 11, "alt_agl": "x"}]}]},
    ]


def mission_cases() -> list[dict]:
    import logging
    import types

    MissionBridge, ref = mission_bridge_class()
    out = []
    for sc in mission_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = MissionBridge(sc, clock)
        FakeEvent.sim = br.sim
        records: list = []

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)

        def monotonic(clock=clock):
            return clock.t

        def lower_circles(items):
            out_items = []
            for item in items:
                if int(item.get("command", 0)) == 18:
                    for _ in range(2):
                        out_items.append(dict(item, command=16))
                else:
                    out_items.append(item)
            return out_items

        def items_to_plan(items, home=None, home_alt_amsl=None):
            return {"plan_index": list(range(len(items))), "home": home, "home_alt": home_alt_amsl}

        saved = (ref.mission_plan.lower_circles, ref.mission_plan.items_to_plan, ref._PendingAck, ref.threading)
        ref.mission_plan.lower_circles = lower_circles
        ref.mission_plan.items_to_plan = items_to_plan
        ref._PendingAck = FakePendingAck
        ref.threading = types.SimpleNamespace(Event=FakeEvent, Lock=threading.Lock)
        results = []
        try:
            with mock.patch("time.monotonic", monotonic):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    result = None
                    if op == "capabilities":
                        result = br.mission_capabilities()
                    elif op == "upload":
                        result = br._upload_mission([mission_spec(i) for i in step["items"]], step.get("type", 0))
                    elif op == "clear":
                        result = br.clear_mission()
                    elif op == "upload_plan":
                        result = br.upload_mission_plan([unclean(i) for i in step["items"]])
                    elif op == "fence":
                        result = br.upload_fence([list(p) for p in step["polygon"]])
                    elif op == "clear_fence":
                        result = br.clear_fence()
                    elif op == "download":
                        result = br.download_mission()
                    elif op == "current":
                        br._handle_mission_current(telemetry_message("MISSION_CURRENT", step["fields"]))
                    elif op == "reached":
                        br._handle_mission_item_reached(telemetry_message("MISSION_ITEM_REACHED", step["fields"]))
                    elif op == "note":
                        br._note_vehicle_mission(step["index"])
                    elif op == "forget":
                        br._forget_vehicle_mission(step["reason"])
                    elif op == "advance":
                        clock.t += step["seconds"]
                    elif op == "serve":
                        br._mission_items = [mission_spec(i) for i in step["items"]]
                        br._mission_upload_type = step.get("type", 0)
                        name = "MISSION_REQUEST_INT" if step["as_int"] else "MISSION_REQUEST"
                        br._handle_mission_request(telemetry_message(
                            name, {"seq": step["seq"], "mission_type": step.get("mission_type", 0)}), step["as_int"])
                        result = br._mission_last_request
                    elif op == "stray_count":
                        br._handle_mission_count(telemetry_message("MISSION_COUNT", {"count": step["count"]}))
                    elif op == "stray_item":
                        br._handle_mission_item(telemetry_message("MISSION_ITEM_INT", {"seq": step["seq"]}), True)
                    elif op == "stray_ack":
                        br._handle_mission_ack(telemetry_message("MISSION_ACK", {"type": step["type"]}))
                    elif op == "spec":
                        args = [unclean(a) for a in step["args"]]
                        result = br._build_mission_item_spec(*args, frame=step.get("frame"))
                    elif op == "home_slot":
                        result = br._with_mission_home_slot([mission_spec(i) for i in step["items"]])
                    elif op == "takeoff_pos":
                        result = br._takeoff_item_position(tuple(step["first"]))
                    elif op == "ack_result":
                        result = br._mission_ack_to_result(step["ack"])
                    elif op == "validate":
                        result = br._validate_fly_to_points(step["points"])
                    elif op == "state":
                        result = {"revision": br._mission_revision, "index": br._vehicle_mission_index,
                                  "id": br._vehicle_mission_id, "confirmed": br._vehicle_mission_confirmed,
                                  "fence": br.vehicle_fence()}
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:], "t": round(clock.t - 1000.0, 6)})
        finally:
            ref.mission_plan.lower_circles, ref.mission_plan.items_to_plan, ref._PendingAck, ref.threading = saved
            logger.removeHandler(handler)
            logger.setLevel(old_level)
            FakeEvent.sim = None
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {
            "last_error": br.last_error, "subs": len(br.subs)}})})
    return out


# ------------------------------------------------------------ remote id ----

class RidMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge
        if bridge.sc.get("v2", True):
            for name in ("open_drone_id_basic_id", "open_drone_id_operator_id", "open_drone_id_self_id",
                         "open_drone_id_system"):
                setattr(self, name + "_send", self._sender(name))

    def _sender(self, name):
        def send(**kwargs):
            fails = self.bridge.sc.get("send_fails")
            if fails and fails["on"] == name:
                raise OSError(fails["message"])
            row = {k: (v.hex() if isinstance(v, bytes) else v) for k, v in kwargs.items()}
            self.bridge.log.append(["send", name, row])
        return send


def rid_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import mavlink_remote_id_python as ref

    class RidBridge(ref.RemoteIdMixin):
        def __init__(self, sc: dict, clock: Clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self._target_system = sc.get("target", [1, 1])[0]
            self._target_component = sc.get("target", [1, 1])[1]
            self._conn = type("Conn", (), {})()
            self._conn.mav = RidMav(self)
            self._init_remote_id_state()

    return RidBridge, ref


RID_UNIX_BASE = 1_700_000_000.0


def rid_raws() -> list:
    sysd = lambda **kw: {"system": kw}
    return [
        None, {}, [], "x", 5, {"enabled": 1}, {"enabled": "no"}, {"enabled": []}, {"enabled": 0.0}, {"enabled": True},
        {"region": "FAA"}, {"region": " faa "}, {"region": ""}, {"region": 5}, {"region": "mars"}, {"region": None},
        {"basic_id": 5}, {"basic_id": {"id_type": "2", "ua_type": 15.9, "uas_id": "  ABC123  "}},
        {"basic_id": {"id_type": True}}, {"basic_id": {"id_type": 9}}, {"basic_id": {"id_type": "1_0"}},
        {"basic_id": {"id_type": " 3 ", "ua_type": "4.0"}}, {"basic_id": {"id_type": None, "ua_type": [1], "uas_id": 12345}},
        {"basic_id": {"uas_id": "ünï\x00cödé OK ✓"}}, {"basic_id": {"uas_id": "A" * 40}}, {"basic_id": {"uas_id": ["a", 1]}},
        {"basic_id": {"uas_id": {"k": "v"}}}, {"basic_id": {"uas_id": True}}, {"basic_id": {"uas_id": 3.0}},
        {"basic_id": {"uas_id": "tab\tinside\nnewline"}}, {"basic_id": {"uas_id": "\x1c\x1dpad\x1f"}},
        {"basic_id": {"uas_id": "it's"}}, {"basic_id": {"uas_id": ["it's", 'say "x"']}},
        {"basic_id": {"uas_id": 1e16}}, {"basic_id": {"uas_id": 0.00001}}, {"basic_id": {"uas_id": None}},
        {"operator_id": {"operator_id_type": 1, "operator_id": "FIN87astrdge12k8-xyz"}},
        {"operator_id": {"operator_id": "x" * 25}}, {"operator_id": []},
        {"self_id": {"description_type": 2, "description": "survey flight over the northern field today"}},
        {"self_id": {"description_type": 3, "description": 7}},
        sysd(operator_location_type=2, operator_latitude=48.1234567, operator_longitude=11.7654321, operator_altitude_geo=520.5),
        sysd(operator_location_type=2, operator_latitude=91, operator_longitude=-181, operator_altitude_geo=40000),
        sysd(operator_latitude="48.5", operator_longitude="x", operator_altitude_geo="nan"),
        sysd(operator_altitude_geo=-2000), sysd(operator_latitude=True), sysd(operator_latitude=None),
        sysd(operator_latitude=-95, operator_longitude=190), sysd(operator_latitude=-89.5, operator_longitude=-179.5),
        sysd(operator_latitude=-90, operator_longitude=180, operator_altitude_geo=31767), sysd(operator_altitude_geo=31768),
        sysd(area_ceiling=-1001, area_floor=31768), sysd(area_ceiling=-1000, area_floor=31767),
        sysd(classification_type=1, category_eu=3, class_eu=7), sysd(classification_type=1, category_eu=9, class_eu=8),
        sysd(classification_type=2), sysd(area_count=0, area_radius=-5), sysd(area_count=70000, area_radius=70000),
        sysd(area_count="5", area_radius="12.7"), sysd(area_count=True, area_radius=False), sysd(area_count=2.9),
        sysd(area_count="nan"), sysd(area_ceiling=120, area_floor=-5), sysd(area_ceiling="inf", area_floor="1e3"),
        {"enabled": True, "region": "faa", "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "1596F37QZ0123456789"},
         "operator_id": {"operator_id": "FIN123"}, "self_id": {"description": "test"},
         "system": {"operator_location_type": 2, "operator_latitude": 48.1, "operator_longitude": 11.5,
                    "operator_altitude_geo": 500, "classification_type": 1, "category_eu": 2, "class_eu": 3,
                    "area_count": 3, "area_radius": 200, "area_ceiling": 120, "area_floor": 0}},
    ]


def rid_scenarios() -> list[dict]:
    full = {"enabled": True, "region": "eu", "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "ABC123"},
            "operator_id": {"operator_id": "FIN123"}, "self_id": {"description_type": 0, "description": "test"},
            "system": {"operator_location_type": 2, "operator_latitude": 48.1234567, "operator_longitude": 11.7654321,
                       "operator_altitude_geo": 520.5, "classification_type": 1, "category_eu": 2, "class_eu": 5}}
    arm = lambda status, error: {"op": "arm", "status": status, "error": error}
    return [
        {"name": "defaults", "steps": [{"op": "settings"}, {"op": "status"}]},
        {"name": "disabled_sends_nothing", "steps": [{"op": "send"}, {"op": "status"}]},
        {"name": "enabled_broadcast", "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, {"op": "status"}, {"op": "advance", "seconds": 2.5}, {"op": "status"},
            {"op": "advance", "seconds": 2.5}, {"op": "status"}, {"op": "send"}, {"op": "status"}]},
        {"name": "takeoff_location_sends_no_position", "steps": [
            {"op": "set", "raw": {**full, "system": {**full["system"], "operator_location_type": 0}}}, {"op": "send"}]},
        {"name": "undeclared_classification_sends_no_class", "steps": [
            {"op": "set", "raw": {**full, "system": {**full["system"], "classification_type": 0}}}, {"op": "send"}]},
        {"name": "timestamp_follows_the_clock", "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, {"op": "advance", "seconds": 1000.25}, {"op": "send"}]},
        {"name": "mavlink_1_link", "v2": False, "steps": [
            {"op": "set", "raw": full}, {"op": "supported"}, {"op": "send"}, {"op": "status"}]},
        {"name": "mavlink_1_link_disabled", "v2": False, "steps": [{"op": "send"}, {"op": "supported"}, {"op": "status"}]},
        {"name": "send_fails_midway", "send_fails": {"on": "open_drone_id_self_id", "message": "boom"}, "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, {"op": "status"}]},
        {"name": "send_recovers", "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, {"op": "status"}]},
        {"name": "arm_status_stream", "steps": [
            {"op": "set", "raw": full}, {"op": "status"},
            arm(0, ""), {"op": "status"}, arm(1, "UAS ID missing"), {"op": "status"}, arm(7, "nul\x00ignored"), {"op": "status"},
            arm(0, "  padded  "), arm(1, "x" * 80), {"op": "status"}]},
        {"name": "arm_status_text", "steps": [arm(1, "café ✓ ok"), {"op": "status"}]},
        {"name": "setting_a_new_identity_forgets_the_old_one", "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, arm(1, "bad"), {"op": "set", "raw": full}, {"op": "status"}]},
        {"name": "forget_the_session", "steps": [
            {"op": "set", "raw": full}, {"op": "send"}, arm(0, ""), {"op": "forget"}, {"op": "status"}, {"op": "settings"}]},
        {"name": "settings_are_resolved", "steps": [
            {"op": "set", "raw": {"enabled": 1, "region": "FAA", "basic_id": {"uas_id": " abc "}}}, {"op": "settings"}]},
        {"name": "targets", "target": [7, 190], "steps": [{"op": "set", "raw": full}, {"op": "send"}]},
        {"name": "pure_settings", "steps": [{"op": "pure_settings", "raw": raw} for raw in rid_raws()]},
        {"name": "pure_messages", "steps": [
            *[{"op": "pure_messages", "raw": raw, "target": [1, 1], "unix": 1_700_000_000.0} for raw in rid_raws()[-8:]],
            *[{"op": "pure_messages", "raw": full, "target": [2, 190], "unix": u}
              for u in (1_546_300_800.0, 1_546_300_799.5, 0.0, 6e9, 1_700_000_000.75)]]},
        {"name": "pure_arm_status", "steps": [
            {"op": "pure_arm", "status": s, "error": e} for s in (0, 1, 2, 255, None, "x") for e in ("", "boom", "aéb")]},
    ]


def rid_cases() -> list[dict]:
    import datetime

    RidBridge, ref = rid_bridge_class()
    model = ref.remote_id_model
    original = model.system_timestamp
    out = []
    for sc in rid_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = RidBridge(sc, clock)

        def monotonic(clock=clock):
            return clock.t

        def stamp(now=None, clock=clock):
            moment = datetime.datetime.fromtimestamp(RID_UNIX_BASE + (clock.t - 1000.0), datetime.UTC)
            return original(moment)

        model.system_timestamp = stamp
        results = []
        try:
            with mock.patch("time.monotonic", monotonic):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    result = None
                    if op == "settings":
                        result = br.remote_id_settings()
                    elif op == "set":
                        result = br.set_remote_id(step["raw"])
                    elif op == "send":
                        br._send_remote_id(br._conn)
                    elif op == "status":
                        result = br.remote_id_status()
                    elif op == "supported":
                        result = br.remote_id_supported()
                    elif op == "advance":
                        clock.t += step["seconds"]
                    elif op == "forget":
                        br._forget_remote_id_session()
                    elif op == "arm":
                        msg = type("Arm", (), {})()
                        msg.status, msg.error = step["status"], step["error"].encode()
                        br._handle_remote_id_arm_status(msg)
                    elif op == "pure_settings":
                        result = model.settings(step["raw"])
                    elif op == "pure_messages":
                        moment = datetime.datetime.fromtimestamp(step["unix"], datetime.UTC)
                        result = [[name, {k: (v.hex() if isinstance(v, bytes) else v) for k, v in kw.items()}]
                                  for name, kw in _rid_messages(model, original, step, moment)]
                    elif op == "pure_arm":
                        err = step["error"].encode("utf-8")
                        result = model.arm_status(step["status"], err)
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:], "t": round(clock.t - 1000.0, 6)})
        finally:
            model.system_timestamp = original
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


def _rid_messages(model, original, step, moment):
    saved = model.system_timestamp
    model.system_timestamp = original
    try:
        return model.messages(step["raw"], target_system=step["target"][0], target_component=step["target"][1], now=moment)
    finally:
        model.system_timestamp = saved


# -------------------------------------------------------------- receive ----

class AckSim:
    """The vehicle answering commands, scripted; waiting on an event runs it."""

    def __init__(self, bridge) -> None:
        self.bridge = bridge
        self.queue: list = []
        self.n = 0
        self.sends: dict = {}

    def on_command(self, command: int) -> None:
        sc = self.bridge.sc
        attempt = self.sends.get(command, 0)
        self.sends[command] = attempt + 1
        script = sc.get("acks", {}).get(str(command), [])
        if attempt < len(script):
            for ack in script[attempt]:
                self.n += 1
                self.queue.append((self.bridge.clock.t + ack["after"], self.n, ack))

    def deliver(self, entry) -> None:
        ack = entry[2]
        fields = {"command": ack.get("command", 0), "result": ack["result"], "progress": ack.get("progress", 0),
                  "target_system": ack.get("target_system", 0), "target_component": ack.get("target_component", 0)}
        msg = receive_message("COMMAND_ACK", fields, ack.get("system", 1), ack.get("component", 1))
        self.bridge._dispatch(msg)

    def wait(self, event, timeout) -> bool:
        clock = self.bridge.clock
        deadline = clock.t + timeout
        while True:
            if event._flag:
                return True
            if not self.queue:
                clock.t = deadline
                return event._flag
            nxt = min(self.queue, key=lambda e: (e[0], e[1]))
            if nxt[0] > deadline:
                clock.t = deadline
                return event._flag
            self.queue.remove(nxt)
            if nxt[0] > clock.t:
                clock.t = nxt[0]
            self.deliver(nxt)


class FakeAckPending:
    def __init__(self, accept_in_progress: bool = False, interim=frozenset()) -> None:
        self.event = FakeEvent()
        self.result = None
        self.accept_in_progress = accept_in_progress
        self.interim = interim
        self.interim_result = None
        self.in_progress = False


def receive_message(name: str, fields: dict, system: int = 1, component: int = 1, raw: str | None = None):
    msg = telemetry_message(name, fields, system)
    msg.get_srcComponent = lambda component=component: component
    if raw is not None:
        data = bytes.fromhex(raw)
        msg.get_msgbuf = lambda data=data: data
    return msg


def receive_frame(spec: dict):
    """A scenario frame as the Python dispatcher is handed it."""
    if spec.get("unknown") is not None:
        msg = type("Unknown", (), {})()
        msg.get_type = lambda: f"UNKNOWN_{spec['unknown']}"
        msg.data = bytes.fromhex(spec.get("data", ""))
        msg.get_srcSystem = lambda: spec.get("system", 0)
        msg.get_srcComponent = lambda: spec.get("component", 0)
        return msg
    return receive_message(spec["name"], unclean(spec.get("fields", {})), spec.get("system", 1), spec.get("component", 1),
                           spec.get("raw"))


class ReceiveStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def heartbeat(self) -> float:
        self.bridge.log.append(["store_heartbeat"])
        return self.bridge.clock.t

    def update(self, **fields) -> None:
        self.bridge.log.append(["update", fields])
        self.bridge.snap.update(fields)

    def get_snapshot(self) -> dict:
        return dict(self.bridge.snap)

    def resolve_warnings(self, kinds) -> None:
        self.bridge.log.append(["resolve_warnings", sorted(kinds)])

    def is_stale(self, timeout=2.0) -> bool:
        return self.bridge.sc.get("stale", False)


def receive_bridge_class():
    import collections

    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from tests.reference import mavlink_bridge_receive_python as rx
    from tests.reference import mavlink_telemetry_python as tm

    def recorder(name, *argnames):
        def call(self, *args, **kwargs):
            shown = []
            for a in list(args) + list(kwargs.values()):
                if hasattr(a, "get_type"):
                    shown.append(a.get_type())
                else:
                    shown.append(a)
            self.log.append(["call", name, shown])
        return call

    class ReceiveBridge(rx.ReceivePathMixin, tm.TelemetryMixin):
        _handle_mission_request = recorder("handle_mission_request")
        _handle_mission_ack = recorder("handle_mission_ack")
        _handle_mission_count = recorder("handle_mission_count")
        _handle_mission_item = recorder("handle_mission_item")
        _handle_mission_current = recorder("handle_mission_current")
        _handle_mission_item_reached = recorder("handle_mission_item_reached")
        _handle_serial_control = recorder("handle_serial_control")
        _handle_remote_id_arm_status = recorder("handle_remote_id_arm_status")
        _handle_statustext = recorder("handle_statustext")
        _handle_param_value = recorder("handle_param_value")
        _handle_ftp_message = recorder("handle_ftp_message")
        _build_mode_mapping = recorder("build_mode_mapping")
        _reset_parameter_cache = recorder("reset_parameter_cache")
        _reset_param_metadata = recorder("reset_param_metadata")
        _request_streams = recorder("request_streams")
        _request_version = recorder("request_version")
        _request_home = recorder("request_home")
        _schedule_message_intervals = recorder("schedule_message_intervals")
        _clear_prearm_reasons = recorder("clear_prearm_reasons")
        _roll_tlog_on_disarm = recorder("roll_tlog_on_disarm")
        _note_prearm_state = recorder("note_prearm_state")

        def __init__(self, sc: dict, clock: Clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self.snap = dict(sc.get("snapshot", {}))
            self._store = ReceiveStore(self)
            self._operation_lock = FlightLock()
            self._send_lock = FlightSendLock(self)
            self._conn = type("Conn", (), {})()
            self._conn.source_system = sc.get("source_system", 255)
            self._conn.source_component = sc.get("source_component", 190)
            self._conn.mav = ReceiveMav(self)
            self._target_system = sc.get("target", [1, 1])[0]
            self._target_component = sc.get("target", [1, 1])[1]
            self._mav_type_id = sc.get("mav_type", 2)
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            self._pending_acks = {}
            self._ack_lock = threading.Lock()
            self._recent_frames = {}
            self._recent_frames_swept = 0.0
            self._duplicate_frames = 0
            self._ignored_systems = set()
            self._degraded_warned = sc.get("degraded", False)
            self._rangefinder_seen = sc.get("rangefinder_seen")
            self._last_boot_ms = sc.get("last_boot_ms")
            self._kill_evidence_at = None
            self._firmware_version = None
            self._board_vendor_id = 0
            self._board_product_id = 0
            self._param_encoding_declared = "unset"
            self._request_sent = True
            self._prearm_reasons = ["reason"] if sc.get("prearm_reasons") else []
            self._log_sink = (lambda msg: self.log.append(["call", "log_sink", [msg.get_type()]])) if sc.get("log_sink") else None
            if sc.get("log_sink_raises"):
                def boom(msg):
                    raise RuntimeError("sink down")
                self._log_sink = boom
            self.message_scope = type("Scope", (), {"record": lambda s, m: self.log.append(["call", "scope", [m.get_type()]])})()
            self.fleet_tracker = type("Fleet", (), {"observe": lambda s, m, vehicle_heartbeat=False: self.log.append(
                ["call", "fleet", [m.get_type(), vehicle_heartbeat]])})()
            self._mission_engine = type("Engine", (), {"cancel_upload": lambda s: self.log.append(["call", "cancel_upload", []])})()
            self.sim = AckSim(self)
            self._init_telemetry_state()

        def _write_tlog(self, msg) -> None:
            self.log.append(["call", "tlog", [msg.get_type()]])

        def _note_heartbeat(self, stamp) -> None:
            self._hb_times.append(stamp)

        def _latch_dialect(self, autopilot_id, mav_type) -> bool:
            dialect = ap.dialect_for(autopilot_id)
            changed = dialect is not self._dialect or int(mav_type) != self._mav_type_id
            if dialect is not self._dialect:
                self._firmware_version = None
            self._dialect = dialect
            self._mav_type_id = int(mav_type)
            return changed

        def _decode_mode(self, hb) -> str:
            return self._dialect.decode_mode(getattr(hb, "custom_mode", 0), getattr(hb, "base_mode", 0),
                                             getattr(hb, "type", self._mav_type_id))

        def _is_serial(self) -> bool:
            return self.sc.get("serial", False)

        def _warn_timeout(self) -> float:
            return 5.0

        def _heartbeat_timeout(self) -> float:
            return 2.0

        def _set_command_error(self, text) -> None:
            self.log.append(["error", text])

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])

        def _handle_autotune_ack(self, msg) -> None:
            self.log.append(["call", "autotune_ack", [msg.result, msg.progress]])

        def _link_gen(self):
            return id(self._conn)

    class ReceiveMav:
        def __init__(self, bridge) -> None:
            self.bridge = bridge

        def command_long_send(self, *args):
            b = self.bridge
            if b.sc.get("send_fails"):
                raise OSError("boom")
            b.log.append(["send", "command_long_send", list(args)])
            b.sim.on_command(args[2])

        def command_int_send(self, *args):
            b = self.bridge
            if b.sc.get("send_fails"):
                raise OSError("boom")
            b.log.append(["send", "command_int_send", list(args)])
            b.sim.on_command(args[3])

    return ReceiveBridge, rx


def receive_scenarios() -> list[dict]:
    hb = lambda **kw: {"name": "HEARTBEAT", "fields": {"type": 2, "autopilot": 12, "base_mode": 81, "custom_mode": 196608,
                                                     "system_status": 4, "mavlink_version": 3, **kw}}
    frame = lambda name, **fields: {"name": name, "fields": fields}
    alive = {"connected": True}
    ack = lambda after, result, **kw: {"after": after, "result": result, **kw}
    return [
        {"name": "plausible_heartbeats", "steps": [{"op": "plausible", "frame": hb(**kw)} for kw in (
            {}, {"mavlink_version": 2}, {"type": 128}, {"type": 127}, {"autopilot": 64}, {"autopilot": 63},
            {"system_status": 32}, {"system_status": 31}, {"type": 255})]},
        {"name": "vehicle_heartbeats", "steps": [{"op": "vehicle_heartbeat", "frame": {**hb(**kw), "component": c}} for kw, c in (
            ({}, 1), ({}, 0), ({}, 240), ({}, 154), ({}, 68), ({}, 191), ({"type": 18}, 1), ({"type": 6}, 1), ({"type": 30}, 1),
            ({"type": 41}, 1), ({"autopilot": 8}, 1), ({"autopilot": 0}, 1), ({"mavlink_version": 2}, 1))]},
        {"name": "sensor_health", "steps": [{"op": "sys_helpers", "fields": f} for f in (
            {}, {"onboard_control_sensors_present": 32}, {"onboard_control_sensors_present": 32, "onboard_control_sensors_health": 32},
            {"onboard_control_sensors_enabled": 32, "onboard_control_sensors_health": 1},
            {"onboard_control_sensors_present": 268435456}, {"onboard_control_sensors_present": 268435456, "onboard_control_sensors_health": 268435456},
            {"onboard_control_sensors_enabled": 268435456 | 32, "onboard_control_sensors_health": 32})]},
        {"name": "rangefinder", "steps": [{"op": "range", "fields": f} for f in (
            {"orientation": 25, "current_distance": 150, "min_distance": 10, "max_distance": 1000},
            {"orientation": 25, "current_distance": 5, "min_distance": 10, "max_distance": 1000},
            {"orientation": 25, "current_distance": 1000, "min_distance": 10, "max_distance": 1000},
            {"orientation": 25, "current_distance": 5000, "min_distance": 10, "max_distance": 0},
            {"orientation": 25, "current_distance": 123, "min_distance": 0, "max_distance": 0},
            {"orientation": 0, "current_distance": 150, "min_distance": 10, "max_distance": 1000},
            {"orientation": 25, "current_distance": 1, "min_distance": 0, "max_distance": 10})]},
        {"name": "gps_helpers", "steps": [{"op": "gps_helpers", "fields": f} for f in (
            {"fix_type": 3, "alt": 520123}, {"fix_type": 4, "alt": -5555}, {"fix_type": 2, "alt": 520123}, {"fix_type": 0},
            {"fix_type": 3, "alt": 0}, {"fix_type": 3, "alt": 520125}, {"fix_type": 3, "alt": 520135},
            {"fix_type": 3, "h_acc": 1234, "v_acc": 0}, {"fix_type": 3, "h_acc": 1235, "v_acc": 5})]},
        {"name": "git_hash", "steps": [{"op": "git_hash", "raw": raw} for raw in (
            [0] * 8, [0, 0, 0, 0xAB, 0xCD, 0xEF, 0x12, 0x34], [ord(c) for c in "4a7b3c9d"], [ord(c) for c in "4A7B3C"],
            [ord(c) for c in "4a7b3"] + [0, 0, 0], [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0], [1, 2, 3, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0, 7], [ord(c) for c in "zzzzzzzz"], [ord(c) for c in "abcdef"] + [0, 0], [])]},
        {"name": "who_sent_it", "target": [1, 1], "steps": [{"op": "who", "frame": {"name": "HEARTBEAT", "system": s, "component": c}}
                                                          for s, c in ((1, 1), (1, 0), (0, 0), (0, 5), (2, 1), (1, 2), (254, 190))]},
        {"name": "who_is_the_ack_for", "steps": [{"op": "ack_for", "frame": {"name": "COMMAND_ACK", "system": s, "component": c,
                                                                              "fields": {"target_system": ts, "target_component": tc}}}
                                                for s, c, ts, tc in ((1, 1, 0, 0), (1, 1, 255, 190), (1, 1, 255, 7), (1, 1, 9, 190), (2, 1, 0, 0),
                                                                    (1, 3, 0, 0), (0, 0, 255, 0), (0, 0, 0, 190))]},
        {"name": "no_connection_no_ack", "no_connection": True, "steps": [
            {"op": "ack_for", "frame": {"name": "COMMAND_ACK", "system": 1, "component": 1}}]},
        {"name": "heartbeat_updates_the_store", "snapshot": alive, "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "dispatch", "frame": hb(base_mode=209, custom_mode=0, system_status=3)},
            {"op": "dispatch", "frame": hb(type=1, autopilot=3, custom_mode=5, base_mode=1)},
            {"op": "dispatch", "frame": hb(type=99, autopilot=99)}, {"op": "state"}]},
        {"name": "heartbeat_stack_switch", "snapshot": alive, "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "dispatch", "frame": hb(autopilot=3, type=2)},
            {"op": "dispatch", "frame": hb(autopilot=3, type=1)}, {"op": "dispatch", "frame": hb(autopilot=3, type=1)}]},
        {"name": "heartbeat_from_a_component_is_ignored", "steps": [
            {"op": "dispatch", "frame": {**hb(), "component": 100}}, {"op": "dispatch", "frame": {**hb(), "system": 5}}]},
        {"name": "ignored_systems_are_told_once", "steps": [
            {"op": "dispatch", "frame": {**hb(), "system": 5}}, {"op": "dispatch", "frame": {**hb(), "system": 5}},
            {"op": "dispatch", "frame": {**hb(), "system": 254}}, {"op": "dispatch", "frame": {**hb(mavlink_version=2), "system": 6}},
            {"op": "dispatch", "frame": {**hb(autopilot=8), "system": 7}}, {"op": "dispatch", "frame": {**hb(), "system": 0}},
            {"op": "dispatch", "frame": {"name": "ATTITUDE", "system": 8, "fields": {"roll": 0.1}}}, {"op": "state"}]},
        {"name": "ignored_systems_are_capped", "steps": [
            *[{"op": "dispatch", "frame": {**hb(), "system": 10 + i}} for i in range(20)], {"op": "state"}]},
        {"name": "heartbeat_degraded_and_recovered", "degraded": True, "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "dispatch", "frame": hb()}, {"op": "state"},
            {"op": "mark_degraded"}, {"op": "mark_degraded"}, {"op": "state"}]},
        {"name": "heartbeat_flight_termination", "snapshot": {"kill_switch": True, "flight_termination": False}, "steps": [
            {"op": "dispatch", "frame": hb(system_status=8)}, {"op": "state"}, {"op": "dispatch", "frame": hb(system_status=4)}, {"op": "state"}]},
        {"name": "heartbeat_termination_without_the_switch", "steps": [{"op": "dispatch", "frame": hb(system_status=8)},
                                                                    {"op": "dispatch", "frame": hb(system_status=4)}]},
        {"name": "armed_clears_the_reasons", "prearm_reasons": True, "steps": [
            {"op": "dispatch", "frame": hb(base_mode=209)}, {"op": "dispatch", "frame": hb(base_mode=81)}]},
        {"name": "heartbeat_expires_the_rangefinder", "rangefinder_seen": 990.0, "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "state"}]},
        {"name": "heartbeat_keeps_a_fresh_rangefinder", "rangefinder_seen": 999.0, "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "state"}]},
        {"name": "position", "steps": [
            {"op": "dispatch", "frame": frame("GLOBAL_POSITION_INT", lat=481234567, lon=117654321, alt=520500, relative_alt=30250,
                                              hdg=18050, vx=150, vy=-250, vz=5)},
            {"op": "dispatch", "frame": frame("GLOBAL_POSITION_INT", lat=-1, lon=-2, alt=-100, relative_alt=-100, hdg=65535, vx=1)},
            {"op": "state"}]},
        {"name": "distance_sensor", "steps": [
            {"op": "dispatch", "frame": frame("DISTANCE_SENSOR", orientation=25, current_distance=250, min_distance=10, max_distance=1000)},
            {"op": "dispatch", "frame": frame("DISTANCE_SENSOR", orientation=25, current_distance=5000, min_distance=10, max_distance=1000)},
            {"op": "dispatch", "frame": frame("DISTANCE_SENSOR", orientation=0, current_distance=250, min_distance=10, max_distance=1000)},
            {"op": "state"}]},
        {"name": "attitude", "steps": [{"op": "dispatch", "frame": frame("ATTITUDE", roll=0.1, pitch=-0.2, yaw=3.0, rollspeed=0.01,
                                                                           pitchspeed=-0.02, yawspeed=0.5)},
                                       {"op": "dispatch", "frame": frame("ATTITUDE", roll=3.14159, pitch=1.5707, yaw=-3.14159)}]},
        {"name": "vfr_hud", "steps": [
            {"op": "dispatch", "frame": frame("VFR_HUD", groundspeed=12.345, airspeed=11.25, heading=181, climb=-0.45)},
            {"op": "dispatch", "frame": frame("VFR_HUD", groundspeed=0.0, airspeed="nan", heading=0, climb=0.0)}]},
        {"name": "gps_raw", "steps": [
            {"op": "dispatch", "frame": frame("GPS_RAW_INT", fix_type=3, eph=121, epv=250, satellites_visible=14, alt=520500, h_acc=1500, v_acc=2500)},
            {"op": "dispatch", "frame": frame("GPS_RAW_INT", fix_type=6, eph=65535, epv=65535, satellites_visible=20, alt=1000)},
            {"op": "dispatch", "frame": frame("GPS_RAW_INT", fix_type=1, eph=0, epv=0, satellites_visible=0)},
            {"op": "dispatch", "frame": frame("GPS_RAW_INT", fix_type=11, eph=50, epv=60, satellites_visible=3)}]},
        {"name": "sys_status", "snapshot": {"armed": False}, "steps": [
            {"op": "dispatch", "frame": frame("SYS_STATUS", voltage_battery=25200, current_battery=1250, battery_remaining=87,
                                              onboard_control_sensors_present=268435456 | 32, onboard_control_sensors_enabled=268435456 | 32,
                                              onboard_control_sensors_health=268435456)},
            {"op": "dispatch", "frame": frame("SYS_STATUS", voltage_battery=65535, current_battery=-1, battery_remaining=-1)},
            {"op": "dispatch", "frame": frame("SYS_STATUS", voltage_battery=22200, current_battery=100, battery_remaining=-1)},
            {"op": "state"}]},
        {"name": "sys_status_with_the_estimate", "snapshot": {"armed": True}, "steps": [
            {"op": "set_battery", "raw": {"estimate": True}},
            {"op": "dispatch", "frame": frame("SYS_STATUS", voltage_battery=25200, current_battery=0, battery_remaining=40)},
            {"op": "dispatch", "frame": frame("SYS_STATUS", voltage_battery=24000, current_battery=0, battery_remaining=-1)}]},
        {"name": "battery_status_goes_to_telemetry", "steps": [
            {"op": "dispatch", "frame": frame("BATTERY_STATUS", id=0, voltages=[4100] * 6 + [65535] * 4, current_consumed=500, temperature=2100,
                                              time_remaining=600)},
            {"op": "dispatch", "frame": {**frame("BATTERY_STATUS", id=1, voltages=[4100] * 6 + [65535] * 4), "system": 5}}]},
        {"name": "system_time", "steps": [
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=1_700_000_000_123_456, time_boot_ms=10000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=11000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=2**62, time_boot_ms=12000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=1_700_000_059_999_999, time_boot_ms=13000)},
            {"op": "dispatch", "frame": {**frame("SYSTEM_TIME", time_unix_usec=1_700_000_000_000_000, time_boot_ms=1), "component": 100}},
            {"op": "state"}]},
        {"name": "reboot_is_noticed", "last_boot_ms": 600000, "serial": True, "steps": [
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=605000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=594000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=2000)},
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=3000)}, {"op": "state"}]},
        {"name": "reboot_threshold", "last_boot_ms": 600000, "steps": [
            *[{"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=ms)}
              for ms in (600000, 595500, 590500, 590499, 0, 4999, 5000, 3)]]},
        {"name": "reboot_on_a_udp_link", "last_boot_ms": 600000, "steps": [
            {"op": "dispatch", "frame": frame("SYSTEM_TIME", time_unix_usec=0, time_boot_ms=2000)}]},
        {"name": "autopilot_version", "steps": [
            {"op": "dispatch", "frame": frame("AUTOPILOT_VERSION", flight_sw_version=(1 << 24) | (16 << 16) | (2 << 8), vendor_id=9900,
                                              product_id=22, capabilities=16, flight_custom_version=[0, 0, 0, 0xAB, 0xCD, 0xEF, 0x12, 0x34])},
            {"op": "dispatch", "frame": frame("AUTOPILOT_VERSION", flight_sw_version=(1 << 24) | (16 << 16) | (2 << 8), capabilities=131072)},
            {"op": "dispatch", "frame": frame("AUTOPILOT_VERSION", flight_sw_version=(4 << 24) | (6 << 16), capabilities=0,
                                              flight_custom_version=[ord(c) for c in "4a7b3c9d"])},
            {"op": "dispatch", "frame": frame("AUTOPILOT_VERSION", flight_sw_version=0, capabilities=16 | 131072)},
            {"op": "dispatch", "frame": {**frame("AUTOPILOT_VERSION", flight_sw_version=1 << 24), "component": 100}}, {"op": "state"}]},
        {"name": "routing", "log_sink": True, "steps": [
            {"op": "dispatch", "frame": frame(n)} for n in (
                "STATUSTEXT", "PARAM_VALUE", "FILE_TRANSFER_PROTOCOL", "LOG_ENTRY", "LOG_DATA", "SERIAL_CONTROL",
                "MISSION_CURRENT", "MISSION_ITEM_REACHED", "MISSION_ACK", "MISSION_COUNT", "MISSION_REQUEST", "MISSION_REQUEST_INT",
                "MISSION_ITEM", "MISSION_ITEM_INT", "RC_CHANNELS", "RC_CHANNELS_RAW", "ATTITUDE_TARGET", "POSITION_TARGET_LOCAL_NED",
                "OPEN_DRONE_ID_ARM_STATUS", "RADIO_STATUS", "NAMED_VALUE_FLOAT")]},
        {"name": "routing_from_another_system", "log_sink": True, "steps": [
            {"op": "dispatch", "frame": {**frame(n), "system": 5}} for n in (
                "STATUSTEXT", "PARAM_VALUE", "LOG_ENTRY", "MISSION_CURRENT", "RC_CHANNELS", "OPEN_DRONE_ID_ARM_STATUS", "RADIO_STATUS",
                "SERIAL_CONTROL", "MISSION_ACK", "HOME_POSITION", "VIBRATION", "EXTENDED_SYS_STATE")]},
        {"name": "routing_from_a_component", "log_sink": True, "steps": [
            {"op": "dispatch", "frame": {**frame(n), "component": 100}} for n in (
                "PARAM_VALUE", "FILE_TRANSFER_PROTOCOL", "LOG_ENTRY", "MISSION_CURRENT", "MISSION_ITEM_REACHED", "HOME_POSITION", "STATUSTEXT",
                "AUTOPILOT_VERSION", "SYSTEM_TIME")]},
        {"name": "log_sink_absent_or_broken", "steps": [{"op": "dispatch", "frame": frame("LOG_ENTRY")}]},
        {"name": "log_sink_raises", "log_sink_raises": True, "steps": [{"op": "dispatch", "frame": frame("LOG_DATA")}]},
        {"name": "gnss_integrity_routes", "steps": [
            {"op": "dispatch", "frame": frame("GNSS_INTEGRITY", id=0, jamming_state=2, spoofing_state=1, gnss_signal_quality=6)},
            {"op": "dispatch", "frame": {"unknown": 441, "data": gnss_frame()}},
            {"op": "dispatch", "frame": {"unknown": 441, "data": gnss_frame(), "system": 7}},
            {"op": "dispatch", "frame": {"unknown": 442, "data": "00"}}]},
        {"name": "vibration_and_landed_state", "steps": [
            {"op": "dispatch", "frame": frame("VIBRATION", vibration_x=0.123456, vibration_y=1.5, vibration_z=0.00004, clipping_0=1, clipping_1=2, clipping_2=3)},
            *[{"op": "dispatch", "frame": frame("EXTENDED_SYS_STATE", landed_state=n)} for n in (0, 1, 2, 3, 4, 5)]]},
        {"name": "home_position", "steps": [
            {"op": "dispatch", "frame": frame("HOME_POSITION", latitude=481234567, longitude=117654321, altitude=520500)},
            {"op": "state"}]},
        {"name": "heartbeat_then_radio_status", "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "dispatch", "frame": {**frame("RADIO_STATUS", remrssi=150, txbuf=100), "system": 51}},
            {"op": "dispatch", "frame": hb()}]},
        {"name": "frames_filtered_by_the_fleet", "steps": [
            {"op": "dispatch", "frame": hb()}, {"op": "dispatch", "frame": {**hb(), "component": 240}},
            {"op": "dispatch", "frame": frame("ATTITUDE", roll=0.0)}, {"op": "dispatch", "frame": {**frame("ATTITUDE", roll=0.0), "system": 3}}]},
        {"name": "duplicates", "steps": [
            {"op": "process", "frame": {**hb(), "raw": "aabbcc"}}, {"op": "advance", "seconds": 0.1},
            {"op": "process", "frame": {**hb(), "raw": "aabbcc"}}, {"op": "process", "frame": {**hb(), "raw": "ddeeff"}},
            {"op": "advance", "seconds": 0.5}, {"op": "process", "frame": {**hb(), "raw": "aabbcc"}},
            {"op": "advance", "seconds": 0.2}, {"op": "process", "frame": {**hb(), "raw": "aabbcc"}},
            {"op": "process", "frame": hb()}, {"op": "process", "frame": hb()}, {"op": "state"}]},
        {"name": "command_ack_logging", "steps": [
            {"op": "dispatch", "frame": frame("COMMAND_ACK", command=400, result=0)}, {"op": "dispatch", "frame": frame("COMMAND_ACK", command=22, result=5)},
            {"op": "dispatch", "frame": frame("COMMAND_ACK", command=22, result=4)}, {"op": "dispatch", "frame": frame("COMMAND_ACK", command=22, result=77)},
            {"op": "dispatch", "frame": {**frame("COMMAND_ACK", command=400, result=0), "system": 9}},
            {"op": "dispatch", "frame": frame("COMMAND_ACK", command=212, result=5, progress=40)},
            {"op": "dispatch", "frame": frame("COMMAND_ACK", command=212, result=0)}]},
        {"name": "command_ok", "acks": {"400": [[ack(0.2, 0, command=400)]]}, "steps": [{"op": "command", "command": 400, "params": [1.0], "timeout": 3.0, "retries": 2}]},
        {"name": "command_denied", "acks": {"400": [[ack(0.2, 2, command=400)]]}, "steps": [{"op": "command", "command": 400, "params": [1.0], "timeout": 3.0, "retries": 2}]},
        {"name": "command_retried", "acks": {"400": [[], [ack(0.4, 0, command=400)]]},
         "steps": [{"op": "command", "command": 400, "params": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], "timeout": 1.0, "retries": 2}]},
        {"name": "command_never_answered", "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 2}]},
        {"name": "command_not_ready", "ready": False, "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 1}]},
        {"name": "command_send_fails", "send_fails": True, "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 1}]},
        {"name": "command_in_progress_then_final", "acks": {"241": [[ack(0.2, 5, command=241), ack(1.5, 0, command=241)]]},
         "steps": [{"op": "command", "command": 241, "params": [], "timeout": 1.0, "retries": 2}]},
        {"name": "command_in_progress_never_finishes", "acks": {"241": [[ack(0.2, 5, command=241)]]},
         "steps": [{"op": "command", "command": 241, "params": [], "timeout": 1.0, "retries": 2}]},
        {"name": "command_accepts_in_progress", "acks": {"212": [[ack(0.2, 5, command=212)]]},
         "steps": [{"op": "command", "command": 212, "params": [1.0], "timeout": 1.0, "retries": 0, "accept_in_progress": True}]},
        {"name": "command_interim_then_final", "acks": {"241": [[ack(0.2, 1, command=241), ack(0.8, 0, command=241)]]},
         "steps": [{"op": "command", "command": 241, "params": [], "timeout": 2.0, "retries": 0, "interim": [1]}]},
        {"name": "command_interim_only", "acks": {"241": [[ack(0.2, 1, command=241)]]},
         "steps": [{"op": "command", "command": 241, "params": [], "timeout": 1.0, "retries": 2, "interim": [1]}]},
        {"name": "command_ack_for_another_command", "acks": {"400": [[ack(0.2, 0, command=22)]]},
         "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 0}]},
        {"name": "command_ack_from_another_system", "acks": {"400": [[ack(0.2, 0, command=400, system=9)]]},
         "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 0}]},
        {"name": "command_ack_for_another_station", "acks": {"400": [[ack(0.2, 0, command=400, target_system=7)]]},
         "steps": [{"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 0}]},
        {"name": "command_int_ok", "acks": {"192": [[ack(0.3, 0, command=192)]]},
         "steps": [{"op": "command_int", "command": 192, "frame": 3, "params": [-1.0, 1.0], "x": 481234567, "y": 117654321, "z": 30.5,
                    "timeout": 1.0, "retries": 1}]},
        {"name": "command_int_in_progress", "acks": {"192": [[ack(0.3, 5, command=192), ack(1.4, 0, command=192)]]},
         "steps": [{"op": "command_int", "command": 192, "frame": 3, "params": [], "x": 1, "y": 2, "z": 3.0, "timeout": 1.0, "retries": 1}]},
        {"name": "command_int_timeout", "steps": [{"op": "command_int", "command": 192, "frame": 0, "params": [], "x": 1, "y": 2, "z": 3.0,
                                                    "timeout": 0.5, "retries": 1}]},
        {"name": "command_int_send_fails", "send_fails": True, "steps": [
            {"op": "command_int", "command": 192, "frame": 0, "params": [], "x": 1, "y": 2, "z": 3.0, "timeout": 0.5, "retries": 1}]},
        {"name": "command_int_not_ready", "ready": False, "steps": [
            {"op": "command_int", "command": 192, "frame": 0, "params": [], "x": 1, "y": 2, "z": 3.0, "timeout": 0.5, "retries": 1}]},
        {"name": "cancel_wakes_the_waiters", "steps": [{"op": "cancel"}]},
        {"name": "registry_is_empty_after_a_command", "acks": {"400": [[ack(0.2, 0, command=400)]]}, "steps": [
            {"op": "command", "command": 400, "params": [], "timeout": 1.0, "retries": 0}, {"op": "pending"}]},
    ]


def receive_cases() -> list[dict]:
    import datetime
    import logging
    import types

    ReceiveBridge, rx = receive_bridge_class()
    out = []
    for sc in receive_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = ReceiveBridge(sc, clock)
        FakeEvent.sim = br.sim
        if sc.get("no_connection"):
            br._conn = None

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)

        def monotonic(clock=clock):
            return clock.t

        def connection_ready(self=br):
            return bool(self._conn and self.sc.get("ready", True))

        saved = (rx._PendingAck, )
        rx._PendingAck = FakeAckPending
        br._connection_ready = connection_ready
        results = []
        try:
            with mock.patch("time.monotonic", monotonic):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    result = None
                    if op == "plausible":
                        result = rx._is_plausible_heartbeat(receive_frame(step["frame"]))
                    elif op == "vehicle_heartbeat":
                        result = rx._is_vehicle_heartbeat(receive_frame(step["frame"]))
                    elif op == "sys_helpers":
                        msg = receive_message("SYS_STATUS", step["fields"])
                        result = [rx._sensor_health(msg, 32), rx._prearm_ok(msg)]
                    elif op == "range":
                        result = rx._rangefinder_distance_m(receive_message("DISTANCE_SENSOR", step["fields"]))
                    elif op == "gps_helpers":
                        msg = receive_message("GPS_RAW_INT", step["fields"])
                        result = [rx._gps_altitude_m(msg), rx._gps_accuracy_m(msg.h_acc), rx._gps_accuracy_m(msg.v_acc)]
                    elif op == "git_hash":
                        result = rx.ReceivePathMixin._decode_git_hash(step["raw"])
                    elif op == "who":
                        msg = receive_frame(step["frame"])
                        result = [br._is_from_vehicle(msg), br._is_from_autopilot(msg)]
                    elif op == "ack_for":
                        result = br._ack_is_for_us(receive_frame(step["frame"]))
                    elif op == "dispatch":
                        br._dispatch(receive_frame(step["frame"]))
                    elif op == "process":
                        br._process_frame(receive_frame(step["frame"]))
                    elif op == "mark_degraded":
                        br._mark_degraded_once()
                    elif op == "set_battery":
                        br.set_battery_settings(step["raw"])
                    elif op == "advance":
                        clock.t += step["seconds"]
                    elif op == "command":
                        result = br._send_command_and_wait(
                            step["command"], step["params"], timeout=step["timeout"], retries=step["retries"],
                            accept_in_progress=step.get("accept_in_progress", False), interim=frozenset(step.get("interim", [])))
                    elif op == "command_int":
                        result = br._send_command_int_and_wait(
                            step["command"], step["frame"], step["params"], x=step["x"], y=step["y"], z=step["z"],
                            timeout=step["timeout"], retries=step["retries"])
                    elif op == "cancel":
                        br._pending_acks[1] = FakeAckPending()
                        br._cancel_pending_commands()
                        result = br._pending_acks[1].result
                    elif op == "pending":
                        result = len(br._pending_acks)
                    elif op == "state":
                        result = {"snapshot": dict(br.snap), "degraded": br._degraded_warned, "ignored": sorted(br._ignored_systems),
                                  "duplicates": br._duplicate_frames, "rangefinder_seen": br._rangefinder_seen,
                                  "last_boot_ms": br._last_boot_ms, "kill_evidence": br._kill_evidence_at,
                                  "firmware": br._firmware_version, "board": [br._board_vendor_id, br._board_product_id],
                                  "encoding": br._param_encoding_declared, "request_sent": br._request_sent,
                                  "stack": br._dialect.stack, "mav_type": br._mav_type_id, "hb": list(br._hb_times)}
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:], "t": round(clock.t - 1000.0, 6)})
        finally:
            rx._PendingAck = saved[0]
            logger.removeHandler(handler)
            logger.setLevel(old_level)
            FakeEvent.sim = None
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# --------------------------------------------------------------- status ----

class StatusStore:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def update(self, **fields) -> None:
        self.bridge.log.append(["update", fields])
        self.bridge.snap.update(fields)

    def get_snapshot(self) -> dict:
        return dict(self.bridge.snap)

    def resolve_warnings(self, kinds) -> None:
        self.bridge.log.append(["resolve_warnings", sorted(kinds)])

    def merge_warning(self, text, level, event=None) -> None:
        self.bridge.log.append(["warning", text, level, event])


class StatusMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def command_long_send(self, *args):
        if self.bridge.sc.get("send_fails"):
            raise OSError("boom")
        self.bridge.log.append(["send", "command_long_send", list(args)])


def status_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from tests.reference import mavlink_bridge_status_python as ref

    class StatusBridge(ref.StatusMixin):
        def __init__(self, sc: dict, clock: Clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self.snap = dict(sc.get("snapshot", {}))
            self._store = StatusStore(self)
            self._send_lock = FlightSendLock(self)
            self._conn = None if sc.get("no_connection") else type("Conn", (), {})()
            if self._conn is not None:
                self._conn.mav = StatusMav(self)
            self._target_system = 1
            self._target_component = 1
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            self._statustext_chunks = {}
            self._prearm_reasons = []
            self._prearm_reason_at = 0.0
            self._prearm_requested_at = 0.0
            self._kill_evidence_at = 0.0
            self._prearm_last = None
            self._prearm_report_started_at = 0.0
            self._prearm_report_kill = False

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])

        def _store_update_warning(self, text, level, event=None) -> None:
            self._store.merge_warning(text, level, event=event)

    return StatusBridge


def status_scenarios() -> list[dict]:
    st = lambda text, sev=6, **kw: {"op": "text", "text": text, "severity": sev, **kw}
    chunk = lambda text, id, seq, sev=6, **kw: {"op": "text", "text": text, "severity": sev, "id": id, "seq": seq, **kw}
    AP = {"stack": "ardupilot"}
    return [
        {"name": "severities", "steps": [st("hello", n) for n in range(8)]},
        {"name": "text_is_cut_at_the_nul_and_stripped", "steps": [
            st("tab ends the event\t"), st("spaces   "), st("nul\x00after"), st(""), st("\x1c\x1dodd end\x1f"), st("café ✓")]},
        {"name": "events_px4", "steps": [
            st("Kill engaged", 6), st("Kill disengaged", 6), st("Disarmed by landing", 6), st("Disarmed by failsafe", 4),
            st("kill-switch engaged", 2), {"op": "state"}]},
        {"name": "events_ardupilot", **AP, "steps": [
            st("RC7: MotorEStop HIGH", 6), st("RC7: MotorEStop LOW", 6), st("RC12: MotorEStop HIGH", 2), {"op": "state"}]},
        {"name": "events_worst_level_wins", "steps": [
            st("Kill engaged", 2), st("Kill disengaged", 6), st("Disarmed by landing", 2), st("Disarmed by landing", 6)]},
        {"name": "unkill_clears_the_termination", "snapshot": {"kill_switch": True, "flight_termination": True}, "steps": [
            st("Kill disengaged"), {"op": "state"}]},
        {"name": "unkill_without_the_switch", "snapshot": {"flight_termination": True}, "steps": [st("Kill disengaged")]},
        {"name": "routine_lines_stay_off_the_board", "steps": [
            st("[logger] Opened full log file", 6), st("[logger] Opened full log file", 4), st("Calibration done", 6)]},
        {"name": "routine_lines_ardupilot", **AP, "steps": [st("[logger] Opened full log file", 6)]},
        {"name": "prearm_failures_px4", "steps": [
            st("Preflight Fail: Compass 0 uncalibrated", 4), st("Preflight Fail: Compass 0 uncalibrated", 4),
            st("Preflight Fail: Baro 0 missing", 4), st("Preflight Fail: ", 4), {"op": "state"}]},
        {"name": "prearm_failures_ardupilot", **AP, "steps": [
            st("PreArm: Compass not calibrated", 4), st("Arm: Throttle not zero", 4), st("PreArm:", 4), {"op": "state"}]},
        {"name": "prearm_reasons_are_capped", "steps": [
            *[st(f"Preflight Fail: reason {i}", 4) for i in range(24)], {"op": "state"}]},
        {"name": "a_new_report_starts_after_a_gap", "steps": [
            st("Preflight Fail: first", 4), {"op": "advance", "seconds": 1.0}, st("Preflight Fail: second", 4),
            {"op": "state"}, {"op": "advance", "seconds": 2.5}, st("Preflight Fail: third", 4), {"op": "state"}]},
        {"name": "kill_named_by_a_failing_check", "steps": [
            st("Preflight Fail: Kill switch engaged", 4), {"op": "state"}, st("Preflight Fail: Kill switch engaged", 4)]},
        {"name": "kill_named_by_a_check_when_the_switch_is_known", "snapshot": {"kill_switch": True}, "steps": [
            st("Preflight Fail: Kill switch engaged", 4), {"op": "state"}]},
        {"name": "kill_named_by_a_check_ardupilot", **AP, "steps": [st("PreArm: Motors Emergency Stopped", 4), {"op": "state"}]},
        {"name": "chunks_in_order", "steps": [
            chunk("A" * 50, 5, 0), chunk("B" * 20, 5, 1), {"op": "state"}]},
        {"name": "chunks_out_of_order", "steps": [
            chunk("B" * 20, 6, 1), chunk("A" * 50, 6, 0), {"op": "state"}]},
        {"name": "chunks_three", "steps": [
            chunk("A" * 50, 7, 0, 6), chunk("B" * 50, 7, 1, 3), chunk("C" * 5, 7, 2, 5), {"op": "state"}]},
        {"name": "chunks_missing_one", "steps": [
            chunk("A" * 50, 8, 0), chunk("C" * 5, 8, 2), {"op": "state"}, chunk("B" * 50, 8, 1), {"op": "state"}]},
        {"name": "chunks_from_two_senders", "steps": [
            chunk("A" * 50, 9, 0, system=1, component=1), chunk("X" * 50, 9, 0, system=2, component=1),
            chunk("B" * 5, 9, 1, system=1, component=1), chunk("Y" * 5, 9, 1, system=2, component=1), {"op": "state"}]},
        {"name": "chunks_time_out", "steps": [
            chunk("A" * 50, 10, 0), {"op": "state"}, {"op": "advance", "seconds": 11.0}, chunk("Z" * 5, 11, 0), {"op": "state"},
            {"op": "cleanup"}, {"op": "state"}]},
        {"name": "chunks_complete_without_a_terminator", "steps": [
            chunk("A" * 50, 12, 0), chunk("B" * 50, 12, 1), chunk("C" * 50, 12, 2), {"op": "state"}]},
        {"name": "chunks_are_capped", "steps": [
            *[x for i in range(66) for x in (chunk("A" * 50, 100 + i, 0), {"op": "advance", "seconds": 0.01})], {"op": "state"},
            chunk("B" * 5, 100, 1), chunk("B" * 5, 165, 1), {"op": "state"}]},
        {"name": "chunk_with_a_known_event", "steps": [chunk("Kill ", 13, 0, 6), chunk("engaged", 13, 1, 6)]},
        {"name": "prearm_pass_clears_the_reasons", "steps": [
            st("Preflight Fail: Compass", 4), {"op": "prearm", "value": True}, {"op": "state"}]},
        {"name": "prearm_pass_releases_the_kill_switch_after_settling", "snapshot": {"kill_switch": True, "flight_termination": True},
         "steps": [{"op": "kill_evidence", "at": 1000.0}, {"op": "prearm", "value": True}, {"op": "advance", "seconds": 3.0},
                   {"op": "prearm", "value": True}, {"op": "state"}]},
        {"name": "prearm_pass_armed_says_nothing_about_the_switch", "snapshot": {"kill_switch": True, "armed": True}, "steps": [
            {"op": "prearm", "value": True}]},
        {"name": "prearm_pass_resolves_the_kill_warning", "steps": [{"op": "prearm", "value": True}]},
        {"name": "prearm_none_is_ignored", "steps": [{"op": "prearm", "value": None}, {"op": "state"}]},
        {"name": "prearm_refused_asks_for_the_report", "steps": [
            {"op": "advance", "seconds": 31.0}, {"op": "prearm", "value": False}, {"op": "prearm", "value": False},
            {"op": "advance", "seconds": 31.0}, {"op": "prearm", "value": False}, {"op": "state"}]},
        {"name": "prearm_refused_with_reasons_does_not_ask", "steps": [
            st("Preflight Fail: Compass", 4), {"op": "advance", "seconds": 40.0}, {"op": "prearm", "value": False}]},
        {"name": "prearm_turning_bad_asks_at_once", "steps": [
            {"op": "advance", "seconds": 5.0}, {"op": "prearm", "value": True}, {"op": "prearm", "value": False},
            {"op": "advance", "seconds": 1.0}, {"op": "prearm", "value": True}, {"op": "prearm", "value": False},
            {"op": "advance", "seconds": 2.5}, {"op": "prearm", "value": True}, {"op": "prearm", "value": False}]},
        {"name": "prearm_turning_bad_while_armed", "snapshot": {"armed": True}, "steps": [
            {"op": "advance", "seconds": 5.0}, {"op": "prearm", "value": True}, {"op": "prearm", "value": False}]},
        {"name": "kill_is_rechecked_on_ardupilot", **AP, "snapshot": {"kill_switch": True}, "steps": [
            {"op": "kill_evidence", "at": 1000.0}, {"op": "advance", "seconds": 4.0}, {"op": "prearm", "value": False},
            {"op": "advance", "seconds": 4.5}, {"op": "prearm", "value": False},
            {"op": "advance", "seconds": 1.5}, {"op": "prearm", "value": False}, {"op": "advance", "seconds": 5.0},
            {"op": "prearm", "value": False}]},
        {"name": "kill_is_not_rechecked_on_px4", "snapshot": {"kill_switch": True}, "steps": [
            {"op": "kill_evidence", "at": 1000.0}, {"op": "advance", "seconds": 6.0}, {"op": "prearm", "value": False}]},
        {"name": "a_whole_report_without_the_kill_releases_it", **AP, "snapshot": {"kill_switch": True},
         "steps": [{"op": "kill_evidence", "at": 990.0}, {"op": "advance", "seconds": 10.0}, {"op": "prearm", "value": False},
                   {"op": "advance", "seconds": 0.5}, st("PreArm: Compass not calibrated", 4), {"op": "advance", "seconds": 3.0},
                   {"op": "prearm", "value": False}, {"op": "state"}]},
        {"name": "a_report_naming_the_kill_keeps_it", **AP, "snapshot": {"kill_switch": True},
         "steps": [{"op": "kill_evidence", "at": 990.0}, {"op": "advance", "seconds": 10.0}, {"op": "prearm", "value": False},
                   {"op": "advance", "seconds": 0.5}, st("PreArm: Motors Emergency Stopped", 4), {"op": "advance", "seconds": 3.0},
                   {"op": "prearm", "value": False}, {"op": "state"}]},
        {"name": "a_report_still_arriving_is_not_judged", **AP, "snapshot": {"kill_switch": True},
         "steps": [{"op": "kill_evidence", "at": 990.0}, {"op": "advance", "seconds": 10.0}, {"op": "prearm", "value": False},
                   {"op": "advance", "seconds": 0.5}, st("PreArm: Compass not calibrated", 4), {"op": "advance", "seconds": 1.0},
                   {"op": "prearm", "value": False}, {"op": "state"}]},
        {"name": "report_request", "steps": [{"op": "request"}]},
        {"name": "report_request_without_a_link", "no_connection": True, "steps": [{"op": "request"}]},
        {"name": "report_request_fails", "send_fails": True, "steps": [{"op": "request"}]},
        {"name": "clearing_the_reasons", "steps": [st("Preflight Fail: Compass", 4), {"op": "clear"}, {"op": "state"}]},
    ]


def status_cases() -> list[dict]:
    import logging

    StatusBridge = status_bridge_class()
    out = []
    for sc in status_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = StatusBridge(sc, clock)

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)

        def monotonic(clock=clock):
            return clock.t

        results = []
        try:
            with mock.patch("time.monotonic", monotonic):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    result = None
                    if op == "text":
                        fields = {"severity": step["severity"], "text": step["text"].encode("utf-8")}
                        if "id" in step:
                            fields["id"], fields["chunk_seq"] = step["id"], step["seq"]
                        msg = receive_message("STATUSTEXT", fields, step.get("system", 1), step.get("component", 1))
                        br._handle_statustext(msg)
                    elif op == "cleanup":
                        br._cleanup_statustext_chunks()
                    elif op == "advance":
                        clock.t += step["seconds"]
                    elif op == "prearm":
                        br._note_prearm_state(step["value"])
                    elif op == "kill_evidence":
                        br._kill_evidence_at = step["at"]
                    elif op == "request":
                        br._request_prearm_report()
                    elif op == "clear":
                        br._clear_prearm_reasons()
                    elif op == "state":
                        result = {"reasons": list(br._prearm_reasons), "reason_at": br._prearm_reason_at,
                                  "requested_at": br._prearm_requested_at, "kill_evidence": br._kill_evidence_at,
                                  "partials": len(br._statustext_chunks), "report_kill": br._prearm_report_kill,
                                  "report_started": br._prearm_report_started_at, "last": br._prearm_last,
                                  "snapshot": dict(br.snap)}
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:], "t": round(clock.t - 1000.0, 6)})
        finally:
            logger.removeHandler(handler)
            logger.setLevel(old_level)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# ------------------------------------------------------------- requests ----

class RequestsMav:
    def __init__(self, bridge) -> None:
        self.bridge = bridge

    def __getattr__(self, method):
        def call(*args):
            if self.bridge.sc.get("send_fails") in (method, True):
                raise OSError("boom")
            self.bridge.log.append(["send", method, list(args)])
        return call


def requests_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from corvus import autopilot as ap
    from tests.reference import mavlink_bridge_requests_python as rq

    class RequestsBridge(rq.RequestMixin):
        _MSG_ID_HOME_POSITION = 242
        _MSG_ID_AUTOPILOT_VERSION = 148
        _GCS_AUTHORITY_PARAMS = ("SYSID_MYGCS", "MAV_GCS_SYSID")

        def __init__(self, sc: dict) -> None:
            self.sc = sc
            self.log: list = []
            self.counts: dict = {}
            self._operation_lock = FlightLock()
            self._send_lock = FlightSendLockPlain()
            self._conn = None if sc.get("no_connection") else RequestsConn(self)
            self._target_system = 1
            self._target_component = 1
            self._dialect = ap.dialect_for_stack(sc.get("stack", "px4"))
            self._request_sent = sc.get("request_sent", False)
            self._conn_str = sc.get("conn_str", "udp:127.0.0.1:14550")
            self._store = type("Store", (), {"update": lambda s, **f: self.log.append(["update", f])})()

        def _is_slow_link(self) -> bool:
            return self.sc.get("slow", False)

        def _connection_ready(self) -> bool:
            return self.sc.get("ready", True)

        def _set_command_error(self, text) -> None:
            self.log.append(["error", text])

        def _console_publish(self, name, text, level) -> None:
            self.log.append(["console", name, text, level])

        def _store_update_warning(self, text, level, event=None) -> None:
            self.log.append(["warning", text, level])

        def _command_failure(self, action, result) -> bool:
            self.log.append(["failure", action, result])
            return False

        def _send_command_and_wait(self, command, params=None, timeout=3.0, retries=2, accept_in_progress=False,
                                   interim=frozenset()):
            self.log.append(["cmd", command, list(params), timeout, retries])
            key = str(int(params[0])) if command == 511 else str(command)
            plan = self.sc.get("results", {}).get(key, self.sc.get("default_result", 0))
            if isinstance(plan, list):
                n = self.counts.get(key, 0)
                self.counts[key] = n + 1
                return plan[min(n, len(plan) - 1)]
            return plan

        def fetch_params(self, names, timeout=3.0):
            self.log.append(["fetch", list(names), timeout])
            return {n: v for n, v in self.sc.get("params", {}).items() if n in names}

        def _schedule_version_retry(self) -> None:
            self.log.append(["call", "schedule_version_retry"])

    class FlightSendLockPlain:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

    class RequestsConn:
        def __init__(self, bridge) -> None:
            self.bridge = bridge
            self.mav = RequestsMav(bridge)
            self.WIRE_PROTOCOL_VERSION = bridge.sc.get("wire", "2.0")

        def auto_mavlink_version(self, first):
            if self.bridge.sc.get("switch_fails"):
                raise RuntimeError("no v2 dialect")
            self.bridge.log.append(["switch_to_mavlink2", first.hex()])
            self.WIRE_PROTOCOL_VERSION = "2.0"

        def setup_signing(self, key, sign_outgoing=False, allow_unsigned_callback=None):
            if self.bridge.sc.get("setup_fails"):
                raise RuntimeError("signing unsupported")
            self.bridge.log.append(["setup_signing", key.hex(), sign_outgoing,
                                    allow_unsigned_callback is rq._allow_unsigned_signed_link])

    return RequestsBridge, rq


def requests_scenarios() -> list[dict]:
    AP = {"stack": "ardupilot"}
    key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
    return [
        {"name": "tables", "steps": [{"op": "tables", "slow": s} for s in (False, True)]},
        {"name": "streams_sent_once", "steps": [{"op": "streams"}, {"op": "streams"}, {"op": "state"}]},
        {"name": "streams_on_a_radio", "slow": True, "steps": [{"op": "streams"}, {"op": "state"}]},
        {"name": "streams_already_sent", "request_sent": True, "steps": [{"op": "streams"}]},
        {"name": "streams_without_a_link", "no_connection": True, "steps": [{"op": "streams"}, {"op": "state"}]},
        {"name": "streams_fail_one_by_one", "send_fails": "request_data_stream_send", "steps": [{"op": "streams"}, {"op": "state"}]},
        {"name": "intervals_accepted", "steps": [{"op": "intervals"}]},
        {"name": "intervals_on_a_radio", "slow": True, "steps": [{"op": "intervals"}]},
        {"name": "intervals_denied_for_one_message", "results": {"242": 2, "24": 2}, "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "intervals_unsupported_falls_back", "default_result": 3, "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "intervals_unsupported_later", "results": {"1": 3, "30": 0, "33": 0, "74": 0}, "steps": [{"op": "intervals"}]},
        {"name": "intervals_without_acks_give_up", "default_result": -1, "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "intervals_silence_then_answers", "results": {"30": -1, "33": 0, "74": -1, "1": -1, "24": 0, "242": -1, "245": 0},
         "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "intervals_link_torn_down", "default_result": -2, "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "intervals_not_ready", "ready": False, "steps": [{"op": "intervals"}]},
        {"name": "intervals_radio_fallback", "slow": True, "default_result": 3, "steps": [{"op": "intervals"}, {"op": "state"}]},
        {"name": "set_interval_loud", "steps": [
            {"op": "set_interval", "id": 30, "us": 20000, "quiet": False}, {"op": "set_interval", "id": 30, "us": 20000, "quiet": True}]},
        {"name": "set_interval_refused", "default_result": 2, "steps": [
            {"op": "set_interval", "id": 30, "us": 20000, "quiet": False}, {"op": "set_interval", "id": 30, "us": 20000, "quiet": True}]},
        {"name": "set_interval_odd_results", "results": {"30": 77}, "steps": [
            {"op": "set_interval", "id": 30, "us": 20000, "quiet": True}, {"op": "set_interval", "id": 30, "us": 20000, "quiet": False}]},
        {"name": "set_interval_not_ready", "ready": False, "steps": [{"op": "set_interval", "id": 30, "us": 1, "quiet": False}]},
        {"name": "home_request", "steps": [{"op": "home"}]},
        {"name": "home_request_without_a_link", "no_connection": True, "steps": [{"op": "home"}]},
        {"name": "home_request_fails", "send_fails": True, "steps": [{"op": "home"}]},
        {"name": "version_request", "steps": [{"op": "version"}]},
        {"name": "version_request_without_a_link", "no_connection": True, "steps": [{"op": "version"}]},
        {"name": "version_request_fails", "send_fails": True, "steps": [{"op": "version"}]},
        {"name": "authority_px4_is_not_checked", "params": {"SYSID_MYGCS": 255}, "steps": [{"op": "authority"}]},
        {"name": "authority_wrong", **AP, "params": {"SYSID_MYGCS": 255}, "steps": [{"op": "authority"}]},
        {"name": "authority_wrong_newer_name", **AP, "params": {"MAV_GCS_SYSID": 255.0}, "steps": [{"op": "authority"}]},
        {"name": "authority_right", **AP, "params": {"SYSID_MYGCS": 254}, "steps": [{"op": "authority"}]},
        {"name": "authority_rounds", **AP, "params": {"SYSID_MYGCS": 254.4}, "steps": [{"op": "authority"}]},
        {"name": "authority_first_name_wins", **AP, "params": {"SYSID_MYGCS": 254, "MAV_GCS_SYSID": 255}, "steps": [{"op": "authority"}]},
        {"name": "authority_no_answer", **AP, "params": {}, "steps": [{"op": "authority"}]},
        {"name": "authority_not_ready", **AP, "ready": False, "params": {"SYSID_MYGCS": 255}, "steps": [{"op": "authority"}]},
        {"name": "signing_off", "signing": {"none": True}, "steps": [{"op": "signing"}]},
        {"name": "signing_on", "signing": {"key": key}, "steps": [{"op": "signing"}]},
        {"name": "signing_switches_to_mavlink2_first", "wire": "1.0", "signing": {"key": key}, "steps": [{"op": "signing"}, {"op": "signing"}]},
        {"name": "signing_key_unusable", "signing": {"error": "OSError", "message": "no such file"}, "steps": [{"op": "signing"}]},
        {"name": "signing_key_wrong", "signing": {"error": "ValueError", "message": "key must be 32 bytes"}, "steps": [{"op": "signing"}]},
        {"name": "signing_cannot_switch", "wire": "1.0", "switch_fails": True, "signing": {"key": key}, "steps": [{"op": "signing"}]},
        {"name": "signing_cannot_be_set_up", "setup_fails": True, "signing": {"key": key}, "steps": [{"op": "signing"}]},
    ]


def requests_cases() -> list[dict]:
    import logging

    RequestsBridge, rq = requests_bridge_class()
    out = []
    for sc in requests_scenarios():
        br = RequestsBridge(sc)

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)

        saved = rq._load_signing_key
        spec = sc.get("signing", {})

        def load_key(spec=spec):
            if spec.get("error"):
                raise {"OSError": OSError, "ValueError": ValueError}[spec["error"]](spec["message"])
            if spec.get("key"):
                return bytes.fromhex(spec["key"])
            return None

        rq._load_signing_key = load_key
        results = []
        try:
            for step in sc["steps"]:
                before = len(br.log)
                op = step["op"]
                result = None
                if op == "tables":
                    br.sc["slow"] = step["slow"]
                    result = {"streams": [[k, v] for k, v in br._stream_rates().items()],
                              "intervals": [[k, v] for k, v in br._message_intervals().items()]}
                elif op == "streams":
                    br._request_streams()
                elif op == "intervals":
                    br._request_message_intervals()
                elif op == "set_interval":
                    ok, code = br._set_message_interval(step["id"], step["us"], quiet=step["quiet"])
                    result = [ok, code]
                elif op == "home":
                    br._request_home()
                elif op == "version":
                    br._request_version()
                elif op == "authority":
                    br._check_gcs_authority()
                elif op == "signing":
                    try:
                        br._apply_signing()
                        result = "ok"
                    except ConnectionError as exc:
                        result = f"connection error: {exc}"
                elif op == "state":
                    result = {"request_sent": br._request_sent}
                else:
                    raise SystemExit(op)
                results.append({"result": result, "events": br.log[before:]})
        finally:
            rq._load_signing_key = saved
            logger.removeHandler(handler)
            logger.setLevel(old_level)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# ----------------------------------------------------------------- tlog ----

class FakeRunning:
    def __init__(self, value: bool) -> None:
        self.value = value

    def is_set(self) -> bool:
        return self.value


def tlog_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import mavlink_bridge_tlog_python as ref

    class TlogBridge(ref.TlogSessionMixin):
        def __init__(self, sc: dict) -> None:
            self.sc = sc
            self.log: list = []
            self._running = FakeRunning(sc.get("running", True))
            self._conn_str = sc.get("conn", "udp:127.0.0.1:14550")
            self._tlog = None
            self._tlog_armed = False
            self._frame_sink = None

        def _request_version(self) -> None:
            self.log.append(["call", "request_version"])

    return TlogBridge, ref


def tlog_scenarios() -> list[dict]:
    frames = ["fd0900000100010000000000", "fe0900", "aa" * 280, "00"]
    return [
        {"name": "one_session", "stamps": ["20260102-030405-000007"], "steps": [
            {"op": "start"}, *[{"op": "frame", "raw": f} for f in frames], {"op": "stop"}, {"op": "files"}]},
        {"name": "not_running", "running": False, "steps": [{"op": "start"}, {"op": "frame", "raw": "00"}, {"op": "stop"}, {"op": "files"}]},
        {"name": "frames_without_a_session", "steps": [{"op": "frame", "raw": "00"}, {"op": "frame", "raw": None}]},
        {"name": "empty_session_has_a_header", "stamps": ["20260102-030405-000001"], "steps": [{"op": "start"}, {"op": "stop"}, {"op": "files"}]},
        {"name": "conn_in_the_header", "conn": "serial:/dev/ttyACM0 \u00e4\u00f6 \"x\" \U0001F680", "stamps": ["20260102-030405-000002"],
         "steps": [{"op": "start"}, {"op": "stop"}, {"op": "files"}]},
        {"name": "two_sessions_one_tick", "stamps": ["20260102-030405-000003", "20260102-030405-000003", "20260102-030405-000003"], "steps": [
            {"op": "start"}, {"op": "frame", "raw": "01"}, {"op": "start"}, {"op": "frame", "raw": "02"}, {"op": "start"},
            {"op": "frame", "raw": "03"}, {"op": "stop"}, {"op": "files"}]},
        {"name": "start_closes_the_old_one", "stamps": ["20260102-030405-000004", "20260102-030406-000004"], "steps": [
            {"op": "start"}, {"op": "frame", "raw": "0a"}, {"op": "start"}, {"op": "frame", "raw": "0b"}, {"op": "stop"}, {"op": "files"}]},
        {"name": "rolls_on_disarm", "stamps": ["20260102-030405-000005", "20260102-030500-000005"], "steps": [
            {"op": "start"}, {"op": "roll", "armed": False}, {"op": "frame", "raw": "01"}, {"op": "roll", "armed": True},
            {"op": "frame", "raw": "02"}, {"op": "roll", "armed": True}, {"op": "roll", "armed": False}, {"op": "frame", "raw": "03"},
            {"op": "roll", "armed": False}, {"op": "stop"}, {"op": "files"}]},
        {"name": "no_roll_without_a_session", "steps": [{"op": "roll", "armed": True}, {"op": "roll", "armed": False}]},
        {"name": "sink_gets_the_same_bytes", "stamps": ["20260102-030405-000006"], "steps": [
            {"op": "sink"}, {"op": "frame", "raw": "aa"}, {"op": "start"}, {"op": "frame", "raw": "bb"}, {"op": "frame", "raw": None},
            {"op": "stop"}, {"op": "frame", "raw": "cc"}, {"op": "files"}]},
        {"name": "unusable_log_dir", "bad_dir": True, "steps": [{"op": "start"}, {"op": "frame", "raw": "00"}, {"op": "files"}]},
        {"name": "stop_twice", "stamps": ["20260102-030405-000008"], "steps": [{"op": "start"}, {"op": "stop"}, {"op": "stop"}, {"op": "files"}]},
    ]


def _tlog_files(directory: pathlib.Path) -> dict:
    import re

    out = {}
    for path in sorted(directory.iterdir()):
        data = path.read_bytes()
        magic = b"#CORVUS-TLOG "
        head, _, body = data.partition(b"\n")
        meta = json.loads(head[len(magic):]) if head.startswith(magic) else None
        if meta:
            from corvus.version import get_version

            meta["version"] = "VERSION" if meta["version"] == get_version() else meta["version"]
            meta["started_at"] = bool(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d{6})?\+00:00", meta["started_at"]))
        out[path.name] = {"meta": meta, "body": body.hex(), "header": head.hex() if meta is None else ""}
    return out


def tlog_cases() -> list[dict]:
    import datetime
    import logging
    import tempfile

    TlogBridge, ref = tlog_bridge_class()
    out = []
    for sc in tlog_scenarios():
        br = TlogBridge(sc)
        tmp = pathlib.Path(tempfile.mkdtemp(prefix="corvus-tlog-"))
        log_dir = tmp / "logs"
        if sc.get("bad_dir"):
            tmp.joinpath("logs").write_text("not a folder")
        stamps = list(sc.get("stamps", []))

        class FakeNow:
            @staticmethod
            def now():
                stamp = stamps.pop(0)
                day, clock, micro = stamp.split("-")
                return datetime.datetime(int(day[:4]), int(day[4:6]), int(day[6:]), int(clock[:2]), int(clock[2:4]),
                                         int(clock[4:]), int(micro))

        class Capture(logging.Handler):
            def emit(self, record):
                text = record.getMessage()
                if text.startswith("tlog disabled"):
                    text = "tlog disabled"
                br.log.append(["log", record.levelname.lower(), text])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)
        saved = (ref.default_log_dir, ref.datetime)
        ref.default_log_dir = lambda: str(log_dir)
        ref.datetime = type("DT", (), {"datetime": FakeNow})
        sink_log: list = []
        results = []
        try:
            for step in sc["steps"]:
                before = len(br.log)
                op = step["op"]
                result = None
                if op == "start":
                    br._start_tlog()
                    result = br._tlog is not None
                elif op == "stop":
                    br._stop_tlog()
                elif op == "frame":
                    msg = type("M", (), {})()
                    if step["raw"] is not None:
                        msg.get_msgbuf = lambda raw=bytes.fromhex(step["raw"]): raw
                    br._write_tlog(msg)
                elif op == "roll":
                    br._roll_tlog_on_disarm(step["armed"])
                    result = br._tlog is not None
                elif op == "sink":
                    br._frame_sink = lambda raw: sink_log.append(raw.hex())
                elif op == "files":
                    result = {"files": _tlog_files(log_dir) if log_dir.is_dir() else {}, "sink": list(sink_log)}
                else:
                    raise SystemExit(op)
                results.append({"result": result, "events": br.log[before:]})
        finally:
            br._stop_tlog()
            ref.default_log_dir, ref.datetime = saved
            logger.removeHandler(handler)
            logger.setLevel(old_level)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# ----------------------------------------------------------------- rtcm ----

def rtcm_bridge_class():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import mavlink_bridge_rtcm_python as ref
    from tests.reference import rtk_python as rtk

    class Mav:
        def __init__(self, bridge) -> None:
            self.bridge = bridge

        def gps_rtcm_data_send(self, flags, length, data):
            b = self.bridge
            if b.sc.get("fail_on") is not None and len([e for e in b.log if e[0] == "send"]) == b.sc["fail_on"]:
                raise OSError("boom")
            b.log.append(["send", flags, length, bytes(data).hex()])

    class RtcmBridge(ref.RtcmInjectionMixin):
        def __init__(self, sc, clock) -> None:
            self.sc = sc
            self.clock = clock
            self.log: list = []
            self._rtcm_lock = threading.Lock()
            self._rtcm_sequence = 0
            self._rtcm_bytes = 0
            self._rtcm_messages = 0
            self._rtcm_dropped = 0
            self._rtcm_last_send = 0.0
            self._send_lock = FlightSendLockPlain2()
            self._conn = None if sc.get("no_connection") else type("Conn", (), {})()
            if self._conn is not None:
                self._conn.mav = Mav(self)

        def _connection_ready(self) -> bool:
            return self.sc.get("ready", True)

    class FlightSendLockPlain2:
        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

    return RtcmBridge, rtk


def rtcm_scenarios() -> list[dict]:
    frame = lambda n: bytes((i * 7 + 3) & 0xFF for i in range(n)).hex()
    return [
        {"name": "fragments", "steps": [{"op": "fragments", "frame": frame(n), "sequence": seq}
                                         for n in (0, 1, 100, 179, 180, 181, 359, 360, 361, 539, 540, 541, 719, 720, 721, 1000)
                                         for seq in (0, 5, 31, 32, 33)]},
        {"name": "inject_small", "steps": [{"op": "inject", "frame": frame(100)}, {"op": "stats"}]},
        {"name": "inject_fragmented", "steps": [{"op": "inject", "frame": frame(400)}, {"op": "inject", "frame": frame(360)}, {"op": "stats"}]},
        {"name": "sequence_wraps", "steps": [*[{"op": "inject", "frame": frame(10)} for _ in range(34)], {"op": "stats"}]},
        {"name": "too_long_is_dropped", "steps": [{"op": "inject", "frame": frame(720)}, {"op": "inject", "frame": frame(10)}, {"op": "stats"}]},
        {"name": "empty_is_refused", "steps": [{"op": "inject", "frame": ""}, {"op": "stats"}]},
        {"name": "not_ready", "ready": False, "steps": [{"op": "inject", "frame": frame(10)}, {"op": "stats"}]},
        {"name": "link_closed", "no_connection": True, "steps": [{"op": "inject", "frame": frame(10)}, {"op": "stats"}]},
        {"name": "a_fragment_fails", "fail_on": 1, "steps": [{"op": "inject", "frame": frame(400)}, {"op": "stats"}]},
        {"name": "stats_age_and_forget", "steps": [
            {"op": "inject", "frame": frame(10)}, {"op": "advance", "seconds": 2.5}, {"op": "stats"}, {"op": "forget"}, {"op": "stats"}]},
    ]


def rtcm_cases() -> list[dict]:
    import logging

    RtcmBridge, rtk = rtcm_bridge_class()
    out = []
    for sc in rtcm_scenarios():
        clock = Clock()
        clock.t = 1000.0
        br = RtcmBridge(sc, clock)

        class Capture(logging.Handler):
            def emit(self, record):
                br.log.append(["log", record.levelname.lower(), record.getMessage()])

        handler = Capture()
        logger = logging.getLogger("corvus.mavlink")
        logger.addHandler(handler)
        old_level = logger.level
        logger.setLevel(logging.DEBUG)
        results = []
        try:
            with mock.patch("time.monotonic", lambda clock=clock: clock.t):
                for step in sc["steps"]:
                    before = len(br.log)
                    op = step["op"]
                    result = None
                    if op == "fragments":
                        result = [[flags, data.hex()] for flags, data in rtk.fragments(bytes.fromhex(step["frame"]), step["sequence"])]
                    elif op == "inject":
                        result = br.inject_rtcm(bytes.fromhex(step["frame"]))
                    elif op == "stats":
                        result = br.rtcm_stats()
                    elif op == "advance":
                        clock.t += step["seconds"]
                    elif op == "forget":
                        br.forget_rtcm_session()
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "events": br.log[before:]})
        finally:
            logger.removeHandler(handler)
            logger.setLevel(old_level)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# ---------------------------------------------------------------- fleet ----

FLEET_MAV_TYPES = {
    0: "GENERIC", 1: "FIXED_WING", 2: "QUADROTOR", 3: "COAXIAL",
    4: "HELICOPTER", 7: "AIRSHIP", 8: "FREE_BALLOON", 9: "ROCKET",
    10: "GROUNDED_ROVER", 11: "SURFACE_BOAT", 12: "SUBMARINE",
    13: "HEXAROTOR", 14: "OCTOROTOR", 15: "TRIROTOR",
    19: "VTOL_DUOROTOR", 20: "VTOL_QUADROTOR", 21: "VTOL_TILTROTOR",
}


def fleet_ref():
    root = pathlib.Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(root))
    from corvus import serial_ports
    sys.modules.setdefault("tests.reference.serial_ports", serial_ports)
    from tests.reference import fleet_python as ref
    return ref


class FleetStoreStub:
    def __init__(self, log: list, link: str, snap: dict | None = None) -> None:
        self.log = log
        self.link = link
        self.snap = dict(snap or {})

    def get_snapshot(self) -> dict:
        return dict(self.snap)

    def update(self, **fields) -> None:
        self.snap.update(fields)
        self.log.append(["store", self.link, "update", fields])

    def forget_vehicle(self) -> None:
        self.log.append(["store", self.link, "forget_vehicle"])

    def shutdown(self) -> None:
        self.log.append(["store", self.link, "shutdown"])


def fleet_bridge_class(ref):
    from corvus.serial_ports import split_serial_connection

    class FleetBridgeStub:
        def __init__(self, link: str, connection: str, log: list, accept: bool = True) -> None:
            self.link = link
            self._conn = connection
            self.log = log
            self.fleet_tracker = ref.FleetTracker(FLEET_MAV_TYPES)
            self.controlled = None
            self.connected = True
            self.accept = accept

        @property
        def fleet_link_id(self) -> str:
            return self.link

        @fleet_link_id.setter
        def fleet_link_id(self, value: str) -> None:
            self.link = value

        def connection_string(self) -> str:
            return self._conn

        def controlled_system(self):
            return self.controlled

        def validate_connection(self, connection: str) -> None:
            if connection.startswith("bad:"):
                raise ValueError(f"{connection} is not a connection string")

        def serial_device(self) -> str:
            return split_serial_connection(self._conn)[0] if self._conn.startswith("serial:") else ""

        def start(self) -> None:
            self.log.append(["bridge", self.link, "start"])

        def stop(self) -> None:
            self.log.append(["bridge", self.link, "stop"])

        def is_connected(self) -> bool:
            return self.connected

        def inject_rtcm(self, frame: bytes) -> bool:
            self.log.append(["bridge", self.link, "rtcm", frame.hex()])
            return self.accept

    return FleetBridgeStub


def fh(link="main", system=1, component=1, autopilot=12, mav_type=2, base_mode=129, custom_mode=196608,
       status=4, vh=True) -> dict:
    return {"op": "frame", "link": link, "name": "HEARTBEAT", "system": system, "component": component, "vh": vh,
            "fields": {"autopilot": autopilot, "type": mav_type, "base_mode": base_mode,
                       "custom_mode": custom_mode, "system_status": status}}


def ff(name: str, fields: dict, link="main", system=1, component=1) -> dict:
    return {"op": "frame", "link": link, "name": name, "fields": fields, "system": system, "component": component,
            "vh": False}


def fgp(lat, lon, rel=12000, alt=480000, hdg=9000, vx=300, vy=400, **kw) -> dict:
    return ff("GLOBAL_POSITION_INT", {"lat": lat, "lon": lon, "relative_alt": rel, "alt": alt, "hdg": hdg,
                                      "vx": vx, "vy": vy}, **kw)


def fleet_scenarios() -> list[dict]:
    adv = lambda s: {"op": "advance", "seconds": s}
    snap = {"op": "snapshot"}
    sysq = lambda link="main": {"op": "systems", "link": link}
    bind = lambda link="main", system=1, component=1: {"op": "bind", "link": link, "system": system, "component": component}
    out: list[dict] = []

    out.append({"name": "tracker_unbound_and_heartbeats", "steps": [
        fh(), sysq(), bind(), fh(vh=False), sysq(), fh(), sysq(),
        fh(autopilot=3, mav_type=2, base_mode=1, custom_mode=4), sysq(),
        fh(autopilot=12, mav_type=99, base_mode=129, custom_mode=50593792, status=5), sysq(),
        fh(autopilot=3, mav_type=1, base_mode=1, custom_mode=11), sysq(),
        fh(autopilot=8, mav_type=0, base_mode=0, custom_mode=0, status=0), sysq(),
        bind("main", 0), bind("main", 256), sysq(),
    ]})
    out.append({"name": "tracker_position_and_speed", "steps": [
        bind(), fgp(473977000, 85455000), sysq(), fh(), fgp(473977000, 85455000), sysq(),
        fgp(0, 0), sysq(),
        fgp(910000000, 85455000), fgp(473977000, 1810000000), sysq(),
        fgp(-473977123, -85455999, rel=-1550, alt=-50, hdg=65535, vx=-30, vy=40), sysq(),
        fgp(473977000, 85455000, hdg=35999, vx=0, vy=0), sysq(),
        fgp(473977049, 85455051, rel=1049, alt=1051, hdg=12345, vx=1, vy=1), sysq(),
        ff("VFR_HUD", {"heading": 270, "groundspeed": 12.345}), sysq(),
        ff("VFR_HUD", {"heading": 359, "groundspeed": 0.05}), sysq(),
    ]})
    out.append({"name": "tracker_attitude_heading", "steps": [
        bind(), fh(), ff("ATTITUDE", {"yaw": 1.5707963267948966}), sysq(),
        ff("ATTITUDE", {"yaw": 3.0}), sysq(), bind("main", 1, 1), fh(),
        ff("VFR_HUD", {"heading": 10, "groundspeed": 1.0}), ff("ATTITUDE", {"yaw": 3.0}), sysq(),
        bind("main", 2), bind("main", 1), fh(), ff("ATTITUDE", {"yaw": -1.5707963267948966}), sysq(),
        bind("main", 2), fh(system=2), ff("ATTITUDE", {"yaw": -1e-20}, system=2), sysq(),
    ]})
    out.append({"name": "tracker_battery", "steps": [
        bind(), fh(),
        ff("SYS_STATUS", {"battery_remaining": 87, "voltage_battery": 12612}), sysq(),
        ff("SYS_STATUS", {"battery_remaining": -1, "voltage_battery": 65535}), sysq(),
        ff("SYS_STATUS", {"battery_remaining": 101, "voltage_battery": 0}), sysq(),
        ff("SYS_STATUS", {"battery_remaining": 0, "voltage_battery": 1}), sysq(),
        ff("SYS_STATUS", {"battery_remaining": 100, "voltage_battery": 25049}), sysq(),
    ]})
    out.append({"name": "tracker_component_and_system", "steps": [
        bind("main", 1, 1), fh(component=2), sysq(), fh(system=2), sysq(),
        fh(component=1), sysq(), fgp(473977000, 85455000, component=3), sysq(),
        fgp(473977000, 85455000, system=2), sysq(), fgp(473977000, 85455000), sysq(),
        fh(component=7), sysq(), bind("main", 1, 9), fh(component=9), sysq(),
        bind("main", 1, 0), fh(component=3), sysq(), fgp(473977000, 85455000, component=9), sysq(),
        bind("main", 2, 1), sysq(), fh(system=2), sysq(),
        {"op": "forget", "link": "main"}, sysq(), fh(system=2), sysq(),
    ]})
    out.append({"name": "tracker_statustext", "steps": [
        bind(), fh(),
        ff("STATUSTEXT", {"severity": 6, "text": "info only"}),
        ff("STATUSTEXT", {"severity": 4, "text": "  Low   battery \t warning  "}),
        ff("STATUSTEXT", {"severity": 2, "text": "Failsafe: RC lost"}),
        ff("STATUSTEXT", {"severity": 3, "text": "   "}),
        ff("STATUSTEXT", {"severity": 0, "text": "äö emergency"}),
        {"op": "events", "link": "main", "since": 0}, {"op": "events", "link": "main", "since": 2},
        *[ff("STATUSTEXT", {"severity": 4, "text": f"Warning number {i}"}) for i in range(45)],
        {"op": "events", "link": "main", "since": 0}, {"op": "events", "link": "main", "since": 40},
        {"op": "events", "link": "main", "since": 1000},
    ]})
    out.append({"name": "tracker_stale_and_forget", "steps": [
        bind(), fh(), adv(4.9), sysq(), adv(0.2), sysq(), adv(594.0), sysq(), adv(1.0), sysq(), adv(1.0), sysq(),
        fh(), sysq(), adv(7), fh(), sysq(),
    ]})

    out.append({"name": "fleet_single_vehicle", "steps": [
        snap, bind(), snap, fh(), fgp(473977000, 85455000), {"op": "controlled", "link": "main", "system": 1}, snap,
        {"op": "store", "link": "main", "fields": {"link_status": "connected", "link_error": ""}}, snap,
        {"op": "store", "link": "main", "fields": {"link_suggestion": {"connection_string": "serial:/dev/ttyACM0:115200"}}},
        snap, {"op": "store", "link": "main", "fields": {"link_suggestion": "junk"}}, snap, {"op": "config"},
    ]})
    out.append({"name": "fleet_two_links_colours", "steps": [
        bind(), fh(), {"op": "controlled", "link": "main", "system": 1},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 1, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2")},
        snap, {"op": "controlled", "link": "link-2", "system": 1}, snap, {"op": "config"},
        {"op": "select", "key": "link-2:1"}, snap, {"op": "select", "key": "main:1"}, snap,
        {"op": "set_meta", "key": "link-2:1", "color": "#00a3b4", "name": "Scout"}, snap, adv(10), snap,
        {"op": "remove_link", "id": "link-2"}, snap, {"op": "config"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14552"}, {"op": "bind_extra", "id": "link-2", "system": 1, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2")}, snap,
    ]})
    out.append({"name": "fleet_colour_grace", "steps": [
        bind(), fh(), {"op": "add_link", "connection": "udp:127.0.0.1:14551"},
        {"op": "bind_extra", "id": "link-2", "system": 2, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=2)},
        snap, adv(14.0), snap, adv(1.5), snap, {"op": "config"},
        {"op": "controlled", "link": "main", "system": 1}, snap,
    ]})
    out.append({"name": "fleet_colour_grace_nothing_flown", "steps": [
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 1, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2")},
        {"op": "add_link", "connection": "udp:127.0.0.1:14552"}, {"op": "bind_extra", "id": "link-3", "system": 2, "component": 1},
        {"op": "frame_extra", "id": "link-3", "spec": fh(link="link-3", system=2)},
        *[s for seconds in (0, 3.0, 3.0, 3.0, 3.0, 2.0, 0.9, 0.1, 0.1) for s in (
            adv(seconds), {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2")},
            {"op": "frame_extra", "id": "link-3", "spec": fh(link="link-3", system=2)}, snap)],
        {"op": "config"},
    ]})
    out.append({"name": "fleet_colours_palette_runs_out", "steps": [
        bind(), fh(), {"op": "controlled", "link": "main", "system": 1},
        *[s for n in range(2, 10) for s in (
            {"op": "add_link", "connection": f"udp:127.0.0.1:{14550 + n}"},
            {"op": "bind_extra", "id": f"link-{n}", "system": 1, "component": 1},
            {"op": "frame_extra", "id": f"link-{n}", "spec": fh(link=f"link-{n}")})],
        snap, {"op": "config"},
    ]})
    out.append({"name": "fleet_add_link_errors", "steps": [
        {"op": "add_link", "connection": ""}, {"op": "add_link", "connection": "   "},
        {"op": "add_link", "connection": "udp:127.0.0.1:14550"},
        {"op": "add_link", "connection": "bad:thing"}, {"op": "add_link", "connection": "fail:nothing"},
        {"op": "add_link", "connection": "  udp:127.0.0.1:14551  "},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"},
        {"op": "add_link", "connection": "serial:/dev/ttyUSB1:57600"},
        {"op": "add_link", "connection": "serial:/dev/ttyUSB1:115200"},
        *[{"op": "add_link", "connection": f"tcp:127.0.0.1:{5760 + n}"} for n in range(7)],
        {"op": "add_link", "connection": "tcp:127.0.0.1:9999"},
        {"op": "in_use", "connection": "serial:/dev/ttyUSB1:9600", "except": "link-3"},
        {"op": "in_use", "connection": "serial:/dev/ttyUSB1:9600", "except": "link-4"},
        {"op": "serial_devices"}, snap, {"op": "config"},
    ]})
    out.append({"name": "fleet_suggestion_withdrawn", "steps": [
        {"op": "store", "link": "main", "fields": {"link_suggestion": {"connection_string": "serial:/dev/ttyACM0:115200"}}},
        {"op": "add_link", "connection": "serial:/dev/ttyACM1:115200"}, snap,
        {"op": "add_link", "connection": "serial:/dev/ttyACM0:57600"}, snap,
        {"op": "store", "link": "main", "fields": {"link_suggestion": {"connection_string": "udp:1.2.3.4:5"}}},
        {"op": "add_link", "connection": "serial:/dev/ttyACM2:57600"}, snap,
    ]})
    out.append({"name": "fleet_remove_link_and_vehicle", "steps": [
        {"op": "remove_link", "id": "main"}, {"op": "remove_link", "id": "link-9"},
        {"op": "remove_vehicle", "key": "nonsense"}, {"op": "remove_vehicle", "key": "link-3:1"},
        {"op": "remove_vehicle", "key": "main:1"},
        bind(), fh(base_mode=129), {"op": "controlled", "link": "main", "system": 1},
        {"op": "remove_vehicle", "key": "main:1"}, {"op": "remove_vehicle", "key": "main:2"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 3, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=3, base_mode=129)},
        {"op": "controlled", "link": "link-2", "system": 3},
        {"op": "set_meta", "key": "link-2:3", "name": "Mule"},
        {"op": "select", "key": "link-2:3"}, snap,
        {"op": "remove_vehicle", "key": "link-2:3"}, {"op": "remove_vehicle", "key": "link-2:3", "confirm": True},
        snap, {"op": "config"},
        adv(6), {"op": "remove_vehicle", "key": "main:1"}, snap,
        fh(base_mode=129), {"op": "remove_vehicle", "key": "main:1", "confirm": True}, snap, {"op": "config"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14553"}, {"op": "remove_link", "id": "link-2"}, snap,
    ]})
    out.append({"name": "fleet_remove_main_with_fallback", "steps": [
        bind(), fh(base_mode=1), {"op": "controlled", "link": "main", "system": 1},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 2, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=2, base_mode=1)},
        {"op": "controlled", "link": "link-2", "system": 2},
        {"op": "set_meta", "key": "main:1", "name": "Alpha", "color": "#112233"},
        {"op": "remove_vehicle", "key": "main:1"}, snap, {"op": "config"},
        {"op": "select", "key": "link-2:2"}, snap,
        {"op": "remove_link", "id": "link-2"}, snap,
    ]})
    out.append({"name": "fleet_set_meta", "steps": [
        {"op": "set_meta", "key": "bad"}, {"op": "set_meta", "key": "main:0", "name": "x"},
        {"op": "set_meta", "key": "main:256", "name": "x"}, {"op": "set_meta", "key": "link-1000:1", "name": "x"},
        bind(), fh(),
        {"op": "set_meta", "key": "main:1", "name": "   Big    Red\tOne  "}, snap, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "name": "x" * 60}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "color": "red"}, {"op": "set_meta", "key": "main:1", "color": "#12345"},
        {"op": "set_meta", "key": "main:1", "color": 5}, {"op": "set_meta", "key": "main:1", "color": "#abcdef"},
        {"op": "set_meta", "key": "main:1", "color": " #ABCDEF "}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "color": ""}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "color": "#ABCDEF", "name": ""}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "name": None, "color": None}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "name": 7}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "color": None, "name": "Named"}, {"op": "config"},
        {"op": "set_meta", "key": "main:1", "color": "", "name": ""}, {"op": "config"},
    ]})
    out.append({"name": "fleet_meta_trim", "steps": [
        *[{"op": "set_meta", "key": f"main:{n}", "color": "#%06X" % n} for n in range(1, 41)],
        *[{"op": "set_meta", "key": f"link-2:{n}", "name": f"Named {n}"} for n in range(1, 31)],
        {"op": "config"},
        {"op": "set_meta", "key": "link-3:1", "name": "last"}, {"op": "config"},
        *[{"op": "set_meta", "key": f"link-3:{n}", "name": f"Over {n}"} for n in range(2, 80)],
        {"op": "config"},
    ]})
    out.append({"name": "fleet_select", "steps": [
        {"op": "select", "key": "x"}, {"op": "select", "key": "link-5:1"}, {"op": "select", "key": "main:1"},
        bind(), fh(), {"op": "select", "key": "main:1"}, {"op": "select", "key": "main:2"},
        {"op": "controlled", "link": "main", "system": 4}, {"op": "select", "key": "main:1"},
        {"op": "controlled", "link": "main", "system": 1}, {"op": "select", "key": "main:1"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 1, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2")},
        {"op": "select", "key": "link-2:1"}, {"op": "select", "key": "link-2:1"}, snap,
        {"op": "controlled", "link": "link-2", "system": 1}, {"op": "select", "key": "main:1"}, snap,
        {"op": "controlled", "link": "main", "system": None}, {"op": "select", "key": "main:1"}, snap,
        {"op": "controlled", "link": "link-2", "system": None}, {"op": "select", "key": "link-2:1"}, snap,
    ]})
    out.append({"name": "fleet_notifications", "steps": [
        bind(), fh(), {"op": "controlled", "link": "main", "system": 1}, {"op": "collect"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "bind_extra", "id": "link-2", "system": 2, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=2)}, {"op": "collect"},
        {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 4, "text": "Low battery"}, link="link-2", system=2)},
        {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 2, "text": "Failsafe"}, link="link-2", system=2)},
        ff("STATUSTEXT", {"severity": 2, "text": "Own failsafe"}), {"op": "collect"}, snap,
        {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 4, "text": "Low battery"}, link="link-2", system=2)},
        {"op": "collect"}, adv(31), {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 4, "text": "Low battery"}, link="link-2", system=2)},
        {"op": "collect"}, snap,
        adv(25), {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 4, "text": "Low battery"}, link="link-2", system=2)},
        {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 3, "text": "Level three"}, link="link-2", system=2)},
        {"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 5, "text": "Level five"}, link="link-2", system=2)},
        {"op": "collect"}, snap, adv(6), {"op": "collect"}, snap, adv(1), {"op": "collect"},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=2)}, {"op": "collect"}, snap,
        adv(6), fh(), {"op": "collect"}, adv(6), {"op": "collect"}, snap,
        *[{"op": "frame_extra", "id": "link-2", "spec": ff("STATUSTEXT", {"severity": 4, "text": f"Msg {i}"}, link="link-2", system=2)} for i in range(30)],
        {"op": "collect"}, snap,
        {"op": "remove_link", "id": "link-2"}, {"op": "add_link", "connection": "udp:127.0.0.1:14560"},
        {"op": "bind_extra", "id": "link-2", "system": 2, "component": 1},
        {"op": "frame_extra", "id": "link-2", "spec": fh(link="link-2", system=2)}, {"op": "collect"}, snap,
        {"op": "controlled", "link": "main", "system": None}, {"op": "select", "key": "link-2:2"}, {"op": "collect"},
    ]})
    out.append({"name": "fleet_publish", "steps": [
        bind(), fh(), {"op": "publish", "force": False}, {"op": "listener", "name": "a"},
        {"op": "publish", "force": False}, {"op": "publish", "force": False},
        fgp(473977000, 85455000), {"op": "publish", "force": False}, {"op": "publish", "force": True},
        {"op": "listener", "name": "b"}, {"op": "publish", "force": True},
        {"op": "set_meta", "key": "main:1", "name": "Hawk"}, {"op": "publish", "force": False},
        {"op": "remove_listener", "name": "a"}, fgp(473977100, 85455000), {"op": "publish", "force": False},
        {"op": "remove_listener", "name": "b"}, fgp(473977200, 85455000), {"op": "publish", "force": False},
    ]})
    out.append({"name": "fleet_load_and_config", "steps": [
        {"op": "config"},
        {"op": "load", "block": {"links": [
            {"id": "link-2", "connection": " udp:127.0.0.1:1 "}, {"id": "link-2", "connection": "udp:x:2"},
            {"id": "link-3", "connection": "udp:127.0.0.1:1"}, {"id": "bad", "connection": "udp:y"},
            {"id": "link-4", "connection": ""}, {"id": "link-5"}, "junk", {"id": 4, "connection": "a"},
            {"id": "link-6", "connection": "tcp:z:1"}],
            "vehicles": {"main:1": {"name": " A  B ", "color": "#abcdef"}, "main:300": {"name": "x"},
                         "link-2:2": {"name": ""}, "link-2:3": {"color": "nope"}, "link-2:4": "junk",
                         "main:2": {"name": "ok", "extra": 1}},
            "theme_vehicle": "main:1", "shared_links": True}},
        {"op": "config"}, {"op": "load", "block": "nothing"}, {"op": "config"}, {"op": "load", "block": {"links": "ab", "vehicles": []}},
        {"op": "config"},
        {"op": "load", "block": {"theme_vehicle": "main:9"}}, {"op": "config"},
        {"op": "load", "block": {"theme_vehicle": "nope"}}, {"op": "config"},
        {"op": "load", "block": {"links": [{"id": "link-2", "connection": "udp:127.0.0.1:14570"}],
                                 "vehicles": {"link-2:1": {"name": "Remembered"}}}},
        {"op": "start"}, snap, {"op": "stop"}, snap,
    ]})
    raws = [
        None, 5, "x", [], {}, {"links": []},
        {"links": [{"id": f"link-{n}", "connection": f"udp:h:{n}"} for n in range(2, 14)]},
        {"links": [{"id": "link-2\n", "connection": "udp:h:1"}], "theme_vehicle": "main:1\n"},
        {"vehicles": {f"main:{n}": {"name": f"n{n}"} for n in range(1, 70)}},
        {"vehicles": {"main:1": {"name": "a"}}, "theme_vehicle": "link-12:255"},
        {"theme_vehicle": "link-1234:1"}, {"theme_vehicle": 5}, {"theme_vehicle": "main:01"},
    ]
    cleans = [
        ("name", [None, 5, "", "  a   b  ", "x" * 41, "\u00e4" * 50, "a\u00a0b", "a\x1cb"]),
        ("color", [None, 5, "", "#abc", "#abcdefg", " #aBcDeF ", "#GGGGGG", "abcdef", "#abcdef\n"]),
        ("key", [None, 5, "main:1", "main:255", "main:256", "main:0", "main:001", "link-2:3", "link-2:3\n",
                 "link-:3", "link-1234:1", "Main:1", "main:", ":1", "main:1:2"]),
    ]
    out.append({"name": "coerce_and_clean", "steps": [{"op": "coerce", "raw": raw} for raw in raws]
                + [{"op": "clean", "fn": fn, "value": value} for fn, values in cleans for value in values]})
    out.append({"name": "fleet_rtcm_fanout", "steps": [
        {"op": "inject", "frame": "d3000a"}, {"op": "connected"},
        {"op": "add_link", "connection": "udp:127.0.0.1:14551"}, {"op": "add_link", "connection": "udp:127.0.0.1:14552"},
        {"op": "link_connected", "link": "link-3", "value": False}, {"op": "inject", "frame": "d30001"},
        {"op": "link_accept", "link": "main", "value": False}, {"op": "inject", "frame": "d30002"},
        {"op": "link_connected", "link": "main", "value": False}, {"op": "link_connected", "link": "link-3", "value": True},
        {"op": "connected"}, {"op": "link_connected", "link": "link-3", "value": False},
        {"op": "link_connected", "link": "link-2", "value": False}, {"op": "connected"},
    ]})
    return out


def fleet_cases() -> list[dict]:
    from corvus_link import dialect_v2  # noqa: F401  (the message classes)

    import logging

    logging.getLogger("corvus.fleet").disabled = True
    ref = fleet_ref()
    Bridge = fleet_bridge_class(ref)
    out = []
    for sc in fleet_scenarios():
        clock = Clock()
        clock.t = 1000.0
        log: list = []
        stores: dict = {}
        bridges: dict = {}

        def bridge_factory(store, connection):
            if connection.startswith("fail:"):
                raise RuntimeError("no such device")
            link = f"pending-{len(bridges)}"
            bridge = Bridge(link, connection, log)
            return bridge

        def store_factory():
            return FleetStoreStub(log, "pending")

        main_store = FleetStoreStub(log, "main")
        main_bridge = Bridge("main", "udp:127.0.0.1:14550", log)
        stores["main"], bridges["main"] = main_store, main_bridge

        def persist(config):
            log.append(["persist", config])

        def named_factory(store, connection, bridge_factory=bridge_factory):
            bridge = bridge_factory(store, connection)
            return bridge

        results = []
        with mock.patch("time.monotonic", lambda clock=clock: clock.t):
            fleet = ref.Fleet(main_bridge, main_store, bridge_factory=named_factory,
                              store_factory=store_factory, persist=persist)
            listeners: dict = {}

            def link_of(name):
                if name in bridges:
                    return bridges[name]
                return fleet.link(name).bridge

            def register(link):
                real = fleet.link(link)
                if real is not None:
                    bridges[link] = real.bridge
                    real.bridge.link = link
                    real.store.link = link

            def tracker_frame(spec, link_name):
                bridge = link_of(link_name)
                fields = {k: (v.encode("utf-8") if isinstance(v, str) else v) for k, v in spec["fields"].items()}
                msg = receive_message(spec["name"], fields, spec.get("system", 1), spec.get("component", 1))
                bridge.fleet_tracker.observe(msg, vehicle_heartbeat=spec.get("vh", False))

            for step in sc["steps"]:
                before = len(log)
                op = step["op"]
                result = None
                if op == "advance":
                    clock.t += step["seconds"]
                elif op == "bind":
                    link_of(step["link"]).fleet_tracker.bind(step["system"], step["component"])
                elif op == "bind_extra":
                    register(step["id"])
                    link_of(step["id"]).fleet_tracker.bind(step["system"], step["component"])
                elif op == "frame":
                    tracker_frame(step, step["link"])
                elif op == "frame_extra":
                    register(step["id"])
                    tracker_frame(step["spec"], step["id"])
                elif op == "forget":
                    link_of(step["link"]).fleet_tracker.forget()
                elif op == "systems":
                    result = link_of(step["link"]).fleet_tracker.systems()
                elif op == "events":
                    last, fresh = link_of(step["link"]).fleet_tracker.events_since(step["since"])
                    result = {"last": last, "fresh": [list(e) for e in fresh]}
                elif op == "controlled":
                    link_of(step["link"]).controlled = step["system"]
                elif op == "store":
                    fleet.link(step["link"]).store.snap.update(step["fields"])
                elif op == "add_link":
                    result = list(fleet.add_link(step["connection"]))
                    for lid in ("link-%d" % n for n in range(2, 12)):
                        register(lid)
                elif op == "remove_link":
                    result = fleet.remove_link(step["id"])
                elif op == "remove_vehicle":
                    result = list(fleet.remove_vehicle(step["key"], step.get("confirm", False)))
                elif op == "select":
                    result = fleet.select(step["key"])
                elif op == "set_meta":
                    kwargs = {k: step[k] for k in ("name", "color") if k in step}
                    result = fleet.set_meta(step["key"], **kwargs)
                elif op == "snapshot":
                    result = fleet.snapshot()
                elif op == "config":
                    result = fleet.to_config()
                elif op == "load":
                    fleet.load(step["block"])
                elif op == "collect":
                    fleet.collect_events()
                elif op == "publish":
                    fleet._publish(step["force"])
                elif op == "listener":
                    name = step["name"]
                    listeners[name] = lambda snapshot, name=name: log.append(["listener", name, snapshot])
                    fleet.add_listener(listeners[name])
                elif op == "remove_listener":
                    fleet.remove_listener(listeners[step["name"]])
                elif op == "in_use":
                    result = fleet.connection_in_use(step["connection"], step["except"])
                elif op == "serial_devices":
                    result = fleet.extra_serial_devices()
                elif op == "start":
                    fleet.start()
                    for lid in ("link-%d" % n for n in range(2, 12)):
                        register(lid)
                    fleet.stop()
                elif op == "stop":
                    pass
                elif op == "coerce":
                    result = ref.coerce_config(step["raw"])
                elif op == "clean":
                    fn = {"name": ref.clean_name, "color": ref.clean_color, "key": ref.parse_vehicle_key}[step["fn"]]
                    result = fn(step["value"])
                elif op == "inject":
                    rtcm = ref.RtcmFanout(main_bridge, fleet)
                    result = rtcm.inject_rtcm(bytes.fromhex(step["frame"]))
                elif op == "connected":
                    result = ref.RtcmFanout(main_bridge, fleet).is_connected()
                elif op == "link_connected":
                    link_of(step["link"]).connected = step["value"]
                elif op == "link_accept":
                    link_of(step["link"]).accept = step["value"]
                else:
                    raise SystemExit(op)
                results.append({"result": result, "log": log[before:], "active": fleet._active_link,
                                "t": round(clock.t - 1000.0, 6)})
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


# ------------------------------------------------------------ forwarder ----

def fwd_ref():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import mavlink_forwarder_python as ref
    return ref


def fwd_frame(version=2, system=1, length=3, signed=False, seq=0) -> str:
    """A framed MAVLink message by its header alone; the forwarder checks only framing."""
    payload = bytes(range(1, length + 1))
    if version == 1:
        raw = bytes([0xFE, length, seq, system, 1, 0]) + payload + b"\x00\x00"
    else:
        raw = bytes([0xFD, length, 1 if signed else 0, 0, seq, system, 1, 0, 0, 0]) + payload + b"\x00\x00"
        if signed:
            raw += bytes(13)
    return raw.hex()


def fwd_scenarios() -> list[dict]:
    wait = lambda key, n: {"op": "wait", "key": key, "at_least": n}
    status = {"op": "status"}
    base = {"host": "127.0.0.1", "port": "$A", "listen_host": "127.0.0.1", "listen_port": 0}
    f1, f2, f3 = fwd_frame(seq=1), fwd_frame(seq=2, length=9), fwd_frame(version=1, seq=3, length=5)
    out = []
    out.append({"name": "mirror_basic", "peers": ["A"], "config": base, "steps": [
        {"op": "start"}, status, {"op": "feed", "raw": f1}, {"op": "feed", "raw": f2}, {"op": "feed", "raw": f3},
        wait("frames_sent", 3), {"op": "recv", "peer": "A"}, status,
    ]})
    out.append({"name": "feed_edge_cases", "peers": ["A"], "config": base, "steps": [
        {"op": "feed", "raw": f1}, status, {"op": "start"}, {"op": "feed", "raw": ""}, {"op": "feed", "raw": f1},
        wait("frames_sent", 1), {"op": "recv", "peer": "A"}, {"op": "start"}, status,
        {"op": "stop"}, {"op": "feed", "raw": f1}, {"op": "stop"}, status, {"op": "recv", "peer": "A"},
        {"op": "start"}, {"op": "feed", "raw": f2}, wait("frames_sent", 2), {"op": "recv", "peer": "A"}, status,
    ]})
    out.append({"name": "dialled_in_peer_is_mirrored_to", "peers": ["A", "B", "C"], "config": base, "steps": [
        {"op": "start"}, {"op": "send", "peer": "C", "data": "00ff12345678"}, wait("datagrams_received", 1), status,
        {"op": "send", "peer": "B", "data": f1}, wait("datagrams_received", 2), status,
        {"op": "feed", "raw": f2}, wait("frames_sent", 2), {"op": "recv", "peer": "A"}, {"op": "recv", "peer": "B"},
        {"op": "recv", "peer": "C"}, {"op": "injected"}, status,
    ]})
    out.append({"name": "commands_from_configured_endpoints_only", "peers": ["A", "B"],
                "config": {**base, "allow_commands": True}, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": f1}, wait("frames_injected", 1),
        {"op": "send", "peer": "B", "data": f2}, wait("commands_refused", 1),
        {"op": "send", "peer": "B", "data": f3}, wait("commands_refused", 2),
        {"op": "send", "peer": "A", "data": f2 + f3}, wait("frames_injected", 3), {"op": "injected"}, status,
    ]})
    out.append({"name": "commands_off_by_default", "peers": ["A"], "config": base, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": f1}, wait("datagrams_received", 1), {"op": "injected"}, status,
    ]})
    out.append({"name": "inject_refused_is_not_counted", "peers": ["A"],
                "config": {**base, "allow_commands": True}, "inject_result": False, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": f1}, wait("datagrams_received", 1), {"op": "settle"},
        {"op": "injected"}, status,
    ]})
    out.append({"name": "our_own_system_id", "peers": ["A", "B"], "config": {**base, "allow_commands": True}, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": fwd_frame(system=254)}, wait("datagrams_received", 1),
        {"op": "settle"}, status, {"op": "send", "peer": "A", "data": fwd_frame(system=254, version=1)},
        wait("datagrams_received", 2), {"op": "send", "peer": "A", "data": fwd_frame(system=255)},
        wait("frames_injected", 1), {"op": "injected"}, status,
        {"op": "send", "peer": "B", "data": fwd_frame(system=254)}, wait("datagrams_received", 4), {"op": "settle"}, status,
    ]})
    out.append({"name": "frames_split_and_resynchronised", "peers": ["A"], "config": {**base, "allow_commands": True}, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": f2[:10]}, wait("datagrams_received", 1), {"op": "settle"},
        {"op": "injected"}, status, {"op": "send", "peer": "A", "data": f2[10:]}, wait("frames_injected", 1),
        {"op": "send", "peer": "A", "data": "0102" + f1 + "aa" + f3 + "fd"}, wait("frames_injected", 3),
        {"op": "send", "peer": "A", "data": "03" + fwd_frame(length=255, seq=9)}, wait("datagrams_received", 4),
        {"op": "settle"}, {"op": "injected"}, status,
    ]})
    out.append({"name": "signed_and_v1_frames", "peers": ["A"], "config": {**base, "allow_commands": True}, "steps": [
        {"op": "start"}, {"op": "send", "peer": "A", "data": fwd_frame(signed=True, length=4)},
        wait("frames_injected", 1), {"op": "send", "peer": "A", "data": fwd_frame(version=1, length=0)},
        wait("frames_injected", 2), {"op": "send", "peer": "A", "data": fwd_frame(signed=True, length=250)},
        wait("datagrams_received", 3), {"op": "settle"}, {"op": "injected"}, status,
    ]})
    out.append({"name": "listen_port_busy", "peers": ["A"], "busy": "BUSY",
                "config": {**base, "listen_port": "$BUSY"}, "steps": [
        {"op": "start"}, status, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "A"},
        {"op": "stop"}, {"op": "start"}, status,
    ]})
    out.append({"name": "listen_port_chosen", "peers": ["A"], "free": "F",
                "config": {**base, "listen_port": "$F"}, "steps": [{"op": "start"}, status]})
    out.append({"name": "endpoint_list_is_cleaned", "peers": ["A", "B"], "free": "F", "config": {
        **base, "listen_port": "$F", "endpoints": [
            "127.0.0.1:{A}", "localhost:{A}", "127.0.0.1:{B}", "127.0.0.1:{F}", "x:99999", ":123", "host:abc", "",
            "  127.0.0.1:{B}  ", "127.0.0.1:0", "127.0.0.1:+{B}", "127.0.0.1: {B}"]},
        "steps": [{"op": "start"}, status, {"op": "feed", "raw": f1}, wait("frames_sent", 2), {"op": "recv", "peer": "A"},
                  {"op": "recv", "peer": "B"}, status]})
    out.append({"name": "peer_expires", "peers": ["A", "B"], "config": base, "steps": [
        {"op": "start"}, {"op": "send", "peer": "B", "data": f1}, wait("datagrams_received", 1), status,
        {"op": "skip", "seconds": 29.0}, status, {"op": "skip", "seconds": 2.0}, status,
        {"op": "feed", "raw": f2}, wait("frames_sent", 1), {"op": "recv", "peer": "A"}, {"op": "recv", "peer": "B"},
        {"op": "send", "peer": "B", "data": f3}, wait("datagrams_received", 2), status,
    ]})
    out.append({"name": "vehicle_link_is_not_mirrored_to", "peers": ["A", "V"], "config": {
        **base, "port": "$V", "endpoints": ["127.0.0.1:{A}"]}, "link": ["127.0.0.1", "$V"], "steps": [
        {"op": "start"}, status, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "A"},
        {"op": "recv", "peer": "V"}, status, {"op": "send", "peer": "V", "data": f2}, {"op": "settle"}, status,
    ]})
    out.append({"name": "vehicle_link_wildcard_host", "peers": ["A", "V"], "config": {
        **base, "port": "$V", "endpoints": ["127.0.0.1:{A}"]}, "link": ["0.0.0.0", "$V"], "steps": [
        {"op": "start"}, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "V"}, status,
        {"op": "send", "peer": "V", "data": f2}, {"op": "settle"}, status,
    ]})
    out.append({"name": "vehicle_link_elsewhere", "peers": ["A", "V"], "config": base, "link": ["10.1.2.3", "$V"], "steps": [
        {"op": "start"}, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "A"}, status,
    ]})
    out.append({"name": "link_address_unavailable", "peers": ["A"], "config": base, "link": None, "steps": [
        {"op": "start"}, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "A"}, status,
    ]})
    out.append({"name": "name_that_does_not_resolve", "peers": ["A"], "config": {
        **base, "endpoints": ["no-such-host.invalid:14550", "localhost:{A}"]}, "steps": [
        {"op": "start"}, status, {"op": "feed", "raw": f1}, wait("frames_sent", 1), {"op": "recv", "peer": "A"},
        {"op": "skip", "seconds": 31.0}, {"op": "feed", "raw": f2}, wait("frames_sent", 2), {"op": "recv", "peer": "A"}, status,
    ]})
    out.append({"name": "restart_keeps_counters", "peers": ["A", "B"], "config": base, "steps": [
        {"op": "start"}, {"op": "send", "peer": "B", "data": f1}, wait("datagrams_received", 1),
        {"op": "feed", "raw": f2}, wait("frames_sent", 2), {"op": "stop"}, status, {"op": "start"}, status,
        {"op": "feed", "raw": f3}, wait("frames_sent", 3), {"op": "recv", "peer": "A"}, {"op": "recv", "peer": "B"}, status,
    ]})
    out.append({"name": "endpoint_parsing", "peers": ["A"], "config": base, "steps": [
        {"op": "parse", "text": t, "default": d} for t, d in [
            ("host", 14550), ("host:1", 14550), ("host:65535", 14550), ("host:65536", 14550), ("host:0", 14550),
            ("host:-1", 14550), (":1", 14550), ("a:b:5", 14550), ("[::1]:14550", 14550), ("  h : 12 ", 14550),
            ("h:1_2", 14550), ("h:_12", 14550), ("h:12_", 14550), ("h:1__2", 14550), ("h:+7", 14550),
            ("h:", 99), ("", 14550), ("   ", 14550), ("::1", 14550), ("h: 7", 14550),
        ]] + [{"op": "split", "data": d} for d in [
            "", "fd", "fdff", f1, f1 + f2, f2[:6], "00" + f1 + "00", f3 + f1[:4], "fe", "fe00", "fe0000000000" "0000",
            fwd_frame(signed=True), fwd_frame(length=255), fwd_frame(length=252) + f1]] + [
        {"op": "source", "data": d} for d in ["", f1, f3, fwd_frame(system=254), "fe0102", "fd01020304050607", "fd0102030405"]]})
    return out


def fwd_cases() -> list[dict]:
    import logging
    import re
    import socket
    import time as real_time

    ref = fwd_ref()
    cases = []
    for sc in fwd_scenarios():
        offset = [0.0]
        names: dict = {}
        socks: dict = {}

        def tokens(value):
            if isinstance(value, str):
                if value.startswith("$") and value[1:] in names:
                    return names[value[1:]]
                return re.sub(r"\{(\w+)\}", lambda m: str(names[m.group(1)]), value)
            if isinstance(value, list):
                return [tokens(v) for v in value]
            if isinstance(value, dict):
                return {k: tokens(v) for k, v in value.items()}
            return value

        def normalise(value):
            by_port = {port: name for name, port in names.items()}
            if isinstance(value, str):
                value = re.sub(r":(\d+)\b", lambda m: ":" + by_port.get(int(m.group(1)), m.group(1)), value)
                return re.sub(r"\bport (\d+)\b", lambda m: "port " + by_port.get(int(m.group(1)), m.group(1)), value)
            if isinstance(value, list):
                return [normalise(v) for v in value]
            if isinstance(value, dict):
                return {k: (by_port[v] if k in ("port", "listen_port", "listen_port_requested") and v in by_port and v != 0
                            else normalise(v)) for k, v in value.items()}
            return value

        for name in sc["peers"]:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.bind(("127.0.0.1", 0))
            sock.settimeout(0.15)
            socks[name], names[name] = sock, sock.getsockname()[1]
        if sc.get("busy"):
            busy = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            busy.bind(("127.0.0.1", 0))
            socks["BUSY"], names["BUSY"] = busy, busy.getsockname()[1]
        if sc.get("free"):
            probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            probe.bind(("127.0.0.1", 0))
            names[sc["free"]] = probe.getsockname()[1]
            probe.close()
        injected: list = []
        lines: list = []

        class Capture(logging.Handler):
            def emit(self, record):
                lines.append([record.levelname.lower(), record.getMessage()])

        logger = logging.getLogger("corvus.forward")
        handler = Capture()
        old_level = logger.level
        logger.addHandler(handler)
        logger.setLevel(logging.INFO)
        real_monotonic = real_time.monotonic
        results = []
        fwd = None
        try:
            with mock.patch("time.monotonic", lambda: real_monotonic() + offset[0]):
                cfg = tokens(sc["config"])
                link = tokens(sc["link"]) if "link" in sc and sc["link"] is not None else None
                link_address = (lambda: tuple(link)) if link else None
                if "link" in sc and sc["link"] is None:
                    def link_address():
                        raise OSError("no address yet")
                result_of = sc.get("inject_result", True)

                def inject(frame, injected=injected):
                    injected.append(bytes(frame).hex())
                    return result_of

                fwd = ref.MavlinkForwarder(
                    inject, host=cfg["host"], port=cfg["port"], listen_host=cfg["listen_host"],
                    listen_port=cfg["listen_port"], endpoints=cfg.get("endpoints", ()),
                    allow_commands=cfg.get("allow_commands", False), link_address=link_address)
                for step in sc["steps"]:
                    before = len(lines)
                    op = step["op"]
                    result = None
                    if op == "start":
                        result = fwd.start()
                        if fwd._bound:
                            names["L"] = fwd.listen_address()[1]
                    elif op == "stop":
                        fwd.stop()
                    elif op == "feed":
                        fwd.feed(bytes.fromhex(step["raw"]))
                    elif op == "status":
                        result = fwd.status()
                    elif op == "send":
                        socks[step["peer"]].sendto(bytes.fromhex(step["data"]), ("127.0.0.1", fwd.listen_address()[1]))
                    elif op == "recv":
                        got = []
                        while True:
                            try:
                                data, _ = socks[step["peer"]].recvfrom(65535)
                            except TimeoutError:
                                break
                            got.append(data.hex())
                        result = got
                    elif op == "wait":
                        deadline = real_monotonic() + 3.0
                        ok = False
                        while real_monotonic() < deadline:
                            if fwd.status()[step["key"]] >= step["at_least"]:
                                ok = True
                                break
                            real_time.sleep(0.01)
                        real_time.sleep(0.08)
                        result = ok
                    elif op == "settle":
                        real_time.sleep(0.3)
                    elif op == "injected":
                        result = list(injected)
                    elif op == "skip":
                        offset[0] += step["seconds"]
                    elif op == "parse":
                        result = ref.parse_endpoint(step["text"], step["default"])
                    elif op == "split":
                        frames, rest = ref.split_frames(bytes.fromhex(step["data"]))
                        result = {"frames": [f.hex() for f in frames], "rest": rest.hex()}
                    elif op == "source":
                        result = ref.frame_source_system(bytes.fromhex(step["data"]))
                    else:
                        raise SystemExit(op)
                    real_time.sleep(0.02)
                    new_lines = [[lvl, re.sub(r"( does not resolve) \(.*\)$", r"\1", text)] for lvl, text in lines[before:]]
                    results.append({"result": normalise(result), "log": normalise(new_lines)})
        finally:
            if fwd is not None:
                fwd.stop()
            logger.removeHandler(handler)
            logger.setLevel(old_level)
            for sock in socks.values():
                sock.close()
        cases.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return cases


# ---------------------------------------------------------- autoconnect ----

def ac_ref():
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
    from tests.reference import autoconnect_python as ref
    return ref


AC_ROWS = {
    "cable": [{"device": "/dev/ttyACM0", "description": "PX4 FMU v5.x", "hwid": "USB VID:PID=26AC:0032 SER=0001"}],
    "cable_moved": [{"device": "/dev/ttyACM1", "description": "PX4 FMU v5.x", "hwid": "USB VID:PID=26AC:0032 SER=0001"}],
    "two_cables": [{"device": "/dev/ttyACM1", "description": "", "hwid": "USB VID:PID=26AC:0032 SER=0002"},
                   {"device": "/dev/ttyACM0", "description": "", "hwid": "USB VID:PID=26AC:0032 SER=0001"}],
    "same_model_two": [{"device": "/dev/ttyACM1", "description": "", "hwid": "USB VID:PID=26AC:0032"},
                       {"device": "/dev/ttyACM2", "description": "", "hwid": "USB VID:PID=26AC:0032"}],
    "radio": [{"device": "/dev/ttyUSB0", "description": "SiK", "hwid": "USB VID:PID=0403:6015 SER=R1"}],
    "radio_moved": [{"device": "/dev/ttyUSB1", "description": "SiK", "hwid": "USB VID:PID=0403:6015 SER=R1"}],
    "cable_and_radio": [{"device": "/dev/ttyUSB0", "description": "", "hwid": ""},
                        {"device": "/dev/ttyACM0", "description": "", "hwid": ""}],
    "bootloader": [{"device": "/dev/ttyACM0", "description": "PX4 BL FMU v5.x", "hwid": "USB VID:PID=26AC:0032 SER=0001"}],
    "bootloader_and_cable": [{"device": "/dev/ttyACM0", "description": "PX4 BL FMU v5.x", "hwid": "USB VID:PID=26AC:0011"},
                             {"device": "/dev/ttyACM1", "description": "PX4 FMU v5.x", "hwid": "USB VID:PID=26AC:0032"}],
    "rtk": [{"device": "/dev/ttyACM0", "description": "u-blox GNSS receiver", "hwid": "USB VID:PID=1546:01A9"}],
    "phantoms": [{"device": "/dev/ttyS0", "description": "", "hwid": ""}, {"device": "/dev/cu.Bluetooth-Incoming-Port", "description": "", "hwid": ""},
                 {"device": "", "description": "", "hwid": ""}, {"device": "/dev/pts/4", "description": "", "hwid": ""}],
    "mac_cable": [{"device": "/dev/cu.usbmodem14201", "description": "PX4", "hwid": "USB VID:PID=26AC:0032 SER=0001"}],
    "mac_no_descriptor": [{"device": "/dev/cu.usbmodem14201", "description": "", "hwid": ""}],
    "none": [],
}


def ac_pure_cases() -> dict:
    ref = ac_ref()
    from corvus.mavlink_bridge import MavlinkBridge

    dummy = MavlinkBridge.__new__(MavlinkBridge)
    validator = dummy.validate_connection
    out: dict = {"sets": AC_ROWS, "classify": [], "pick": [], "resolve": [], "suggest": []}
    for name, rows in AC_ROWS.items():
        ports = ref.classify_ports(rows)
        out["classify"].append([name, [dict(device=p.device, description=p.description, hwid=p.hwid, kind=p.kind) for p in ports]])
        for usb in (True, False):
            for sik in (True, False):
                pick = ref.pick_usb_serial(ports, usb_enabled=usb, sik_enabled=sik)
                out["pick"].append([name, usb, sik, list(pick) if pick else None])
    clis = [None, "", "  ", "udp:127.0.0.1:14550", " tcp:1.2.3.4:5760 ", "bogus", "serial:/dev/x:0"]
    configs = [None, "", "udp:0.0.0.0:14551", "serial:/dev/ttyACM0:921600", "serial:/dev/ttyUSB0:115200", "nonsense", "serial:/dev/ttyACM9:9600"]
    for name in ("cable", "two_cables", "radio", "cable_and_radio", "bootloader", "bootloader_and_cable", "rtk", "none", "phantoms"):
        ports = ref.classify_ports(AC_ROWS[name])
        for cli in clis[:4] + ["bogus"]:
            for configured in configs:
                for flags in ((True, True, True), (False, True, True), (True, False, False)):
                    d = ref.resolve_startup_connection(
                        cli, configured, ports, usb_enabled=flags[0], sik_enabled=flags[1],
                        udp_fallback_enabled=flags[2], validator=validator)
                    out["resolve"].append([name, cli, configured, list(flags),
                                           [d.connection_string, d.reason, d.detail]])
    d = ref.resolve_startup_connection(None, "bogus", [], udp_fallback_enabled=False, validator=validator)
    out["resolve"].append(["none", None, "bogus", [True, True, False], [d.connection_string, d.reason, d.detail]])
    d = ref.resolve_startup_connection(None, None, [], validator=None)
    out["resolve"].append(["none", None, None, [True, True, True], [d.connection_string, d.reason, d.detail]])
    snaps = [{}, {"link_connection": "serial:/dev/ttyACM0:115200"}, {"link_connection": "udp:1.2.3.4:5"},
             {"link_connection": None}, {"link_connection": "serial:/dev/ttyUSB0:9600"}]
    for name in ("cable", "radio", "cable_and_radio", "none", "bootloader"):
        ports = ref.classify_ports(AC_ROWS[name])
        for snap in snaps:
            for enabled, manual, usb, sik in ((True, False, True, True), (False, False, True, True), (True, True, True, True),
                                              (True, False, False, True), (True, False, True, False)):
                session = ref.SessionState()
                session.enabled, session.usb, session.sik = enabled, usb, sik
                if manual:
                    session.note_manual_connect()
                sug = ref.suggest_connection(snap, ports, session)
                out["suggest"].append([name, snap, [enabled, manual, usb, sik],
                                       None if sug is None else [sug.connection_string, sug.kind, sug.device, sug.reason]])
    return out


def ac_watcher_scenarios() -> list[dict]:
    t = {"op": "tick"}
    ports = lambda name: {"op": "ports", "rows": AC_ROWS[name]}
    snap = lambda **kw: {"op": "snapshot", "fields": kw}
    return [
        {"name": "dial_a_cable_when_idle", "conn": "udp:0.0.0.0:14550", "steps": [
            ports("none"), snap(link_status="disconnected"), {"op": "stale", "value": True}, t,
            ports("cable"), t, t, ports("none"), t, ports("cable"), t]},
        {"name": "not_running_and_disabled", "conn": "udp:0.0.0.0:14550", "steps": [
            {"op": "running", "value": False}, ports("cable"), t, {"op": "manual"}, t, {"op": "running", "value": True}, t,
            {"op": "config", "block": {"enabled": False}}, t, {"op": "config", "block": None}, t]},
        {"name": "suggest_while_connected", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="connected", link_connection="udp:0.0.0.0:14550"), {"op": "stale", "value": False},
            ports("none"), t, ports("cable"), t, t, ports("cable_and_radio"), t, ports("none"), t, ports("cable"), t,
            {"op": "manual"}, t]},
        {"name": "reconnecting_but_fresh", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="reconnecting"), {"op": "stale", "value": False}, ports("cable"), t,
            {"op": "stale", "value": True}, t]},
        {"name": "bootloader_is_published_and_skipped", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("bootloader"), t, t,
            ports("bootloader_and_cable"), t, ports("none"), t]},
        {"name": "rtk_and_phantoms_are_never_dialled", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("rtk"), t, ports("phantoms"), t]},
        {"name": "dial_keeps_the_known_baud", "conn": "serial:/dev/ttyACM0:921600", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("cable"), t, t]},
        {"name": "follow_the_device_to_another_name", "conn": "serial:/dev/ttyACM0:115200", "steps": [
            snap(link_status="connected", link_connection="serial:/dev/ttyACM0:115200"), {"op": "stale", "value": False},
            ports("cable"), t, ports("cable_moved"), t, snap(link_status="reconnecting"), {"op": "stale", "value": True}, t, t,
            ports("cable_moved"), t]},
        {"name": "follow_with_manual_override", "conn": "serial:/dev/ttyUSB0:57600", "steps": [
            snap(link_status="connected"), {"op": "stale", "value": False}, {"op": "manual"}, ports("radio"), t,
            snap(link_status="disconnected"), ports("radio_moved"), t, t]},
        {"name": "no_follow_when_ambiguous", "conn": "serial:/dev/ttyACM0:57600", "steps": [
            snap(link_status="connected"), {"op": "stale", "value": False},
            {"op": "ports", "rows": [{"device": "/dev/ttyACM0", "description": "", "hwid": "USB VID:PID=26AC:0032"}]}, t,
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("same_model_two"), t]},
        {"name": "device_gone_while_still_connected", "conn": "serial:/dev/ttyACM0:57600", "steps": [
            snap(link_status="connected"), {"op": "stale", "value": False}, ports("cable"), t, ports("none"), t]},
        {"name": "ports_held_by_another_link_are_left_alone", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("cable"), {"op": "busy", "devices": ["/dev/ttyACM0"]},
            t, {"op": "busy", "devices": []}, t]},
        {"name": "refused_connection_is_latched", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True},
            {"op": "ports", "rows": [{"device": "/dev/ttyACM0 x", "description": "", "hwid": ""}]}, t,
            {"op": "ports", "rows": [{"device": "/dev/ttyACM7", "description": "", "hwid": ""}]}, {"op": "switch_fails", "value": True}, t, t]},
        {"name": "radio_when_no_cable_and_the_toggles", "conn": "udp:0.0.0.0:14550", "steps": [
            snap(link_status="disconnected"), {"op": "stale", "value": True}, ports("radio"),
            {"op": "config", "block": {"sik": False}}, t, {"op": "config", "block": {"usb": False, "sik": True}}, t,
            ports("cable_and_radio"), t]},
    ]


def ac_watcher_cases() -> list[dict]:
    import logging

    ref = ac_ref()
    from corvus.mavlink_bridge import MavlinkBridge

    dummy = MavlinkBridge.__new__(MavlinkBridge)
    out = []
    for sc in ac_watcher_scenarios():
        log: list = []
        state = {"rows": [], "snapshot": {}, "stale": True, "running": True, "conn": sc["conn"], "busy": [], "fails": False}

        class Bridge:
            def is_running(self):
                return state["running"]

            def connection_string(self):
                return state["conn"]

            def validate_connection(self, conn):
                dummy.validate_connection(conn)

            def switch_connection(self, conn):
                log.append(["switch", conn])
                if state["fails"]:
                    raise OSError("busy")
                state["conn"] = conn

        class Store:
            def get_snapshot(self):
                return dict(state["snapshot"])

            def is_stale(self, timeout):
                return state["stale"]

            def update(self, **fields):
                log.append(["store", fields])
                state["snapshot"].update(fields)

        session = ref.SessionState()
        watcher = ref.AutoConnectWatcher(
            bridge=Bridge(), store=Store(), session=session, list_ports_fn=lambda: state["rows"],
            busy_devices_fn=lambda: state["busy"])
        results = []
        logging.disable(logging.CRITICAL)
        try:
            with mock.patch("time.time", lambda: 1234.5):
                for step in sc["steps"]:
                    before = len(log)
                    op = step["op"]
                    result = None
                    if op == "tick":
                        result = watcher.tick()
                    elif op == "ports":
                        state["rows"] = step["rows"]
                    elif op == "snapshot":
                        state["snapshot"].update(step["fields"])
                    elif op == "stale":
                        state["stale"] = step["value"]
                    elif op == "running":
                        state["running"] = step["value"]
                    elif op == "busy":
                        state["busy"] = step["devices"]
                    elif op == "manual":
                        session.note_manual_connect()
                    elif op == "config":
                        session.apply_config(step["block"])
                    elif op == "switch_fails":
                        state["fails"] = step["value"]
                    else:
                        raise SystemExit(op)
                    results.append({"result": result, "log": log[before:], "conn": state["conn"], "session": session.as_dict(),
                                    "last_dial": session.last_dial_key, "last_suggestion": session.last_suggestion_key})
        finally:
            logging.disable(logging.NOTSET)
        out.append({"scenario": clean(sc), "expected": clean({"steps": results, "final": {}})})
    return out


def autoconnect_cases() -> list[dict]:
    return [{"pure": ac_pure_cases()}] + ac_watcher_cases()


COMMANDS = {"params_cases.json": cases, "logs_cases.json": logs_cases, "shell_cases.json": shell_cases,
            "ftp_cases.json": ftp_cases, "flight_cases.json": flight_cases,
            "setup_cases.json": setup_cases,
            "telemetry_cases.json": telemetry_cases,
            "missions_cases.json": mission_cases,
            "remote_id_cases.json": rid_cases,
            "receive_cases.json": receive_cases,
            "status_cases.json": status_cases,
            "requests_cases.json": requests_cases,
            "tlog_cases.json": tlog_cases,
            "rtcm_cases.json": rtcm_cases,
            "fleet_cases.json": fleet_cases,
            "forwarder_cases.json": fwd_cases,
            "autoconnect_cases.json": autoconnect_cases}


def main(argv: list[str]) -> int:
    stale = False
    for name, build in COMMANDS.items():
        extra = {"constants": {k: getattr(P, k) for k in dir(P) if k.startswith("PARAM_")}} if name == "params_cases.json" else {}
        text = json.dumps({**extra, "cases": build()}, indent=1, allow_nan=False) + "\n"
        path = DATA / name
        if "--check" in argv:
            if not path.is_file() or path.read_text() != text:
                print(f"stale: {name}; run tools/dump_protocol_cases.py", file=sys.stderr)
                stale = True
            continue
        path.write_text(text)
        print(f"wrote {path}")
    return 1 if stale else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
