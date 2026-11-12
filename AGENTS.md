# Corvus GCS: Agent Context

This file is the shared instructions file for every OpenCode session on this
project (loaded via `.opencode/opencode.json`; default agent `orchestrator`).
Read it first. It defines the product, the non negotiable invariants every
agent must honor, and the conventions that keep a multi agent change
consistent. Role specific duties live in `.opencode/agents/<name>.md`; the
slash commands in `.opencode/commands/` (`/next`, `/gates`, `/audit`) start the
usual jobs.

## Repository layout

```
Cargo.toml  build.rs  rust-toolchain.toml  src/    the Rust program: the egui interface (src/main.rs, one
                                                   folder per area), src/core (the backend), src/link
                                                   (the MAVLink crate), src/api and src/plugins, and
                                                   src/tools (corvus-build: the build, manual and
                                                   language tools, never shipped)
assets/  docs/  VERSION  LICENSE.md  PLAN.md       artwork, website and manual, version, licence, open work
build*.sh  build-windows.ps1  run.sh  release.sh   packaging for the interface plus its backend
android/  .github/  .githooks/  .cargo/           the Android shell, the one CI workflow, the version hook, cargo env
```

One cargo workspace, one `Cargo.lock`, one `target/`, the toolchain pinned by
`rust-toolchain.toml`. Members: the interface crate (`corvus_gui`, the root),
`src/core` (`corvus_core`, the backend), `src/link` (`corvus_link`, MAVLink),
`src/api` (`corvus_api`, what plugins build on), one crate per bundled plugin
in `src/plugins/<id>/`, and `src/tools` (`corvus_tools`, binary `corvus-build`).

Where to look first: `src/README.md` (how the interface, the core, the plugin
API and the 3D map work, and the `CORVUS_GUI_*` switches), `src/PARITY.md`
(where each page stands against the web page it replaced), `src/BACKEND_PORT.md`
(how the backend was ported and the rules that still hold), and `PLAN.md`
(the only list of open work; section 0 is the Rust program).

The Python program (`serve.py`, the `corvus/` backend, the web interface and
its Qt window, the pymavlink parity tests, the simulated aircraft, the build
helpers) was deleted on 2026-11-12. Nothing in the repository needs Python to
build, test or run, no `.venv` is used, and `src/tools/tests/repo.rs` fails if a
build step calls Python or a `.py` file comes back. What the Python answered
lives on as frozen fixtures: `src/core/tests/data` (routes, protocol scenarios,
setup pages, map data, logs) and `src/link/tests/data` (pymavlink's tables,
frames and signing). The Rust code is the reference now; the fixtures are not
regenerated to match it.

## Agent roster

OpenCode is configured here as a primary **orchestrator** that decomposes
work and delegates to specialists. The cross cutting invariants below apply to
all of them. An agent's name is its file name: `backend` is
`.opencode/agents/backend.md`, which also holds its `permission` block, the
machine readable ownership map.

- `orchestrator` (primary): decomposes requirements, assigns subtasks, enforces
  invariants, keeps `PLAN.md` current, owns the `VERSION` override and commits
  completed work. Writes no product code.
- `backend`: `corvus_core` outside the vehicle link, map data and plugin areas:
  module contract, router, state store, HTTP server, SSE and WebSocket,
  settings, the vehicle, mission and geofence routes, the per stack setup page
  schemas, the version route and the core's shutdown.
- `mavlink`: `src/link` (the wire format and the links), the bridge, the
  protocol engines, the dialect layer `src/core/src/dialect.rs` that keeps PX4
  and ArduPilot differences in one place, tlog and ULog, firmware flashing,
  and the radio and companion services (RTK, SiK, SSH, shell, video).
- `gui`: the native egui interface and its process lifecycle, the shared widgets
  of `corvus_api::ui`. Applies the `apple-design` skill.
- `map`: the 2D and 3D map, mission geometry, and the backend's map data
  (offline tile cache, DEM, buildings, geocoding).
- `plugins`: the plugin crates in `src/plugins/`, the registry, the plugin API
  of `corvus_api`, and the MCP server.
