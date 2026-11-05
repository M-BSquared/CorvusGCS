# Parity of the native interface with the web interface

Every page, dialog, window, control, shortcut and behaviour of the web
interface (`python/src/index.html`, `python/src/js/*.js`, the plugin hooks),
and where the native interface (`src/`) stands. The web interface stays until
the user has compared both page by page.

Status: **done** (in `src/`, tested against the web's expected values),
**partial** (what is left is in the row), **missing** (not built, in the order
of how much a field operator needs it), **n/a** (cannot or must not exist in a
native window, with the reason).

Paths in the "web file" column are in `python/src/js/` unless they say
otherwise; "native file" is in `src/`.

## Window frame

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Top bar: logo, vehicle, mode, readiness, GPS, battery, altitude, speeds, flight clock, notifications pill | topbar.js | home/topbar.rs | done |
| Hover cards behind GPS, Battery, Altitude, Groundspeed, Vertical speed | topbar.js | home/topbar_cards.rs | done |
| Vehicle block shows the fleet colour swatch and the vehicle name; click opens the Vehicles dialog | topbar.js, fleet.js | home/topbar.rs, fleet/mod.rs | done |
| Notification board, toasts, command dedupe, clear all | topbar.js, notification_dedupe.js | notify/ | done |
| Notifications from vehicles that are not flown (fleet events) | fleet.js `announce` | fleet/mod.rs `new_events` | done |
| Left rail: HOME, MISSION (when enabled), SETUP, ANALYSIS, LOGS, SET | sidenav.js | app/nav.rs | done |
| Right panel with LINK, CONSOLE, SSH, PLUGINS tabs, resize grip, collapse handle, scroll arrows | panel.js | panel/mod.rs | done |
| Responsive width classes (1279, 959, 900, 720, 600) | css/responsive.css | app/responsive.rs | done |
| Interface scale (Settings, `ui.scale`, steps 0.8 to 1.5) | scale.js | app/mod.rs (`set_zoom_factor`) | done |
| Theme from the config, six themes | theme.js | ui/style.rs | done |
| Tablet and touch mode: larger targets, long press as right click, hover controls shown | touch.js, css/touch.css | app/touch.rs | missing |
| Window title from `/api/version` | app.js | app/mod.rs | done |
| First start page (welcome): look, units and review, workspace, controls, import, skip | welcome.js | welcome/ | missing |
| Update check, badge and "newer release" dialog, skip this version, open release page | update.js | settings/about.rs (check only) | partial: no automatic check, no dialog |
| MCP: question dialog (Allow, Refuse), indicator pill with Stop, plugin tools heartbeat | mcp.js | mcp/ | missing |
| One shared `/api/events` stream for console, params, firmware, tiles, shell, mcp, fleet | events.js | net/stream.rs (one stream per topic) | done |
| Vehicle capabilities, greyed controls with the reason | capabilities.js | setup/calibration.rs, mission/backend.rs | partial: no shared module, Home flight bar not gated |
| Lazy vendor bundles | lazy.js | n/a | n/a: nothing to load lazily in a compiled program |
| Credits dialog | credits.js | settings/credits.rs | done |
| Language files, translation function | i18n.js | n/a | later: the language session |
| Unit conversion and display | units.js | data/units.rs | done |
| Animation helpers | anim.js | (egui animates natively) | n/a |

## Home page: map

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| 2D map on the tile proxy, layers, offline | map.js | map/mod.rs | done |
| Rail: zoom, centre/follow, layers, 3D, areas, measure, clear track | map.js | map/rail.rs | done |
| Camera button and the camera list (opens a video window) | map.js `wireCameraButton` | map/rail.rs | missing |
| Search box: coordinates, areas, geocode | map-search.js | map/search*.rs | done |
| Distance tool | map-measure.js | map/measure.rs | done |
| Offline map dialog | tiles.js | map/tiles_*.rs | done |
| Downloaded areas outline | map.js | map/regions.rs | done |
| Track: record, smooth, clear, per-flight colours, keep over restart | map.js | data/track.rs | partial: not kept over restart |
| Track and vehicle drawn in 3D with the depth test | map.js `renderTrack3D` | map3d/gpu.rs | missing: screen overlays today |
| 3D map: terrain, imagery, hillshade, sky, buildings | map.js | map3d/ | done |
| 3D panel: detail, elevation model | map.js | map3d/panel.rs | done |
| Context menu on the map: Fly to this point, Set home here, copy coordinates | map.js, app.js | map/tools.rs | done |
| Fly-to-points plan (PLAN button, click to add, slider AGL, FLY, CLEAR) | map.js, app.js | map/tools.rs, home/flight.rs | done |
| Home and vehicle markers | map.js | map/mod.rs | done |
| Other vehicles on the map in their own colour, name under, own track, click opens the dialog | map-fleet.js | map/others.rs, fleet/tracks.rs | done (tracks are not kept over a restart) |
| Plugin overlays on the map: lines, areas, labels, buttons, markers, icons | map-overlays.js, map-icons.js | plugins/ (another session) | not mine |
| Geofence drawn on the map | geofence.js | setup/geofence.rs | done |

## Home page: flight controls and instruments

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Flight bar: ARM/DISARM, TAKEOFF, LAND, RTL, mode picker, phase | app.js | home/flight.rs | done |
| Takeoff card: altitude slider, entry, confirm | app.js | home/flight.rs | done |
| PLAN button on the flight bar | app.js | home/flight.rs | done |
| Flight bar adapts to ground vehicles (Drive, no takeoff) | app.js | home/flight.rs | check |
| HUD: compass, attitude indicator, six readouts | instruments.js | home/hud.rs | done |
| HUD window: drag, pin, compact, hide readouts, collapse to a pill, position kept, square corners, switched off | hud-panel.js | home/hud.rs, home/mapwindow.rs | done |
| Checklist window on the map, arming warning | checklist.js | home/checklist_window.rs | done |
| Checklist editor | checklist-editor.js | setup/checklist.rs | done |
| Virtual joystick pad, arrow keys, WASD, lock and unlock, boost, key strength, collapse, drag, position kept, 20 Hz and 5 Hz stream | joystick.js | control/mod.rs, control/model.rs, home/mapwindow.rs | done |
| USB gamepad: the block, the sticks summed into the frame, the arm gesture and the gate after it | joystick.js | control/model.rs, control/mod.rs | partial: the rules are done and tested with fed axes; reading a device needs a crate (gilrs, which needs libudev on the Linux build host), left for the user to decide |
| MCP "AI client connected" pill with Stop | mcp.js | mcp/ | missing |

## Multi-vehicle

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Fleet state from the `fleet` topic and `/api/fleet` | fleet.js | fleet/mod.rs | done |
| Vehicles dialog: rows with colour, name edit, status, readout, locate, Take control, remove | fleet.js | fleet/dialog.rs | done |
| Take control confirmation while the flown vehicle is armed | fleet.js | fleet/dialog.rs | done |
| Remove vehicle confirmation (primary link, armed warning) | fleet.js | fleet/dialog.rs | done |
| Links panel: rows, plugged-in offer, free serial ports, open link, close link | fleet.js | fleet/dialog.rs | done |
| Settings Connections: vehicles section | sidenav.js `renderVehiclesSection` | settings/conn_cards.rs | partial |

## Pop-out and floating windows

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Floating frame: drag, corner size grip, maximize, close, shared layer, cascade | float-window.js | term/window.rs, setup/video_window.rs | done (two copies) |
| Terminal in a floating window | term-window.js | term/window.rs | done |
| Camera in a floating window | video-window.js | setup/video_window.rs | done |
| Window of its own: terminal or camera in a native viewport | popout.js, popout-page.js, popout.html | window/ | missing |
| "Open inside the Corvus window" setting, frameless Corvus title bar, pin above | popout.js, popout-page.js | window/ | missing |

## Setup

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Setup page, tiles | setup.js | setup/mod.rs | done |
| First Setup wizard and new controller detection | setup-firstsetup.js, first-setup.js | setup/firstsetup*.rs | done |
| Firmware | setup-firmware.js | setup/firmware*.rs | done |
| Parameters | setup-parameters.js | setup/params*.rs | done |
| Motors; motor dialog title border in red | setup-motors.js | setup/motors.rs | partial: no red title border |
| Safety and sensors, geofence, landing | setup-safety.js, setup-geofence.js | setup/safety.rs, geofence*.rs, landing.rs | done |
| Battery and power | setup-battery.js | setup/battery*.rs | done |
| Calibration, figures, protocol | setup-calibration.js, calib-*.js | setup/calib*.rs | done |
| Radio Control, wizard | setup-control.js | setup/radio*.rs | partial: wizard not clicked through against the simulator |
| Radio table scrolls sideways under 580 px | setup-control.js | setup/radio.rs | missing |
| RC transmitter drawing, learn, inspector | rc-transmitter.js | setup/rc_*.rs | done |
| PID tuning; run bar sticks to the top | setup-tuning.js | setup/tuning.rs | partial: run bar scrolls away |
| Telemetry radio (SiK) | setup-sik.js | setup/sik*.rs | done |
| RTK GPS | setup-rtk.js | setup/rtk*.rs | done |
| Video | setup-video.js | setup/video*.rs | done |
| Remote ID | setup-remoteid.js | setup/remoteid*.rs | done |
| Shared schema form, check values | setup-shared.js | setup/schema_form.rs, check.rs | done |

## Mission

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Planner on the Home map: tools, items, editor, profile, patterns, terrain | mission.js, mission-geometry.js | mission/ | done |
| Upload, save, open, read, new, clear confirmations | mission.js | mission/dialogs.rs | done |
| Fly a mission end to end (upload, start, progress) against the simulator | mission.js | mission/ | done: the simulated PX4 accepts MISSION_START; verified upload, start and "Flying to 2" |
| Row dragging, profile height dragging, marker dragging | mission.js | mission/side.rs, profile.rs, mapview.rs | partial: unit tested only |
| Touch FINISH button, offscreen plan notice, stacked layout under 960, last plan reopening | mission.js | mission/ | missing |
| Shortcuts: Esc cancels row drag, search, pattern, tool; Enter finishes a pattern; Delete removes the point | mission.js | mission/mod.rs | check |

## Settings, Logs, Analysis

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Settings: Interface, Layout, Flight view, Map, Controls, Connections, Data, Plugins, About | sidenav.js | settings/ | done |
| Switches: HUD and square HUD take effect | sidenav.js | settings/mod.rs `Effects` | done |
| Switches: track options, top bar blocks, compass, flight bar take effect | sidenav.js | settings/mod.rs `Effects` | partial: stored, not read by the native pages |
| MCP settings, permissions, connect dialog, token | settings-mcp.js | settings/conn_cards.rs | done |
| Settings export and import | settings-transfer.js | settings/data_cards.rs | done |
| Logs page | sidenav.js | logs/ | done |
| Analysis: logs, Flight Review, Telemetry Review, Live Messages | analysis.js | analysis/ | done |
| Review ground track over satellite imagery | analysis.js | analysis/review.rs | missing |

## Right panel

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Link: status, recent, suggestion, new link (serial, network, presets), auto connect | link.js | panel/link.rs | done |
| Share link (forward to a second station) | link.js | panel/link.rs | done |
| Console: filter, pause, copy, save, clear, input with history, jump to latest | panel.js | panel/console.rs | done |
| SSH tab and saved connections, terminal | panel.js, ssh-term.js | panel/mod.rs, term/ | done |
| Fullscreen console (Shift+Escape) | panel.js | panel/ | check |
| PLUGINS tab | plugins.js | plugins/ (another session) | not mine |

## Keyboard

| Shortcut | Web file | Native file | Status |
| --- | --- | --- | --- |
| Arrow keys and W A S D fly the aircraft (locked until unlocked), Shift boosts, Escape locks | joystick.js | control/mod.rs | done |
| Esc closes menus, dialogs, popovers | ui.js | ui/ | done |
| Esc cancels the distance tool, the plan mode | map-measure.js, map.js | map/measure.rs | partial |
| Enter and Escape in the takeoff entry | app.js | home/flight.rs | done |

## Not mine, listed for completeness

| Feature | Owner |
| --- | --- |
| Plugins in Rust, the PLUGINS tab, plugin map overlays and MCP tools | the plugins session (`src/plugins/`) |
| Every UI string through a translation function | the language session |
