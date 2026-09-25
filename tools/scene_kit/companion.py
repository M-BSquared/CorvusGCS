"""A companion computer on an SSH socket, simulated well enough to photograph.

The SSH tab and the terminal windows are real terminals over a real SSH
session: xterm.js in the browser, paramiko in the backend, a pseudo-terminal
on the far end. A screenshot of one needs something on the far end, and the
operator's own machines are the wrong thing to put in a README: their host
names, their accounts, their home directories.

So this is the far end. It is a real SSH server (paramiko's server half, the
same library the backend connects with) that accepts one password, allocates
a pty, prints a login banner and runs a small scripted shell: a prompt in the
colours a stock Ubuntu gives, a handful of commands a companion computer
answers, and "command not found" for everything else. Like the aircraft, it is
a simulation, and the README says the pictures are made with one.

It listens on 127.0.0.1 only, on a port the runner picks, and stops with the
scene.
"""
from __future__ import annotations

import socket
import threading
import time
from dataclasses import dataclass, field
from collections.abc import Callable

import logging

import paramiko

PASSWORD = "corvus-scene"
# A fixed port where it is free, so the address on the connection card is the
# same in every picture; any free port otherwise.
DEFAULT_PORT = 2222

# paramiko narrates every handshake at INFO, which buries a scene's own output.
logging.getLogger("paramiko").setLevel(logging.WARNING)

# ANSI, as Ubuntu's default .bashrc colours the prompt.
_GREEN = "\x1b[01;32m"
_BLUE = "\x1b[01;34m"
_RESET = "\x1b[00m"


@dataclass(frozen=True)
class Machine:
    """One simulated computer: who logs in, what it is called, what it says."""

    username: str
    hostname: str
    banner: str
    commands: dict[str, str] = field(default_factory=dict)
    # Typed into the session as soon as it opens, so the terminal in the
    # picture shows a shell in use rather than a bare prompt.
    autotype: tuple[str, ...] = ()


