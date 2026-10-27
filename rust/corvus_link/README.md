# corvus_link

MAVLink for Corvus GCS, written in Rust. A Python extension module (PyO3,
built with maturin) with pymavlink's interface, so the backend imports

```python
from corvus_link import mavutil
```

where it used to import `pymavlink.mavutil`, and nothing else in Corvus
changes.

## Layout

```
src/
  lib.rs        the Python module: corvus_link, corvus_link.mavutil, corvus_link.params
  wire/         MAVLink itself: dialect tables, messages, codec, CRC, framing, modes
  link/         the links: mavlink_connection, per-system state, UDP, TCP, serial, ...
  protocol/     the bridge's protocols, moved from Python one at a time
```

## What it does

| Part | Source | pymavlink counterpart |
| --- | --- | --- |
| Message, enum and constant tables of the `all` dialect, generated from the XML at build time | `build.rs`, `src/wire/defs.rs`, `message_definitions/v1.0/` | `mavgen` output in `dialects/v10/all.py`, `dialects/v20/all.py` |
| `mavutil.mavlink`: constants, `enums`, `MAVLink_<name>_message` classes, `mavlink_map` | `src/wire/dialect.rs` | the generated dialect module |
| Message objects (`get_type`, `get_msgbuf`, `to_dict`, fields as attributes) | `src/wire/message.rs` | `MAVLink_message` |
| Payload packing and unpacking with `struct`'s rules | `src/wire/codec.rs` | `struct.Struct` per message |
| Framing, CRC_EXTRA, sequence numbers, MAVLink 2 signing, the stream parser | `src/wire/frame.rs`, `src/wire/crc.rs` | `MAVLink`, `MAVLinkSigning`, `x25crc` |
| `mavlink_connection`, per-system state, `recv_match`, `wait_heartbeat`, `mode_mapping`, `setup_signing` | `src/link/connection.rs` | `mavfile` and its subclasses |
| The links | `src/link/transport.rs`, `src/link/serial_posix.rs` | `mavudp`, `mavtcp`, `mavtcpin`, `mavserial`, `mavuds`, `mavmcast`, `mavchildexec`, `mavlogfile` |
| Flight mode tables and names | `src/wire/modes.rs` | `mode_mapping_*`, `px4_map`, `mode_string_v10` |

A link starts on the MAVLink 1 view (message ids up to 255, no extension
fields) and moves to MAVLink 2 on its first MAVLink 2 frame, process wide, as
pymavlink does.

## Protocols moved from the bridge

| Protocol | Source | Python side |
| --- | --- | --- |
| Parameters: the cache, the download state machine and its lossy-link watchdog, PARAM_VALUE checks and decoding, named reads with the barrier, confirmed writes, verify, the batch upload, and the integer value codec (bytewise or C cast) | `src/protocol/params.rs` (`corvus_link.params`) | `corvus/mavlink_params.py` keeps the bridge's methods and attribute names, the locks, the two worker threads and the listeners, and hands the engine the bridge for the link, the clock and the command error |

| Missions: the upload and download handshakes for missions and geofences, the item requests served during an upload, MISSION_CURRENT and MISSION_ITEM_REACHED as a place in the plan, the record of what is on the vehicle, the vehicle's verdict on a new mission, the fly-to payload checks and item specs | `src/protocol/missions.rs` (`corvus_link.missions`) | `corvus/mavlink_missions.py` keeps the bridge's methods and attribute names and the tunables the engine reads; fly to points, mission start (mode and arming order) and reposition stay here until the flight commands move |

