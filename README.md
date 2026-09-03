<p align="center">
  <img src="assets/CorvusGCS_logo.png" width="128" alt="Corvus GCS logo">
</p>

<h1 align="center">Corvus GCS (CGCS)</h1>

<p align="center">
  <em>A modern, offline-first Ground Control Station for PX4 autonomous aircraft — built for the field: a laptop, a telemetry radio, and no internet.</em>
</p>

<div align="center">
  <a href="https://www.python.org/" target="_blank"><img src="https://img.shields.io/badge/Python_3.10%2B-3776AB?logo=python&logoColor=white&style=for-the-badge" height="30" alt="Python 3.10+" /></a>
  <img width="12" />
  <a href="https://github.com/ArduPilot/pymavlink" target="_blank"><img src="https://img.shields.io/badge/pymavlink-00A6E2?logoColor=white&style=for-the-badge" height="30" alt="pymavlink" /></a>
  <img width="12" />
  <a href="https://px4.io/" target="_blank"><img src="https://img.shields.io/badge/PX4-v1.16%20%7C%201.17%20%7C%201.18-00C7B7?logoColor=white&style=for-the-badge" height="30" alt="PX4 v1.16 | 1.17 | 1.18" /></a>
  <br>
  <a href="https://git.unibw.de/l11bmabo/CorvusGCS" target="_blank"><img src="https://img.shields.io/badge/Vibecoded-5B2C6F?style=for-the-badge" height="30" alt="Vibecoded" /></a>
  <br>
  <img src="https://img.shields.io/badge/License-TBD-lightgrey?style=for-the-badge" height="30" alt="License: TBD" />
</div>

<p align="center">
  Self-hosted GitLab: <a href="https://git.unibw.de/l11bmabo/CorvusGCS">git.unibw.de/l11bmabo/CorvusGCS</a>
</p>

<details>
<summary><strong>Version note</strong> (why there is no version badge)</summary>

The version is intentionally **not** shown as a badge. The repository is hosted on a
self-hosted GitLab at `git.unibw.de` whose project API is not publicly readable, so a
dynamic shields.io version badge would render as an error rather than a number.
The canonical version string lives in the [`VERSION`](VERSION) file at the repo root
and is served live by the running app at `GET /api/version`. No version literal is
ever typed into this README.

</details>

<p align="center">
  <img src="assets/CorvusGCS.png" alt="Corvus GCS — operator interface with map, floating HUD, and right-side engineering workspace">
</p>

---

## Contents

