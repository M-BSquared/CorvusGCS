# Corvus GCS — Improvement & Correction Plan

> Generated from a full codebase audit. Every item below is a concrete,
> actionable todo. Items are grouped into workstreams; each carries an
> **owner** (the specialist agent responsible), an **interface contract**
> (so parallel work stays consistent), and an **acceptance check**.
>
> Priority order: **A (connection stability) → B (performance) →
> C (design system) → D (offline maps) → E (extensibility) → F (reliability)**.
> Stability and reliability of the drone link are the dominant constraints —
> everything else builds on a link that stays up.
>
> Conventions: English only, stdlib-first, single-source versioning (`VERSION`
> file), PX4 target v1.16/v1.17/v1.18, clean shutdown on every exit.

---

## Workstream A — MAVLink Connection Stability  (owner: `mavlink` + `backend`)

**Root cause of "es wechselt immer bisschen stark":** a single stale heartbeat
(`is_stale`) raises `ConnectionError` → the `_run` loop tears the whole
connection down and reconnects from scratch (close socket → re-handshake →
re-request all streams → re-fetch version). On a lossy 57 kbps radio, a momentary
heartbeat blip causes a full link reset, visible mode/telemetry flapping, and a
2–5 s blackout. There is also **no link-quality metric** (the `uplink` state
field is initialized to `0` and never updated), so the operator and the
reconnect logic are blind to actual link health.

### A1. Add link-quality tracking (RADIO_STATUS + heartbeat jitter)  `mavlink`
- [ ] Parse `RADIO_STATUS` messages in `_dispatch`: extract `rssi`, `remrssi`,
      `txbuf`, `noise`, `remnoise`, `rxerrors`, `fixed`. Normalize to a 0–100
      **uplink quality** score (use `remrssi` = remote signal + `txbuf` headroom;
      SiK radios report relative RSSI, so map via the SiK 1.9 dB scale or a
      configurable curve).
- [ ] Track heartbeat arrival times; compute **heartbeat jitter** and
      **packet-loss** (gaps in `ATTITUDE`/`HEARTBEAT` sequence). Store a rolling
      `deque(maxlen=N)` of inter-heartbeat intervals.
- [ ] Publish into the store: `uplink` (0–100), `uplink_rssi` (dB), `uplink_rxerrors`,
      `uplink_fixed`, `heartbeat_jitter_ms`, `link_quality` (`good`/`fair`/`poor`/`lost`).
- [ ] **Contract:** `VehicleStateStore` gains the fields above (see A2). The
      frontend `telemetry.js` `initState` mirrors them.
- [ ] **Acceptance:** with a SiK radio simulation, `uplink` updates at ~1 Hz and
      `link_quality` reflects `good` while the link is up and degrades to `poor`
      before `lost`. Unit test in `tests/test_mavlink_linkquality.py`.

### A2. Extend VehicleStateStore with link-quality fields  `backend`
- [ ] Add `uplink`, `uplink_rssi`, `uplink_rxerrors`, `uplink_fixed`,
      `heartbeat_jitter_ms`, `link_quality` to the `_data` dict (defaults:
      `uplink=0`, `link_quality="unknown"`).
- [ ] Keep `_sanitize` behaviour (no NaN to the wire).
- [ ] **Acceptance:** `GET /api/state` returns the new fields; existing tests pass.

### A3. Replace hard heartbeat-timeout disconnect with graceful degradation  `mavlink`
- [ ] Introduce a **two-tier** link state machine:
  - `healthy`: heartbeat fresh (< timeout).
  - `degraded`: heartbeat stale (between `warn_timeout` and `drop_timeout`) —
    keep the socket open, keep parsing, mark `link_quality="poor"`, do NOT
    reconnect. Stream rates stay as-is.
  - `lost`: no heartbeat for `drop_timeout` (serial 15 s, UDP 8 s) — only then
    tear down and reconnect.
- [ ] Make `is_stale` checks in `_receive_loop` set the degraded state instead of
      raising immediately; only raise `ConnectionError` once `drop_timeout` is
      exceeded. Add a `_last_heartbeat_received` wall-clock guard separate from
      the `warn` threshold.
