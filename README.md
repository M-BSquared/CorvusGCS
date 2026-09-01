# CORVUS GCS

A modern, desktop-first Ground Control Station for PX4-based autonomous
aircraft. Dark operator-oriented interface with a large satellite map as
the visual center, a floating flight-instrument overlay, and a collapsible
right-side engineering workspace (MAVLink console, SSH, plugin slot).

![CORVUS GCS](assets/CorvusGCS.png)

---

## Requirements

- **Python 3.10+**
- **pymavlink** — MAVLink communication (`pip install pymavlink`)
- **paramiko** — SSH access (`pip install paramiko`)
- **pyserial** — serial connections (`pip install pyserial`)
- A modern browser with WebGL support (Chromium, Firefox, Edge)

Install all dependencies:

```bash
pip install pymavlink paramiko pyserial
```

The frontend loads MapLibre GL JS, Lucide icons, and Google Fonts from
CDNs, so an internet connection is needed for map tiles and icons.

---

## Quick start

### Standalone desktop app (recommended)

```bash
# one command — creates conda env, installs deps, launches standalone window
./run.sh
```

This builds the conda environment from `environment.yml` and launches
`corvus/app.py` — a PyQt6 + QtWebEngine standalone window. No browser
needed. Chromium rendering flags handle GBM/Vulkan fallback automatically.

```bash
# custom MAVLink connection
./run.sh 8000 serial:/dev/ttyUSB0:57600
```

### Browser mode (development)

```bash
# start only the backend server, open in browser manually
python3 serve.py
# -> http://localhost:8000/
```

### AppImage (Linux x86_64)

A single self-contained `.AppImage` that runs on a clean Ubuntu/Debian
x86_64 install with no system Python or Qt needed — the easiest way to
ship the app to the offline field laptop. The build host needs
Ubuntu/Debian x86_64, `python3` (3.10+), `pip`, `wget` or `curl`, and
roughly 1 GB of free disk for the build cache. No conda required.

```bash
# one command — builds a self-contained Linux x86_64 AppImage
./build-appimage.sh
```

