# Corvus GCS — Agent Context

This file is the shared instructions file for every OpenCode session on this
project (loaded via `.opencode/opencode.json`; default agent `orchestrator`).
Read it first. It defines the product, the non-negotiable invariants every
agent must honor, and the conventions that keep a multi-agent change
consistent.

## Repository layout

```
Cargo.toml  build.rs  rust-toolchain.toml  src/    the Rust program: the egui interface (src/main.rs, one
                                                   folder per area) and, in src/link/, the MAVLink crate
                                                   (its own Cargo.toml, PyO3, built by maturin)
python/                                            tests and tools, no program: tests/ (corvus_link
                                                   against pymavlink, the bundles), tools/ (the
                                                   simulated aircraft scene.py, build helpers),
                                                   corvus/buildings.py (3D map shots), pyproject.toml
assets/  docs/  VERSION  LICENSE.md                shared by both
build*.sh  build-windows.ps1  run.sh  release.sh   packaging for the Rust interface plus its backend
```

The backend is `corvus_core` (`src/core`, open work in `PLAN.md` section 0),
run inside the interface's process. The Python program (`serve.py`, the
`corvus/` backend, the web interface and its Qt window) was deleted on
2026-11-12 once Rust had replaced it; its answers live on as the fixtures in
`src/core/tests/data`. Sections below that still describe the Python backend
or the web page are history: the Rust code is the reference now. Paths in this
file that name `tests/`, `tools/` or `pyproject.toml` are relative to `python/`.

## Agent roster

OpenCode is configured here as a primary **orchestrator** that decomposes
work and delegates to specialists. Role-specific duties live in
`.opencode/agents/*.md`; the cross-cutting invariants below apply to all of
them. References to "Orchestrator" and "Review-Agent" in this file mean those
agents. An agent's name is its file name: `backend` is `.opencode/agents/backend.md`.

- `orchestrator` (primary) — decomposes requirements, assigns subtasks,
  enforces invariants, owns the release `VERSION` bump, and commits completed
  work. Writes no product code except the `VERSION` file.
- `backend` — Vehicle State Store, HTTP/SSE, version endpoint, process
  supervisor, per-stack schema registry.
- `mavlink` — MAVLink v1/v2, autopilot comms, version-aware parameter schema,
  the flight-stack dialect layer (`corvus/autopilot.py`) that keeps PX4 and
  ArduPilot differences in one place, and `src/link`, the Rust module
  that carries the wire format and the links.
- `gui` — web UI, desktop app wrapper, lifecycle/shutdown path.
- `map` — map engine, GIS transforms, offline tile cache, DEM, buildings.
- `plugins` — the plugins in `plugins/`, the plugin registry and the
  `Corvus.plugins` API a plugin sees.
- `i18n` — interface language files, the loader and `tools/i18n.py`.
- `perf` — frame-rate/latency optimization, leak hunting; measures before and
  after.
- `doc` — code/architecture/user documentation only; never changes logic.
  Owns docstrings, the user manual, the offline install guide, and the deep
  API reference.
- `readme` — GitHub-facing README, the project website and the artwork. Owns
  the top-level `README.md`: badges (including "Vibe Coded"), screenshots,
  plain-language project description, quick start, and the "developed with the
  Universität der Bundeswehr München" attribution (see *Conventions* for the
  exact wording rule). Also `docs/index.html`, `script.js`, `style.css` and
  `docs/assets/`. Promotes Corvus GCS (CGCS); never hardcodes a version.
- `build` — packaging/distribution for every supported platform. Owns
  `build-appimage.sh` (Linux AppImage), `build-macos-app.sh` (macOS `.app`
  bundle + optional `.dmg`), `build-windows.ps1` (Windows zip), the bundle
  tools under `tools/` and `pyproject.toml`, and produces reproducible,
  self-contained artifacts after every major change; never hardcodes a
  version.
- `devops` — CI/CD and release automation. Owns the CI pipeline
  (`.github/workflows/build.yml` on GitHub, the only CI), `release.sh` and the
  release pipeline (tag -> AppImage + macOS `.app` + Windows zip -> GitHub
  Release); automates the build after every major change; never hardcodes a
  version.
