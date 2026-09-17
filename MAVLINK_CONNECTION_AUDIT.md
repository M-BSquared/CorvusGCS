# MAVLink Connection Audit — Bridge, Forwarder, PX4 SITL, Second-Station Forwarding

> **Status: audited, then verified, then acted on.** The body below is the audit
> as written. Every §8.2 gap has since been checked. Two were real:
>
> * **(a)** the onboard-port warning matched `udp:` only, so the explicit
>   spelling of the same bind (`udpin:`) held a companion process' socket in
>   silence. Fixed; regression tests in `tests/test_mavlink_stream_fallback.py`.
> * **(i)** the forwarder has counted refused commands and named the address
>   from the start, and the LINK tab never showed it — so a station at an
>   unlisted address got a working-looking link whose every command vanished.
>   Now said in the forwarding hint (`src/js/link.js`).
>
> **(b)**, **(c)**, **(d)**, **(e)**, **(f)**, **(g)** and **(h)** were confirmed
> intended and are now pinned by `tests/test_mavlink_audit_gaps.py` and
> `tests/test_mavlink_router.py`, so the intent survives the next change.
> The §10.A live SITL + QGroundControl smoke has **not** been run — neither PX4
> nor QGC is installed here. `tests/test_autoconnect_live.py` covers the same
> ground for the link layer on real sockets against a real MAVLink heartbeat
> source; the PX4-specific behaviour on top of it remains unexercised end to end.
>
> Read-only audit. No code was changed *for the audit itself*. Target: PX4 v1.18
> Gazebo default SITL on `udp:…:14550`,
> built-in forwarder to QGroundControl (mirror to `127.0.0.1:14550`, listen on `127.0.0.1:14551`),
> general correctness audit (no specific failure repro).

## 1. Scope + method

Read-only. One new file created; no existing file modified.

Files examined (line numbers are approximate anchors, not exact-change pointers):

| Area | File | Lines / anchors |
|---|---|---|
| Bridge (all link logic) | `corvus/mavlink_bridge.py` (~5015 lines) | `262–263` identity, `749–751` prefixes, `869–920` serial/transport, `1051` `_run`, `1096` serial lock, `1167–1197` onboard warning, `1199–1325` `_connect`, `1327` signing, `1371–1421` heartbeat wait, `1488–1532` GCS hb, `1565–1654` tlog/sink/inject, `1684–1862` streams/intervals, `1869–1990` home/version, `2048–2184` receive/dispatch, `2473–2605` radio/quality/guards, `2753–2758` `_connection_ready`, `2889–3008` command+ACK |
| Forwarder | `corvus/mavlink_forwarder.py` (674 lines) | `76–116` defaults/constants, `119–187` parse/split/source-sysid, `203–278` `__init__`, `284–366` start/bind, `368–392` stop, `398–465` feed/resolve/peers/targets, `467–573` tx/rx loops, `575–625` sweeps/notices, `631–674` introspection |
| Config defaults | `corvus/config.py` | `119` `mavlink_connection`, `331–374` `_coerce_forwarding` |
| Server wiring | `corvus/server.py` | `515–552` `_build_forwarder`, `1936–1994` connect/disconnect, `3517–3637` forwarding GET/POST, `4805–4923` `DEFAULT_MAVLINK_CONNECTION` / `apply_startup_connection` / `create_server` |
| Desktop wrapper | `corvus/app.py` | `161–267` `start_backend`, `402–484` `_stop_all` |
| Browser entry | `serve.py` | `38–121` `_stop_all`, `124–263` `main` |
| Tests | `tests/test_mavlink_router.py`, `tests/test_mavlink_forwarder.py`, `tests/test_server_forwarding.py`, `tests/test_mavlink_stream_fallback.py`, `tests/test_regression_link.py` (+ `test_mavlink_serial.py`, `test_mavlink_version.py`, `test_mavlink_linkquality.py`, `test_mavlink_vibration.py`, `test_mavlink_rc.py`, `test_windows_serial.py`) | see §9 |

Method: follow the connect path top-down (`create_server` → `apply_startup_connection` → `MavlinkBridge.start` → `_run` → `_connect` → `_receive_loop` → `_dispatch`), then the orthogonal forwarder path (`_build_forwarder` → `set_frame_sink(feed)` → `tx_loop`/`rx_loop` → `inject_raw`), checking each gate (heartbeat filter, vehicle guard, ACK matching, port handling) against the PX4 v1.16/1.17/1.18 target and the Gazebo SITL default topology.

## 2. Architecture map

### 2.1 Bridge thread model

`corvus/mavlink_bridge.py:1051` `_run()` is the outer reconnect loop: `_connect()` → `_schedule_message_intervals()` → `_receive_loop()`, and on any exception tears down (`_cancel_pending_commands`, `_abort_parameter_operations`, `_reset_shell_state`, `_stop_tlog`, close, `set_disconnected`) then backs off (`_reconnect_delay`: `corvus/mavlink_bridge.py:1152`, `RECONNECT_BASE_S 0.5 / CAP 8.0 / JITTER 0.25`, `corvus/mavlink_bridge.py:241–244`) and retries.

