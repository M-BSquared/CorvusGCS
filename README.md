<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS_logo.png">
    <img src="assets/CorvusGCS_logo_inverted.png" width="128" alt="Corvus GCS logo">
  </picture>
</p>

<h1 align="center">Corvus GCS (CGCS)</h1>

<p align="center">
  <em>A modern, offline-first Ground Control Station for PX4 and ArduPilot autonomous aircraft.<br>
  Built for the field: a laptop, a telemetry radio, and no internet.<br>
  Loads only what it needs, so it is <strong>ready to fly in seconds, not minutes</strong>.</em>
</p>

<div align="center">
  <!-- corvus:version-badge -->
  <img src="https://img.shields.io/badge/Version-2026.09.57-f7ebe1?style=for-the-badge" height="28" alt="Version 2026.09.57" />
  <img width="8" />
  <a href="https://www.python.org/" target="_blank"><img src="https://img.shields.io/badge/Python_3.12%2B-3776AB?logo=python&logoColor=white&style=for-the-badge" height="28" alt="Python 3.12+" /></a>
  <img width="8" />
  <a href="https://developer.mozilla.org/en-US/docs/Web/JavaScript" target="_blank"><img src="https://img.shields.io/badge/JavaScript-F7DF1E?style=for-the-badge&logo=javascript&logoColor=black" height="28" alt="JavaScript" /></a>
  <img width="8" />
  <img src="https://img.shields.io/badge/%F0%9F%A4%96-Vibe%20Coded-5B2C6F?style=for-the-badge" height="28" alt="Vibe Coded" />
  <img width="8" />
  <!-- <a href="https://github.com/ArduPilot/pymavlink" target="_blank"><img src="https://img.shields.io/badge/pymavlink-00A6E2?logoColor=white&style=for-the-badge" height="28" alt="pymavlink" /></a>
  <img width="8" /> -->
  <br>
  <a href="https://px4.io/" target="_blank"><img src="https://img.shields.io/badge/PX4-v1.16%20%7C%201.17%20%7C%201.18-9f2dfe?logoColor=white&style=for-the-badge" height="28" alt="PX4 v1.16 | 1.17 | 1.18" /></a>
  <img width="8" />
  <a href="https://ardupilot.org/" target="_blank"><img src="https://img.shields.io/badge/ArduPilot-4.3%20to%204.6-e2e518?logoColor=white&style=for-the-badge" height="28" alt="ArduPilot 4.3 to 4.6" /></a>
  <br>
  <!-- <img src="https://img.shields.io/badge/%F0%9F%A4%96-Vibe%20Coded-5B2C6F?style=for-the-badge" height="28" alt="Vibe Coded" />
  <img width="8" /> -->
  <img src="https://img.shields.io/badge/Lines%20of%20Code-90k%2B-1F6FEB?style=for-the-badge" height="28" alt="Lines of code: 90k+" />
  <img width="8" />
  <img src="https://img.shields.io/badge/Tests-69k%20lines%20%C2%B7%20154%20files-18a4de?style=for-the-badge" height="28" alt="Tests: 69k lines across 154 files" />
  <br>
  <img src="https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-6E7681?style=for-the-badge" height="28" alt="Platform: macOS | Linux | Windows" />
  <img width="8" />
  <img src="https://img.shields.io/badge/Offline-First-b91701?style=for-the-badge" height="28" alt="Offline first" />
  <img width="8" />
  <img src="https://img.shields.io/badge/Ready%20to%20Fly-in%20seconds-38c602?style=for-the-badge" height="28" alt="Ready to fly in seconds" />
  <img width="8" />
  <!-- <br> -->
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/License-Sustainable%20Use%201.0-0c5918?style=for-the-badge" height="28" alt="License: Sustainable Use License 1.0" /></a>
</div>

<!--
  The version badge above shows a real number, and it cannot go stale: the
  pre-commit hook that bumps VERSION rewrites that badge from it in the same
  step, so the two are updated together or not at all. Do not hand-edit the
  number, and do not remove the corvus:version-badge marker; the hook finds
  the line by it. VERSION remains the single source of truth; the badge is a
  generated view of it, not a second place to maintain.
-->

<p align="center">
  <img src="assets/screenshot_flight.jpg" alt="Corvus GCS in flight: live map, flight HUD and connection workspace">
</p>

<p align="center"><em>Live map, the floating instrument panel, and the engineering workspace on the right.</em></p>

---

## Contents

