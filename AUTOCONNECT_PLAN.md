# Corvus GCS — QGC-Style Auto-Connect: Implementation Plan

## 0. Header

**Status: IMPLEMENTED.** The plan below is kept as written, as the record of what was decided and why. The §14 questions were answered with this document's own recommendations: Q1 = A (the UDP *fallback* literal is `udp:0.0.0.0:14550`, living in `corvus/autoconnect.py`; `DEFAULT_MAVLINK_CONNECTION` and the `POST /connect` default are untouched), Q2 = 2.5 s, Q3 = A (auto-dial), Q4 = A (disconnect stays disarmed), Q5 = A (backend + config file; no Settings switch yet). Option A *and* B were taken for §6 status visibility: the store keys ride SSE and `GET /api/mavlink/auto` is a read-only view of them.

| Field | Value |
|---|---|
| Document | `AUTOCONNECT_PLAN.md` (new file, repo root) |
| Scope | Minimal smart default auto-connect. NOT a full link manager. |
| Date | 2026-09-16 |
| Version policy | This document contains **no version literal** as source of truth. Any version shown in UI/logs/about is read at runtime from `VERSION` via `corvus/version.py` (`__version__` / `VERSION` / `get_version()`) or `GET /api/version`. No hand-edit of `VERSION` or `corvus/version.py` is part of this plan. |
| Attribution rule | Corvus GCS is developed **with** the Universität der Bundeswehr München — never "at". This plan introduces no licence/copyright lines. |

### 0.1 Scope locks (non-negotiable)

1. **Minimal smart default, NOT a full link manager.** No multi-link store, no per-link enable/disable grid, no parallel dialing, no link priority editor. One bridge, one active connection string at a time.
2. **Priority order is fixed:** (1) USB direct Pixhawk first, then (2) SiK radio, then (3) UDP 14550 fallback / SITL.
3. **Never steal an active link.** Auto-connect acts **only when disconnected**. Definition of "disconnected" is specified in §3/§10 (no fresh vehicle heartbeat + store `link_status` in `disconnected`/`reconnecting`).
4. **Mid-session plug only suggests in UI.** If a new USB/SiK device appears while already connected (or degraded with fresh heartbeat), the backend publishes a `link_suggestion`; it does **not** tear down the active link.
5. **Manual `POST /api/mavlink/connect` always wins for the session.** A successful manual connect sets a session flag `manual_override` that suppresses all auto-dial actions until cleared (see §3.4, §7).
6. **Stdlib only.** New module `corvus/autoconnect.py` uses stdlib only (`threading`, `time`, `dataclasses`/`typing`, `pathlib` if needed). No new third-party dependency.
7. **No behaviour change to PX4 target handling.** v1.16 / v1.17 / v1.18 remain the regression set; v1.12–v1.15 best-effort fallback unchanged. No `MAV_CMD` / parameter-write change in this plan.

### 0.2 Non-goals (explicitly out of scope)

| # | Non-goal | Rationale |
|---|---|---|
| NG1 | Full QGC LinkManager / Comm Links editor (named links, multiple simultaneous links, per-link autoconnect toggles, Bluetooth, NMEA, RTK, LibrePilot, PX4Flow toggles) | QGC has `AutoConnect Pixhawk / SiK Radio / LibrePilot / PX4Flow / RTK GPS / NMEA` plus manual `Comm Links` with `Auto Connect on Start` and high-latency links. We deliberately implement only the `usb + sik + udp_fallback` subset as booleans. Full parity is a separate project. |
| NG2 | MultiVehicleManager per `(link, sysid)` | QGC multiplexes vehicles per link/sysid. Corvus GCS has a single Vehicle State Store and single `MavlinkBridge`. Multi-vehicle is out of scope. |
| NG3 | Parallel probing / racing dial attempts | Would need multi-socket management, `flock` coordination, and teardown races. Single sequential decision only. |
| NG4 | Bootloader-aware flashing integration | No bootloader-wait outside the existing flash path. Autoconnect must *skip*, not claim, bootloader devices (see §10). |
| NG5 | Frontend auto-`POST` without operator (except backend-driven disconnected-auto case) | Frontend remains display + manual trigger; the one automatic dial path is the backend watcher acting while disconnected (see §8). Browser never scans ports itself. |
| NG6 | Changing GCS MAVLink identity, signing, dispatch guards, heartbeat cadence, interval/version/home logic, tlog cadence | All unchanged (see §9/§10). |
| NG7 | Changing shutdown order or watchdog | Order stays `flash → logs → forwarder-detach → mavlink → ssh → store → http+tiles → server_close` with 2 s `os._exit` watchdog (see §10). |
| NG8 | Changing bind-default globally in this plan | The `0.0.0.0 vs 127.0.0.1` unification is a **decision for the user** (§14); this plan does not pre-decide it in code. |

---

## 1. Goals + Acceptance Criteria

### 1.1 Goals

- G1: Fresh boot with a USB Pixhawk plugged in connects **without any click**.
- G2: SITL-only boot (no USB/SiK, simulator on UDP 14550) connects **without any click**.
- G3: No-vehicle boot shows an honest **AUTO scanning** state and keeps the existing reconnecting behaviour; it never spins hot, never opens serial ports to probe, never crashes.
- G4: Plug-while-disconnected auto-dials **once** (no flap loop).
- G5: Plug-while-connected **only suggests** (UI row/badge), never tears down.
- G6: Manual connect always wins for the session; disconnect stops all retries deterministically.
- G7: Zero new sockets/threads beyond **one** watcher thread; zero serial opens for enumeration.

### 1.2 Acceptance criteria (observable, testable)

| ID | Scenario | Setup | Expected observable behaviour | Where observed |
|---|---|---|---|---|
| AC1 | Fresh boot, USB Pixhawk present | USB FC enumerated, no CLI arg, default config | Connected without click; log line names winner + reason `usb-direct` (see §4); HUD shows telemetry | Backend log + `GET /api/version` unchanged + SSE `link_status=connected` + HUD |
| AC2 | SITL-only boot | No USB/SiK, Gazebo SITL publishing to UDP 14550 | Connected without click via `udp_fallback`; reason `udp-fallback/sitl` | Same as AC1 |
| AC3 | No-vehicle boot | Nothing attached, no SITL | UI shows `AUTO scanning` + existing `CONNECTING`/`RECONNECTING` cycle (`src/js/link.js:505-575`); backend in `_wait_vehicle_heartbeat` 10 s → `ConnectionError: No heartbeat` → `_reconnect_delay` backoff loop forever; CPU idle | `src/js/link.js:505-575`, `corvus/mavlink_bridge.py:1371-1421`, `corvus/mavlink_bridge.py:1152-1161` |
| AC4 | Plug-while-disconnected | Boot with no vehicle → wait for `reconnecting` → plug USB Pixhawk | Backend auto-dials the new USB device **once**; next heartbeat succeeds; no repeated stop/start flapping | Backend log + SSE transition `reconnecting → connected` |
| AC5 | Plug-while-connected | Already connected (USB or SITL) → plug second device (e.g. SiK or second FC) | No teardown; UI shows suggestion row `New device: <dev> (<kind>) — [Connect]`; backend log `suggestion-published` | Store `link_suggestion` + SSE + `src/js/link.js` suggestion row (§8) |
| AC6 | Manual wins session | Auto-connected via USB → operator `POST /api/mavlink/connect` with `udpout:...` (router) | Manual connection applied immediately; `manual_override=true` for session; subsequent USB plug/unplug does **not** auto-dial away | `POST /api/mavlink/connect` (`corvus/server.py:1936-1966`) + `GET /api/mavlink/auto` (§6/§7) |
| AC7 | Disconnect stops retries | Operator `POST /api/mavlink/disconnect` while watcher active | Bridge stopped (idempotent, `corvus/server.py:1968-1994`); watcher does **not** re-dial until re-armed per §14 decision (default proposal: stays disarmed until next manual connect or restart — user to confirm) | SSE `link_status=disconnected`, no further `set_connection` calls |
| AC8 | Never steal | Degraded-but-fresh-heartbeat link + new USB appears | No auto-dial; only suggestion. Watcher no-op predicate false (see §3.5/§10 table) | Log `watcher-noop: fresh-heartbeat` |
| AC9 | No phantom dial | Only `ttyS*` / Bluetooth / `cu.*`-without-`tty.*` / test `pty` present | No dial; reason `no-candidate (phantoms filtered)` | `corvus/mavlink_bridge.py:163-167` filter + resolver log |
| AC10 | Bootloader not stolen | Device enumerating in bootloader (flash mode) | Skipped with reason `bootloader-skip`; flash path unaffected | §10 rule |

---

## 2. Current-State Map

### 2.1 Startup chain (today — precedence)

| Step | `serve.py` path | `app.py` path | Notes |
|---|---|---|---|
| CLI / config load | `serve.py:125-156` parses args; `serve.py:171` calls `create_server` | `app.py:494-519` parses args; `app.py:546` calls `start_backend` | Precedence today: `sys.argv[2]` else `cfg.mavlink_connection` (verified fact; do not re-derive). |
| Bridge construction | `corvus/server.py:4842-4844` constructs `MavlinkBridge(store, DEFAULT_MAVLINK_CONNECTION)` | `app.py:176`, `app.py:180`, `app.py:261-266` mirror: construct then `apply_startup_connection` then `mavlink.start()` | Both layers do **construct → apply_startup_connection → `mavlink.start()`**. |
| Startup-connection resolution | `corvus/server.py:4808-4833` `apply_startup_connection(...)` | `app.py` mirror of same helper (see `app.py:161-267` region) | Falls back on `ValueError` (invalid string → default). Must keep this fallback (§4). |
| Server start | `corvus/server.py:4922` (`mavlink.start()` inside `create_server` `corvus/server.py:4836-4923`) | `app.py:261-266` | No thread is started before `mavlink.start()` today. Keep that ordering (§5). |
| Default skew (pinned) | `corvus/server.py:4805` `DEFAULT_MAVLINK_CONNECTION = udp:127.0.0.1:14550` | `corvus/config.py:119` fresh default `udp:0.0.0.0:14550` | Bridge `__init__` default `udp:0.0.0.0:14550` (`corvus/mavlink_bridge.py:639-643`). Three literals disagree; pin in tests, unify only after §14 decision. |