def _crlf(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


JETSON = Machine(
    username="jetson",
    hostname="companion",
    # Short lines: this one is photographed in the side panel, which is about
    # forty columns wide, and a banner that wraps looks broken there.
    banner=(
        "Ubuntu 22.04.4 LTS, Jetson Orin Nano\n"
        "Autopilot on /dev/ttyTHS1, 921600 baud\n"
        "\n"
        "Last login: Thu Sep 24 09:12:40 2026\n"
    ),
    commands={
        "uptime": " 10:41:07 up 38 min,  1 user,  load average: 0.62, 0.48, 0.41",
        "uptime -p": "up 38 minutes",
        "systemctl is-active mavlink-router": f"{_GREEN}active{_RESET}",
        "pgrep -l mavlink": "912 mavlink-routerd",
        "systemctl status mavlink-router --no-pager": (
            f"{_GREEN}●{_RESET} mavlink-router.service - MAVLink Router\n"
            "     Loaded: loaded (/etc/systemd/system/mavlink-router.service; enabled)\n"
            f"     Active: {_GREEN}active (running){_RESET} since Thu 2026-09-24 10:03:12 CEST; 38min ago\n"
            "   Main PID: 912 (mavlink-routerd)\n"
            "      Tasks: 3 (limit: 9324)\n"
            "     Memory: 2.1M\n"
            "        CPU: 41.772s\n"
            "     CGroup: /system.slice/mavlink-router.service\n"
            "             └─912 /usr/bin/mavlink-routerd -c /etc/mavlink-router/main.conf\n"
            "\n"
            "Sep 24 10:03:12 companion mavlink-routerd[912]: Opened UART [4]uart: /dev/ttyTHS1\n"
            "Sep 24 10:03:12 companion mavlink-routerd[912]: Opened UDP Client [5]gcs: 192.168.144.11:14550\n"
            "Sep 24 10:03:13 companion mavlink-routerd[912]: Opened UDP Server [6]camera: 0.0.0.0:14560"
        ),
        "ls": f"{_BLUE}flight_logs{_RESET}  {_BLUE}missions{_RESET}  {_BLUE}payload{_RESET}  camera.env  mavlink-router.conf",
        "df -h /": (
            "Filesystem      Size  Used Avail Use% Mounted on\n"
            "/dev/nvme0n1p1  234G   41G  182G  19% /"
        ),
        "ip -br addr": (
            "lo     UNKNOWN  127.0.0.1/8\n"
            "eth0   UP       192.168.144.20/24\n"
            "wlan0  DOWN"
        ),
        "tegrastats --interval 1000 --count 1": (
            "09-24-2026 10:41:12 RAM 2913/7620MB (lfb 3x4MB) CPU [18%@1510,12%@1510,"
            "9%@1510,14%@1510,7%@1510,11%@1510] GR3D_FREQ 31% cpu@47.1C gpu@45.9C"
        ),
        "whoami": "jetson",
        "hostname": "companion",
    },
    autotype=("uptime -p", "pgrep -l mavlink", "ip -br addr"),
)

RASPBERRY = Machine(
    username="pi",
    hostname="payload",
    banner=(
        "Linux payload 6.6.51+rpt-rpi-2712 #1 SMP PREEMPT Debian 1:6.6.51-1+rpt3 aarch64\n"
        "\n"
        "Last login: Thu Sep 24 09:58:03 2026 from 192.168.144.11\n"
    ),
    commands={
        "ls payload": "camera.py  gimbal.py  config.yaml  README.md",
        "python3 payload/camera.py --status": (
            "camera   IMX477 12.3 MP   streaming   rtsp://0.0.0.0:8554/main   30 fps\n"
            "gimbal   SIYI A8 mini     locked      pitch -45.0°  yaw 0.0°\n"
            "storage  /media/pi/SD     212 images  18.4 GB free"
        ),
        "vcgencmd measure_temp": "temp=52.6'C",
        "whoami": "pi",
        "hostname": "payload",
    },
    # This one is photographed in a terminal window, which has room for a
    # status table.
    autotype=("ls payload", "python3 payload/camera.py --status", "vcgencmd measure_temp"),
)

MACHINES: dict[str, Machine] = {m.username: m for m in (JETSON, RASPBERRY)}


class _Server(paramiko.ServerInterface):
    def __init__(self) -> None:
        self.shell_ready = threading.Event()
        self.username = ""
        self.size = (80, 24)

    def get_allowed_auths(self, username: str) -> str:
        return "password"

    def check_auth_password(self, username: str, password: str) -> int:
        if username in MACHINES and password == PASSWORD:
            self.username = username
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind: str, chanid: int) -> int:
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_pty_request(self, channel, term, width, height,
                                  pixelwidth, pixelheight, modes) -> bool:
        self.size = (int(width) or 80, int(height) or 24)
        return True

    def check_channel_window_change_request(self, channel, width, height,
                                            pixelwidth, pixelheight) -> bool:
        self.size = (int(width) or 80, int(height) or 24)
        return True

    def check_channel_shell_request(self, channel) -> bool:
        self.shell_ready.set()
        return True