- [Contents](#contents)
- [What is Corvus GCS?](#what-is-corvus-gcs)
  - [Fast to ready-for-flight](#fast-to-ready-for-flight)
- [Why this project exists](#why-this-project-exists)
- [Features](#features)
- [Screenshots](#screenshots)
- [Install \& run](#install--run)
  - [The easy way: a ready-made app](#the-easy-way-a-ready-made-app)
  - [From source](#from-source)
- [Connect to your aircraft](#connect-to-your-aircraft)
  - [Run QGroundControl at the same time](#run-qgroundcontrol-at-the-same-time)
  - [One Corvus at a time](#one-corvus-at-a-time)
  - [Who can reach the ground station](#who-can-reach-the-ground-station)
- [Using Corvus](#using-corvus)
  - [The map and the flight HUD](#the-map-and-the-flight-hud)
  - [3D and the globe](#3d-and-the-globe)
  - [Offline maps](#offline-maps)
  - [Mission planner](#mission-planner)
  - [Setup: motors, safety, parameters, calibration, tuning, radio, firmware](#setup-motors-safety-parameters-calibration-tuning-radio-firmware)
  - [Analysis: logs, Flight Review and Telemetry Review](#analysis-logs-flight-review-and-telemetry-review)
  - [The side workspace](#the-side-workspace)
  - [Settings](#settings)
- [Plugins](#plugins)
  - [Adding your own](#adding-your-own)
- [Console commands](#console-commands)
- [Flight-stack compatibility](#flight-stack-compatibility)
- [Get involved](#get-involved)
- [About / Origins](#about--origins)
- [License](#license)

---

## What is Corvus GCS?

**Corvus GCS is a ground control station for autonomous aircraft running PX4 or
ArduPilot.**
It is the software you have open on a laptop while an aircraft is in the air:
it shows you where the vehicle is, what it is doing, and lets you talk to it
over a serial telemetry radio, UDP, or TCP.

It is a Python backend plus a web frontend, wrapped into a single standalone
desktop application. There is no server to deploy and no browser tab to
manage: one window, one process, one clean shutdown.

**What it is used for**

- **Flying and monitoring**: a large live map with the vehicle, its home point
  and its flown track, plus a floating flight-instrument HUD (compass, attitude
  indicator, altitude, speeds, GPS) that stays visible while you fly. Switch it
  into 3D and you get a globe to spin when you zoom out, and when you zoom in,
  ground with its real shape, buildings standing up, and the aircraft flying
  above them at the height it is actually at.
- **Preparing an aircraft**: motor wiring and ESC protocol on a drawing of your
  own airframe, parameters, sensor and ESC calibration, PID tuning, and
  firmware flashing, all from the same window.
- **Understanding a flight afterwards**: download the vehicle's ULog files over
  MAVLink and analyse them locally in the built-in Flight Review.
- **Working in the field**: the interface loads nothing from the internet, map
  tiles are cached into a local offline store, and named map regions can be
  downloaded ahead of time, elevation included. A laptop that has never seen a
  network still gets correct icons, fonts, graphs, maps and terrain.

### Fast to ready-for-flight

> **This is the difference you notice first.** Most ground control stations pull
> the **entire** parameter set off the autopilot the moment they connect:
> a thousand-plus parameters, in full, before the interface is usable. Over a
> 57 kbps telemetry radio that is a wait measured in minutes, every single time
> you power up, and a lossy link can stall or restart the whole transfer.
>
> **Corvus loads only as much as it actually needs.** On connect it streams
> nothing but the telemetry required to fly: position, attitude, GPS, battery and
> link health. The full parameter set is fetched **lazily**: only when you open
> the Parameters page, because that is the only moment it is needed. Nothing
> else in the application waits on it.
>
> The practical effect is that Corvus is **ready to fly in seconds rather than
> minutes**, and it stays reliable on exactly the marginal links where a
> full-set download is most likely to fail. On the flight line, where you are
> standing next to a powered aircraft waiting to launch, that is the property
> that matters most. And when you *do* need parameters, they are one click
> away, with a live progress indicator and an editor that refuses writes while
> armed.

**Who it is for.** Anyone operating a PX4 or ArduPilot aircraft who wants a ground station
that is small enough to read end-to-end, honest about what it is doing, and
built around a field laptop rather than a desk. It is a working tool, not a
demo. Because the codebase stays readable, it is
also a reasonable starting point if you want to build your own ground-station
features on top.

The interface is **operator-oriented**: a map-centred layout, six colour themes
(two light, four dark), one slider that scales the whole interface from 80 % to
150 %, and a collapsible right-hand workspace holding the MAVLink console, an
SSH terminal, and a plugin slot for specialist views.

---

## Why this project exists

The ground stations I used did the job, but they got in the way. They load every
parameter before you can do anything, feel slow on a field laptop, and leave log
review to separate tools. I wanted a station that is ready in seconds, fetches
parameters only when I open them, has a flight reviewer built in, and gets the
small details right, in simulation as much as on a real aircraft. And because
no station covers every setup, you can write your own [plugins](#plugins) and
extend it yourself.

<sub>In my day-to-day work as a PhD researcher there is very little room for
AI-assisted, "vibe-coded" development, because the setting simply does not
allow for it. That method is nonetheless becoming a real part of how software
gets built, and the only honest way to find out where it works, where it
breaks, and how to direct it well is to use it seriously on something
non-trivial.</sub>

---

## Features

Everything here is **built and working today**.

| | What you get | |
|---|---|:--:|
| ⚡ **Ready fast** | On connect Corvus loads only flight telemetry. Parameters are fetched when *you* ask for them, so you are flying in seconds, not minutes | ✅ |
| 🛩️ **Fly** | Live map, floating flight HUD, arm / takeoff / land / RTL, flight-mode selection, on-screen joystick and arrow-key control with adjustable key strength | ✅ |
| 🗺️ **Navigate** | 6 map services with 20 layers (four that need nothing, two that take your own API key), plus vehicle heading, home point, the flown track, and click-the-map to fly there or move home | ✅ |
| 🧭 **Plan a mission** | An optional planning screen: draw a start point, a takeoff, waypoints, orbits and a landing on their own map, then read the whole flight as an altitude profile against the real terrain under it, and drag any point's height straight on that chart. Save missions to disk, upload to the aircraft, and fly only when you press the second button | ✅ |
| 🌍 **See in 3D** | A spinnable globe when you zoom out, real terrain relief and extruded OpenStreetMap buildings when you zoom in, and the aircraft drawn at the altitude it is actually flying. Or switch terrain and buildings off and keep just the camera tilt, for a slow link or a low battery | ✅ |
| 📴 **Work offline** | Nothing loads from the internet. Download named map areas in advance and the whole app keeps working with no connection | ✅ |
| 📡 **Connect** | Connects on its own to whatever is plugged in (flight controller on USB first, then telemetry radio, then the simulator port), plus serial, UDP and TCP by hand, a live port picker, saved recent connections, link-quality display and automatic reconnect | ✅ |
| 🛰️ **RTK GPS** | Plug a base station into this computer and Corvus finds it, surveys it in, and streams RTCM 3 corrections to the aircraft. No port to pick, no protocol to know. An NTRIP caster works the same way, over the network instead of a second receiver | ✅ |
| 🔧 **Set up** | Airframe drawn to scale: click a motor to wire, position or spin-test it; ESC protocol; parameter editor with import / export; guided sensor calibration, ESC calibration, PID tuning by hand or by in-flight autotune, and firmware flashing for both stacks, with every build target of a release laid out as a searchable list grouped by vendor, your own board detected and put first, and the developer builds folded away until you ask for them | ✅ |
| 🛡️ **Set limits** | Maximum distance and height, the return-to-launch profile, and a failsafe action for every loss the autopilot can detect, plus a distance sensor or optical-flow camera brought up by one switch, driver and estimator together | ✅ |
| 📊 **Review flights** | Download the vehicle's logs and record the live stream, then read either on your own machine: Flight Review for a ULog, Telemetry Review for the recording that exists even when the ULog does not, including the radio link, which an onboard log cannot see | ✅ |
| 🖥️ **Tools** | MAVLink console, SSH terminal to an onboard companion computer, and an extensible plugin system. The Vibration Monitor and the SSH Launcher ship with it, and dropping a folder in adds your own | ✅ |
| 🎨 **Personalise** | Six colour themes, interface scale from 80 % to 150 %, your own logo, an app-icon switch with an optional backplate, all saved between sessions | ✅ |
| 🔒 **One at a time** | Launching Corvus while it is already running tells you so instead of splitting one serial link's telemetry across two windows. `CORVUS_ALLOW_MULTI=1` is as the escape hatch for a genuine two-aircraft setup | ✅ |
| 💻 **Just run it** | One standalone app for macOS, Linux and Windows. No install, no server, no browser, clean shutdown every time | ✅ |
| 🔔 **Stay current** | Tells you when a newer release is published on GitHub. It never checks while you are flying or when you are offline | ✅ |

**Safety is built in:** parameter writes, motor tests, firmware flashing and ESC calibration
are all refused while the aircraft is armed, and the on-screen controls never
arm, change mode, or override a failsafe. A refused write is never shown as
applied: the control snaps back to the value the aircraft still holds.

---

## Screenshots

<p align="center">
  <img src="assets/screenshot_map.jpg" alt="Full-width map with the workspace collapsed">
</p>

<p align="center"><em><strong>The map is the interface.</strong> Collapse the side panel and the whole
window becomes the operational picture: vehicle, heading, home point and the flown track, with the
instrument panel wherever you put it. It stays there across restarts and page changes.</em></p>

<p align="center">
  <img src="assets/screenshot_motors.png" alt="Setup: the airframe drawn to scale with every motor">
</p>

<p align="center"><em><strong>Your airframe, drawn.</strong> Every motor at its real distance from the
centre of gravity, with its number, its output pin and a spin-direction arrow, so "the front-left one"
is something you point at instead of work out. Click a motor to wire, position or bench-test it.</em></p>

<p align="center">
  <img src="assets/screenshot_safety.png" alt="Setup: safety limits, failsafe actions and sensors">
</p>

<p align="center"><em><strong>Limits and failsafes.</strong> Maximum distance and height, the
return-to-launch profile, and an action for every loss the autopilot can detect. A distance sensor or
optical-flow camera comes up with one switch, driver and estimator together.</em></p>

<p align="center">
  <img src="assets/screenshot_calibration.png" alt="Accelerometer calibration: the aircraft drawn in each position PX4 asks for">
</p>

<p align="center"><em><strong>Guided calibration.</strong> PX4 names the six accelerometer positions in
its own order and in its own vocabulary. It names the side facing <strong>down</strong>, so "up" means
upside down. So each one is drawn instead of named: hold the aircraft the way the bird is held. Every position
is ticked off as PX4 accepts it, the autopilot's own messages run underneath, and a calibration in
progress can be cancelled on the vehicle. Compass, gyro, level horizon, barometer and airspeed work the
same way.</em></p>

<p align="center">
  <img src="assets/screenshot_analysis.png" alt="Analysis: vehicle logs and Flight Review">
</p>

<p align="center"><em><strong>Review the flight.</strong> Download the vehicle's logs, record the live
stream, and open either in the built-in Flight Review, on your own machine, with nothing uploaded
anywhere.</em></p>

<p align="center">
  <img src="assets/screenshot_console.jpg" alt="MAVLink console in the right-hand workspace">
</p>

<p align="center"><em><strong>The side workspace.</strong> A MAVLink console, an SSH terminal for the
companion computer, and a plugin slot, right next to the map.</em></p>

<p align="center">
  <img src="assets/screenshot_dark.jpg" alt="Corvus GCS in a dark theme">
</p>

<p align="center">
  <img src="assets/screenshot_parameters.png" alt="The parameter editor in another theme">
</p>

<p align="center"><em><strong>Six themes, two light and four dark.</strong> Switching is instant, with no
reload and no flash, and the map, the plots and the instruments all follow.</em></p>

<p align="center"><sub>Every screenshot is the real interface driven by real MAVLink: telemetry,
parameters and the flown track all arrive over the wire and are parsed by the same code a flight uses.
The aircraft producing them is simulated, not airborne. The imagery is the Neubiberg test site.</sub></p>

---

## Install & run

### The easy way: a ready-made app

Download the build for your machine and run it. Nothing else is needed: Python,
Qt and every dependency are already inside.

| Platform | File | First launch |
|---|---|---|
| **macOS** (Apple Silicon) | `Corvus_GCS-<version>-macOS-arm64.dmg` | Drag to `/Applications`. The build is not notarized, so the first time use **right-click → Open**. |
| **Linux** (x86_64) | `Corvus_GCS-<version>-x86_64.AppImage` | `chmod +x` it, then run it. Works on a clean Ubuntu/Debian with no system Python or Qt. |
| **Windows** (x64) | `Corvus_GCS-<version>-windows-x64.zip` | Unzip anywhere and run `Corvus GCS.exe`. The build is unsigned, so SmartScreen asks once: *More info → Run anyway*. |

To pass a port and a connection at startup, launch it from a terminal:

```bash
"/Applications/Corvus GCS.app/Contents/MacOS/corvus-gcs" 8000 serial:/dev/tty.usbserial-0001:57600
```

```powershell
& ".\Corvus GCS\Corvus GCS.exe" 8000 serial:COM7:57600
```

On Windows the serial port is a `COM` name rather than a device path. Pick it
from the dropdown on the LINK tab and you never have to type one. Ports past
`COM9` need the escaped form, `\\.\COM12`, which is what the dropdown fills in.

### From source

Requires **Python 3.12+**. One command creates a virtual environment in
`.venv/`, installs every dependency from [`pyproject.toml`](pyproject.toml) and
opens the app:

```bash
./run.sh
```

```bash
./run.sh 8000 serial:/dev/ttyUSB0:57600    # with a connection
```

Later starts reuse `.venv/` and only reinstall when `pyproject.toml` has
changed, so relaunching needs no network. For development you can also run
just the backend and open it in a browser:

```bash
.venv/bin/python serve.py        # -> http://localhost:8000/
```

On Windows, from PowerShell:

```powershell
py -3.12 -m venv .venv
.venv\Scripts\python -m pip install --upgrade pip
.venv\Scripts\python -m pip install --group dev
.venv\Scripts\python corvus\app.py
```

<details>
<summary>Building the standalone apps yourself</summary>

<br>

```bash
./build.sh          # Linux -> AppImage,  macOS -> .app
./build.sh --dmg    # macOS: also produce a .dmg
```

```powershell
.\build-windows.ps1 -Zip    # Windows -> dist\Corvus GCS\ + a versioned .zip
```

`build.sh` dispatches to the platform script for the host you are on and never
pretends to cross-build; Windows is PowerShell, so it has its own entry point
rather than a shell one. All three produce a self-contained bundle carrying a
Python interpreter, Qt and every runtime dependency, named from the
[`VERSION`](VERSION) file.

**Linux** needs Ubuntu/Debian x86_64, `python3` (3.12+), `pip`, `wget` or
`curl`, and ~1 GB of free disk. The first run downloads `appimagetool` and the
Python wheels and caches them under `build/`.

**macOS** needs the Xcode command line tools (`xcode-select --install`) and a
**framework** CPython 3.12+ (Homebrew's `python@3.12` or a python.org install).
A conda interpreter cannot be relocated into a bundle and is rejected with a
clear error. The bundle is ad-hoc signed; set
`CODESIGN_IDENTITY="Developer ID Application: ..."` to sign it properly.

Signing needs a checkout that is *not* inside a cloud-synced folder. iCloud
Drive, OneDrive and Dropbox re-apply `com.apple.FinderInfo` to directories
inside the bundle while the build runs, and `codesign` refuses a bundle
carrying it. Nothing can win that race in place. The build says so when it
hits this, and the way out is to put the artifact somewhere the sync agent is
not:

```bash
CORVUS_DIST=/tmp/corvus-dist ./build-macos-app.sh --dmg
```

**Windows** needs a 64-bit python.org CPython 3.12+ on `PATH` (a conda
interpreter is rejected because its DLLs live outside the prefix and cannot be
packaged) and ~2 GB of free disk for Qt. Unlike the other two the bundle is
built with PyInstaller rather than a hand-relocated interpreter: Windows puts
no constraint on where a DLL may point, so there is nothing for the extra 300
lines to buy. `assets\corvus-gcs.ico` is generated from the logo with Pillow,
which the build installs into its own venv along with PyInstaller (both from
`pyproject.toml`); if that step fails the build still succeeds, with the
default icon.

Build artifacts (`build/`, `dist/`, `*.AppImage`, `*.dmg`, the Windows `.zip`)
are gitignored. CI runs the same scripts, see
[`.github/workflows/build.yml`](.github/workflows/build.yml) and
[`.gitlab-ci.yml`](.gitlab-ci.yml).

</details>

---

## Connect to your aircraft

Usually you do not have to. Corvus looks for a link at launch and takes the
first one it finds: a flight controller on a **USB cable**, then a **SiK
telemetry radio**, then the **ground-station UDP port**, which is where PX4
SITL publishes, so a simulator connects with no clicking either. The LINK tab
says which rule won with an `AUTO` chip beside the status, and `AUTO ·
SCANNING` while it is still looking.

Two things it will not do. It never takes a link that is up: plug a second
device in mid-flight and it offers you a row with a **Connect** button instead
of switching, because the link it would replace may be carrying an aircraft
that is flying. And once *you* pick a link, it stops picking for that
session, then starts fresh at the next launch, so tomorrow's cable wins again
on its own. A connection string on the command line always wins outright.

To turn any of it off, add an `autoconnect` block to `~/.corvus/config.json`:

```json
{ "autoconnect": { "enabled": true, "usb": true, "sik": true, "udp_fallback": true } }
```

Open the **LINK** tab in the side panel to connect by hand at any time. This is
the normal way to override the automatic choice. A command-line connection
string is only for scripting. Serial and UDP/TCP share one card and one **Connect** button; the
switch at the top of it chooses which, and it opens on whichever kind you
connected with last.

- **Serial**: choose the port from the dropdown (it is re-read every time you
  open the tab, so a radio plugged in after launch just appears) and a baud
  rate. 57600 is the default, labelled for the Holybro SiK Radio V3. The port
  you last flew stays on the list even when it is unplugged, marked *not
  connected*, so an empty dropdown is never the whole answer.
- **UDP / TCP**: type a connection string such as `udp:0.0.0.0:14550`, or use
  one of the presets for the endpoints PX4 publishes. Presets fill the field
  rather than connecting outright, so you can edit before you commit.
  `udp:`/`udpin:` bind and wait; `udpout:` dials out, which is what a
  **mavlink-router** `UdpEndpoint` in `Mode = Server` needs (there the router
  binds and the station speaks first), and what reaches a router behind NAT.
  `tcp:` connects to a `TcpEndpoint`, `tcpin:` listens for one.
- **Recent connections** sit at the top of the tab, one click to reuse, and are
  kept exact: a `:57600` and a `:115200` link to the same port are different
  entries, because collapsing them would silently reconnect at the wrong baud.
  The newest is also the tab's memory: the card reopens on that kind, port and
  baud, so a laptop opened at the field is one button from the link it had.
- **Disconnect** frees the radio without quitting the app, and works *during* a
  connection attempt too, which is exactly when a retry loop needs stopping.

### Run QGroundControl at the same time

A serial port, a USB autopilot and a SiK radio can each be opened by exactly
one program, so a second ground station has always meant closing the first. It
does not have to. Tick **Mirror this link over UDP** on the LINK tab, then
start QGroundControl. Nothing to configure on either end and nothing to
install: 14550 is the UDP link QGroundControl opens by itself, Corvus mirrors
every frame there from the first one, and the router is inside Corvus and stops
when Corvus does.

Corvus deliberately does *not* bind 14550 itself. It listens on 14551 and
talks first. The distinction is the difference between the feature working and
the feature looking like it works: QGroundControl's default link **binds**
14550 and waits to be spoken to, so a Corvus that binds it too leaves two
listeners and no talker. On Linux the second bind fails and QGroundControl has
no link; on macOS both binds succeed and the more specific socket silently
takes every datagram, so QGroundControl shows a connected link that receives
nothing and neither program reports an error. If something else already holds
14551, Corvus takes any free port instead and says which on the LINK tab.
Mirroring outward needs no fixed local port, and losing the whole feature over
one would be the wrong trade.

By default the second station is a *screen*: telemetry flows out to it and
nothing flows back. **Let it command the aircraft** is a separate switch,
because two stations that can both arm and both change mode is a hazard rather
than a convenience, and only you know whether you want it. Turn it on and
QGroundControl's frames reach the aircraft byte-for-byte (mission uploads,
mode changes, parameter writes), with Corvus still recording the whole stream
to its tlog.

One setting to change on the other station: **give QGroundControl a different
MAVLink system ID.** Corvus deliberately uses the GCS identity 254/190 (system
ID / component ID), and PX4 tracks message sequence numbers per system ID. Two
stations sharing one makes the autopilot report packet loss that is not
happening, and muddies which station a GCS failsafe is about. In QGroundControl
it is *Application Settings → MAVLink → Ground Station system ID*; 255 is a
fine choice. If you forget, the LINK tab says so: Corvus notices a second
station transmitting under 254 and names the setting to change.

Whether you share the link this way or put **mavlink-router** in front of it,
the link then carries more than the aircraft: the other station's heartbeat,
a companion computer, a gimbal, sometimes a second vehicle. Corvus reads only
its own aircraft off that link: another node's heartbeat cannot change the
armed flag or the flight mode, cannot keep a lost aircraft looking connected,
and the command target is picked from the first heartbeat that comes from an
actual autopilot rather than the first heartbeat of any kind.

### One Corvus at a time

Launching Corvus while Corvus is already running brings up a message saying so
rather than a second window. That is deliberate, and it is the same constraint
as the one above: a serial port can be opened by exactly one program *usefully*,
but on Linux and macOS the operating system does not enforce it. Two copies both
open `/dev/ttyUSB0` without either being told anything is wrong, and each one
then reads whatever bytes it got to first, so both windows show the same
aircraft with half its telemetry missing. Attitude updates while position
freezes; a command's acknowledgement arrives in the other window. Neither shows
a disconnect, because from each program's point of view nothing failed.

It is the quietest way this application can be badly wrong, so the second launch
is stopped before it opens anything, and told where the first one is listening.
The forwarder's UDP port, the tile database and `~/.corvus/config.json` are
shared the same way, only less dangerously.

If you really want two (two aircraft, two radios, two separate links on one
laptop), set `CORVUS_ALLOW_MULTI=1` and both will start. Give the second one its
own connection string and its own forwarding port; they still cannot share a
radio, and they will write over each other's settings.

```bash
CORVUS_ALLOW_MULTI=1 ./run.sh
```

The guard is a lock the operating system holds on the running process, not a
file left behind on disk, so a Corvus that crashed, was force-quit, or died
with the machine leaves nothing to clean up. The next launch just works.

Next to the connection state you get **link quality**, not just "connected":
signal strength, heartbeat regularity and receive errors. Connected tells you
the socket is up; link quality tells you whether it is worth flying on.

| State | Indicator |
|---|---|
| Connecting / reconnecting | Yellow dot (last error shown) |
| Connected | Green dot |
| Disconnected | Grey dot |
| Ready to fly | **READY** in a green pill: the autopilot's own preflight checks pass |
| Armed, on the ground | **ARMED** in an amber pill: propellers are live and the aircraft is still within reach |
| Airborne | **FLYING** in a blue pill, from the vehicle's own `EXTENDED_SYS_STATE`, falling back to height above home on firmware that does not send it |
| Preflight failing | Amber **NOT READY**: the autopilot would refuse to arm. The failing checks are counted beside it and listed on hover, and Corvus asks the autopilot for them when it has not said |
| Readiness not reported | Grey **STANDBY**: firmware that does not publish its preflight state |

The three states worth recognising at a glance (cleared to fly, propellers
live, airborne) carry a tinted pill as well as a colour, so they are
distinguishable by shape before the colour is read at all. `NOT READY`
deliberately gets no pill: it is the absence of a clearance, not an active
state.

Notification counts follow the same principle. The badge is **green** when the
board is empty, **blue** when the only unread lines are informational (a normal
flight produces a steady trickle of those), **amber** for a real warning and
**red** for a critical. It used to go amber for anything unread at all, which
taught the operator to ignore the one colour that has to keep working.

<details>
<summary>Holybro SiK Telemetry Radio V3, and simulation</summary>

<br>

On Linux the radio enumerates as **two** devices, e.g. `/dev/ttyUSB0` and
`/dev/ttyUSB1`. The **lower-numbered one** is the MAVLink data port.

| Setting | Value |
|---|---|
| Device path | `/dev/ttyUSB0` (the lower of the two) |
| Baud rate | 57600 (factory default) |
| Connection string | `serial:/dev/ttyUSB0:57600` |

On a serial link Corvus automatically applies conservative MAVLink stream rates
so a 57 kbps radio is not saturated, allows a longer heartbeat timeout (10 s),
and reconnects with backoff if the link drops.

**PX4 SITL** publishes to UDP 14550 (the ground-station link), which Corvus
binds automatically on startup:

```bash
make px4_sitl     # in the PX4-Autopilot directory
```

**Alongside MAVROS / MAVSDK / ROS.** Those bind UDP **14540**, PX4's *onboard*
link, a different socket from the 14550 one Corvus uses. Only one process can
hold a UDP port, so pointing Corvus at 14540 while MAVROS is running (or the
other way round) leaves one of them with no telemetry at all. On a simulator
that looks like a broken vehicle rather than a port clash, which is why Corvus
now names the conflict when the bind fails. Leave Corvus on 14550, and if you
need both on the same endpoint put a **mavlink-router** in front and give each
one its own port.

**What needs a position, and what does not.** Corvus asks the aircraft for
`HOME_POSITION` as soon as it connects, and needs home (or the global position)
only where an altitude has to be *converted* or *set*:

| Action | Without a position reference |
|---|---|
| Takeoff | Waits briefly, then sends it anyway with the altitude field unspecified, so the **vehicle** picks its own configured takeoff altitude. You get a warning, not a refusal. |
| Fly to points | Unaffected. Mission items carry your AGL number directly in `MAV_FRAME_GLOBAL_RELATIVE_ALT`, so nothing has to be converted. |
| Set home | Waits briefly, then **refuses**. This altitude is not being converted for the wire. It is the altitude home will *have*, and home altitude is what RTL descends to. Guessing it would move the landing point vertically as a side effect of dragging it sideways. |

Whether the aircraft can actually take off stays the autopilot's decision; if it
cannot, its own refusal reaches you with a reason attached.

</details>

### Who can reach the ground station

Corvus is a web UI served by a local HTTP server, and that server answers on
**this machine only**. It matters because the API behind the UI is the whole
application: arming, takeoff, a parameter write and a firmware upload are each
one request, and none of them asks who is calling. There is no login, because
the ground station is a program on your laptop rather than a service on a
network, and that is only true for as long as the port stays off the network.

Two things follow from it.

The server binds `127.0.0.1`. On a flight-line hotspot the alternative is every
laptop and phone on that WiFi being able to arm your aircraft. If you genuinely
want a second screen (a tablet running the UI off the same link), set
`CORVUS_BIND` and it will listen where you point it:

```bash
CORVUS_BIND=0.0.0.0 ./run.sh
```

Corvus says so in the log when you do, every launch. Treat that network the way
you would treat handing someone the transmitter.

A website you visit cannot read your telemetry. The API used to answer every
browser with `Access-Control-Allow-Origin: *`, which is not about who can reach
the port. It is about who is allowed to *read the answer*. With the wildcard,
any page open in any tab could fetch `http://localhost:8000/api/state` and read
back position, battery and mode, or pull the SSH host, username and key path
out of the config. The header now names loopback origins only, so another tool
on your own machine still works and the web at large gets nothing.

The page itself is fenced in, with a Content Security Policy. The UI is a local
web page that can arm an aircraft, so the question is not only who can reach the
port but what a script running *inside* the page is allowed to do, and the
answer used to be "anything". The policy now says everything the page loads
comes from this server, and, the line that matters most, that the page may not
talk to any other host: `connect-src 'self'`. If something ever did manage to
inject a script (a plugin you installed, a value that slipped through), it
cannot ship your telemetry, your SSH credentials or your map keys off the
machine. It also refuses to be embedded in another page, so nobody can frame
the UI and trick you into clicking ARM.

SSH to a companion computer remembers host keys, in `~/.corvus/known_hosts`. The
first connection to a new machine is accepted and written down. You rarely have
a known_hosts entry for a companion computer, and being stopped at the field is
worse than the risk. The second one is checked: a key that does not match what
was recorded is refused, which is the case that actually means something. Set
`CORVUS_SSH_HOST_KEYS=strict` to refuse unknown hosts as well.

---

## Using Corvus

### The map and the flight HUD

The map is the centre of the app. Your aircraft is drawn with a **heading cone**
and a white separating ring so it stays visible over any imagery, the **home
point** is a landing-pad mark on the exact coordinate, and the **flown track**
trails behind you in red.

The track survives a link drop. Only an actual vehicle reboot clears it, or the
small clear button in the map's corner. That way a dropout does not erase where
you have been.

**Click anywhere on the map** to open a menu: fly to that position, or move the
home point there.

The **flight HUD** (compass, attitude indicator, altitude, speeds, heading and
satellite count) floats over the map in a window you control. Grab it anywhere
that is not a button and move it, **pin** it so a stray gesture cannot shift it,
shrink it to a compact size, or collapse it away; double-click sends it back to
its corner. The on-screen control pad moves the same way.

Where you put either window stays put: across restarts, across a trip to Setup
or Options and back, and when the side workspace slides open, a window parked
against the right edge travels in with it rather than disappearing behind it.

### 3D and the globe

The cube button on the map's control rail turns 3D on and off. Rest the pointer
on it (or long-press it on a touch screen) and a small panel appears with the
one thing the button cannot say: **which** 3D it gives you:

- **Terrain & buildings on** is the full picture below: elevation relief,
  extruded buildings and the globe. It fetches elevation tiles and building
  footprints.
- **Off** is the camera tilt alone, over flat ground, and nothing downloaded
  that a flat map does not already download. This is the one to pick on a
  laptop that is low on battery, or on a link you would rather keep for
  telemetry, and it is how the switch starts: the expensive picture is a
  choice you make, not one you discover after the download.

The switch chooses; it never turns 3D on by itself, so you can set it before
you press the button. Both the switch and whether 3D was on come back the next
time you open Corvus.

With terrain and buildings on, zoomed out you get **the globe**: the Earth as
a sphere, with an atmosphere around its rim, that you can spin. Zoom in and it
hands over to the close-up view with no mode to switch and nothing to press:
from the whole planet down to the far end of the runway is one continuous
movement.

Close up, the ground has **its real shape**. Terrain comes from an elevation
model, so hills are hills and a valley is a valley, drawn at true scale rather
than exaggerated: what you are judging is clearance, and a hill drawn half again
too tall is a hill you misjudge. **Buildings** are extruded from OpenStreetMap
footprints at their tagged heights, so the things actually in your way are in
your way on screen too.

Your aircraft flies in that scene rather than sliding along the ground. It is
drawn at the altitude it is really at, with a dashed line down to a shadow on
the ground directly beneath it and its height above ground beside it, so "how
high am I over that ridge" is something you can see instead of work out from two
numbers.

Elevation and building data are cached exactly like map tiles, and **Download
offline map** includes the elevation for the area by default. 3D therefore keeps
its relief in the field with no connection. Without a connection over ground you
never downloaded, the terrain flattens and the buildings stay away; everything
else on the map keeps working.

A note on what this is not: the photorealistic 3D buildings in Google Earth are
a licensed product that needs a Google API key and a different renderer. Corvus
draws real footprints at real heights, not photographed models.

### Offline maps

Corvus caches every map tile it fetches into a local store, so ground you have
already looked at keeps working with no connection.

To prepare for a field trip, use **Download offline map** on the map and select
an area. Downloads are **named**, listed, and drawn as outlines on the map, so
"what do I actually have offline?" is a question you can answer at a glance
instead of guessing from coordinates. **Elevation** rides along by default, so
the area you downloaded is an area that still has terrain in 3D; the estimate
shown before you commit includes it.

Panning onto ground you never downloaded stays fast: after a few failed
lookups Corvus stops trying for 30 seconds, so cached tiles render at full speed
and the rest simply stays blank rather than freezing the map. Walking back into
coverage recovers on its own.

### Mission planner

**Off by default.** Turn it on with Settings → Appearance → **Pages**, and
**MISSION** appears under HOME in the left rail. A station that is flown by
hand never needs it, so it is not in anybody's way until it is asked for.

The screen is three regions, and they are the same flight drawn three ways.

**The map**, with a tool bar across the top left (the same bar as the flight
actions on the Home tab, icon over caption) and the usual zoom, fit and layer
controls on the rail at the top right. The layer switcher is the Home map's own,
so it offers the same twelve layers under the same four services and the choice
follows you between the two screens.

It also opens **where the Home map is looking**. Pan or zoom either map and the
other one starts from that view the next time you open it, so the planner and
the flight screen are never maps of two different places. Move the planner's
map and it stays where you put it. The Home map is never dragged around behind
your back, and only your own next move on Home changes what the planner opens
on. Nothing moves the camera by itself: if the mission you were last editing
happens to be somewhere else entirely, the planner says so and leaves the view
alone, and the **fit** button on the rail frames the plan when you ask it to.

Next to the offline-download button there is a **search**. Type a place name and
it finds it (over OpenStreetMap's Nominatim, so that half needs a connection),
or type coordinates and it goes there with no network at all, in whatever
notation you have them in: `48.0802, 11.6410`, `48,0802 11,6410`, `48.5N
11.25E`, `48°04'48.8"N 11°38'27.5"E`. Areas you have already downloaded for
offline use are matched by name too, and listed first, since those are named
after exactly the places you fly from. Picking a result flies there and drops a
mark; press the mark to take it off.

Pick a tool and click:

- **Start point**: where the flight begins. Every altitude in the plan is
  measured from here, which is the same reference PX4 flies a mission in.
- **Takeoff**, **Waypoint**, **Circle** (orbit a point a set number of times),
  **Hold** (circle it for a set time), **Land**, and **Return** (which needs no
  click, since it names no place).

Points are dragged to move them, right-click removes the last one, and Esc puts
the tool rail back to the pointer. A **circle** draws its real radius as a ring
on the map; select it and a grip appears on that ring. Drag it to size the
orbit, and pick which way round it is flown. (PX4 carries the turn direction as
the sign of the loiter radius; Corvus keeps the radius a length and sets the
sign on the way out, because a negative distance is not something anyone should
have to type to turn the other way.)

The plan is drawn in the same amber the **PLAN** button on the Home tab uses
(a dashed route and numbered rings), so a mission you drew and a set of points
you clicked read as the same kind of thing.

**The list**, on the right, is the mission in order: reorder, delete, and edit
any point's exact latitude, longitude, height, orbit radius or hold time by
number. Above it, what the plan actually costs: item count, ground distance,
estimated duration, the highest point, and the closest the route comes to the
ground.

**The altitude profile**, underneath, is the flight seen from the side: distance
flown along the bottom, height above the start point up the side, and the real
terrain under the route drawn beneath it, read from the same elevation tiles 3D
mode uses (and cached with them, so it works offline). Every point of the plan
is marked on it, **and you set each one's height by dragging it there.** The
gap between the line and the ground is the clearance, measured rather than
calculated in your head.

The **offline area download** is on this screen too, working the same map and
the same tile store as the Home tab's. Planning is when you find out which
ground you have no imagery for, so the fix belongs where you found it. An area
downloaded here is an area the Home map already has; there is no second copy
and nothing to keep in step.

Missions are saved to disk as readable JSON (`~/.corvus/missions` by default),
so one can be copied to another laptop or kept with the rest of a job's paperwork.

**Nothing reaches the aircraft until you say so, and flying is a second
decision.** *Upload to vehicle* writes the route and stops. The aircraft holds
a mission it has not been told to run, which is what lets you upload from the
tent and walk out to it. *Upload and fly* is the one that switches to MISSION,
arms and takes off, and it asks first, with any warnings the plan raised in
front of you.

**From vehicle** reads the mission the aircraft already holds into the planner:
one uploaded by QGroundControl, or by Corvus before a reconnect. Anything the
planner has no way to draw (a camera trigger, an altitude above terrain) is
named rather than dropped, since uploading the plan again would take it off
the aircraft. When the aircraft holds a mission that is not the one on screen,
the planner says so and offers to read it.

While the plan on screen is the one on the aircraft, the planner follows the
flight: the leg being flown is drawn in green, the point it is heading for is
ringed, the points behind it fade, and a line above the list says where it is
("Flying to 3. Waypoint", paused, finished). Edit the plan, or let another
station upload a different one, and it stops claiming any of that, because the
aircraft is no longer flying what you are looking at.

### Setup: motors, safety, parameters, calibration, tuning, radio, firmware

- **Motors**: your airframe, drawn. Every motor sits at its real distance from
  the centre of gravity with its number, its output and a spin-direction arrow,
  so "the front-left one" is something you point at instead of work out. The
  body behind them follows the airframe class: arms for a multirotor, a wing and
  tail for a plane, booms plus a wing for a quadplane, a rotor disc and tail
  boom for a helicopter, a chassis for a rover. A rotor that pushes rather than
  lifts (a VTOL's pusher, a plane's tractor) is drawn with a thrust arrow
  instead of a boom, read from its own axis parameters. PX4 stores no wingspan,
  so the outline is schematic and the page says so; only the motors are
  measured.
- Click a motor to select it, then set which output drives it (MAIN, AUX,
  DroneCAN, or the simulated ESCs of a Gazebo SITL airframe, and which pin)
  along with its position and rotation, and the limits of that output: minimum,
  maximum, disarmed and failsafe, which current PX4 keeps per output rather
  than per header. Picking a pin
  that already has a motor on it swaps the two, which is the one-click fix for
  crossed motors; picking one that drives a servo is refused by name rather than
  silently taken. A motor wired to nothing is flagged on the drawing. The
  airframe type and the motor count sit on the same card, next to the picture
  they change, and adding or removing a motor redraws it.
- **Motor test**: spin one motor on the bench to find out which one it is.
  **Remove the propellers first**: the button will not enable until you confirm
  you have, it is refused while armed, the throttle stops at half, and the
  duration is bounded on the *vehicle*, so the motor stops even if the link
  dies. Leaving the page stops a running motor.
- **Output protocol**: DShot150/300/600, OneShot or a plain PWM rate per
  timer group, and the DShot minimum. Reading the whole page costs about a
  hundred parameters, not the full set, so it opens in a second, and only what
  the connected firmware actually reports is shown. The page is honest across
  firmware versions, and across both flight stacks, rather than offering
  settings your board does not have. Every
  field is written back one at a time, confirmed by the aircraft, and the whole
  page is read-only while armed.
- **Safety & Sensors**: the envelope the aircraft is allowed to use, in one
  page: maximum distance and height from home and what happens at the limit,
  the return-to-launch profile (return and descend height, the climb cone, the
  loiter before landing), and a failsafe action for every loss the autopilot can detect:
  RC, data link, position, battery, actuator. The *levels* the battery action
  reacts to are on the Battery & Power page below, with the rest of the pack, so
  that one number has one place it is edited.
- **Distance sensor and optical flow**: a **Sensors** card on the same page,
  because a ground lidar is what half those limits lean on. Each sensor is one
  tile that says whether it is up and what is feeding it, and opens onto its own
  page. There, one switch brings the sensor up: Corvus starts the driver *and*
  tells the estimator to fuse it, which is the step usually missed. A
  rangefinder reading perfectly while the EKF ignores it looks exactly like a
  working sensor. The state line always names both halves, so a half-configured
  sensor cannot look finished.
- **Hardware presets**: you do not own a `SENS_TFMINI_CFG`, you own a TFmini-S.
  One dropdown lists the modules Corvus knows (Holybro H-Flow, Benewake
  TFmini-S, TFmini Plus and TF03, with more to come), each row naming its bus
  and model, because picking "UART" when the module on the bench has a CAN plug
  is the mistake a list like this can actually prevent. Choose one and Corvus
  writes its whole chain: the driver, the estimator, and the numbers off that
  module's datasheet (the height band a flow camera can track in, the noise a
  lidar's accuracy implies). Every parameter it would write is listed with its
  value and its reason *before* anything is sent, a module this firmware cannot
  run says so in the row rather than failing once chosen, and a parameter your
  firmware does not carry is named rather than silently skipped.
- **Custom, and your own parameters**: the first row of that dropdown writes
  nothing and leaves everything to you: the model picker (Lightware, Lidar-Lite,
  Benewake, PMW3901, DroneCAN and the rest, or a sensor arriving over MAVLink),
  the serial port if it needs one, and the settings that are genuinely
  per-airframe: mounting offset, height limits, quality gates. It also stops
  pretending Corvus knows every setting your airframe needs: name any
  parameter and it joins the form, read in the same batch and written by the
  same path as everything else. The list is remembered per browser profile, so
  the row you added because *this* aircraft needs it is still there next time.
- **Battery & Power**: the pack on one page, and the page is split the way the
  job is: the aircraft on the left, everything you can change on the right. The
  left is a battery drawn with the cells it actually has, filled to what is
  left, each cell's voltage under it and the failsafe levels marked along the
  bar, with the live numbers underneath: pack voltage, current, watts, mAh
  used, temperature, and the spread between the best and worst cell, which is
  the first thing a tired pack tells you. The right is what is configurable:
  cells and capacity, the voltage a cell holds full and empty, the power
  module's divider and amps-per-volt, and the levels the low-battery failsafe
  fires at. Every `BAT_` parameter on PX4 and every `BATT_` one on ArduPilot
  lives here. That prefix is the whole rule for what is on this page and what
  is on Safety & Sensors, and a parameter your firmware does not carry is one
  row fewer rather than an error.
- **A second opinion on "how much is left"**: the percentage an autopilot
  publishes is a capacity count that starts from a guess, so a pack flown,
  charged to storage and flown again reads full on the second take-off;
  ArduPilot publishes none at all unless a capacity is set. Corvus can read the
  cell voltage against a real discharge curve instead (LiPo, Li-ion or
  LiFePO₄, each with its own table, because 3.3 V a cell is a flat LiPo and a
  nearly full LiFePO₄) and correct it back to rest through the pack's internal
  resistance, since a 6S pulling 60 A through 5 mΩ a cell reads 1.8 V low in a
  climb. Neither reading is right in every case, so both are computed on every
  frame and both are on screen: one switch chooses which one the top bar, the
  map and the logs fly by, and the bar says on hover which one you are reading
  and what the other one thinks. The cell count is worked out the moment the
  battery is plugged in, the one moment that reading is unambiguous (since
  19.8 V is a fresh 5S and a tired 6S), then held for the flight, or pinned by
  hand. A smart or DroneCAN battery that reports its own cells is believed over
  any of this, and its weakest cell is drawn where you can see it.
- **Check values**: every page above writes a field the moment you change it,
  and a write can be lost on a radio or refused by the aircraft. *Check values*
  reads each field back from the aircraft (never from Corvus's own copy),
  writes again whatever you set that did not stick, and redraws the page from
  what the aircraft holds now, with the outcome listed field by field. It is on
  Safety & Sensors, Motors, PID Tuning, Radio Control, Battery & Power and
  Remote ID.
- **Parameters**: the full set is downloaded only when you open this page (see
  [Fast to ready-for-flight](#fast-to-ready-for-flight)), with a live progress
  bar. Then you can edit any value; writes are confirmed by the aircraft and
  **refused while armed**. *Export* and *Import* write and read a readable JSON
  file with the airframe and date in the name, and after an import the list
  shows the values it wrote.
- **Reboot autopilot**: some settings are only read when the autopilot starts
  (the airframe, a sensor driver, a serial port, the output protocol), and an
  accelerometer or compass calibration takes effect after a restart. The
  Parameters page, and a calibration that needs it, offer the reboot; it asks
  first and is refused while armed.
- **Sensor calibration**: a guided wizard for compass, gyro, accelerometer,
  level horizon, airspeed and baro. The aircraft is drawn in the position the
  autopilot is asking for, each orientation is ticked off as it completes, and a
  running calibration can be cancelled on the vehicle. PX4 recognises each
  position by itself; ArduPilot asks to be told, so on an ArduPilot aircraft a
  confirm button appears under the figure and the calibration waits for it.
- **Motor / ESC calibration**: behind a safety confirmation, because motors
  spin at full PWM. **Remove the propellers first.** Refused while armed.
- **Radio Control**: the transmitter in your hands, on one page, and drawn
  there. At the top is a labelled diagram of a twin-stick handset (two
  thumbsticks, six switches, two knobs, every one of them named out in the
  margin), and it moves with your own radio: push a stick and the thumbstick
  travels across its well, flip a switch and its handle swings to the position
  it is in and lights up, turn a knob and its pointer follows. So "is the radio
  even talking, and is that switch the one I think it is" is answered by
  looking at the thing rather than by a test flight, with no props on and
  nothing armed. Click any control, on the drawing or on its label, to see the
  channel it sends on, **learn** one by moving it, and give that channel a job:
  arm, kill, return, hold, an AUX passthrough. Choose your stick mode (1 to 4) so
  the drawn sticks are where yours are. Underneath sit the live channel bars
  and the full configuration: which input the vehicle accepts and what it does
  when the transmitter goes quiet, the stick channels, the flight-mode switch
  with its six positions, every other switch the autopilot can bind, and the
  auxiliary channels. Next to every channel picker is **Detect**: press it, move the
  switch, and Corvus binds the channel that moved. A channel bound to two
  actions at once is flagged, because both stacks permit it and a kill switch
  sharing the mode switch's channel fires on a mode change.
- **Radio calibration**: a guided wizard, and on this page the wizard *is* the
  calibration: neither stack has an autopilot-side RC procedure, so a ground
  station has to watch the channels while you sweep every control and write the
  endpoints it saw. Centre the sticks, sweep everything through its travel, then move one
  named stick at a time. The channel that answers is the one that gets bound,
  and the direction it moved decides whether the channel has to be reversed. Nothing is
  written until you have seen the whole measurement, and a channel that never
  really moved is refused by name instead of being written as a stick that
  works like a switch. The drawn handset comes along for the ride: each step
  draws an arrow inside the gimbal it wants and points it the way to push, and
  during the sweep every control whose channel has already travelled far enough
  is marked off, so what is left to move is something you can see rather than
  count. The six mode positions light up live, so you can check the order
  before you take off rather than in the air.
- **PID Tuning**: its own page, split the way the controller is: rate,
  attitude, velocity and position, one tab per loop, innermost first. Every
  gain is editable by hand and written back one at a time, confirmed by the
  aircraft. Autotune only covers two of the four loops, so a page that offered
  nothing else could not tune a vehicle. Each tab plots what the controller
  *asked for* beside what the airframe *did*, because a response trace on its
  own cannot tell a gain that is too low from one that is too high. Only the
  loops the connected firmware reports are shown, so a multicopter and a fixed
  wing each get their own.
- **Autotune**: both stacks tune the rate and attitude controllers **in
  flight**: the autopilot injects steps and measures what comes back, so the
  vehicle has to be armed and hovering. The page says that up front, states the
  preconditions before you take off, and refuses to start on the ground. On PX4
  it is a command, and the page follows its progress to a stop button that works
  throughout; PX4 v1.16 to v1.18 expose no separate roll, pitch or yaw selection.
  On ArduPilot the autotune *is* a flight mode, so starting it switches the
  vehicle into AUTOTUNE, stopping it returns to the mode you were in, and the
  autopilot narrates the run in the console.
- **Telemetry Radio**: program a SiK radio pair the way Mission Planner's SiK
  Radio page does, without leaving Corvus. Pick the port, press Load, and both
  radios come back side by side: the one on your cable, and the one on the
  aircraft read through it over the air. Network ID, air data rate, transmit
  power, MAVLink framing, the hopping band, duty cycle and listen-before-talk
  are all editable, each with what it actually costs you written next to it.
  Eight of those settings have to be identical at both ends or the radios
  cannot hear each other at all, so a pair that disagrees is called out by
  name, with both values, and one button stages the near radio's values into
  the far column. Nothing is written until you press Save, and then the far
  radio is written **first**, because it is only reachable through the near
  one. Air rates and transmit powers that the radio would silently round up are
  offered as the list it actually supports rather than as a free number, and
  both radios are read back afterwards so you see what took rather than what
  was sent. A radio at a baud rate you no longer remember is found by trying
  the rest; change the near radio's baud and Corvus moves the link to match.
  Configuring the radio that carries the live link interrupts telemetry for the
  few seconds a session takes (Corvus stops the link itself and reconnects),
  and a radio on any other port does not touch the link at all. Refused while
  armed.
- **RTK GPS**: centimetre positioning, set up by plugging it in. An RTK base
  station connected to this computer is found on its own, surveyed, and put on
  the link without the page being opened: Corvus recognises a GNSS receiver by
  its USB descriptor, configures it as a base over UBX, waits out the
  survey-in, then forwards its RTCM 3 corrections to the aircraft as
  `GPS_RTCM_DATA`. The flight controller's own port is never a candidate, so
  the telemetry link is never at risk. The defaults are QGroundControl's:
  survey until the base knows its position to two metres, and for at least
  three minutes. Both are editable, as is a base position you already know
  from a surveyed mark, which skips the survey entirely. A receiver that
  identifies itself is configured; one that does not, but is already streaming
  corrections, is forwarded untouched, so a base set up in u-center or a make
  Corvus has never heard of still works. Where a network is available, an NTRIP
  caster is the other source and needs no second receiver. The page shows the
  whole chain rather than one verdict: which receiver, on which port, how far
  the survey has got and what is holding it up, how many corrections the base
  produced, how many reached the aircraft, and the fix the aircraft ended up
  with. A base that is surveying, a base streaming into a link that is
  down, and a base that is working are three different problems. The receiver
  is put back out of base mode when Corvus lets go of it.
- **Remote ID**: the identity the aircraft broadcasts about you and about
  itself, which is the one page here whose subject is not the vehicle. The
  serial number, the operator registration your authority issued, the flight
  description and the EU class mark are not stored on the aircraft and no
  parameter holds them: they live in the ground station and are pushed down the
  link once a second, and both PX4 and ArduPilot stop arming when that stream
  stops. So this is a pre-flight page, not a one-off setup page.

  It has a switch, and it starts off. An aircraft broadcasting a blank or
  half-filled identity has made a *false* filing, not a small one. Fill in
  **Basic ID** (the airframe's serial number, checked live against the
  ANSI/CTA-2063-A format that is the usual reason a filing is rejected),
  **Operator ID** (your registration; if you paste the whole EU number the page
  tells you the last three characters are the secret half and are never
  broadcast), **Self ID** (a line about what this flight is, and the switch that
  says *emergency* while the aircraft is still in the air), **EU vehicle info**
  (operational category, the C0 to C6 class mark, and the operational volume for a
  Specific-category flight) and the **operator position**: the take-off point,
  or a fixed one you type, with a button that takes the aircraft's own position
  since you are normally standing next to it.

  Pick your region and the page lists what that region's published broadcast
  format asks for and does not have. The FAA's Part 89 and the EU's
  EN 4709-002 want different things, and it says which. Those are advisory:
  nothing is refused, because an aircraft flown under an exemption is not
  misconfigured. A live panel says whether the link can carry the messages at
  all (they are MAVLink 2 only), whether they are actually going out, and the
  arm verdict the aircraft sent back. The aircraft's own Remote ID settings
  (ArduPilot's `DID_` family, PX4's `COM_ARM_ODID`) are at the bottom of the
  same page. The identity stays editable while armed, because a wrong serial
  number on a live aircraft has to be fixable; the vehicle's parameters do not.
  None of it is legal advice, and Corvus certifies nothing.
- **Firmware**: flash PX4 or ArduPilot firmware over a **direct USB connection only**.
  Refused over a telemetry radio or UDP/TCP, and refused while armed.

  Picking the right image is the part that goes wrong, so the boards are a
  list rather than a dropdown: a PX4 release ships around 150 build targets,
  and a control that shows one of them at a time answers neither *"which
  boards can I flash?"* nor *"which do I already have?"*. They are grouped by
  vendor under their real spelling, named by the board rather than the target
  string, and searchable by any of it: type `pixhawk`, `cube` or `v6x` and
  all three land on the row you meant. The board Corvus detected on the USB
  port leads the list, and its vendor's block is first. Images already on disk
  are marked **downloaded** (those flash with no network at all), and a count
  under the list says how many targets the filter is showing out of the
  release's total, and how many of them you hold.

  Two kinds of target are set aside rather than removed. PX4's **developer
  builds** (`_rover`, `_multicopter`, `_debug` and the rest, roughly a third
  of every release) are folded away behind one button, because none of them is
  what an operator flashing an aircraft wants and all of them are reachable in
  a click by whoever came for one. **Peripherals** (the IO coprocessor, CAN
  nodes, GNSS modules, published in the same release with the same `.px4`
  extension) are sorted last and labelled *peripheral*, never hidden: a board
  wrongly called a peripheral is merely listed late, while the reverse would
  be an invitation to write the IO firmware to a flight controller.

  The release list itself is fetched from GitHub once and then read from disk
  forever, which is right in the field and wrong the day PX4 publishes a new
  version, so there is a **Refresh** next to it. It is the only control on
  the page that needs the network on purpose.

### Analysis: logs, Flight Review and Telemetry Review

- **Vehicle logs (ULog)**: browse the flight controller's SD card and download
  logs. Tick several and walk away: they are fetched one after another with a
  progress bar and a Cancel that works throughout. Files land as
  `log_<id>_<UTC date>.ulg`, still readable a month later.
- **Recorded tlogs**: the MAVLink stream Corvus recorded on this laptop, one
  file per flight session, written from the first frame of every connection
  without being asked. This is the log that always exists.
- **Flight Review**: pick a downloaded log and Corvus reduces it to the plots
  that answer *"was that flight healthy?"* (motors, clipping, EKF, battery),
  with findings above the plots and the aircraft's own messages below, filtered
  by severity. **A log that never came through Corvus can be opened too**, from
  anywhere on your machine. Everything is parsed locally; nothing is uploaded.

  The findings name the problem and say where to look. **Each is one line you
  can scan, with the reasoning behind it one click away**. The explanations are
  a paragraph each, and a dozen paragraphs stacked up is a wall that costs you
  the one line that mattered. Problems come first in severity order; context
  that is worth knowing but needs nothing done about it folds away under *"2
  more observations"*. **Every finding carries the second of the flight it was
  measured at and the mode being flown then**, and so does every line of the
  aircraft's own message log, so a claim at the top of the page and the dip in
  the plot that caused it line up.

  Beyond the thresholds, the review reads the things a plot alone will not tell
  you: a motor held at full output and for how long, how long the estimator
  actually rejected a sensor rather than merely how high the innovation spiked,
  in-flight position and heading resets, PX4's own failure detector firing, a
  compass whose field strength rises and falls with the throttle (interference
  in the power wiring, not a calibration to redo), and a rate loop ringing at a
  named frequency and amplitude. Where the ESCs report RPM, they are plotted too:
  commands say what was asked for, RPM says what happened.

  The ground track has **two readings, and a switch in its corner between
  them**. *Chart* is metres north over metres east on equal axes, which is what
  makes the gap between the estimate and the projected GPS measurable.
  *Satellite* lays the same path over the map imagery, at the place it was
  actually flown (the field, the runway, the treeline the turn was flown
  around), through the same tile cache the map pages use, so it works offline
  wherever those tiles were downloaded. A flight with no global reference
  (indoors, or no fix) has no origin to place on the Earth, and offers no
  switch rather than a guess.

  It is equally careful about what it does **not** say, because one alarm about
  something ordinary costs every other finding its credibility. A motor at its
  limit is only reported when the others still had headroom. All of them
  together is a hard climb, not a failure. An estimator reset counts only in the
  air, since every counter ticks when the first GPS fix arrives on the ground.
  Compass interference needs a physically believable swing before the wiring is
  blamed. And a brief innovation spike is filed as an observation rather than a
  warning, because it is only a problem when it lasts.
- **Telemetry Review**: the same page, for a recording rather than a ULog.
  The ULog is the better log and, when you can get it, it is the one to read.
  This exists because you cannot always get it: the card was not fitted, the
  download would take an hour over a 57600 link, or the aircraft did not come
  back. Corvus recorded the flight on this laptop either way, so pick a tlog
  and it is reduced to the same plots, the same mode and armed bands, the same
  findings and the same message log, drawn by the renderer Flight Review
  already uses, so the two read alike.

  What it can and cannot show is stated on the page rather than left for you to
  find out. Telemetry arrives at 1 to 50 Hz, not a ULog's 200 to 1000: an oscillation
  is visible, the shape of one cycle is not. There are no motor outputs and no
  per-IMU data, because those never leave the aircraft. And there is one thing a ULog
  can never have: **the link itself**. RADIO_STATUS, the drop counters and both
  ends' signal strength describe the radio between you and the aircraft, which
  a log written on board has no way of knowing about, including the stretches
  where **the link went silent**, measured against the pace that recording
  actually kept rather than a fixed threshold, so a slow radio is not reported
  as a fault. A recording that **ends with the aircraft still armed** is said
  first and plainly, because that is the log somebody reads when an aircraft did
  not come home. The vehicle's own verdict is read too: the subsystems it
  reported as configured but not healthy, and the glitches its estimator
  flagged.

### The side workspace

A collapsible panel beside the map, so tools never replace the operational
picture.

- **MAVLink console**: your direct line to the airframe. Filter the live
  stream, and read severity off the colour rather than hunting through level
  buttons: errors red, warnings amber, success green, your own commands
  accented, shell replies blue. **Pause** freezes the view while still buffering
  behind it, so reading one message does not cost you the next fifty. Tab
  completes commands, `?` prints the whole command table, history is kept across
  launches, and Copy / Save hand you the visible lines for a bug report.
- **SSH**: a real terminal into an onboard companion computer, not a command
  box: every keystroke goes straight to the remote shell, so `top`, `vim`,
  `sudo` password prompts, colour output and Ctrl-C all behave the way they do
  in any other terminal, and the prompt you see is the machine's own. The
  remote is told the terminal's actual size, and the panel is a few hundred
  pixels wide, so there is a **full-screen** button next to the connection name
  (Shift-Escape leaves it; plain Escape belongs to whatever is running).
  Connections are **saved by name** with password or key-file authentication,
  so reconnecting is one click.
- **Plugins**: specialist views that plug in without touching the core. Two
  ship with it: the **Vibration Monitor**, a live graph of the aircraft's
  vibration levels plus cumulative clipping counters, and the **SSH Launcher**,
  a shelf of one-press buttons that start programs on a companion computer,
  each with its own terminal to watch and stop them in. Both are ordinary
  plugin folders, and you can add your own, see [Plugins](#plugins) below.

### Settings

Reachable from **SET** at the bottom of the left rail. Everything applies
instantly and is saved.

- **Appearance**: six colour themes (two light, four dark), an interface-size
  slider from 80 % to 150 % that scales the entire app, your own company
  logo in the top right, and an **app-icon switch** that flips the desktop
  app's Dock (macOS) / taskbar (Linux) icon between the white mark and the
  black one for a light dock. The icon switch changes nothing but the icon.
- **Map service**: which service and which of its layers. Also switchable from
  the layer control on the map itself. Esri, OpenStreetMap, Google and Bing
  need nothing from you. **MapTiler** and **Mapbox** need an API key, and
  **API keys** is the button that takes one: a free key from either gets you
  licensed satellite imagery and street cartography on your own quota, instead
  of a shared public endpoint. Both are listed whether or not you have a key.
  A service that says "Needs an API key" is something you can act on, one that
  quietly vanished is not.

  The key never reaches your browser. Corvus fetches the tiles itself and hands
  the images to the page, so the key stays in `~/.corvus/config.json` (readable
  by you alone), never appears in a page, a URL or a log line, and is never
  sent back out by the API. The Settings page is only ever told *that* a key
  is saved. You will not see it again after you save it; **REMOVE** clears it.
- **Controls**: three switches, **all off by default**, for the on-screen
  manual controls: a **virtual joystick** (throttle and yaw left, pitch and roll
  right), an **arrow-key pad** for pitch and roll, and a **WASD pad** for
  thrust and yaw. The two key pads share one window and your keyboard's own
  arrow and W/A/S/D keys drive them. One **key-strength** slider (25 to 100 %,
  50 % by default) sets how far a held key pushes the stick (the same strength
  for both pads, arrows and WASD alike), and holding **Shift** flies at twice
  that strength for as long as it is down, on either pad. The slider is the
  cruise, Shift is the dash. Neither touches the sticks, which always
  reach their own stops. They are input sources only. They never arm, change
  mode, or override a failsafe.
- **Pages**: one switch, **off by default**, that adds **MISSION** under HOME
  in the left rail. See [Mission planner](#mission-planner).
- **Flight bar**: one switch, **off by default**. Off, the **ARM / TAKEOFF /
  LAND / RTL / PLAN** bar keeps its full size and narrows only when the row
  genuinely will not fit, the same rule, at the same size, as the Mission
  planner's tool bar, which reaches that point sooner only because its sidebar
  takes half the window. On, the bar starts stepping down as soon as the row
  would cover more than half the map: the buttons drop to the width of their
  own word first, then to icons alone. It is measured on the map itself, so
  opening the side panel narrows the bar just as shrinking the window does.
- **SSH connections**: add, connect and remove saved hosts.
- **Files**: where parameter exports, logs and downloads are written.
- **Plugins**: what Corvus found, and a button that opens the folder you drop
  plugins into. See [Plugins](#plugins).
- **About**: version, the live connection summary, and **Credits** listing
  every bundled dependency and its licence.
- **Updates**: a switch (on by default) that compares the running version
  against the published releases on GitHub and shows a notice when a newer one
  exists, plus **Check now** for an immediate look. Nothing is downloaded and
  nothing about your machine is sent; with no internet the check fails silently.
  The notice never appears while the aircraft is armed, and **Skip this
  version** stops it coming back for that release.

---

## Plugins

The **TOOLS** tab in the side workspace is an extension point: a plugin adds
its own view there without a fork and without touching the rest of the app.

Two ship with Corvus:

- **Vibration Monitor**: a live graph of gyro coning, gyro high-frequency and
  accelerometer high-frequency vibration, with the cumulative clipping counters
  beside it. It asks the autopilot for a higher `VIBRATION` rate while it is
  open and puts the default back when you close it.
- **SSH Launcher**: a shelf of buttons for the programs you start before a
  flight. Each one carries its own saved SSH connection, folder and command, so
  the mission script, the video pipeline and the log recorder are three presses
  rather than three trips to a terminal. Add, rename, edit and remove them from
  the plugin itself; they are saved and there again next launch.

  A button runs its program **in its own SSH session**, and pressing it again
  runs it again **in that same terminal**: one run under the last, in one
  scrollback, with nothing thrown away in between. (It is a real shell: if the
  last program is still running in the foreground, what you send goes to it,
  just as if you had typed it there. Ctrl-C first, or let it finish.)

  The **arrow** beside a button opens that session's terminal, a floating
  window you can move, resize, maximise and put away like any other, while the
  tab you were on stays where it was. Only the arrow opens it; pressing the
  button starts the program without throwing a window at you, which matters
  when four of them go up on the pad. Three buttons are three terminals, side
  by side when you want to see them. Output is right there to read, and Ctrl-C
  (or the window's disconnect button) is how you stop it; closing the window
  with × leaves the program running, and the arrow brings the window back with
  its scrollback intact. A green dot marks the buttons that have something
  running. Turn *Run in a terminal* off for a program that has to outlive
  Corvus itself: it is then started with `nohup` and detached, with nothing to
  watch and nothing to stop from here.

  A button does not need a connection you set up beforehand. The connection
  list in the editor ends in **New connection…**, which opens host, user,
  password and key file right there, so a shelf can be built on the pad, for a
  companion computer Corvus has never seen. What you type is saved as a normal
  SSH connection (it appears in the SSH tab, ready to be picked by the next
  button, edited or removed), named `user@host` unless you give it a name of
  your own.

  Corvus never holds the password in the plugin. A button names a saved SSH
  connection and the backend takes the credentials from there.

### Adding your own

Plugins are folders. Corvus reads two places:

| Where | What it is |
| --- | --- |
| `~/.corvus/plugins` (`%USERPROFILE%\.corvus\plugins` on Windows) | Yours. Survives updates. |
| `plugins/` inside the application | The ones that ship with Corvus, including the two above live here. Replaced by an update. |

**Settings ▸ Plugins ▸ Open plugin folder** opens the first one in Finder /
Explorer / your file manager, and lists what Corvus found. Drop a folder in,
restart, and it is on the TOOLS tab. A plugin in your folder with the same id
as a shipped one replaces it, so you can patch one without editing inside the
app bundle.

One folder per plugin:

```
~/.corvus/plugins/
  my-plugin/
    plugin.json
    my-plugin.js
    my-plugin.css      (optional)
```

`plugin.json` describes it: `id` defaults to the folder name, `scripts`
defaults to `<id>.js`, `icon` is any [Lucide](https://lucide.dev) name, and
`order` decides where the card sits in the grid (lower is earlier, default 100,
ties broken by name):

```json
{
  "id": "my-plugin",
  "name": "My Plugin",
  "icon": "puzzle",
  "description": "What it does, one line.",
  "version": "1.0.0",
  "order": 100,
  "scripts": ["my-plugin.js"],
  "styles": ["my-plugin.css"]
}
```

The script registers itself as it loads and gets a container to build into
plus a small `api`: live telemetry, the MAVLink console, notifications, JSON
requests, and its own saved settings:

```js
Corvus.plugins.register("my-plugin", {
  name: "My Plugin",
  icon: "puzzle",
  description: "What it does, one line.",
  init: function (containerEl, api) { /* build your UI */ },
  destroy: function (containerEl) { /* tear it down again */ },
});
```

The full `api` is documented at the top of `src/js/plugins.js` and the manifest
in `corvus/plugin_registry.py`. Both shipped plugins are complete worked
examples that go through exactly this path: `plugins/ssh-launcher` for a form,
saved settings and a backend call, `plugins/vibration` for a live chart on the
telemetry stream. Copy one and start from there. A plugin that fails to load
costs itself and nothing else: the rest of the app, and the other plugins, come
up regardless.

A plugin is ordinary JavaScript running in the app's own page, and the SSH
Launcher runs whatever command you give it as the account you point it at. Read
a plugin before you drop it in, exactly as you would a script you were about to
run yourself.

---

## Console commands

In the MAVLink console, type:

```
help                            list commands
arm                             arm the vehicle
disarm                          disarm the vehicle
mode AUTO                       set flight mode
takeoff 10                      take off to 10 m
land                            land at current position
rtl                             return to launch
listener sensor_combined        stream a PX4 uORB topic through the MAVLink shell
shell listener sensor_combined  explicitly route the command to the raw PX4 shell
```

Tab completes, `↑` walks history, and `?` prints the full table. It works with
the link down, too.

---

## Flight-stack compatibility

Primary target: **PX4 v1.16, v1.17, v1.18.** Secondary target: **ArduPilot
4.3 to 4.6** for Copter, Plane, Rover/Boat and Sub.

Corvus reads the flight stack and the vehicle type out of the first heartbeat
and behaves accordingly. It detects the firmware version on connect, loads the
matching parameter schema, and degrades gracefully rather than crashing when
something is missing. It refuses to send a command or write a parameter the
connected firmware does not understand. Both stacks are checked before any
parameter, mode or flight-plan change is accepted. Older PX4 firmwares
(v1.12 to v1.15) work on a best-effort basis but are not the focus.

The two stacks are not the same aircraft with different words, and the places
they differ are not cosmetic. A takeoff altitude in the wrong frame turns "ten
metres up" into "ten metres above sea level"; a compass calibration sent in
PX4's parameter slots is acknowledged by ArduPilot and starts nothing. Every
one of those differences lives in one module rather than being spread across
the app:

| | PX4 | ArduPilot |
| --- | --- | --- |
| **Flight mode** | packed main/sub mode | one flat number, read against this airframe's own table |
| **Takeoff** | command, then arm; altitude above sea level | GUIDED, arm, then command; altitude above home |
| **Compass calibration** | `PREFLIGHT_CALIBRATION` | `DO_START_MAG_CAL`, with the GCS confirming each accelerometer position |
| **Autotune** | a command with a progress stream | the AUTOTUNE flight mode |
| **Mission** | items from 0, flown in `MISSION` | item 0 is home, flown in `AUTO` |
| **On-board log** | ULog (`.ulg`) | DataFlash (`.bin`) |

**What ArduPilot does not get.** Three things, and Corvus says so on the page
rather than offering a button that fails:

- **The MAVLink console.** ArduPilot has no NSH shell; the console's flight
  commands still work, the shell verbs are hidden.
- **ESC calibration.** ArduPilot does it by setting `ESC_CALIBRATION` and
  rebooting with the throttle high, which is not something a ground station
  should put behind a button.
- **Flight-log analysis.** DataFlash `.bin` logs download normally and land in
  the same folder, but the review page reads ULog only. It names the format and
  points you at Mission Planner rather than failing obscurely. The tlog review,
  which is recorded by Corvus itself, works on both.

**One thing to set on the aircraft.** ArduPilot only accepts stick input from
the ground station named in `SYSID_MYGCS` (`MAV_GCS_SYSID` from 4.5), and it
drops everything else *silently*. Corvus announces itself as system **254**
rather than the usual 255, deliberately, so it can share a link with
QGroundControl without the two fighting over one id, which means the joystick
does nothing on a stock ArduPilot vehicle. Corvus checks this on connect and
says so, with the number to change; it does not write the parameter itself,
because which ground station may take an aircraft's sticks is your decision.
Flight commands, mode changes and everything else are unaffected.

A second, smaller one: the PID tuning page's *commanded-vs-achieved* overlay
needs the autopilot to stream its setpoints. ArduPilot streams the attitude and
rate targets in any mode, but the position and velocity targets only in GUIDED,
so those two charts stay empty in a manual hover. The gains themselves edit
normally.

Firmware flashing works for both: ArduPilot's `.apj` and PX4's `.px4` are the
same container, and the release picker lists ArduPilot's stable and beta builds
alongside PX4's.

---

## Get involved

**Contributions, bug reports and feature wishes are very welcome.**

This project improves fastest through use. If you fly with it and something is
awkward, slow, or wrong, that is worth reporting. A short description of what
you expected and what happened is enough. The same goes for features: if your
workflow needs something Corvus does not do yet, say so. Wishes are not an
imposition here, they are the most useful input there is.

- **Found a bug?** Open an issue with the autopilot and firmware version, the
  connection type (serial / UDP / TCP), and what you were doing.
- **Want a feature?** Describe the operational problem rather than the solution.
  That usually leads somewhere better.
- **Want to contribute code?** Pull requests are welcome. `AGENTS.md` documents
  the architecture rules and conventions the codebase follows; keeping to them
  is all that is asked.
- **Just have a question?** Ask. Questions about the design decisions are
  particularly welcome, since a lot of them are deliberate.

---

## About / Origins

Corvus GCS is developed with the **Universität der Bundeswehr München** (University
of the German Federal Armed Forces, Munich), in the context of the work at
**Chair LRT 1.1 of Prof. Dr. Matthias Gerdts**.

It is an **AI-assisted project**, built under my direction and review,
and it is built for real field use rather than for demonstrations. See
[Why this project exists](#why-this-project-exists) for what that means in
practice and why the project is set up this way.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/CorvusGCS_logo.png">
    <img src="assets/CorvusGCS_logo_inverted.png" width="96" alt="Corvus GCS logo">
  </picture>
</p>

## License

Corvus GCS is released under the **[Sustainable Use License, Version 1.0](LICENSE.md)**,
a *fair-code* license: the source is open to read, modify and build on, but not
to resell.

In short, and without replacing the terms in [`LICENSE.md`](LICENSE.md):

- **You may** use and modify it for your own internal business purposes, and
  for non-commercial or personal use: flying your own aircraft, research,
  teaching, and building on it are all covered.
- **You may** pass it on, provided you do so free of charge and for
  non-commercial purposes, and that whoever receives it also receives these
  terms. If you modified it, say so prominently.
- **You may not** sell it, or offer it to third parties as a paid product or
  service.

Third-party components (pymavlink, paramiko, pyserial, PyQt6/Qt, MapLibre GL
JS, Plotly, Lucide) keep the licenses of their own authors. The full list, with
each component's license, is in the app under **[Settings > About >
Credits](#settings)**. The license file ships inside the AppImage and the macOS
app as well as in the repository, because the terms have to travel with the
software.

---

<p align="center">
  <sub>Developed with the Universität der Bundeswehr München · Corvus GCS (CGCS)</sub>
</p>

<p align="center">
  <img src="assets/unibw_logo.png" width="200" alt="Universität der Bundeswehr München">
</p>
