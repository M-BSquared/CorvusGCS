<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS_logo.png">
    <img src="assets/CorvusGCS_logo_inverted.png" width="128" alt="Corvus GCS logo">
  </picture>
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
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS.png">
    <img src="assets/CorvusGCS_inverted.png" alt="Corvus GCS">
  </picture>
</p>

---

## Contents

- [What is Corvus GCS?](#what-is-corvus-gcs)
- [Screenshots & imagery](#screenshots--imagery)
- [Quick start](#quick-start)
- [Connecting to a drone](#connecting-to-a-drone)
- [MAVLink console](#mavlink-console)
- [Connection (LINK)](#connection-link)
- [Plugins — Vibration Monitor](#plugins--vibration-monitor)
- [Setup — Parameters, Calibration, Autotune, Firmware](#setup--parameters-calibration-autotune-firmware)
- [The map — aircraft, home, and the flown track](#the-map--aircraft-home-and-the-flown-track)
- [Flight HUD — a movable window](#flight-hud--a-movable-window)
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
  autopilot over Server-Sent Events (the frontend never polls). The HUD is a
  movable window: drag it by its title bar, pin it so a stray gesture cannot
  shift it, shrink it to compact, or collapse it to the bar alone — its
  position and state survive a restart.
- **Fly from the screen** — two optional on-screen controls in one movable
  window over the map: a **virtual joystick** (two spring-return sticks —
  throttle and yaw left, pitch and roll right) and an **arrow-key pad** that
  the keyboard's own arrow keys also drive, for forward / back / left / right.
  Both stream to the autopilot as `MANUAL_CONTROL`. Off by default; they are
  input sources only and never arm, change mode, or override a failsafe.
- **Read the aircraft** — the vehicle marker carries a heading cone and a white
  separating ring so it stays visible over any imagery; the home point is a
  landing-pad mark with crosshair ticks on the exact coordinate. The flown
  track is a red trail with a casing, and it survives a link drop — only an
  actual vehicle reboot discards it, or a small clear button in the map's
  corner.
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
- **Calibrate sensors** — a guided wizard for compass, gyro, accelerometer,
  level-horizon, airspeed and baro: the aircraft is drawn in the attitude PX4 is
  asking for, every position is tracked as it completes, and a running
  calibration can be aborted on the vehicle.
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
- **Configure** — an editable **Settings** page (left nav → SET): six
  predefined color themes (two light, four dark), the interface size, the map
  service to use (Esri, OpenStreetMap, Google, Bing) and which of its layers,
  the on-screen flight controls, saved SSH connections, and the live
  MAVLink/HTTP connection summary.
  Everything applies instantly and is persisted. See
  [Settings](#settings--appearance-ssh-map).

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

> **Fully offline:** the interface loads **nothing from the internet**. MapLibre
> GL JS, Plotly, Lucide icons and both webfonts (Inter / JetBrains Mono) are all
> vendored under `src/vendor/`, so icons render and type is correct on a laptop
> that has never seen a network. The **map tiles** are the one thing that must
> originate online, and even those never touch the browser directly: all tile
> traffic is routed through the backend (`/api/tiles/...`), which fetches and
> caches into the offline **MBTiles (SQLite) tile-cache store** in
> `corvus/tile_cache.py`. Use the on-map "Download offline map" button to cache
> a **named area** while you still have a connection, and the map then works in
> the field with no connection at all.
>
> Every vendored dependency and its license is listed in **Settings → About →
> Credits**.
>
> Offline is also a *speed* property, not just an availability one. Panning
> onto ground that was not pre-downloaded is a whole viewport of cache misses,
> and each one used to spend the full upstream timeout failing to reach a
> network that is not there. A circuit breaker now trips after a few
> consecutive failures and fails subsequent misses instantly for 30 s, then
> lets a single probe through — so an offline pan renders its cached tiles at
> full speed and simply leaves the rest blank, and walking back into coverage
> recovers on its own without a restart.

---

## Screenshots & imagery

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS.png">
    <img src="assets/CorvusGCS_inverted.png" alt="Corvus GCS wordmark">
  </picture>
</p>

<p align="center"><em>The operator interface is map-centered, with a floating HUD and a collapsible right-side engineering workspace (MAVLink console, SSH, plugin slot). It ships in six color themes — white with orange accents by default, plus a white/black one and four dark ones — and its whole size is one slider, from 80% to 150% — see <a href="#appearance-color-theme--map-service">Appearance</a>. No interface screenshot is checked in yet; drop one into <code>assets/</code> and wire it in below.</em></p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS_logo.png">
    <img src="assets/CorvusGCS_logo_inverted.png" width="160" alt="Corvus GCS logo">
  </picture>
</p>

<p align="center"><em>Corvus GCS project logo.</em></p>

Drop additional screenshots into `assets/` and wire them in here. Keep image
paths relative (`assets/...`) so they render on both GitLab and GitHub.

The logo ships in two cuts — white artwork (`*_logo.png`, `CorvusGCS.png`) for
dark backgrounds and black (`*_inverted.png`) for light ones — because a
transparent-background logo is invisible on the wrong surface. Every usage above
is a `<picture>` whose `<source>` serves the white cut to dark-mode readers and
whose `<img>` fallback is the black cut. The fallback is the black one on
purpose: both GitLab and GitHub default to a light theme, and a renderer that
strips `<source>` falls through to the `<img>`, so the safe degradation is
black-on-white. The app itself swaps the same two files through the
`--logo-mark` token in `src/css/themes.css`.

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

## MAVLink console

The operator's direct line to the airframe, built to stay usable when a lot is
happening rather than only when the stream is quiet:

- **Filter** — substring match over the live stream, applied to lines already on
  screen as well as new ones.
- **Colour, not a severity filter** — there are no ALL / INFO / WARN / ERR
  buttons. Every line reaches the stream and its severity is read off the line
  itself: critical and error red, warning amber, success green, the operator's
  own commands accent, shell replies blue — each with a matching colour rail
  down the left edge so a warning is findable while scrolling a busy stream. A
  severity floor hid the INFO lines that explain the error above them, and the
  question is nearly always "what happened around this", not "show me only
  errors". `critical` (STATUSTEXT severity ≤ 3) shares the red of `error`, so
  an emergency never renders in the ordinary text colour.
- **Pause** — freezes the view while still buffering, so reading a message does
  not mean losing the next fifty. The status bar reports how many are held.
- **Copy / Save** — the visible lines, for a bug report or a flight log. Save
  writes server-side (`POST /api/console/save`) beside the tlogs, for the same
  reason parameter export does.
- **Tab completion** — completes an unambiguous command, lists the candidates
  otherwise; `?` prints the whole command table into the console where it can be
  scrolled back to, and works with the link down.
- **History** — persisted across launches, not just across tab switches.
  `Ctrl/Cmd-L` clears.

The stream is a buffer of records, not just DOM — filtering has to be able to
reveal a line it previously hid, so the line must still exist somewhere. Clearing
drops the buffer as well, or the next filter change would resurrect everything
just dismissed.

---

## Connection (LINK)

- **Disconnect.** Previously the only way out of a link was into another one, so
  freeing the radio — to hand the aircraft to another GCS, swap a cable, or stop
  a reconnect loop hammering a port that moved — meant quitting the app.
  Available while *connecting* too, which is exactly when a retry loop needs
  stopping.
- **Link quality** from the same SSE field the top bar uses. "Connected" says the
  socket is up; this says whether it is worth flying on.
- **Recent connections**, persisted and one click to reuse. Kept exact:
  `serial:/dev/ttyUSB0:57600` and `…:115200` are different links, and collapsing
  them would silently reconnect at the wrong baud.
- **Presets** for the endpoints PX4 publishes. They fill the field rather than
  connecting outright, because a preset is a starting point you may want to edit.
- **Port re-enumeration** when the tab is opened — a radio plugged in after
  launch no longer needs the refresh button found.

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

Setup → **Calibration** tile: a guided wizard per sensor, not a row of buttons.
The list names what each calibration is for, how many positions it needs, how
long it takes, and whether the autopilot has to be rebooted afterwards:

| Calibration | Positions | Typical time |
| --- | --- | --- |
| Accelerometer | 6 | 2–3 min (reboot after) |
| Compass (magnetometer) | 6, each rotated | 3–5 min (reboot after) |
| Level Horizon | stays still | < 30 s |
| Gyroscope | stays still | < 30 s |
| Barometer | stays still | < 30 s |
| Airspeed | stays still | < 1 min |
| Motors / ESC | stays still | 1–2 min |

**The aircraft is drawn, not described.** PX4 asks for a position in its own
vocabulary — `[cal] Rotate to a pending side: back` — which is where field
calibrations go wrong. Corvus renders a low-poly raven, wings spread, in the
exact attitude the autopilot is asking for: a real 3-D model, rotated by the
pose and shaded per face, standing on a ground plane so "down" is unambiguous.
The compass figure rotates about the vertical axis, which is the motion PX4
wants. Every position the calibration will ask for is shown as a strip beneath
the figure and can be previewed before the aircraft is picked up; each one is
marked active, done, or failed as the calibration runs.

The wizard reads PX4's `STATUSTEXT` guidance off the console stream and turns it
into the instruction on screen — progress, the position being measured, sides
that completed, sides that were too shaky, operator prompts ("connect the
battery now", "blow into the pitot"), and the final outcome. The raw autopilot
transcript stays visible underneath as the audit trail. If the autopilot goes
quiet after accepting the command, or stops talking mid-calibration, the wizard
says so instead of leaving a spinner running.

Calibration is **refused while armed** — PX4 rejects it too, but Corvus refuses
client-side first — and the wizard states the missing precondition (no link, or
armed) rather than failing at the moment the operator presses start.

**Abort.** A running calibration can be stopped from the wizard: Corvus sends
`MAV_CMD_PREFLIGHT_CALIBRATION` with all seven parameters at zero, which PX4
reads as "cancel the calibration in progress". Without it, a calibration waiting
for a position the operator cannot produce is only escapable by power-cycling
the autopilot.

### Motors (ESC) calibration

Setup → **Calibration** → **Motors / ESC**: PX4 motor/ESC calibration via
`MAV_CMD_PREFLIGHT_CALIBRATION` (param7 = 1.0). The card is flagged
**Props off**, and starting it opens a safety-confirm modal — **remove all
propellers** and follow the battery procedure (disconnect the flight battery,
then re-plug it to power the ESCs when PX4 instructs). Motors spin at maximum
PWM during calibration. Refused while armed, like every other calibration.

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
PX4 firmware onto the flight controller. Corvus reboots the autopilot into its
USB bootloader, uploads and verifies the image, then reboots into the new
firmware.

**Two ways to choose an image.**

*PX4 release (default).* Pick a release and your board; Corvus downloads the
matching `.px4` for you. The release list comes from PX4's GitHub releases, the
board list is that release's own build targets — around 150 per release, so
there is a filter — and images already downloaded are marked, because those
flash with no network at all. The newest **stable** release is preselected:
PX4's newest tag is usually a beta, and a pre-release is a choice an operator
should make deliberately rather than land on by not choosing.

*Local file.* Select a `.px4` / `.bin` yourself — for custom builds, and for a
laptop that has never been online.

**Board detection.** Corvus tries to recognise what is plugged in and
preselects it. PX4 builds its USB product string from the board, so the
descriptor the OS already has ("PX4 FMU v6X.x", "CubeOrange") is the most direct
answer; the token is then matched against *that release's own build targets*, so
a board PX4 added after Corvus shipped is still found. For the few boards whose
descriptor says nothing useful there is a short USB VID:PID table, and
`AUTOPILOT_VERSION` supplies the same ids over a link with no serial device at
all. A board sitting in its bootloader identifies itself too.

It is a **suggestion**: the select stays free, and once the operator picks a
board themselves detection stops moving the selection under them. An
unrecognised device detects nothing rather than guessing — a wrong preselection
is worse than none, because it is the one nobody re-reads.

**What the app fetches, and when.** Nothing at startup, and nothing from the
browser. The catalogue is fetched only when the Firmware page asks for it, by
the *backend*, and is cached to `firmware_dir` (default `~/.corvus/firmware`)
together with every image it downloads. Offline, the page serves that cache and
says so; already-downloaded images still flash. This is the same deliberate
exception the map tiles are (see the offline note at the top): operator-
initiated, backend-side, and cached for the field.

**What the browser cannot choose.** A flash request names a *release* and a
*board*, never a URL. The backend resolves the download target from a fixed
release-URL template and checks the host against an allow-list, so neither the
frontend nor a tampered catalogue cache can point the fetch somewhere else.
Bootloader and cannode images are filtered out of the board list entirely —
they ship in the same release as the firmware and differ by one filename
suffix, and flashing one through the firmware uploader bricks the board.

**Direct-USB-only (hard constraint):** flashing is permitted **only** over a
direct USB connection to the flight controller's CDC ACM device
(`/dev/ttyACM*`). It is **refused** over a SiK telemetry radio
(`/dev/ttyUSB*`) and over any UDP / TCP link — those transports cannot carry
the bootloader protocol. Flashing is also **refused while the vehicle is armed**.
The gate is re-checked *after* the download, too: the fetch takes time, and a
vehicle that armed meanwhile must not be flashed. Download and flash are one
job with one progress bar and one Cancel — including during the download, which
is the longer half.

### Firmware version

The connected autopilot's firmware version is read from `AUTOPILOT_VERSION` and
shown in the top bar and under Setup → **Vehicle Info → Firmware Version**. It
arrives within a second or two of connecting and is **independent of the
parameter download** — parameters are lazy, and a version that waited for them
would stay blank for the whole flight.

> Corvus asks for it three ways, because no single one covers the range: PX4
> v1.16–v1.18 answer `MAV_CMD_REQUEST_MESSAGE` (message id 148), older builds
> answer the deprecated `MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES`, and the bare
> `AUTOPILOT_VERSION_REQUEST` message is an ArduPilot-era legacy that PX4 does
> not handle at all. A single retry ~4 s after connect covers a slow or lossy
> link.

---

## Analysis — flight-log download

Left nav → **ANALYSIS**. Current Telemetry sits at the top (live, not a snapshot
taken when the page opened), then a single slim row for the download folder,
then one tile per log kind. Opening a tile **replaces** the page, the same way
the Setup tiles do — a log list is a place you went to and Back is the way out,
not a card that unfolds and pushes everything else off the screen. Each tile's subtitle carries its count, so "is there anything
to fetch?" is answered before you open either one.

Two kinds of log matter after a flight and they live in different places:

| | Where it is | How it gets there |
| --- | --- | --- |
| **ULog** | the flight controller's SD card | pulled off the vehicle over the MAVLink `LOG_*` protocol |
| **tlog** | this laptop, in `tlog_dir` | Corvus recorded it from the MAVLink stream as you flew |

**One folder, chosen once.** The download folder is one slim row above the
tiles — a setting, not a section — and is persisted to the config (`log_download_dir`, default
`~/.corvus/flightlogs`), so it is already set the next time you connect. It is
deliberately *not* `tlog_dir`: mixing what the app writes with what the operator
pulled off the aircraft makes both harder to reason about. The folder is created
and checked for writability when you set it — not after you have queued an hour
of downloads into it.

**It knows what it already has.** The download folder is scanned and every log
the vehicle reports is matched against it: the ones already saved are tagged
**in folder** with their path, and **Select missing** ticks only the rest — the
common case after a flight. The match needs the id *and* the size to agree,
because SD cards recycle log numbers and reporting last month's `log_003` as
this flight's would send an operator home with the wrong evidence.

**Straight from a log to its review.** A row tagged **in folder** carries a
`Review →` shortcut that opens Flight Review on that exact file. Without it the
only route is Back, the Flight Review tile, and finding the same flight again in
a list that names files by filename while the row you were looking at names them
by log number.

**Select several, walk away.** Tick the logs you want and press Download; they
are fetched **one after another**, with the current one marked, the rest queued,
a progress bar, and a Cancel that works throughout. Sequential is a protocol
constraint, not a simplification: MAVLink has a single log session per vehicle,
so two concurrent downloads interleave their `LOG_DATA` and corrupt both files.

Files land as `log_<id>_<UTC date>.ulg` — sortable, and still readable a month
later.

### Flight Review

The third tile on the Analysis page. Pick a downloaded ULog and Corvus reads it
**locally** — nothing leaves the machine — and reduces it to the plots that
answer "was that flight healthy?". Findings sit above the plots, the aircraft's
own log messages below them, with a severity filter.

**A log that never came through Corvus** can be opened too: pulled off the card
by hand, or sent over by whoever flew it. That one is a file dialog, not a path
field, and the bytes are posted rather than the path — so the operator's own
dialog stays the only thing that ever names a file on this machine, and the
backend never gains an endpoint that reads an arbitrary path on request. The
route into the download folder stays confined to it, checked with realpath. An
upload is capped at 128 MB, well under the parser's own ceiling, because this
one is held in memory as a request body before anything has looked at it. It is
cached like any other review, keyed on a digest of the content since there is no
file to stat.

**Flight modes, twice.** A labelled strip with a time ruler at the top (which
mode, when, and how long each was flown), and the *same* colours as bands behind
every time-based plot. The bands are the difference between a graph and a story:
an oscillation in Position and the same oscillation in Manual are different
findings, and without the mode behind the trace you cannot tell them apart.
Armed time is reported separately.

Every band **names its mode inside the plot**, written down the band rather than
across the trace. A colour on its own is a legend lookup, and reading a spike
should not mean scrolling back to the strip to find out what was flying. A span
too narrow to hold a readable label is left unnamed rather than printed over its
neighbours. There is deliberately no separate mode timeline plot: with the bands
named in place it said the same thing a third time and cost a screen of scroll
doing it.

> Two mode enums exist and they are not interchangeable: `vehicle_status`
> carries `NAVIGATION_STATE_*`, the older `commander_state` carries
> `MAIN_STATE_*`, and 6 means Position-slow in one and Acro in the other. Each
> source is read with its own table. Where a firmware publishes a topic without
> updating its timestamp — `commander_state` in the v1.4-era logs does exactly
> that — no bands are drawn at all, because bands placed from a stopped clock
> would relabel the whole flight.

Up to 38 plots in six sections, with jump links across the top:

| Section | Plots |
| --- | --- |
| **Flight** | ground track (estimate, setpoint, projected GPS and the commanded waypoints on equal axes), altitude, local position X / Y / Z each against its setpoint, ground speed, velocity X / Y / Z each against its setpoint, airspeed, estimated wind |
| **Control** | manual control input (the pilot's sticks), roll / pitch / yaw **angle** each against its setpoint, roll / pitch / yaw **angular rate** each against its rate setpoint, thrust demand |
| **Airframe** | per-motor outputs |
| **Estimator** | EKF innovation test ratios with the 1.0 rejection line drawn, altitude estimate (GPS MSL vs barometer vs fused, with the altitude setpoint as markers), GPS vs estimated horizontal velocity, estimated gyro bias |
| **Sensors** | accelerometer clipping, vibration metrics (accel and gyro, per IMU), magnetic field strength, IMU temperature, barometer altitude and temperature, GPS satellites, GPS uncertainty (eph/epv/speed variance), GPS noise and jamming, GPS fix type, rangefinder |
| **System** | battery voltage and current, pack state, processor and RAM, RC link |

One plot per axis rather than three axes on one, because that is how the
question is actually asked — "is roll tracking?" — and three estimates plus
three setpoints on a single pair of axes is six lines nobody can read.
Setpoints are drawn as markers where they are sparse and stepped: a line
through them would imply values that were never commanded.

Plots are drawn as they scroll into view. A full review is three dozen Plotly
graphs, and building them all up front stalls the page before anything is
readable — including the summary at the top, which is the part most reviews
never scroll past.

Reading the log is the expensive half, and it is paid once: the last two reviews
are kept, keyed by the file's path, size and modification time, so going back
and opening the same flight again is instant while a log re-downloaded over its
own name is re-read rather than answered from a stale review. While a read is in
flight the page says so; if you leave before it lands, the answer is dropped
rather than painted into the page you moved on to.

**The ground track is a comparison, not a picture.** Four things share the
axes because the question is what disagrees: where the estimator thought it was,
where it was told to go, where GPS said it was, and which points were actually
commanded. Raw fixes are projected through the estimator's *own* origin with
PX4's azimuthal-equidistant transform — the same one that produced
`vehicle_local_position` in the first place — so the gap between the two tracks
is estimator error rather than a difference of frames; a flat-earth shortcut
would open a few metres of fake disagreement at range. A flight without a global
reference (indoors, no GPS) gets no GPS track rather than one projected against
zeros, fixes below a 3D lock are left out, and the commanded positions are drawn
as points rather than joined: the triplet is republished every cycle, so a line
through it would show legs the aircraft was never asked to fly. The estimate is
drawn last so a noisy GPS trace cannot hide it.

Only the plots the log can support are drawn — an airspeed plot on a multirotor
log is absent, not empty, and the three-way altitude comparison appears only
when at least two sources are present.

Findings are limited to what the log actually shows: accel clipping, EKF
rejection, motor imbalance (one output averaging far above the others — an
airframe problem, not a tuning one), vibration graded against PX4's own
thresholds, RC signal loss, pack sag, logging dropouts, truncation, and
error-level messages. No score, no grade: a review that invents a verdict is
worse than one that points at the plot.

It is modelled on [PX4's flight_review](https://github.com/PX4/flight_review)
and deliberately not a port of it: this is the pass an operator makes between
flights, not a full analysis suite.

Three things are load-bearing:

- **The ULog reader is ours, not a dependency** (`corvus/ulog.py`, stdlib
  `struct`). The format is small and stable, the app has to stay self-contained
  for the offline build, and a parser we own cannot change its rules under a
  field release. It is validated by diffing every decoded field against the
  reference implementation (pyulog) on PX4's own sample log — **153 fields,
  zero mismatches**, parameters and logged messages identical. Each message
  format is compiled once into a single `struct.Struct` over the whole record,
  padding included as pad bytes, so a record costs one `unpack` rather than one
  per field — the loop that dominates the read, at 2.4× on PX4's sample. A
  record shorter than its format still falls back to the field-by-field reader,
  because a log cut off by a power loss is exactly the log worth reading.
- **Decimation keeps the spikes.** A ten-minute log is hundreds of thousands of
  samples and the browser gets ~1200. Stride sampling would drop exactly the
  frame where a motor saturated or an accel clipped, so each bucket contributes
  its most extreme sample instead.
- **Version tolerance.** PX4 renames topics and fields between releases, so
  every plot names several candidates and takes the first the log actually
  contains — a v1.14 log and a v1.18 log both produce something. PX4's own 2016
  sample log still yields nine plots across all six sections.

A truncated log — the aircraft lost power mid-write, which is when the log
matters most — is read up to the cut and says so rather than being rejected.

**Erasing the vehicle.** *Erase all on vehicle* clears the flight controller's
log directory. It is deliberately not a bin icon per row: MAVLink's log
protocol has **no per-log delete**, `LOG_ERASE` takes everything, and a control
that looked like it removed one log would be lying. The confirm states how many
logs are about to go and — the number that matters — how many of them are not
yet in the download folder and therefore unrecoverable. Refused while armed and
while another log job owns the vehicle's log session. Afterwards the list is
re-read from the vehicle rather than assumed, so what you see is the result and
not our optimism. Files already downloaded to the laptop are untouched; that is
the whole point of downloading first.

Three things the download does that the bare protocol does not:

- **A lost packet is re-requested, not written as a hole.** The `LOG_*` protocol
  has no ACK and no retransmit of its own. Corvus tracks which bytes actually
  arrived and re-asks for the first real gap; a file that opens and lies is
  worse than a download that fails.
- **One bad log does not abandon the queue.** An operator who selects five logs
  and walks away gets the other four.
- **The log session is always ended.** PX4 keeps it open until it hears
  `LOG_REQUEST_END`, and an open session blocks logging of the next flight, so
  it is sent on every exit path — success, failure, or cancel.

---

## The map — aircraft, home, and the flown track

**Vehicle marker.** A heading cone, a white separating ring, the vehicle colour,
and a centre dot on the reported position. The ring is the part that matters:
the previous marker had no outline and disappeared over imagery its own colour.
The whole thing is one SVG, so the cone and the body cannot drift apart.

**Home point.** A landing-pad mark — "H" in a ring with crosshair ticks on the
exact coordinate — rather than a house glyph, which turned to mush at the size
it renders. Deliberately quieter than the vehicle: where the aircraft *is* has
to win over where it started.

**Flown track.** Three stacked lines, not one: a casing underneath, a glow, and
the coloured core. A single stroke is legible on grass and invisible over a
red-tiled roof; the casing gives the track an edge against any background, which
is the same trick road maps use. The colour is `--track` (a deeper red than the
vehicle marker, so the live position stays pickable out of its own history), and
each theme's casing is the opposite lightness of its track.

Two things about the track are deliberate:

- **It survives a link drop.** Only an actual vehicle reboot discards it,
  detected from `SYSTEM_TIME.time_boot_ms` moving *backwards* — the one honest
  signal that the airframe restarted rather than the radio glitching. A track
  erased by a dropped packet would be a real loss.
- **It is decimated by distance, not time.** A hovering aircraft adds one point,
  not six hundred, so the buffer is an hours-long budget instead of a stopwatch.
  The old 500-sample cap held under a minute of flight and quietly ate its own
  beginning mid-sortie.

A very small trash button appears in the map's bottom-left corner while a track
exists, at low opacity until reached for. An action taken once a flight should
not out-shout the thing it acts on.

---

## Flight HUD — a movable window

The HUD (compass, attitude indicator, and the live telemetry grid) used to be
nailed to the bottom-right corner of the map, which is as often as not exactly
the ground being flown over. It is now a small window with a title bar:

- **Drag** it anywhere over the map by grabbing the panel itself. It is clamped
  so a strip always stays reachable — it cannot be thrown off screen, and it is
  pulled back in if the window or the right panel shrinks the map underneath it.
- **Pin** locks the position. This is the one that matters in the field, where
  the panel sits under a thumb on a trackpad while the aircraft is airborne.
- **Compact** keeps every readout but shrinks the instruments and tightens the
  grid, for when the map matters more than the numbers.
- **Collapse** reduces it to the title bar.
- **Double-click the panel** returns it to its default corner.

There is deliberately no title bar. A permanent strip saying "FLIGHT" would
cost map for something the compass and horizon under it already say, so the
three controls sit over the panel's top-right corner and stay invisible until
a pointer is over the panel or a control takes keyboard focus. Collapsed is the
exception: with the readouts folded away the controls are all that is left, so
the panel becomes a small control pill.

Position and state persist in `localStorage`, so the panel stays where it was
put. The behaviour lives in `src/js/hud-panel.js`, deliberately separate from
`instruments.js`: that module renders the dials and knows nothing about where
the panel sits, and this one moves the box and never touches its contents.

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

### Appearance (company logo + color theme + interface size + map service + controls)

**Company logo.** An optional PNG shown at the **far top right** of the status
bar — a university or unit crest beside the telemetry blocks. Nothing ships
with the app and none is set by default: the Corvus mark keeps the left end of
the bar either way, and the right end stays empty until an operator picks a
file. Upload replaces the previous logo, **Remove** clears it, and both apply
to the live bar without a reload.

Unlike every other setting, the image is *not* stored in the config file: the
bytes are written to `~/.corvus/branding/logo.png` (beside the config) and only
the display filename is persisted, under `branding.logo`. That keeps
`config.json` readable and diffable instead of carrying a base64 blob. The
backend checks the PNG magic bytes and caps the upload at 4 MB, so a renamed
JPEG is refused at `POST /api/branding/logo` rather than rendering as a broken
image in the bar.

**Color theme.** Six complete predefined themes, defined in
[`src/css/themes.css`](src/css/themes.css): **Light Orange** (default) — white
surfaces with a burnt-orange accent; **Light** — the same white surfaces with a
near-black accent; and the four dark ones, **Green**, **Blue**, **Pink** and
**Orange**. Selecting one writes `data-theme="<id>"` on `<html>`; because every
color, shadow, and translucency in the app is a token defined in that one file,
the whole UI — map chrome, HUD, popovers, terminal — restyles at once, with no
per-component overrides. This replaces the v1 accent picker, where `--accent`
was the only themeable value and everything else stayed dark.

There are two bases rather than one. Bare `:root` carries the default light
theme in full, so the app is completely styled before any script runs; the four
dark themes share one grouped block that restates the whole dark surface/text/
shadow stack and then override only their accent family, and **Light** differs
from the default in nothing but its accent. The light accent is not the dark
themes' amber — `#F58A2B` carries 2.5:1 on white and fails as both text and a
fill behind white button labels — but a deeper `#C2540A` at 4.6:1 in both
directions.

The choice is stored twice on purpose: in `localStorage`, so a small inline
script in `index.html` can apply it **before first paint** (no flash of the
default theme), and in `~/.corvus/config.json` under `theme.name`, so it
survives a cache clear. A config carrying the legacy `theme.accent` hex still
loads — it maps to the nearest predefined theme. Changing the shipped default
only affects a fresh install: an operator who has already picked a theme has it
in both stores, and both outrank the default.

**Interface size.** One slider with step indicators — **80 / 90 / 100 / 110 /
125 / 150 %** — that scales the *whole* interface: text, icons, the top bar,
the nav rail, the engineering panel, the map chrome and the 1px hairlines
between them. Larger reads better on a bright field laptop at arm's length;
smaller fits more map and more of the engineering panel on a small screen. The
slider previews on every step the thumb crosses and persists once, on release,
so dragging it is a live preview rather than six writes to the config file.
Clicking a step indicator jumps straight to it — the fastest way back to 100%.

It is a single token, `--ui-scale`, and a single CSS rule: `zoom` on `<body>`.
That is the only lever that reaches everything, because this app sizes in
absolute pixels — including the icon dimensions JS writes inline, which no
`font-size` or `rem` scheme can touch. `zoom` sits on `<body>` and not `<html>`
so the viewport stays the unscaled reference: `position: fixed` chrome still
anchors to the real window edges and `height: 100%` still resolves to exactly
one screen.

Three places have to be told, and they are why the change is an event
(`corvus:scalechange`, with a plain `resize` dispatched alongside):

- **The map.** MapLibre sizes its drawing buffer from the container's
  *unscaled* size, so `zoom` alone would leave the map soft at 150% and
  oversampled at 80%. Multiplying the pixel ratio by the scale cancels both
  exactly, pinning the buffer to the screen's native resolution at every size.
  Pointer accuracy needs no help: MapLibre divides by `rect.width /
  offsetWidth` itself, which *is* the scale.
- **The HUD panel.** `getBoundingClientRect` and pointer coordinates come back
  in scaled pixels while `style.left` is written in unscaled ones, so the drag
  maths divides by the ratio — otherwise the panel runs away from the cursor.
- **The right panel's auto-collapse.** Its thresholds now read
  `document.body.clientWidth`, so the panel collapses when the layout actually
  gets cramped rather than at a fixed window width.

Persisted like the theme: `localStorage` for a pre-paint apply (the interface
never visibly resizes itself on load) and `~/.corvus/config.json` under
`ui.scale`. A hand-edited absurd value is **clamped, never rejected** — an
interface painted at 4000% cannot reach the settings page that would fix it.

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

**Controls.** Two switches, **both off by default**, that put a manual-control
pad over the bottom-left of the map on the Home tab. Each adds its own surface
to the same window, and either one alone is enough to show it:

- **Virtual joystick** — two spring-return sticks: **throttle and yaw** on the
  left, **pitch and roll** on the right. Centre for throttle is the **hover
  detent** (0.5 of the range), not zero, so releasing the sticks in Position or
  Altitude mode parks the aircraft rather than dropping it. A diagonal is
  clamped to the circle, because a stick has no corners.
- **Arrow keys** — a four-key cluster laid out as a keyboard's own inverted T,
  and the keyboard's real arrow keys drive it, lighting up the on-screen key as
  they go. **Pitch and roll only** — forward, back, left, right — at **half
  stick**: a key has no travel to meter, so full authority on a keypress would
  make the mildest correction the largest one available. A keypress is ignored
  while the caret is in a field, so typing a connection string cannot fly the
  aircraft. Keys and sticks **sum into one virtual stick** and the result is
  clamped, so pushing both at once cannot exceed full travel.

The pad is a **movable window**, like the flight HUD: drag it by its grip bar
(the sticks and keys are not drag handles — a drag that started on them would
fly the aircraft while moving the window), double-click that bar to send it
back to its corner, and the position persists in `localStorage`. It sits one
step above the HUD in the stacking order: both are movable and can be dragged
over each other, and when they overlap the control the operator is flying with
wins over a readout. Persisted as `controls.virtual_joystick` and
`controls.arrow_keys`; only real JSON booleans count, so a hand-edited `"true"`
reads as off rather than arming a flight control by typo, and the two keys are
merged **per key** by `POST /api/config` (unlike `theme`/`map`, which replace
wholesale) so turning one on never switches the other off.

Both are **input sources and nothing else**. They never arm, never change mode,
and never override a failsafe. Whether the autopilot listens at all is the
*vehicle's* decision — `COM_RC_IN_MODE` must be **1** (joystick only) or **3**
(both stick sources), and the aircraft has to be in a mode that flies from the
sticks — and the GCS deliberately does not set that parameter on the operator's
behalf.

Each frame goes out as one `POST /api/mavlink/manual` and reaches PX4 as a
`MANUAL_CONTROL` message, with the four axes scaled to the ±1000 ticks the
wire format carries (thrust runs 0…1000). There is no ACK for `MANUAL_CONTROL`
and none is waited for: it is a stream whose next frame supersedes the last, so
the bridge takes only the send lock and a 20 Hz stick stream never queues
behind a parameter download. The browser sends at **20 Hz while an axis is off
centre and 5 Hz at neutral** — neutral frames are still sent, because a *gap*
is what PX4 reads as RC loss, and 5 Hz stays well inside the default
`COM_RC_LOSS_T` of 0.5 s. Only one frame is ever in flight; a tick that finds
the previous POST unfinished skips rather than queues, so a slow link costs
rate, not latency.

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

**Credits** opens a dialog listing everything in the product that someone else
wrote — vendored libraries with their versions and licenses, the two typefaces,
the Python runtime dependencies, and the map services — alongside the project
credit. The library half is a static list in `src/js/credits.js` (with a
maintenance note: bump it when a file under `src/vendor/` is replaced); the map
attributions are read live from `/api/tiles/sources`, because those are legally
required, change whenever a source is added, and already have a single home in
`corvus/tile_sources.py`. URLs are shown as plain text rather than links — the
field laptop has no internet, so a link would be a dead end.

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
│   │   ├── themes.css        # ALL design tokens + the 6 color themes
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
│   │   ├── sidenav.js        # left navigation, Settings page, theme + interface scale
│   │   ├── ui.js             # reusable UI component helpers
│   │   ├── credits.js        # third-party attribution dialog
│   │   ├── hud-panel.js      # movable/collapsible flight HUD window
│   │   └── app.js            # top bar wiring + flight actions
│   └── vendor/                  # everything the UI loads — no CDN, no network
│       ├── maplibre-gl.min.js   # MapLibre GL JS (offline map)
│       ├── maplibre-gl.css      # MapLibre GL stylesheet
│       ├── plotly-basic.min.js  # Plotly (offline graphs)
│       ├── lucide.min.js        # Lucide icon set (pinned, not @latest)
│       ├── fonts.css            # @font-face for the two vendored families
│       └── fonts/               # Inter + JetBrains Mono, latin subsets
├── assets/                   # logo (white + inverted cuts) + screenshots
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
| GET | `/api/branding/logo` | The operator's company logo PNG (404 when none is set) |
| POST | `/api/branding/logo` | Store a raw PNG body as the company logo (`?name=` = display name, max 4 MB) |
| POST | `/api/branding/logo/remove` | Drop the company logo; idempotent |
| GET | `/api/state` | Current vehicle state (JSON) |
| GET | `/api/telemetry` | SSE stream of vehicle state |
| GET | `/api/console/stream` | SSE stream of MAVLink messages |
| POST | `/api/console/command` | Send MAVLink command |
| POST | `/api/console/save` | Write the console transcript beside the tlogs; returns the path |
| POST | `/api/mavlink/connect` | Reconnect with different connection string |
| POST | `/api/mavlink/disconnect` | Close the link and leave it closed (idempotent) |
| GET | `/api/mavlink/serial-ports` | List available serial ports → `{"ports": [{"device", "description", "hwid"}, ...]}` |
| POST | `/api/mavlink/arm` | Arm/disarm vehicle |
| POST | `/api/mavlink/mode` | Set flight mode |
| POST | `/api/mavlink/manual` | One virtual-joystick frame as MANUAL_CONTROL `{x, y, z, r, buttons}` (normalized axes, no ACK) |
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
table below shows the **Light Orange** (default) theme — each other theme
redefines the same token names, and the dark ones re-pick the status colors at
lighter, glowing values (`#45D483` / `#F5C842` / `#FF514D`) that would wash out
on white:

| Color | Hex | Meaning |
|-------|-----|---------|
| Green | `#0F8B50` | healthy / connected / armed |
| Blue | `#0B6FC0` | flight / navigation / mission |
| Burnt Orange | `#C2540A` | UI interaction accent (not status) |
| Amber | `#B07600` | warning |
| Red | `#D62B26` | critical / error |

The operator picks a whole **theme**, not a single color, via
**Settings → Appearance** — Light Orange, Light, Green, Blue, Pink or Orange —
applied live and persisted.

The Plotly graphs (autotune, vibration) follow it too. They are the one place
in the app that cannot reference `var(--token)`: Plotly draws into a surface it
owns and takes colors as literal strings, so `Corvus.ui.plotlyTheme()` resolves
the tokens and hands them over, and `Corvus.ui.onThemeChange()` forces a redraw
when the theme flips — everything else restyles itself the moment the attribute
changes, which is why a theme switch is an event at all. See [Settings](#settings--appearance-ssh-map).

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
| `slider` | `.ui-slider` | stepped slider with step indicators (interface size) |
| `tile` | `.tile` | Setup grid, Plugins grid |
| `navItem` | `.nav-item` | left rail |
| `progress` / `message` | `.ui-progress` / `.ui-msg` | download progress, inline status |
| `modal` | `.modal` | offline map, SSH add, parameter export, motor-calibration confirm |
| `statusDot` | `.status-dot` | connection / health indicators |

A screen-specific class layers a *modifier* on top (`.calib-btn`,
`.link-select`, `.setup-tile`) rather than re-implementing the control.

### Stacking order

Every `z-index` in the app comes from one ladder in `themes.css`
(`--z-map-chrome` → `--z-toast`), because the alternative is what it replaced:
a scatter of 10/11/12/15/20/30 where the only way to know whether a popover
would be covered was to open it and look. Within the map, transient surfaces
opened *from* the chrome (the layer menu, the takeoff and planning panels) sit
above everything that merely lives on the map (the flight bar, the HUD) — a
panel covering the menu that opened it is never what anyone wanted. The map
bands all sit below the app chrome, since `.main` creates no stacking context
and a layer menu covering the telemetry bar would be a different bug, not a fix.

### Surface material

Buttons, cards, tiles and option cards share one subtle glass material, defined
once in `themes.css` as `--panel-fill` / `--panel-hairline` / `--panel-wash`:
a translucent fill, a hairline of the contrast overlay, and a light blur — the
flight-action bar's substance at smaller scale, so a settings page and the map
chrome read as the same app. It is deliberately restrained: over an opaque page
there is nothing to blur, and the honest result there is a slightly lifted tint
rather than a frosted slab. The light theme carries more fill and a black
hairline, because white at 55% over a near-white page reads as nothing.

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
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS_logo.png">
    <img src="assets/CorvusGCS_logo_inverted.png" width="96" alt="Corvus GCS logo">
  </picture>
</p>

**License:** no license file ships with the repository yet — the badge above
reads *TBD*. Add a `LICENSE` file at the repo root and update this section when
licensing is finalized. The project source currently lives on the university's
self-hosted GitLab at `git.unibw.de`.

---

<sub>Developed at the Universität der Bundeswehr München · Corvus GCS (CGCS) ·
version from <a href="VERSION"><code>VERSION</code></a>, served live at <code>GET /api/version</code>.</sub>
