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
- `doc` — code/architecture/user documentation only; never changes logic.
  Owns docstrings, the user manual, the offline install guide, and the deep
  API reference.
- `readme` — GitHub-facing README and marketing copy. Owns the top-level
  `README.md`: badges (including "Vibecoded"), screenshots, plain-language
  project description, quick start, and the "developed with the Universität
  der Bundeswehr München" attribution (see *Conventions* for the exact wording
  rule). Promotes Corvus GCS (CGCS); never hardcodes a version.
- `build` — packaging/distribution for every supported platform. Owns
  `build-appimage.sh` (Linux AppImage) and `build-macos-app.sh` (macOS `.app`
  bundle + optional `.dmg`), and produces reproducible, self-contained
  artifacts after every major change; never hardcodes a version.
- `devops` — CI/CD and release automation. Owns the CI pipeline
  (`.gitlab-ci.yml` on the self-hosted GitLab at `git.unibw.de`;
  `.github/workflows/` if a GitHub mirror is added) and the release pipeline
  (tag -> AppImage + macOS `.app` -> release); automates the build after every
  major change; never hardcodes a version.
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

## Platforms & packaging

Corvus GCS ships as a self-contained desktop artifact per platform. Both
artifacts run the *same* `corvus/app.py` (PyQt6 + QtWebEngine wrapper) and both
derive their version from the one `VERSION` read at build time.

`./build.sh` is the one entry point: it dispatches to the platform script for
the current host and fails loudly rather than pretending to cross-build.

| Platform | Script | Artifact | Notes |
| --- | --- | --- | --- |
| Linux x86_64 | `./build.sh` -> `build-appimage.sh` | `Corvus_GCS-<version>-x86_64.AppImage` | `appimagetool`, bundled CPython + Qt |
| macOS (arm64 / x86_64) | `./build.sh --dmg` -> `build-macos-app.sh` | `dist/Corvus GCS.app` (+ `Corvus_GCS-<version>-macOS-<arch>.dmg`) | relocatable framework CPython, ad-hoc codesigned |

Both CI pipelines call the same `./build.sh`: `.gitlab-ci.yml` (primary, on
`git.unibw.de`, Linux only — no macOS runner) and `.github/workflows/build.yml`
(test + frontend + appimage + macos-app + release). They must not drift.

"Must not drift" is checkable, so check it rather than assuming it. The two
have to agree on: the jobs that exist (GitLab carried no `frontend` job for a
while, so the whole browser-side suite went unrun on the primary pipeline), the
apt package set for the AppImage build, and the build distro — an AppImage
links against the glibc of its build host, so building on a newer Ubuntu than
the sibling pipeline silently narrows the machines the artifact runs on.

Shared packaging invariants — a violation of any of these is a build bug:

- **Self-contained.** Bundled CPython + stdlib + PyQt6/QtWebEngine +
  pymavlink/paramiko/pyserial. No system Python, no conda, no Qt install on the
  target machine.
- **Offline at runtime.** The build may download wheels and packaging tools
  once; the *running* app must never need the network.
- **Layout contract.** `VERSION`, `corvus/`, `src/`, and `assets/` stay
  siblings inside the bundle, because `corvus/version.py` and `corvus/server.py`
  resolve them as `Path(__file__).parent.parent / …`.
- **Licence travels with the software.** `LICENSE.md` ships inside every
  artifact next to `VERSION`. This is not housekeeping: the Sustainable Use
  License requires that anyone who receives a copy of the software also receives
  a copy of its terms, so a bundle without it is a licence violation, not a
  cosmetic omission. Both platform scripts copy it; neither may stop.
- **Signal-transparent launcher.** The platform launcher (`AppRun`,
  `Contents/MacOS/corvus-gcs`) `exec`s the bundled interpreter so `SIGINT` /
  `SIGTERM` reach `corvus/app.py` directly and its handlers tear down cleanly.
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
   `build-appimage.sh` and `build-macos-app.sh` still bundle it, and both CI
   pipelines (`.gitlab-ci.yml`, `.github/workflows/build.yml`) still pass.
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
python3 -m pytest -q            # test suite (the gate for every change)
./run.sh                        # conda env create/update + launch desktop app
./launch.sh                     # launch in an existing conda env
python3 serve.py                # backend only, UI in a normal browser
./build.sh [--dmg]              # artifact for the current host (dispatches below)
./build-appimage.sh             # Linux artifact  (x86_64)
./build-macos-app.sh [--dmg]    # macOS artifact  (arm64 / x86_64)
for f in tests/*.js; do node "$f"; done   # frontend assertions
git config core.hooksPath .githooks   # one-time, enables the VERSION auto-bump
```
