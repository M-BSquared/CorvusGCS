# Corvus GCS: Plan

The one working list for the project. Only open work is listed; finished items
are removed rather than ticked, and the decisions that closed the old audit
gaps are pinned by their tests (`tests/test_mavlink_audit_gaps.py`,
`tests/test_mavlink_router.py`, `tests/test_mavlink_stream_fallback.py`).

Priorities:

* **P1**: breaks PX4 SITL or a real flight, or shows the operator something
  that is not true.
* **P2**: wrong under a condition that will occur, or a missing piece an
  operator will reach for.
* **P3**: polish.

**Verify** marks a finding read from the code and the PX4 source but not yet
seen on a live PX4. Confirm it in SITL before or while fixing it.

Section 2 is what is left of the MAVLink / PX4 audit of 2026-09-22 (PX4 v1.16
to v1.18, Gazebo SITL and real hardware). Fixed since, and so not listed:

* PX4 integer parameters were read and written with the wrong encoding (every
  `INT32` read as 0; a write of `BAT1_N_CELLS = 4` stored 1082130432 while the
  echo looked confirmed), and `_HASH_CHECK` was counted as a parameter.
* Battery & Power has **Check values** (fresh read back, write again what did
  not stick, redraw from the vehicle).
* `corvus/mavlink_bridge.py` is split by protocol (parameters, missions,
  setup commands, shell, telemetry, Remote ID, plus serial ports, signing and
  the command lock), each owning its own state.
* The GCS heartbeat goes out before the vehicle is heard, so `udpout:`,
  mavlink-router server endpoints, SITL in a VM and `SYS_USB_AUTO = 1` connect.
* A vehicle that reboots under a live link is noticed (`SYSTEM_TIME` uptime
  steps back): the parameter state is dropped and the stream rates, version
  and home are asked for again.
* Fly to points puts its takeoff item at the aircraft (then home, then the
  first point), never at 0/0.
* Mission items send NaN, not 0 (north), in the yaw slot of takeoff, waypoint
  and land.
* A calibration cancel waits past PX4's interim `TEMPORARILY_REJECTED` for the
  calibration's own ACK; unused `PREFLIGHT_CALIBRATION` params are 0, not NaN.
* Parameters a firmware lacks are no longer re-requested on every page load.
* 0 mAh consumed is shown as 0; the tab PX4 appends to event-doubling
  STATUSTEXT is stripped.

Fixed on 2026-09-23 from the code and the PX4 v1.16 to v1.18 source (still to
be seen live, see 1.2):

* `COM_RC_IN_MODE`: 3 is "keep the first", 4 is "disabled"; v1.17's priority
  modes 5 to 8 are named when a vehicle holds one.
* Motors: per-output limits (`PWM_MAIN_MIN1`, `_MAX1`, `_DIS1`, `_FAIL1`, the
  same for AUX, DroneCAN and SIM) read for the pins that drive a motor; a SIM
  bank for Gazebo's `SIM_GZ_EC_*`; the DShot labels were one speed off (-5 is
  DShot150, -3 DShot600, no DShot1200); `DSHOT_MIN`.
* `BAT1_SOURCE`: -1 is Disabled, 1 is BATTERY_STATUS over MAVLink, not an ADC.
* `COM_LOW_BAT_ACT = 1`: still obeyed by v1.16 to v1.18 (returns even at the
  emergency level), deprecated and undocumented, so no longer offered; named
  when a vehicle holds it.
* A USB cable gets the UDP stream rates and the 30 s parameter budget; only a
  radio or an unidentified serial port is throttled.
* The calibration wizard no longer falls back to "starting" when its first
  `[cal]` lines beat the command's ACK.
* Reboot autopilot (`MAV_CMD_PREFLIGHT_REBOOT_SHUTDOWN` 1, disarmed only), on
  the Parameters page and after an accelerometer or compass calibration.
* Check values on Safety & Sensors, Motors, PID Tuning, Radio Control and
  Remote ID, sharing Battery & Power's code; their reads take `?fresh=1`.
* A parameter file upload no longer leaves the set at `upload_complete` (the
  editor came back empty); its progress no longer counts the echoes into the
  download counters ("3 / 1400", and a whole-file upload onto an empty cache
  declared itself complete halfway).
* The mode selector keeps the dialect's order with a live mapping.
* Mission download (From vehicle, `MISSION_REQUEST_LIST`) and progress
  (`MISSION_CURRENT`, `MISSION_ITEM_REACHED`) on the Mission page, withdrawn
  when the plan is edited or another station replaces the mission.