- [ ] Surface `link_status="degraded"` through the store so the UI shows a yellow
      "DEGRADED" indicator without a full disconnect flap.
- [ ] **Acceptance:** a 3-second heartbeat gap on UDP no longer triggers a
      reconnect; telemetry resumes seamlessly when heartbeats return. Test in
      `tests/test_mavlink_reconnect.py` simulating a heartbeat gap.

### A4. Smarter reconnect with backoff + jitter  `mavlink`
- [ ] Replace the fixed UDP `2.0` s and linear serial backoff with exponential
      backoff + jitter: `delay = min(base * 2^attempt, cap) * (1 + rand(-j,j))`,
      `base=0.5`, `cap=8.0`, `j=0.25`. Reset `attempt` to 1 on a clean connect.
- [ ] On reconnect, **reuse** the already-detected firmware version, mode
      mapping, and home altitude where possible (cache per connection string) so
      re-handshake is faster and the UI doesn't flap to "undetected".
- [ ] **Acceptance:** reconnect delays grow then cap; no thundering-herd retry.
      Test `tests/test_mavlink_reconnect.py`.

### A5. Switch main telemetry streams to MAV_CMD_SET_MESSAGE_INTERVAL  `mavlink`
- [ ] On connect, after the first heartbeat, request the core streams via
      `MAV_CMD_SET_MESSAGE_INTERVAL` (the modern, per-message PX4 mechanism —
      already implemented for vibration) instead of legacy `REQUEST_DATA_STREAM`.
      Target the same rates as `_stream_rates()` but per-message-id
      (`ATTITUDE`, `GLOBAL_POSITION_INT`, `VFR_HUD`, `SYS_STATUS`, `GPS_RAW_INT`,
      `HEARTBEAT`, `HOME_POSITION`, `STATUSTEXT`).
- [ ] Keep `REQUEST_DATA_STREAM` as a **fallback** for firmware that does not ACK
      the interval command (best-effort v1.12–v1.15).
- [ ] Verify against PX4 v1.16, v1.17, v1.18 SITL.
- [ ] **Acceptance:** stream rates are honored on all three target versions;
      `ATTITUDE` holds 50 Hz on UDP, 10 Hz on serial. Test
      `tests/test_mavlink_streams.py`.

### A6. Send-lock + recv race hardening  `mavlink`
- [ ] The `_send_lock` guards sends, but `recv_match` and the stale check race:
      guard `_conn` access with a short-lived reference snapshot
      (`conn = self._conn; if conn is None: continue`) so a concurrent
      `stop()`/reconnect cannot send on a closed socket.
- [ ] Make `_gcs_hb_loop` re-check `_running` and `_conn` inside the send lock
      and on exception break the loop cleanly (currently swallows silently,
      leaving a zombie loop after disconnect).
- [ ] **Acceptance:** `stop()` during active send does not raise; no
      "send on closed socket" log spam. Covered by `tests/test_shutdown.py`.

### A7. Keep GCS heartbeat + streams alive across soft drops  `mavlink`
- [ ] During `degraded` state (A3), keep sending GCS heartbeats so the autopilot
      does not drop *us* while we wait for its heartbeat to return.
- [ ] **Acceptance:** a 6-second degraded window on UDP does not cause the
      autopilot to time out the GCS. Manual SITL check.

---

## Workstream B — Performance & Fluidity  (owner: `perf` + `backend`)

The app already has a good shared-rAF interpolation loop (`Corvus.anim`) and
latest-wins SSE buffering. The remaining bottlenecks are server-side: the store
notifies listeners and serializes a full snapshot on **every** MAVLink message
(50 Hz attitude + 10 Hz position + ...), and each SSE client re-serializes the
same snapshot. On a field laptop with a few browser tabs this is heavy.