- `review` — final safety/reliability audit and test authoring.
- `verify` — runs the real app against a simulated aircraft (`tools/scene.py`,
  sandboxed servers), reports what it saw, and makes the README and website
  pictures. Edits no product code.

## Product

**Corvus GCS** (Ground Control Station) is a modern, offline-capable ground
station for autonomous aircraft running **PX4 or ArduPilot**. It is a Python
backend plus a web frontend served from a local HTTP server and wrapped as a
standalone desktop app. The field use case is the driving constraint: it must
run on a laptop with no internet, talk to an autopilot over serial/UDP/TCP, and
never leak processes, sockets, or file handles on exit.

### Architecture, in one paragraph

A Python process hosts a local `ThreadingHTTPServer`, serves the web UI, and
runs the MAVLink connection(s) on background threads. Telemetry flows from the
autopilot → MAVLink parser → thread-safe **Vehicle State Store** → the HTTP
server exposes it to the browser over **SSE** (or WebSockets). The browser
renders the HUD, map, and parameter editor. The same Python process supervises
all child processes (e.g. MAVLink bridges) and guarantees clean teardown.

The same server answers AI clients at `POST /mcp` (Model Context Protocol,
Streamable HTTP, `corvus/mcp.py` + `corvus/routes/mcp.py`): off by default,
loopback only, bearer token in `~/.corvus/mcp-token`, one operator switch per
kind of act. Every tool runs an existing endpoint in-process
(`internal_call`), never the link directly, so an MCP tool cannot do what a
button could not. Plugins add tools from the browser (`mcpTools` in their
spec). A new endpoint that changes the vehicle is not reachable over MCP
until a tool and its permission are added there on purpose.

### Tech stack

- **Backend / wrapper (the Python reference, `python/`):** CPython, standard library only where possible
  (`http.server`, `threading`, `subprocess`, `signal`, `atexit`,
  `collections.deque`, `pathlib`, `json`, `struct`). Add a third-party dependency
  only when the stdlib genuinely cannot do the job, and justify it.
- **MAVLink:** `corvus_link`, written in Rust (`src/link`, PyO3,
  built with maturin). It is a drop-in for pymavlink's interface: the code
  imports `from corvus_link import mavutil` and gets the same `mavutil`,
  message classes, `MAVLink` object and connections, with the `all` dialect
  generated from the XML in `src/link/message_definitions/` at build
  time. pymavlink stays in the `test` group only, as the reference the parity
  tests (`tests/test_corvus_link_*.py`) hold every message, constant, frame and
  link to, and as the simulated vehicle in `tools/scene_kit`. No module under
  `corvus/` imports pymavlink. The parameter, mission, FTP, log and
  shell protocols, the Setup commands, Remote ID, the telemetry handlers and
  the flight commands run in corvus_link too, and so does the bridge's
  receive path, command transaction, status reporting (STATUSTEXT, kill
  switch, preflight report), its own requests and signing, tlog, the vehicle
  list's tracker, the second station forwarder, auto-connect, RTK (with the
  NTRIP client and the base's serial port), SiK radio configuration, the dormant ExpressLRS
  telemetry, firmware flashing and the connection cycle, whose
  threads are
  Rust threads (`corvus_link.params`, `.missions`,
  `.ftp`, `.logs`, `.shell`, `.setup`, `.remote_id`, `.telemetry`, `.flight`,
  `.bridge`, behind the `corvus/mavlink_*.py` module of the
  same name);
  the other protocols are still Python and move one at a time, each held to
  its Python predecessor kept in `tests/reference/`. Keep wire-format parsing and parameter handling
  version- *and* stack-aware (see the compatibility target below).
- **Frontend:** vanilla JS / small framework, MapLibre GL JS or Leaflet for the
  map. Telemetry is pushed from the backend; the frontend never polls for live
  data. It is being replaced by a native egui app (`Cargo.toml` and `src/` at the repository root), a
  client of the same HTTP/SSE API that starts and supervises `serve.py`
  itself (or connects to `--server`), page by page with the same words and
  rules as the web page it replaces. The web interface stays until the
  switch.
