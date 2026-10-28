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
| The links | `src/link/transport.rs`, `src/link/serial_posix.rs`, `src/link/websocket.rs` | `mavudp`, `mavtcp`, `mavtcpin`, `mavserial`, `mavuds`, `mavmcast`, `mavchildexec`, `mavlogfile`, `mavwebsocket`, `mavwebsocket_client` |
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
| What the vehicle says in words: STATUSTEXT reassembled from its chunks and published (console, warning board, routine chatter left out), the kill switch latched and released from its line, from the preflight check that names it or from a ready verdict or whole report after it, and the preflight report requests | `src/bridge/status.rs` (`corvus_link.bridge`) | `corvus/mavlink_bridge.py` keeps the methods, the state and the console fan out; which line means what is the dialect's |
| The bridge's own requests and signing: the stream rate tables, the interval batch with its REQUEST_DATA_STREAM fallback (UNSUPPORTED, silence, proof of the modern path), the interval command, home and version requests, the ArduPilot stick authority warning, the signing key (read through Python's `os`, owner only, no symlink, binary mode) and the three messages exempt from it | `src/bridge/requests.rs` (`corvus_link.bridge`) | `corvus/mavlink_bridge.py` keeps the methods and the operation lock around the interval command; `corvus/mavlink_signing.py` the environment variable |
| The telemetry log: the writer (bounded drop-oldest queue, a writer thread, the header line, batches, a full disk reported once and the file still closed) and the bridge's session (file names that never collide, a file per connection and per flight, every frame to it and to the forwarder) | `src/bridge/tlog.rs` (`corvus_link.bridge`) | `corvus/tlog.py` keeps `TlogWriter` with its file, queue and locks; `corvus/mavlink_bridge.py` the session methods |
| The connection cycle and its threads: start and stop, the connect/receive cycle with its reconnect backoff, connect (open, the first autopilot heartbeat, pinning the target, the dialect), the GCS heartbeat at 1 Hz with Remote ID on it, the deferred interval and version workers, all on Rust threads | `src/bridge/cycle.rs`, `src/bridge/thread.rs` (`corvus_link.bridge`) | `corvus/mavlink_bridge.py` keeps the methods, the lifecycle lock and the state |
| The vehicle list's tracker: one bound autopilot per link (a companion or station heartbeat on its id ignored), its position, heading, speed, altitude, battery, mode and arm state, its warnings as events, stale after 5 s and forgotten after 10 min; the fleet's publish thread on a `CoreThread` | `src/bridge/fleet.rs` (`corvus_link.bridge`) | `corvus/fleet.py` keeps `FleetTracker` with its lock and state, and the `Fleet` manager (links, names, colours, config, the HTTP proxies) |
| The second station forwarder: every frame mirrored to UDP endpoints through a drop-oldest queue, inbound datagrams split into whole frames, a peer learned only on a whole frame, commanding only from configured endpoints, never a frame under Corvus' id, never a mirror into the vehicle link's own socket, name resolution off the send path, and both threads on `CoreThread`s | `src/bridge/forward.rs` (`corvus_link.bridge`) | `corvus/mavlink_forwarder.py` keeps `MavlinkForwarder` with its socket (Python's) and state |
| Auto-connect: the port classification and pick (USB before SiK, by device name, never a bootloader or a phantom), the startup resolver (command line, USB, SiK, configured, UDP fallback), the mid-session suggestion, and the watcher: one dial per appearance while idle, a suggestion while busy, a manual connect winning the session, a link followed to the name its device came back under, the bootloader notice; its thread on a `CoreThread` | `src/bridge/autoconnect.rs` (`corvus_link.bridge`) | `corvus/autoconnect.py` keeps the data classes, `SessionState` with its lock, and the watcher's store, bridge and busy list readers |
| RTK corrections: RTCM 3 framing with CRC-24Q, GPS_RTCM_DATA fragments, every UBX message that makes a u-blox receiver a base (F9 CFG-VALSET and M8P CFG-TMODE3), the NAV-SVIN and MON-VER reports, the settings resolver; the NTRIP client (request with Basic auth, the response header, the stream over its own TCP socket); the base's serial port (corvus_link's own, opened straight at its baud rate); the session (search, identify, survey, inject, recover, undo on the way out) on a `CoreThread` named `rtk-base`; and the bridge's injection with its counters | `src/rtk/` (`corvus_link.rtk`), `src/bridge/rtcm.rs` | `corvus/rtk.py` keeps the constants, `corvus/rtk_service.py` the `RtkService` object with its lock and state |
| SiK radio configuration: reading a radio's `ATI5`/`ATI7`/`ATI` replies, the baud and RSSI conversions, comparing the two ends and the copy to the remote, validating a write (enum, range, whole numbers, an inverted band), the AT/RT write plan; and the AT dialogue itself: `+++` with its guard times, a reply ended by silence, the baud scan, reading, applying and factory reset, and handing the port and the MAVLink bridge over and back around a session. The serial port is corvus_link's own, opened straight at the radio's baud rate | `src/sik.rs` (`corvus_link.sik`) | `corvus/sik_config.py` keeps the register table, labels and hints and the page description; `corvus/sik_service.py` keeps `SikSession`, `SikService` (gate, status, load, save, reset) and the bridge restart |
| ExpressLRS telemetry over CRSF (dormant: switched off and imported by nothing): the CRC-8/DVB-S2 checksum, the frame builder and the resynchronising parser, the decoders for GPS, vario, battery, barometric altitude, link statistics, attitude and flight mode, the thread-safe telemetry snapshot with its stale position, the reader on a `CoreThread` named `corvus-elrs`, and the fly-to planner for an ELRS link | `src/elrs.rs` (`corvus_link.elrs`) | `corvus/elrs.py` keeps the switch, the constants, `CrsfFrame`, `FlyToPlan`, `ElrsTelemetry` and `ElrsTelemetryReader` with their locks and state |
| Firmware flashing: the PX4 bootloader protocol (sync, board and flash size, the board compatibility check before anything is erased, erase, 252 byte blocks, the CRC check over the padded flash, reboot only after a clean verify; a failure or cancel never reboots) with the bootloader CRC-32; and the flash service: the gates (USB link, disarmed, idle, or exactly one board waiting in its bootloader), the upload and the download-then-flash workers on `CoreThread`s, the bridge stopped and restarted around them, cancel and shutdown | `src/flash.rs` (`corvus_link.flash`) | `corvus/firmware_uploader.py` keeps the image container parsing (JSON, ZIP, raw) and the progress state; `corvus/flash_service.py` keeps the status, the listeners and the state; the release catalogues (`firmware_catalog.py`, `ardupilot_firmware.py`) stay Python as they only fetch release lists |
| The parameter workers (batch upload, download watchdog, metadata download) on `CoreThread`s through `start_thread` | `src/bridge/thread.rs` | `corvus/mavlink_params.py` and `corvus/param_metadata.py` keep the work itself; the metadata file is still decompressed and parsed in Python |