### B1. Coalesce store notifications to a capped rate  `backend` + `perf`
- [ ] In `VehicleStateStore.update`, batch notifications: instead of notifying
      listeners inside every `update()` call, schedule a single notification on
      a short timer (e.g. a daemon thread waking every ~33 ms / 30 Hz, or a
      `threading.Condition` with a 33 ms debounce). The latest snapshot is
      published once per tick regardless of how many messages arrived.
- [ ] Keep a **fast path** for high-priority state changes (armed, mode, link
      transitions) that notifies immediately, so arming still feels instant.
- [ ] **Contract:** `add_listener`/`remove_listener` API unchanged; listeners
      still receive a full snapshot. The frontend `telemetry.js` rAF coalescing
      stays as the second tier.
- [ ] **Acceptance:** a 50 Hz attitude flood produces ≤35 SSE `state` events/s
      per client (was ~60+). `tests/test_state_store.py` extended.

### B2. Avoid full snapshot copy per notify  `perf`
- [ ] `_snapshot_locked` deep-copies the warnings list on every call. With B1's
      coalescing this drops to 30 Hz, but further optimize: keep a cached
      "last published snapshot" and only rebuild when the data version changes.
- [ ] Profile with `cProfile` during a SITL connection; target < 5% CPU on the
      backend at 60 Hz telemetry.
- [ ] **Acceptance:** perf report attached; no regression in `tests/`.

### B3. SSE serialization: reuse the sanitized JSON  `backend`
- [ ] Each `_sse_telemetry` client calls `json.dumps(_sanitize(snapshot))`
      independently. With multiple clients this duplicates work. Compute the
      serialized JSON once per coalesced tick (B1) and broadcast the same bytes.
- [ ] **Acceptance:** 3 concurrent SSE clients do not triple CPU. Test
      `tests/test_server_sse.py`.

### B4. Frontend: throttle non-critical DOM updates  `perf` + `gui`
- [ ] The topbar `updateTopBarValues` runs on every state push. After B1 it's
      ≤30 Hz, but several `querySelector` calls per tick are avoidable — cache
      element refs once (already partially done; finish it for all blocks).
- [ ] The flight-telemetry grid (`instruments.js`) re-queries
      `[data-ft="..."]` every update; cache the cell refs.
- [ ] **Acceptance:** 60 FPS sustained on the map+instruments during SITL on the
      field laptop. Lighthouse/perf trace shows no long tasks > 50 ms.

### B5. Map path buffer: cap + efficient update  `perf` + `map`
- [ ] `pathCoords` is sliced with `pathCoords.slice(-500)` on every new point —
      O(n) copy. Replace with a `deque`-style ring or use
      `pathSource.setData` with an incremental `LineString` update.
- [ ] **Acceptance:** long flights (1000+ points) do not cause per-frame jank.

---

## Workstream C — UI Design System & Reusable Components  (owner: `gui`)

**Problem:** every button is hand-coded with duplicated CSS. `.fa-btn`,
`.takeoff-confirm`, `.plan-fly`, `.plan-clear`, `.link-connect`, `.console-send`,
`.ssh-connect`, `.params-download-btn`, `.params-apply`, `.calib-btn`,
`.autotune-btn`, `.ssh-modal-btn.primary`, `.mc-btn`, `.icon-btn`, `.layer-opt`
all repeat the same pattern: `background: var(--accent); color: ...; padding;
border-radius; transition; :hover; :active; :disabled`. There is no shared base,
so each new button is recoded from scratch and drifts.

**Goal:** an Apple/Tesla-inspired component foundation — a single button base
with semantic variants driven by CSS custom properties, plus a small JS helper
to build consistent elements. The flight-action buttons (ARM/TAKEOFF/LAND/RTL)
become the flagship "Tesla-style" controls: tactile, pill-shaped, with clear
state and press feedback.

### C1. Design tokens & button base classes  `gui`
- [ ] Add a `src/css/components.css` (loaded after `main.css`) holding the
      reusable component layer; keep `main.css` for layout/panels.
