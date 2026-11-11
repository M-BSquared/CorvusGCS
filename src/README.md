# The Rust program

The Corvus GCS desktop interface as a native Rust app (egui/eframe), built
from `Cargo.toml` and this `src/` folder at the repository root. It replaces
the web interface in `python/src/`, page by page, and later becomes the
Android app as well. `src/link/` is the MAVLink crate (PyO3, its own
`Cargo.toml`, built by maturin) that the Python backend imports as
`corvus_link`; see its README.

```
src/main.rs            entry point, arguments, signals
src/app/               the window: layout, rail, page switching, responsive widths
src/net/ data/         the backend child, SSE, telemetry, track (the HTTP client and units are in src/api)
src/core/              corvus_core: the backend as a library (router, internal_call, state store, settings, HTTP server)
src/api/               the crate plugins build on: ui, i18n, net::http, plugin (trait, options, shapes)
src/home/ map/ map3d/  the Home page: flight bar, HUD, top bar, the 2D and the 3D map
src/mission/ setup/    the Mission planner and the Setup pages
src/lang/              the interface language files (the translation layer is in src/api/src/i18n)
src/settings/ logs/ analysis/ notify/ panel/ term/
src/plugins/           the registry and one crate per bundled plugin (src/plugins/<id>/)
src/link/              the MAVLink crate for the Python backend
```

It is a client of the backend's HTTP/SSE API, the same one the web interface
uses, so the backend does not change for it:

```bash
cargo run --release                                   # run the backend (corvus_core) in this process and show it
cargo run --release -- --server http://10.0.0.5:8080  # a backend elsewhere
cargo run --release -- --root DIR --python PATH       # run the Python backend (serve.py) as a child instead, the reference
cargo run --release -- --page motors --scroll 560     # open on a page (setup, any tile such as battery or motors, safety:SENSOR, mission, mission:NAME, settings, logs, analysis), scrolled
# looking at it without a hand on the mouse
CORVUS_GUI_SHOT=out.png cargo run -- --page motors     # save the window's own pixels after 9 s and quit
CORVUS_GUI_SIZE=700,812 cargo run                      # open at that size, below the usual floor too
CORVUS_GUI_HOVER=600,50 cargo run                      # hold the pointer there (a hover card, a hint)
CORVUS_GUI_CLICK=560,310 cargo run -- --page motors    # press there once, at 6 s (an open menu)
CORVUS_GUI_CLICK="178,472;400,200;enter" cargo run    # several presses, half a second apart; enter is the key, a leading r is a right click ("r600,400")
CORVUS_GUI_3D=11.5,47.4,13,200,82 cargo run             # open the 3D map, here: lng,lat,zoom[,bearing[,pitch]]; 1 alone keeps the view
CORVUS_GUI_3D_PANEL=1 CORVUS_GUI_3D=1 cargo run         # with the 3D panel (buildings, elevation models) open
CORVUS_GUI_SOAK=1 cargo run --release                   # the window drives itself: pages, map, 3D, language (src/app/soak.rs)
```

Locally the backend is `corvus_core` in this process, with its HTTP server on a
free loopback port that the interface's client reads (a second Corvus on this
computer is refused unless `CORVUS_ALLOW_MULTI=1`). Every way out (window
closed, SIGINT, SIGTERM) stops the core, which joins every thread it started. A
route of an area that is not ported yet answers `501 Not available in this
build`, which a page shows as its error text. With `--root` or `--python` the
Python backend runs as a child instead: every way out sends it SIGTERM, which
runs its own teardown, and SIGKILL if it is still there after 8 s. If the window
does not close within 10 s of a signal, the child is killed and the process
exits.

## The core (`src/core`, crate `corvus_core`)

The backend of the Python program (`python/corvus`) becomes this library
(plan: `src/BACKEND_PORT.md`). The foundation is the router, the state store,
settings under `~/.corvus`, the instance lock, the version, the languages and
the optional HTTP server; every other area (vehicle, map data, logs and
firmware, peripherals, AI and plugins) is a module that registers its routes
through the contract below, in its own folder, and is held to the Python routes
by tests whose expected values are dumped from the Python
(`python/tools/dump_core_cases.py`).

