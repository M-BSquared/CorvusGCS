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
| Tablet and touch mode: flight bar and rail targets, no zoom buttons, long press as right click, HUD buttons shown, SSH tab and panel edge gone, kept on this device | touch.js, css/touch.css | app/touch.rs, home/flight.rs, map/rail.rs, home/hud.rs, panel/mod.rs, settings/tabs.rs | partial: the long press and the mode are unit tested, the sizes seen on screen with the mode on; not done: bigger form controls, Settings pills, setup pages, fullscreen entry, hold to move the HUD, double tap |
| Window title from `/api/version` | app.js | app/mod.rs | done |
| First start page (welcome): look, units and review, workspace, controls, import, skip | welcome.js | welcome/mod.rs, welcome/model.rs | done: start page and four steps drawn and unit tested; the steps after the first were not clicked through against the sandbox |
| Update check, "newer release" dialog, skip this version, open release page | update.js | update/mod.rs, settings/about.rs | done: unit tested, the dialog was not seen against a real answer (the sandbox is offline) |
| Update marker (a dot on SET, the version in its tooltip) | update.js | app/nav.rs, update/mod.rs | done (the dot on the About pill is not drawn) |
| MCP: question dialog (Allow, Refuse), indicator pill with Stop, refusals on the board, window heartbeat | mcp.js | mcp/mod.rs | done: unit tested; the plugin tools of Schwalby were announced by the heartbeat and called by a live MCP client |
| One shared `/api/events` stream for console, params, firmware, tiles, shell, mcp, fleet | events.js | net/stream.rs (one stream per topic) | done |
| Vehicle capabilities, greyed controls with the reason | capabilities.js | data/capabilities.rs (calibration cards, the control pad's stick mode, ground vehicle words) | done; the Mission page still reads its own `mission_unsupported` copy of the document |
| Lazy vendor bundles | lazy.js | n/a | n/a: nothing to load lazily in a compiled program |
| Credits dialog | credits.js | settings/credits.rs | done |
| Language files, translation function | i18n.js | i18n/, lang/ | done: looked at in German on the plugin pages (Schwalby shelf, editor and popup, Vibration, SSH Launcher, Map Entities), Settings Plugins, the Vehicles and Offline map dialogs and the notification board; nothing clipped. Texts the web leaves in English were added in `lang/native` and the plugins' own files. The rail's PROTOKOLLE is wider than its 58 px item, as it is on the web page |
| Unit conversion and display | units.js | data/units.rs | done |
| Animation helpers | anim.js | (egui animates natively) | n/a |

## Home page: map

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| 2D map on the tile proxy, layers, offline | map.js | map/mod.rs | done |
| Rail: zoom, centre/follow, layers, 3D, areas, measure, clear track | map.js | map/rail.rs | done |
| Camera button and the camera list (opens a video window) | map.js `wireCameraButton` | map/cameras.rs, map/rail.rs | done |
| Search box: coordinates, areas, geocode | map-search.js | map/search*.rs | done |
| Distance tool | map-measure.js | map/measure.rs | done |
| Offline map dialog | tiles.js | map/tiles_*.rs | done |
| Downloaded areas outline | map.js | map/regions.rs | done |
| Track: record, smooth, clear, per-flight colours, keep over restart | map.js | data/track.rs, data/track_store.rs | done |
| 3D robustness: context checks and the flat map as fallback, budgets per frame, bounded GPU tables, antimeridian and poles, offline notice, debug overlay (F3), regression pictures | map.js | map3d/gpu.rs, map3d/mod.rs, map3d/shots/ | done: the 3D map tested live on the Alps and a city; not tried on Linux or Windows, not on a second screen with another pixel density |
| Geofence outline in 3D | map.js `setPolygonOverlay` | map3d/mod.rs `paint_fence` | done: drawn on the ground in pieces, the ones behind a hill left out; the fill is a flat wash on the screen |
| Track drawn in 3D with the depth test (a ribbon mesh, constant width on the screen) | map.js `renderTrack3D` | map3d/ribbon.rs, map3d/gpu.rs | done |
| Vehicle and home hidden behind a hill in 3D | map.js `occludedByGlobe` | map3d/mod.rs `hidden` | done: a terrain ray test on the screen markers (home, vehicle, other vehicles, plugin markers, geofence pieces), not a depth test; the ray test is a function with a test of its own |
| 3D map: terrain, imagery, hillshade, sky, buildings | map.js | map3d/ | done |
| 3D panel: detail, elevation model | map.js | map3d/panel.rs | done |
| 3D buildings from an Overpass answer, through the backend's own cell | map.js, python/corvus/buildings.py | map3d/buildings.rs | done: a dense city centre (515 buildings, `src/map3d/shots/make_city.py`) goes through the real `BuildingService` from a stubbed Overpass server into the backend's cache and was looked at in the running app (heights, opacity, no z fighting); also read and extruded in a test |
| Context menu on the map: Fly to this point, Set home here, copy coordinates | map.js, app.js | map/tools.rs | done |
| Fly-to-points plan (PLAN button, click to add, slider AGL, FLY, CLEAR) | map.js, app.js | map/tools.rs, home/flight.rs | done |
| Home and vehicle markers | map.js | map/mod.rs | done |
| Other vehicles on the map in their own colour, name under, own track, click opens the dialog | map-fleet.js | map/others.rs, fleet/tracks.rs | done (kept over a restart in ~/.corvus/gui-tracks.json) |
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
| MCP "AI client connected" pill with Stop | mcp.js | mcp/mod.rs | done |

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
| Window of its own: terminal or camera in a native viewport, back in with a button | popout.js, popout-page.js, popout.html | app/popout.rs, term/window.rs, setup/video_window.rs | done: the frame in the app was seen with its new button; the window out was started without error but a screenshot cannot see a second window |
| "Open inside the Corvus window" setting | popout.js | app/popout.rs, settings/mod.rs `Effects` | done |
| Frameless Corvus title bar, pin above the Corvus window, drag past the edge to carry a frame out | popout.js, popout-page.js | n/a | not done: egui draws the window out with the system's title bar, and has no always-on-top above one window only |

## Setup

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Setup page, tiles | setup.js | setup/mod.rs | done |
| First Setup wizard and new controller detection | setup-firstsetup.js, first-setup.js | setup/firstsetup*.rs | done |
| Firmware | setup-firmware.js | setup/firmware*.rs | done |
| Parameters | setup-parameters.js | setup/params*.rs | done |
| Motors; motor dialog title border in red | setup-motors.js | setup/calibration.rs, ui/modal.rs | done |
| Safety and sensors, geofence, landing | setup-safety.js, setup-geofence.js | setup/safety.rs, geofence*.rs, landing.rs | done |
| Battery and power | setup-battery.js | setup/battery*.rs | done |
| Calibration, figures, protocol | setup-calibration.js, calib-*.js | setup/calib*.rs | done |
| Radio Control, wizard | setup-control.js | setup/radio*.rs | partial: the wizard opens against the simulator and shows step 1 of 5; Next to Write to vehicle was not driven (the harness cannot scroll to the buttons or move the simulated sticks), the later steps rest on the model tests |
| Radio table scrolls sideways under 580 px | setup-control.js | setup/radio.rs | done: built, not looked at on screen |
| RC transmitter drawing, learn, inspector | rc-transmitter.js | setup/rc_*.rs | done |
| PID tuning; run bar sticks to the top | setup-tuning.js | setup/tuning.rs | done: built, not looked at on screen |
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
| Touch FINISH button (tablet mode), offscreen plan notice | mission.js | mission/toolbar.rs, mission/mod.rs | done: both seen in the running app against the simulator (the plan notice after a quiet reopening of a plan in another city; FINISH ending a drawn area with 16 points). Found and fixed: a finished shape still said "Enter or a double click" in tablet mode instead of "Press FINISH ..." |
| Stacked layout under 960 px: map at 55 percent of the window (not under 320 px), the profile as a band, the sidebar at its natural height with the list capped at 320 px, one column that scrolls | mission.js, css/mission.css | mission/mod.rs `show_stacked`, mission/side.rs, mission/profile.rs | done: seen at 900 px with a drawn plan; scrolling was exercised with a forced offset only |
| Last plan reopening (the plan last saved or opened, kept in `~/.corvus/gui-mission-last.txt`, opened quietly, forgotten when deleted) | mission.js | mission/mod.rs `last` | done |
| Shortcuts: Esc cancels row drag, search, pattern, tool; Enter finishes a pattern; Delete removes the point | mission.js | mission/mod.rs | check |

## Settings, Logs, Analysis

| Feature | Web file | Native file | Status |
| --- | --- | --- | --- |
| Settings: Interface, Layout, Flight view, Map, Controls, Connections, Data, Plugins, About | sidenav.js | settings/ | done |
| Switches: HUD and square HUD take effect | sidenav.js | settings/mod.rs `Effects` | done |
| Switches: track options (earlier flights, rainbow, clear on restart, in the air in 3D), altitude, groundspeed and vertical speed blocks, compass nose up | sidenav.js | settings/mod.rs `Effects`, map/mod.rs `TrackLook`, home/hud.rs | done |
| Switches: flight bar shrink and separate Takeoff and Land, the distance sensor row of the altitude card and its stream | sidenav.js | home/flight.rs, home/topbar_cards.rs, app/mod.rs | done: unit tested, not looked at on screen |
| MCP settings, permissions, connect dialog, token | settings-mcp.js | settings/conn_cards.rs | done |
| Settings export and import | settings-transfer.js | settings/data_cards.rs | done |
| Logs page | sidenav.js | logs/ | done |
| Analysis: logs, Flight Review, Telemetry Review, Live Messages | analysis.js | analysis/ | done |
| Review ground track over satellite imagery: the Chart / Satellite switch, the layer rule, fit with 36 px padding, the key, the zoom buttons, ctrl + scroll to zoom with its hint | analysis.js | analysis/trackmap.rs, analysis/review.rs | done: seen against the simulator's flight log on the Esri layer; the imagery's square corners poke 2 px past the rounded border |

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
| Plugins in Rust, the PLUGINS tab, plugin map overlays and MCP tools | the plugins session (`src/plugins/`); all four bundled plugins are ported, Schwalby with its shelf, companion strip and MCP tools (seen against the simulator and an MCP client over `POST /mcp`) |
| Every UI string through a translation function | the language session |