- `i18n`: the language files, the translation layer and `corvus-build i18n`.
- `perf`: frame rate, latency, start time, build time, leak hunting; measures
  before and after, owns `corvus-build soak`.
- `doc`: the user manual and architecture text; never changes logic.
- `readme`: the GitHub README, the website and the artwork. Promotes Corvus GCS
  (CGCS); never hardcodes a version.
- `build`: the workspace manifests, toolchain and every platform build
  (AppImage and .deb, macOS `.app` and `.dmg`, Windows folder and zip, Android).
- `devops`: the one CI workflow, `release.sh`, the pre-commit hook.
- `review`: the final safety and reliability audit, and the tests it writes.
- `verify`: runs the real app in a sandbox home (`CORVUS_GUI_*` switches,
  `corvus-build soak`, the stand alone `corvus-core`), reports what it saw, and
  makes the README and website pictures. Edits no product code. There is no
  simulated aircraft any more: a vehicle is a real one or a SITL the operator
  runs.

## Product

**Corvus GCS** (Ground Control Station) is a modern, offline capable ground
station for autonomous aircraft running **PX4 or ArduPilot**. It is one native
Rust program: an egui interface with its backend inside the same process. The
field use case is the driving constraint: it must run on a laptop with no
internet, talk to an autopilot over serial, UDP, TCP or WebSocket, start in
seconds, keep up with a 50 Hz telemetry stream, and never leak threads,
sockets, child processes or file handles on exit.

### Architecture, in one paragraph

`corvus-gcs` (the interface, glow renderer, one window) starts `corvus_core`
in its own process. The core registers modules (vehicle, mission, setup pages,
map data, logs, firmware, peripherals, plugins, MCP) with a router; the
MAVLink bridge, running on `corvus_link`'s transports and the core's own
threads, writes the thread safe **state store**; the interface reads the store
and the push topics and calls routes through `Core::internal_call`, the same
handler, validation and guards an HTTP request gets. An optional loopback
HTTP, SSE and WebSocket server (`corvus_core::http`) serves the same routes to
an AI client (`POST /mcp`), a `--server` client and a second station;
`corvus-core` (`src/core/src/bin/corvus-core.rs`) runs the core without a
window. Everything is torn down on one normal exit path.

MCP (Model Context Protocol, Streamable HTTP, `src/core/src/mcp`,
`src/core/src/http/mcp.rs`, and the window's half in `src/mcp`): off by
default, loopback only, bearer token in `~/.corvus/mcp-token`, one operator
switch per kind of act. Every tool runs an existing route in process, never the
link directly, so an MCP tool cannot do what a button could not. Plugins add
tools from the interface. A new route that changes the vehicle is not
reachable over MCP until a tool and its permission are added there on purpose.

### Tech stack

- **Language:** Rust only, edition 2021, toolchain pinned in
  `rust-toolchain.toml`. Standard library first. A third party crate needs a
  reason in its commit; shared versions and features live once in the root
  `[workspace.dependencies]`, and every build is `--locked`.
- **Interface:** egui and eframe on OpenGL (glow, no wgpu, 24 bit depth buffer
  for the 3D map). Six themes from one token set (`src/api/src/ui/style.rs`),
  fonts and Lucide icons compiled in. It looks like the web interface it
  replaced: sizes, colours, words and behaviour come from there, held by
  `src/PARITY.md` and the tests.
- **Backend:** `corvus_core`, a library of modules behind a router. Blocking
  I/O on `std` threads; no async runtime inside the interface.
- **MAVLink:** `corvus_link`, plain Rust: the `all` dialect generated from the
  XML in `src/link/message_definitions/` at build time, v1 and v2 frames,
  signing, the connections, parameter value encoding. Held to pymavlink's
  frozen answers. The parameter, mission, FTP, log, shell, setup, Remote ID,
  telemetry and flight protocols run in `src/core/src/protocol/`.
- **Maps:** the 2D map draws the core's tile proxy (`/api/tiles/...`, MBTiles
  in SQLite, offline cache); the 3D map has its own terrain, buildings and GPU
  tile cache. The interface never talks to a tile server.
