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


COMMANDS = {"params_cases.json": cases, "logs_cases.json": logs_cases, "shell_cases.json": shell_cases,
            "ftp_cases.json": ftp_cases, "flight_cases.json": flight_cases}


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