The script writes `Corvus_GCS-<version>-x86_64.AppImage` to the repo
root, where `<version>` is read from the `VERSION` file (see
[Version control](#version-control)). On the field laptop:

```bash
chmod +x Corvus_GCS-*-x86_64.AppImage
./Corvus_GCS-*-x86_64.AppImage
# optional MAVLink connection:
./Corvus_GCS-*-x86_64.AppImage 8000 serial:/dev/ttyUSB0:57600
```

The first run downloads `appimagetool` and the Python wheels (PyQt6 /
QtWebEngine, pymavlink, paramiko, pyserial); `appimagetool` is cached
under `build/` for fast re-runs. The AppImage bundles the app version
from `VERSION`, so to release a new version you only bump `VERSION` and
rebuild. Build artifacts (`build/`, `*.AppImage`) are gitignored.

---

## Connecting to a drone

### PX4 SITL (simulated)

If PX4 SITL is running (default UDP 14540), Corvus GCS connects
automatically on startup and starts receiving live telemetry:

```bash
# in PX4-Autopilot directory:
make px4_sitl
# SITL broadcasts MAVLink to 127.0.0.1:14540
```

### Real autopilot

```bash
# serial connection (USB telemetry radio)
python3 serve.py 8000 serial:/dev/ttyUSB0:57600

# UDP connection (WiFi telemetry)
python3 serve.py 8000 udp:192.168.2.10:14550
```

For field use, the recommended way to connect is the **LINK** tab in the
right-side panel rather than a command-line connection string. The operator
picks a serial port from a dropdown (populated live from the backend),
chooses a baud (57600 by default, labeled for the SiK Radio V3), or enters a
custom UDP/TCP connection string such as `udp:0.0.0.0:14540` for PX4 SITL.

#### Holybro SiK Telemetry Radio V3

A USB serial radio. On Linux it enumerates as two devices, e.g.
`/dev/ttyUSB0` and `/dev/ttyUSB1`; the lower-numbered one is the MAVLink
data port.

- **Device path:** `/dev/ttyUSB0` (the lower of the two `ttyUSB` devices)
- **Baud rate:** 57600 (factory default)
- **Connection string:** `serial:/dev/ttyUSB0:57600`

On serial links Corvus GCS automatically applies conservative MAVLink
stream rates so the 57 kbps radio link is not saturated, uses a longer
heartbeat timeout (10 s), and reconnects with backoff if the link drops.

### Connection states

| State | Indicator |
|-------|-----------|
| Connecting | Yellow dot |
| Connected | Green dot |
| Disconnected | Gray/red dot |
| Reconnecting | Yellow dot, last error shown |
| Armed | Green "ARMED" |
| Disarmed | Gray "DISARMED" |

---

## Plugins (FUTURE tab)

The **FUTURE** tab in the right-side panel is the extension point of
Corvus GCS. It shows a grid of plugin cards; clicking a card opens that
plugin's view with a back button, and opening another plugin first
closes the current one so its teardown runs exactly once (no listener
leaks). The first shipped plugin is the **Vibration Monitor**.

### Vibration Monitor

A live Plotly line graph of the PX4 `VIBRATION` message, plus a stats
row of cumulative accelerometer-clipping counters (3 counters). When the
plugin opens it asks PX4 to stream `VIBRATION` at ~10 Hz **on demand**;
closing the plugin restores PX4's default rate. High-rate vibration data
flows only while someone is watching, so the serial link stays
uncongested the rest of the time — the lean, field-first pattern used
throughout Corvus.

The graph shows three traces:

| Trace | `VIBRATION` field | Color | Meaning |
|-------|-------------------|-------|---------|
| Gyro coning | `vibration_x` | Blue | Gyro delta-angle coning metric |
| Gyro HF vibration | `vibration_y` | Yellow | Gyro high-frequency vibration |
| Accel HF vibration | `vibration_z` | Red | Accelerometer high-frequency vibration — the main mechanical-health indicator |

**PX4-specific semantics** (verified from PX4 source
`src/modules/mavlink/streams/VIBRATION.hpp`): PX4 repurposes the three
standard MAVLink `VIBRATION` fields — `vibration_x` is a gyro delta-angle
coning metric, `vibration_y` is gyro high-frequency vibration, and
`vibration_z` is accelerometer high-frequency vibration. A rising
**accel-HF (red)** trace means more mechanical vibration — suspect motor
imbalance, a loose or chipped prop, or worn bearings. Operators should
read the red trace first as the mechanical-health indicator.

On-demand streaming uses `MAV_CMD_SET_MESSAGE_INTERVAL`, PX4's standard
per-message rate control (works on v1.16–v1.18). Plotly is **vendored
locally** at `src/vendor/plotly-basic.min.js`, so the graph renders with
no internet connection — matching the offline field-use requirement.

---

## Setup page

The Setup page (left nav) holds the parameter editor and the
calibration / autotune tiles. All three features are **lazy** and
**armed-safe**: nothing is fetched until the operator opens a tile, and
every parameter write, calibration, and autotune is refused while the
vehicle is armed.

### Parameters

Parameters are downloaded **lazily** — only when the operator opens the
**Parameters** tile. Until then Corvus streams only the telemetry needed
to fly (GPS, attitude, etc.), keeping the GCS lean and fast to
ready-for-flight. The download shows a live progress indicator, and the
editor is usable only after the full parameter set has arrived (the
operator must wait for completion). The editor lets the operator change
any parameter; writes go through `PARAM_SET` and are confirmed by a
`PARAM_VALUE` echo. Parameter writes are **refused while armed** (safety).

### Sensor calibration

Setup → **Calibration** tile: one-tap sensor calibration for:

- Compass (magnetometer)
- Gyroscope
- Accelerometer
- Level Horizon
- Airspeed
- Baro

Calibration is **refused while armed**. PX4 also rejects calibration
when armed, but Corvus refuses client-side first. During interactive
calibrations (compass rotation, accelerometer positions) PX4 streams
step-by-step guidance as `STATUSTEXT`, which appears in the MAVLink
console and the warnings popover.

### Autotune

Setup → **Calibration** tile → **POD Tuning** subsection: PX4 autotune
via `MAV_CMD_DO_AUTOTUNE_ENABLE`. The operator picks an axis — **Roll,
Pitch, Yaw, or All**. Autotune tunes the rate and attitude controllers
together (PX4 module `mc_autotune_attitude_control`). Autotune is
**refused while armed**, and tuning progress streams as `STATUSTEXT`.

> **No velocity-controller autotune in PX4.** PX4 provides only rate +
> attitude autotune — there is no velocity-controller autotune. The UI
> marks the velocity controller as unsupported rather than sending a
> command the firmware does not understand.

The autotune graphs (roll rate, roll attitude, horizontal velocity) use
Plotly. These traces come from existing telemetry that streams
continuously (not on-demand like vibration): roll rate from the
`ATTITUDE` body rate `rollspeed`, roll attitude from `ATTITUDE` roll,
and horizontal velocity from `VFR_HUD` groundspeed /
`GLOBAL_POSITION_INT`.

Body angular rates (`rollspeed`, `pitchspeed`, `yawspeed`, in deg/s)
are now part of the telemetry state.

---

## Architecture

```
serve.py                     # browser-mode entry point
run.sh                       # standalone app launcher (conda env + PyQt6)
build-appimage.sh            # build a self-contained Linux x86_64 AppImage
├── corvus/                  # Python backend package
│   ├── app.py               # standalone PyQt6 + QtWebEngine app wrapper
│   ├── version.py           # reads VERSION file (single source of truth)
│   ├── state_store.py       # thread-safe Vehicle State Store
│   ├── mavlink_bridge.py    # pymavlink connection, message parsing
│   ├── ssh_bridge.py        # paramiko SSH sessions
│   └── server.py            # HTTP + SSE + API server
├── src/                     # web frontend
│   ├── index.html
│   ├── css/main.css
│   ├── js/
│       ├── telemetry.js     # SSE client for real telemetry
│       ├── map.js           # MapLibre map, vehicle tracking
│       ├── instruments.js   # compass + attitude indicator
│       ├── link.js          # LINK tab (serial/UDP/TCP connect)
│       ├── panel.js         # MAVLink console + SSH
│       ├── plugins.js       # FUTURE-tab plugin system (extension point)
│       ├── plugin-vibration.js  # Vibration Monitor plugin
│       └── app.js           # top bar, left nav, wiring
│   └── vendor/
│       └── plotly-basic.min.js  # vendored Plotly (offline graphs)
├── assets/                  # logo artwork
└── VERSION                  # single-source version string
```

### Data flow

```
PX4 Autopilot → MAVLink (UDP/Serial) → pymavlink bridge → Vehicle State Store
                                                                    ↓
Web browser ← SSE (/api/telemetry) ← HTTP server ← (thread-safe)
```

Telemetry is pushed from backend to frontend via **Server-Sent Events**
(SSE). The frontend never polls.

---

## API endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/version` | GCS version + PX4 profile |
| GET | `/api/state` | Current vehicle state (JSON) |
| GET | `/api/telemetry` | SSE stream of vehicle state |
| GET | `/api/console/stream` | SSE stream of MAVLink messages |
| POST | `/api/console/command` | Send MAVLink command |
| POST | `/api/mavlink/connect` | Reconnect with different connection string |
| GET | `/api/mavlink/serial-ports` | List available serial ports → `{"ports": [{"device", "description", "hwid"}, ...]}` |
| POST | `/api/mavlink/arm` | Arm/disarm vehicle |
| POST | `/api/mavlink/mode` | Set flight mode |
| POST | `/api/params/download` | Start a full parameter download (lazy) |
| GET | `/api/params` | Parameter cache + download status (params only sent when complete — lean) |
| GET | `/api/params/progress` | SSE stream of parameter download progress |
| POST | `/api/params/set` | Write a parameter (refused while armed) |
| POST | `/api/calibrate` | Run a sensor calibration `{type: gyro\|compass\|baro\|accel\|level\|airspeed}` (refused while armed) |
| POST | `/api/autotune` | Run PX4 autotune `{axis: roll\|pitch\|yaw\|all}` (refused while armed) |
| POST | `/api/vibration/stream` | Request high-rate VIBRATION streaming on demand `{enabled, rate_hz}` (lean: restore default on close) |
| POST | `/api/ssh/connect` | Open SSH session |
| POST | `/api/ssh/send` | Send input to SSH shell |
| POST | `/api/ssh/disconnect` | Close SSH session |
| GET | `/api/ssh/stream` | SSE stream of SSH output |
| GET | `/api/ssh/sessions` | List SSH sessions |

### Console commands

In the MAVLink console, type:

```
help        — list commands
arm         — arm the vehicle
disarm      — disarm the vehicle
mode AUTO   — set flight mode
takeoff 10  — takeoff to 10m
land        — land at current position
rtl         — return to launch
```

---

## Color system

The UI follows a strict semantic palette (defined in `src/css/main.css`):

- **Green** `#45D483` — healthy / connected / armed
- **Blue** `#4CC9FF` — flight / navigation / mission
- **Forest Green** `#3DA876` — UI interaction accent (not status)
- **Yellow** `#F5C842` — warning
- **Red** `#FF514D` — critical / error

---

## Version control

The version string lives in `VERSION` at the repo root. `corvus/version.py`
reads it at import time. The HTTP server exposes it at `GET /api/version`.
No component hardcodes a version literal. See `AGENTS.md` for the full
version policy.

---

## PX4 compatibility

Primary target: **PX4 v1.16, v1.17, v1.18.** The MAVLink bridge detects
the firmware via `AUTOPILOT_VERSION` and adapts. See `AGENTS.md` for the
full compatibility policy.