- **Maps:** local tile cache (MBTiles/SQLite) for full offline field use.
- **Vendored libraries** live in `src/vendor/` and are dropped in by hand. Two
  of them are patched, both marked `CORVUS-ZOOM-PATCH` and both for the same
  reason: they do their own hit-testing against a client rect, and neither can
  see the CSS `zoom` the interface scale puts on `<body>`. Without the patches
  every chart (`plotly-basic.min.js`) and every terminal selection
  (`xterm.js`) answers the pointer in the wrong place above 100%. Replacing
  either file drops its patch; `tests/test_frontend_vendor_zoom.js` fails when
  that happens and prints the exact edit to re-apply. MapLibre 6 ships only
  as ES modules: `maplibre-gl.mjs` plus the `maplibre-gl-worker.mjs` it starts
  from beside it, loaded by one module script in `index.html` that puts it on
  `window.maplibregl`. It is unpatched: the image queue crash Corvus once
  patched (`CORVUS-QUEUE-PATCH`, an aborted tile request marking an unrelated
  tile failed for good) is fixed upstream since 6.1, and
  `tests/test_frontend_vendor_maplibre_queue.js` fails if an upgrade brings
  the late controller read back.

## Platforms & packaging

Corvus GCS ships as a self-contained desktop artifact per platform. Every
artifact is the native Rust interface (`Cargo.toml` and `src/`, egui, built with
cargo) with the backend (`corvus_core`) inside the same executable; no artifact
holds Python. All of them derive their version from the one `VERSION` read at
build time. The interface runs the core in its own process.

`./build.sh` is the one entry point: it dispatches to the platform script for
the current host and fails loudly rather than pretending to cross-build.

