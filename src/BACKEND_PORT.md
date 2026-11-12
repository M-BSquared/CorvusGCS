# Backend port: Corvus GCS without Python

Done. The native app runs with no Python at all: no `serve.py`, no bundled
CPython, no PyO3, no pymavlink, no web engine. The backend that was
`python/corvus` is the library crate `corvus_core` (`src/core`), run inside the
interface's process. Decided with the user on 2026-11-06; the Python program
was deleted on 2026-11-12. This file keeps the decisions and the rules that
still hold for any change to the core.

## Decisions

- **In the same process.** The interface calls the core directly
  (`Core::internal_call`). Its threads are Rust threads, torn down on the normal
  exit path (`AGENTS.md`, lifecycle).
- **HTTP stays as an option, not as the way in.** A local HTTP, SSE and
  WebSocket server (`corvus_core::http`) serves the same routes to MCP
  (`POST /mcp`), `--server` clients and a second station. The interface itself
  does not need it. The stand alone binary is `corvus-core`
  (`src/core/src/bin/corvus-core.rs`).
- **No async runtime.** A small blocking server on `std::net`, one thread per
  connection. The shutdown rule and the 60 fps interface bound the choice.
- **Fixtures instead of the reference.** What the Python program and pymavlink
  answered was dumped into `src/core/tests/data` and `src/link/tests/data`
  before they were deleted. The Rust tests play those files. They are not
  regenerated to match the code, and a test that disagrees with one is a bug in
  the Rust until the user says the old answer was wrong.

## Status

Every route of the Python server is registered by a module of the core
(`src/core/tests/no_unavailable_routes.rs`), the interface runs the core in its
own process, and no artifact holds Python (`build-*.sh`, `corvus-build
bundle-check`, CI). The open work, in priority order, is in `PLAN.md` section 0.

## Rules that still hold

- The same words, numbers, files and wire bytes as the frozen answers. Where an
  endpoint answered JSON, the Rust answer is equal for the same input, and the
  same for files under `~/.corvus` (plans, config, plugin settings), which stay
  readable by older copies.
- Every module keeps its tests: a bug fix gets a regression test, a new route
  gets a test. Protocol engines are driven against scripted vehicles over real
  UDP or a stub, with a fake clock where time matters; never against hardware.
- Flight stack rules of `AGENTS.md` apply unchanged: `src/core/src/dialect.rs`
  is the one place that knows PX4 from ArduPilot; PX4 v1.16 to v1.18, ArduPilot
  4.3 to 4.6; never send a command or parameter the connected firmware does not
  know.
- A module is a folder with a `Module` (`register` declares, `start` spawns,
  `stop` joins in reverse order). A route registered twice fails the build.
- Standard library first; a new crate is justified in its commit and added once
  to the root `[workspace.dependencies]`.
- One commit per finished module, gates green after every commit
  (`cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings`, `cargo test --workspace --locked`).

## Map of the code

| Area | Folder or file in `src/core/src` |
| --- | --- |
| Foundation | `core.rs`, `module.rs`, `router.rs`, `request.rs`, `response.rs`, `hub.rs`, `events.rs`, `store.rs`, `buffer.rs`, `ws.rs`, `http/`, `config.rs`, `settings.rs`, `bundle.rs`, `paths.rs`, `lock.rs`, `version.rs`, `languages.rs` |
| MAVLink | `bridge.rs`, `protocol/`, `dialect.rs`, `priority_lock.rs`, `tlog.rs`, and the crate `src/link` |
| Vehicle | `vehicle.rs`, `mission.rs`, `geofence.rs`, `fleet.rs`, `forwarder.rs`, `autoconnect.rs`, `battery.rs`, `setup_schemas/` |
| Map data | `mapdata/` (tiles, cache, proxy, downloader, DEM, buildings, CRS, geocoding) |
| Logs and firmware | `logs/`, `firmware/` |
| Peripherals | `peripherals/` (serial ports, RTK, SiK, SSH, shell, video, Betaflight MSP) |
| AI and plugins | `mcp/`, `plugins/`, `bundle.rs` |