- [ ] Define a `.btn` base and variants via attribute/`data-` modifiers:
  - `data-variant="primary|secondary|ghost|danger|success|nav"`
  - `data-size="sm|md|lg"`
  - `data-shape="pill|round|block"`
  - `.btn:disabled`, `.btn.is-busy`, `.btn.is-active` states.
  - All colours read from the existing semantic palette (`--accent`, `--healthy`,
    `--warning`, `--critical`, `--nav`) so status colours stay consistent.
- [ ] Apple/Tesla motion: press = `scale(0.97)` instant, hover = subtle lift +
      brightness, focus ring = `--accent-bright` outline, all
      `prefers-reduced-motion` safe. Use spring-free cubic-bezier
      `(0.2, 0.8, 0.2, 1)` consistent with the existing plugin-card motion.
- [ ] **Acceptance:** a single `.btn` class renders a correct primary, secondary,
      ghost, and danger button with only `data-variant` changing. Visual review.

### C2. Migrate existing buttons to the component system  `gui`
- [ ] Replace the duplicated button CSS (`link-connect`, `console-send`,
      `ssh-connect`, `params-download-btn`, `params-apply`, `calib-btn`,
      `autotune-btn`, `ssh-modal-btn`, `plan-fly`, `plan-clear`, `takeoff-confirm`)
      with `.btn` + `data-variant`. Keep the exact visual outcome; reduce CSS
      ~300 lines.
- [ ] Update the HTML/JS that builds these elements to emit the new classes.
- [ ] **Acceptance:** side-by-side screenshot before/after is pixel-identical;
      `tests/test_frontend_*.js` pass.

### C3. Tesla-style flight-action buttons  `gui`
- [ ] Redesign `.fa-btn` (ARM / TAKEOFF / LAND / RTL / PLAN) as the flagship
      control group — the "Tesla-like" reference the user wants:
  - Pill shape, glassy translucent material (existing backdrop-blur), larger hit
    area, icon+label stack.
  - Per-action colour identity on hover/active: ARM=healthy green, TAKEOFF=green
    lift, LAND=amber, RTL=nav blue, PLAN=accent.
  - Distinct **armed** state (filled green glow), **busy** state (pulsing ring,
    reduced-motion-safe), **disabled** state (dimmed).
  - Haptic-feel press feedback (scale + brief colour flash).
- [ ] Keep the existing `Corvus.topbar.beginCommand/succeedCommand/failCommand`
      wiring; expose a small `Corvus.ui.button(el, {onAction, busyState})` helper
      that manages disabled/busy/success classes so app.js stops hand-managing
      `btn.disabled` + class toggles (currently repeated per button in app.js).
- [ ] **Acceptance:** ARM/TAKEOFF/LAND/RTL feel responsive and premium; busy
      ring shows during the ACK wait; reduced-motion users get instant feedback.

### C4. Shared `Corvus.ui` component helpers  `gui`
- [ ] Create `src/js/ui.js` exporting:
  - `button(options)` → returns a configured `<button>` using the `.btn` system.
  - `icon(name)` / `iconBtn(name)` → consistent Lucide icon elements (currently
    reimplemented in `topbar.js`, `sidenav.js`, `map.js` separately).
  - `statusDot(level)` → the `.tb-dot`/`.link-status-dot` pattern unified.
  - `card(options)` → the surface-card pattern (`.page-card`/`.link-card`/
    `.ssh-card` share structure).
- [ ] Refactor `topbar.js`, `sidenav.js`, `map.js`, `panel.js` to use these
      helpers, killing the per-module `icon()` re-implementations.
- [ ] **Contract:** helpers are pure (return DOM elements, no global state);
      load order: `ui.js` before the modules that use it (add to `index.html`
      after `telemetry.js`).
- [ ] **Acceptance:** no module defines its own `icon()` factory anymore;
      `tests/test_frontend_*.js` pass.

### C5. Polish: materials, typography, motion consistency  `gui`
- [ ] Apply the `apple-design` skill principles across overlays (flight-actions,
      takeoff/plan panels, warnings popover, layers popover): consistent blur
      radius, border opacity, shadow depth, and entrance/exit transitions.
- [ ] Add a unified entrance animation utility (fade+rise) for panels/popovers,
      reduced-motion-safe, replacing the ad-hoc `slideDown`/`scaleIn`/`fadeIn`
      keyframes.