| Platform | Script | Artifact | Notes |
| --- | --- | --- | --- |
| Linux x86_64 | `./build.sh` -> `build-appimage.sh`, `build-deb.sh` | `dist/Corvus_GCS-<version>-x86_64.AppImage` and `dist/Corvus_GCS-<version>-amd64.deb` | one AppDir (the Rust interface, nothing else that runs) packed twice; `--no-deb` / `--deb-only`; the .deb installs under `/opt/corvus-gcs` |
| macOS (arm64 / x86_64) | `./build.sh --dmg` -> `build-macos-app.sh` | `dist/Corvus GCS.app` (+ `dist/Corvus_GCS-<version>-macOS-<arch>.dmg`) | the interface in the bundle, ad-hoc codesigned |
| Windows x64 | `.\build-windows.ps1 [-Zip]` | `dist\Corvus GCS\` (+ `dist\Corvus_GCS-<version>-windows-x64.zip`) | `Corvus GCS.exe` beside `VERSION` and `LICENSE.md` (no PyInstaller, no interpreter); `./build.sh` from a Windows shell points here rather than cross-building |

The Android app (`android/`, `./build-android.sh`, `dist/Corvus_GCS-<version>-android.apk`)
is not a fourth bundle of the app. It is a WebView shell on a Corvus server on
the network, with no Python inside, whose user agent carries `CorvusAndroid` so
the web UI starts in tablet mode (`src/js/touch.js` and the inline script in
`src/index.html` read the same marker). Its `versionName` reads `VERSION` and
no literal lives in `android/`.

CI is GitHub Actions only: `.github/workflows/build.yml` (test + test-windows +
lint + rust + rust-gui + frontend + appimage + macos-app + windows-app + android-app + release) calls the same
`./build.sh` a developer runs locally. There is no second pipeline; do not add
one back.

Windows is the one row whose entry point is not `./build.sh`, because the script
is PowerShell — `build.sh` detects an MSYS/Git-Bash shell and says so rather
than pretending to cross-build. Everything else in this section still applies to
it unchanged: one `VERSION`, the layout contract, `LICENSE.md` inside the
artifact, no version literal.

The AppImage job builds on `ubuntu-22.04` on purpose: an AppImage links
against the glibc of its build host, so building on a newer Ubuntu silently
narrows the machines the artifact runs on.

Shared packaging invariants — a violation of any of these is a build bug:

- **Self-contained, and no Python.** The compiled interface with `corvus_core`
  inside it, and nothing else that runs: no CPython, no `pip`, no PyInstaller,
  no `site-packages`, no `serve.py`, no PyO3 extension. `corvus_link` is
  built without its `python` feature (the root workspace depends on it with
  `default-features = false`). Every build script fails when it finds a Python
  file in the bundle, and `tools/bundle_trim.py check <bundle>` makes the same
  check on a finished artifact (CI runs it on the AppImage, the macOS smoke
  test on the `.app`). No system Python, no conda, no Qt on the target machine.
  The interface opens OpenGL, X11 and Wayland from the machine it runs on, as
  every desktop program does.
- **No Qt in a bundle.** The interface is egui on OpenGL (glow) with a 24 bit
  depth buffer for the 3D map; nothing in a bundle links Qt or a web engine.
  macOS 12 is the floor
  (`MIN_MACOS` in `build-macos-app.sh`, read by both cargo and `Info.plist`).
- **The interface is built by cargo, locked.** `cargo build --release
  --locked` from the repository root (its `rust-toolchain.toml` pins the Rust),
  into a target folder under `build/`. Fonts and icons are cut out of
  `src/vendor` by its `build.rs`, the logos are read from `assets/`, and the
  languages, the version and the bundled plugins are compiled in, so the
  binary carries them. The executable is the macOS bundle's
  `Contents/MacOS/corvus-gcs`, the AppImage's `usr/bin/corvus-gcs-ui` (started
  by `AppRun`, which `exec`s it so signals reach it) and `Corvus GCS.exe`.
  Backend teardown is the interface's normal exit path: the core is stopped and
  its threads joined; there is no child process to kill.
- **Build host.** A Rust toolchain (cargo) and the platform's own tools. The
  Linux script uses `python3` for small helpers (the library bundler
  `tools/appimage_libs.py`, an icon fallback); it never enters the artifact.
- **Offline at runtime.** The build may download wheels and packaging tools
  once; the *running* app must never need the network.
- **Layout contract.** `VERSION`, `LICENSE.md` and `assets/licenses/` sit
  beside the executable (`Contents/Resources` in a `.app`). The program reads
  none of them at run time; they travel with it.
- **Licence travels with the software.** `LICENSE.md` ships inside every
  artifact next to `VERSION`. This is not housekeeping: the Sustainable Use
  License requires that anyone who receives a copy of the software also receives
  a copy of its terms, so a bundle without it is a licence violation, not a
  cosmetic omission. Both platform scripts copy it; neither may stop.
- **Signal-transparent launcher.** The platform launcher (`AppRun`,
  `Contents/MacOS/corvus-gcs`) `exec`s the interface so `SIGINT` / `SIGTERM`
  reach it directly; it sends the backend SIGTERM, then SIGKILL after a grace
  period, and so tears everything down cleanly.
- **No version literal** in any script, launcher, `Info.plist`, or desktop
  entry — every one of them reads `VERSION`.
- Artifacts (`build/`, `dist/`, `*.AppImage`, `*.dmg`) are gitignored and never
  committed.

Adding a platform means adding a `build-<platform>.sh` that honours all of the
above, plus a row in this table and in the README — never a fork of the app
code.

## Version control — single source of truth (mandatory)

The app is called **Corvus GCS**. Its version must never be hardcoded in more
than one place. Every component reads from the same source so a single bump
propagates everywhere automatically. The version **auto-increments on every
commit**, so it tracks every change — small or large — with no manual editing.

- **Format:** CalVer `YYYY.MM.PP` (e.g. `2026.09.01`).
  - `YYYY` — 4-digit year, real-world UTC date.
  - `MM` — 2-digit month, zero-padded; the counter resets to `01` on a new
    month.
  - `PP` — 2-digit per-month commit counter, zero-padded (grows past 99 if a
    month has more than 99 commits).
- **Canonical source:** the `VERSION` file at the repository root holds the
  one version string. This is the *only* place it is stored; it is **not**
  hand-edited for routine commits (see *Auto-bump* below). A manual override is
  permitted only when the user explicitly asks for a specific version.
- **Auto-bump (the mechanism):** `.githooks/pre-commit` bumps `VERSION` on
  every `git commit`. Rule: if `VERSION`'s year-month == today's year-month,
  `PP += 1`; otherwise (new month, first run, or migrated scheme) `PP = 01`
  with today's year-month. A future-dated `VERSION` is never downgraded — its
  counter simply increments. The hook re-stages `VERSION` so the bump ships
  with the commit, and rewrites the README's version badge from the same value
  (re-staging `README.md` when the badge actually moved — so an unrelated
  README edit left unstaged will ride along with that commit). A missing README
  or missing badge marker is a no-op, never a failed commit. One-time setup per
  clone:
  `git config core.hooksPath .githooks`. Skip for a single commit (rare) with
  `git commit --no-verify`; the version then does not bump for that commit.
- **Python:** `corvus/version.py` reads `VERSION` at import time and exposes
  `__version__`, `VERSION` (the string), and `get_version()`. No other Python
  module may contain a literal version string — it imports from `corvus.version`.
- **HTTP:** the backend serves the running version at `GET /api/version` (JSON:
  `{"product": "Corvus GCS", "version": "...", "px4_profile": "..."}`). The
  frontend fetches it once on load; the HUD / About dialog / window title all
  read from that value. Never hardcode a version in HTML or JS.
- **README / marketing:** the top-level `README.md` (owned by the `readme`
  agent) shows the version as a **real number** in a badge at the top —
  `YYYY.MM.PP`, not a pointer to the `VERSION` file. That number is *generated,
  never hand-typed*: the pre-commit hook rewrites it from `VERSION` in the same
  step that bumps it, so the two cannot disagree. `VERSION` is still the single
  source of truth; the badge is a view of it, which is why this is not a second
  place for the number to live.
  - Do not hand-edit the badge number, and do not remove the
    `corvus:version-badge` marker comment — the hook locates the line by it.
  - No *other* version literal is typed into the README.
- **App wrapper:** the desktop wrapper imports `corvus.version` (or reads
  `VERSION`) for the window title, the `--app` window title, the About page,
  and any user-agent string it sets.
- **Logs:** tlogs and flight logs record the GCS version in their metadata
  header so a log is always attributable to the build that produced it.
- **Release:** a release is `git commit` (hook bumps `VERSION`) -> the
  orchestrator tags `v<VERSION>` -> devops/build produce the AppImage from
  that tag. There is no separate hand-edit of `VERSION` for a release. A
  release/build script may verify that no component carries a stale or
  hardcoded version.

**Enforcement:** the Orchestrator rejects any change that duplicates the
version string outside `VERSION` / `corvus/version.py`. The Review-Agent
fails any review where a component hardcodes a version literal instead of
reading the canonical source. See the *Version control* section of each
agent for its specific responsibilities.

## Flight-stack compatibility target

The primary target is **PX4 v1.16, v1.17, and v1.18.** The secondary target is
**ArduPilot 4.3–4.6** (Copter, Plane, Rover/Boat, Sub). Everything the MAVLink
and backend agents do with parameters, flight modes, and commands must be
verified against both. Older firmwares (PX4 v1.12–v1.15) are supported on a
best-effort basis via fallback tables, but they are *not* the focus and must
never block a fix for the target versions.

- Parameter names, types, and existence differ across firmware versions
  (renames, splits, removals in `COM_ARM_*`, `MPC_*`, `MC_*`, `NAV_*`, etc.).
  The app must detect the connected firmware via `AUTOPILOT_VERSION` and load
  the matching parameter schema, then fall back gracefully (never crash) when
  a parameter is absent.
- Never send a `MAV_CMD` or set a parameter that the connected firmware does
  not understand. The backend must refuse to dispatch a command/param write
  that is not in the loaded schema for the detected version.
- The target versions are the regression set: any parameter/mode/flight-plan
  change is checked against PX4 1.16, 1.17 and 1.18, and against ArduPilot,
  before it is accepted.

### The dialect layer (mandatory)

`corvus/autopilot.py` is the **one** place that knows which stack is on the
other end and what that changes. Nothing else branches on `MAV_AUTOPILOT` — the
bridge, the HTTP layer and the frontend ask it. A second place that decides
"is this PX4?" is a review failure, because the failure mode of every one of
these differences is a silent wrong answer rather than an error:

| | PX4 | ArduPilot |
| --- | --- | --- |
| Flight mode | packed `main_mode`/`sub_mode` in `custom_mode` | one flat number, per-vehicle table (4 is GUIDED on a copter, ACRO on a plane) |
| `NAV_TAKEOFF` param7 | altitude **AMSL** | altitude **relative to home** |
| Takeoff order | command, then arm | GUIDED, arm, then command (and disarm again if it is refused) |
| Compass calibration | `PREFLIGHT_CALIBRATION` param2 | `DO_START_MAG_CAL` |
| Accelerometer positions | detected by the firmware | confirmed by the GCS with `ACCELCAL_VEHICLE_POS` |
| ESC calibration | `PREFLIGHT_CALIBRATION` param7 | `ESC_CALIBRATION` parameter + reboot (not offered) |
| Autotune | `DO_AUTOTUNE_ENABLE` | the AUTOTUNE flight mode |
| Mission item 0 | the first item | home |
| Mission mode | `MISSION` | `AUTO` |
| Mission start | `MISSION_START` (first, last), which also arms | arm in GUIDED (AUTO on a plane), then `MISSION_START` with 0, 0 |
| `NAV_LOITER_TURNS` in a mission | unsupported, sent as waypoints around the rim | flown |
| Mission validity | checked after the ACK, `MISSION_CURRENT` NO_MISSION | the ACK is the answer |
| MAVLink shell | NSH over `SERIAL_CONTROL` | none |
| On-board log | ULog (`.ulg`) | DataFlash (`.bin`) |

Parameter **names** are not in that module: they belong to the page that
renders them. Each setup page has a schema per stack —
`safety_config`/`ardupilot_safety`, `tuning_config`/`ardupilot_tuning`,
`rc_config`/`ardupilot_rc`, `motor_config`/`ardupilot_motors` — with the same
output shape, so the frontend cannot tell which stack built a page. The HTTP
layer picks one from the connected stack; PX4's are the default, including for
a stack neither dialect recognises.

A capability that a stack lacks is **reported, not hidden**: `GET
/api/mavlink/capabilities` carries the flags, and a control that cannot work is
greyed with the reason on it. "Not available on this autopilot" is an answer an
operator can act on; a missing button is not.

## Process lifecycle & cleanup (mandatory)

The field laptop is rebooted between flights. The app must shut down
perfectly every time, with no zombies, no leaked sockets, and no unflushed
logs.

- `atexit` handlers **and** `signal` handlers (`SIGINT`, `SIGTERM`) terminate
  every subprocess (`SIGTERM` → timeout → `SIGKILL`) and join every daemon
  thread. This is required of the backend, the app wrapper, and any bridge
  process.
- File handles (tlogs, caches) are flushed and closed in the shutdown path.
- The MAVLink link runs on corvus_link's native threads (`CoreThread`). They
  are joined by `MavlinkBridge.stop()` like any other, and corvus_link's own
  `atexit` hook stops and joins any still running before the interpreter
  finalises. The tlog writer and the parameter workers run on them too
  (`corvus_link.bridge.start_thread`); a native thread left inside a finalising interpreter is ended
  mid-frame. Do not start one that this path does not reach.
- The Review-Agent does not green-light a change until it has audited the
  shutdown path for the change in question.

## Conventions

- **Language:** all agent output, code comments, docstrings, and
  documentation are in **English**. (This replaces the earlier German
  convention.)
- **No dashes in prose (mandatory).** User-facing text never uses an em dash
  (`—`), and never uses an en dash or a hyphen as a dash between words. That
  covers UI strings, messages the operator sees, the README, the website and
  the docs. Use a full stop, comma, colon or parentheses instead, and write
  number ranges with "to" ("2 to 3 min"). A lone `—` standing in for a missing
  value in a data cell is not prose and stays. Keep the tone plain and
  direct, not marketing copy.
- **Comments:** do not add comments unless asked. When you do, keep them
  short and explain *why*, not *what*.
- **Typing:** Python code uses type annotations; public functions carry
  docstrings.
- **Dependencies:** stdlib first. Justify any third-party add. Every Python
  package — runtime, test, lint and build tooling — is named in
  `pyproject.toml`'s `[dependency-groups]` (`headless`, `app`, `test`, `lint`,
  `dev`) and nowhere else; every installer reads it with
  `pip install --group`. No `requirements*.txt`, no conda environment file —
  `tests/test_packaging_single_source.py` fails if one comes back.
  (`requirements.lock` is the one exception, and it is generated, never
  written: a build's resolved set, promoted to pin a release rebuild.)
- **Testing:** `pytest` for unit/integration tests. MAVLink parsers, the
  state store, and HTTP/SSE endpoints must have tests.
- **Attribution (mandatory wording).** Corvus GCS is developed **with** the
  Universität der Bundeswehr München — never *at* it. That preposition is not a
  stylistic choice and it is not negotiable: it applies to the README, the
  in-app credits, docstrings, code comments, and **commit messages**. The
  institution is furthermore never named in `LICENSE.md` or in any copyright
  notice (`NSHumanReadableCopyright`, plist, badge, footer); those carry the
  author, Maximilian Böck, and nobody else. A change that reintroduces "at",
  or that puts the institution into a licence or copyright line, is rejected on
  sight.

## Definition of done

A subtask is not finished until every applicable box is true. The Orchestrator
does not accept a handoff that skips one; `review` fails it.

1. **Scope.** Only the files the subtask owns were touched (see the ownership
   map in each agent file). Anything outside it was handed back, not edited.
2. **Version.** No new version literal anywhere; every consumer still reads
   `VERSION` -> `corvus.version` -> `GET /api/version`.
3. **PX4.** Any parameter / mode / `MAV_CMD` change was checked against v1.16,
   v1.17 and v1.18, with a graceful fallback when absent.
4. **Lifecycle.** Every thread, socket, subprocess and file handle the change
   introduces is torn down on the `atexit` + `SIGINT`/`SIGTERM` path.
5. **Tests.** `python3 -m pytest -q` passes; new behaviour has a test, fixed
   bugs have a regression test.
6. **Packaging.** If the change adds a file, dependency, or runtime path, both
   `build-appimage.sh` and `build-macos-app.sh` still bundle it, and the CI
   pipeline (`.github/workflows/build.yml`) still passes.
7. **Docs.** User-visible changes were routed to `readme` (README) and `doc`
   (manual / API reference).

## Agent handoff protocol

Every subagent ends its turn with this exact block, so the Orchestrator can
chain work without re-reading the diff:

```
DONE:    <one line: what changed>
FILES:   <paths touched>
CONTRACT:<endpoints / SSE event names / state-store fields / function
          signatures other agents must match — or "none">
