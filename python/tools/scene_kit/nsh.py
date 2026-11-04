"""The PX4 NuttShell (NSH) the simulated aircraft runs, for the NuttX Console.

PX4 runs NSH for a ground station over SERIAL_CONTROL (device SHELL): the
ground station sends what is typed in chunks of up to 70 bytes, and the
autopilot answers with the echo, the command's output and a fresh ``nsh>``
prompt, in chunks of the same size. The NuttX Console plugin is a terminal on
that shell, so a picture of it needs something on the far end that answers
the way PX4 does.

This is that far end: line editing, an echo, and canned answers for the
commands a pilot types at a PX4 shell, written for the aircraft the scenes fly
(PX4 1.18 on a Pixhawk 6X). Everything else gets NSH's own "command not
found". Like the companion computer, it is a simulation; nothing here is read
from a real flight controller.
"""
from __future__ import annotations

PROMPT = "nsh> "

_VER_ALL = """\
HW arch: PX4_FMU_V6X
HW type: V6X010010
HW version: 0x00000001
HW revision: 0x00000010
PX4 git-hash: c9b8a7f6e5d4c3b2a1908f7e6d5c4b3a29180706
PX4 version: 1.18.0 0 (17956863)
PX4 git-branch: release/1.18
Vendor version: 0.0.0 0 (0)
OS: NuttX
OS version: Release 11.0.0 (184549631)
OS git-hash: 7d4a1e0c3b9f2a6e5d8c1b0a9f8e7d6c5b4a3928
Build datetime: Sep 10 2026 10:22:41
Build uri: localhost
Build variant: default
Toolchain: GNU GCC, 13.2.1 20231009
PX4GUID: 000600000000373833333430510d00360031
MCU: STM32H7[4|5]xxx, rev. V"""

_FREE = """\
              total       used       free    maxused    maxfree  nused  nfree
        Umem:    925632     402816     522816     411472     481440   1452     23"""

_TOP = """\
Processes: 41 total, 3 running, 38 sleeping
CPU usage: 41.62% tasks, 1.27% sched, 57.11% idle
DMA Memory: 5120 total, 1536 used 1536 peak
Uptime: 1846.312s total, 1012.540s idle

 PID COMMAND                   CPU(ms) CPU(%)  USED/STACK PRIO(BASE) STATE FD
   0 Idle Task                 1012540 57.110   264/  728   0 (  0)  READY  3
   1 hpwork                        212  0.000   304/ 1224 249 (249)  w:sem  3
   2 lpwork                       1405  0.081   632/ 1584  50 ( 50)  w:sem  3
   3 nsh_main                      392  0.000  2104/ 3208 100 (100)  w:sem  3
 252 wq:manager                     19  0.000   464/ 1232 255 (255)  w:sem  4
 260 wq:SPI1                     54204  2.975  1424/ 2336 253 (253)  w:sem  4
 262 wq:SPI2                     41866  2.300  1488/ 2336 252 (252)  w:sem  4
 263 wq:SPI3                     38902  2.134  1424/ 2336 251 (251)  w:sem  4
 265 wq:I2C1                      9611  0.528  1160/ 2312 246 (246)  w:sem  4
 276 wq:nav_and_controllers     214455 11.774  1720/ 2216 242 (242)  w:sem  4
 277 wq:rate_ctrl               172030  9.430  1528/ 3120 255 (255)  w:sem  4
 279 wq:INS0                    121377  6.663  4664/ 5976 241 (241)  w:sem  4
 281 wq:hp_default               45219  2.482  1416/ 2768 237 (237)  w:sem  4
 297 commander                    5304  0.291  1504/ 3192 140 (140)  w:sig  5
 312 mavlink_if0                 28410  1.559  2384/ 3080 100 (100)  READY  4
 315 mavlink_rcv_if0              6931  0.380  3456/ 4648 175 (175)  w:sem  4
 332 logger                       9874  0.542  2160/ 3640 230 (230)  w:sem  3
 348 navigator                    1502  0.082  1272/ 2160 105 (105)  w:sem  6"""

