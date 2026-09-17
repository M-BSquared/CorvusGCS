"""corvus.sik_service — AT sessions, the gate, and who owns the serial port.

The schema module decides what a radio may be told; this one decides when it is
safe to say anything at all. Three properties carry the whole feature and each
has a way to fail that ends with an aircraft that cannot be talked to:

* the MAVLink bridge is stopped before the port is taken and restarted
  afterwards, *whatever* happened in between, including a refusal;
* the far radio is written before the near one, because the near one is the
  path to the far one;
* a write that is rejected part-way never reaches ``AT&W``.

A fake serial port stands in for the radio. Nothing here opens a real device,
and the guard timings are collapsed so the suite does not spend a minute
sleeping through escape sequences.
"""
from __future__ import annotations

from typing import Any

import pytest

pytest.importorskip("pymavlink")

from corvus import sik_config, sik_service  # noqa: E402


@pytest.fixture(autouse=True)
def _no_sleeping(monkeypatch: pytest.MonkeyPatch) -> None:
    """Collapse the guard intervals and the reboot wait.

    They are real requirements of the protocol — the radio measures the silence
    around ``+++`` against its own clock — but they are requirements of the
    radio, not of the logic under test, and at full length this file would take
    most of a minute to assert on dictionaries.
    """
    monkeypatch.setattr(sik_service, "GUARD_S", 0.0)
    monkeypatch.setattr(sik_service, "IDLE_S", 0.0)
    monkeypatch.setattr(sik_service, "REBOOT_S", 0.0)
    # The read timeouts matter more than the guards here: a reply is "finished"
    # when the port goes quiet, so a radio that says nothing costs a full
    # timeout — eight times over, once per candidate baud, in the scan tests.
    monkeypatch.setattr(sik_service, "LOCAL_TIMEOUT_S", 0.05)
    monkeypatch.setattr(sik_service, "REMOTE_TIMEOUT_S", 0.05)


ATI5_LOCAL = "\r\n".join([
    "S0: FORMAT=25", "S1: SERIAL_SPEED=57", "S2: AIR_SPEED=64", "S3: NETID=25",
    "S4: TXPOWER=20", "S5: ECC=0", "S6: MAVLINK=1", "S8: MIN_FREQ=915000",
    "S9: MAX_FREQ=928000", "S10: NUM_CHANNELS=50", "S11: DUTY_CYCLE=100",
    "S12: LBT_RSSI=0", "S15: MAX_WINDOW=131", "OK", "",
])
ATI5_REMOTE = ATI5_LOCAL.replace("NETID=25", "NETID=30")


class FakePort:
    """A serial port that answers AT commands from a scripted table.

    Records every command it was given, in order, which is what the write-order
    assertions read. ``answers`` maps an upper-cased command to its reply;
    anything not in it gets ``OK``, and ``errors`` names the commands that come
    back as ``ERROR`` instead.
    """

    def __init__(self, answers: dict[str, str] | None = None,
                 errors: set[str] | None = None,
                 escape_ok: bool = True, baud_required: int | None = None,
                 baudrate: int = 57600) -> None:
        self.answers = dict(answers or {})
        self.errors = set(errors or ())
        self.escape_ok = escape_ok
        self.baud_required = baud_required
        self.baudrate = baudrate
        self.commands: list[str] = []
        self.closed = False
        self._pending = ""

    # -- the pyserial surface the session touches -----------------------

    @property
    def in_waiting(self) -> int:
        return len(self._pending)

    def read(self, count: int) -> bytes:
        chunk, self._pending = self._pending[:count], self._pending[count:]
        return chunk.encode("ascii")

    def reset_input_buffer(self) -> None:
        self._pending = ""

    def flush(self) -> None:
        pass

    def close(self) -> None:
        self.closed = True

    def _reachable(self) -> bool:
        """A port opened at the wrong baud delivers noise, not words.

        Modelled as silence rather than as garbage: either way nothing the
        session sends is understood and nothing it reads parses, and silence is
        the version that does not accidentally match a regex.
        """
        return self.baud_required is None or self.baud_required == self.baudrate

    def write(self, data: bytes) -> int:
        text = data.decode("ascii")
        if text == "+++":
            self._pending = "OK\r\n" if (self.escape_ok and self._reachable()) else ""
            return len(data)
        command = text.strip().upper()
        if not self._reachable():
            self.commands.append(command)
            self._pending = ""
            return len(data)
        self.commands.append(command)
        if command in self.errors:
            self._pending = "ERROR\r\n"
        elif command in self.answers:
            self._pending = self.answers[command]
        else:
            self._pending = "OK\r\n"
        return len(data)