| | |
| --- | --- |
| `Module` | `name`, `register(&mut Registry)`, `start(&Core)`, `stop(&Core)`. `register` only declares; threads start in `start` and are all joined in `stop`, which runs in reverse order on the way out. |
| `Registry` | `get`, `post` (a JSON object in `req.payload()`), `post_with` (another cap, or a raw body for an upload), `delete`, `get_pattern` (`/api/tiles/{source}/{z:d}/{x:d}/{y:d}.png`, `{rest*}`), `guard` (a refusal before every non-GET handler, for HTTP and in process alike), `service`. A route registered twice fails the build. |
| `Request` | `method`, `path`, `query` (`parse_qs`: `query.get("code")`), `headers`, `body`, `peer`, `origin`, `param("z")`, `payload()`. |
| `Response` | `json`, `json_status`, `error(status, text)` (`{"error": text}`), `not_ok` (`{"ok": false, "error": text}`), `bytes`, `empty`, `sse(Box<dyn SseStream>)`. |
| `Core::internal_call(method, path, payload)` | runs an endpoint in process and returns `(status, JSON)`: the same handler, validation and guards a request has. An AI tool and the interface use it; nothing reaches the link around a route. |
| `Core::service::<T>()` | a value another area published (`reg.service(Arc<T>)`). |
| `Core::wait_stopping(d)`, `is_stopping()` | what a loop sleeps on, so shutdown does not wait for it. |
| `VehicleStateStore` (`core.store()`) | the Python store's fields (the 100+ keys of `tests/data/state_defaults.json`, dumped from `state_store.py`, in that order), `update(pairs)` (unknown keys ignored), `merge_warning`, `resolve_warnings`, `clear_warnings`, `forget_vehicle`, `heartbeat`, `set_disconnected`, `is_stale`, `snapshot()` (an `Arc<Map>` cached per `version()`), `history`, `add_listener` / `remove_listener`. Listeners get the latest snapshot at most every 1/30 s, at once for the keys in `IMMEDIATE_KEYS`; they run on the writer's thread without the lock. `core.service::<StoreHub>()` is the main store, the one being flown and the `?vehicle=` resolver the fleet module sets. |
| Event streams | `GET /api/telemetry` (event `state`, latest wins, each version serialised once), `GET /api/events?topics=a,b&job=ID` (one event name per topic, `ping` every 15 s) and `Core::topic_stream` for the single-topic forms. An area registers a topic: `reg.topic(Topic::new("fleet", 1, \|core, sink, job\| Binding { unbind, initial }).shaped(f))`; `sink.put_latest`, `put_fifo` and `put_console` are the three drop policies of `sse_buffers.py`. |
| `Settings` (`core.settings()`) | the live `~/.corvus/config.json` (`config.rs`: the same files, coercion and bytes as `config.py`): `get()` is a snapshot, `update(core, \|cfg\| ...)` is one read-modify-write that saves atomically (mode 0600) and tells the listeners, `apply_partial` is `POST /api/config`. An area's block (`battery`, `remote_id`, `rtk`, `video`, `checklists`, `fleet`, `mcp`) is coerced by `reg.config_block(key, f)` and passes through as an object until then. `reg.on_config(f)` hears every change. |
| Also owned | `GET /api/version`, `GET`/`POST /api/config`, `GET`/`POST /api/welcome`, `GET /api/languages`, `GET /api/languages/strings` (compiled-in languages, `~/.corvus/languages`, plugin `lang/` folders from `reg.language_roots`), `bundle.rs` (settings export/import: sections, redact, merge, parse; the routes are the plugins area's), `lock.rs` (`core.claim_instance()`, `CORVUS_ALLOW_MULTI`, the same lock file as Python), `priority_lock.rs` (the bridge's command lock: re-entrant, aborts first). |
| HTTP (`Http` module, `http/`) | optional: `CoreBuilder::new(o).module(Http::new(HttpOptions { port: 8000, .. }))`; `core.service::<HttpInfo>()` says where it listens. Standard library only, one thread per connection, HTTP/1.0 and close like Python's server; static files from `Options::web_root`; the CORS, Host, cross-site and header rules of `server.py` (`http/security.rs`, held to values dumped from Python); `POST/GET/DELETE /mcp` gated by loopback, the switch and the bearer token (`McpEndpoint`, which the AI module registers as `McpHandle`; `TokenStore` is `~/.corvus/mcp-token`). Event streams are `Response::sse(Box<dyn SseStream>)`; a WebSocket is `Response::upgrade(req, \|ws\| ...)`, whose closure runs on the connection thread and must return once `core.is_stopping()`. No async runtime exists anywhere. |
| Tests in Python | the program is Rust; the tests that hold it to the original stay Python. `python/tools/dump_core_cases.py` dumps expected values from the Python code into `src/core/tests/data/*.json` (`--check` fails when one is stale), and `python/tests/test_core_routes.py` starts `serve.py` and the headless `corvus-core` binary (`src/core/src/bin`, `cargo build -p corvus_core --bin corvus-core`) on identical state folders and diffs their answers through `python/tools/route_harness.py` (`Pair`, `compare_get`, `compare_post`, `diff_json`). An area adds its routes to a test there and reuses the helper. |
| MAVLink | `corvus_core::link` is `corvus_link` without its Python layer (`default-features = false`, a path dependency; `src/link` stays outside the workspace with its own lock for maturin): the dialect tables, the transports and `link::plain` (message, codec, frames, signing). The Vehicle area builds its bridge on it. |
| Battery (`corvus_core::battery`) | `battery.py` in Rust: chemistry tables, settings coercion, cell count from the pack voltage, sag correction, state of charge from the cell curve, the drain rate fit behind the time to empty. Held to Python by `dump_core_cases.py battery`: about 2 500 estimates over settings, voltages and currents, plus the settings, cell, rest voltage, charge and drain grids, played by `tests/battery_cases.rs`. |
| Dialect (`corvus_core::dialect`) | `autopilot.py` in Rust: the one place that tells PX4 from ArduPilot (mode tables and decoding, takeoff order and frame, calibration, motor test, autotune, mission start, status text events, capabilities). Held to Python by `python/tools/dump_core_cases.py dialect`, a matrix of about 10 000 answers over the three stacks, 48 vehicle types and 10 firmware versions, played by `tests/dialect_cases.rs`. |
| Bridge (`corvus_core::bridge`) | One MAVLink link to one vehicle: `Bridge::start` and `stop`, the connect and reconnect cycle (exponential backoff with jitter, a superseded cycle stands down), the heartbeat wait that skips a ground station's heartbeat, the receive loop with its two tier heartbeat timeout, the GCS heartbeat, the deferred interval and version retry workers, signing from `CORVUS_MAVLINK_SIGNING_KEY_FILE`. `Shared` is the host of the receive path, status, requests, telemetry, RTCM, tlog and flight engines. Wired to the same link: parameters (with the watchdog thread), missions and the geofence, FTP, logs, shell, setup and Remote ID (on the GCS heartbeat). The vehicle list is wired: `Bridge::fleet_tracker` binds at connect and sees every frame of the flown system. The forwarder is wired through `Forwarder::for_bridge` and `attach` (the frame sink, `Bridge::inject_raw`, `Bridge::local_udp_address`). The planner (`corvus_core::mission`, `corvus_core::geofence`) answers the mission engine's callbacks. Not wired yet (a callback that does nothing): auto connect. Held by `tests/bridge_udp.rs` against a simulated vehicle over real UDP (connect, store, status text, ACK confirmed arm, tlog, a parameter download, a mission upload handshake, Remote ID on the heartbeat, stop and start again); the cycle's threads are not line for line parity tested against Python. |
| Vehicle list (`corvus_core::fleet`) | `FleetTracker` (what the one autopilot on a link last said: heartbeat, mode through the dialect, position, heading, speed, battery, STATUSTEXT at warning or worse; stale after 5 s, dropped after 600 s) and `Fleet` over `FleetBridge` and `FleetStore` traits (`Bridge` implements the first): links and their ids, `add_link` with its refusals (limit, same connection, same serial port at another baud, `validate_connection`), `remove_link`, `remove_vehicle` (an armed vehicle needs `confirm_armed`), `select`, names and colours (`set_meta`, palette handed out once with the 15 s grace, 64 remembered), notifications about the vehicles not flown, the RTCM fan out, `snapshot()` as the interface receives it, `publish` to listeners every 0.25 s, `load` and `to_config` (`coerce_config`). Python swaps proxies on `select`; here `Fleet::active_link` says which link is flown and `set_switch_hook` is called once. `serial_ports::same_port` is the only part of `serial_ports.py` ported so far. Held to `tests/reference/fleet_python.py` by 24 scenarios of `python/tools/dump_protocol_cases.py` (`tests/fleet_scenarios.rs`: results, store and bridge calls, persisted config, listener payloads, the flown link and the fake clock all equal). Differences from Python: a `links` entry that is not a list is treated as empty (Python raises), and a bridge whose `start` fails stays listed in both. |
| Second station forwarder (`corvus_core::forwarder`) | `Forwarder` mirrors every raw frame the vehicle link receives to UDP endpoints (QGroundControl on 127.0.0.1:14550 by default) through a bounded drop oldest queue and a sender thread, learns a station only on a whole frame, lets only the configured endpoints put frames on the uplink (and only when `allow_commands` is on), never injects a frame under Corvus' own system id, never mirrors into the vehicle link's own socket, falls back to an ephemeral listen port when the chosen one is busy, retries names that did not resolve every 30 s, and reports all of it in `status()`. `parse_endpoint`, `split_frames`, `frame_source_system` are the plain functions. One UDP socket (SO_REUSEADDR, as Python), two threads, all released by `stop`. Held to `tests/reference/mavlink_forwarder_python.py` by 20 scenarios of `python/tools/dump_protocol_cases.py` played over real loopback sockets (`tests/forwarder_scenarios.rs`; ports are named A, B, V and L in the fixtures, time is a clock with an offset) and by `tests/bridge_udp.rs` against a simulated vehicle (mirrored out, a configured station's command reaches the vehicle, a frame under Corvus' id does not). Differences from Python: a port written with non ASCII digits is not accepted by `parse_endpoint`. |
| Mission planner and fence (`corvus_core::mission`, `corvus_core::geofence`) | `validate_plan` (every value treated as hostile, the same words for every refusal), `plan_to_items` (a speed is a command of its own, a landing's approach is a waypoint on a multicopter's plan), `lower_circles` (a Circle as waypoints around its rim for PX4), `items_to_plan` (a downloaded mission read back, with `plan_index`, `skipped` and `adjusted`), distances, `safe_plan_name`, `clean_point_name`, the plan files under `~/.corvus/missions` (`list_plans`, `read_plan`, `write_plan` atomic with Python's `json.dump(indent=2)` text, `remove_plan`) and the fence area (`validate_polygon`, `self_intersects`, `load`, `save`). The bridge's mission engine calls `lower_circles` and `items_to_plan` through its host. Held to `corvus/mission.py` and `corvus/geofence.py` by `planner_cases.json` (`python/tools/dump_core_cases.py planner`: 111 plans, 45 lowerings, 21 circles, 19 mission read backs, the file store and the fence files; numbers computed with trigonometry are compared to 1e-9). Differences from Python: `list_plans` and `read_plan` do not log what they skip; a float written to a file may use Rust's exponent spelling (`1e-5`, Python `1e-05`); a request body with a bare `NaN` is refused when parsed rather than by `validate_plan`. |
| Vehicle area (`corvus_core::vehicle`, `VehicleModule`) | Makes the main `Bridge` on the core's own state store and the `Fleet` around it (extra links from the `fleet` config block, saved back through `Settings::update_quiet`), publishes both as `VehicleService` (`fleet`, `handles(link)`, `active()`), moves `StoreHub::current` when another vehicle is selected and resolves `?vehicle=` for the hub. It does not open the main link: connect and auto connect come with the rest of `routes/vehicle.py`. Routes: `GET /api/fleet`, `POST /api/fleet/select`, `/vehicle`, `/links/add`, `/links/remove`, `/vehicle/remove`; `GET /api/geofence`, `POST /api/geofence/save`, `/upload`, `/clear`; `POST /api/mission/upload`, `/start`, `/clear`, `/download`, `GET /api/mission/plans`, `POST /api/mission/plans/save`, `/load`, `/remove`; and the `fleet` event topic. A request naming `vehicle` acts on that vehicle's bridge and store, or is refused 400, 409 or 503 as `targets_vehicle` refuses. Held to the Python server by `python/tests/test_core_routes.py` (`corvus-core` and `serve.py` on identical state folders: 57 route cases plus the config, fence and plan files byte for byte) and to a simulated vehicle by `tests/vehicle_routes.rs`. Not here yet: `GET /api/mission/return` (needs the safety schema), `_note_manual_link` after removing the main vehicle (auto connect), and every other route of `routes/vehicle.py` and `routes/control.py`. Differences from Python: an OS error in a `could not save` message reads Rust's text. |
| Peripherals (`corvus_core::peripherals`, `PeripheralsModule`) | Radios, shell, SSH and video, in `src/core/src/peripherals/`, published as `Peripherals` (`core.service::<Peripherals>()`); every child process, thread and pty is ended in the module's `stop`. Done: `ports` (`serial_ports.py`: phantom, bootloader, RTK and USB classification, `serial:` strings, USB identity, `list_serial_ports` from sysfs on Linux and from `serialport` on macOS and Windows), `net_probe` (`net_probe.py`: `Pinger`, one ping child per probe, four at a time, killed at its deadline and on shutdown), `child_env` (`child_env.py`), `rtk` (`rtk.py`, `rtk_service.py`, `routes/rtk.py` over the RTK engine of `src/link`, rewritten on plain types: `rtk::wire` (RTCM 3 framer, UBX parser and frames for F9 and M8P receivers, MON-VER, NAV-SVIN), `rtk::config` (settings bounds, redacted copy, checks, survey progress), `rtk::io` (serial port, TCP socket, NTRIP request and header), `rtk::service::RtkService` (one thread `rtk-base`, the search, identify, survey and forward loop, base mode taken back on exit; `RtkHost` is what it needs from the vehicle side, `VehicleRtkHost` the real one over the vehicle list's RTCM fan out), `GET /api/rtk/status`, `POST /api/rtk/settings` and `/restart`, the `rtk` config block, started from the config at module start) and the routes `POST /api/local/ping`. Held to Python by `tests/peripherals_rtk.rs` (every frame byte for byte, about 40 settings blocks, framer and parser over chunkings; a simulated u-blox receiver, a local NTRIP caster and a pty serial port for the session) and by `tests/peripherals_ports.rs` (about 800 ports, 14 connection strings, 13 descriptors, 3 platforms of ping commands and replies, from `python/tools/dump_peripherals_cases.py`) and `python/tests/test_core_routes_peripherals.py`. Differences from Python: the RTK session reports every failure as its own words (Python also wraps an unexpected exception as `the correction session failed: ...`), the OS error text of an NTRIP connect is `[Errno N] text` as Python's, a baud that does not fit 32 bits is not a baud, a Linux port has no `LOCATION` when sysfs does not say, a macOS or Windows port carries no `LOCATION`. |
| Protocols (`corvus_core::protocol`) | the MAVLink protocols in plain Rust on `corvus_link::plain`, each the twin of an engine in `src/link/protocol` (kept in Python until `python/` goes). An engine owns its state and decisions and asks a host trait for what belongs to the bridge (link send, ready, armed, clock, listeners, error text); it never holds its lock across a host call. `protocol::params::ParamEngine` (download with the lossy link watchdog, named reads, confirmed writes, verify, batch upload) with `ParamHost`; `protocol::link_host::LinkHost` is the host over a `plain::connection::Connection`. Also done: `protocol::logs` (13 scenarios), `protocol::shell` (19 scenarios) `protocol::ftp` (19 scenarios) and `protocol::flight` (mode, arm, takeoff in both stacks' orders, land, RTL, home, plugin commands, velocity, fly to points, mission start; 105 scenarios, held to `tests/reference/mavlink_flight_python.py` on a scripted vehicle and a fake clock), `protocol::setup` (calibration, motor test, autotune in both stacks' styles, reboot, bootloader; 56 scenarios on `tests/reference/mavlink_setup_python.py`), `protocol::telemetry` (battery detail and estimate, GNSS jamming and spoofing, controller setpoints, transmitter channels, link quality and jitter, stream rates; 33 scenarios on `tests/reference/mavlink_telemetry_python.py`), `protocol::missions` (upload with MISSION_COUNT retries and the vehicle's verdict, download with restarts, the receive side, which item the vehicle flies, fence upload; 92 scenarios on `tests/reference/mavlink_missions_python.py` against a scripted vehicle on a fake clock), each with its host trait (`RequestHost`, `ShellHost`, `FtpHost`, `FlightHost`, `SetupHost`, `TelemetryHost`, `MissionHost`) and `protocol::remote_id` (the identity coerced like Python's `int()`, `float()` and `str()`, the four OPEN_DRONE_ID messages, the arm status, the page's broadcast status; 20 scenarios on `tests/reference/mavlink_remote_id_python.py`, about 60 identities through `settings`), with `RemoteIdHost`, and `protocol::receive` (the bridge core's receive path: the dispatcher that turns every frame into store updates and hand overs, the duplicate window, who a frame may come from, the fleet and ignored systems, a reboot under a live link, and the COMMAND_LONG and COMMAND_INT transaction with its ACK registry, retries, IN_PROGRESS and interim results; 66 scenarios on `tests/reference/mavlink_bridge_receive_python.py`), with `ReceiveHost`, and `protocol::status` (STATUSTEXT chunk reassembly, the vehicle events a line reports, the kill switch from its line, a failing check or a pass, and the preflight report requests; 42 scenarios on `tests/reference/mavlink_bridge_status_python.py`), with `StatusHost`, and `protocol::requests` (the stream and interval tables for a cable and for a radio, the fallback decided from the ACKs, the home and version requests, the GCS authority check, applying a signing key; 37 scenarios on `tests/reference/mavlink_bridge_requests_python.py`), with `RequestsHost`, `protocol::rtcm` (GPS_RTCM_DATA fragmenting and the injection counters, `RtcmHost`; 10 scenarios) and `corvus_core::tlog` (the tlog writer on real files and the session: file names that never collide, the metadata line, a new file per flight, the forwarder's sink; 12 scenarios). The planner (`mission.py`: plan to items, circles as waypoints, items back to a plan) is the host's to answer and is not part of these engines. Held to the Python engines by the other 29 scenarios dumped by `python/tools/dump_protocol_cases.py` (the sent messages, listener statuses, error texts, results, fake clock time and cache all equal), and run over real UDP against a simulated vehicle. The next protocol copies that pattern: a Python scenario dump, a fake host, the engine. |
| 501 | every route the Python server answers that no module registered answers `501 {"error": "Not available in this build"}` (`unported.rs`, from `tests/data/python_routes.json`). Registering the route takes it over, with nothing else to edit. The interface shows the 501 as an offline notice. |

## What is here

| | |
| --- | --- |
| `src/net/backend.rs` | starts the core in this process (or `serve.py` as a child with `--root`/`--python`) and stops it |
| `src/api/src/net/http.rs` | a small HTTP/1.1 client, with a read timeout per read so a stream reader can stop |
| `src/net/sse.rs` | server-sent events |
| `src/data/telemetry.rs` | the `/api/telemetry` reader thread and the latest state |
| `src/data/units.rs` | display units from the config's `ui.units`, rounded the way the web interface rounds |
| `src/home/company.rs` | the company logo at the far right of the top bar: fetched from `/api/branding/logo`, sized as `.tb-company-logo`, a PNG header read for its shape |
| `src/home/topbar.rs` | the status bar, word for word the web interface's (`python/src/js/topbar.js`) |
| `src/map/mod.rs` | the map (walkers) on the backend's tile proxy, vehicle and home markers |
| `src/map/rail.rs` | the rail and the two buttons beside it: zoom, centre, layers, downloaded areas, measure, the clear track disc |
| `src/map/regions.rs` | the downloaded areas of `/api/tiles/regions`, merged as the web map merges them (`map.js`) |
| `src/map/credit.rs` | the tile credit as MapLibre's compact attribution draws it: a 25 px glass disc with the info icon, opened by a click into a pill wrapped at `min(46ch, 40vw)`, folded by a click on the icon or a drag of the map; used by the Home map (2D and 3D), the Analysis track map and the geofence card map |
| `src/crash.rs` | the panic hook, the crash log and the frame guard (see Reliability) |
| `src/data/intern.rs` | a `'static` text for an API that wants one, kept once and capped |
| `src/api/src/ui/tip.rs` | hover texts through the translation layer: use `on_hover_tr`, never `on_hover_text` |
| `src/app/soak.rs`, `python/tools/gui_soak.py` | the soak driver and the script that samples memory, threads and descriptors |
| `src/map/measure.rs` | the distance tool: great circle distance, bearing and the two click rule (`map-measure.js`) |
| `src/map/search.rs`, `src/map/search_card.rs` | the place search: coordinate notations read offline, area names, `/api/geocode`, the box and its list (`map-search.js`) |
| `src/map/tiles_model.rs`, `src/map/tiles_card.rs` | the offline map dialog: tile counts and sizes, the download and its jobs, the areas on this laptop (`tiles.js`) |
| `src/map3d/camera.rs`, `src/map3d/mod.rs`, `src/map3d/terrain.rs`, `src/map3d/buildings.rs`, `src/map3d/tile_cache.rs`, `src/map3d/panel.rs` | the 3D map: camera maths, the tilted ground of imagery and elevation tiles with hillshade and skirts, the sky and horizon haze, OSM buildings extruded from the backend's cells, the elevation model choice, the tile cache behind them, and the 3D panel (`python/src/js/map.js` 3D mode) |
| `src/data/track.rs` | the flown track, recorded and smoothed as `python/src/js/map.js` does |
| `src/home/hud.rs` | compass, attitude indicator and readouts (`python/src/js/instruments.js`) |
| `src/home/flight.rs` | the flight bar, takeoff card and mode picker (`python/src/js/app.js`) |
| `src/api/src/ui/style.rs` | the design tokens of the six themes (light, light-orange, green, blue, pink, orange) and the type |
| `src/setup/mod.rs` | the Setup page: head, vehicle card, the tile grid and the sub-pages (`python/src/js/setup.js`) |
| `src/setup/params.rs` | the Parameters page: header actions, download, search, filter, defaults, edit and apply, rows that stack under 721 px (`python/src/js/setup-parameters.js`) |
| `src/setup/params_dialogs.rs`, `src/setup/params_files.rs` | Export, Import and the reboot confirmation, and the three file formats with the import preview; Import asks for a file path because a native window has no picker without a dependency |
| `src/api/src/ui/modal.rs` | `.modal`: the one dialog (scrim, title, close, body, `actions`); every dialog of every page, the checklist's included, goes through `modal::show` |
| `src/term/emu.rs`, `src/term/view.rs` | the terminal: a VT parser (the `vte` crate) feeding a cell grid with colours, scroll regions, the alternate screen and 5000 lines of scrollback, drawn with keyboard, wheel, selection and paste (`python/src/js/ssh-term.js`) |
| `src/term/session.rs`, `src/term/window.rs` | one SSH session over `/api/ssh/stream` with ordered keystrokes, and the floating terminal window (`python/src/js/term-window.js`); the SSH tab holds the same terminal |
| `src/setup/motors.rs` | the Motors page: the airframe drawing, the motor table with output assignment, the motor test, frame size, protocol forms (`python/src/js/setup-motors.js`) |
| `src/setup/safety.rs` | the Safety & Sensors page: the backend's description in two stacks, the landing preset, the sensor tiles, Check values and the checklist card (`python/src/js/setup-safety.js`) |
| `src/setup/battery.rs` | the Battery & Power page: the pack drawn with its cells, fill and failsafe levels, the live readout, how Corvus reads the pack (its own settings, saved to the config), the vehicle's parameters (`python/src/js/setup-battery.js`) |
| `src/setup/battery_model.rs` | its rules: where a level sits on the pack, the flag stacking, the readout lines, the values copied from the vehicle |
| `src/setup/calibration.rs` | the Calibration page: readiness chips, the flight controller rotation, the seven calibration cards, and the wizard (stage, phase, cue, progress, position strip, the autopilot's messages, the motor warning) (`python/src/js/setup-calibration.js`) |
| `src/setup/calib_protocol.rs` | the calibration protocol as a state machine: STATUSTEXT lines to events, events to what the wizard shows; held to `calib_protocol_cases.json`, what `calib-protocol.js` answered for 182 lines and 7 transcripts |
| `src/setup/calib_figure.rs` | the raven in the attitude asked for: pose matrices, slerp between poses, projection, painter sort and shading, the ground, the compass ring (`python/src/js/calib-figures.js`); the mesh is `calib_raven.json`, the web figure's own, and `calib_figure_cases.json` is what the web figure drew |
| `src/api/src/ui/chip.rs` | `Chip`: a pill with an icon or status dot (`.calib-ready-chip`, `.calib-phase`), and `Chip::tag`, the small outlined label (`IN FOLDER`, a mode name, link quality) |
| `src/setup/radio.rs` | the Radio Control page: the transmitter and the live channel bars, the two tabs of configuration (Detect beside every channel picker, the flight mode strip, the per channel calibration table), and the calibration wizard in five steps (`python/src/js/setup-control.js`) |
| `src/setup/radio_model.rs` | its rules: the tab of a section, PX4's and ArduPilot's mode slot arithmetic, double booked channels, and the wizard's measurement (sweep, stick detection, the rows written and the request) |
| `src/setup/rc_transmitter.rs` | the drawn transmitter's model: the layout (`rc_transmitter.json`, the web `LAYOUT` dumped by running it), the four stick modes, pulse to position, the eased reading between samples, and the controls this station learned, kept in `~/.corvus/gui-rc-transmitter.json` |
| `src/setup/rc_drawing.rs` | the handset drawn and moved by RC_CHANNELS, its callouts, Learn, and the inspector that assigns a channel a function (`python/src/js/rc-transmitter.js`) |
| `src/setup/tuning.rs` | the PID Tuning page: the readiness chips, the run strip, the autotune card, one block per loop (live graphs, gains, its tune button) and the autotune settings (`python/src/js/setup-tuning.js`) |
| `src/setup/tuning_model.rs` | its rules: when an autotune can start and what stands in its way, the reboot the module waits for, the bar, the graph buffers of 600 samples |
| `src/api/src/ui/chart.rs` | a line chart in the look of the web's Plotly graphs: auto ranged axes, a dotted setpoint beside the response; its `tick_step`, `ticks` and `tick_label` are the one nice tick rule, the Analysis plots use them too |
| `src/setup/rtk.rs` | the RTK GPS page: the route the corrections take (base, this station, the aircraft's receiver) polled once a second, the survey bar and its sentence, and the settings (survey or known mark, NTRIP caster) (`python/src/js/setup-rtk.js`) |
| `src/setup/rtk_model.rs` | its words and rules: states, the survey sentence, the rows, the request body; checked against `rtk_cases.json`, what the web helpers answered |
| `src/setup/firmware.rs` | the Firmware page: the link and gate cards, the picker (flight stack, release, manufacturer, board, developer builds, what the board was detected as), a local file by path, Flash and Cancel, the bar and the flash log fed by the `firmware` event topic (`python/src/js/setup-firmware.js`) |
| `src/setup/firmware_model.rs` | its rules: the catalogue and the picker that lands only on a board somebody chose, the flash gate, the words for the transport and the progress, the request bodies |
| `src/setup/firmware_io.rs` | the catalogue read with a minute to answer, and the image upload as a raw body that stops when the page closes |
| `src/setup/firmware_flow.rs` | tests: the page against a stand-in backend that answers the status and catalogue, accepts a flash and streams progress (no board, no bootloader) |
| `src/setup/firstsetup.rs` | the First Setup page: the eight numbered steps, the bar with Previous, Skip and Next, the hosted pages (Firmware, Motors, Calibration, Radio Control, Safety, Battery), the aircraft type step on `POST /api/first-setup/airframe` with its reboot, and the finish summary (`python/src/js/setup-firstsetup.js`) |
| `src/setup/firstsetup_model.rs` | its rules: which step shows as done, where the wizard opens, the aircraft and finish wording, the session's memory, the reboot follower, and the detector's timers and the list of controllers not to ask about again (`python/src/js/first-setup.js`); held to `firstsetup_cases.json`, what the web helpers answered |
| `src/setup/firstsetup_watch.rs` | the detector's glue: reads `/api/first-setup` and `/api/firmware/status` when a link or a bootloader port appears and draws the "New controller detected" dialog |
| `src/setup/firstsetup_flow.rs` | tests: the page against a stand-in backend (the step it opens on, the preset it writes, a refusal, Finish, the reboot) |
| `src/api/src/ui/optioncards.rs` | `Corvus.ui.optionCards`: the one grid of selectable cards (`Card`: label, quiet description, optional icon or three colour swatch) |
| `src/setup/remoteid.rs` | the Remote ID page: the broadcast switch (off by default), what is going out right now, the advisory list of what the rules still ask for, the identity cards (aircraft, operator, flight, position, EU class) saved to the config as they are left, and the vehicle's own parameters through the shared schema form (`python/src/js/setup-remoteid.js`) |
| `src/setup/remoteid_model.rs` | its rules: the nested patch a row saves, the serial number check as the operator types, the "not known" altitude, the suggested aircraft type, the three status rows |
| `src/setup/sik.rs` | the Telemetry Radio page: the port card, then after Load the link report, the settings the two radios disagree on, both radios side by side with their staged edits, and Save, Reset and the advanced toggle; every session is a long POST (`python/src/js/setup-sik.js`) |
| `src/setup/sik_model.rs` | its rules: the port and baud pickers, the gate, the edits staged per side (an edit typed back to the original is dropped), copying to the remote radio, the request bodies |
| `src/setup/video.rs` | the Video page: the camera list with each camera's state, the editor of one camera (draft, Save, Delete with a confirmation) and the decoder card with the ffmpeg path (`python/src/js/setup-video.js`) |
| `src/setup/video_model.rs` | its words and rules: state text, the detail line, the draft and what blocks a save, the request bodies |
| `src/setup/video_window.rs` | the floating camera windows (`python/src/js/video-window.js`): a frame loop on `GET /api/video/frame`, a window with the terminal windows' frame; a WebRTC camera says that the native interface cannot play it |
| `src/api/src/ui/message.rs` | `Tone` (`Plain`, `Ok`, `Err`, `Warn`) and its two painters: `show`, the tinted line of feedback (`Corvus.ui.message`), and `boxed`, the notice box (`.params-desc`, `.params-banner`) |
| `src/api/src/ui/formfield.rs` | `Corvus.ui.field`: the one painter of a caption (with its optional info icon), a 32.5 px box or select, and the hint under it; `Form` and the Setup form pages both draw through it |
| `src/setup/sensors.rs` | the sensor model and its rules: the write chain of a switch, the presets, the extra parameters kept in `~/.corvus/safety-extra.json` |
| `src/setup/sensor_view.rs` | a sensor page drawn: Status card, Hardware card with the write table, the parameters you add by name |
| `src/setup/check.rs` | Check values: read back, write again what did not stick, the result card (`python/src/js/setup-shared.js`) |
| `src/setup/geofence.rs` | the geofence area the station keeps, its geometry (corners, crossing, area, framing) and the one shared copy (`python/src/js/geofence.js`) |
| `src/setup/geofence_card.rs` | the Geofence card: the small map, drawing, dragging and adding corners, upload and removal, the large editor (`python/src/js/setup-geofence.js`) |
| `src/setup/landing.rs` | the Landing card: the descent as one bar, the fields beside each stretch, the brackets with their lengths (`landingCard` of `python/src/js/setup-safety.js`) |
| `src/mission/mod.rs` | the Mission page, the planner (stacked into one scrolling column under 960 px; the last plan reopens quietly): the plan on the Home map as an overlay, the tool bar, the sidebar and the profile, with the map gestures applied to the plan (`python/src/js/mission.js`) |
| `src/mission/plan.rs` | the item table and the bounds the backend validates against, the plan's JSON both ways, and the editing rules: start and takeoff, one ending, reorder slots, altitude, the aircraft kind, a start that follows the aircraft |
| `src/mission/geometry.rs` | distances, stations, the altitude profile per aircraft, climb and glide, route length and time, circle rings, area and corridor sweeps, clearances (`python/src/js/mission-geometry.js`) |
| `src/mission/issues.rs` | the warnings under the summary, a row's one line, the summary cells and the display units |
| `src/mission/backend.rs`, `src/mission/progress.rs` | the `/api/mission` calls on background threads, and which revision and leg the vehicle is flying |
| `src/mission/mapview.rs` | the route, rings, points, the start, the radius and pattern grips, and the pointer on the map |
| `src/mission/toolbar.rs` | the tool bar (`ui::fabar`, fitted like the flight bar), its hint line and the pattern card |
| `src/mission/side.rs` | the sidebar (beside the map, or stacked under it below 960 px at its natural height): name, summary, vehicle line, warnings, the item list with row dragging, the editor, the status line and the upload, save, open, read and new buttons |
| `src/mission/profile.rs`, `src/mission/terrain.rs` | the altitude profile with draggable heights, and the elevation model sampled along the route |
| `src/mission/dialogs.rs` | the upload, read and clear confirmations and the list of saved plans |
| `src/api/src/ui/fabar.rs` | `.fa-btn`: an icon over a caption on a glass bar, with the active, armed, busy, blocked, disabled and next looks; the flight bar and the planner's tool bar |
| `src/app/responsive.rs` | the width classes of the web (`data-vw`): what folds, shrinks or stacks at 1279, 959, 900, 720 and 600 px (`python/src/css/responsive.css`) |
| `src/api/src/ui/popover.rs` | the hint icon and the sheet it opens, the cards of the top bar: hover intent, pinning, one at a time, placement (`Corvus.ui.popover`, `infoHint`) |
| `src/home/topbar_cards.rs` | the cards behind GPS, Battery, Altitude, Groundspeed and Vertical speed (`python/src/js/topbar.js`) |
| `src/home/checklist_window.rs` | the checklist's window on the Home map: ticks kept by item text, drag and fold, the warning on arming with items open (`python/src/js/checklist.js`) |
| `src/setup/checklist.rs` | the preflight checklists: config block, Setup card and the dialog the lists are written in (`python/src/js/checklist-editor.js`) |
| `src/setup/schema_form.rs` | the schema-driven form the backend describes (enum, number, sign, bitmask), shared by the Safety and Motors pages (`python/src/js/setup-shared.js`) |
| `src/api/src/ui/pagekit.rs` | page furniture: header, header with its Reload and Check buttons and status, section title, tabs, tile, back button, the armed notice |
| `src/api/src/ui/kit.rs` | the facade of the controls, so `kit::Button` and the rest stay valid; each lives in its own file below |
| `src/api/src/ui/button.rs`, `src/api/src/ui/select.rs`, `src/api/src/ui/toggle.rs`, `src/api/src/ui/text.rs` | `Button`, `Variant`, `icon_btn` (`.btn`, `.icon-btn`); `Choice` and `select` (`.field-select`); `checkbox`, `check_box`, `switch`; `wrapped`, `wrapped_weight`, `dot` |
| `src/api/src/ui/field.rs` | `TextField`: the one text box, on a well, glass or floating surface, with an optional icon; `committed`, a well that keeps what is typed until Enter or blur; `outline`, the red or changed outline over a field |
| `src/api/src/ui/filepicker.rs` | the system file dialog (`rfd`): pick a file (with an extension filter), pick a folder, save a file; the start folder is taken from the value held |
| `src/api/src/ui/pathfield.rs` | the path field (`form::path`): read only box in mono with Choose, Choose file, Choose folder and Clear buttons; modes file, folder, either and save; a backend field turns into a text box for a remote `--server` |
| `src/api/src/ui/progress.rs` | `Bar`: the track and fill of every progress bar, determinate or sweeping, with the optional border |
| `src/api/src/ui/listrow.rs` | `Mark` and `paint`: the hover and selected background of a list row |
| `src/api/src/ui/slider.rs`, `src/api/src/ui/pills.rs` | the stepped slider (`.ui-slider`) and the icon pills of the Settings page (`.settings-tab`) |
| `src/api/src/ui/menu.rs` | `Menu`: every list and popup menu (select choices, flight modes, the layers and camera lists, the context menu, the power menu, the search answers, elevation models), after `.ui-menu`, `.option-item` and `Corvus.ui.menu`; `Look` says which kind, `Anchor` where it opens, `Item` what is in it |
| `src/api/src/ui/card.rs` | every bordered box: `card::paint` / `reserve` / `fill` with a `Look` (Panel, Page, Inset, Note, Raised, Quiet, Well, Console, Chosen, Tone, Custom), and `card::window`, the surface of a floating window |
| `src/api/src/ui/segmented.rs` | the segmented switch (`.params-filter`, `.link-kinds`) |
| `src/settings/mod.rs`, `src/settings/model.rs` | the Settings page: the pills, writes through `POST /api/config` that are in force at once and taken back when the backend refuses (`strict`), the `Effects` the window reads back (theme, size, units, Mission entry, distance tool, side panel tabs, base layer), and the rules of every picker with the web's expected text in tests (`renderSettingsPage` of `python/src/js/sidenav.js`) |
| `src/settings/tabs.rs` | the Interface, Layout, Flight view and Controls pills: language, theme, size, units and the switch cards, with the web's hint texts |
| `src/settings/map_cards.rs` | the Map pill: map service and layer, the API keys dialog, the operator's elevation models (import by file or folder path, rename, remove), track, tools and animation |
| `src/settings/conn_cards.rs` | the Connections pill: links, saved SSH connections (add, edit, remove, connect in the side panel), the connection facts, the MCP server with its permissions and connect dialogs (`python/src/js/settings-mcp.js`) |
| `src/settings/data_cards.rs` | the Data pill: parameter folder and defaults copy, flight review sensitivity, export, import and reset of the settings file (`python/src/js/settings-transfer.js`) |
| `src/settings/about.rs`, `src/settings/credits.rs` | the Plugins and About pills, the update check, and the credits dialog with the native interface's own crate list (`python/src/js/credits.js`) |
| `src/settings/dialogs.rs` | the dialogs open over the page and the company logo card |
| `src/logs/mod.rs` | the Logs page: the last 30 console lines, Export log through `POST /api/console/save` and Copy (`renderLogsPage` of `python/src/js/sidenav.js`) |
| `src/analysis/mod.rs`, `src/analysis/model.rs` | the Analysis page: live telemetry tiles, the download folder, the four tiles, the status poll and its jobs, and the rules (sizes, local times, rates, sorting, findings, summary facts) with the web's expected text in tests (`python/src/js/analysis.js`) |
| `src/analysis/lists.rs`, `src/analysis/scope.rs`, `src/analysis/review.rs` | the vehicle log list with sequential download and erase, the recorded tlogs, Live Messages, and Flight Review and Telemetry Review: summary, mode strip, findings, plots by group, the aircraft's messages |
| `src/analysis/trackmap.rs` | the ground track over imagery: the inverse of the backend's projection, which layer opens, the fit, the map on the backend's tile proxy with the track as casing and core, the key, the zoom buttons and the ctrl + scroll hint (`trackMapView` of `python/src/js/analysis.js`); held to values the web code answered |
| `src/analysis/plot.rs` | the plotting module that stands in for Plotly: linear axes with nice ticks, lines, markers and steps, flight mode bands, threshold line, legend, hover read out, drag to zoom and double click to reset, long traces reduced per pixel column |
| `src/data/clock.rs` | calendar dates from Unix times, UTC and local |
| `src/api/src/ui/form.rs` | the form vocabulary of a page that is a column of cards: the `Form` cursor over `formfield`, `message`, `optioncards`, `slider` and `pills`, with section, card, switch field, rows, actions |
| `src/api/src/plugin/mod.rs` | the `Plugin` trait, the `Host` a plugin talks through, `Info`, `MapEvent`, `McpTool` and the `Outbox`: the stable surface a plugin crate may use (`python/src/js/plugins.js`) |
| `src/plugins/mod.rs`, `src/plugins/ssh_setup.rs` | the `Registry` (starts the plugins once `GET /api/plugins` has answered, ticks them every frame, keeps their options and what they ask of the window; `python/corvus/plugin_registry.py`) and `sshSetup`, the login dialog for a saved SSH connection this computer lacks (`askSSHConnection` of `python/src/js/panel.js`) |
| `build.rs` | makes `plugin_list.rs`, the list the registry is built from, out of the folders of `src/plugins`; a folder without its dependency line in `Cargo.toml` fails the build |
| `src/api/src/plugin/options.rs` | the options a plugin declares (toggle, select, text, number), cleaned, coerced and saved under `_options` as the web does |
| `src/api/src/plugin/shapes.rs`, `src/plugins/layer.rs`, `src/map3d/overlays.rs` | `api.map`: lines, areas, circles, text, buttons and icon markers in longitude and latitude, painted on the flat map (with the pointer on markers and buttons, and `pick_point`); on the 3D map lines, outlines and areas are meshes draped on the terrain and depth tested, text, markers and buttons screen elements hidden by a terrain ray (`map-overlays.js`, `map-icons.js`, `.map-overlay-*` of `map.css`) |
| `src/api/src/plugin/ssh.rs` | what both SSH plugins share: `Mode`, the new connection form, `ssh_needs`, `reply_of` |
| `src/plugins/view.rs` | the cards under PLUGINS, the opened plugin, a plugin's own tab and the options dialog of its gear in Settings (`.plugin-*` of `panel.css`) |
| `src/plugins/vibration/` (crate `corvus_plugin_vibration`) | Vibration Monitor: PX4's vibration metrics on a live chart, the 10 Hz stream while it is open, the one-shot warning (`python/plugins/vibration`) |
| `src/plugins/ssh-launcher/` | SSH Launcher: the shelf, the editor, terminal and background runs, restart, stop, the live dot (`python/plugins/ssh-launcher`); `model.rs` holds its rules, held to `model_cases.json` |
| `src/plugins/schwalby/` | Schwalby: the shelf of one-press buttons that run here (`/api/local/*`) or over SSH, with edit mode and drag reordering, colours, the companion computer's ping strip with its popup and power menu, and the AI client's half over MCP (`python/plugins/schwalby`); `model.rs` holds the rules, held to `model_cases.json`, what the web functions answered; `companion.rs` the ping loop, the popup's state and the power calls; `mcp.rs` the tools; `view.rs` everything drawn; its language files are in `lang/` |
| `src/plugins/map-entities/` | Map Entities: gates, trees and symbols with an area on the Home map, and the watch over the aircraft's track (`python/plugins/map-entities`); `model.rs` holds the geometry and the rules, held to `model_cases.json`, what the web functions answered (`src/plugins/dump_cases.js` writes it for this, SSH Launcher and Vibration Monitor; `--check` fails when it is stale) |
| `src/notify/mod.rs` | the notification board, its read and dismiss rules, the command dedupe and the toasts (`python/src/js/topbar.js`) |
| `src/notify/overlay.rs` | the NOTIFICATIONS pill, the board and the toast stack |
| `src/api/src/i18n/mod.rs`, `src/api/src/i18n/table.rs`, `src/api/src/i18n/files.rs` | the interface language (see "Languages" below): `tr`, `tr_fmt`, the table with `{name}` patterns, the language files compiled in and the operator's folder |
| `src/lang/*.json` | the four languages (en, de, fr, dl), one file each; the Map Entities and Schwalby plugins' files sit in their own `lang/` folders |
| `src/api/src/ui/frost.rs` | the frosted glass under every `.glass` surface |
| `src/api/src/ui/fonts.rs`, `src/api/src/ui/icons.rs`, `src/api/build.rs` | Inter, JetBrains Mono and the Lucide icons, from the web's own files |

## Languages

The interface is multilingual the way the web interface is, with the same
files (`src/lang/en.json`, `de.json`, `fr.json`, `dl.json`, compiled into the
program). The English text of the interface is the key and there are no
message ids, so a page writes plain English and a text a language does not
list stays English. `en.json` is empty on purpose: English is the built in
default. `python/src/lang` holds the same files for the web interface; a test
(`the_bundled_files_are_the_web_interfaces_files`) fails when the two copies
differ, until the web interface is removed.

Where the swap happens. The translation sits where text is laid out, so a
widget follows the language without its page knowing: `ui::style::galley`,
`ui::text::wrapped`, `wrapped_weight` and `clamped`, the hint of a text box
and the plot titles all call `i18n::tr`. A text built with `format!` is
translated whole, as the web translates a text node, so `Settings of Vibration`
is found by the entry `Settings of {name}`. Call `i18n::tr` and `i18n::tr_fmt`
yourself where text is stored or compared away from a painter:

```rust
use crate::i18n::{tr, tr_fmt};
let title = tr("Reload languages");
let line = tr_fmt("Reloaded. {n} languages installed.", &[("n", "4")]);
```

- A missing key, an empty translation, an unreadable file and a text without a
  letter all give the English text back; nothing panics.
- Text set in capitals (`MODE`) is found under its web wording (`Mode`, which
  CSS capitalises there) and comes back in capitals.
- A label that ends in a parameter (`Action on RC loss (NAV_RCL_ACT)`) is
  translated by the label alone.
- Data is not wording and is laid out with `style::galley_mono`, which does not
  translate: parameter values, names, the log, the terminal.
- The language comes from the config's `language` (`settings::Effects`), is
  put in force at start and on every change, and the Settings picker writes it
  the way the web does. The picker lists the compiled in languages and the files in
  `~/.corvus/languages`; Reload languages reads that folder again.
- A longer text must still fit. Where the web ellipsizes (`text-overflow`,
  `line-clamp`) use `ui::text::clamped`; a row of buttons shares its width by
  the width of their text, as a grid does.
- `CORVUS_GUI_I18N_MISSING=file.json` (with `CORVUS_GUI_SHOT`) writes the
  English texts the page showed that the language lacks, as a blank file's
  `strings`. `tools/i18n.py` in `python/` still checks the files.

`cargo test i18n` holds the loader, the fallback, a missing key, placeholders
and that every literal given to `tr` or `tr_fmt` in `src/` is in every language
file.

## Plugins

A plugin is a crate of its own (`src/plugins/<id>/`, the folder named as the
web plugin's) with a Rust type behind the `Plugin` trait of `corvus_api`. It
depends on `corvus_api` and on nothing inside the program, so it cannot reach
into `app`, `map` or `panel`; what it may use is the API: the widgets of
`corvus_api::ui`, `i18n`, `net`, `data` and `plugin` (the trait, `Host`,
options, shapes). The crates are compiled into the program and listed by
`build.rs`. There is no loader and no JavaScript: a plugin works the same on every platform the program
is built for, Android included. Third party JavaScript plugins that the web
interface loads from `~/.corvus/plugins` do not run here; Settings lists them
as "installed, not loaded".

What the operator sees is what the web interface shows:

- **PLUGINS tab.** One card per plugin (icon, name, description), a click opens
  the plugin under a back button. A plugin that can have a tab and whose
  **Own tab** option is on has a tab of its own before PLUGINS instead. That
  option is the only per plugin switch the web has, so there is no separate
  enable switch here either; `ui.plugin_tabs` is its default.
- **Settings, Plugins.** The rows of `GET /api/plugins` marked with what the
  registry knows ("built in 1.0.0 · own tab", "not loaded"), and a gear for a
  plugin with options. The gear opens a dialog in which every change is in
  force at once and saved through `POST /api/plugins/settings` (`_options`
  of the plugin's own `config.json`); a refused save puts the control back.
- **Saved state.** `Host::save_settings(patch, replace)` writes the plugin's own
  `config.json` through the same endpoint, merging unless `replace` is set. The
  options are the operator's: a plugin neither writes nor clears them.

### Writing one

```rust
pub struct Hello { opened: f64 }

impl Plugin for Hello {
    fn info(&self) -> Info {
        Info { id: "hello", name: "Hello", icon: "puzzle", description: "Says hello",
               version: "1.0.0", order: 100, can_tab: false }
    }
    fn options(&self) -> Vec<OptionSpec> { vec![OptionSpec::toggle("loud", "Say it loudly", false)] }
    fn open(&mut self, h: &mut Host) { self.opened = h.now; }
    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32 {
        // draw into `area` from its top and return the height used
        let g = style::galley(ui, "Hello", 13.0, Weight::W600, style::text_1(), 0.0);
        ui.painter().galley(area.min, g.clone(), style::text_1());
        g.size().y
    }
}
```

That type goes in `src/plugins/hello/src/lib.rs` of a crate whose
`Cargo.toml` depends on `corvus_api` (copy one of the four), with a
`pub fn plugin() -> Box<dyn Plugin>` that makes one and a `pub const LANG:
&[(&str, &str)]` of its `lang/<code>.json` files (`include_str!`). Add the crate
to `members` and `default-members` of the root `Cargo.toml` and one dependency
line `corvus_plugin_hello = { path = "src/plugins/hello" }` to its
`[dependencies]`: `build.rs` finds the folder, checks the line, and the
registry registers the language files and makes the plugin. The id is the
folder name of the plugin's settings, so it must be a safe name
(`[A-Za-z0-9][A-Za-z0-9_.-]*`); `order` places the card (lower is earlier, ties
by name). Test its `Info` against `python/plugins/<id>/plugin.json` as the four
do. What the trait offers, all optional but
`info` and `show`:

| | |
| --- | --- |
| `options()` | the settings of the gear: `OptionSpec::toggle`, `select`, `text`, `number`; read them with `h.options()` |
| `start(h)` | once per session, as soon as the saved settings are known, open or not: put a saved shape back on the map |
| `tick(h)` | every frame, open or not; the telemetry state is `h.state` (the browser's `subscribe`) |
| `open(h)`, `close(h)` | the view is about to be shown, and gone (`init` and `destroy`): leave nothing behind |
| `show(ui, area, h)` | draws the view; the registry gives it a scrolling area 14 px from the sides |
| `options_changed(h)` | the operator changed an option; every value is in `h.options()` |
| `map_event(event, h)` | the pointer did something to one of its shapes: `Click`, `Drag`, `DragEnd`, or the `Picked` point of `h.pick_point()` (`PickCancelled` on Escape or a right click) |
| `ssh_setup_done(tag, saved, h)` | the login dialog asked for with `h.ssh_setup(need, tag)` was answered |

The `Host` is the browser's `api`: `h.map()` (`drawLine`, `drawPolygon`,
`drawCircle`, `drawText`, `drawButton`, `drawMarker`, `remove`, with the 21
symbols of `shapes::ICONS`; each key is the plugin's own and drawing it again
replaces it), `h.settings()` and `h.save_settings()`, `h.get`, `h.post` and
`h.post_any` (a POST whose body is the answer whatever the status), all on a
thread of their own and picked up by polling the returned `Reply`, `h.notify`
(a toast and a line on the board, both marked with the plugin's name),
`h.console`, `h.terminal` (a terminal window on a saved SSH session),
`h.fit` (frame points on the Home map) and `h.pick_point`.

UI is built from `src/api/src/ui/` like any page: `form::Form` is a cursor and
`form::field`, `form::switch_field`, `kit::select`, `kit::Button` and
`field::committed` paint one block each. A plugin does not paint a box or a list
of its own. Shapes are painted on both maps; only the flat map answers the
pointer for markers and buttons.

Rules go in a `model.rs` beside the plugin, with no `egui` in it, and their
tests quote what the web helper answered (run the plugin's `.js` under `node`
with a stub `window.Corvus` and print the result, as the existing tests did).

### What is not ported

- **MCP tools.** A plugin offers tools to an AI client with `mcp_tools(h)`
  (the web's `mcpTools`), runs a call in `mcp_call(name, args, token, h)` and
  hands the answer back from `mcp_take(token)` once it has one, which may take
  frames (a save, a press). The window announces the tools with its heartbeat
  and at once when they change, and answers a `call` event through
  `POST /api/mcp/result` (`src/mcp/mod.rs`). Schwalby is the one plugin that
  offers any, behind its two switches.
- **Flight bar buttons.** The web `Corvus.plugins` has none; `drawButton` on the
  map is the closest thing and is in `shapes.rs`.
- **Reload plugins** re-reads the list of installed folders, as it always did;
  a compiled-in plugin has nothing to reload.
- Markers and buttons answer the pointer on the flat map only. On the 3D map
  they are drawn, not clickable.

## Settings

The page writes what the web page writes, key for key, so the two interfaces
share one config. Differences that follow from a native window:

- **Plugins.** The Plugins pill lists the compiled-in plugins with a gear for their options (see Plugins below).
- **Files.** Nobody types a path. Every file or folder on this computer is
  chosen with the system file dialog (`rfd`: Cocoa, Windows, and the desktop
  portal on Linux, so no GTK is linked); `src/api/src/ui/filepicker.rs` wraps it and
  `src/api/src/ui/pathfield.rs` is the field that offers it (`form::path`): a read
  only box in the style of a text field showing the path in mono, cut in the
  middle when long, with Choose (and Clear where the setting is optional), or
  Choose file and Choose folder where either is valid. The dialog opens in the
  folder of the value held. Used by the company logo (drawn at the far right of
  the top bar, `src/home/company.rs`), the parameter export folder, the
  settings export folder and import file, elevation files or folders, SSH key
  files, the flight log import and download folder, the parameter export folder
  and import file, and the firmware file. A setting that names a place on the
  backend's machine (export folders, the flight log folder, SSH keys) falls
  back to a text box, with a line in its hint saying so, when `--server` points
  at another computer (`Server::is_loopback`, `pathfield::set_backend_remote`).
  `--page settings:TAB --scroll PX` opens a Settings tab scrolled.
- **Language.** The language picker lists `GET /api/languages` and writes
  `language`, as the web page does. The native interface's own words are
  English only for now: its strings are not yet looked up in `src/lang`, so a
  language file changes the web interface and is stored for the native one.
  Making native strings translatable is a later step, taken once every page is
  native.
- **Tablet mode** is the web page's own, kept in the browser, and has no
  native counterpart.
- **Switches** that the native pages do not read yet (HUD, track, top bar
  blocks, compass, flight bar) are stored in the config and take effect in the
  web interface; the native pages follow as their consumers learn the keys.
  Theme, size, units, the Mission entry, the distance tool, the console and
  SSH tabs and the base layer are live.

## Analysis

The review's ground track is drawn as a chart with equal axes, and has the web
page's Chart / Satellite switch: the same path over imagery of the Home map's
layer (or its provider's satellite layer when that is a street map), on the
backend's tile proxy so it works offline. A ULog from outside the download folder is opened by its path, and
posted to the backend as bytes the way the web page posts a picked file.
`--page analysis:ulog`, `analysis:tlog`, `analysis:review` and
`analysis:scope` open a view, `--page logs` and `--page settings:map` the
other pages, with `--scroll` for how far down.

## The 3D map

`walkers` draws a flat map only, so 3D mode (`python/src/js/map.js` `set3D`) has a
renderer of its own: OpenGL in an egui paint callback (`src/map3d/gpu.rs`),
with a depth buffer (`depth_buffer: 24` of the window). Meshes are built once
on the CPU and stay on the GPU; a frame is uniforms and draw calls.

- **Space.** Everything is in metres east, north and up around the view
  centre (`camera3d.rs`), which is exact enough for a view of tens of
  kilometres and keeps the maths in plain `f64`. The camera looks at the
  centre from a distance that gives the same metres per pixel as the flat map
  at the same zoom, tilted by `THREE_D_PITCH` (60 degrees), at the map's
  bearing, with MapLibre's 36.87 degree field of view.
- **Ground.** A quadtree of web mercator tiles, split while a tile would
  cover more than about 230 px, so the near ground is fine and the horizon
  coarse. A leaf is dropped when it is outside the real view frustum (taken
  over the sea level plane up to 5000 m, the ground not being known yet), and
  the copy of the world a leaf is in is counted, so the map continues over
  longitude 180; the centre stops at the mercator limit. Each leaf is a 16 by
  16 grid of quads. Heights come from the elevation tiles (`terrain.rs`,
  terrarium or mapbox packing, PNG or WebP, 256 or 512 px) through
  `/api/tiles/<dem>/z/x/y.png`, the proxy the web map reads, so its cache
  serves offline. A leaf reads the tile four zooms up (`terrain::leaf_part`),
  which gives it one pixel a cell and 16 times fewer tiles than one per leaf. The imagery is the base layer's own
  tiles from the same proxy, draped by texture coordinates. A tile whose
  data has not come yet is drawn from its nearest ancestor, never left out.
- **Depth.** The depth buffer settles what hides what: a ridge hides the
  valley behind it, a building the one beyond. The callback clears depth inside
  its clip, draws the ground opaque and the buildings blended after it.
  Imagery is a mipmapped, anisotropic texture per tile; hillshade is baked
  into a per vertex shade.
- **Meshes.** A leaf is a 16 by 16 grid plus skirts around its north west
  corner, made once per elevation tile (`build_leaf`); its place is a uniform,
  so panning moves nothing on the GPU. Heights are f32 around the leaf's own
  origin, which keeps millimetres at tens of kilometres.
- **Elevation models.** The catalogue is `terrain` of `/api/tiles/sources`
  with the web's choice rule (`terrainFor`: the operator's pick, else the one
  of the map service, else the preferred Copernicus, else SRTM). The
  operator's own models (GeoTIFF, XYZ) are served by the backend as the same
  tiles, so they need no reader here.
- **Cracks.** Neighbouring leaves can come from different levels or models,
  so their edges do not meet. Each leaf hangs a skirt below its edges.
- **Budgets.** Per frame at most 40 leaf meshes are built (nearest first), 64
  meshes and 16 textures (5 ms) uploaded; what does not fit waits for the next
  frame and the view asks for it. A leaf whose own image is not on the GPU yet
  draws its nearest ancestor's (`Item::fallback`), so a tile arriving does not
  blink. Tiles are asked for from the far leaves to the near ones because the
  cache serves the last asked first. The GPU holds at most 2400 meshes and 700
  textures (`gpu::Pool`, longest unused go first); the track ribbons follow the
  camera and are made, drawn and freed in one paint (`Item::transient`).
- **Failure.** `gpu::check_caps` reads the context (GLSL with `in` and `out`,
  depth bits, texture size) before the first draw, a shader that does not
  compile or link is reported with its log, and `gpu::failure()` then makes the
  map fall back to the flat one while the 3D panel says why.
  `CORVUS_GUI_3D_FAIL=reason` forces it for a look. With most ground without
  an image for 2.5 s and tiles the backend could not give, a pill says no map
  is saved for the area and it is not available offline.
- **Looking at what it costs.** F3 over the 3D map, or `CORVUS_GUI_3D_DEBUG=1`,
  shows leaves, meshes built, draws, imaged draws, uploads, live GPU objects
  and the milliseconds of planning and submitting, and logs them every 5 s.
- **Pictures.** `src/map3d/shots/run.py` renders five fixed views and compares
  them with `ref/*.jpg` (see its README for how to refresh them).
- **Camera.** The wheel zooms to the pointer, tilt stops at 60 degrees as
  MapLibre's does, the camera is kept 8 m above the ground under it by lowering
  the tilt, and two fingers pinch, twist and tilt.
- **Hills and markers.** Ground and track are depth tested. Screen markers
  (home, vehicle, other vehicles, plugin markers and text) and the pieces of the
  geofence outline are hidden by a ray test against the elevation (`line_blocked`).
- **Slices.** Done: the camera, the ground with imagery and heights, hillshade,
  the vehicle, home and track in 3D, drag to pan, ctrl or right drag to turn
  and tilt, wheel to zoom, the elevation model list with the operator's own
  models (they are listed in `terrain` and served as the same tiles), the
  elevation credit with the height at the centre, the sky and a haze band at
  the horizon (seen from about 70 degrees of tilt), the ground colour where
  imagery has not come, and the buildings.
- **Buildings.** The full 3D (the panel's switch, kept as `three_d_detail`)
  reads the cells of `/api/buildings/15/x/y.json` for the ground in view,
  nearest first, 36 at most, from zoom 14. A dense city centre (a few hundred buildings a cell) was looked at in the running app against the backend's own cell cache. Each cell becomes one mesh once the
  terrain under it is loaded. A cell the backend has not got yet
  (`pending`, or `unavailable` offline) is asked for again every 3 s. Walls
  and roofs of a cell go into one mesh, 4000 buildings at most, in the web's slate at 85 percent opacity with each wall
  darker toward its foot. The cells are gzipped as the backend stores them,
  which is why `flate2` is a dependency (already in the build through `png`).
  A building sits on the terrain height at its middle.
- **Switching it on.** `Map::set_three_d`; the rail button calls it.

## The UI base

Everything a page draws that is not its own picture comes from `src/api/src/ui/`, so
one change reaches every page. A page does not call `rect_filled` or
`rect_stroke` for a card, a list row, a field, a message, a bar or a switch;
if the control it needs is missing, it is added to `src/api/src/ui/` first, in the
file that names it. Drawings (the airframe, the plots, the map, the handset)
are the pages' own.

Which widget for what:

| You need | Use |
| --- | --- |
| Any bordered box: a card, a note, a well, a state coloured box | `card::paint(painter, rect, Look::...)`. `Look` holds fill, border and corner of each kind (Panel, Page, Inset, Note, Raised, Quiet, Well, Console, Chosen, Tone, and Custom for state driven colours). For a height known only afterwards, `card::reserve` then `card::fill` |
| A floating window's surface | `card::window` |
| A dialog | `modal::show`, with `modal::actions` for its buttons |
| A button, an icon button | `kit::Button` (`Variant`), `kit::icon_btn` |
| A text box | `field::TextField`; `kit::text_field` for the plain case; `field::outline` for the red outline of a refused value |
| A select | `kit::select` (the list is `menu::dropdown`) |
| Any other dropdown, list menu or popup menu (a chooser with dots, a list of actions with icons, a head, headings between groups, a note under a label) | `menu::Menu::new(id, Anchor, &Look::..., &items)` with `Item::Head`, `Section`, `Note`, `Row` (`.icon`, `.note`, `.trail`, `.on`); `.selected`, `.head` for a painted head, `.opacity`; `show(ctx)` returns the `Outcome` (picked value, dismissed, rect). Never paint rows by hand |
| A check box, a switch | `kit::checkbox`, `toggle::check_box`, `kit::switch` |
| A caption, a box and a hint (a field) | `formfield::field`, `field_with`, `field_info` at absolute positions, or `Form::field` and `Form::input` on a column |
| A line of feedback (ok, error, warning) | `message::show` with a `message::Tone`; `Form::message` on a column |
| A grey notice or the amber banner | `message::boxed`; `Form::box_note`, `pagekit::warning_banner` |
| Selectable cards with a label, a description, an icon or swatch | `optioncards::show` with `Card`; `Form::option_cards` on a column |
| A list row's hover and selected background | `listrow::paint` |
| A progress bar | `progress::Bar` |
| A small outlined or filled label | `chip::Chip`, `Chip::tag` |
| A stepped slider, the icon pills, a segmented switch | `slider::show`, `pills::show`, `segmented::show` |
| A page's head, section title, tabs, tile, back button | `pagekit` |
| A hint icon and its sheet | `popover::info` |
| Wrapped text, a status dot | `kit::wrapped_weight`, `kit::dot` |
| A column of cards in the Settings manner | `Form` (`form::section`, `form::card`, `switch`, `row`, `actions`), a cursor over the primitives above |
| The nice ticks of an axis | `chart::tick_step`, `ticks`, `tick_label` |

The tokens under all of it are in `style.rs` (colours of the six themes,
radii, fonts). Where the web interface and an older native page disagreed by a
pixel or two, the base follows the stylesheet: a `.ui-msg` and a notice box
include their 1 px border inside the padding, an option card's hover border is
`--panel-hairline-hover`.

## Looking like the web interface

Every measure is taken from the CSS with one CSS pixel as one egui point,
so a block here is the size, weight and colour of the element it copies.
The fonts are the web's WOFF2 files and the icons are cut out of its
`lucide.min.js`, both at build time, so there is no second copy of either.
The glass panels are frosted as the CSS frosts them (`backdrop-filter:
blur(24px) saturate(180%)`, then 60 % white, 55 % on the map rail): a paint
callback on the OpenGL renderer copies what is already drawn under the
panel, blurs and saturates it inside the panel's rounded corners
(`src/api/src/ui/frost.rs`), and the fill paints over it. The copy is taken before the
panel's shadow, because a browser draws a box shadow outside the box only.

The theme is the config's `theme.name`, as in the web interface, and is read
once at start; the palettes are `themes.css` value for value (the dark
themes share one base, blue and pink and orange change the accent family,
blue also the navigation colour). The frost saturates 190 % and brightens
10 % on the dark themes, and the logo is the white cut there.

The window title comes from `GET /api/version`; the crate carries no
version of its own.

## Reliability

The interface is used in the field, so it must not crash, hang or grow.

- **Panics.** `crash::install` writes every panic with a backtrace to
  `~/.corvus/gui-crash.log` (cut to its newest half past 256 KB, the same panic
  logged once in 5 s) and shows nothing. A panic while a frame is drawn is
  caught (`crash::guarded` in `App::ui`): that frame shows "This page stopped
  working" with a Back to Home button and the next frame tries again. A panic
  on the main thread outside the guard still takes the backend child down.
  Locks that a panic elsewhere could poison are taken with
  `net::lock::Relock`, so one panic does not become a panic every frame.
- **Bounds.** Nothing that grows with the session is unbounded: the console
  (2000 lines), the notification board (200 own lines, 100 commands in flight,
  toast memories forgotten after their repeat window), the translation cache
  and miss set, tracks (20000 points), the HTTP client (16 KB per head line,
  200 headers, 256 MB per body, 16 MB per SSE line or event, write timeouts),
  the 3D map (at most 480 GPU textures and 2400 meshes, the least recently
  drawn go first, and 160 MB of decoded imagery and 48 MB of decoded elevation
  on the CPU side, counted in bytes so a source with larger tiles is held too), `data::intern` (256 texts). A test holds each bound.
- **Soak.** `python/tools/gui_soak.py` starts a sandboxed backend with a
  simulated PX4 (never the operator's own Corvus), starts the interface with
  `CORVUS_GUI_SOAK=1` and writes a CSV row every 10 s with the resident memory,
  threads and open descriptors of the interface and the backend:

  ```bash
  python3 python/tools/gui_soak.py --binary target/release/corvus-gcs \
      --python .venv/bin/python --minutes 15 --csv soak.csv
  ```

  The driver cycles twelve steps of 4 s: the map panned and zoomed, the 3D map
  opened and moved, every page, one setup tile at a time, a Settings tab, the
  Mission page, the language switched. `--mode 3d` stays in the 3D map and
  jumps between eight places (Alps, Munich, San Francisco, Everest, Tokyo,
  Madrid, ...) every step, the worst case for the view's memory; `--max-mb N`
  makes the script exit with 2 when the interface's footprint passes N. On the
  development machine a 7 minute 3D run held 0.9 GB (1.2 GB before the byte
  budgets) with a flat slope; `the_worst_case_the_view_can_hold_stays_under_a_gigabyte`
  holds the arithmetic.
- **Geofence round trip.** `python/tools/geofence_roundtrip.py` drives the
  fence endpoints against three sandboxed simulators (one accepting, one
  refusing, one that stops answering after two corners, `CORVUS_SIM_FENCE`).

## Toolchain

eframe 0.37 needs Rust 1.95 or newer. `rust-toolchain.toml` at the repository
root pins 1.99 for the interface; `src/link` has its own toolchain file and
keeps the default. rustup installs what is pinned on first use.

## Checks

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The root `Cargo.toml` is also a cargo workspace over `src/api` and the plugin
crates (`default-members` lists them all, so the three commands above cover
every crate, with one lock file and one target folder). `src/link` is left out
on purpose: it has its own toolchain file (stable), its own lock and the
maturin build, so the two pinned toolchains never meet.

Every rule a block of the bar follows has a test with the web interface's
expected text, including `toFixed` and `Math.round` rounding.