- [What is Corvus GCS?](#what-is-corvus-gcs)
- [Screenshots & imagery](#screenshots--imagery)
- [Quick start](#quick-start)
- [Connecting to a drone](#connecting-to-a-drone)
- [Plugins — Vibration Monitor](#plugins--vibration-monitor)
- [Setup — Parameters, Calibration, Autotune, Firmware](#setup--parameters-calibration-autotune-firmware)
- [Offline map — named areas](#offline-map--named-areas)
- [Settings — Appearance, SSH, Map](#settings--appearance-ssh-map)
- [Architecture](#architecture)
- [API endpoints](#api-endpoints)
- [Console commands](#console-commands)
- [Color system](#color-system)
- [Version control](#version-control)
- [PX4 compatibility](#px4-compatibility)
- [About / Origins](#about--origins)

---

## What is Corvus GCS?

Corvus GCS (CGCS) is a modern, **offline-capable Ground Control Station for
PX4-based autonomous aircraft**. It is a Python backend plus a web frontend,
wrapped into a single standalone desktop app — designed first for the person
standing next to an aircraft on a flight line, not for a desk.

The interface is **dark and operator-oriented**. The visual center is a **large
map** that tracks the vehicle in real time, with a choice of **five base
layers** — OpenStreetMap plus ESRI Satellite, Hybrid, Topographic, and Streets.
Overlaid on the map is a
**floating flight-instrument HUD** — a compass, an attitude indicator, and the
telemetry numbers that matter while you fly, always visible. On the right side,
a **collapsible engineering workspace** holds the tools you reach for between
flights: a **MAVLink console**, an **SSH terminal**, and a **plugin slot** for
specialist views.

From one window you can:

- **Fly** — live HUD, map, attitude, GPS, and body angular rates pushed from the
  autopilot over Server-Sent Events (the frontend never polls).
- **Read the map** — four map services (Esri, OpenStreetMap, Google, Bing) with
  twelve base layers between them, per-source attribution, and a
  download-a-region dialog for fully offline field use. Downloaded areas are
  **named**, listed, and drawn on the map, so "what do I have offline?" has an
  answer you can act on. The selected service and layer are saved to config and
  restored on the next launch. MapLibre GL JS is bundled locally, so the map
  renders with no internet.
- **Connect** — serial, UDP, or TCP telemetry radios, with a live serial-port
  picker and sensible defaults for the Holybro SiK Radio V3.
- **Tune parameters** — a lazy, armed-safe parameter editor; the full set is
  fetched only when you open it.
- **Calibrate sensors** — one-tap compass, gyro, accelerometer, level-horizon,
  airspeed, and baro calibration, with live PX4 step guidance.
- **Calibrate motors (ESC)** — PX4 motor/ESC calibration behind a safety-confirm
  modal (remove propellers first; motors spin at max PWM), with live STATUSTEXT
  guidance. Refused while armed.
- **Autotune** — PX4 rate + attitude autotune per axis (roll / pitch / yaw / all),
  with live progress and graphs.
- **Flash firmware** — flash PX4 firmware to the flight controller from the
  Setup page, over a **direct USB connection only** (`/dev/ttyACM*`); refused
  over a SiK radio (`/dev/ttyUSB*`) or UDP / TCP, and refused while armed.
- **Diagnose vibration** — a Vibration Monitor plugin with a live Plotly graph of
  the PX4 `VIBRATION` message and cumulative clipping counters.
- **Reach the companion** — an SSH terminal for an onboard companion computer,
  over the same link. Connections are **saved by name** in config (add and
  remove from the SSH tab or Settings), with password **or** key-file auth.
- **Extend** — a plugin system (the **TOOLS** tab, "Tools & Plugins") where new
  specialist views plug in without touching the core.
- **Configure** — an editable **Settings** page (left nav → SET): five
  predefined color themes (including a full light theme), the map service to
  use (Esri, OpenStreetMap, Google, Bing) and which of its layers, saved SSH
  connections, and the live MAVLink/HTTP connection summary. Everything applies
  instantly and is persisted. See [Settings](#settings--appearance-ssh-map).

### The field-use case

The driving constraint is the field: a laptop with **no internet**, a
**serial/UDP/TCP telemetry radio**, and a **clean shutdown between flights**.
Corvus GCS is built around that:

- The core ground station — telemetry, HUD, map, parameters, calibration,
  autotune, and the vibration monitor — runs **fully offline**. Both Plotly and
  MapLibre GL JS are **vendored locally** (`src/vendor/plotly-basic.min.js` and
  `src/vendor/maplibre-gl.min.js` + `maplibre-gl.css`), so the map and the graphs
  render with no connection.
- On a serial link it automatically applies conservative MAVLink stream rates so
  a 57 kbps radio isn't saturated, uses a longer heartbeat timeout (10 s), and
  reconnects with backoff if the link drops.
- The app **shuts down cleanly every time**: every child process is terminated
  (`SIGTERM` → timeout → `SIGKILL`), every daemon thread is joined, and file
  handles are flushed and closed — no zombies, no leaked sockets, no unflushed
  logs. The field laptop is rebooted between flights, and the app must disappear
  perfectly on `SIGINT` / `SIGTERM` / `atexit`.

> **Map tiles, fonts & icons (honest caveat):** the map *library* now renders
> fully offline — MapLibre GL JS is **vendored locally** at
> `src/vendor/maplibre-gl.min.js`. The **map tiles** themselves (the imagery from
> ESRI / OpenStreetMap) still come from the internet, but all tile traffic is
> routed through the backend (`/api/tiles/...`), which fetches and caches into the
> offline **MBTiles (SQLite) tile-cache store** in `corvus/tile_cache.py` —
> covering every registered source. Use the on-map "Download offline map" button
> to cache a **named area**, and the map then works fully offline. The last remaining CDN
> dependencies are **Lucide icons** (`unpkg`) and **Google Fonts** (Inter /
> JetBrains Mono); offline, icons are absent (most controls keep a text label)
> and fonts fall back to system defaults.

---

## Screenshots & imagery

<p align="center">
  <img src="assets/CorvusGCS.png" alt="Corvus GCS operator interface">
</p>

<p align="center"><em>The operator interface: dark, map-centered, with the floating HUD and the collapsible right-side engineering workspace (MAVLink console, SSH, plugin slot).</em></p>

<p align="center">
  <img src="assets/CorvusGCS_logo.png" width="160" alt="Corvus GCS logo">
</p>

<p align="center"><em>Corvus GCS project logo.</em></p>

Drop additional screenshots into `assets/` and wire them in here. Keep image
paths relative (`assets/...`) so they render on both GitLab and GitHub.

---

## Quick start

### Requirements

- **Python 3.10+**
- **pymavlink** — MAVLink communication
- **paramiko** — SSH access
- **pyserial** — serial connections
- A modern browser with WebGL support (Chromium, Firefox, Edge)

```bash
pip install pymavlink paramiko pyserial
```

### Standalone desktop app (recommended)

```bash
# one command — creates conda env, installs deps, launches standalone window
./run.sh
```

This builds the conda environment from `environment.yml` and launches
`corvus/app.py` — a PyQt6 + QtWebEngine standalone window. No browser needed.
Chromium rendering flags handle GBM/Vulkan fallback automatically.

```bash
# custom MAVLink connection
./run.sh 8000 serial:/dev/ttyUSB0:57600
```

To launch again later without rebuilding the env, use `launch.sh`:

```bash
# launch in the existing conda env (no rebuild)
./launch.sh
```

### Browser mode (development)

```bash
# start only the backend server, open in browser manually
python3 serve.py
# -> http://localhost:8000/
```

### Packaged builds (AppImage / macOS .app)

One entry point builds the artifact for whatever host you are on — it
dispatches to the platform script and never pretends to cross-build:

```bash
./build.sh          # Linux -> AppImage,  macOS -> .app
./build.sh --dmg    # macOS: also produce a .dmg
```

#### AppImage (Linux x86_64)

A single self-contained `.AppImage` that runs on a clean Ubuntu/Debian x86_64
install with no system Python or Qt needed — the easiest way to ship the app to
the offline field laptop. The build host needs Ubuntu/Debian x86_64, `python3`
(3.10+), `pip`, `wget` or `curl`, and roughly 1 GB of free disk for the build
cache. No conda required.

```bash
# one command — builds a self-contained Linux x86_64 AppImage
./build-appimage.sh
```

The script writes `Corvus_GCS-<version>-x86_64.AppImage` to the repo root, where
`<version>` is read from the `VERSION` file (see [Version control](#version-control)).
On the field laptop:

```bash
chmod +x Corvus_GCS-*-x86_64.AppImage
./Corvus_GCS-*-x86_64.AppImage
# optional MAVLink connection:
./Corvus_GCS-*-x86_64.AppImage 8000 serial:/dev/ttyUSB0:57600
```

The first run downloads `appimagetool` and the Python wheels (PyQt6 /
QtWebEngine, pymavlink, paramiko, pyserial); `appimagetool` is cached under
`build/` for fast re-runs. The AppImage bundles the app version from `VERSION`,
so to release a new version you only bump `VERSION` and rebuild. Build artifacts
(`build/`, `*.AppImage`) are gitignored.

#### macOS app (Apple Silicon / Intel)

A standard `.app` bundle with the same guarantees: a relocatable CPython, Qt,
and every runtime dependency inside `Corvus GCS.app`, so it runs on a clean Mac
with no Homebrew, no conda, and no Qt install. The build host needs macOS with
the Xcode command line tools (`xcode-select --install`) and a **framework**
CPython 3.10+ — Homebrew's `python@3.11` or a python.org install. A conda
interpreter cannot be relocated into a bundle and is rejected with a clear
error.

```bash
# one command — builds Corvus GCS.app (add --dmg for a disk image)
./build-macos-app.sh --dmg
```

The script writes `dist/Corvus GCS.app` and, with `--dmg`,
`Corvus_GCS-<version>-macOS-<arch>.dmg` to the repo root — `<version>` from the
`VERSION` file, `<arch>` from `uname -m`. Drag the app to `/Applications` and
launch it like any other app, or start it from a terminal to pass a port and a
MAVLink connection:

```bash
"/Applications/Corvus GCS.app/Contents/MacOS/corvus-gcs" 8000 serial:/dev/tty.usbserial-0001:57600
```

The bundle is **ad-hoc signed and not notarized**, so the first launch on
another Mac needs right-click -> Open (or
`xattr -dr com.apple.quarantine "Corvus GCS.app"`). Set
`CODESIGN_IDENTITY="Developer ID Application: ..."` to sign it properly.
Artifacts (`dist/`, `*.dmg`) are gitignored.

#### Continuous builds

Both pipelines run the same `./build.sh`:
[`.gitlab-ci.yml`](.gitlab-ci.yml) (primary, `git.unibw.de`: tests + AppImage
release) and [`.github/workflows/build.yml`](.github/workflows/build.yml)
(tests, frontend assertions, AppImage, macOS `.app`/`.dmg`, and a GitHub
Release on a version tag).

---

## Connecting to a drone

### PX4 SITL (simulated)

If PX4 SITL is running (default UDP 14540), Corvus GCS connects automatically on
startup and starts receiving live telemetry:

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
right-side panel rather than a command-line connection string. The operator picks
a serial port from a dropdown (populated live from the backend), chooses a baud
(57600 by default, labeled for the SiK Radio V3), or enters a custom UDP/TCP
connection string such as `udp:0.0.0.0:14540` for PX4 SITL.

#### Holybro SiK Telemetry Radio V3

A USB serial radio. On Linux it enumerates as two devices, e.g.
`/dev/ttyUSB0` and `/dev/ttyUSB1`; the lower-numbered one is the MAVLink data
port.

| Setting | Value |
|---------|-------|
| Device path | `/dev/ttyUSB0` (the lower of the two `ttyUSB` devices) |
| Baud rate | 57600 (factory default) |
| Connection string | `serial:/dev/ttyUSB0:57600` |

On serial links Corvus GCS automatically applies conservative MAVLink stream
rates so the 57 kbps radio link is not saturated, uses a longer heartbeat
timeout (10 s), and reconnects with backoff if the link drops.

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

## Plugins — Vibration Monitor

The **TOOLS** tab (labelled "Tools & Plugins") in the right-side panel is the
extension point of Corvus GCS. It shows a grid of plugin cards; clicking a card
opens that plugin's view with a back button, and opening another plugin first
closes the current one so its teardown runs exactly once (no listener leaks).
The first shipped plugin is the **Vibration Monitor**.

### Vibration Monitor

A live Plotly line graph of the PX4 `VIBRATION` message, plus a stats row of
cumulative accelerometer-clipping counters (3 counters). When the plugin opens
it asks PX4 to stream `VIBRATION` at ~10 Hz **on demand**; closing the plugin
restores PX4's default rate. High-rate vibration data flows only while someone is
watching, so the serial link stays uncongested the rest of the time — the lean,
field-first pattern used throughout Corvus.

The graph shows three traces:

| Trace | `VIBRATION` field | Color | Meaning |
|-------|-------------------|-------|---------|
| Gyro coning | `vibration_x` | Blue | Gyro delta-angle coning metric |
| Gyro HF vibration | `vibration_y` | Yellow | Gyro high-frequency vibration |
| Accel HF vibration | `vibration_z` | Red | Accelerometer high-frequency vibration — the main mechanical-health indicator |

**PX4-specific semantics** (verified from PX4 source
`src/modules/mavlink/streams/VIBRATION.hpp`): PX4 repurposes the three standard
MAVLink `VIBRATION` fields — `vibration_x` is a gyro delta-angle coning metric,
`vibration_y` is gyro high-frequency vibration, and `vibration_z` is
accelerometer high-frequency vibration. A rising **accel-HF (red)** trace means
more mechanical vibration — suspect motor imbalance, a loose or chipped prop, or
worn bearings. Operators should read the red trace first as the
mechanical-health indicator.

On-demand streaming uses `MAV_CMD_SET_MESSAGE_INTERVAL`, PX4's standard
per-message rate control (works on v1.16–v1.18). Plotly is **vendored locally**
at `src/vendor/plotly-basic.min.js`, so the graph renders with no internet
connection — matching the offline field-use requirement.

---

## Setup — Parameters, Calibration, Autotune, Firmware

The Setup page (left nav) holds the parameter editor, the
calibration / autotune tiles, and a firmware flash tab. The parameter editor,
calibration, and autotune are **lazy** and **armed-safe**: nothing is fetched
until the operator opens a tile, and every parameter write, calibration,
autotune, and firmware flash is refused while the vehicle is armed.

### Parameters

Parameters are downloaded **lazily** — only when the operator opens the
**Parameters** tile. Until then Corvus streams only the telemetry needed to fly
(GPS, attitude, etc.), keeping the GCS lean and fast to ready-for-flight. The
download shows a live progress indicator, and the editor is usable only after
the full parameter set has arrived (the operator must wait for completion). The
editor lets the operator change any parameter; writes go through `PARAM_SET` and
are confirmed by a `PARAM_VALUE` echo. Parameter writes are **refused while
armed** (safety).

**Export / Import.** *Export* opens a dialog with the **file name** and the
**folder**, both prefilled — the name as
`corvus-params_<vehicle>_<YYYY-MM-DD_HH-MM>.json` (readable date and airframe,
because a folder of exports has to be scannable by eye), the folder from
`params_dir` in the config (default `~/.corvus/params`, editable per export and
in Settings → Files). The confirmation names the full path the file was written
to. *Import* reads the same file back and uploads it to the vehicle.

> The file is written by the **backend**, not pulled as a browser download. The
> desktop build runs this UI inside QtWebEngine, which drops an `<a download>`
> unless the host application implements a download handler — so the previous
> blob-based export silently produced no file at all there. Writing it
> server-side behaves identically in the desktop app and in a browser, lands it
> in a folder the operator chose, and can report exactly where it went.

### Sensor calibration

Setup → **Calibration** tile: one-tap sensor calibration for:

- Compass (magnetometer)
- Gyroscope
- Accelerometer
- Level Horizon
- Airspeed
- Baro

Calibration is **refused while armed**. PX4 also rejects calibration when armed,
but Corvus refuses client-side first. During interactive calibrations (compass
rotation, accelerometer positions) PX4 streams step-by-step guidance as
`STATUSTEXT`, which appears in the MAVLink console and the warnings popover.

### Motors (ESC) calibration

Setup → **Calibration** tile → **Motors (ESC)** button: PX4 motor/ESC
calibration via `MAV_CMD_PREFLIGHT_CALIBRATION` (param7 = 1.0). A
safety-confirm modal opens first — **remove all propellers** and follow the
battery procedure (disconnect the flight battery, then re-plug it to power
the ESCs when PX4 instructs). Motors spin at maximum PWM during calibration.
Calibration is **refused while armed**, and PX4 streams step-by-step guidance
as `STATUSTEXT` into the guidance list.

### Autotune

Setup → **Calibration** tile → **POD Tuning** subsection: PX4 autotune via
`MAV_CMD_DO_AUTOTUNE_ENABLE`. The operator picks an axis — **Roll, Pitch, Yaw, or
All**. Autotune tunes the rate and attitude controllers together (PX4 module
`mc_autotune_attitude_control`). Autotune is **refused while armed**, and tuning
progress streams as `STATUSTEXT`.

> **No velocity-controller autotune in PX4.** PX4 provides only rate + attitude
> autotune — there is no velocity-controller autotune. The UI marks the velocity
> controller as unsupported rather than sending a command the firmware does not
> understand.

The autotune graphs (roll rate, roll attitude, horizontal velocity) use Plotly.
These traces come from existing telemetry that streams continuously (not
on-demand like vibration): roll rate from the `ATTITUDE` body rate `rollspeed`,
roll attitude from `ATTITUDE` roll, and horizontal velocity from `VFR_HUD`
groundspeed / `GLOBAL_POSITION_INT`.

Body angular rates (`rollspeed`, `pitchspeed`, `yawspeed`, in deg/s) are part of
the telemetry state.

### Firmware

Setup → **Firmware** tab (alongside **Calibration** and **Parameters**): flashes
PX4 firmware onto the flight controller. The operator selects a PX4 firmware
file (`.px4` / `.bin`); Corvus reboots the autopilot into its USB bootloader,
uploads and verifies the image, then reboots into the new firmware.

**Direct-USB-only (hard constraint):** flashing is permitted **only** over a
direct USB connection to the flight controller's CDC ACM device
(`/dev/ttyACM*`). It is **refused** over a SiK telemetry radio
(`/dev/ttyUSB*`) and over any UDP / TCP link — those transports cannot carry
the bootloader protocol. Flashing is also **refused while the vehicle is armed**.

---

## Offline map — named areas

The **Download offline map** button on the map opens a dialog (not a popover:
as an anchored panel it was constantly covered by the flight bar, the HUD, the
layer switcher and the right panel). It is centred above everything, closes on
Escape or a click outside, and a download in flight is **not** tied to it —
close the dialog, keep flying, re-open it and the live progress is still there.

Each download is recorded as a **named area** in the source's `.mbtiles` file
(the `corvus_regions` table in `corvus/tile_cache.py`), so the names travel with
the tiles they describe: copy the file to another laptop and the areas come
along; delete it and they go with it.

- **Name it when you download it.** Optional — an unnamed area is named after
  its centre coordinates, which is exact but not memorable, so rows can be
  renamed later from the list.
- **See what you have.** Every area is drawn on the map as a dashed outline with
  its name at the centre, and listed in the dialog with its service, zoom span,
  tile count and date. The map rail's frame button toggles the overlay.
- **Go to an area.** Clicking a row frames it on the map and closes the dialog.
- **Reclaim disk.** Deleting an area removes its record *and* its tiles — except
  tiles another area still covers. Areas overlap by design (a wide low-zoom
  region with a high-zoom landing site inside it), so a naive delete would punch
  holes in the neighbour.

---

## Settings — Appearance, SSH, Map

<!-- The heading (and therefore the #settings--appearance-ssh-map anchor the
     links above use) is deliberately unchanged; "Map" now means the map-service
     picker inside Appearance rather than a separate read-only section. -->

The **SET** page in the left nav is the operator's settings surface. It used to
be a read-only summary; it is now **editable**, and the connection settings it
shows are read live from the backend (no longer hardcoded). Everything on the
page is persisted in the single config store, `~/.corvus/config.json`, which is
written **atomically** and chmod'd to **0o600** because it may hold SSH
credentials. The same file backs the right-panel SSH tab.

### Appearance (color theme + map service)

**Color theme.** Five complete predefined themes, defined in
[`src/css/themes.css`](src/css/themes.css): **Green** (default), **Blue**,
**Pink**, **Orange**, and **Light** — the white/black theme, where every dark
surface in the app becomes light and the accent is near-black. Selecting one
writes `data-theme="<id>"` on `<html>`; because every color, shadow, and
translucency in the app is a token defined in that one file, the whole UI —
map chrome, HUD, popovers, terminal — restyles at once, with no per-component
overrides. This replaces the v1 accent picker, where `--accent` was the only
themeable value and everything else stayed dark.

The choice is stored twice on purpose: in `localStorage`, so a small inline
script in `index.html` can apply it **before first paint** (no flash of the
default theme), and in `~/.corvus/config.json` under `theme.name`, so it
survives a cache clear. A config carrying the legacy `theme.accent` hex still
loads — it maps to the nearest predefined theme.

**Map service.** Which tile service to use — **Esri**, **OpenStreetMap**,
**Google**, or **Bing** — and which of its layers (Satellite / Streets /
Hybrid / Topographic, whichever that service serves). Switching service keeps
the equivalent layer where there is one, so Esri Satellite → Google lands on
Google Satellite rather than resetting. The choice applies to the live map
immediately, keeps the Home tab's layer switcher in sync, is persisted as
`map.base_layer` + `map.provider`, and is what the **offline map downloader**
opens on. Each row shows how many tiles that layer already has cached.

> **Terms of service.** Only the Esri and OpenStreetMap endpoints are
> documented public tile services. The Google and Bing entries address those
> providers' internal tile endpoints directly, which their terms of service do
> not permit outside their own SDKs/APIs. Shipping them needs a licensed key
> (Google Maps Tile API / Bing Maps Key) swapped into the template in
> `corvus/tile_sources.py` first.

### SSH Connections

Saved SSH connections live in `~/.corvus/config.json` as a persisted list, so
the operator connects **by name** instead of re-typing credentials every flight.
Both the right-panel **SSH** tab and this Settings page show the same saved
list; each row has a **CONNECT** button and a **REMOVE** (trash) button.

- **Add** — name, host, port, username, and either a **password** (optional)
  or a **key file** path for key-based auth (e.g. `~/.ssh/id_rsa`).
- **Connect** — connects by name; the credentials load from config, so a
  password is never echoed back over the API.
- **Remove** — first disconnects any live session for that name, then deletes
  the saved entry. The endpoint is idempotent.

### Connection

- **Connection** — the live MAVLink connection string and HTTP port, shown
  exactly as the backend sees them (no longer hardcoded).

The map base layer used to be a read-only row here; it is now editable in
**Appearance** above (and still switchable from the on-map layer control, which
stays in sync with it).

### Files

- **Parameter export folder** — where Setup → Parameters → *Export* writes
  parameter files, on the machine running Corvus. Empty means
  `~/.corvus/params`. Persisted as `params_dir`; the export dialog can still
  override it per file.

The tile-cache and tlog directories are config keys too (`tile_cache_dir` /
`tlog_dir`) but are read at startup, so they stay config-file-only rather than
appearing here as settings that silently need a restart.

### About

Reads `GET /api/version` for the product name, version, and PX4 profile (no
hardcoded version — see [Version control](#version-control)).

---

## Architecture

```
serve.py                      # browser-mode entry point (HTTP server only)
run.sh                        # standalone launcher: conda env + PyQt6/QtWebEngine
launch.sh                     # launch in an existing conda env (no rebuild)
build.sh                      # build the artifact for the current host
build-appimage.sh             # build a self-contained Linux x86_64 AppImage
build-macos-app.sh            # build a self-contained macOS .app (+ .dmg)
environment.yml               # conda env spec (PyQt6, pymavlink, paramiko, pyserial)
pyproject.toml                # tooling/pytest config (version comes from VERSION, not here)
├── corvus/                   # Python backend package
│   ├── app.py                # standalone PyQt6 + QtWebEngine app wrapper
│   ├── version.py            # reads VERSION (single source of truth)
│   ├── config.py             # operator config (~/.corvus/config.json, atomic, 0o600)
│   ├── state_store.py        # thread-safe Vehicle State Store
│   ├── mavlink_bridge.py     # pymavlink connection + message parsing
│   ├── ssh_bridge.py         # paramiko SSH sessions
│   ├── tile_cache.py         # offline MBTiles store + named downloaded areas
│   └── server.py             # HTTP + SSE + API server
├── src/                      # web frontend
│   ├── index.html
│   ├── css/
│   │   ├── themes.css        # ALL design tokens + the 5 color themes
│   │   ├── main.css          # layout + screen-specific styling (no tokens)
│   │   └── components.css    # reusable component styles (.btn/.field/.tile/.modal/…)
│   ├── js/
│   │   ├── telemetry.js          # SSE client for real telemetry
│   │   ├── notification_dedupe.js # dedupes STATUSTEXT / warnings
│   │   ├── topbar.js         # top bar: arm/takeoff/land/RTL, mode, status
│   │   ├── map.js            # MapLibre map, vehicle tracking
│   │   ├── instruments.js    # compass + attitude indicator (HUD)
│   │   ├── panel.js          # MAVLink console + SSH (saved connections)
│   │   ├── link.js           # LINK tab (serial/UDP/TCP connect)
│   │   ├── plugins.js        # TOOLS-tab plugin system (extension point)
│   │   ├── plugin-vibration.js # Vibration Monitor plugin
│   │   ├── setup.js          # Setup page orchestrator (tile grid)
│   │   ├── setup-calibration.js # calibration + autotune sub-page
│   │   ├── setup-parameters.js  # parameter editor sub-page
│   │   ├── setup-shared.js   # shared Setup-page helpers
│   │   ├── sidenav.js        # left navigation + Settings page
│   │   ├── ui.js             # reusable UI component helpers
│   │   └── app.js            # top bar wiring + flight actions
│   └── vendor/
│       ├── maplibre-gl.min.js   # vendored MapLibre GL JS (offline map)
│       ├── maplibre-gl.css      # vendored MapLibre GL stylesheet
│       └── plotly-basic.min.js  # vendored Plotly (offline graphs)
├── assets/                   # logo + screenshots
└── VERSION                   # single-source version string
```

### Data flow

```
PX4 Autopilot → MAVLink (UDP/Serial) → pymavlink bridge → Vehicle State Store
                                                                    ↓
Web browser ← SSE (/api/telemetry) ← HTTP server ← (thread-safe)
```

Telemetry is pushed from backend to frontend via **Server-Sent Events** (SSE).
The frontend never polls.

---

## API endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/version` | GCS version + PX4 profile |
| GET | `/api/config` | Live operator config (SSH passwords redacted) |
| POST | `/api/config` | Apply a partial config update; persist atomically (chmod 0o600) |
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
| GET | `/api/params/export/target` | Where an export would be written + the default filename |
| POST | `/api/params/export` | Write a parameter file to disk; returns the full path |
| GET | `/api/tiles/sources` | Registered tile sources + the provider grouping + cache stats |
| POST | `/api/tiles/download` | Start a tile download for a named area |
| POST | `/api/tiles/cancel` | Cancel a running download job |
| GET | `/api/tiles/progress` | SSE stream of one download job's progress |
| GET | `/api/tiles/regions` | The named, pre-downloaded areas across every source |
| POST | `/api/tiles/regions/rename` | Rename a stored area |
| POST | `/api/tiles/regions/remove` | Forget an area, optionally deleting the tiles no other area covers |
| POST | `/api/calibrate` | Run a sensor calibration `{type: gyro\|compass\|baro\|accel\|level\|airspeed}` (refused while armed) |
| POST | `/api/autotune` | Run PX4 autotune `{axis: roll\|pitch\|yaw\|all}` (refused while armed) |
| POST | `/api/vibration/stream` | Request high-rate VIBRATION streaming on demand `{enabled, rate_hz}` (lean: restore default on close) |
| GET | `/api/ssh/connections` | Saved SSH connections + live `connected` status (no password echoed) |
| POST | `/api/ssh/connections` | Upsert a saved SSH connection by name (does not connect) |
| POST | `/api/ssh/connections/remove` | Disconnect (if live) and remove a saved SSH connection by name |
| POST | `/api/ssh/connect` | Open SSH session (by saved name or explicit host/port/user) |
| POST | `/api/ssh/send` | Send input to SSH shell |
| POST | `/api/ssh/disconnect` | Close SSH session |
| GET | `/api/ssh/stream` | SSE stream of SSH output |
| GET | `/api/ssh/sessions` | List SSH sessions |

---

## Console commands

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

The UI follows a strict semantic palette. Every design token lives in
`src/css/themes.css`; `main.css` and `components.css` only consume them. The
table below shows the **Green** (default) theme — each other theme redefines
the same token names:

| Color | Hex | Meaning |
|-------|-----|---------|
| Green | `#45D483` | healthy / connected / armed |
| Blue | `#4CC9FF` | flight / navigation / mission |
| Forest Green | `#3DA876` | UI interaction accent (not status) |
| Yellow | `#F5C842` | warning |
| Red | `#FF514D` | critical / error |

The operator picks a whole **theme**, not a single color, via
**Settings → Appearance** — Green, Blue, Pink, Orange, or Light — applied live
and persisted. See [Settings](#settings--appearance-ssh-map).

### Component layer

UI is composed from `Corvus.ui` (`src/js/ui.js`) and its matching CSS in
`src/css/components.css` — one factory per control, never hand-rolled markup at
the call site:

| Factory | Class system | Used for |
|---------|--------------|----------|
| `button` / `iconButton` | `.btn` / `.icon-btn` | every button in the app |
| `label` / `field` / `select` / `input` | `.field-*` | every form control |
| `card` / `section` / `pageHeader` / `row` / `empty` / `actions` | `.page-*` / `.ui-actions` | page structure |
| `optionCards` / `optionList` | `.option-cards` / `.option-list` | theme + map-service pickers, layer switcher |
| `tile` | `.tile` | Setup grid, Plugins grid |
| `navItem` | `.nav-item` | left rail |
| `progress` / `message` | `.ui-progress` / `.ui-msg` | download progress, inline status |
| `modal` | `.modal` | offline map, SSH add, parameter export, motor-calibration confirm |
| `statusDot` | `.status-dot` | connection / health indicators |

A screen-specific class layers a *modifier* on top (`.calib-btn`,
`.link-select`, `.setup-tile`) rather than re-implementing the control.

---

## Version control

The version string lives in [`VERSION`](VERSION) at the repo root.
`corvus/version.py` reads it at import time. The HTTP server exposes it at
`GET /api/version`. No component hardcodes a version literal — a single bump on
release propagates everywhere automatically. See `AGENTS.md` for the full version
policy. This README is a *consumer* of the version, never a source: it never
types a version number.

---

## PX4 compatibility

Primary target: **PX4 v1.16, v1.17, v1.18.** The MAVLink bridge detects the
firmware via `AUTOPILOT_VERSION`, loads the matching parameter schema, and falls
back gracefully (never crashes) when a parameter is absent. It refuses to send a
`MAV_CMD` or write a parameter the connected firmware does not understand. The
three target versions are the regression set: any parameter / mode / flight-plan
change is checked against v1.16, v1.17, and v1.18 before it is accepted. Older
firmwares (v1.12–v1.15) are supported on a best-effort basis via fallback tables
but are not the focus. See `AGENTS.md` for the full compatibility policy.

---

## About / Origins

Corvus GCS is developed at the **Universität der Bundeswehr München** (University
of the German Federal Armed Forces, Munich). It is an AI-assisted project —
"vibe coded" with human direction — and is built for real field use, not just
for demos.

<p align="center">
  <img src="assets/CorvusGCS_logo.png" width="96" alt="Corvus GCS logo">
</p>

**License:** no license file ships with the repository yet — the badge above
reads *TBD*. Add a `LICENSE` file at the repo root and update this section when
licensing is finalized. The project source currently lives on the university's
self-hosted GitLab at `git.unibw.de`.

---

<sub>Developed at the Universität der Bundeswehr München · Corvus GCS (CGCS) ·
version from <a href="VERSION"><code>VERSION</code></a>, served live at <code>GET /api/version</code>.</sub>