* NOT READY names the failing checks: the "Preflight Fail:" / "PreArm:" lines
  of the last report, asked for with `MAV_CMD_RUN_PREARM_CHECKS` when the
  vehicle says not ready without saying why.
* The scene kit's simulated PX4 answers mission downloads and sends
  MISSION_CURRENT, and no longer stops every stream (and so drops the link)
  after adopting an uploaded mission.

---

## 1. Live checks

Nothing below has run against a real PX4 yet: the development machine has
neither PX4 SITL nor QGroundControl. From a source checkout, `./run.sh`.

### 1.1 PX4 SITL (Gazebo) with QGroundControl

- [ ] `make px4_sitl gz_x500`. Confirm it offers 14550 (GCS) and 14540+
      (onboard).
- [ ] Corvus on `udp:0.0.0.0:14550`: heartbeat within about 2 s, mode decoded,
      firmware version filled in (the 4 s retry may be needed on alpha builds),
      home marker within one round trip, attitude about 50 Hz and position
      about 10 Hz in the HUD.
- [ ] Forwarding on (mirror `127.0.0.1:14550`, listen `127.0.0.1:14551`,
      commands off), QGC on its default UDP link: QGC is fed and the LINK tab
      lists `127.0.0.1:14550` as a target.
- [ ] Forwarding off: exactly one of the two programs starves (Linux: the
      second bind fails; macOS: both bind, one receives nothing).
- [ ] Commands on, a disarmed mode change from QGC: injected from a configured
      address, refused (and named in the LINK tab) from an unlisted one. If QGC
      still transmits as sysid 254, the conflict warning latches.
- [ ] Kill SITL for 10 s: degraded at about 3 s, reconnect at about 8 s, a
      fresh tlog per cycle. Restart SITL: home cleared, then re-requested.
- [ ] This machine: `~/.corvus/config.json` still has `udp:0.0.0.0:14540`,
      PX4's onboard port. Switch it to `udp:0.0.0.0:14550` on the LINK tab.

### 1.2 PX4 SITL: the audit findings

- [ ] Parameter encoding: read `BAT1_N_CELLS`, `COM_FLTMODE1` (-1) and
      `SYS_AUTOSTART` in Corvus and in QGC; they must agree. Write
      `BAT1_N_CELLS` from Corvus and read it in QGC.
- [ ] Battery & Power, **Check values**: change two fields, change one of
      them again from QGC, press Check values. Expect "written again,
      confirmed" for that one and the fields redrawn from the vehicle.
- [ ] Calibration: accelerometer, compass, gyro, level, baro. The wizard must
      reach "done" for each (see 2.3 for the start race), and Abort during an
      accelerometer calibration must end in "cancelled" without an error
      toast (the interim-ACK handling was written from PX4's source).
- [ ] Takeoff button, land, RTL, set home by map click, fly to points from
      the ground (the takeoff item is now at the aircraft; confirm PX4 accepts
      it with the default `MIS_DIST_1WP`).
- [ ] Mission page: upload, start, and check the aircraft keeps the heading
      mode's heading at each waypoint (yaw is now NaN).
- [ ] Reboot the vehicle from the MAVLink shell (`reboot`) with the link up:
      the console says it rebooted, the parameter editor asks for a fresh
      download, the HUD rates come back.
- [ ] `udpout:127.0.0.1:18570` straight to SITL's GCS instance connects (the
      station now speaks first).
- [ ] Position and Altitude mode without an RC transmitter: does PX4 accept
      the switch with only the on-screen joystick? Which `COM_RC_IN_MODE`
      does SITL default to?
- [ ] Motor test on `gz_x500` (actuator test in SITL). The Motors page should
      list the motors on the **Simulation** bank (`SIM_GZ_EC_FUNC1..4`) with
      their limits under the selected motor.
- [ ] Output protocol labels on a real board: set a timer to DShot600 in QGC,
      confirm Corvus reads it as DShot600 (-3).
- [ ] Check values on Motors, Safety & Sensors, PID Tuning, Radio Control and
      Remote ID: change a field in Corvus and again in QGC, press Check values.
- [ ] Reboot autopilot from the Parameters page and after an accelerometer
      calibration: accepted disarmed, refused armed, the parameters are read
      again after the restart.
- [ ] Mission download: upload a mission from QGC, press From vehicle on the
      Mission page, compare. Fly it: the green leg and "Flying to" follow
      MISSION_CURRENT. Upload a different mission from QGC mid-flight: Corvus
      stops claiming progress within a MISSION_CURRENT or two. Does this
      pymavlink parse MISSION_CURRENT.mission_id from SITL (it does not in the
      bundled common.xml, so the item count is the signal that fires)?
