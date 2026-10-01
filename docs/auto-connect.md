# Auto-connect: interface reference

What Corvus GCS connects to when nobody tells it, and the contract anything
building on that behaviour can rely on.

For the operator's version of this, see
[Connect to the aircraft](https://m-bsquared.github.io/CorvusGCS/guide/connect.html)
in the documentation. This file is the interface: the resolution order, the two state
fields, the endpoint, the config block, and the reason strings. Implementation
lives in `corvus/autoconnect.py`, whose module docstring carries the reasoning
behind each rule.

---

## 1. Resolution order

At startup, `apply_startup_connection()` (`corvus/server.py`) asks
`resolve_startup_connection()` for a connection string. The first rule that
produces one wins:

| # | Rule | `reason` | Notes |
|---|---|---|---|
| 1 | Command-line argument | `cli` | `serve.py <port> <connection>` or `corvus/app.py <port> <connection>`. Only a person types this, so it outranks hardware. An unusable value falls through rather than failing the launch. |
| 2 | Direct USB flight controller | `usb-direct` | A CDC ACM node, or a `/dev/serial/by-id/` name matching a Pixhawk-class vendor. Dialled as `serial:<device>:<baud>`, where the baud rate is the configured connection's when that names the same port, and 57600 otherwise. |
| 3 | SiK telemetry radio | `sik` | A USB-to-serial bridge (FTDI / CP210x / CH340). Same string shape. |
| 4 | Configured connection | `configured` | `mavlink_connection` from `~/.corvus/config.json`. Honoured **verbatim**, including a stale `14540`, which is warned about and never rewritten. |
| 5 | UDP fallback | `udp-fallback` | `udp:0.0.0.0:14550`. The ground-station port, which is where PX4 SITL publishes. |
| 6 | Built-in default | `default-invalid-fallback` | Reached only when everything above is absent or rejected by `validate_connection`. Uses `DEFAULT_MAVLINK_CONNECTION`. |

Rules 2 and 3 are skipped when their config toggle is off; rule 5 likewise.
Both launchers pass `None` (not the config value) when no command-line
argument was given, so rule 1 cannot shadow rules 2 to 4.

**Ports are never opened to classify them.** Enumeration is
`MavlinkBridge.list_serial_ports()`, a `pyserial` describe plus a `/dev` glob.
Classification is `classify_serial_device()`, the same function the firmware
flasher uses, so the two cannot disagree about what is a flight controller.

Ports excluded before any rule sees them:

- **Phantoms**: `/dev/ttyS*`, the two macOS nodes that exist whether or not
  anything is plugged in, and pseudo-terminals (`is_phantom_device`).
- **Bootloaders**: a board in DFU / PX4 bootloader mode speaks the bootloader
  protocol, not MAVLink, and the flasher is about to want the port
  (`is_bootloader_port`). Closed-world: only a positive descriptor match skips,
  so an unknown board is never starved. The match is on the product string
  (`PX4 BL FMU ...`, ArduPilot's `<board>-BL`, `bootloader`, `DFU`) and on
  STM32 DFU's `0483:DF11`, never on a PX4 vendor/product id: PX4 boards use
  the same id in the bootloader and in the firmware (`26AC:0011` on
  FMUv2/v3), so matching it skipped every running Pixhawk 1 and Cube Black.
- **RTK base stations**: a GNSS receiver is a serial port that never sends a
  heartbeat, and on Linux a u-blox ZED-F9P on a USB lead enumerates as a
  `/dev/ttyACM*` node indistinguishable from a Pixhawk's. Without this the
  ground station would dial the base station plugged in beside the aircraft,
  hold its port, and wait for a heartbeat that cannot come, while the RTK
  service found the device already taken. Recognised by USB descriptor only
  (`is_rtk_device`: u-blox `1546`, Septentrio `152a`, plus product-name
  tokens), and closed-world in the opposite direction from the bootloader
  check: only a positive match is treated as a base, because refusing to
  auto-connect a real flight controller is by far the worse error.
  `classify_serial_device()` returns `rtk` for these, which is also what
  `corvus/rtk_service.py` picks its candidates by: one table, so the two
  cannot disagree about what is a receiver.

With two candidates of the same kind, the pick is the first by device name,
deterministic across launches, because connecting to a different aircraft
depending on USB enumeration order is not acceptable.

---

## 2. Runtime watcher

One daemon thread, `autoconnect-watcher`, created **after** `mavlink.start()`
and joined **before** `mavlink.stop()` in both launchers. It opens no sockets
and no serial ports. Default poll interval 2.5 s.

Each tick:

| Condition | Action |
|---|---|
| Auto-connect disabled | nothing |
| Bridge not running | nothing |
| The link's serial device is gone and `link_status` is still `connected`/`degraded`/`connecting` | nothing until the bridge notices (about 2 s) |
| The link's serial device is gone, `link_status` is `disconnected`/`reconnecting`, and exactly one present port has the USB identity it had | **follow**: re-dial that port at the link's own baud rate, also when `manual_override` is set |
| `manual_override` set | nothing |
| No serial candidate | clear any suggestion; reset the dial latch |
| `link_status` in `disconnected`/`reconnecting` **and** heartbeat older than 8 s | **dial**: `stop` → `validate` → `set_connection` → `start`, once per device appearance |
| anything else (connected, or degraded with a fresh heartbeat) | **suggest**: publish `link_suggestion`, never touch the link |

The dial is the identical sequence `POST /api/mavlink/connect` performs, so the
exclusive serial `flock`, the tlog rotation and the forwarder frame sink behave
exactly as they do for a manual connect.

The "once per device appearance" latch is cleared when the device disappears:
an operator who unplugs a cable and plugs it back in is asking again.

**Following a device to its new name.** One device does not keep one device
node: macOS names a device without a USB serial number after the socket it is
in, Windows may give it a new COM number per socket, and Linux hands out
`ttyACM1` when the cable comes back while `ttyACM0` is still held open. The
bridge would retry the vanished name forever, so the watcher remembers the USB
identity of the link's port (`VID:PID` plus the serial number from `hwid`,
`usb_identity()`) whenever it sees it, and follows that identity once the old
name is gone. Two ports with the same identity is no answer and nothing
happens. A bootloader port is neither remembered nor followed. An auto-dial
keeps the baud rate of the current connection when it names the same port.

---

## 3. State-store fields (pushed over the existing SSE stream)

No new socket and no polling. Both fields ride `/api/telemetry` beside
`link_status`, and both are in `IMMEDIATE_KEYS`, so a transition is not
delayed by the 30 Hz coalesce window.

### `link_auto`

```json
{
  "enabled": true,
  "usb": true,
  "sik": true,
  "udp_fallback": true,
  "manual_override": false,
  "reason": "usb-direct",
  "winner": "serial:/dev/ttyACM0:57600"
}
```

`reason` is one of the strings in §1, plus `manual` once the operator has
connected by hand. `winner` is the connection string that decision produced, or
`null` before the first decision.

### `link_suggestion`

`null`, or one offer:

```json
{
  "connection_string": "serial:/dev/ttyUSB0:57600",
  "kind": "usb-direct",
  "device": "/dev/ttyUSB0",
  "reason": "new-device-while-connected",
  "ts": 1789305600.0
}
```

`kind` is `usb-direct` or `sik`. Published only when the link is busy
elsewhere, deduped by `connection_string`, and cleared to `null` when the
device disappears or the operator connects to anything.

**A suggestion is display-only.** The backend never dials it. The frontend's
Connect button is the ordinary `POST /api/mavlink/connect` path.

---

## 4. Endpoints

### `GET /api/mavlink/auto`

Read-only. Decides nothing, dials nothing.

```json
{
  "auto": { "...": "the link_auto shape above" },
  "suggestion": null
}
```

Always `200`.

### `POST /api/mavlink/connect` (changed)

On success it additionally sets `manual_override = true` for the process,
clears `link_suggestion`, and sets `link_auto.reason` to `manual`. Status codes
and the validate-before-teardown order are unchanged; a connection the bridge
rejected sets nothing, because a rejected string is a typo, not a choice.

### `POST /api/mavlink/disconnect` (changed)

Idempotent stop as before, and additionally takes the manual override and
clears `link_suggestion`. "Leave it closed" has to mean closed: nothing dials
the link back open behind an operator who freed the radio. The way back is
another connect, or a restart.

### `POST /api/config` (changed)

Persists the `autoconnect` block and refreshes the live watcher's toggles. It
**does not dial**, the same invariant `mavlink_connection` has always had on
this endpoint.

---

## 5. Config block

`~/.corvus/config.json`, all keys optional:

```json
{
  "autoconnect": {
    "enabled": true,
    "usb": true,
    "sik": true,
    "udp_fallback": true
  }
}
```

- **Genuine booleans only.** `"true"` or `1` is rejected with a logged warning
  and the default applies, because `enabled` decides whether the station dials
  an aircraft on its own and a config must not flip it on a typo.
- A missing block, a missing key, or an unknown key inside it is not an error.
  A config file written before this feature loads exactly as it did.
- `manual_override` is **never** persisted. It is one operator's intent for one
  session; surviving a restart would make it fight the USB cable at the next
  field launch.

---

## 6. What it will not do

These are guarantees, not current behaviour that might drift:

- **It never takes a link that is up.** Dialling requires both an idle
  `link_status` and a stale heartbeat. A connected link, or a degraded one
  still receiving heartbeats, is left alone and gets a suggestion at most.
- **Manual wins the session.** After a manual connect or disconnect, nothing
  auto-dials and nothing is suggested until the process restarts.
- **It never opens a port to find out what is behind it.**
- **It never synthesises a `udpout:` or a TCP string.** The fallback is always
  the single `udp:` listen string; dial-out forms are operator-chosen only.
- **It never auto-picks PX4's onboard range** (`14540` to `14549`). An explicitly
  configured port there is honoured and warned about, never rewritten.
- **It adds one thread and no sockets.**

---

## 7. Tests

| File | Covers |
|---|---|
| `tests/test_autoconnect.py` | The policy: resolution order, the never-steal matrix, bootloader and phantom exclusion, dedupe and dial latches, the config block, the reason enum, the HTTP surface. |
| `tests/test_autoconnect_live.py` | The plumbing, on real sockets against a real MAVLink heartbeat source: connect-without-a-click, scanning with nothing present, loss and recovery, a disconnect that stays disconnected, forwarder-sink survival, no leaked threads. |

The serial half (USB and SiK priority against real hardware) needs a flight
controller on a cable and is covered by the policy tests against the real
classifier. The PX4-specific behaviour layered on top of the link (version
triple, parameter schema, stream intervals) is covered by the version,
parameter and stream-fallback suites; the plan's L1 to L6 Gazebo smoke is still
the only thing that exercises all of it end to end.
