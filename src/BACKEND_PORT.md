# Backend port: Corvus GCS without Python

Goal: the native app runs with no Python at all. No `serve.py`, no bundled
CPython, no PyO3, no pymavlink, no web engine. The backend that `python/corvus`
is today becomes a Rust library inside the app. Decided with the user on
2026-11-06:

- **In the same process.** The backend is a library crate (`corvus_core`, in
  the root workspace) that the interface calls directly. Its threads are Rust
  threads, torn down on the normal exit path (`AGENTS.md`, lifecycle).
- **HTTP stays as an option, not as the way in.** A local HTTP/SSE server
  (`corvus_core::http`) keeps serving the same routes for MCP (`POST /mcp`),
  `--server` clients, a second station and the later Android app. The interface
  itself does not need it.
- **`python/` stays until everything is compared.** It is the reference for the
  parity tests, and it is deleted only when the user says so after comparing
  page by page and endpoint by endpoint. Until then no bundle ships it.

## Status

The open work per area, in priority order, is in `PLAN.md` section 0.

## Rules

- The same words, numbers, files and wire bytes as the Python. Where an
  endpoint answers JSON, the Rust answer is equal to the Python answer for the
  same input. A route harness (`python/tools` talking to both servers, or
  fixtures dumped from Python) holds each ported route to it.
- Every ported module keeps its tests: the pytest cases move to Rust tests with
  the same inputs and expected values (dump the values from Python into JSON
  fixtures, as `model_cases.json` does for the plugins).
- Flight-stack rules of `AGENTS.md` apply unchanged (the dialect layer
  `autopilot.py` stays the one place that knows PX4 from ArduPilot; PX4 v1.16 to
  v1.18, ArduPilot 4.3 to 4.6; never send a command or parameter the connected
  firmware does not know).
- Standard library first; a new crate is justified in its commit.
- No hardware tests: everything is simulated (`python/tools/scene.py` and
  `scene_kit` stay the simulated aircraft until a Rust one exists).
- One commit per finished module, gates green after every commit
  (`cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
  `cargo test --locked`, and the Python suite while `python/` is touched).

## Map of the work

Sizes are the Python lines today.

| Area | Python | Becomes |
| --- | --- | --- |
| Foundation | `server.py` (5.9k), `routes/*`, `state_store.py`, `config.py`, `websocket.py`, `sse_buffers.py`, `http_input.py`, `instance_lock.py`, `priority_lock.py`, `child_env.py`, `version.py`, `languages.py` | `corvus_core`: router, in-process `internal_call`, HTTP/SSE/WebSocket server, state store, settings under `~/.corvus`, shutdown path |
| MAVLink | `mavlink_*.py` (glue over `corvus_link`), `autopilot.py` | `corvus_link` loses its PyO3 layer and becomes a plain crate used by `corvus_core` |
| Vehicle | `mission.py`, `geofence.py`, `fleet.py`, `battery*.py`, `safety_config.py`, `ardupilot_*.py`, `tuning_config.py`, `rc_config.py`, `motor_config.py`, `mounting_config.py`, `param_*.py`, `first_setup.py`, `checklists.py`, `routes/{vehicle,mission,geofence,fleet,control,first_setup}.py` | `corvus_core::vehicle` |
| Map data | `tile_*.py`, `dem_*.py`, `custom_dem.py`, `buildings.py`, `crs.py`, `geocode.py`, `routes/{tiles,dem}.py` | `corvus_core::mapdata` (MBTiles through `rusqlite`, offline cache, Overpass cell) |
| Logs and firmware | `flight_review.py` (2.9k), `tlog_review.py`, `ulog.py`, `log_service.py`, `tlog.py`, `firmware_*.py`, `ardupilot_firmware.py`, `flash_service.py`, `routes/{logs,firmware}.py` | `corvus_core::logs`, `::firmware` |
| Peripherals | `rtk_service.py`, `sik_*.py`, `elrs.py`, `remote_id.py`, `video.py` (1.4k), `ssh_bridge.py`, `local_shell.py`, `serial_ports.py`, `net_probe.py`, `routes/{rtk,video,console}.py` | `corvus_core::radio`, `::video`, `::shell` |
| AI and plugins | `mcp.py`, `routes/mcp.py`, `plugin_registry.py`, `plugin_config.py`, `plugin_files.py`, `update_check.py`, `settings_bundle.py` | `corvus_core::mcp` and friends |
| Not ported | `app.py`, `native_window.py`, `desktop_icon.py`, `app_icon.py` (the Qt window) | stay in `python/` until it is deleted |

## Order

1. **Foundation** (one session, first, nothing else can start before it):
   `corvus_core` with the router, `internal_call`, HTTP/SSE/WebSocket server,
   state store, settings storage, instance lock, version, languages, the
   shutdown path; `corvus_link` without PyO3; the interface starts the core
   in its own process instead of a child; a route harness that compares Rust and
   Python answers. Python stays runnable beside it.
2. **Then in parallel, one session each** (they only touch their own module
   folder and register routes through the foundation's router): Vehicle, Map
   data, Logs and firmware, Peripherals, AI and plugins.
3. **Switch:** the three build scripts and CI drop CPython, PyInstaller and
   `pip`; the bundle smoke test checks that no Python is inside.
4. **Delete `python/`** only on the user's word.

## Open at the start

- Which HTTP crate (a small blocking server on `std::net` like the Python
  `ThreadingHTTPServer`, or `tiny_http`, or `axum` with tokio): the foundation
  session decides and says why; the answer is bounded by the shutdown rule
  and the 60 fps interface (no async runtime inside the interface thread).
- `rusqlite` (bundled SQLite) for MBTiles; `rustls` is already in
  `corvus_link`.