Pseudocode of today's startup (both entry points):

```text
conn_str = sys.argv[2] if given else cfg.mavlink_connection      # serve.py:125-156 / app.py:494-519
bridge   = MavlinkBridge(store, DEFAULT_MAVLINK_CONNECTION)      # server.py:4842-4844 / app.py:176
apply_startup_connection(bridge, cli_arg, configured)            # server.py:4808-4833 / app.py:161-267 mirror
  try: bridge.set_connection(resolved) except ValueError: bridge.set_connection(DEFAULT)
bridge.start()                                                   # server.py:4922 / app.py:261-266
```

### 2.2 Manual triggers (today — only triggers)

| Trigger | Handler | Behaviour |
|---|---|---|
| `POST /api/mavlink/connect` | `corvus/server.py:1936-1966` | Default payload `udp:127.0.0.1:14550` when body empty; **validates BEFORE** `stop`/`set`/`start` (`server.py:1959-1962` sequence `stop → set_connection → start`). Must preserve validate-before-teardown. |
| `POST /api/mavlink/disconnect` | `corvus/server.py:1968-1994` | Idempotent `stop`. No re-dial today. |
| `GET /api/mavlink/serial-ports` | `corvus/server.py:1571-1587` via `list_serial_ports()` in `corvus/mavlink_bridge.py:787-826` | pyserial + `/dev` glob; phantom filter `_PHANTOM_TTY_RE` (`corvus/mavlink_bridge.py:163-167`); **never opens** ports. Reuse as-is for enumeration. |
| Frontend presets | `src/js/link.js:52-86` | SITL 14550, onboard 14540, `udpout` router, ArduPilot TCP 5760. |
| Frontend serial connect | `src/js/link.js:445-461` `connectSerial` + `src/js/link.js:107-110` `buildConnectionString` (`serial:dev:baud`) | Builds `serial:<dev>:<baud>`. |
| Frontend dial | `src/js/link.js:417-437` `doConnect` via `src/js/telemetry.js:235-237` | Single `POST /api/mavlink/connect`. |
| Frontend recent | `src/js/link.js:170-187` localStorage | Remembers recent strings. |
| Frontend init | `src/js/link.js:577-654` `init` | Restores fields only; **no auto dial**. `refreshPorts` at `src/js/link.js:638`, `644-648`; SSE subscribe at `src/js/link.js:651-653`. Status render at `src/js/link.js:505-575` (`CONNECTING`/`RECONNECTING`). `renderRecent`/`applyConnection` near `src/js/link.js:258-278`, `604-605` (reuse for suggestion row, §8). |
| `POST /api/config` | `corvus/server.py:1048-1237` (`mavlink_connection` write at `server.py:1219` only) | **Never dials.** Keep that invariant. |

### 2.3 Boot-with-no-vehicle sequence (today)

```text
bridge.start()
  -> _run()                                   # mavlink_bridge.py:1051-1094
    -> _connect()                             # mavlink_bridge.py:1199-1326
      -> _wait_vehicle_heartbeat(timeout=10s) # mavlink_bridge.py:1371-1421 (slices + immediate-None bail)
        -> timeout -> raise ConnectionError("No heartbeat ...")
    -> except ConnectionError
      -> delay = _reconnect_delay(n)          # mavlink_bridge.py:1152-1161: 0.5 * 2^(n-1), cap 8s, ±25% jitter
      -> status=reconnecting; sleep(delay); retry forever
UI: link.js:505-575 shows CONNECTING / RECONNECTING
```

Key bridge facts reused by the plan (no change):

- `_VALID_PREFIXES`: `udp: / udpin: / udpout: / udpbcast: / tcp: / tcpin: / serial:` (`mavlink_bridge.py:749-751`); `validate_connection_string` at `mavlink_bridge.py:753-774`.
- `_parse_serial` at `mavlink_bridge.py:869-885`, default baud `57600`.
- `_claim_serial_exclusive` with `flock` at `mavlink_bridge.py:1096-1150` (`nt` early-return).
- `transport()` at `mavlink_bridge.py:887-920`, `_classify_by_descriptor` at `mavlink_bridge.py:922-956`, aliases at `mavlink_bridge.py:958-973`, `is_direct_usb` at `mavlink_bridge.py:975-977`.
- GCS identity `254` / `MISSIONPLANNER` at `mavlink_bridge.py:262-263`.
- `_is_vehicle_heartbeat` at `mavlink_bridge.py:358-398`.
- `_dispatch` guards at `mavlink_bridge.py:2107-2322` (`COMMAND_ACK`, `MISSION`, `SERIAL_CONTROL`, `RADIO_STATUS` pre-guard, vehicle+autopilot guards).
- `_pending_acks` dict + `_operation_lock` `_PriorityLock` at `mavlink_bridge.py:677`.
- Warn/drop timeouts: serial `6/15`, UDP `3/8` (`mavlink_bridge.py:228-231`) vs legacy `_connection_ready` `10/5` (`mavlink_bridge.py:2025-2032`).
- GCS heartbeat 1 Hz persistent at `mavlink_bridge.py:1488-1532`.
- Interval deferral BUG1 at `mavlink_bridge.py:1773-1801` + fallback `mavlink_bridge.py:1803-1862` (cap 3); version triple + 4 s retry at `mavlink_bridge.py:1904-1996` (`v1.18-alpha1` `UNSUPPORTED`); home nudge MAVLink msg 242 at `mavlink_bridge.py:1869-1902`; tlog per-cycle at `mavlink_bridge.py:1565-1596`; `inject_raw` at `mavlink_bridge.py:1632-1655` + `set_frame_sink` at `mavlink_bridge.py:1628-1630`.
- Onboard ports `14540-14549` + warning at `mavlink_bridge.py:1169-1197` (14540 is onboard, never GCS auto-pick).
- `EADDRINUSE` rewrite at `mavlink_bridge.py:1248-1259`.
- `GET /api/version` serves `{product, version, px4_profile}` read from `corvus/version.py` (no literal).

### 2.4 Shutdown order (today — identical in both entry points)

```text
serve.py:38-121  ==  app.py:402-484
  1. flash cleanup
  2. logs flush/close
  3. forwarder detach sink (set_frame_sink(None))
  4. mavlink.stop()
  5. ssh teardown
  6. store close
  7. http + tiles close, server_close()
  watchdog: 2s -> os._exit (must keep)
```

Autoconnect watcher stop slots **before** `mavlink.stop()` (see §5/§10).

### 2.5 QGC model (reference, not cloned)

- QGC General Settings → `AutoConnect` toggles: Pixhawk, SiK Radio, LibrePilot, PX4Flow, RTK GPS, NMEA device, UDP. Manual `Comm Links`: Serial/TCP/UDP/Bluetooth + `Auto Connect on Start` + high-latency option.
- `LinkManager` polls, waits for bootloader, broad-filter misconnect is a known issue class.
- UDP passive listen on 14550 = SITL zero-config; 14540 is onboard (not GCS).
- `MultiVehicleManager` per `(link, sysid)` — out of scope here.

### 2.6 Gaps (why this plan exists)

| # | Gap | Evidence |
|---|---|---|
| G1 | No auto loop | Zero `autoconnect` hits in repo today. |
| G2 | No per-type toggles | Only single `mavlink_connection` string in config. |
| G3 | Single bridge/store | Every `connect` tears down the previous transport. Hence "never steal" must be policy, not refcounting. |
| G4 | No bootloader-wait outside flash | Autoconnect must skip bootloader PIDs itself. |
| G5 | Single-string bind only | No multi-bind; `udp:0.0.0.0:14550` vs `udp:127.0.0.1:14550` skew (§2.1) matters for Gazebo vs loopback SITL. |

### 2.7 Hook-point index (where the plan attaches)

| Hook | File:line | Current behaviour | Plan attachment |
|---|---|---|---|
| `apply_startup_connection` | `corvus/server.py:4808-4833` | CLI-else-configured with `ValueError` fallback | Call `resolve_startup_connection()` (§4) |
| `create_server` | `corvus/server.py:4836-4923` (bridge at `4842-4844`, start at `4922`, wiring at `4911-4921`) | Construct → apply → start; forwarder/logs wiring at `4911-4921` | Pass-through + watcher attr (§5) |
| `start_backend` | `app.py:161-267` (bridge at `176/180`, start at `261-266`, wiring at `247-260`) | Mirror of server | Mirror edits (§4/§5) |
| `POST /connect` | `corvus/server.py:1936-1966` (`stop/set/start` at `1959-1962`) | Validate-before-teardown, then dial | Set `manual_override` (§7) |
| `POST /disconnect` | `corvus/server.py:1968-1994` | Idempotent stop | Clear-or-keep override per §14 decision; stop watcher retries (§7) |
| `GET /serial-ports` | `corvus/server.py:1571-1587` | Read-only enumerate | Reused by resolver/watcher; optional `auto` status endpoint alongside (§6) |
| `POST /api/config` | `corvus/server.py:1048-1237` (write at `1219`) | Never dials | Unchanged; only persists `autoconnect` block (§6) |
| Bridge `_run` | `corvus/mavlink_bridge.py:1051-1094` | Reconnect loop | **Not** modified; watcher lives outside bridge (§5) |
| Shutdown | `serve.py:38-121`, `app.py:402-484` | Ordered teardown + 2 s watchdog | Watcher join slots before `mavlink.stop()` (§10) |