class FakeBridge:
    """Bridge stand-in recording the stop/start cycle a session drives."""

    def __init__(self, device: str = "/dev/ttyUSB0", baud: int = 57600,
                 running: bool = True) -> None:
        self.device = device
        self.baud = baud
        self.running = running
        self.events: list[str] = []

    def serial_device(self) -> str:
        return self.device

    def connection_string(self) -> str:
        return f"serial:{self.device}:{self.baud}"

    def is_running(self) -> bool:
        return self.running

    def transport(self) -> str:
        return "sik"

    def list_serial_ports(self) -> list[dict[str, str]]:
        return [
            {"device": self.device, "description": "FT232R", "hwid": "USB VID:PID=0403:6001"},
            {"device": "/dev/ttyUSB9", "description": "spare", "hwid": ""},
        ]

    def _parse_serial(self, conn: str) -> tuple[str, int]:
        rest = conn[len("serial:"):]
        device, _, baud = rest.rpartition(":")
        return device, int(baud)

    def stop(self) -> None:
        self.running = False
        self.events.append("stop")

    def start(self) -> None:
        self.running = True
        self.events.append("start")

    def set_connection(self, conn: str) -> None:
        self.device, self.baud = self._parse_serial(conn)
        self.events.append(f"set:{conn}")


class FakeStore:
    def __init__(self, armed: bool = False) -> None:
        self.armed = armed

    def get_snapshot(self) -> dict[str, Any]:
        return {"armed": self.armed}


def _service(monkeypatch: pytest.MonkeyPatch, port: FakePort,
             bridge: FakeBridge | None = None,
             store: FakeStore | None = None) -> tuple[sik_service.SikService, list[int]]:
    """Wire a service whose ``serial.Serial`` hands back *port*.

    Returns the bauds it was asked to open at, so the scan can be asserted on.
    """
    tried: list[int] = []

    class FakeSerialModule:
        @staticmethod
        def Serial(device: str, baudrate: int = 57600, **kwargs: Any) -> FakePort:
            tried.append(baudrate)
            port.baudrate = baudrate
            port.closed = False
            return port

    monkeypatch.setattr(sik_service, "_serial", FakeSerialModule)
    service = sik_service.SikService(bridge, store or FakeStore())
    return service, tried


# ---------------------------------------------------------------------------
# Reading
# ---------------------------------------------------------------------------

def test_a_load_reads_both_radios_and_reports_the_disagreement(monkeypatch) -> None:
    port = FakePort({
        "ATI5": ATI5_LOCAL, "RTI5": ATI5_REMOTE,
        "ATI": "SiK 2.0 on HM-TRP\r\n", "RTI": "SiK 2.0 on HM-TRP\r\n",
        "ATI7": "L/R RSSI: 208/205  L/R noise: 41/38 pkts: 20\r\n",
    })
    service, _ = _service(monkeypatch, port, FakeBridge())

    result = service.load("/dev/ttyUSB0", 57600)

    assert result["local"]["version"] == "SiK 2.0 on HM-TRP"
    assert result["remote_reachable"] is True
    assert [m["name"] for m in result["mismatches"]] == ["NETID"]
    assert result["link"]["local_rssi"] == 208


