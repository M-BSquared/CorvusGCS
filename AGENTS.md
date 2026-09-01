# Corvus GCS — Agent Context

This file is the shared instructions file for every OpenCode session on this
project (loaded via `.opencode/opencode.json`; default agent `orchestrator`).
Read it first. It defines the product, the non-negotiable invariants every
agent must honor, and the conventions that keep a multi-agent change
consistent.

## Agent roster

OpenCode is configured here as a primary **orchestrator** that decomposes
work and delegates to specialists. Role-specific duties live in
`.opencode/agents/*.md`; the cross-cutting invariants below apply to all of
them. References to "Orchestrator" and "Review-Agent" in this file mean those
agents.

- `orchestrator` (primary) — decomposes requirements, assigns subtasks,
  enforces invariants, owns the release `VERSION` bump, and commits completed
  work. Writes no product code except the `VERSION` file.
- `backend` — Vehicle State Store, HTTP/SSE, version endpoint, process
  supervisor, PX4 schema registry.
- `mavlink` — MAVLink v1/v2, autopilot comms, version-aware parameter schema.
- `gui` — web UI, desktop app wrapper, lifecycle/shutdown path.
- `map` — map engine, GIS transforms, offline tile cache, DEM.
- `perf` — frame-rate/latency optimization, leak hunting.
- `doc` — documentation only; never changes logic.
- `review` — final safety/reliability audit and test authoring.

## Product

**Corvus GCS** (Ground Control Station) is a modern, offline-capable ground
station for PX4-based autonomous aircraft. It is a Python backend plus a web
frontend served from a local HTTP server and wrapped as a standalone desktop
app. The field use case is the driving constraint: it must run on a laptop with
no internet, talk to a PX4 autopilot over serial/UDP/TCP, and never leak
processes, sockets, or file handles on exit.

### Architecture, in one paragraph

A Python process hosts a local `ThreadingHTTPServer`, serves the web UI, and
runs the MAVLink connection(s) on background threads. Telemetry flows from the
autopilot → MAVLink parser → thread-safe **Vehicle State Store** → the HTTP
server exposes it to the browser over **SSE** (or WebSockets). The browser
renders the HUD, map, and parameter editor. The same Python process supervises
all child processes (e.g. MAVLink bridges) and guarantees clean teardown.

### Tech stack

- **Backend / wrapper:** CPython, standard library only where possible
  (`http.server`, `threading`, `subprocess`, `signal`, `atexit`,
  `collections.deque`, `pathlib`, `json`, `struct`). Add a third-party dependency
  only when the stdlib genuinely cannot do the job, and justify it.
- **MAVLink:** pymavlink is the expected MAVLink library. Keep wire-format
  parsing and parameter handling version-aware (see PX4 target below).
- **Frontend:** vanilla JS / small framework, MapLibre GL JS or Leaflet for the
  map. Telemetry is pushed from the backend; the frontend never polls for live
  data.
- **Maps:** local tile cache (MBTiles/SQLite) for full offline field use.

## Version control — single source of truth (mandatory)

The app is called **Corvus GCS**. Its version must never be hardcoded in more
than one place. Every component reads from the same source so that a single
bump on release propagates everywhere automatically.

- **Canonical source:** a `VERSION` file at the repository root holding one
  semver string, e.g. `1.4.2`. This file is the *only* place the version string
  is authored by hand.
- **Python:** `corvus/version.py` reads `VERSION` at import time and exposes
  `__version__`, `VERSION` (the string), and `get_version()`. No other Python
  module may contain a literal version string — it imports from `corvus.version`.
- **HTTP:** the backend serves the running version at `GET /api/version` (JSON:
  `{"product": "Corvus GCS", "version": "...", "px4_profile": "..."}`). The
  frontend fetches it once on load; the HUD / About dialog / window title all
  read from that value. Never hardcode a version in HTML or JS.
- **App wrapper:** the desktop wrapper imports `corvus.version` (or reads
  `VERSION`) for the window title, the `--app` window title, the About page,
  and any user-agent string it sets.
- **Logs:** tlogs and flight logs record the GCS version in their metadata
  header so a log is always attributable to the build that produced it.
- **Release:** bumping `VERSION` is the entire release action for the version
  string; no secondary edits across components. A release/build script may
  verify that no component carries a stale or hardcoded version.

**Enforcement:** the Orchestrator rejects any change that duplicates the
version string outside `VERSION` / `corvus/version.py`. The Review-Agent
fails any review where a component hardcodes a version literal instead of
reading the canonical source. See the *Version control* section of each
agent for its specific responsibilities.

## PX4 compatibility target

The primary target is **PX4 v1.16, v1.17, and v1.18.** Everything the MAVLink
and backend agents do with parameters, flight modes, and commands must be
verified against these three versions. Older firmwares (v1.12–v1.15) are
supported on a best-effort basis via fallback tables, but they are *not* the
focus and must never block a fix for the three target versions.

- Parameter names, types, and existence differ across firmware versions
  (renames, splits, removals in `COM_ARM_*`, `MPC_*`, `MC_*`, `NAV_*`, etc.).
  The app must detect the connected firmware via `AUTOPILOT_VERSION` and load
  the matching parameter schema, then fall back gracefully (never crash) when
  a parameter is absent.
- Never send a `MAV_CMD` or set a parameter that the connected firmware does
  not understand. The backend must refuse to dispatch a command/param write
  that is not in the loaded schema for the detected version.
- The three target versions are the regression set: any parameter/mode/flight-
  plan change is checked against 1.16, 1.17, and 1.18 before it is accepted.

## Process lifecycle & cleanup (mandatory)

The field laptop is rebooted between flights. The app must shut down
perfectly every time, with no zombies, no leaked sockets, and no unflushed
logs.

- `atexit` handlers **and** `signal` handlers (`SIGINT`, `SIGTERM`) terminate
  every subprocess (`SIGTERM` → timeout → `SIGKILL`) and join every daemon
  thread. This is required of the backend, the app wrapper, and any bridge
  process.
- File handles (tlogs, caches) are flushed and closed in the shutdown path.
- The Review-Agent does not green-light a change until it has audited the
  shutdown path for the change in question.

## Conventions

- **Language:** all agent output, code comments, docstrings, and
  documentation are in **English**. (This replaces the earlier German
  convention.)
- **Comments:** do not add comments unless asked. When you do, keep them
  short and explain *why*, not *what*.
- **Typing:** Python code uses type annotations; public functions carry
  docstrings.
- **Dependencies:** stdlib first. Justify any third-party add.
- **Testing:** `pytest` for unit/integration tests. MAVLink parsers, the
  state store, and HTTP/SSE endpoints must have tests.