- **Plugins:** crates against `corvus_api`, compiled in. Plugins written for the
  web interface do not run; Settings lists them as "installed, not loaded".
- **Tools:** `corvus-build` (`src/tools`): `appimage-libs`, `bundle-check`,
  `icon`, `png-size`, `squashfs`, `elf-static`, `guide`, `i18n`, `soak`. It
  never enters an artifact.

## Platforms & packaging

Corvus GCS ships as a self contained desktop artifact per platform. Every
artifact is the interface built by cargo with `corvus_core` inside the same
executable; no artifact holds Python. All of them derive their version from the
one `VERSION` read at build time.

`./build.sh` is the one entry point: it dispatches to the platform script for
the current host and fails loudly rather than pretending to cross build.

| Platform | Script | Artifact | Notes |
| --- | --- | --- | --- |
| Linux x86_64 | `./build.sh` -> `build-appimage.sh`, `build-deb.sh` | `dist/Corvus_GCS-<version>-x86_64.AppImage` and `dist/Corvus_GCS-<version>-amd64.deb` | one AppDir (the interface, nothing else that runs) packed twice; `--no-deb` / `--deb-only`; the .deb installs under `/opt/corvus-gcs` |
| macOS (arm64 / x86_64) | `./build.sh --dmg` -> `build-macos-app.sh` | `dist/Corvus GCS.app` (+ `dist/Corvus_GCS-<version>-macOS-<arch>.dmg`) | ad hoc codesigned; macOS 12 is the floor |
| Windows x64 | `.\build-windows.ps1 [-Zip] [-Fast]` | `dist\Corvus GCS\` (+ `dist\Corvus_GCS-<version>-windows-x64.zip`) | `Corvus GCS.exe` beside `VERSION` and `LICENSE.md`; `./build.sh` from a Windows shell points here |

The Android app (`android/`, `./build-android.sh`,
`dist/Corvus_GCS-<version>-android.apk`) is not a fourth bundle of the program.
It is a WebView shell for a Corvus server on the network, and the web interface
it showed is gone, so it has no page to show until it is rebuilt on the Rust
interface or the server serves one (`PLAN.md`). Its `versionName` reads
`VERSION`; no literal lives in `android/`.

CI is GitHub Actions only: `.github/workflows/build.yml` (`rust-gui`,
`rust-windows`, `appimage`, `macos-app`, `windows-app`, `android-app`,
`release`) calls the same `./build.sh` a developer runs locally. There is no
second pipeline and no Python job; do not add either. The AppImage job builds
on `ubuntu-22.04` on purpose: an AppImage links the glibc of its build host, so
a newer Ubuntu silently narrows the machines it runs on.

Shared packaging invariants. A violation of any of these is a build bug:

- **Self contained, and no Python.** The compiled interface with `corvus_core`
  inside it, and nothing else that runs: no CPython, no `pip`, no PyInstaller,
  no `site-packages`, no `serve.py`, no PyO3, no Qt, no web engine. Every build
  script fails on a Python file in the bundle, and `corvus-build bundle-check
  <bundle>` and `.github/scripts/check-bundle.sh` make the same check on a
  finished artifact. The interface opens OpenGL, X11 and Wayland from the
  machine it runs on, as every desktop program does.
- **The interface is built by cargo, locked.** `cargo build --release --locked`
  from the repository root into a target folder under `build/`. Fonts and icons
  are cut out of `assets/` by `src/api/build.rs`; the languages, the version
  and the bundled plugins are compiled in. The executable is the macOS bundle's
  `Contents/MacOS/corvus-gcs`, the AppImage's `usr/bin/corvus-gcs-ui` (started
  by `AppRun`, which `exec`s it so signals reach it) and `Corvus GCS.exe`.
- **Build host.** A Rust toolchain and the platform's own tools, nothing else:
  no Python, no ImageMagick. The Linux helpers are `corvus-build`.
- **Offline at runtime.** The build may download crates and tools once; the
  running app never needs the network unless the operator asks for a download.
- **Layout contract.** `VERSION`, `LICENSE.md` and `assets/licenses/` sit beside
  the executable (`Contents/Resources` in a `.app`). The program reads none of
  them at run time; they travel with it.
- **Licence travels with the software.** `LICENSE.md` ships inside every
  artifact next to `VERSION`. The Sustainable Use License requires that anyone
  who receives a copy also receives its terms, so a bundle without it is a
  licence violation, not a cosmetic omission.
- **Signal transparent launcher.** The launcher `exec`s the interface so
  `SIGINT` and `SIGTERM` reach it; the interface stops the core and joins its
  threads. There is no child backend to kill.
- **No version literal** in any script, launcher, `Info.plist`, desktop entry or
  Gradle file; every one of them reads `VERSION`.
- Artifacts (`build/`, `dist/`, `*.AppImage`, `*.dmg`, `*.apk`) are gitignored
  and never committed.

Adding a platform means adding a `build-<platform>.sh` that honours all of the
above, plus a row in this table and in the README, never a fork of the app code.

## Version control: single source of truth (mandatory)

The app is called **Corvus GCS**. Its version is never hardcoded in more than
one place. Every component reads from the same source, so one bump propagates
everywhere. The version **auto increments on every commit**.

- **Format:** CalVer `YYYY.MM.PP` (e.g. `2026.11.01`): UTC year, zero padded
  month, a per month commit counter that resets to `01` in a new month and
  grows past 99 if needed.
- **Canonical source:** the `VERSION` file at the repository root. Routine
  commits never hand edit it; a manual override is permitted only when the user
  explicitly asks for a specific version.
- **Auto bump:** `.githooks/pre-commit` bumps `VERSION` on every `git commit`
  (same year and month: `PP += 1`; otherwise `PP = 01`; a future dated `VERSION`
  is never downgraded), re-stages it, and rewrites the README's version badge
  from the same value (re staging `README.md` when the badge moved, so an
  unrelated README edit left unstaged rides along). One time setup per clone:
  `git config core.hooksPath .githooks`. Skip for one commit with
  `git commit --no-verify`; the version then does not bump.
- **Rust:** `corvus_core::version` (`src/core/src/version.rs`) reads `VERSION` at
  compile time with `include_str!`. Nothing else carries a version literal.
  A plugin's own `version` in its `Info` is its own and never the GCS version.
- **HTTP and interface:** `GET /api/version` returns `{"product": "Corvus GCS",
  "version": "...", "px4_profile": "..."}`. The window title, About and the
  update check read the compiled in value or that route, never a literal.
- **README:** the badge shows the real number, generated by the hook between the
  `corvus:version-badge` marker comment and its line. Do not hand edit the badge
  or remove the marker. No other version literal is typed into the README.
- **Logs:** tlogs and flight logs record the GCS version in their metadata
  header, so a log is attributable to the build that produced it.
- **Release:** `git commit` (the hook bumps `VERSION`), then `./release.sh`
  (the local gate, tag `v<VERSION>`, push) on the user's explicit word; CI
  builds every artifact from the tag and attaches it to a GitHub Release. A
  release script may verify that no component carries a stale literal.

**Enforcement:** the orchestrator rejects any change that duplicates the version
string outside `VERSION` and `src/core/src/version.rs`. `review` fails any
change that hardcodes one.

## Flight stack compatibility target

The primary target is **PX4 v1.16, v1.17 and v1.18.** The secondary target is
**ArduPilot 4.3 to 4.6** (Copter, Plane, Rover and Boat, Sub). Everything the
`mavlink` and `backend` agents do with parameters, flight modes and commands is
verified against both. Older firmware (PX4 v1.12 to v1.15) is supported on a
best effort basis through fallback tables, never at the cost of a target
version. Betaflight (MSP) and ExpressLRS telemetry are supported where the
dialect layer says so.

- Parameter names, types and existence differ across firmware versions
  (renames, splits, removals in `COM_ARM_*`, `MPC_*`, `MC_*`, `NAV_*`, etc.).
  The app detects the firmware via `AUTOPILOT_VERSION`, loads the matching
  schema, and falls back gracefully when a parameter is absent, never crashes,
  and does not re request it on every page load.
- Never send a `MAV_CMD` or set a parameter the connected firmware does not
  understand. The core refuses to dispatch a command or write that is not in the
  loaded schema for the detected version and returns the reason.
- Parameter values are type exact: `PARAM_VALUE` carries a `MAV_PARAM_TYPE`, and a
  PX4 `INT32` is not a float.
- The target versions are the regression set: any parameter, mode or flight plan
  change is checked against PX4 1.16, 1.17 and 1.18 and against ArduPilot before
  it is accepted.
- What a PX4 audit finding in `PLAN.md` marks **Verify** is read from code and
  not yet seen live. Say so; do not report it as confirmed.
- No hardware tests. Everything is tested against scripted vehicles over real
  UDP or a stub. A live check on a SITL the operator runs is `verify`'s, and its
  report says which stack it saw.

### The dialect layer (mandatory)

`src/core/src/dialect.rs` is the **one** place that knows which stack is on the
other end and what that changes. Nothing else branches on `MAV_AUTOPILOT`: the
bridge, the routes and the interface ask it. A second place that decides "is
this PX4?" is a review failure, because the failure mode of every one of these
differences is a silent wrong answer rather than an error:

| | PX4 | ArduPilot |
| --- | --- | --- |
| Flight mode | packed `main_mode`/`sub_mode` in `custom_mode` | one flat number, per vehicle table (4 is GUIDED on a copter, ACRO on a plane) |
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
| On board log | ULog (`.ulg`) | DataFlash (`.bin`) |

Parameter **names** are not in that module: they belong to the page that renders
them. Each setup page has a schema per stack in `src/core/src/setup_schemas/`
(`safety.rs` and `ardupilot_safety.rs`, `tuning.rs` and `ardupilot_tuning.rs`,
`rc.rs` and `ardupilot_rc.rs`, `motors.rs` and `ardupilot_motors.rs`, battery,
mounting, Remote ID) with the same output shape, so the interface cannot tell
which stack built a page. The route picks one from the connected stack; PX4's
are the default, including for a stack neither dialect recognises.

A capability that a stack lacks is **reported, not hidden**: `GET
/api/mavlink/capabilities` carries the flags, and a control that cannot work is
greyed with the reason on it. "Not available on this autopilot" is an answer an
operator can act on; a missing button is not.

## Process lifecycle & cleanup (mandatory)

The field laptop is rebooted between flights. The app shuts down perfectly every
time: no zombies, no leaked sockets, no unflushed logs.

- Every way out (window closed, `SIGINT`, `SIGTERM`, Ctrl+C on Windows, and the
  stand alone `corvus-core` on its signals) takes the same path: the interface
  stops the core, `Module::stop` runs in reverse order of `start`, every thread
  is joined, every socket and serial port closed, every tlog flushed and closed.
  If the window does not close within 10 s of a signal, the process exits.
- A thread is started in a module's `start` (or owned by a view) and stopped and
  joined in `stop`. A loop sleeps on `Core::wait_stopping` so shutdown never
  waits for it. A detached thread nobody joins is a defect.
- A child process (ffmpeg, a shell, a flasher) gets `SIGTERM`, a timeout, then
  `SIGKILL`, then is reaped.
- A mutex a panic could poison is taken so that a poisoned lock does not take
  the interface down (`net::lock::Relock` in the interface); a panic goes to
  `~/.corvus/gui-crash.log` through `crash.rs`.
- User data goes under `~/.corvus` (`paths.rs`), never next to the executable: a
  packaged build runs from a read only bundle. Settings and plugin files are
  written atomically and tolerate a missing or corrupt file.
- The `review` agent does not green light a change until it has audited the
  shutdown path for it.

## Conventions

- **Language:** all agent output, code comments, docstrings and documentation are
  in **English**.
- **No dashes in prose (mandatory).** User facing text never uses an em dash
  (`—`), and never uses an en dash or a hyphen as a dash between words. That
  covers UI strings, messages the operator sees, the README, the website and the
  docs, and every language file. Use a full stop, comma, colon or parentheses
  instead, and write number ranges with "to" ("2 to 3 min"). A lone `—` standing
  in for a missing value in a data cell is not prose and stays. Keep the tone
  plain and direct, not marketing copy.
- **Comments:** do not add comments unless asked. When you do, keep them short and
  explain *why*, not *what*. Doc comments (`///`) on public items in the style of
  the file are normal. A `// SAFETY:` note on an `unsafe` block states the
  invariant.