def test_a_remote_that_does_not_answer_is_reported_not_raised(monkeypatch) -> None:
    # The ordinary case of a partner that is switched off. A page that showed an
    # error here would send the operator looking for a fault that is not there.
    port = FakePort({"ATI5": ATI5_LOCAL, "RTI5": "", "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    result = service.load("/dev/ttyUSB0", 57600)

    assert result["remote_reachable"] is False
    assert result["remote"] is None
    assert result["mismatches"] == []


def test_skipping_the_remote_never_sends_an_rt_command(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    service.load("/dev/ttyUSB0", 57600, include_remote=False)

    assert not [c for c in port.commands if c.startswith("RT")]


def test_a_radio_that_answers_the_escape_but_no_registers_is_an_error(monkeypatch) -> None:
    port = FakePort({"ATI5": "OK\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="ATI5"):
        service.load("/dev/ttyUSB0", 57600)


def test_a_port_that_never_answers_names_the_port_and_gives_up(monkeypatch) -> None:
    port = FakePort(escape_ok=False)
    service, tried = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="could not reach a radio"):
        service.load("/dev/ttyUSB0", 57600)
    # Every candidate baud was tried before giving up.
    assert set(tried) >= set(sik_config.BAUD_CANDIDATES)
    assert port.closed is True


def test_a_radio_at_a_forgotten_baud_is_still_found(monkeypatch) -> None:
    # The operator who most needs this page is the one who changed SERIAL_SPEED
    # and no longer remembers to what.
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"}, baud_required=9600)
    service, tried = _service(monkeypatch, port, FakeBridge())

    result = service.load("/dev/ttyUSB0", 57600, include_remote=False)

    assert result["baud"] == 9600
    assert tried[0] == 57600  # the requested one first, all the same


# ---------------------------------------------------------------------------
# Who owns the port
# ---------------------------------------------------------------------------

def test_the_bridge_is_stopped_and_restarted_around_a_session_on_its_own_port(
        monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    bridge = FakeBridge(device="/dev/ttyUSB0")
    service, _ = _service(monkeypatch, port, bridge)

    service.load("/dev/ttyUSB0", 57600, include_remote=False)

    assert bridge.events == ["stop", "set:serial:/dev/ttyUSB0:57600", "start"]
    assert bridge.running is True


def test_a_session_on_another_port_leaves_the_live_link_alone(monkeypatch) -> None:
    # Configuring the spare radio on the bench must not interrupt telemetry.
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    bridge = FakeBridge(device="/dev/ttyUSB0")
    service, _ = _service(monkeypatch, port, bridge)

    service.load("/dev/ttyUSB9", 57600, include_remote=False)

    assert bridge.events == []
    assert bridge.running is True


def test_a_failed_session_still_gives_the_link_back(monkeypatch) -> None:
    # The failure mode this prevents: a radio that does not answer costs the
    # operator their telemetry link until they restart the application.
    port = FakePort(escape_ok=False)
    bridge = FakeBridge(device="/dev/ttyUSB0")
    service, _ = _service(monkeypatch, port, bridge)

    with pytest.raises(sik_service.SikError):
        service.load("/dev/ttyUSB0", 57600)

    assert bridge.events[-1] == "start"
    assert bridge.running is True


def test_a_shutdown_in_progress_does_not_respawn_the_bridge(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    bridge = FakeBridge(device="/dev/ttyUSB0")
    service, _ = _service(monkeypatch, port, bridge)
    service.shutdown()

    service.load("/dev/ttyUSB0", 57600, include_remote=False)

    assert "start" not in bridge.events


# ---------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------

def test_every_operation_is_refused_while_armed(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL})
    bridge = FakeBridge()
    service, _ = _service(monkeypatch, port, bridge, FakeStore(armed=True))

    for call in (lambda: service.load("/dev/ttyUSB0"),
                 lambda: service.save("/dev/ttyUSB0", local={"NETID": 42}),
                 lambda: service.reset("/dev/ttyUSB0")):
        with pytest.raises(sik_service.SikError, match="armed"):
            call()
    # And nothing was taken from the link on the way to the refusal.
    assert bridge.events == []


def test_a_session_without_a_port_is_refused_before_anything_is_opened(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL})
    service, tried = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="no serial port"):
        service.load("  ")
    assert tried == []


def test_the_status_reports_the_gate_and_marks_the_links_own_port(monkeypatch) -> None:
    port = FakePort()
    bridge = FakeBridge(device="/dev/ttyUSB0", baud=57600)
    service, _ = _service(monkeypatch, port, bridge, FakeStore(armed=True))

    status = service.status()

    assert status["can_configure"] is False
    assert "armed" in status["blocked_reason"]
    assert status["link_device"] == "/dev/ttyUSB0"
    assert status["link_baud"] == 57600
    by_device = {p["device"]: p for p in status["ports"]}
    assert by_device["/dev/ttyUSB0"]["is_link"] is True
    assert by_device["/dev/ttyUSB0"]["kind"] == "sik"
    assert by_device["/dev/ttyUSB9"]["is_link"] is False


# ---------------------------------------------------------------------------
# Writing
# ---------------------------------------------------------------------------

def test_the_remote_radio_is_written_before_the_one_on_the_cable(monkeypatch) -> None:
    # The property the whole save path is built around. Writing the near radio
    # first would sever the only path to the far one before it had been told,
    # leaving an aircraft radio reachable only by retrieving the airframe.
    port = FakePort({"ATI5": ATI5_LOCAL, "RTI5": ATI5_REMOTE,
                     "ATI": "SiK 2.0\r\n", "RTI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    service.save("/dev/ttyUSB0", 57600,
                 local={"NETID": 42}, remote={"NETID": 42})

    writes = [c for c in port.commands if c.endswith("=42")]
    assert writes == ["RTS3=42", "ATS3=42"]
    assert port.commands.index("RT&W") < port.commands.index("ATS3=42")


def test_each_side_commits_and_reboots_after_its_own_writes(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    service.save("/dev/ttyUSB0", 57600, local={"NETID": 42, "TXPOWER": 14})

    tail = [c for c in port.commands if c.startswith("ATS") or c in {"AT&W", "ATZ"}]
    assert tail == ["ATS3=42", "ATS4=14", "AT&W", "ATZ"]


def test_a_rejected_register_aborts_before_the_settings_are_committed(monkeypatch) -> None:
    # A radio that committed the subset it happened to accept would reboot into
    # a configuration nobody chose.
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"}, errors={"ATS4=14"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="ATS4=14"):
        service.save("/dev/ttyUSB0", 57600, local={"NETID": 42, "TXPOWER": 14})

    assert "AT&W" not in port.commands
    assert "ATZ" not in port.commands


def test_an_invalid_value_is_refused_by_the_schema_before_the_radio_sees_it(
        monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_config.SikConfigError):
        service.save("/dev/ttyUSB0", 57600, local={"AIR_SPEED": 100})

    assert not [c for c in port.commands if c.startswith("ATS2")]


def test_writing_a_remote_that_is_not_answering_is_refused(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "RTI5": "", "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="not answering"):
        service.save("/dev/ttyUSB0", 57600, remote={"NETID": 42})


def test_changing_the_local_baud_moves_the_link_to_match(monkeypatch) -> None:
    # Without this the bridge restarts on the old baud and the link simply
    # never comes back, with nothing on screen explaining why.
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    bridge = FakeBridge(device="/dev/ttyUSB0", baud=57600)
    service, _ = _service(monkeypatch, port, bridge)

    result = service.save("/dev/ttyUSB0", 57600, local={"SERIAL_SPEED": 115})

    assert result["new_serial_speed"] == 115200
    assert bridge.events[-1] == "start"
    assert "set:serial:/dev/ttyUSB0:115200" in bridge.events
    assert result["warnings"] and "115200" in result["warnings"][0]


def test_a_baud_change_on_another_port_does_not_move_the_link(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    bridge = FakeBridge(device="/dev/ttyUSB0", baud=57600)
    service, _ = _service(monkeypatch, port, bridge)

    service.save("/dev/ttyUSB9", 57600, local={"SERIAL_SPEED": 115})

    assert bridge.events == []
    assert bridge.baud == 57600


# ---------------------------------------------------------------------------
# Resetting
# ---------------------------------------------------------------------------

def test_a_factory_reset_writes_and_reboots(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    service.reset("/dev/ttyUSB0", 57600, target="local")

    assert [c for c in port.commands if c.startswith("AT&") or c == "ATZ"] == [
        "AT&F", "AT&W", "ATZ",
    ]


def test_resetting_a_remote_that_is_not_answering_is_refused(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL, "RTI5": "", "ATI": "SiK 2.0\r\n"})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="not answering"):
        service.reset("/dev/ttyUSB0", 57600, target="remote")

    assert "RT&F" not in port.commands


def test_an_unknown_reset_target_is_refused(monkeypatch) -> None:
    port = FakePort({"ATI5": ATI5_LOCAL})
    service, _ = _service(monkeypatch, port, FakeBridge())

    with pytest.raises(sik_service.SikError, match="local"):
        service.reset("/dev/ttyUSB0", 57600, target="both")


# ---------------------------------------------------------------------------
# Port identity
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(("left", "right", "same"), [
    ("/dev/ttyUSB0", "/dev/ttyUSB0", True),
    ("/dev/ttyUSB0", "/dev/ttyUSB1", False),
    # A POSIX device path is case-sensitive; a Windows COM name is not.
    ("/dev/ttyUSB0", "/dev/ttyusb0", False),
    ("COM7", "com7", True),
    ("COM7", "COM17", False),
    ("", "/dev/ttyUSB0", False),
])
def test_two_port_names_refer_to_the_same_device_or_not(left, right, same) -> None:
    assert sik_service._same_device(left, right) is same