| MAVLink FTP, the read side: open, burst read with the gaps asked for again, session reset, CRC32, terminate, one transfer at a time with its cancel | `src/protocol/ftp.rs` (`corvus_link.ftp`) | `corvus/mavlink_ftp.py` keeps the methods, the transfer slot `_ftp_lock`, `FtpError` and the tunables |
| On-board logs, the request side: list, data, erase (refused while armed), end | `src/protocol/logs.rs` (`corvus_link.logs`) | `corvus/mavlink_logs.py` keeps the methods and the reply sink |
| The PX4 NSH shell over SERIAL_CONTROL: console lines (CR dropped, backspaces applied, long lines flushed), the terminal's raw text and backlog, release on stop, UTF-8 split across chunks | `src/protocol/shell.rs` (`corvus_link.shell`) | `corvus/mavlink_shell.py` keeps the methods, `_shell_lock` and the limits |
| The Setup commands: sensor calibration, its cancel and ArduPilot's accelerometer positions, the bench motor test and its stop, the autotune (PX4 command with its progress ACKs, ArduPilot flight mode), the two reboots | `src/protocol/setup.rs` (`corvus_link.setup`) | `corvus/mavlink_setup.py` keeps the methods, the operation lock around each and the tunables |
| Remote ID: the identity coerced to what may go on air, the four OPEN_DRONE_ID messages at 1 Hz, the aircraft's ARM_STATUS verdict, the broadcast status | `src/protocol/remote_id.rs` (`corvus_link.remote_id`) | `corvus/mavlink_remote_id.py` keeps the methods; `corvus/remote_id.py` keeps the page's option lists and filing checks |
| Telemetry: BATTERY_STATUS detail and the battery fields with their latched cell count and drain series, GNSS_INTEGRITY (decoded and CRC checked from an UNKNOWN_441 frame too), the attitude and velocity setpoints, the RC channels, RADIO_STATUS, heartbeat jitter, link quality and the page stream rates | `src/protocol/telemetry.rs` (`corvus_link.telemetry`) | `corvus/mavlink_telemetry.py` keeps the methods and the operation lock around the stream requests; the battery estimator stays in `corvus/battery.py` |
| Flight commands: mode, arm and disarm, takeoff in each stack's order, land, RTL, set home, hold, the plugin command table, velocity setpoints, and the mission sequences (fly to points, mission start, reposition) with their abort checks between steps | `src/protocol/flight.rs` (`corvus_link.flight`) | `corvus/mavlink_flight.py` keeps the methods, the tunables and the priority lock around mode, disarm, land and RTL; `corvus/mavlink_missions.py` delegates its sequences |
| The bridge's receive path: the receive loop (degraded and lost heartbeats, read failures), the duplicate frame filter, the dispatcher with the aircraft's own state (heartbeat, position, attitude, GPS, battery, versions, home), the source checks, and the COMMAND_LONG / COMMAND_INT waits with their ACK registry | `src/bridge/receive.rs` (`corvus_link.bridge`) | `corvus/mavlink_bridge.py` keeps the methods, the state they read and write, the operation lock around the command waits |
| The connection cycle and its threads: start and stop, the connect/receive cycle with its reconnect backoff, connect (open, the first autopilot heartbeat, pinning the target, the dialect), the GCS heartbeat at 1 Hz with Remote ID on it, the deferred interval and version workers, all on Rust threads | `src/bridge/cycle.rs`, `src/bridge/thread.rs` (`corvus_link.bridge`) | `corvus/mavlink_bridge.py` keeps the methods, the lifecycle lock and the state; the parameter workers stay Python threads |

The link's threads are `CoreThread`s (`src/bridge/thread.rs`): native
threads that hold a Python thread state and call into Python with the GIL,
releasing it wherever Python does. A native thread must not be inside the
interpreter while it shuts down, so every one is registered, and an `atexit`
hook (registered when corvus_link is imported, so it runs after the
application's own teardown) stops the link loops and joins what is left. A
`CoreThread` handle exists only once its thread has started, and a stopped
one leaves no entry in `threading.enumerate()`.

`src/protocol/host.rs` holds what every engine asks of the bridge: logger,
command error, console, events, send lock, and Python's number conversions.

The parameter engine reads the clock as `time.monotonic` and `time.sleep` at call
time and reaches the link only through the bridge, so a test that replaces
either drives the Rust code exactly as it drove the Python. The mission
engine reads its tunables and `time` from `corvus.mavlink_missions` for the
same reason, and calls the bridge's own helpers (`_upload_mission`,
`_with_mission_home_slot`, ...) through the bridge, so a replaced one is the
one that runs.

## Link types

Built: `udpin:`, `udpout:`, `udpbcast:`, `udp:` (and a bare `host:port`),
`tcp:`, `tcpin:`, serial devices (`/dev/...`, `COMx`, `device,baud`),
`uds:` and `unix:` (POSIX), `mcast:`, a `.elf` or `/bin/` executable as a
child process (POSIX), and tlog and raw MAVLink log files.

Not built yet. They fail with `NotImplementedError` and a message that says
so, rather than falling through to the serial or UDP branch:

- `wsserver:`, `ws:`, `wss:`: MAVLink over WebSocket, server and client.
  Needs a WebSocket codec and, for `wss:`, TLS.
- `csv:`: pymavlink's CSV log reader.
- DataFlash logs: `*.bin`, `*.px4log`, and text `*.log` starting with `FMT,`
  (pymavlink's `DFReader`). Corvus reads ArduPilot `.bin` logs through its
  own log service.
- `recv_match(condition=...)`: pymavlink's expression conditions. Corvus
  never passes one.
- `mavmmaplog`'s per-type index: a tlog is read frame by frame and gives the
  same messages, without the seek-ahead.
- `~/.pymavlink/custom_mode_map.json`: pymavlink's user extension of the
  ArduPilot mode tables.

## Building

`./run.sh` and every build script run `pip install rust/corvus_link`, which
needs cargo on the build machine. After changing Rust code in a dev venv:

```bash
cd rust/corvus_link && maturin develop --release
```

## Proof that it behaves like pymavlink

pymavlink stays in the `test` dependency group as the reference:

- `tests/test_corvus_link_parity.py` compares every message definition and
  constant in both wire versions, encodes and decodes every message both ways,
  feeds both parsers the same damaged stream, and checks signing byte for
  byte.
- `tests/test_corvus_link_links.py` runs each link against a far end that
  speaks pymavlink, including serial over a pseudo-terminal, and feeds the
  same stream into pymavlink's `mavfile` and this one to compare the
  per-system state.
- `tests/test_param_engine_parity.py` runs the parameter protocol Corvus had
  in Python (kept in `tests/reference/`) and the Rust engine through the same
  PARAM_VALUE streams, downloads, writes, reads, verifies and uploads, and
  compares the cache, the progress, every notification, every message sent
  and every result.
- `tests/test_mission_engine_parity.py` does the same for the mission
  protocol: uploads (accepted, refused, rejected after the ACK, unanswered),
  downloads with lost and dropped items, MISSION_CURRENT streams, fences,
  clears and the fly-to checks, on PX4 and ArduPilot.
- `tests/test_ftp_logs_parity.py` does it for FTP (plain, lossy, unsized,
  busy, empty, missing, silent, aborted and cancelled reads, checksums,
  replies addressed elsewhere) and for the log requests.
- `tests/test_shell_parity.py` does it for the shell: console reassembly and
  terminal text over chunked streams with split characters, invalid bytes,
  carriage returns and backspaces, the backlog, switching owners, commands,
  writes, refusals and the release.
- `tests/test_setup_parity.py` does it for the Setup commands: every
  calibration on both stacks, cancels, accelerometer positions, motor tests
  with bad numbers, throttles and durations, the stop order, the PX4 and
  ArduPilot autotune, the autotune ACK stream and both reboots.
- `tests/test_remote_id_parity.py` does it for Remote ID: a few thousand
  random identities and every odd value in every field, the messages field
  for field, timestamps, arm statuses and whole broadcast sessions on v2,
  v1, missing and failing links.
- `tests/test_telemetry_parity.py` does it for telemetry: Python's
  `round()` and `sum()` against 45000 values, quaternions, thousands of
  intact and damaged GNSS_INTEGRITY frames, every handler on random and
  malformed messages with each store update compared, the battery
  fields and drain rate, jitter and link quality, and every stream request.
- `tests/test_flight_parity.py` does it for the flight commands: random
  sessions on PX4 and ArduPilot copters, planes and rovers against a
  scripted vehicle that accepts, refuses and confirms in telemetry now,
  late or never, with operator aborts landing between sequence steps and
  the link dropping.
- `tests/test_bridge_receive_parity.py` does it for the receive path:
  random frame streams from this vehicle, its other components and other
  systems through both dispatchers on one fake clock, every store update
  and handler call compared; the helpers, the duplicate filter, failing
  handlers, the receive loop's endings and every kind of ACK wait.
- `tests/test_bridge_cycle_parity.py` does it for the connection cycle:
  connect against scripted links and open failures, the run cycle with its
  backoff and supersession, the heartbeat wait, the GCS heartbeat loop, the
  sleep, both workers, stop's teardown order and a real start and stop.
- The rest of the suite drives the bridge with corvus_link's messages.