_LISTENER = """\

TOPIC: sensor_combined
 sensor_combined
    timestamp: 1846311204 (0.004 s ago)
    gyro_rad: [0.0021, -0.0038, 0.0012]
    gyro_integral_dt: 4000
    accelerometer_m_s2: [0.13, -0.09, -9.80]
    accelerometer_integral_dt: 4000
    accelerometer_clipping: 0
    gyro_clipping: 0
    accel_calibration_count: 3"""

_UORB = """\
update: 1s, num topics: 168
TOPIC NAME                        INST #SUB RATE #Q SIZE
actuator_motors                      0    4  400  1   80
battery_status                       0    9    2  1  168
estimator_status                     0    5  100  1  120
sensor_accel                         0    6  800  8   48
sensor_combined                      0    5  250  1   48
sensor_gyro                          0    6  800  8   48
vehicle_attitude                     0   10  250  1   56
vehicle_global_position              0   12  100  1   64
vehicle_local_position               0   21  100  1  168
vehicle_status                       0   26    2  1   96"""

_DMESG = """\
HW arch: PX4_FMU_V6X
HW type: V6X010010
PX4 git-hash: c9b8a7f6e5d4c3b2a1908f7e6d5c4b3a29180706
sercon: Registering CDC/ACM serial driver
INFO  [param] selected parameter default file /fs/mtd_params
INFO  [param] importing from '/fs/mtd_params'
INFO  [parameters] BSON document size 2304 bytes, decoded 2304 bytes (INT32:41, FLOAT:96)
INFO  [dataman] data manager file '/fs/microsd/dataman' size is 1208528 bytes
INFO  [logger] logger started (mode=all)
INFO  [mavlink] mode: Normal, data rate: 1200 B/s on /dev/ttyS6 @ 57600B
INFO  [init] starting PX4 sensors
INFO  [px4] Startup script returned successfully"""

COMMANDS: dict[str, str] = {
    "ver all": _VER_ALL,
    "ver hwcmp": "PX4_FMU_V6X",
    "free": _FREE,
    "top once": _TOP,
    "listener sensor_combined": _LISTENER,
    "uorb top -1": _UORB,
    "dmesg": _DMESG,
    "param show MPC_XY_VEL_MAX": (
        "Symbols: x = used, + = saved, * = unsaved\n"
        "x + MPC_XY_VEL_MAX [436,812] : 12.0000\n\n"
        " 1043/1220 parameters used."
    ),
    "param show COM_DISARM_LAND": (
        "Symbols: x = used, + = saved, * = unsaved\n"
        "x   COM_DISARM_LAND [162,281] : 2.0000\n\n"
        " 1043/1220 parameters used."
    ),
    "uptime": "uptime: 1846.31 s",
}


class Nsh:
    """One NSH session: bytes in, the bytes PX4 would answer with out."""

    def __init__(self) -> None:
        self.line = ""

    def feed(self, data: bytes) -> str:
        """What the shell prints for *data*, the typed bytes of one packet."""
        out: list[str] = []
        for char in data.decode("utf-8", "replace"):
            if char == "\x00":
                continue
            if char in ("\r", "\n"):
                out.append("\r\n")
                out.append(self.run(self.line))
                out.append(PROMPT)
                self.line = ""
            elif char == "\x03":
                self.line = ""
                out.append("^C\r\n" + PROMPT)
            elif char in ("\x7f", "\b"):
                if self.line:
                    self.line = self.line[:-1]
                    out.append("\b \b")
            elif char >= " ":
                self.line += char
                out.append(char)
        return "".join(out)

    @staticmethod
    def run(command: str) -> str:
        """The output of one command line, with NSH's line endings."""
        command = " ".join(command.split())
        if not command:
            return ""
        text = COMMANDS.get(command)
        if text is None:
            text = f"nsh: {command.split()[0]}: command not found"
        return text.replace("\n", "\r\n") + "\r\n"


def chunks(text: str, size: int = 70) -> list[bytes]:
    """*text* as SERIAL_CONTROL payloads, never splitting a character."""
    data = text.encode("utf-8")
    out: list[bytes] = []
    start = 0
    while start < len(data):
        end = min(start + size, len(data))
        while end < len(data) and end > start and (data[end] & 0xC0) == 0x80:
            end -= 1
        out.append(data[start:end])
        start = end
    return out