- **Rust quality bar.** `cargo clippy --workspace --all-targets --locked -- -D
  warnings` and `cargo fmt` (`rustfmt.toml`: 120 columns) are clean before a
  handoff.
  - No `unwrap`, `expect`, indexing that can panic or `as` casts that can
    truncate on data from a vehicle, a file, the network or a request body: parse,
    check and return an error. A parser never panics on a damaged frame.
  - `unsafe` stays in the files that need it (serial and sockets in `src/link`,
    the pty in `src/core/src/peripherals/shell`, GPU and window glue, the clock
    and local time) and carries a `// SAFETY:` note. Do not spread it.
  - Nothing blocks the frame loop and no page polls: live data is pushed, the
    interface requests a repaint when data arrives.
  - Nothing that grows with flight time is unbounded: histories, buffers,
    caches and textures have a cap and a test that holds it.
  - Hot paths do not allocate or encode per message when it can be done once at
    the push boundary; measure before optimising (`perf`).
  - Errors reach the operator as plain text with a reason, not a log line.
- **Dependencies:** the standard library first. Justify any third party crate;
  shared versions live in the root `Cargo.toml`'s `[workspace.dependencies]`
  (`build` edits it), and every build is `--locked`.
- **Testing:** `cargo test --workspace --locked`. MAVLink parsing, the state
  store, the dialect layer and the routes have tests; a bug fix has a regression
  test that fails without it. Behaviour the Python program defined is held to the
  frozen fixtures (`src/core/tests/data`, `src/link/tests/data`), which are not
  regenerated to match the code: a test that disagrees with one is a bug in the
  Rust until the user says the old answer was wrong. Tests drive scripted
  vehicles over UDP or stubs (`src/core/tests/support/`), never hardware.