| Thread | Spawned at | Job | Joined at |
|---|---|---|---|
| `mavlink` (`_run`) | `start()` `corvus/mavlink_bridge.py:988–1001` | connect/receive/reconnect cycle | `stop()` `corvus/mavlink_bridge.py:1046–1047` (3 s) |
| `gcs-hb` (`_gcs_hb_loop`) | `_start_gcs_heartbeat()` `corvus/mavlink_bridge.py:1488–1492` | 1 Hz `heartbeat_send` forever | `stop()` `corvus/mavlink_bridge.py:1048–1049` (2 s) |
| `mavlink-intervals` | `_schedule_message_intervals()` `corvus/mavlink_bridge.py:1773–1801` | one-shot deferred `_request_message_intervals` after 50 ms | `stop()` `corvus/mavlink_bridge.py:1043–1045` (2 s) |
| version-retry | `_schedule_version_retry()` `corvus/mavlink_bridge.py:1946–1960` | one-shot 4 s delayed `AUTOPILOT_VERSION` retry | `stop()` `corvus/mavlink_bridge.py:1040–1042` (2 s) |
| param watchdog | param download path (`PARAM_WATCHDOG_TICK_S 0.5`, `corvus/mavlink_bridge.py:403–434`) | bounded re-request of missing `PARAM_VALUE` indices | `stop()` `corvus/mavlink_bridge.py:1037–1039` (2 s) |
| param upload | `start_param_upload` path | batch `set_param` worker | `stop()` `corvus/mavlink_bridge.py:1024–1026` (3 s) |
| tlog writer | `_start_tlog()` `corvus/mavlink_bridge.py:1565–1586` | non-blocking per-cycle file writer | `_stop_tlog()` `corvus/mavlink_bridge.py:1587–1596` |
| forwarder `mav-forward-rx/tx` | `MavlinkForwarder.start()` `corvus/mavlink_forwarder.py:284–325` | rx parse/learn/inject + tx mirror | `MavlinkForwarder.stop()` `corvus/mavlink_forwarder.py:368–392` (2 s each) |

All sleeps on daemon workers go through `_interruptible_sleep()` (`corvus/mavlink_bridge.py:1756–1771`, 0.1 s slices) so `stop()` wakes them promptly. Shutdown order is identical in both launchers (`serve.py:38–121`, `corvus/app.py:402–484`): flash → logs → forwarder (detach `set_frame_sink(None)` first) → mavlink → ssh → store → HTTP + tiles → `server_close()`.

### 2.2 GCS identity

`corvus/mavlink_bridge.py:262–263`:

```python
GCS_SYSTEM_ID = 254
GCS_COMPONENT_ID = mavutil.mavlink.MAV_COMP_ID_MISSIONPLANNER
```

254 is deliberate: QGroundControl normally uses 255, so Corvus `COMMAND_ACK` / `SERIAL_CONTROL` replies stay separable when both share a vehicle through mavlink-router or the built-in forwarder. Every `mavlink_connection()` call passes `source_system=254, source_component=MISSIONPLANNER` (`corvus/mavlink_bridge.py:1230–1242`). The forwarder repeats the constant without importing it to stay stdlib-only (`corvus/mavlink_forwarder.py:111–116`, `_CORVUS_SYSTEM_ID = 254`; a test pins the two together — see §9).

### 2.3 Connection-string table

Accepted prefixes (`corvus/mavlink_bridge.py:749–751` `_VALID_PREFIXES`, enforced by `validate_connection()` `corvus/mavlink_bridge.py:753–774` and `set_connection()` `corvus/mavlink_bridge.py:776–780`):

| Prefix | pymavlink meaning | Corvus `transport()` (`corvus/mavlink_bridge.py:887–920`) | Notes |
|---|---|---|---|
| `udp:` | bind + listen | `udp` | Default form. Binds the named port. |
| `udpin:` | bind (explicit inbound) | `udp` | Same bucket as `udp:` for transport. |
| `udpout:` | dial-out to a listener | `udp` | Reaches mavlink-router `UdpEndpoint` in server mode / behind NAT. |
| `udpbcast:` | broadcast | `udp` | Same bucket. |
| `tcp:` | dial-out (client) | `tcp` | pymavlink `tcp:` connects out; there is no `tcpout:`. |
| `tcpin:` | bind/listen (server) | `tcp` | The listen half. |
| `serial:` | user-facing only; translated to bare device + `baud=` kwarg (`corvus/mavlink_bridge.py:1226–1235`) | `usb` / `sik` / `unknown` | pymavlink has no `serial:` handler; `:` would route to mavudp. |

Serial parse (`corvus/mavlink_bridge.py:869–885` `_parse_serial`): strip `serial:`, `rpartition(":")`, numeric suffix → baud, else baud `57600` (SiK Radio V3 factory default). Never raises; garbage remainder is treated as device path and rejected later by `validate_connection` (empty device) or by open failure.

Exclusive open (`corvus/mavlink_bridge.py:1096–1150` `_claim_serial_exclusive`): POSIX `fcntl.flock(LOCK_EX|LOCK_NB)` on the descriptor pymavlink opened (what pyserial `exclusive=True` does; pymavlink does not pass it through). Held process dies with the fd — crash leaves nothing stale. Windows (`os.name == "nt"`) returns early: COM opens exclusively already. Best-effort: un-lockable device (pty, odd driver, no `fcntl`) is used anyway.

`transport()` classification (`corvus/mavlink_bridge.py:887–956`):