- [ ] Audit tabular-nums usage on all numeric readouts (topbar, instruments,
      params) for stable layout.
- [ ] **Acceptance:** visual coherence review; no layout shift during telemetry
      updates.

---

## Workstream D — Offline Map Tile Downloader  (owner: `map` + `backend`)

**Current state:** the map loads raster tiles exclusively from
`server.arcgisonline.com` (ArcGIS) over the internet. There is **no** local
cache, no MBTiles, no download UI. The README explicitly says an internet
connection is required for tiles — a hard blocker for field use.

### D1. MBTiles SQLite cache backend  `map`
- [ ] Create `corvus/tile_cache.py`: a stdlib `sqlite3`-backed MBTiles reader/
      writer conforming to the MBTiles 1.3 spec (tiles table with
      `zoom_level/tile_column/tile_row/tile_data`).
- [ ] Store under a config-driven cache dir (default
      `~/.corvus/tiles/<source>.mbtiles`), created lazily.
- [ ] API: `get_tile(source, z, x, y) -> bytes|None`, `put_tile(source, z, x, y,
      blob)`, `has_tile(...)`, `stats(source) -> {count, minzoom, maxzoom}`,
      `bounds(source) -> (w,s,e,n)`.
- [ ] **Contract:** `tile_row` is TMS (y-flipped) per MBTiles spec; convert in
      the layer. Keep the file flush+close on shutdown (lifecycle, F3).
- [ ] **Acceptance:** round-trip test `tests/test_tile_cache.py` (write/read/
      stats). No third-party dep (stdlib `sqlite3`).

### D2. Custom MapLibre tile source (offline-first)  `map`
- [ ] In `map.js`, add a tile-protocol handler that MapLibre can call:
      `corvus://{source}/{z}/{x}/{y}.png` resolved via a `transformRequest` hook
      or a custom `raster` source with a `tiles` URL the backend serves (D3).
- [ ] On tile load: if cached locally, serve from MBTiles; if online and a
      download is configured for that region/zoom, fetch + cache + serve; if
      offline and missing, serve a transparent placeholder (no broken tiles).
- [ ] Add a layer switcher entry "Offline (cached)" that uses the local source.
- [ ] **Acceptance:** after a region download (D5), the map renders fully with
      no network. Manual offline test (unplug network).

### D3. Backend tile-download + serve endpoints  `backend` + `map`
- [ ] `POST /api/tiles/download` — start a region download. Body:
      `{source, bounds:{w,s,e,n}, minzoom, maxzoom}`. Returns a job id.
      Backend fetches tiles from the upstream URL in a background thread
      (respectful: ~2 concurrent, small delay), writes to MBTiles.
- [ ] `GET /api/tiles/progress?id=...` (SSE) or `GET /api/tiles/jobs` — download
      progress `{done, total, failed, state}`.
- [ ] `POST /api/tiles/cancel?id=...` — cancel a running download.
- [ ] `GET /api/tiles/{source}/{z}/{x}/{y}.png` — serve a cached tile to the
      frontend (so the browser never talks to the internet directly). Falls
      back to upstream fetch+cache when online and the tile is absent.
- [ ] `GET /api/tiles/sources` — list configured sources + cache stats.
- [ ] **Contract:** sources map 1:1 to the frontend `TILE` keys
      (`satellite`, `streets`, `hybrid`). Bounds use WGS84 degrees.
- [ ] **Acceptance:** `tests/test_server_tiles.py` covers download start,
      progress, serve, cancel. No leaked download threads on shutdown.

### D4. Tile-downloader worker (rate-limited, resumable)  `map`
- [ ] Enumerate the tile XYZ set for `bounds` × `minzoom..maxzoom` (TMS y-flip).
      Estimate total count and warn if very large (cap e.g. 50 000 tiles/job).
- [ ] Download with `urllib.request` (stdlib), a bounded thread pool
      (`concurrent.futures.ThreadPoolExecutor(maxworkers=3)`), retry-once on
      network error, skip existing cached tiles (resumable).
