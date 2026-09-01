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

## Architecture

```
serve.py                     # browser-mode entry point
run.sh                       # standalone app launcher (conda env + PyQt6)
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
│   └── js/
│       ├── telemetry.js     # SSE client for real telemetry
│       ├── map.js           # MapLibre map, vehicle tracking
│       ├── instruments.js   # compass + attitude indicator
│       ├── panel.js         # MAVLink console + SSH + future
│       └── app.js           # top bar, left nav, wiring
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