CHECKS:  <commands run and their result, e.g. "pytest -q: 42 passed">
RISKS:   <what a reviewer should look at hardest — or "none">
NEXT:    <follow-up work and the agent that owns it — or "none">
```

Rules that keep the chain honest:

- Report failures verbatim. A skipped check is reported as skipped, never as
  passed.
- If a subtask requires editing a file another agent owns, stop and return it
  under `NEXT` instead of editing across the boundary.
- Never bump `VERSION` or commit; both belong to the Orchestrator.

## Commands

```
./run.sh                        # build (dev profile, incremental) and run the native interface, core inside, no Python
./run.sh --release              # the optimised build a bundle ships (thin LTO, one codegen unit: slow to build)
./run.sh --fast                 # release code without LTO, for a quick look at real speed
cargo run                       # the same as ./run.sh, from a clean checkout (rust-toolchain.toml is honoured)
cargo fmt --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --locked --workspace   # the Rust gates, src/link included
cargo build --timings           # where a build spends its time (target/cargo-timings/)
./run.sh --venv                 # .venv for the Python tests and tools (pymavlink, corvus_link with its `python` feature)
(cd python && ../.venv/bin/python -m pytest -q)   # the Python tests: corvus_link against pymavlink, the bundles
(cd python && ../.venv/bin/python -m ruff check corvus tests tools)   # lint, as CI runs it
python3 python/tools/scene.py run flight   # a sandboxed corvus-core with a simulated PX4 (needs a cargo build)
python3 src/map3d/shots/run.py  # the 3D map pictures against ref/
./build.sh [--dmg] [--no-deb] [--fast]   # artifact for the current host (dispatches below); --fast: release code without LTO, to try locally
./build-appimage.sh [--no-deb|--deb-only|--fast]   # Linux artifact  (x86_64)
./build-macos-app.sh [--dmg] [--fast]    # macOS artifact  (arm64 / x86_64)
.\build-windows.ps1 [-Zip] [-Fast]       # Windows artifact (PowerShell)
python3 python/tools/guide.py   # after editing docs/guide/*.html: rewrite each page's frame
python3 python/tools/i18n.py check   # the language files in src/lang and src/plugins/*/lang
git config core.hooksPath .githooks   # one-time, enables the VERSION auto-bump
```