- [ ] Persist a small job-state JSON so a crashed/interrupted download can resume.
- [ ] **Acceptance:** interrupting and restarting a download resumes without
      re-fetching cached tiles. `tests/test_tile_downloader.py`.

### D5. Download UI in the map layers popover / a new "Maps" panel  `gui`
- [ ] Add a "Download region" control: draw a bounding box on the map (or use
      the current view bounds), pick zoom range (min/max sliders), pick source,
      show estimated tile count + disk size, start/progress/cancel.
- [ ] Surface cache stats (cached zoom range + approximate size) and a "clear
      cache" action.
- [ ] Visualize download progress with the existing progress-bar component.
- [ ] **Acceptance:** an operator can select a region, download, then go fully
      offline and still pan/zoom the cached area.

### D6. DEM / elevation (optional, later)  `map`
- [ ] Stub a DEM tile source slot in the cache for future terrain-aware
      features (3D relief, terrain following). Out of scope for the first pass
      but the MBTiles schema should not preclude it.
- [ ] **Acceptance:** design note only.

---

## Workstream E — Extensibility & Architecture  (owner: `backend` + `gui`)

### E1. Backend endpoint registry (instead of a hardcoded if/elif chain)  `backend`
- [ ] `server.py` `_handle_api_get`/`_handle_api_post` are growing if/elif chains.
      Refactor to a route registry: `@route("/api/mavlink/arm")` decorators or a
      `{"GET": {path: handler}}` dict. Keep the same URLs (no frontend changes).
- [ ] This makes adding endpoints (D3, E3, future plugins) a one-liner and
      removes the long procedural dispatch.
- [ ] **Acceptance:** all existing `tests/test_server_*.py` pass; new endpoints
      register trivially.

### E2. Frontend module contract & loader  `gui`
- [ ] The frontend uses `<script>` tags in a fixed order in `index.html`.
      Formalize the module contract: each `Corvus.<module>` exposes `init()` and
      optional `destroy()`; document load order in a header comment.
- [ ] Add a tiny `Corvus.register(id, module)` so modules declare themselves and
      `app.init()` iterates the registry (order preserved). Keeps new pages
      (maps download, future tools) consistent.
- [ ] **Acceptance:** `app.js` no longer hand-calls every module's `init`;
      `tests/test_frontend_*.js` pass.

### E3. Plugin API expansion for the FUTURE tab  `gui` + `backend`
- [ ] The plugin `api` currently exposes telemetry subscribe/getState +
      requestJson/postAction. Add: `api.console(text, level)` (publish to the
      MAVLink console), `api.notification(level, msg)`, and a documented way to
      register a backend-backed action (so a plugin can request a new endpoint
      without forking the server).