- **Interface text.** English source text through the translation layer
  (`corvus_api::i18n::tr`, `tr_fmt`, `on_hover_tr`); `i18n` keeps `de`, `fr`
  and `dl` complete (`corvus-build i18n check`).
- **Attribution (mandatory wording).** Corvus GCS is developed **with** the
  Universität der Bundeswehr München, never *at* it. That preposition is not a
  stylistic choice and not negotiable: it applies to the README, the in app
  credits, docstrings, code comments and **commit messages**. The institution is
  furthermore never named in `LICENSE.md` or in any copyright notice
  (`NSHumanReadableCopyright`, plist, badge, footer); those carry the author,
  Maximilian Böck, and nobody else. A change that reintroduces "at", or that puts
  the institution into a licence or copyright line, is rejected on sight.
- **Commits.** Imperative subject line in the repo's style, then short `*`
  bullets grouped by area. English, plain, no dashes between words. The commit
  carries the author alone: no `Co-Authored-By` trailer, no "Generated with"
  line, no tool names. Only the orchestrator (or the user) commits; never
  `--no-verify`, never amend or force push, never push unless the user asks.

## Definition of done

A subtask is not finished until every applicable box is true. The orchestrator
does not accept a handoff that skips one; `review` fails it.

1. **Scope.** Only the files the subtask owns were touched (the ownership map is
   each agent's `permission` block). Anything outside it was handed back under
   `NEXT`, not edited.
2. **Version.** No new version literal anywhere; every consumer still reads
   `VERSION` -> `corvus_core::version` -> `GET /api/version`.
3. **PX4 and ArduPilot.** Any parameter, mode or `MAV_CMD` change was checked
   against PX4 v1.16, v1.17 and v1.18 and ArduPilot, with a graceful fallback
   when absent, and the stack specific part lives in `dialect.rs` or a per stack
   schema.
4. **Lifecycle.** Every thread, socket, child process and file handle the change
   introduces is started in a module's `start` and torn down in `stop` on the
   `SIGINT` / `SIGTERM` / window close path.
5. **Tests.** `cargo test --workspace --locked` passes; new behaviour has a test,
   fixed bugs have a regression test; no frozen fixture was rewritten to get
   green.
6. **Gates.** `cargo fmt --check`, `cargo clippy --workspace --all-targets
   --locked -- -D warnings`, `corvus-build i18n check` and `corvus-build guide
   --check` are clean.
7. **Packaging.** If the change adds a file, crate or runtime path, every platform
   script still bundles it (`bundle-check` passes) and CI still passes.
8. **Docs.** User visible changes were routed to `readme` (README) and `doc`
   (manual), new strings to `i18n`, and `PLAN.md` lost the item it closed.

## Agent handoff protocol

Every subagent ends its turn with this exact block, so the orchestrator can
chain work without re reading the diff:

```
DONE:    <one line: what changed>
FILES:   <paths touched>
CONTRACT:<routes / SSE or WebSocket topics / state store fields / Core::service
          types / function and trait signatures / corvus_api widgets other agents
          must match, or "none">
CHECKS:  <commands run and their result, e.g. "cargo test: 842 passed">
RISKS:   <what a reviewer should look at hardest, or "none">
NEXT:    <follow up work and the agent that owns it, or "none">
```

Rules that keep the chain honest:

- Report failures verbatim. A skipped check is reported as skipped, never as
  passed.
- If a subtask requires editing a file another agent owns, stop and return it
  under `NEXT` instead of editing across the boundary.
- Never bump `VERSION` or commit; both belong to the orchestrator.
- A change that shows on screen is not "looks right" until `verify` has seen it.

## Commands

```
./run.sh                        # build (dev profile, incremental) and run the native interface, core inside
./run.sh --release              # the optimised build a bundle ships (thin LTO, one codegen unit: slow to build)
./run.sh --fast                 # release code without LTO, for a quick look at real speed
cargo run                       # the same as ./run.sh, from a clean checkout (rust-toolchain.toml is honoured)
cargo run -p corvus_core --bin corvus-core -- --port 8080 --state-dir DIR   # the backend without a window
cargo fmt --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --locked --workspace   # the Rust gates, src/link included
cargo clippy -p corvus_link --all-targets --features tls --locked -- -D warnings   # the tls feature, linted on its own in CI
cargo test -p corvus_core --test <file>   # one integration test file while iterating
cargo build --timings           # where a build spends its time (target/cargo-timings/)
HOME=$(mktemp -d) CORVUS_GUI_SHOT=out.png target/debug/corvus-gcs --page motors   # the window's own pixels in a sandbox home
cargo run -q -p corvus_tools --bin corvus-build -- soak --mode 3d --at 11.6385,48.0795 --minutes 5   # a soak run in a sandbox home, memory sampled
./build.sh [--dmg] [--no-deb] [--fast]   # artifact for the current host (dispatches below); --fast: release code without LTO
./build-appimage.sh [--no-deb|--deb-only|--fast]   # Linux artifact  (x86_64)
./build-macos-app.sh [--dmg] [--fast]    # macOS artifact  (arm64 / x86_64)
.\build-windows.ps1 [-Zip] [-Fast]       # Windows artifact (PowerShell)
cargo run -q -p corvus_tools --bin corvus-build -- guide [--check]   # after editing docs/guide/*.html: rewrite each page's frame (--check: verify only)
cargo run -q -p corvus_tools --bin corvus-build -- i18n check   # the language files in src/lang and src/plugins/*/lang (fmt, new CODE NAME NATIVE)
git config core.hooksPath .githooks   # one time, enables the VERSION auto bump
```