---

## 3. Proposed Architecture

### 3.1 New module: `corvus/autoconnect.py` (stdlib only, pure + thin watcher)

New file, no existing-file logic change required to unit-test it. Three pure functions + one small watcher class + session-state dataclass.

```text
corvus/autoconnect.py
  resolve_startup_connection(cli_arg, configured, ports) -> StartupDecision
  pick_usb_serial(ports) -> UsbPick | None
  suggest_connection(snapshot, ports, session_state) -> Suggestion | None
  class AutoConnectWatcher(threading.Thread)   # thin policy loop; IO only via injected callables
  @dataclass class SessionState               # manual_override, last_suggestion_key, enabled snapshot
  @dataclass class StartupDecision            # connection_string, reason, kind
  @dataclass class Suggestion                 # connection_string, kind, device, reason
```

Function-signature sketches (names are proposals; backend owner finalises but keeps these semantics):

```python
# corvus/autoconnect.py (sketch — not applied in this PLAN-ONLY step)
from __future__ import annotations
from dataclasses import dataclass
from typing import Optional, Sequence

@dataclass(frozen=True)
class PortInfo:
    device: str          # e.g. "/dev/ttyACM0" or "COM7"
    description: str     # from list_serial_ports()
    hwid: str            # USB VID:PID / serial string
    kind: str            # "usb-direct" | "sik" | "other"  (via transport()/classify)

@dataclass(frozen=True)
class StartupDecision:
    connection_string: str   # e.g. "serial:/dev/ttyACM0:57600" | "udp:0.0.0.0:14550"
    reason: str              # "cli" | "usb-direct" | "sik" | "configured" | "udp-fallback" | "default-invalid-fallback"
    kind: str                # "cli" | "usb-direct" | "sik" | "serial-configured" | "udp-configured" | "udp-fallback"

@dataclass(frozen=True)
class Suggestion:
    connection_string: str
    kind: str                # "usb-direct" | "sik"
    device: str
    reason: str              # e.g. "new-usb-while-connected"

@dataclass
class SessionState:
    manual_override: bool = False
    last_suggestion_key: str | None = None   # e.g. connection_string last published; dedupes UI spam
    enabled: bool = True                     # snapshot of config autoconnect.enabled

def resolve_startup_connection(
    cli_arg: str | None,
    configured: str | None,
    ports: Sequence[PortInfo],
    *,
    usb_enabled: bool = True,
    sik_enabled: bool = True,
    udp_fallback_enabled: bool = True,
    default_connection: str = "udp:127.0.0.1:14550",  # injected; actual literal lives in server.py:4805
) -> StartupDecision: ...

def pick_usb_serial(
    ports: Sequence[PortInfo],
    *,
    usb_enabled: bool = True,
    sik_enabled: bool = True,
) -> tuple[str, int, str] | None:
    """Return (device, baud, kind) or None. Uses transport()/_parse_serial semantics; never opens ports."""
    ...

def suggest_connection(
    snapshot: dict,          # read-only view: link_status, last_heartbeat_age_s, connection_string
    ports: Sequence[PortInfo],
    session: SessionState,
) -> Suggestion | None: ...
```

### 3.2 `resolve_startup_connection(cli_arg, configured, ports) -> StartupDecision` (pure)

Precedence (must match §4 log reasons):

```text
1. if cli_arg is non-empty (after strip):
     validate via bridge validate (749-751/753-774 semantics)
     if valid   -> ("cli", cli_arg)
     if invalid -> fall through to step 2 BUT remember cli-invalid (final fallback logs "default-invalid-fallback")
2. usb/sik scan via pick_usb_serial(ports)   # only if enabled flags allow
     if usb-direct found -> ("usb-direct", serial:DEV:BAUD)
     elif sik found      -> ("sik",        serial:DEV:BAUD)
3. if configured is non-empty and valid -> ("configured", configured)
4. if udp_fallback_enabled -> ("udp-fallback", <single default — §14 decision>)
5. else -> ("default-invalid-fallback", DEFAULT_MAVLINK_CONNECTION)  # keeps server.py:4808-4833 ValueError fallback
```

Rules:

- Pure: no IO, no `list_serial_ports()` inside; caller injects `ports`. This keeps unit tests hermetic.
- Validation reuses bridge `_VALID_PREFIXES` / `validate` semantics (`mavlink_bridge.py:749-751`, `753-774`) — watcher module must **import** the validator, not duplicate the prefix list (single source for prefixes).
- Baud comes from `_parse_serial` default `57600` (`mavlink_bridge.py:869-885`) unless the port descriptor implies otherwise (see §3.3). No probing.
- `None`/empty handling: `cli_arg=""` ≡ absent; `configured=None` ≡ absent. `autoconnect.enabled=False` forces skip of step 2 (straight to configured/default) — specified in §6 coercion.
- Reason strings are stable log/API values (`cli`, `usb-direct`, `sik`, `configured`, `udp-fallback`, `default-invalid-fallback`, `no-candidate` for watcher no-op). Backend log line format proposed in §4.

### 3.3 `pick_usb_serial(ports) -> (device, baud, kind) | None` (pure, read-only)

Input `ports` is the already-enumerated list from `list_serial_ports()` (`mavlink_bridge.py:787-826`), mapped to `PortInfo`. The function **never opens** a port.

```text
pick_usb_serial(ports):
  normals = [p for p in ports if not _is_phantom_or_test(p)]   # _PHANTOM_TTY_RE 163-167 + pty/test skip (§11)
  if usb_enabled:
    usb_direct = [p for p in normals if transport(p)=="serial" and is_direct_usb(p) and not is_bootloader(p)]
    if usb_direct:
      dev = stable_sort_pick(usb_direct)     # deterministic: sorted by device name; first wins; two-FC case logged
      baud = parse_baud_or_default(dev, 57600)  # _parse_serial 869-885 semantics
      return (dev, baud, "usb-direct")
  if sik_enabled:
    sik = [p for p in normals if transport(p)=="serial" and is_sik(p) and not is_bootloader(p)]
    if sik:
      dev = stable_sort_pick(sik)
      baud = parse_baud_or_default(dev, 57600)  # SiK radios typically 57600; keep default unless descriptor says otherwise
      return (dev, baud, "sik")
  return None
```

Classification reuse (no duplication):

- `transport()` (`mavlink_bridge.py:887-920`) + `_classify_by_descriptor` (`922-956`) + aliases (`958-973`) + `is_direct_usb` (`975-977`) are the **only** classifiers. `autoconnect.py` imports them (or small wrappers injected by backend owner if import cycle demands — contract in §13).
- SiK detection = existing SiK descriptor branch inside `_classify_by_descriptor` (do not invent a new VID:PID table in the plan; mavlink owner confirms exact predicate during implementation and pins it in `tests/test_autoconnect.py`).
- Bootloader skip: `is_bootloader(p)` = descriptor/VID:PID indicates bootloader/flash mode (mavlink owner confirms predicate; default-deny: unknown → treated as **not** bootloader so normal FCs are never skipped; only positive bootloader match skips). See §10.
- Phantom/test skip: `_PHANTOM_TTY_RE` (`163-167`) + `pty`/`test` devices + bare `ttyS*` without USB hwid + Bluetooth (see edge table §11). Rationale: enumeration is read-only glob+pyserial; opening is forbidden.
- `cu.*` vs `tty.*` macOS alias: normalise via existing alias helper (`958-973`); prefer `tty.*` when both enumerate the same hardware (see §11).
- Baud: default `57600` (`869-885`); only override when the descriptor/alias path already implies a baud in the existing bridge code. No new baud table.

### 3.4 Session flag: `manual_override` (session-scoped, in-memory)

| Aspect | Specification |
|---|---|
| Type | `bool`, in-memory only (not persisted to config file). Lives on server object (e.g. `server.autoconnect_session: SessionState`) mirrored in `app.py` backend holder. |
| Set | Successful `POST /api/mavlink/connect` (`server.py:1936-1966`) sets `manual_override=True` and clears `link_suggestion` (see §7). Validate-before-teardown retained: flag is set **after** the new connection validates and `stop/set/start` succeeds. |
| Cleared | `POST /api/mavlink/disconnect` (`server.py:1968-1994`) — behaviour is a §14 decision: proposal A (recommended): clearing disconnect **keeps** `manual_override=True` (stays disarmed until next manual connect or restart) so "disconnect stops all retries" (AC7) holds literally; proposal B: disconnect re-arms (`manual_override=False`) so unplug-replug after a manual disconnect can auto-dial. User picks; default in this plan = A. Also cleared on process restart (fresh `SessionState`). `POST /api/config` never touches it. |
| Read | Watcher predicate + `GET /api/mavlink/auto` (§6/§7). |
| Why session, not config | Operator intent ("I chose this link") must not survive restart to fight fresh USB priority on next field boot, and must not be written into the shared config file. |

### 3.5 Background watcher design (daemon thread in server layer, NOT inside bridge)