- [ ] Keep the strict open/close single-instance + `destroy()`-once guarantee.
- [ ] **Acceptance:** the vibration plugin still works; a new "Link Stats" plugin
      (using A1's data) can be added in < 60 lines. `tests/test_frontend_plugins.js`.

### E4. Config file for connections / sources / cache  `backend`
- [ ] Introduce an optional `~/.corvus/config.json` (or `corvus.toml`) for:
      default MAVLink connection, tile sources + upstream URLs, cache dir,
      stream-rate overrides. Loaded by `serve.py`/`app.py`; CLI flags override.
- [ ] No hard requirement (field laptop can still run zero-config), but enables
      operators to pin a custom tile source or a non-default radio baud.
- [ ] **Acceptance:** config is optional; missing file = current defaults.
      `tests/test_config.py`.

---

## Workstream F — Reliability, Lifecycle & Testing  (owner: `review` + `backend`)

### F1. Audit & fix the shutdown path  `review` + `backend`
- [ ] Both `serve.py` and `app.py` end shutdown with `os._exit(0)`. Audit:
      ensure every file handle (MBTiles cache from D1, tlogs if added) is
      flushed/closed before the `os._exit`. Prefer joining threads then a single
      `sys.exit` where possible; keep `os._exit` only as a last-resort guard
      with a short timeout.
- [ ] The `_version_retry_thread` and `_hb_thread` are daemon threads not joined
      by `stop()`. Verify they observe `_running` and self-terminate; add an
      explicit join with timeout in `stop()` for the heartbeat thread.
- [ ] Ensure download worker threads (D4) are cancelled and joined on shutdown.
- [ ] **Acceptance:** `tests/test_shutdown.py` extended: after SIGTERM, no
      zombie threads/sockets/SQLite handles remain (`lsof`/`ps` check in CI).

### F2. tlog / flight-log recording (mentioned in AGENTS.md, not implemented)  `backend` + `mavlink`
- [ ] AGENTS.md states tlogs/flight logs record the GCS version in a metadata
      header — but no recording code exists. Add a lean tlog writer: append raw
      MAVLink frames + a small JSON header (`product`, `version`, `conn`,
      `started_at`) to `~/.corvus/logs/<timestamp>.tlog`. Flushed + closed on
      shutdown.
- [ ] Start/stop recording with the connection lifecycle; never block the recv
      loop (write from a bounded queue on a writer thread).
- [ ] **Acceptance:** a SITL session produces a non-empty tlog closed cleanly
      on exit. `tests/test_tlog.py`.

### F3. Review-Agent audit per change  `review`
- [ ] After each workstream merges, `review` audits: flight-command safety
      (no new armed-unsafe path), shutdown cleanliness, PX4 v1.16/17/18
      compatibility, and version single-source (no hardcoded version literal).
- [ ] **Acceptance:** review report attached to each workstream; CI green.

### F4. Regression: PX4 v1.16 / v1.17 / v1.18  `mavlink` + `review`
- [ ] Any change touching parameters, modes, or `MAV_CMD`s (A5) is run against
      SITL for all three target versions before acceptance.
- [ ] **Acceptance:** matrix test report.

### F5. Version discipline  `orchestrator`
- [ ] On release: bump `VERSION` only. `review` verifies `GET /api/version`,
      the window title, and `corvus.version` all resolve to the new value with
      no hardcoded literals anywhere.
- [ ] **Acceptance:** grep for the old version string returns only `VERSION`.

---

## Suggested execution order (phased)

1. **Phase 1 — Stable link (A1–A7):** the foundation. Without a stable link,
   every other improvement is undermined. Owner: `mavlink`+`backend`; audit `review`.
2. **Phase 2 — Smooth & fast (B1–B5):** coalesced SSE, snapshot reuse, DOM cache.
   Owner: `perf`+`backend`+`gui`.
3. **Phase 3 — Design system (C1–C5):** tokens, `.btn`, Tesla flight buttons,
   `Corvus.ui` helpers. Owner: `gui` (uses `apple-design` skill).
4. **Phase 4 — Offline maps (D1–D6):** MBTiles cache, downloader, UI. Owner:
   `map`+`backend`+`gui`.
5. **Phase 5 — Extensibility (E1–E4):** route registry, module loader, plugin
   API, config. Owner: `backend`+`gui`.
6. **Phase 6 — Reliability (F1–F5):** shutdown audit, tlog, regression, release.
   Owner: `review`+`backend`; `orchestrator` for the `VERSION` bump.

> Each phase is independently shippable. Phases 1–2 can largely proceed in
> parallel; 3 can start once 2's DOM-cache contract (B4) is agreed; 4 depends on
> nothing and can run in parallel with 3 after the route registry (E1) is
> stubbed early.

---

## Open questions (resolve before starting)

- Q1. Which upstream tile source(s) to license/bundle for offline? ArcGIS is
  currently used without attribution. Confirm acceptable source for field use
  (OSM raster, ESRI, Mapbox?) — affects D1/D4.
- Q2. Should the tlog (F2) record raw MAVLink or a parsed JSON stream? Raw is
  standard; confirm.
- Q3. Default cache size cap for MBTiles on the field laptop (disk budget)?
- Q4. Is a second MAVLink connection (e.g. a companion-computer link) in scope,
  or single-link only? Affects E1/E3.