- `usb` — direct FC USB: `/dev/ttyACM[0-9]+` (`_DIRECT_USB_ACM_RE`, `corvus/mavlink_bridge.py:174`) or `/dev/serial/by-id/*` matching `_PIXHAWK_BYID_RE` (`corvus/mavlink_bridge.py:183`). Only this gates firmware flash.
- `sik` — `/dev/ttyUSB[0-9]+` (`_SIK_RADIO_RE`, `corvus/mavlink_bridge.py:175`).
- Windows `COM<n>` (`is_windows_com_port`, `corvus/mavlink_bridge.py:216`) and macOS `/dev/cu.*|tty.*` (`_MACOS_SERIAL_RE`, `corvus/mavlink_bridge.py:202`) carry no information in the name → `_classify_by_descriptor()` (`corvus/mavlink_bridge.py:922–956`) reads pyserial `hwid`/description; bridge chips (FTDI/CP210x/CH340) → `sik`, Pixhawk FMU → `usb`, else `unknown` (unknown wins ties — fail closed against flashing down a radio). macOS `cu` vs `tty` aliasing handled (`corvus/mavlink_bridge.py:958–973`); `list_serial_ports()` (`corvus/mavlink_bridge.py:787–826`) globs only `cu.*` so each port is listed once.
- `udp`/`tcp` — any of the UDP / TCP prefixes above. Anything else → `unknown`.

### 2.4 Defaults (three places, same intent, different strings)

| Layer | Value | Location |
|---|---|---|
| Fresh-install operator config | `udp:0.0.0.0:14550` | `corvus/config.py:119` (comment `110–118` explains the move off 14540) |
| Server / startup fallback | `udp:127.0.0.1:14550` | `corvus/server.py:4805` `DEFAULT_MAVLINK_CONNECTION`; used by `apply_startup_connection()` `4808–4833` and `create_server()` `4836–4844` |
| `POST /api/mavlink/connect` fallback | `udp:127.0.0.1:14550` | `corvus/server.py:1938` (`payload.get("connection", …)`) |

Both `udp:…:14550` forms are **bind** forms in pymavlink. `0.0.0.0` listens on all interfaces; `127.0.0.1` on loopback only. See gap (b) in §8 about what that implies for QGC coexistence.

## 3. Connect sequence step-by-step

`_connect()` is `corvus/mavlink_bridge.py:1199–1325`. In order:

1. **Reset per-cycle state** (`1201–1224`): `_reset_shell_state()`, `_reset_parameter_cache()`, `_request_sent = False`, home alt refs cleared, `store.update(home=[0,0])` (so a reconnect never draws last session's launch point), heartbeat/jitter/degraded/radio/uplink state cleared, `store.update(link_status="connecting", link_connection=…, link_error="")`.
2. **Open** (`1225–1260`): serial → `mavutil.mavlink_connection(device, baud=…, source_system=254, …)`; else `mavlink_connection(conn_str, timeout=2, source_system=254, …)`. `OSError/FileNotFoundError` on serial → `ConnectionError("serial device unavailable")`. `errno.EADDRINUSE` → rewritten operator message naming QGC / MAVROS / MAVSDK / second Corvus (`1248–1259`). Anything else re-raised.
3. **Claim / warn** (`1261–1264`): serial → `_claim_serial_exclusive()`; else → `_warn_if_onboard_port()`.
4. **Signing** (`1265` → `1327–1369` `_apply_signing`): loads `CORVUS_MAVLINK_SIGNING_KEY_FILE` (32 raw bytes or 64 hex, owner-only 0600, `O_NOFOLLOW`); `setup_signing(sign_outgoing=True, allow_unsigned_callback=…)` exempting only `RADIO_STATUS` (SiK modem has no key), `ADSB_VEHICLE`, `COLLISION`. Unloadable key **fails the connection** rather than downgrading silently.
5. **Heartbeat** (`1267–1269`): `_wait_vehicle_heartbeat(HEARTBEAT_WAIT_S=10.0)` (`266`, `1371–1421`); `None` → `ConnectionError("No heartbeat received")`.
6. **Latch target + pin mavutil** (`1270–1287`): `src_sys/src_comp` from heartbeat (fallback to `conn.target_system/component`, then 1); stored as `_target_system/_target_component` **and** written back to `conn.target_system/target_component` so pymavlink's own `probably_vehicle_heartbeat` latch (which may have picked a different node behind a router) cannot make `mode_mapping()` read the wrong node's table.
7. **Mode mapping** (`1288–1299`): decode `custom_mode` (`_decode_mode`, `1423–1433`, PX4 main/sub layout), `store.update(connected=True, vehicle_type, autopilot, armed, mode, …)`, `store.heartbeat()`, `_build_mode_mapping()` (`1435–1486`; PX4 triple table accepted, flat ArduPilot table → `_modes_unsupported`, empty → built-in `PX4_FALLBACK_MODE_VALUES`).
8. **Streams — serial now, UDP later** (`1300–1317`): serial → `_request_streams()` immediately (point-to-point, nobody else to disturb, 57 kbps radio must not wait a round trip for a HUD). UDP → nothing here; deferred to `_schedule_message_intervals()` in `_run()` (`1061`) because the ACKs are dispatched by the receive loop which does not exist yet (BUG 1).
9. **Version triple + retry** (`1318` → `1904–1990`): `_request_version()` sends `MAV_CMD_REQUEST_MESSAGE(148)` + `MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES` + legacy `autopilot_version_request_send`; then `_schedule_version_retry()` (4 s one-shot, silent after shutdown, skips if `px4_version` already known). Rationale: v1.16–1.18 answer the first; older builds the second; the legacy message is ArduPilot-era (PX4 ignores it); v1.18.0-alpha1 SITL ACKs capabilities with `UNSUPPORTED`, so without the retry the version stays blank.
10. **Home nudge** (`1319` → `1869–1902` `_request_home`): fire-and-forget `MAV_CMD_REQUEST_MESSAGE(242)`; promptness only — the 0.5 Hz stream (plus 1 Hz `SET_MESSAGE_INTERVAL` on UDP, 0.2 Hz on serial) is the backstop.
11. **GCS heartbeat** (`1320` → `1488–1532`): `_start_gcs_heartbeat()` spawns the persistent 1 Hz `MAV_TYPE_GCS / MAV_AUTOPILOT_INVALID` loop if not already alive. Runs while `_running` regardless of degraded state (else the autopilot drops Corvus while Corvus waits for heartbeats to return).
12. **tlog** (`1321–1325` → `1565–1596`): `_start_tlog()` opens a fresh timestamped (+collision-counter) file per cycle, gated on `_running` so direct-`_connect()` unit tests stay hermetic; dir errors disable tlog for the session, never the link.

## 4. Heartbeat / target / router logic

- `_wait_vehicle_heartbeat()` (`corvus/mavlink_bridge.py:1371–1421`): 1 s `wait_heartbeat` slices inside the 10 s wall-clock budget; checks `_stop_event` and `_conn is None` each iteration; an **immediate** `None` (returned in < half the slice) bails at once — a transport answering "nothing" instantly will not start answering differently, and re-spinning would burn the budget at full tilt (see gap (d)).
- `_is_vehicle_heartbeat()` (`corvus/mavlink_bridge.py:358–398`): 3-stage filter. (1) Component blocklist `_NON_AUTOPILOT_COMPONENTS` (`350–355`: GIMBAL, ONBOARD_COMPUTER, TELEMETRY_RADIO, UDP_BRIDGE). (2) Type blocklist `_NON_VEHICLE_TYPES` (`330–344`: GCS, GIMBAL, ADSB, ONBOARD_CONTROLLER, CAMERA, SERVO, BATTERY, PARACHUTE, LOG, OSD, IMU, GPS, WINCH). (3) `autopilot == MAV_AUTOPILOT_INVALID` rejected; missing field (synthetic/BAD_DATA) accepted. This is what stops the bridge latching onto QGC's 1 Hz heartbeat (sys 255) or a MAVSDK companion (`GENERIC` autopilot) behind mavlink-router.
- `_dispatch()` (`corvus/mavlink_bridge.py:2107–…`):
  - `COMMAND_ACK` + `_is_from_vehicle` → console line + autotune hook + `_pending_acks[command]` wake (unless `IN_PROGRESS` and the waiter did not opt in), under `_ack_lock` (`2110–2128`).
  - `MISSION_REQUEST(_INT)` / `MISSION_ACK` → upload handlers gated by `_ack_is_for_us` (`2130–2133`, `2607–2656`).
  - `SERIAL_CONTROL` → shell handler standalone branch (`2138–2139`).
  - `RADIO_STATUS` → `_handle_radio_status()` **before** the vehicle guard and `return` (`2144–2146`), because the SiK modem heartbeats as `ord('3')/ord('D')`, not as the vehicle.
  - Everything else → `if not _is_from_vehicle(msg): return` (`2157–2158`); `HEARTBEAT` additionally requires `_is_from_autopilot` (`2160–2166`) so a gimbal/camera under the vehicle's sysid cannot overwrite type/mode/armed. Only the autopilot heartbeat feeds `store.heartbeat()` (`2170`) — the staleness timer that drives warn/degraded/drop/reconnect — so a foreign heartbeat can never keep a lost aircraft looking connected.
- Guards: `_is_from_vehicle` (`2563–2572`) accepts sysid 0 (synthetic/BAD_DATA, by design — gap (g)) else requires `== _target_system`. `_is_from_autopilot` (`2574–2585`) adds `component == _target_component` (0 accepted). `_ack_is_for_us` (`2587–2605`) checks source sys/comp **and** that the ACK's `target_system/component` matches our own `conn.source_system/component`.
- `_pending_acks: dict[int, _PendingAck]` (`673–674`): single slot per command id, created/consumed under `_ack_lock` with identity check (`is pending`) on pop (`2905–2935`, `2961–2990`); all senders serialize through `_operation_lock` (`_PriorityLock`, `677`), so same-command overlap is structurally limited (gap (c)).

## 5. Stream-rate design

`_message_intervals()` (`corvus/mavlink_bridge.py:1711–1754`):

| Message | Serial (57 kbps SiK budget) | UDP (SITL / router) |
|---|---|---|
| HEARTBEAT | — (own 1 Hz loop covers it) | 1 000 000 µs (1 Hz) |
| ATTITUDE | 100 000 (10 Hz) | 20 000 (50 Hz) |
| GLOBAL_POSITION_INT | 200 000 (5 Hz) | 100 000 (10 Hz) |
| VFR_HUD | 200 000 (5 Hz) | 100 000 (10 Hz) |
| SYS_STATUS | 1 000 000 (1 Hz) | 1 000 000 (1 Hz) |
| GPS_RAW_INT | 1 000 000 (1 Hz) | 1 000 000 (1 Hz) |
| HOME_POSITION | 5 000 000 (0.2 Hz) | 1 000 000 (1 Hz) |
| EXTENDED_SYS_STATE | 1 000 000 (1 Hz) | 1 000 000 (1 Hz) |
| RC_CHANNELS | — (radio buys attitude/position instead) | 200 000 (5 Hz) |

`_request_message_intervals()` (`1803–1862`) walks the table through `_set_message_interval()` (`4619–…`, `_send_command_and_wait` with ACK). Semantics per answer:

- `UNSUPPORTED` → log, `_request_streams()` fallback, **return** (rest of the list would each burn a round trip proving the same thing; never an old PX4 — every PX4 answers this command).
- timeout `-1` → `_request_streams()` insurance once, `silent += 1`; at `INTERVAL_REQUEST_SILENT_LIMIT = 3` (`272`) stop walking (each costs two ACK timeouts holding `_operation_lock`; parking operator commands behind a dead batch is the failure being avoided).
- `-2` (link torn down) → return; next connect starts over.
- accept / DENIED → `silent = 0`, continue; modern path proven, legacy stream never sent on UDP.

`_request_streams()` (`1684–1709`) sends deprecated `REQUEST_DATA_STREAM` from `_stream_rates()` (`1657–…`), guarded by the `_request_sent` once-per-link latch (cleared per `_connect`, `1203`), so the UDP fallback path cannot re-blast the whole set per message. VIBRATION / RC / tuning streams are on-demand via `set_message_interval` public paths, not in either table.

Link-quality tiers behind this: `_heartbeat_timeout()` `2025–2032` (legacy 10 s serial / 5 s UDP) gates **commands**; the receive loop uses the two-tier A3 path `_warn_timeout()` `2034–2039` (6 s serial / 3 s UDP → `degraded`+`poor`, `2093–2103`) and `_drop_timeout()` `2041–2045` (15 s serial / 8 s UDP → `ConnectionError("heartbeat timeout")` → reconnect). UDP SITL has no `RADIO_STATUS`, so `_publish_link_quality()` (`2540–2561`) is heartbeat-driven (`good` on fresh heartbeat; A3 owns `poor`/`lost`). See gap (e).

## 6. Forwarder deep-dive

Off by default: `_build_forwarder()` (`corvus/server.py:515–552`) returns `None` unless config `forwarding.enabled`; a forwarder that cannot bind is still returned (stopped) so Settings can show why. `GET /api/forwarding` (`3517–3541`) reports live `status()` or a synthetic stopped shape from config defaults; `POST /api/forwarding` (`3543–3637`) enforces strict booleans for `enabled`/`allow_commands` (400 otherwise), port ranges (`port` 1–65535, `listen_port` 0–65535 where 0 = ephemeral), string endpoints, then stop-old-start-new **under `_config_write_lock`** (double-click/double-tab race documented `3581–3590`), persists via `save_config`, and returns 409 if the new forwarder did not come up.

- `feed()` (`corvus/mavlink_forwarder.py:398–410`): bounded `deque(maxlen=2000)` (`_MAX_QUEUE`, `98`) drop-oldest, `Condition.notify`, never blocks/raises; no-ops when stopped/sockless/empty. Called from `_write_tlog()` alongside the tlog write (`corvus/mavlink_bridge.py:1598–1626`) — one `get_msgbuf()` serves both.
- `tx_loop` (`467–494`): drains the whole queue as a batch, resolves pending DNS, computes `_targets()` = resolved-static + live-peers **minus self** (`457–465`), `sendto` per frame per target; per-endpoint `OSError` counts `dropped`, never aborts the batch.
- `_resolve_static()` (`412–445`): DNS once at `start(force=True)` (`310`), thereafter only failures retried at most every `_RESOLVE_RETRY_S = 30 s` (`109`) — the resolver never runs per-frame on the tx thread.
- `rx_loop` (`496–573`): `SO_REUSEADDR` socket (`341`), `recvfrom(65535)`; drops own datagrams (`addr == _bound`, `512–513`); per-endpoint reassembly buffer + `split_frames()` v1/v2 (`140–173`, length+framing only — CRC stays in pymavlink; signed v2 `+13` handled, `_MAX_FRAME 280` enforced); `_rx_buf` capped at `_MAX_RX_BUFFERS = 64` with TTL sweep (`523–528`, `575–585`, `_PEER_TTL_S = 30 s`, `101`); **peer learned only on a whole frame** (`544–547`) — a stray/scan datagram or a trailing-byte remainder never subscribes anyone.
- `inject_raw()` (`corvus/mavlink_bridge.py:1632–1654`): writes the forwarder's bytes **untouched** to the vehicle under `_send_lock` (preserves QGC seq/sysid/signature; re-encoding would break signing and PX4 per-sender tracking); `False` on dead link/empty/swap/exception — a second station never kills the link.
- Command gate (`557–568`): `may_command = allow_commands AND addr in _resolved`. Telemetry-out is earned by speaking MAVLink (peer learning); uplink-in requires being a **resolved configured** endpoint. Anything else with commanding on → `_note_command_refused()` (`587–606`, counted + logged once per address). With commanding off nothing is refused and nothing counted (`728` test).
- Self-address drop: constructor refuses an endpoint equal to its own listen address (`234–244`) — else the mirror would loop back and, with commanding on, put own telemetry on the uplink (double load on a 57 kbps radio).
- Busy-listen fallback (`_bind`, `327–366`): requested port busy → ephemeral `0` + `notice` string (LINK tab shows it); only a wholly un-creatable socket returns `None`/failure. Outward mirroring needs no fixed local port.
- Sysid-254 conflict latch (`_note_sysid_conflict`, `608–625`): any inbound frame with source sysid 254 sets `sysid_conflict` once + warns (QGC path to fix: App Settings → MAVLink → Ground Station system ID). Checked **before** the command gate — real on the downlink even for telemetry-only listeners.
- `status()` (`643–674`): `running/host/port/listen_host/listen_port/listen_port_requested/allow_commands/endpoints/peers/targets/frames_sent/datagrams_received/frames_injected/dropped/sysid_conflict/commands_refused/commands_refused_from/notice/error`. `peers` = only stations that spoke (honest "is anything there"), `targets` = where the next frame goes.

## 7. PX4 SITL section

Gazebo default SITL (PX4 v1.16–1.18 shape):

| Port | Role |
|---|---|
| `14550` | **GCS** link — what a ground station binds |
| `14540–14549` | **Onboard** links — MAVROS / MAVSDK / companion (`_ONBOARD_PORTS = range(14540, 14550)`, `corvus/mavlink_bridge.py:1167`) |

Why the default moved off 14540 (`corvus/config.py:110–119`): `udp:` **binds**, so Corvus was taking the socket a companion process needs. Only one holder wins: on **Linux** the second bind fails (`EADDRINUSE` → rewritten message, `corvus/mavlink_bridge.py:1248–1259`); on **macOS** both binds succeed (`SO_REUSEADDR`) and the more specific socket silently wins every datagram — MAVROS goes quiet (no HOME/odometry) and it looks like a broken autopilot, not a port clash. Hence the warning path `_warn_if_onboard_port()` (`1169–1197`, console + log, non-fatal, operator's link left alone). Only fresh installs move; a pre-existing config keeps its value by design (gap (h)).

- UDP SITL link quality is heartbeat-driven: no `RADIO_STATUS` arrives, `_radio_status_seen` stays false, every autopilot heartbeat marks `good` and A3 owns degraded/lost (`corvus/mavlink_bridge.py:2540–2561`, `2177–2179`).
- Shell-on-by-default: `_connect` always `_reset_shell_state()`; `stop()` calls `stop_shell()` best-effort to free PX4's exclusive NSH shell for other clients (`corvus/mavlink_bridge.py:1013–1018`). Worth knowing on SITL because the shell channel is exclusive.
- Version triple-request (`1904–1944` + 4 s retry `1946–1990`): v1.16–1.18 answer `REQUEST_MESSAGE`; older builds `REQUEST_AUTOPILOT_CAPABILITIES`; legacy bare request is ArduPilot-era; v1.18.0-alpha1 SITL answers capabilities with `UNSUPPORTED`, hence the delayed re-ask of `REQUEST_MESSAGE` only.
- HOME prompt (`1869–1902`): one fire-and-forget `REQUEST_MESSAGE(242)` at connect (and after `set_home`); the periodic stream is the backstop.
- QGC double-bind trap on 14550: **Linux** — second binder gets `EADDRINUSE` and QGC silently has no link. **macOS** — both bind, QGC's `127.0.0.1`-specific socket wins, QGC shows a link that receives nothing, neither program errors. Documented in the forwarder module docstring (`corvus/mavlink_forwarder.py:29–49`). That is why Corvus binds **14551** and talks first (same shape as mavlink-router): mirror to `127.0.0.1:14550` unconditionally from the first frame (`DEFAULT_HOST/PORT`, `76–80`), accept dial-ins on 14551 (`DEFAULT_LISTEN_HOST/PORT`, `82–84`), never wait to be spoken to.

## 8. Findings

### 8.1 Correct / intended (why)

1. **Vehicle heartbeat filter (3-stage).** Component + type + autopilot checks (`358–398`) handle the real router cases: QGC heartbeat, MAVSDK `GENERIC` companion, gimbal/radio/bridge nodes. Without all three, `_target_system` latches onto a non-vehicle and every command times out behind a green dot.
2. **Mavutil target pinning.** Writing `_target_system/component` back to `conn.target_system/component` (`1283–1287`) aligns pymavlink's independent `probably_vehicle_heartbeat` latch with the filtered choice, so `mode_mapping()` reads the aircraft's table, not whichever node pymavlink saw first.
3. **Serial-now / UDP-deferred streams.** Serial asks up front (point-to-point, HUD must not wait); UDP defers to the intervals thread (ACKs need the receive loop; blanket legacy rates would shout at a shared stack). The `UNSUPPORTED → fallback+return / timeout → fallback+count-cap-3 / -2 → abort` ladder (`1834–1862`) is the honest evidence-based choice of mechanism.
4. **Once-per-link stream latch.** `_request_sent` (`1694–1709`, cleared per connect) lets the UDP fallback retry per message without re-blasting the legacy set.
5. **Pre-guard `RADIO_STATUS`, post-guard everything else.** Serving the modem's status before the vehicle guard (`2144–2146`) while gating all aircraft state behind `_is_from_vehicle` + autopilot (`2157–2166`) keeps both the SiK uplink score and the "foreign heartbeat never feeds staleness" invariant.
6. **Forwarder two-port shape.** Mirror-to-14550 + listen-on-14551 with talk-first (`29–49`, `76–84`) is the only arrangement that coexists with QGC's default bind on both Linux and macOS. Busy-listen → ephemeral + notice keeps telemetry working when the port is taken.
7. **Asymmetric command gate.** Telemetry-out by peer learning, uplink-in by resolved-configured-address (`557–568`) plus once-only refusal accounting (`587–606`) is the safe default for a widened `listen_host`, and makes misconfiguration visible instead of silent.
8. **Whole-frame-only peer learning + untouched inject.** `split_frames` admission (`529–547`) stops scans/stray packets subscribing or reaching the link; `inject_raw` under `_send_lock` (`1632–1654`) preserves seq/sysid/signature.
9. **EADDRINUSE rewrite + onboard-port warning.** The raw errno becomes an operator sentence naming the likely holders (`1248–1259`); holding 14540–14549 gets a non-fatal named warning (`1169–1197`) instead of a refused connection.
10. **Shutdown/teardown ordering.** Flash+logs before bridge, forwarder detached (`set_frame_sink(None)`) before bridge, all daemon workers joined with bounded timeouts, identical in `serve.py` and `app.py`. The forwarder's socket/threads/queue/peers are fully released by `stop()` (`368–392`).

### 8.2 Candidate gaps (severity-ordered, verify — no fix in this report)

**(a) `_warn_if_onboard_port` only matches `udp:` — misses `udpin:`/`udpout:`/`udpbcast:` on 14540–14549.**
Location: `corvus/mavlink_bridge.py:1184–1189` (`startswith("udp:")`; comment even names the other forms). Why it matters: an operator on `udpin:0.0.0.0:14540` holds the same companion socket with no warning. Verify: unit-test `_warn_if_onboard_port` across all four UDP prefixes, or live-bind 14540 and connect with each form watching for the LINK warning. Related test: `test_binding_px4s_onboard_port_says_so` in `tests/test_mavlink_stream_fallback.py:209`.

**(b) `POST /connect` + server default are bind-forms on loopback — QGC coexistence depends on the forwarder.**
Locations: `corvus/server.py:1938` fallback `udp:127.0.0.1:14550`, `4805` default, `corvus/config.py:119` `udp:0.0.0.0:14550`. Why it matters: all three bind; a second binder on the same host/port hits the §7 trap, so "Corvus + QGC on one laptop" only works via mirror-to-14550/listen-on-14551. This is intended, but operator guidance must say so: to run both, enable forwarding and leave QGC on its default; do not point Corvus at `udpout:` to QGC's port as a substitute. Verify: live SITL + QGC with forwarder off (expect one side starved) vs on (both fed); see §10.

**(c) `_pending_acks` single-slot per command id — safe only via `_operation_lock`.**
Locations: `corvus/mavlink_bridge.py:673–677`, `2120–2128`, `2889–2935`. Why it matters: two overlapping waits on the same command id would share one slot; the design serializes all command senders through `_operation_lock`, and the pop checks identity (`is pending`). Confirm no path (mission upload, param ops, shell, log service) waits on a command ACK outside that lock. Verify: `rg "_pending_acks" corvus/` + focused `pytest tests/test_mavlink_router.py -q` and command-timeout tests.

**(d) Immediate-`None` bail in heartbeat wait.**
Location: `corvus/mavlink_bridge.py:1405–1414`. Why it matters: a transport that instantly returns `None` ends the 10 s budget immediately by design (it will not start answering). Confirm a slow-starting SITL (Gazebo still loading, first heartbeat at t+8 s) cannot false-negative: it returns `None`-with-blocking (waits the slice), not immediate-`None`, so the loop continues. Verify: live cold-start SITL timing; unit test with a fake `wait_heartbeat` that blocks-then-answers vs answers-instantly-`None`.

**(e) Legacy `_connection_ready` (10 s serial / 5 s UDP) vs A3 WARN (3 s) / DROP (8 s) UDP.**
Locations: `corvus/mavlink_bridge.py:2025–2045`, `2753–2758`. Why it matters: during a degraded-but-not-dropped window (3–8 s without heartbeat on UDP) link shows `degraded` while commands are still gated by the 5 s threshold — "connected" semantics differ between display and dispatch. Confirm this split is intended (briefly-degraded link still commands) and surfaced. Verify: `tests/test_mavlink_linkquality.py` + `test_regression_link.py` stall-timing tests.

**(f) No `tcpout:` prefix.**
Location: `corvus/mavlink_bridge.py:749–751`. Why it matters: only because pymavlink's `tcp:` **is** dial-out already (`tcpin:` = listen), so no prefix is missing — but confirm intentional (docs/UI text must not promise a `tcpout:` that validation rejects). Verify: `test_the_dial_out_schemes_are_accepted_and_classified` (`tests/test_mavlink_router.py:294`) + `validate_connection("tcpout:…")` rejection check.

**(g) Sysid 0 accepted by design.**
Locations: `corvus/mavlink_bridge.py:2563–2572`, `2574–2585`, `390–394`. Why it matters: synthetic test messages and pymavlink `BAD_DATA` carry no sender id and must not be rejected — but that is also a spoof surface (a frame with no id is treated as the vehicle's for non-heartbeat state). Intentional; confirm the autopilot-heartbeat tier (`_is_from_autopilot`) is the backstop for the safety-critical fields (mode/armed/type). Verify: router-filter tests feeding sysid-0 vs foreign-sysid frames.

**(h) Stale pre-existing configs still naming 14540 keep old behavior by design.**
Location: `corvus/config.py:116–118` + `corvus/mavlink_bridge.py:1163–1167`. Why it matters: the default move is fresh-install-only; a field laptop upgraded in place keeps binding the onboard port until the operator changes it. Call out as a migration note (warn, don't auto-migrate — auto-changing the link out from under the operator is worse). Verify: load an old config file, confirm value preserved + warning fires on connect.

**(i) Dual-command LAN widening (`listen_host 0.0.0.0` + `allow_commands`) — mitigated but misconfiguration is a silent no-op.**
Locations: `corvus/mavlink_forwarder.py:548–568`, `587–606`; `corvus/server.py:3543–3637`. Why it matters: the resolved-static gate correctly blocks the whole LAN from commanding, but a tablet whose address is not in the endpoint list gets telemetry and silently nothing else. The `commands_refused_from` counter + one-time log exist for exactly this — check they are surfaced in the LINK tab (`status()` fields `commands_refused`, `commands_refused_from`, `643–674`). Verify: `test_a_refused_station_is_named_rather_than_silently_ignored` (`tests/test_mavlink_forwarder.py:689`), `test_nothing_is_refused_when_commanding_is_off` (`:728`), and live: enable commanding, command from an unlisted host, confirm refusal counter increments.

## 9. Test coverage map

| Behavior | Test | What it pins |
|---|---|---|
| Router heartbeat filter (foreign/node-type/autopilot) | `tests/test_mavlink_router.py` | `_is_vehicle_heartbeat` stages; QGC/companion/gimbal heartbeats ignored; vehicle latched |
| Connect pins mavutil target | `tests/test_mavlink_router.py` | `conn.target_system/component` set to filtered choice, not pymavlink's first-seen |
| Dial-out schemes | `tests/test_mavlink_router.py:294` `test_the_dial_out_schemes_are_accepted_and_classified` | `udpout:`/`tcpin:` accepted + `transport()` classified |
| Forwarder sysid parity | `tests/test_mavlink_router.py:356` `test_the_forwarders_copy_of_the_gcs_system_id_matches_the_bridge` | `_CORVUS_SYSTEM_ID == GCS_SYSTEM_ID` (stdlib-only duplication stays in step) |
| Forwarder feed/tx/rx/split/inject/gates | `tests/test_mavlink_forwarder.py` (`:128` stopped-feed no-op, `:340` own-address refusal, `:689` refused station named, `:728` nothing refused when off) | bounded queue, whole-frame learning, static-gate refusal, sysid-conflict latch, ephemeral fallback |
| Forwarding API validation + lifecycle | `tests/test_server_forwarding.py` (`:76` bad listen port, `:97` bad target port) | strict bool/ports, stop-old-start-new, 409-when-down, status shape |
| Stream fallback ladder | `tests/test_mavlink_stream_fallback.py` (incl. `:209` `test_binding_px4s_onboard_port_says_so`) | `UNSUPPORTED → fallback+return`, silent-cap-3, `-2` abort, onboard-port warning text |
| EADDRINUSE message | stream-fallback / router set (message-text assertion) | errno rewritten to QGC/MAVROS/second-Corvus sentence |
| Serial exclusivity + baud default | `tests/test_mavlink_serial.py`, `tests/test_windows_serial.py` | `flock` conflict → `ConnectionError` + `link_error`; `57600` default; COM/`cu` classification |
| Version triple + retry | `tests/test_mavlink_version.py` | three requests sent; 4 s retry only when `px4_version` blank; silent after stop |
| Link quality / A3 tiers | `tests/test_mavlink_linkquality.py`, `tests/test_regression_link.py` | heartbeat-driven `good` on SITL; WARN→degraded, DROP→reconnect; `_connection_ready` gate |
| On-demand streams (vibration/RC/tuning) | `tests/test_mavlink_vibration.py` (`:141–202`), `tests/test_mavlink_rc.py`, `tests/test_tuning.py` | `set_message_interval` connected/disconnected paths; armed-safety exemptions |
| Shell via router | `tests/test_mavlink_shell.py` (`:203–286`) | router shell traffic ignored unless console active; target gating |

## 10. Recommended next steps (verify-only, no code changes in this task)

### A. Live SITL smoke — PX4 v1.18 Gazebo + QGC via forwarder

1. Start default SITL: `make px4_sitl gz_x500` (or the v1.18 Gazebo default). Confirm it offers `14550` (GCS) and `14540+` (onboard).
2. Launch Corvus with default connection (`udp:0.0.0.0:14550` fresh config, or `udp:127.0.0.1:14550`). Expect: heartbeat ≤ ~2 s, mode decoded, `AUTOPILOT_VERSION` populated (may take the 4 s retry on alpha SITL), home marker within ~1 round trip + 1 Hz thereafter, attitude ~50 Hz / position ~10 Hz in the HUD.
3. Enable forwarding (Settings → forwarding → on; mirror `127.0.0.1:14550`, listen `127.0.0.1:14551`, `allow_commands` **off**). Start QGC with its default UDP link. Expect: QGC feeds without touching Corvus; Corvus LINK tab shows `targets` including `127.0.0.1:14550`, `peers` once QGC dials 14551 (if it does).
4. QGC coexistence check (forwarder off): confirm exactly one side starves per the OS rule (Linux `EADDRINUSE` on second bind / macOS silent winner) — documents gap (b) for operators.
5. Command-gate check: turn `allow_commands` on, send a (safe, disarmed) mode change from QGC. With QGC's address configured → injected (`frames_injected` increments). From an unlisted host → refused (`commands_refused`/`commands_refused_from` increment, one warning logged). Confirm `sysid_conflict` latches if QGC still transmits as 254, and clears on forwarder restart.
6. Kill SITL 10 s → confirm `degraded` at ~3 s, reconnect cycle at ~8 s, fresh tlog per cycle; restart SITL → home cleared then re-asked, no stale marker.

### B. Focused pytest commands

```bash
python3 -m pytest tests/test_mavlink_router.py tests/test_mavlink_forwarder.py tests/test_server_forwarding.py -q
python3 -m pytest tests/test_mavlink_stream_fallback.py tests/test_mavlink_version.py tests/test_mavlink_serial.py -q
python3 -m pytest tests/test_mavlink_linkquality.py tests/test_regression_link.py -q
python3 -m pytest -q   # full gate
```

---

*Audit method: static read of the paths in §1. No SITL was launched and no code edited for this report; live steps in §10 are proposed verification, not claimed results.*