The link's threads are `CoreThread`s (`src/bridge/thread.rs`): native
threads that hold a Python thread state and call into Python with the GIL,
releasing it wherever Python does. A native thread must not be inside the
interpreter while it shuts down, so every one is registered, and an `atexit`
hook (registered when corvus_link is imported, so it runs after the
application's own teardown) stops the link loops and joins what is left. A
`CoreThread` handle exists only once its thread has started and is listed
in `threading.enumerate()` under its name, as `threading.Thread.start`
promises, and a stopped one leaves no entry there.

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
child process (POSIX), tlog and raw MAVLink log files, and MAVLink over
WebSocket: `ws:host:port[:resource]`, `wss:host:port[:resource]` and
`wsserver:host:port`; and CSV files as `csv:path[:option=value...]`.

The WebSocket links carry MAVLink in binary messages; a text frame is
ignored. The client reconnects when the connection is reset and tries again
on every call while it has none. The server takes one client at a time and
returns to accepting when it leaves. A resource without a leading `/` gets
one (pymavlink sent it as written, which is not a valid request line).
`wss:` trusts the system's root certificates and the file `SSL_CERT_FILE`
names (as Python does), and checks the host name. rustls is stricter than
OpenSSL about one thing: a certificate that is its own CA and also the
server's is refused as a server certificate ("CaUsedAsEndEntity"), so use a
CA and a server certificate it signed. Building with
`--no-default-features` leaves TLS (and its C compiler requirement) out; a
`wss:` link then raises `NotImplementedError`. The default build has TLS.

`csv:` reads a CSV file as messages (pymavlink's `CSVReader`): the first row
names the columns, every later row is a message of type `CSV` (the
`message_type` option renames it), numbers are numbers and everything else
stays text, and the first column is the timestamp in microseconds. Options
are `separator` (`;` by default, `separator=:` for a colon, the
`CSV_SEPARATOR` environment variable overrides it) and `message_type`. Rows
are cut as Python's `csv.reader` cuts them. The `timestamp_expression`
option and `recv_match` conditions need pymavlink's expression evaluator and
raise `NotImplementedError`.

Dependencies added for this: `tungstenite` (the WebSocket protocol, with
only its handshake feature), `rustls` with the `ring` provider, and
`rustls-native-certs`. The standard library has neither a WebSocket codec nor
TLS.

Not built yet. They fail with `NotImplementedError` and a message that says
so, rather than falling through to the serial or UDP branch:

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
- `tests/test_corvus_link_websocket.py` runs `ws:`, `wss:` and `wsserver:`
  through pymavlink (with wsproto) and through corvus_link against a scripted
  wsproto server and client: binary messages both ways, split messages, text
  frames, a ping, a close, a reset and the reconnect, a missing server, a
  second client after the first left, bad device strings, and `wss:` against
  a TLS server with a throwaway CA (refused untrusted, refused for the wrong
  host name, carrying data once trusted).
- `tests/test_corvus_link_csv.py` reads random CSV files (quoted and
  multi-line fields, blank lines, CRLF and CR, a byte order mark, an open
  quote at the end, every separator, every spelling of a number) and 3000
  strings of raw quotes and line ends through pymavlink's `CSVReader` and
  corvus_link, and compares the rows, the messages, the count, the rewind,
  every `recv_match` form, the device options and the ways opening fails.
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
- `tests/test_bridge_status_parity.py` does it for STATUSTEXT and the kill
  switch: random sessions of whole, chunked, reordered, partial, broken and
  odd lines from several sources with SYS_STATUS verdicts in between, on
  PX4 and ArduPilot, and the partial eviction and the report requests.
- `tests/test_bridge_requests_parity.py` does it for the requests and
  signing: key files of every kind (raw, hex, short, zero, a symlink, a
  directory, the wrong mode), applying signing on MAVLink 1 and 2 links,
  the rate tables in order, the legacy and interval requests with scripted
  answers, the interval command, the authority check, home and version.
- `tests/test_fleet_parity.py` does it for the vehicle list's tracker:
  random streams of heartbeats, positions, attitude, speed, warnings and
  battery from the autopilot and other components, binds and forgets,
  the vehicle and its events compared at every step, and the publish
  thread.
- `tests/test_forwarder_parity.py` does it for the forwarder: the frame
  splitter on random byte runs, endpoint parsing, the static list and the
  vehicle link loop check, name resolution, and a live UDP exchange with a
  station sending whole, split, junk and Corvus-id frames from a configured
  and an unknown address, commanding on and off.
- `tests/test_autoconnect_parity.py` does it for auto-connect: the policy
  on random port lists (classification, pick, startup resolver, suggestion,
  baud and validity), random watcher stories (devices plugged, pulled,
  replugged under another name and in the bootloader, the link busy and
  idle, a manual connect, dials refused and failing) compared tick by tick
  with every store write and log line, and the watcher thread.
- `tests/test_rtk_parity.py` does it for RTK: RTCM and UBX streams cut at
  random, fragments of every length, every UBX builder with odd numbers,
  MON-VER and NAV-SVIN payloads, random config blocks, NTRIP requests and
  scripted caster answers, a real local caster over TCP, serial base
  acquisition against scripted ports, whole sessions on both receiver
  generations on a fake clock, a session on its thread, and the injection
  with its counters and logs.
- `tests/test_sik_parity.py` does it for SiK: the parsers on random radio
  output (unicode digits and broken lines too), the conversions, comparing
  and copying between random radios, validating random write requests with
  odd values, write plans, and whole AT conversations (load, save, reset,
  baud scan, refused commands, silent or failing ports, the bridge stopped
  and restarted around them, armed or not) on a fake clock with every
  command, log line and bridge event compared.
- `tests/test_elrs_parity.py` does it for ExpressLRS: the checksum and the
  frame builder with odd arguments, the parser on random streams cut at
  random, every decoder on payloads of every length, the telemetry
  snapshot over random frame streams on a fake clock, the reader thread
  against scripted sources, and the fly-to planner for every link mode,
  stack and point.
- `tests/test_flash_parity.py` does it for flashing: the bootloader CRC on
  random data and odd arguments, whole uploads against a scripted
  bootloader that fails in every way (silent or refused steps, bad silicon, a
  wrong board, a failing erase or block, a wrong or missing CRC, failing
  writes, reads and reboot, a cancel at any step) with every byte written and
  every log line compared, and the service on its worker threads (gates,
  upload, catalogue download, a board already in its bootloader, cancel,
  shutdown, the bridge stopped and restarted).
- `tests/test_param_threads_parity.py` does it for the parameter workers:
  uploads with valid and broken lists, armed and disarmed, a second upload
  while one runs, the watchdog with the bridge running or not, and the
  metadata download on both stacks.
- `tests/test_tlog_parity.py` does it for the tlog: the files both writers
  produce byte for byte, a full disk on write, flush and header, the
  drop-oldest queue, and the bridge's sessions, file names and an
  unusable log directory.
- `tests/test_bridge_cycle_parity.py` does it for the connection cycle:
  connect against scripted links and open failures, the run cycle with its
  backoff and supersession, the heartbeat wait, the GCS heartbeat loop, the
  sleep, both workers, stop's teardown order and a real start and stop.
- The rest of the suite drives the bridge with corvus_link's messages.