Why not inside `MavlinkBridge._run`: the bridge owns a single transport string and its reconnect loop (`1051-1094`); policy (scan → decide → `stop/set/start` or suggest) belongs to the server layer where `store`, `config`, and `forwarder/logs` wiring already live (`server.py:4911-4921`, `app.py:247-260`). Keeps the bridge single-string + unit-testable; watcher is policy + easily disabled.

Thread sketch:

```python
# sketch — server layer owns this; autoconnect.py provides predicate + action helpers
class AutoConnectWatcher:
    def __init__(self, *, poll_s=2.5, bridge, store, list_ports_fn, session: SessionState, cfg_snapshot_fn):
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, name="autoconnect-watcher", daemon=True)

    def _loop(self):
        while not self._stop.wait(self.poll_s):   # _interruptible_sleep pattern (Event.wait, not time.sleep)
            try:
                if not self._should_act():        # read-only checks first (§3.5 predicate)
                    continue
                ports = self.list_ports_fn()      # list_serial_ports(), read-only, never opens
                pick = pick_usb_serial(ports)     # pure
                if pick is None:
                    continue                      # stay silent; optional debug log with reason no-candidate
                if self._bridge_reports_connected_or_fresh():
                    self._publish_suggestion(pick)  # mid-session plug -> suggest only
                else:
                    self._auto_dial(pick)           # disconnected -> stop/set/start (same as server.py:1959-1962)
            except Exception:
                log.exception("autoconnect watcher iteration failed (suppressed)")
                continue                          # watcher never kills server
```

`_should_act()` predicate (all must hold to even enumerate; enumeration itself is cheap but still gated):

```text
should_act =
    session.enabled
AND bridge._running is set                      # bridge exists and start() was called
AND NOT session.manual_override                 # manual wins session (§3.4)
AND store.link_status in {"disconnected", "reconnecting"}   # never when connected/degraded-with-fresh-heartbeat
AND no fresh vehicle heartbeat                  # last_heartbeat_age > stale threshold (see §10 table; default: warn/drop UDP 3/8s semantics inform threshold)
AND NOT shutdown_initiated                      # _stop_event respected
```

Action branch:

```text
if pick is None:            -> no-op (log at debug: "autoconnect: no-candidate")
elif connected_or_fresh:    -> publish suggestion via store.update(link_suggestion=...) + console publish; dedupe by last_suggestion_key
else (disconnected+stale):  -> bridge.stop(); bridge.set_connection(serial:DEV:BAUD) [validate first]; bridge.start()
                              # identical order to POST /connect server.py:1959-1962; validate-before-teardown retained
                              # single attempt per new device key; backoff via existing bridge _reconnect_delay on failure
```

Poll interval: default proposal `2.5 s` (rationale: faster than bridge 10 s heartbeat wait so plug-while-disconnected is picked up within one heartbeat window, slower than 1 Hz GCS heartbeat so CPU/battery cost is negligible; no port opens so cost ≈ one pyserial enumerate per tick). §14 asks user to confirm `2 vs 2.5 vs 3 s`.

CPU/battery note: one `list_serial_ports()` per tick (glob + pyserial enumerate, no open) + a few dict reads. No sockets created by the watcher itself. Measured cost is noise vs 1 Hz GCS heartbeat (`1488-1532`) and tlog per-cycle (`1565-1596`).

---

## 4. Startup Integration Diff-Plan

### 4.1 Exact edit locations (no code changed in this plan; locations for implementers)

| # | File | Lines | Change |
|---|---|---|---|
| S1 | `corvus/server.py` | `4805` (`DEFAULT_MAVLINK_CONNECTION`) | No literal change in this plan. Import resolver; pass default as injected value to `resolve_startup_connection` so the skew (`127.0.0.1` vs `0.0.0.0`) stays visible until §14 decision. |
| S2 | `corvus/server.py` | `4808-4833` (`apply_startup_connection`) | Call `resolve_startup_connection(cli_arg, configured, ports, usb_enabled, sik_enabled, udp_fallback_enabled, default_connection)`. `ports = list_serial_ports()` (read-only) gathered here (or injected for tests). Keep `try/except ValueError → DEFAULT` fallback unchanged. Add one info log naming winner + reason (format below). |
| S3 | `corvus/server.py` | `4836-4923` (`create_server`; bridge `4842-4844`; wiring `4911-4921`; start `4922`) | Pass-through: `apply_startup_connection` result flows into `bridge.set_connection`; **no watcher thread started before `mavlink.start()`**. Watcher creation (if enabled) goes after `4922` (see §5). |
| S4 | `corvus/app.py` | `161-267` (`start_backend`; bridge `176/180`; wiring `247-260`; start `261-266`) + arg parsing `494-519`, launch `546` | Mirror S2+S3 exactly. Same resolver, same precedence, same log line, same no-thread-before-start ordering. |
| S5 | `corvus/config.py` | `119` + forwarding block `331-374` (template) | Add `autoconnect` block read with genuine-bools coercion (§6). No dial here. |

### 4.2 Fallback chain (startup)

```text
cli_arg (sys.argv[2] / app.py:494-519)
  > usb-direct (pick_usb_serial, usb_enabled)
  > sik        (pick_usb_serial, sik_enabled)
  > configured (cfg.mavlink_connection)
  > DEFAULT_MAVLINK_CONNECTION (server.py:4805; literal unchanged in this plan)
  on ValueError at any set_connection -> DEFAULT (existing server.py:4808-4833 fallback, kept)
```

`POST /api/config` (`server.py:1048-1237`, write at `1219`) still only persists the string; it never dials. Startup resolver reads the persisted string on next boot.

### 4.3 Log-line contract (winner + reason)

Single info log at startup (both entry points, identical text):

```text
autoconnect: startup winner=<connection_string> reason=<cli|usb-direct|sik|configured|udp-fallback|default-invalid-fallback> [detail=<device/kind>]
```

Examples (illustrative strings, not literals to copy into code as constants beyond the reason enum):

```text
autoconnect: startup winner=serial:/dev/ttyACM0:57600 reason=usb-direct detail=Pixhawk
autoconnect: startup winner=udp:127.0.0.1:14550 reason=udp-fallback
autoconnect: startup winner=<configured> reason=configured
autoconnect: startup winner=<cli> reason=cli
```

Keep the existing `ValueError` fallback log alongside (do not remove).

---

## 5. Runtime Watcher Integration

### 5.1 Where the thread lives

| Layer | Creation point | Attribute | Wiring mirror |
|---|---|---|---|
| `serve.py` / `server.py` | `create_server`, **after** `mavlink.start()` (`server.py:4922`), next to forwarder/logs wiring (`server.py:4911-4921`) | `server.autoconnect` (watcher) + `server.autoconnect_session` (`SessionState`) | Follows existing `server.forwarder` / `server.logs` attach pattern. |
| `app.py` | `start_backend`, **after** `mavlink.start()` (`app.py:261-266`), next to wiring (`app.py:247-260`) | Same two attributes on backend holder | Mirror exactly; no divergence. |

Ordering invariant: **no watcher thread before `mavlink.start()`**. Startup resolver (§4) decides the first string synchronously; the watcher only handles *runtime* plug events afterwards.

### 5.2 Start/stop wiring

```text
start (after mavlink.start()):
  session = SessionState(enabled=cfg.autoconnect.enabled, manual_override=False)
  watcher = AutoConnectWatcher(poll_s=2.5, bridge=mavlink, store=store,
                               list_ports_fn=list_serial_ports,
                               session=session, cfg_snapshot_fn=lambda: cfg.autoconnect)
  watcher.start(); server.autoconnect = watcher

stop (shutdown path, BEFORE mavlink.stop()):
  # serve.py:38-121 slot: after forwarder-detach, before mavlink.stop()
  # app.py:402-484 same slot
  if getattr(server, "autoconnect", None):
      server.autoconnect.stop()        # sets _stop_event
      server.autoconnect.join(timeout=2.0)
  mavlink.stop()                       # existing step, unchanged
  ... rest of ordered teardown unchanged (ssh -> store -> http+tiles -> server_close; 2s os._exit watchdog kept)
```

- `_stop_event` respected every loop iteration via `Event.wait(poll_s)` (the `_interruptible_sleep` pattern) so shutdown join is prompt (< poll interval).
- `join(timeout=2.0)` matches the watchdog budget; watcher `daemon=True` so a stuck enumerate cannot block `os._exit`.
- Watcher exceptions are caught per-iteration and logged; the watcher never raises out of `_loop`.

### 5.3 Why NOT inside `MavlinkBridge._run`

| Reason | Detail |
|---|---|
| Separation of concerns | Bridge owns one transport + reconnect backoff (`1051-1094`, `1152-1161`); watcher owns cross-transport policy. Mixing them would tangle `_connect`/`_wait_vehicle_heartbeat` with enumeration. |
| Testability | Pure resolver + predicate are unit-testable without sockets/threads; bridge tests keep pinning single-string behaviour. |
| Shutdown clarity | Server layer already owns ordered teardown (`serve.py:38-121`, `app.py:402-484`); adding a second looping concern inside `_run` would create two stop paths. |
| No bridge API churn | Bridge keeps `stop/set_connection/start`, `validate`, `transport`, `_parse_serial`, `flock`, heartbeat/interval/version/home/tlog semantics untouched. Watcher reuses them. |

---

## 6. Config Schema (Minimal, Optional, Backward-Compatible)

Template: existing forwarding-style block in `corvus/config.py:331-374`. Follow that pattern exactly (genuine-bools-only coercion, `None` means defaults, unknown keys ignored, old files load unaffected).

Proposed schema:

```python
# corvus/config.py — sketch (defaults; no code changed in this plan)
DEFAULT_AUTOCONNECT = {
    "enabled": True,        # master switch; False disables both startup scan and watcher
    "usb": True,            # USB direct Pixhawk priority
    "sik": True,            # SiK radio second priority
    "udp_fallback": True,   # UDP 14550 fallback / SITL when no serial candidate
}
```

Coercion rules (mirror forwarding `331-374`):

- Genuine `bool` only: `True/False` accepted; truthy strings (`"true"`, `1`) are **rejected → default** (log a warning naming the key). Rationale: config booleans must not silently flip on a typo.
- `None` (key absent) → default (`True`). Old config files without `autoconnect` load exactly as today.
- Unknown keys inside `autoconnect` ignored (forward-compat).
- `POST /api/config` (`server.py:1048-1237`) persists the block like other blocks; it **must NOT dial** (keep `1219` write-only behaviour for `mavlink_connection`; same for `autoconnect` — persist + apply to session `enabled` snapshot only, no `stop/set/start`).

Session vs persisted: `enabled/usb/sik/udp_fallback` are persisted; `manual_override` is never persisted (§3.4).

### Status visibility — pick ONE (recommendation: option B)

| Option | Shape | Pros | Cons |
|---|---|---|---|
| A. New `GET /api/mavlink/auto` → `{enabled, reason, suggestion}` | New endpoint | Explicit; self-documenting; easy test | New surface to maintain; yet another poll endpoint (frontend should use SSE anyway) |
| B. **(Recommended)** Reuse `GET /api/mavlink/serial-ports` + store fields (`link_suggestion`, `link_auto`) pushed over existing SSE | No new endpoint | Zero new routes; frontend already subscribes (`link.js:651-653` via `telemetry.js`); suggestion rides the same channel as `link_status` | Discovery slightly less obvious → document in API reference + appendix |
| C. Extend `GET /api/version` | No | Wrong layer (version ≠ link policy) | Rejected |

Recommendation **B**: watcher publishes `link_suggestion`/`link_auto` into the Vehicle State Store; existing SSE pushes them; `GET /serial-ports` remains the read-only enumerate used by Settings UI. If reviewers insist on an explicit status route, add A as a thin read-only view over the same store keys (no new state).

---

## 7. API + State-Store Contract

### 7.1 New store keys

| Key | Type | Set by | Cleared by | SSE visible | Notes |
|---|---|---|---|---|---|
| `link_suggestion` | `null \| {connection_string: str, kind: "usb-direct"\|"sik", device: str, reason: str, ts: float}` | Watcher, only when connected-or-fresh + new device + deduped (`last_suggestion_key`) | Successful `POST /connect` (any string); watcher when device disappears; `POST /disconnect` (decided with §14 re-arm choice; default: cleared) | Yes (same channel as `link_status`) | Display-only; frontend `Connect` button reuses `doConnect` with this string. |
| `link_auto` | `{enabled: bool, reason: str, winner: str\|null}` | Startup resolver + watcher (last decision) | Overwritten each decision; never `null` after boot | Yes | Debug/About visibility; `reason` enum from §4.3. |

Naming is proposed; backend owner finalises but keeps `link_` prefix (consistent with `link_status`) and the types above. No other agent renames these without updating §8 + tests.

### 7.2 Endpoint deltas

| Endpoint | Change | Status codes |
|---|---|---|
| `POST /api/mavlink/connect` (`server.py:1936-1966`) | After successful `stop → set_connection → start` (`1959-1962`): set `session.manual_override=True`; clear `link_suggestion` (`last_suggestion_key=None`); update `link_auto.reason="manual"`. Validate-before-teardown order unchanged. | `400` invalid string, `503` bind/start failure, `409` conflict semantics **unchanged**. |
| `POST /api/mavlink/disconnect` (`server.py:1968-1994`) | Idempotent stop (unchanged) + clear `link_suggestion`; `manual_override` handling per §14 decision (default A: keep `True` = stay disarmed). Watcher performs no re-dial after this until re-armed. | Unchanged codes. |
| `GET /api/mavlink/serial-ports` (`server.py:1571-1587`) | No change (read-only). Watcher calls the same underlying `list_serial_ports()`. | Unchanged. |
| `POST /api/config` (`server.py:1048-1237`) | Persist `autoconnect` block; refresh `session.enabled` snapshot; **never dial**. | Unchanged codes. |
| (`GET /api/mavlink/auto` — only if option A chosen) | Read-only `{enabled, reason, suggestion}` from store+session. No side effects. | `200` only. |

### 7.3 SSE

- `link_suggestion` and `link_auto` ride the existing SSE stream the frontend already subscribes to (`link.js:651-653` via `telemetry.js`). No new socket, no polling.
- Suggestion dedupe: watcher publishes only when `connection_string != last_suggestion_key`; disappearance clears to `null` (so a unplugged device does not linger).

---

## 8. Frontend Plan (`src/js/link.js` + `telemetry.js`)

Constraints: no auto-`POST` from the browser without operator action, except the backend-driven disconnected-auto case (which the backend performs itself; the frontend only displays). Refresh cadence unchanged.

| # | Change | Reuse | Lines |
|---|---|---|---|
| F1 | **AUTO badge** in status area: when `link_auto.enabled && link_auto.reason in (usb-direct, sik, udp-fallback)` and no manual override, show `AUTO · <reason>` chip next to existing `CONNECTING/RECONNECTING/CONNECTED` text. | Existing status render `link.js:505-575` | `505-575` |
| F2 | **Suggestion row**: when `link_suggestion != null` and currently connected, render one row `New device: <device> (<kind>) [Connect] [Dismiss]`. `Connect` calls existing `doConnect(suggestion.connection_string)`; `Dismiss` clears row locally (backend re-publishes only on new key). | Reuse `renderRecent`/`applyConnection` (`258-278`, `604-605`) + `doConnect` (`417-437` via `telemetry.js:235-237`) | `258-278`, `417-437`, `604-605` |
| F3 | **Scanning hint**: when disconnected/reconnecting and `link_auto.enabled`, status text becomes `AUTO scanning… <existing CONNECTING/RECONNECTING>` (AC3). No new timer. | `505-575` | `505-575` |
| F4 | `telemetry.js` subscribe extension: include `link_suggestion` + `link_auto` in the parsed SSE payload and forward to `link.js` handler. No new `EventSource`. | Existing subscribe path used at `link.js:651-653` | `telemetry.js` + `651-653` |
| F5 | `refreshPorts` cadence unchanged (`638`, `644-648`); `init` (`577-654`) still restores fields only, no auto dial. `connectSerial` (`445-461`) + `buildConnectionString` (`107-110`) unchanged; suggestion `Connect` builds the identical `serial:dev:baud` string. Recent list (`170-187`) appends suggestion-driven connects as usual. | — | `107-110`, `170-187`, `445-461`, `577-654`, `638`, `644-648`, `651-653` |
| F6 | Presets (`52-86`) unchanged. No preset auto-clicked. | — | `52-86` |

Pseudocode (frontend, illustrative):

```text
onSSE(msg):
  renderStatus(msg.link_status)                       # 505-575 + F1/F3 AUTO chip
  if msg.link_suggestion and isConnected(msg):
      showSuggestionRow(msg.link_suggestion)          # F2; Connect -> doConnect(s.connection_string)
  else hideSuggestionRow()
```

---

## 9. PX4 SITL Compatibility

| Topic | Specification (unchanged unless noted) |
|---|---|
| Gazebo 14550 bind | Candidates are the single configured/DEFAULT UDP string. `0.0.0.0` (listen-all, `config.py:119` fresh default + bridge `__init__` `639-643`) vs `127.0.0.1` (loopback-only, `server.py:4805` + `POST /connect` default `1936-1966`) skew is **pinned, not unified, in this plan**. §14 asks the user to pick one. Recommendation to present: standardise on `udp:0.0.0.0:14550` for SITL fallback (accepts both loopback Gazebo and bridged sim) and keep `127.0.0.1` working as explicit CLI/configured value; rationale: `0.0.0.0` is a superset bind for local sim while `127.0.0.1` silently drops off-loopback simulators. Until decided, resolver injects the current `DEFAULT` verbatim and tests pin both literals. |
| 14540 onboard | **Never auto-picked.** Onboard range `14540-14549` warning at `mavlink_bridge.py:1169-1197` retained. Preset at `link.js:52-86` stays manual-only. |
| `udpout:` router-server | Manual-only (`link.js:52-86` preset + `POST /connect`). Watcher never synthesises `udpout:`; fallback is always the single `udp:` listen string. User-typed `udpout:` is `configured` kind and respected at startup (see §11 row). |
| TCP 5760 | ArduPilot-only preset (`link.js:52-86`); never auto-picked for PX4. |
| Version / intervals / home | Unchanged: version triple + 4 s retry (`1904-1996`, `v1.18-alpha1` `UNSUPPORTED`); interval deferral BUG1 (`1773-1801`) + fallback (`1803-1862`, cap 3); home nudge msg 242 (`1869-1902`). SITL 1.18 path exercises these as today. |
| Multi-instance | Second-instance lock (`CORVUS_ALLOW_MULTI` / instance-lock behaviour) unchanged; `EADDRINUSE` rewrite (`1248-1259`) retained — autoconnect fallback that hits `EADDRINUSE` surfaces the rewritten message and does not loop hot (bridge backoff `1152-1161` applies). |
| Forwarder 14551 | Coexistence note: forwarder (`set_frame_sink` `1628-1630`, `inject_raw` `1632-1655`) attaches after `mavlink.start()` (`4911-4921` / `247-260`); watcher auto-dial reuses `stop/set/start` which must keep the forwarder sink attached across re-dial (same guarantee `POST /connect` already provides — verify, do not redesign). QGC-via-forwarder coexistence smoke in §12. |
| Heartbeat/timeout asymmetry | Serial warn/drop `6/15` vs UDP `3/8` (`228-231`) vs legacy `_connection_ready` `10/5` (`2025-2032`) — watcher staleness threshold derives from the A3 timeouts, not the legacy pair (see §10 table). |