- [ ] NOT READY reasons: with a failing check (no GPS lock, or `COM_ARM_*`
      that fails), connect: the top bar counts the checks and lists them on
      hover without an arm attempt.
- [ ] Parameter file import with the editor open: the rows show the new values
      and the editor stays on the full set.
- [ ] Log download: list, download one `.ulg`, erase.
- [ ] MAVLink shell: `ver all`, `listener vehicle_status`.
- [ ] Parameter defaults over MAVLink FTP: open the parameter editor after a
      download. The note "Reading the defaults" must give way to a Default
      column, **Modified** must count the parameters off their default, and
      descriptions, units and value lists must show. So far only seen against
      the scene simulator (`tools/scene.py run parameters`), never a real PX4.

### 1.3 Real hardware

- [ ] Pixhawk on USB: autoconnect picks it, parameters download in full,
      calibration and motor test work, firmware flash via bootloader.
- [ ] SiK radio at 57600: telemetry rates stay inside the link, full parameter
      download finishes inside the 180 s budget, Check values works.
- [ ] Both at once (USB plus radio): autoconnect prefers USB and never steals
      an active link.
- [ ] Parameter defaults on real boards with PX4 v1.16, v1.17 and v1.18: the
      editor reads `/etc/extras/parameters.json.xz` over MAVLink FTP
      (`corvus/mavlink_ftp.py`, `corvus/param_metadata.py`) and shows each
      parameter's default. Not yet tried on any real board.
- [ ] A board with little flash (1 MB, PX4's constrained-flash builds) may be
      built without that file (to be confirmed per board). The editor must
      then say "Defaults are not available: this firmware was built without
      parameter metadata" instead of the defaults, and everything else must
      keep working. QGroundControl fetches the file from the web in that case;
      Corvus is offline and does not.
- [ ] The same over a SiK radio at 57600: how long the metadata download
      takes (estimate 20 to 40 s) and whether telemetry stays usable meanwhile.
- [ ] Settings, Files, **Keep a copy on this computer** (off by default): with
      it on, the second connection to the same firmware must answer
      `CalcFileCRC32` for `/etc/extras/parameters.json.xz` and read the copy
      instead of downloading (the editor says "Defaults from the copy kept on
      this computer"). Written from PX4's `MavlinkFTP.cpp`, only seen against
      the scene simulator. A firmware that refuses the checksum must fall
      back to the download and keep nothing.
- [ ] Export a `.params` file from Corvus and import it in QGroundControl, and
      the other way round.

### 1.4 ArduPilot

- [ ] Parameter defaults on real ArduPilot 4.3 to 4.6 (Copter, Plane, Rover):
      `@PARAM/param.pck?withdefaults=1` over MAVLink FTP. Only the pack parser
      has been tested, against packs built in the test suite, never against a
      live vehicle.
- [ ] Export a `.param` file and load it in Mission Planner, and the other way
      round.

---

## 2. MAVLink and PX4 (audit 2026-09-22)

### 2.1 Missions

- [ ] **P3** The Home map does not draw the mission on the vehicle or its
      progress; only the Mission page does. Draw the known mission and the
      leg being flown on the Home map too, from the same store fields
      (`mission_item`, `mission_revision`).

### 2.2 Telemetry and messages

- [ ] **P3** PX4's events protocol itself (`EVENT`, `CURRENT_EVENT_SEQUENCE`,
      decoded with the component metadata fetched over MAVLink FTP). The
      reasons for NOT READY now come from the "Preflight Fail:" text PX4 sends
      beside each arming-check event, which covers the arming checks; decoding
      the events would add the ones PX4 sends only as events (navigation mode
      availability, health warnings without a text twin).

---

## 3. Deferred (do when the trigger occurs)

- [ ] **PERF-4** Per-thread read connections for the tile cache. Trigger:
      profiling shows map tile fill waiting on `TileCache._lock`. Then give
      each HTTP thread its own read connection (`threading.local()`) and keep
      the lock for writes; WAL already allows concurrent readers.
- [ ] **ARCH-3** Split `CorvusHandler` by domain. `corvus/server.py` is about
      7,000 lines with every route in one class. Trigger: the next time two
      changes collide in that file, or the next large feature adds a route
      family. Move route methods into mixin modules (`corvus/routes/mavlink.py`,
      `routes/tiles.py`, ...); the `@route` registry collects them regardless
      of module. Do not start it while other work on `server.py` is
      uncommitted.