class CompanionServer:
    """An SSH server on 127.0.0.1 that hosts the machines above."""

    def __init__(self, port: int = DEFAULT_PORT,
                 on_event: Callable[[str], None] | None = None) -> None:
        self._port = port
        self._on_event = on_event or (lambda _text: None)
        self._sock: socket.socket | None = None
        self._stop = threading.Event()
        self._threads: list[threading.Thread] = []
        self._transports: list[paramiko.Transport] = []
        self._lock = threading.Lock()
        self._key: paramiko.PKey | None = None

    @property
    def port(self) -> int:
        return self._sock.getsockname()[1] if self._sock is not None else self._port

    def start(self) -> None:
        # A fresh host key per run: the sandbox's known_hosts is thrown away
        # with the sandbox, so there is nothing for a stable key to match.
        self._key = paramiko.RSAKey.generate(2048)
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind(("127.0.0.1", self._port))
        except OSError:
            sock.bind(("127.0.0.1", 0))
        sock.listen(8)
        sock.settimeout(0.3)
        self._sock = sock
        self._stop.clear()
        thread = threading.Thread(target=self._accept_loop, name="companion-accept",
                                  daemon=True)
        thread.start()
        self._threads.append(thread)
        self._on_event(f"companion SSH on 127.0.0.1:{self.port}")

    def stop(self) -> None:
        self._stop.set()
        sock, self._sock = self._sock, None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        with self._lock:
            transports = list(self._transports)
            self._transports.clear()
        for transport in transports:
            try:
                transport.close()
            except Exception:  # noqa: BLE001 - teardown must not raise
                pass
        for thread in self._threads:
            thread.join(timeout=2.0)
        self._threads.clear()

    def _accept_loop(self) -> None:
        while not self._stop.is_set():
            sock = self._sock
            if sock is None:
                return
            try:
                client, _addr = sock.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            thread = threading.Thread(target=self._serve, args=(client,),
                                      name="companion-session", daemon=True)
            thread.start()
            self._threads.append(thread)

    def _serve(self, client: socket.socket) -> None:
        transport = paramiko.Transport(client)
        with self._lock:
            self._transports.append(transport)
        transport.add_server_key(self._key)
        server = _Server()
        try:
            transport.start_server(server=server)
        except (paramiko.SSHException, EOFError, OSError):
            return
        channel = transport.accept(timeout=10)
        if channel is None:
            return
        if not server.shell_ready.wait(timeout=10):
            channel.close()
            return
        machine = MACHINES.get(server.username)
        if machine is None:
            channel.close()
            return
        self._on_event(f"companion: {machine.username}@{machine.hostname} logged in")
        try:
            _Shell(channel, machine, self._stop).run()
        except (OSError, EOFError, paramiko.SSHException):
            pass
        finally:
            try:
                channel.close()
            except Exception:  # noqa: BLE001
                pass


class _Shell:
    """Line editing, a prompt and the canned answers. Enough for a picture."""

    def __init__(self, channel: paramiko.Channel, machine: Machine,
                 stop: threading.Event) -> None:
        self.channel = channel
        self.machine = machine
        self.stop = stop
        self.line = ""

    def write(self, text: str) -> None:
        self.channel.sendall(_crlf(text).encode("utf-8"))

    def prompt(self) -> None:
        m = self.machine
        self.write(f"{_GREEN}{m.username}@{m.hostname}{_RESET}:{_BLUE}~{_RESET}$ ")

    def run_command(self, command: str) -> bool:
        command = command.strip()
        if not command:
            return True
        if command in ("exit", "logout"):
            self.write("logout\n")
            return False
        if command == "clear":
            self.write("\x1b[H\x1b[2J")
            return True
        out = self.machine.commands.get(command)
        if out is None:
            name = command.split()[0]
            out = f"{name}: command not found"
        self.write(out + "\n")
        return True

    def run(self) -> None:
        # A login takes a moment, and the backend writes its own "Connected
        # to" line into the terminal once the channel is up. Speaking first
        # would put that line after the prompt instead of above the banner.
        time.sleep(1.0)
        self.write(self.machine.banner + "\n")
        self.prompt()
        # Typed the way a person types, so the echo arrives in pieces and the
        # terminal draws it as it would for a real session.
        time.sleep(0.6)
        for command in self.machine.autotype:
            for char in command:
                self.write(char)
                time.sleep(0.012)
            self.write("\n")
            self.run_command(command)
            self.prompt()
            time.sleep(0.25)

        self.channel.settimeout(0.3)
        while not self.stop.is_set():
            try:
                data = self.channel.recv(1024)
            except TimeoutError:
                continue
            if not data:
                return
            for char in data.decode("utf-8", "replace"):
                if char in ("\r", "\n"):
                    self.write("\n")
                    keep = self.run_command(self.line)
                    self.line = ""
                    if not keep:
                        return
                    self.prompt()
                elif char == "\x03":
                    self.write("^C\n")
                    self.line = ""
                    self.prompt()
                elif char in ("\x7f", "\b"):
                    if self.line:
                        self.line = self.line[:-1]
                        self.write("\b \b")
                elif char == "\x04" and not self.line:
                    self.write("logout\n")
                    return
                elif char >= " ":
                    self.line += char
                    self.write(char)