---

## 10. Safety + Lifecycle

### 10.1 Preserved mechanisms (no change)

- `flock` exclusive claim (`mavlink_bridge.py:1096-1150`, `nt` early-return) — watcher dial path goes through the same `set_connection`/`_connect`, so locking is inherited. No direct serial open in `autoconnect.py` (import-time or runtime).
- GCS identity `254` / `MISSIONPLANNER` (`262-263`) + `_is_vehicle_heartbeat` (`358-398`) + `_dispatch` guards (`2107-2322`) + `_pending_acks`/`_PriorityLock` (`677`) — untouched.
- Validate-before-teardown (`server.py:1959-1962` order) — watcher auto-dial reuses the identical order.
- GCS 1 Hz heartbeat (`1488-1532`), tlog per-cycle (`1565-1596`) — untouched.
- Shutdown order + 2 s `os._exit` watchdog (`serve.py:38-121`, `app.py:402-484`) — watcher join slots before `mavlink.stop()` (§5.2).

### 10.2 Bootloader skip rule

```text
if is_bootloader(port): skip with reason=bootloader-skip (no flock, no open, no suggestion)
```

- Detection predicate owned by mavlink agent (descriptor/VID:PID for bootloader/flash mode; exact set confirmed at implementation, pinned in tests).
- Closed-world default: only positive bootloader matches skip; unknown descriptors never skip (a real FC is never starved by an over-broad rule).
- Flash path outside this plan is unaffected; autoconnect never claims a port the flasher is about to use because it never claims bootloader ports at all.

### 10.3 Degraded vs disconnected decision table (when the watcher may act)

Staleness reference: A3 warn/drop timeouts — serial `6/15`, UDP `3/8` (`228-231`). Watcher uses a single `stale_after_s` threshold (proposal: `8 s` — covers UDP drop boundary; serial still in warn band, so serial links are given grace; mavlink owner confirms final number; test pins it).

| Store `link_status` | Heartbeat age | `manual_override` | Watcher action |
|---|---|---|---|
| `connected` | fresh (≤ stale) | either | **No-op** (never steal). Optionally publish suggestion if new device (deduped). |
| `connected` | stale (> stale) | either | **No-op dial** (link layer owns reconnect via `_reconnect_delay`); suggestion allowed. Rationale: a stale-but-`connected` link may be mid-backoff; tearing it down races `_run`. |
| `degraded` | fresh | either | **No-op dial**; suggestion allowed. Degraded + fresh = usable link, do not steal. |
| `degraded` | stale | `True` | **No-op** (manual wins). Suggestion suppressed while overridden (avoid nagging the operator's chosen link). |
| `degraded` | stale | `False` | **No-op dial** by default (conservative); suggestion allowed. Rationale: degraded implies transport alive; auto-teardown is NG-steal-adjacent. Revisit only with user approval. |
| `disconnected` / `reconnecting` | stale or none | `True` | **No-op** (manual wins session). |
| `disconnected` / `reconnecting` | stale or none | `False` | **May auto-dial** (the single allowed dial case) OR publish suggestion if policy is suggest-only first boot (§14). |
| any | any | any, shutdown initiated | **No-op**; loop exits via `_stop_event`. |

### 10.4 Resource budget

- One daemon thread total (`autoconnect-watcher`). No new sockets (enumeration is glob+pyserial describe; dial reuses bridge transport). No new file handles (no tlog change). Join `2 s` inside watchdog budget.
- Poll `2.5 s` default → ~24 enumerates/min, each non-opening. Negligible vs 1 Hz heartbeat + map/tile IO.

---

## 11. Edge Cases Table (16 rows)

| # | Case | Input / state | Expected | Notes / refs |
|---|---|---|---|---|
| E1 | Two USB FCs | `ttyACM0` + `ttyACM1`, both `usb-direct` | Deterministic pick: sorted-first wins; log names loser (`startup winner=... reason=usb-direct detail=2-candidates,picked=<dev>`); suggestion for the other suppressed while connected (avoid nag) | Stable sort in `pick_usb_serial` (§3.3) |
| E2 | USB + SITL | USB FC + Gazebo on 14550 | USB wins (`usb-direct` > `udp-fallback`); SITL ignored, no bind | Priority §3.2 |
| E3 | SiK + USB | Both enumerated | USB wins; SiK only if USB absent/disabled | Priority §3.2 |
| E4 | Stale 14540 config | `cfg.mavlink_connection=udp:...:14540`, no USB | Startup: no serial candidate → `configured` (14540) honoured as today (no silent rewrite); onboard warning (`1169-1197`) fires as today | Never auto-pick 14540, but never override explicit config |
| E5 | `EADDRINUSE` with QGC running | Fallback/CLI UDP bind collides | Rewritten message (`1248-1259`); bridge backoff (`1152-1161`); watcher does not hot-loop (single dial per key, then backoff owns retry) | Forwarder-14551 coexistence §9 |
| E6 | macOS double-bind steal | Two Corvus instances / QGC + Corvus on same UDP | Same as E5; `CORVUS_ALLOW_MULTI` row E15 applies for second instance | Instance lock unchanged |
| E7 | Bootloader enumerating | e.g. `ttyACM0` in DFU/bootloader PID at boot | Skipped (`bootloader-skip`); falls to SiK/configured/UDP; flash path unaffected | §10.2 |
| E8 | `pty` / test devices | `/dev/pts/*`, test fixtures | Filtered, never picked, never suggested | §3.3 phantom/test skip |
| E9 | Phantom `ttyS*` / Bluetooth | Bare `ttyS0`, `bluetooth`/`rfcomm` descriptors | Filtered by `_PHANTOM_TTY_RE` (`163-167`); reason `no-candidate (phantoms filtered)` | Never opened |
| E10 | COM alias (Windows) | `COM7` / `\\.\COM10` spellings | Normalised via alias helpers (`958-973`); `serial:COM7:57600` canonical form | No new table |
| E11 | `cu.*` / `tty.*` alias (macOS) | Both `cu.usbmodemX` + `tty.usbmodemX` | Prefer `tty.*`; single pick; no double suggestion | §3.3 |
| E12 | Immediate-None heartbeat bail | `_wait_vehicle_heartbeat` gets immediate `None` (`1371-1421`) | Bail fast as today; watcher does not interpret `None` as "device gone" — only store/heartbeat-age predicate gates dials | No bridge change |
| E13 | Legacy `_connection_ready` (10/5, `2025-2032`) vs A3 timeouts (serial 6/15, UDP 3/8, `228-231`) | Stale link near boundary | Watcher staleness derives from A3 pair only; legacy pair ignored (flagged for later cleanup, not this plan) | §10.3 threshold |
| E14 | `sysid 0` / `RADIO_STATUS`-only link | Non-vehicle heartbeat (`358-398` rejects) or pre-guard `RADIO_STATUS` only (`2107-2322`) | Not treated as vehicle-connected; watcher may dial when otherwise disconnected+stale (correct: radio noise ≠ vehicle link) | Guards unchanged |
| E15 | User typed `udpout:` router / `CORVUS_ALLOW_MULTI` second instance | `configured=udpout:router:14550`; second process | Startup honours `configured` verbatim (`reason=configured`); watcher never synthesises `udpout:`; second instance follows existing lock semantics | Router-server §9 |
| E16 | Forwarder enabled + autoconnect same boot | Forwarder sink attached (`4911-4921`/`247-260`) then USB picked at startup or auto-dial at runtime | Sink stays attached across `stop/set/start` (same guarantee as manual `POST /connect`); QGC-via-forwarder smoke in §12 verifies | `set_frame_sink` `1628-1630` |

---

## 12. Test Plan

### 12.1 New suite: `tests/test_autoconnect.py` (backend owner; review audits)

Pure-function tests (no sockets, no threads, no serial opens):

| # | Case | Asserts |
|---|---|---|
| T1 | Resolution order | `usb > sik > configured > udp-fallback` across port fixtures. |
| T2 | CLI wins | Non-empty valid `cli_arg` beats present USB + valid configured; reason `cli`. |
| T3 | Garbage fallback | Invalid CLI + invalid configured → `DEFAULT` with reason `default-invalid-fallback`; `ValueError` path preserved (`server.py:4808-4833` semantics). |
| T4 | USB beats SiK beats UDP | Fixtures: usb-only → `usb-direct`; sik-only → `sik`; neither → `udp-fallback`; both → `usb-direct`. |
| T5 | Never-steal matrix | Predicate false for `connected`/`degraded+fresh`/`manual_override`/`shutdown`; true only for `disconnected|reconnecting + stale + !override + running + enabled` (§10.3). |
| T6 | Bootloader skip | Bootloader-port fixture → skipped; reason `bootloader-skip`; next priority wins. |
| T7 | Suggestion published | Connected + new USB → `suggest_connection()` returns `Suggestion`; dedupe key stable; disappearance → `None`. |
| T8 | Manual-override session | `manual_override=True` suppresses both dial and suggestion-nag; cleared only per §14 decision. |
| T9 | Watcher no-op when connected | Connected+fresh + new device → no `stop/set/start` calls (mock bridge records calls); suggestion only. |
| T10 | Default-skew pinned | `server.py:4805` (`127.0.0.1`), `config.py:119` (`0.0.0.0`), `mavlink_bridge.py:639-643` (`0.0.0.0`), `POST /connect` default (`1936-1966`, `127.0.0.1`) asserted as documentation until §14 unification lands. Test fails on silent literal drift. |
| T11 | Phantom/test filtering | `ttyS*`, `pty`, Bluetooth fixtures → `pick_usb_serial() is None`. |
| T12 | `cu`/`tty` + COM alias normalisation | Alias fixtures → single canonical `serial:<dev>:<baud>` pick. |
| T13 | `None`/empty handling | `cli=""`, `configured=None`, `ports=[]` → `udp-fallback` (or disabled→`DEFAULT`); `enabled=False` skips scan. |
| T14 | Genuine-bools coercion | `"true"`/`1`/`None`/unknown keys → defaults per §6; old config without block loads. |
| T15 | Reason-enum stability | All `reason` strings asserted against the §4.3 enum (log/API contract). |

### 12.2 Existing suites to keep green (no regressions)

- Router / forwarder / stream-fallback / regression-link / serial / windows / version / linkquality suites (names per repo layout; full `python3 -m pytest -q` gate per AGENTS.md Definition of Done).
- Frontend assertions: `for f in tests/*.js; do node "$f"; done` (per repo `Commands`).

### 12.3 Live SITL smoke (PX4 v1.18 Gazebo; 1.16/1.17 spot-check per PX4 target)

| # | Step | Expect |
|---|---|---|
| L1 | SITL-only boot (no USB) | Auto-connected via `udp-fallback`; telemetry flows; version triple resolves (not `UNSUPPORTED`); intervals cap-3 path as today. |
| L2 | USB-priority mocked (no HW) | Virtual serial FC fixture or config-forced pick → `usb-direct` wins over live SITL (documents priority without HW). Real-HW check when available. |
| L3 | Plug-while-disconnected | Kill SITL → `reconnecting` → restart SITL (or plug USB) → single auto-dial → `connected`. |
| L4 | Plug-while-connected suggestion | While SITL-connected, enumerate second device → suggestion row, no teardown. |
| L5 | QGC via forwarder coexistence | Forwarder to 14551 + QGC attached → autoconnect boot/dial does not detach sink; both GCSs receive frames. |
| L6 | Kill-SITL reconnect | Bridge backoff (`1152-1161`) + UI `RECONNECTING` (`505-575`) cycle intact; no watcher hot-loop. |

---

## 13. Rollout Steps (Ordered, File-by-File, With Owners)

Interface contracts between owners are normative: producer/consumer names must match exactly or the handoff is rejected by review.

| Step | Owner | File(s) | Task | Contract produced |
|---|---|---|---|---|
| R1 | `backend` | `corvus/autoconnect.py` (new) | Pure `resolve_startup_connection` + `pick_usb_serial` + `suggest_connection` + `SessionState`/`StartupDecision`/`Suggestion` + reason enum; stdlib only; no IO. | `resolve_startup_connection(cli_arg, configured, ports, *, usb_enabled, sik_enabled, udp_fallback_enabled, default_connection) -> StartupDecision`; `pick_usb_serial(ports, *, usb_enabled, sik_enabled) -> (device, baud, kind) \| None`; `suggest_connection(snapshot, ports, session) -> Suggestion \| None` |
| R2 | `mavlink` | `corvus/mavlink_bridge.py` (read-only confirm; minimal export if needed) | Confirm `transport` (`887-920`) / `_classify_by_descriptor` (`922-956`) / aliases (`958-973`) / `is_direct_usb` (`975-977`) / `_parse_serial` (`869-885`) / `validate` (`753-774`) / `list_serial_ports` (`787-826`) / phantom RE (`163-167`) reuse + bootloader predicate; no `_run` change. | Bootloader predicate name + SiK predicate confirmation; pinned in `tests/test_autoconnect.py` T6. |
| R3 | `backend` | `corvus/config.py` (`119`, `331-374` template) + `corvus/server.py` (`4805`, `4808-4833`, `4836-4923`) | `autoconnect` block (§6) + resolver call in `apply_startup_connection` + winner log (§4.3) + watcher create/stop wiring (`server.autoconnect`, join before `mavlink.stop()`). `POST /connect` (`1936-1966`) sets override; `POST /disconnect` (`1968-1994`) clears per §14; `link_suggestion`/`link_auto` store writes + SSE. | Store keys `link_suggestion`, `link_auto` (types §7.1); `server.autoconnect` / `server.autoconnect_session` attrs; log-line format §4.3. |
| R4 | `backend` + `gui` (app wrapper) | `corvus/app.py` (`161-267`, `247-260`, `261-266`, `402-484`, `494-519`, `546`) + `serve.py` (`38-121`, `125-156`, `171`) | Mirror R3 exactly in `start_backend`/shutdown/arg-parse. | Behavioural parity `serve.py` ≡ `app.py` (startup chain, watcher lifecycle, shutdown order). |
| R5 | `gui` (frontend) | `src/js/link.js` (`52-86`, `107-110`, `170-187`, `258-278`, `417-437`, `445-461`, `505-575`, `577-654`, `604-605`, `638`, `644-648`, `651-653`) + `src/js/telemetry.js` (`235-237` + subscribe) | AUTO badge (F1), suggestion row (F2), scanning hint (F3), subscribe extension (F4); cadence/init/presets untouched (F5/F6). | SSE fields `link_suggestion`, `link_auto` consumed; `doConnect` reuse (no new POST path). |
| R6 | `review` | All touched files + `tests/test_autoconnect.py` | Safety/reliability audit: shutdown path, `flock`, validate-before-teardown, never-steal matrix, no new sockets/threads beyond one watcher, `pytest -q` + frontend `node` assertions green. Test authoring for fixed bugs (regression tests). | Green gate: `python3 -m pytest -q` + `for f in tests/*.js; do node "$f"; done`. |
| R7 | `doc` | User manual + API reference + offline install guide (no logic change) | Document: PX4 v1.16/1.17/1.18 target + v1.12–v1.15 best-effort fallback; `autoconnect` block defaults; `link_suggestion`/`link_auto` semantics; AUTO/suggestion UI; single-source version policy (`VERSION` → `corvus.version` → `GET /api/version`); shutdown guarantees. Version numbers in docs generated from `VERSION` at build time, never hand-typed. | Manual + API reference updated. |
| R8 | `readme` | `README.md` (badge marker `corvus:version-badge` preserved) | If user-visible: one-line AUTO-connect note; no version literal hand-edit (hook owns badge number). | README diff without version literal. |
| R9 | `build` + `devops` | `build-appimage.sh`, `build-macos-app.sh`, `build-windows.ps1` via `build.sh`; `.gitlab-ci.yml` + `.github/workflows/build.yml` | Bundle new `corvus/autoconnect.py` (layout contract `VERSION`/`corvus/`/`src/`/`assets/` siblings); verify no version literal; confirm both pipelines agree (jobs, apt set, build distro per AGENTS.md新疆 note — check, don't assume). Produce artifacts after merge. | Reproducible self-contained artifacts; CI green on both pipelines. |

### Definition-of-Done mapping (per AGENTS.md)

| Gate | Check |
|---|---|
| Scope | Only `corvus/autoconnect.py` (new) + `corvus/server.py` + `corvus/app.py` + `corvus/config.py` + `src/js/link.js` + `src/js/telemetry.js` + `tests/test_autoconnect.py` + docs/README touched. Anything else → `NEXT` handoff, not edit. |
| Version | No new version literal; `VERSION` untouched by hand (hook owns bump); badge marker preserved. |
| PX4 | Priority/resolver/suggestion paths checked against v1.16, v1.17, v1.18 SITL smoke (L1–L6); graceful fallback when params absent (unchanged code paths). |
| Lifecycle | Watcher join before `mavlink.stop()` both entry points; `_stop_event` respected; `flock`/sockets/handles audited; 2 s watchdog kept. |
| Tests | `python3 -m pytest -q` passes; T1–T15 new; fixed bugs get regression tests. |
| Packaging | New file bundled on all platforms; layout/`LICENSE.md`/signal-transparency/no-literal invariants hold; both CIs pass without drift. |
| Docs | User-visible changes routed to `readme` + `doc`. |

Version-policy note: no agent hand-edits `VERSION`. The `.githooks/pre-commit` hook owns the CalVer `YYYY.MM.PP` bump + README badge rewrite. Release = commit (hook bumps) → orchestrator tags `v<VERSION>` → devops/build produce artifacts.

---

## 14. Open Questions / Decisions for User

| # | Question | Options | Recommendation + rationale |
|---|---|---|---|
| Q1 | Single UDP default: `0.0.0.0:14550` vs `127.0.0.1:14550` | A. `0.0.0.0` (listen-all) / B. `127.0.0.1` (loopback-only) / C. keep skewed literals | **A** for the SITL fallback: superset bind accepts loopback Gazebo and bridged sim; `127.0.0.1` stays valid as explicit CLI/configured value. Unify only the *fallback* literal; pin the rest in T10 until cutover. |
| Q2 | Poll interval | `2 s` / `2.5 s` / `3 s` | **2.5 s**: inside one 10 s heartbeat window (`1371-1421`), well below 1 Hz heartbeat cost relevance, prompt-enough plug response (~1–3 s perceived). |
| Q3 | First-boot behaviour when never connected | A. auto-dial immediately on candidate / B. suggest-only until operator confirms once | **A** (auto-dial): matches "fresh boot with USB connects without click" (AC1) and QGC expectation; B would break G1/G2. Suggest-only applies only mid-session (scope lock). |
| Q4 | `POST /disconnect` re-arm | A. stay disarmed (`manual_override` kept) / B. re-arm auto | **A** (default in this plan): "disconnect stops all retries" (AC7) reads literally; operator re-connects manually or restarts. B is one-line change if field workflow prefers re-arm. |
| Q5 | Settings UI toggle now or backend-only first | A. backend-only + config file / B. + Settings UI switch now | **A first** (backend + `GET` visibility via store/SSE), UI switch as fast follow: keeps this change minimal-smart-default per scope lock; F1–F4 display already covers observability. If user wants the toggle now, it binds to `autoconnect.enabled` via existing `POST /api/config` (no dial). |

---

## 15. Appendix

### 15.1 Exact file:line index of all touch points

| File | Lines | Symbol / behaviour |
|---|---|---|
| `serve.py` | `38-121` | Ordered shutdown (flash→logs→forwarder-detach→mavlink→ssh→store→http+tiles→server_close) + 2 s `os._exit` watchdog |
| `serve.py` | `125-156`, `171` | Arg parse (`sys.argv[2]` precedence) + `create_server` call |
| `corvus/server.py` | `4805` | `DEFAULT_MAVLINK_CONNECTION = udp:127.0.0.1:14550` |
| `corvus/server.py` | `4808-4833` | `apply_startup_connection` (+ `ValueError` fallback) |
| `corvus/server.py` | `4836-4923` | `create_server` (bridge `4842-4844`, wiring `4911-4921`, `mavlink.start()` `4922`) |
| `corvus/server.py` | `1936-1966` | `POST /api/mavlink/connect` (default `udp:127.0.0.1:14550`; `stop/set/start` `1959-1962`) |
| `corvus/server.py` | `1968-1994` | `POST /api/mavlink/disconnect` (idempotent stop) |
| `corvus/server.py` | `1571-1587` | `GET /api/mavlink/serial-ports` |
| `corvus/server.py` | `1048-1237` (`1219`) | `POST /api/config` (`mavlink_connection` write-only, never dials) |
| `corvus/app.py` | `161-267` (`176`, `180`, `261-266`) | `start_backend` (bridge construct → apply → start) |
| `corvus/app.py` | `247-260` | Forwarder/logs wiring (mirror of `server.py:4911-4921`) |
| `corvus/app.py` | `402-484` | Ordered shutdown (mirror of `serve.py:38-121`) |
| `corvus/app.py` | `494-519`, `546` | Arg parse + `start_backend` launch |
| `corvus/config.py` | `119` | Fresh default `udp:0.0.0.0:14550` |
| `corvus/config.py` | `331-374` | Forwarding-pattern block (bool-coercion template for `autoconnect`) |
| `corvus/mavlink_bridge.py` | `163-167` | `_PHANTOM_TTY_RE` |
| `corvus/mavlink_bridge.py` | `2025-2032` vs `228-231` | Legacy `_connection_ready` 10/5 vs A3 warn/drop serial 6/15, UDP 3/8 |
| `corvus/mavlink_bridge.py` | `262-263` | GCS id 254 / `MISSIONPLANNER` |
| `corvus/mavlink_bridge.py` | `358-398` | `_is_vehicle_heartbeat` |
| `corvus/mavlink_bridge.py` | `639-643` | `__init__` default `udp:0.0.0.0:14550` |
| `corvus/mavlink_bridge.py` | `677` | `_pending_acks` + `_operation_lock` (`_PriorityLock`) |
| `corvus/mavlink_bridge.py` | `749-751`, `753-774` | `_VALID_PREFIXES` + `validate_connection_string` |
| `corvus/mavlink_bridge.py` | `787-826` | `list_serial_ports` (pyserial + `/dev` glob, never opens) |
| `corvus/mavlink_bridge.py` | `869-885` | `_parse_serial` (default 57600) |
| `corvus/mavlink_bridge.py` | `887-920` | `transport()` |
| `corvus/mavlink_bridge.py` | `922-956` | `_classify_by_descriptor` |
| `corvus/mavlink_bridge.py` | `958-973` | Aliases |
| `corvus/mavlink_bridge.py` | `975-977` | `is_direct_usb` |
| `corvus/mavlink_bridge.py` | `1051-1094` | `_run` |
| `corvus/mavlink_bridge.py` | `1096-1150` | `_claim_serial_exclusive` (`flock`; `nt` early-return) |
| `corvus/mavlink_bridge.py` | `1152-1161` | `_reconnect_delay` (`0.5·2^(n-1)`, cap 8 s, ±25%) |
| `corvus/mavlink_bridge.py` | `1169-1197` | Onboard ports `14540-14549` + warning |
| `corvus/mavlink_bridge.py` | `1199-1326` | `_connect` |
| `corvus/mavlink_bridge.py` | `1248-1259` | `EADDRINUSE` rewrite |
| `corvus/mavlink_bridge.py` | `1371-1421` | `_wait_vehicle_heartbeat` (10 s, slices, immediate-`None` bail) |
| `corvus/mavlink_bridge.py` | `1488-1532` | GCS heartbeat 1 Hz persistent |
| `corvus/mavlink_bridge.py` | `1565-1596` | Tlog per-cycle |
| `corvus/mavlink_bridge.py` | `1628-1630`, `1632-1655` | `set_frame_sink`, `inject_raw` |
| `corvus/mavlink_bridge.py` | `1773-1801`, `1803-1862` | Interval deferral BUG1 + fallback (cap 3) |
| `corvus/mavlink_bridge.py` | `1869-1902` | Home nudge (msg 242) |
| `corvus/mavlink_bridge.py` | `1904-1996` | Version triple + 4 s retry (`v1.18-alpha1` `UNSUPPORTED`) |
| `corvus/mavlink_bridge.py` | `2107-2322` | `_dispatch` guards |
| `src/js/link.js` | `52-86` | Presets |
| `src/js/link.js` | `107-110` | `buildConnectionString` |
| `src/js/link.js` | `170-187` | Recent localStorage |
| `src/js/link.js` | `258-278` | `renderRecent` / `applyConnection` region |
| `src/js/link.js` | `417-437` | `doConnect` |
| `src/js/link.js` | `445-461` | `connectSerial` |
| `src/js/link.js` | `505-575` | Status render (`CONNECTING`/`RECONNECTING`) |
| `src/js/link.js` | `577-654` | `init` (restore-only, no auto dial) |
| `src/js/link.js` | `604-605` | `applyConnection` call site region |
| `src/js/link.js` | `638`, `644-648` | `refreshPorts` |
| `src/js/link.js` | `651-653` | SSE subscribe |
| `src/js/telemetry.js` | `235-237` | `POST /connect` call |

### 15.2 QGC / PX4 source links (reference set for implementers)

- QGC docs — General settings / AutoConnect: `https://docs.qgroundcontrol.com/Stable_V8.0/en/qgc-user-guide/settings_view/general.html`
- QGC docs — Comm Links (manual Serial/TCP/UDP, Auto Connect on Start, high latency): `https://docs.qgroundcontrol.com/Stable_V8.0/en/qgc-user-guide/settings_view/comm_links.html`
- QGC docs — Troubleshooting / vehicle connection: `https://docs.qgroundcontrol.com/Stable_V8.0/en/qgc-user-guide/troubleshoot_qgc/setup_troubleshoot.html`
- QGC source — `AutoConnectSettings`: `https://github.com/mavlink/qgroundcontrol/blob/master/src/AutoConnect/AutoConnectSettings.cc` (toggle model reference)
- QGC source — `LinkManager` / link handling: `https://github.com/mavlink/qgroundcontrol/blob/master/src/comm/LinkManager.cc` (poll + bootloader-wait reference; broad-filter misconnect caution)
- QGC source — `MultiVehicleManager`: `https://github.com/mavlink/qgroundcontrol/blob/master/src/Vehicle/MultiVehicleManager.cc` (per-`(link,sysid)` — out of scope, listed so implementers do not reinvent it)
- PX4 docs — Simulation quickstart (Gazebo, UDP 14550 vs onboard 14540 semantics): `https://docs.px4.io/main/en/simulation/`

---

**Status footer: implemented and verified on 2026-09-16.**

R1–R6 and R8 are done. R9's packaging half is done: the macOS `.app` was built
with `./build.sh` and the new module ships, the layout contract holds, and
neither CI pipeline enumerates modules (their apt sets and build distro agree;
the extra GitHub jobs are the macOS/Windows ones GitLab has no runner for).
R7 had no target — there is no user manual, API reference or offline install
guide in this repo; `docs/auto-connect.md` was written for this feature's
contract and README carries the operator's version.

The §12.3 live SITL smoke (L1–L6) has **not** been run: PX4, Gazebo and
QGroundControl are not installed here. `tests/test_autoconnect_live.py` covers
L1/L3/L5/L6 and AC2/AC3/AC7 on real sockets against a real MAVLink heartbeat
source; AC1/AC4/AC5/AC10 need a flight controller on a cable and remain covered
by the policy tests against the real classifier.

Landed in `corvus/autoconnect.py` (new), `corvus/server.py`, `corvus/app.py`, `serve.py`, `corvus/config.py`, `corvus/state_store.py`, `corvus/mavlink_bridge.py` (classification exports only), `src/js/link.js`, `src/index.html`, `src/css/main.css`, `README.md`, and `tests/test_autoconnect.py` (new).
