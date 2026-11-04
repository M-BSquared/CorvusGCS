# corvus_gui

The Corvus GCS desktop interface as a native Rust app (egui/eframe). It is
replacing the web interface in `src/`, page by page, and later becomes the
Android app as well.

It is a client of the backend's HTTP/SSE API, the same one the web interface
uses, so the backend does not change for it:

```bash
cargo run --release                                   # start serve.py here and show it
cargo run --release -- --server http://10.0.0.5:8080  # a backend elsewhere
cargo run --release -- --root DIR --python PATH       # where serve.py is, what runs it
cargo run --release -- --page motors --scroll 560     # open on a page (setup, any tile such as battery or motors, safety:SENSOR), scrolled
# looking at it without a hand on the mouse
CORVUS_GUI_SHOT=out.png cargo run -- --page motors     # save the window's own pixels after 9 s and quit
CORVUS_GUI_SIZE=700,812 cargo run                      # open at that size, below the usual floor too
CORVUS_GUI_HOVER=600,50 cargo run                      # hold the pointer there (a hover card, a hint)
CORVUS_GUI_CLICK=560,310 cargo run -- --page motors    # press there once, at 6 s (an open menu)
CORVUS_GUI_CLICK="178,472;400,200;enter" cargo run    # several presses, half a second apart; enter is the key
CORVUS_GUI_3D=11.5,47.4,13,200,82 cargo run             # open the 3D map, here: lng,lat,zoom[,bearing[,pitch]]; 1 alone keeps the view
CORVUS_GUI_3D_PANEL=1 CORVUS_GUI_3D=1 cargo run         # with the 3D panel (buildings, elevation models) open
```

Locally, `serve.py` runs as a child on a free port. Every way out (window
closed, SIGINT, SIGTERM) sends it SIGTERM, which runs its own teardown, and
SIGKILL if it is still there after 8 s. If the window does not close within
10 s of a signal, the child is killed and the process exits.

## What is here

| | |
| --- | --- |
| `src/net/backend.rs` | starts and stops `serve.py` |
| `src/net/http.rs` | a small HTTP/1.1 client, with a read timeout per read so a stream reader can stop |
| `src/net/sse.rs` | server-sent events |
| `src/data/telemetry.rs` | the `/api/telemetry` reader thread and the latest state |
| `src/data/units.rs` | display units from the config's `ui.units`, rounded the way the web interface rounds |
| `src/home/topbar.rs` | the status bar, word for word the web interface's (`src/js/topbar.js`) |
| `src/map/mod.rs` | the map (walkers) on the backend's tile proxy, vehicle and home markers |
| `src/map/rail.rs` | the rail and the two buttons beside it: zoom, centre, layers, downloaded areas, measure, the clear track disc |
| `src/map/regions.rs` | the downloaded areas of `/api/tiles/regions`, merged as the web map merges them (`map.js`) |
| `src/map/measure.rs` | the distance tool: great circle distance, bearing and the two click rule (`map-measure.js`) |
| `src/map/search.rs`, `src/map/search_card.rs` | the place search: coordinate notations read offline, area names, `/api/geocode`, the box and its list (`map-search.js`) |
| `src/map/tiles_model.rs`, `src/map/tiles_card.rs` | the offline map dialog: tile counts and sizes, the download and its jobs, the areas on this laptop (`tiles.js`) |
| `src/map3d/camera.rs`, `src/map3d/mod.rs`, `src/map3d/terrain.rs`, `src/map3d/buildings.rs`, `src/map3d/tile_cache.rs`, `src/map3d/panel.rs` | the 3D map: camera maths, the tilted ground of imagery and elevation tiles with hillshade and skirts, the sky and horizon haze, OSM buildings extruded from the backend's cells, the elevation model choice, the tile cache behind them, and the 3D panel (`src/js/map.js` 3D mode) |
| `src/data/track.rs` | the flown track, recorded and smoothed as `src/js/map.js` does |
| `src/home/hud.rs` | compass, attitude indicator and readouts (`src/js/instruments.js`) |
| `src/home/flight.rs` | the flight bar, takeoff card and mode picker (`src/js/app.js`) |
| `src/ui/style.rs` | the design tokens of the six themes (light, light-orange, green, blue, pink, orange) and the type |
| `src/setup/mod.rs` | the Setup page: head, vehicle card, the tile grid and the sub-pages (`src/js/setup.js`) |
| `src/setup/params.rs` | the Parameters page: header actions, download, search, filter, defaults, edit and apply, rows that stack under 721 px (`src/js/setup-parameters.js`) |
| `src/setup/params_dialogs.rs`, `src/setup/params_files.rs` | Export, Import and the reboot confirmation, and the three file formats with the import preview; Import asks for a file path because a native window has no picker without a dependency |
| `src/ui/modal.rs` | the `.modal` card shared by those dialogs |
| `src/term/emu.rs`, `src/term/view.rs` | the terminal: a VT parser (the `vte` crate) feeding a cell grid with colours, scroll regions, the alternate screen and 5000 lines of scrollback, drawn with keyboard, wheel, selection and paste (`src/js/ssh-term.js`) |
| `src/term/session.rs`, `src/term/window.rs` | one SSH session over `/api/ssh/stream` with ordered keystrokes, and the floating terminal window (`src/js/term-window.js`); the SSH tab holds the same terminal |
| `src/setup/motors.rs` | the Motors page: the airframe drawing, the motor table with output assignment, the motor test, frame size, protocol forms (`src/js/setup-motors.js`) |
| `src/setup/safety.rs` | the Safety & Sensors page: the backend's description in two stacks, the landing preset, the sensor tiles, Check values and the checklist card (`src/js/setup-safety.js`) |
| `src/setup/battery.rs` | the Battery & Power page: the pack drawn with its cells, fill and failsafe levels, the live readout, how Corvus reads the pack (its own settings, saved to the config), the vehicle's parameters (`src/js/setup-battery.js`) |
| `src/setup/battery_model.rs` | its rules: where a level sits on the pack, the flag stacking, the readout lines, the values copied from the vehicle |
| `src/setup/calibration.rs` | the Calibration page: readiness chips, the flight controller rotation, the seven calibration cards, and the wizard (stage, phase, cue, progress, position strip, the autopilot's messages, the motor warning) (`src/js/setup-calibration.js`) |
| `src/setup/calib_protocol.rs` | the calibration protocol as a state machine: STATUSTEXT lines to events, events to what the wizard shows; held to `calib_protocol_cases.json`, what `calib-protocol.js` answered for 182 lines and 7 transcripts |
| `src/setup/calib_figure.rs` | the raven in the attitude asked for: pose matrices, slerp between poses, projection, painter sort and shading, the ground, the compass ring (`src/js/calib-figures.js`); the mesh is `calib_raven.json`, the web figure's own, and `calib_figure_cases.json` is what the web figure drew |
| `src/ui/chip.rs` | a pill with an icon or status dot (`.calib-ready-chip`, `.calib-phase`, `.calib-prep-item`) |
| `src/setup/radio.rs` | the Radio Control page: the transmitter and the live channel bars, the two tabs of configuration (Detect beside every channel picker, the flight mode strip, the per channel calibration table), and the calibration wizard in five steps (`src/js/setup-control.js`) |
| `src/setup/radio_model.rs` | its rules: the tab of a section, PX4's and ArduPilot's mode slot arithmetic, double booked channels, and the wizard's measurement (sweep, stick detection, the rows written and the request) |
| `src/setup/rc_transmitter.rs` | the drawn transmitter's model: the layout (`rc_transmitter.json`, the web `LAYOUT` dumped by running it), the four stick modes, pulse to position, the eased reading between samples, and the controls this station learned, kept in `~/.corvus/gui-rc-transmitter.json` |
| `src/setup/rc_drawing.rs` | the handset drawn and moved by RC_CHANNELS, its callouts, Learn, and the inspector that assigns a channel a function (`src/js/rc-transmitter.js`) |
| `src/setup/tuning.rs` | the PID Tuning page: the readiness chips, the run strip, the autotune card, one block per loop (live graphs, gains, its tune button) and the autotune settings (`src/js/setup-tuning.js`) |
| `src/setup/tuning_model.rs` | its rules: when an autotune can start and what stands in its way, the reboot the module waits for, the bar, the graph buffers of 600 samples |
| `src/ui/chart.rs` | a line chart in the look of the web's Plotly graphs: auto ranged axes, nice ticks, grid, a dotted setpoint beside the response |
| `src/setup/rtk.rs` | the RTK GPS page: the route the corrections take (base, this station, the aircraft's receiver) polled once a second, the survey bar and its sentence, and the settings (survey or known mark, NTRIP caster) (`src/js/setup-rtk.js`) |
| `src/setup/rtk_model.rs` | its words and rules: states, the survey sentence, the rows, the request body; checked against `rtk_cases.json`, what the web helpers answered |
| `src/setup/firmware.rs` | the Firmware page: the link and gate cards, the picker (flight stack, release, manufacturer, board, developer builds, what the board was detected as), a local file by path, Flash and Cancel, the bar and the flash log fed by the `firmware` event topic (`src/js/setup-firmware.js`) |
| `src/setup/firmware_model.rs` | its rules: the catalogue and the picker that lands only on a board somebody chose, the flash gate, the words for the transport and the progress, the request bodies |
| `src/setup/firmware_io.rs` | the catalogue read with a minute to answer, and the image upload as a raw body that stops when the page closes |
| `src/setup/firmware_flow.rs` | tests: the page against a stand-in backend that answers the status and catalogue, accepts a flash and streams progress (no board, no bootloader) |
| `src/setup/firstsetup.rs` | the First Setup page: the eight numbered steps, the bar with Previous, Skip and Next, the hosted pages (Firmware, Motors, Calibration, Radio Control, Safety, Battery), the aircraft type step on `POST /api/first-setup/airframe` with its reboot, and the finish summary (`src/js/setup-firstsetup.js`) |
| `src/setup/firstsetup_model.rs` | its rules: which step shows as done, where the wizard opens, the aircraft and finish wording, the session's memory, the reboot follower, and the detector's timers and the list of controllers not to ask about again (`src/js/first-setup.js`); held to `firstsetup_cases.json`, what the web helpers answered |
| `src/setup/firstsetup_watch.rs` | the detector's glue: reads `/api/first-setup` and `/api/firmware/status` when a link or a bootloader port appears and draws the "New controller detected" dialog |
| `src/setup/firstsetup_flow.rs` | tests: the page against a stand-in backend (the step it opens on, the preset it writes, a refusal, Finish, the reboot) |
| `src/ui/optioncards.rs` | `Corvus.ui.optionCards`: a grid of selectable cards with a label and a quiet description |
| `src/ui/formfield.rs` | `Corvus.ui.field`: the small caption, a 32.5 px box or select, and the hint under it, shared by the plain form pages |
| `src/setup/sensors.rs` | the sensor model and its rules: the write chain of a switch, the presets, the extra parameters kept in `~/.corvus/safety-extra.json` |
| `src/setup/sensor_view.rs` | a sensor page drawn: Status card, Hardware card with the write table, the parameters you add by name |
| `src/setup/check.rs` | Check values: read back, write again what did not stick, the result card (`src/js/setup-shared.js`) |
| `src/setup/geofence.rs` | the geofence area the station keeps, its geometry (corners, crossing, area, framing) and the one shared copy (`src/js/geofence.js`) |
| `src/setup/geofence_card.rs` | the Geofence card: the small map, drawing, dragging and adding corners, upload and removal, the large editor (`src/js/setup-geofence.js`) |
| `src/setup/landing.rs` | the Landing card: the descent as one bar, the fields beside each stretch, the brackets with their lengths (`landingCard` of `src/js/setup-safety.js`) |
| `src/app/responsive.rs` | the width classes of the web (`data-vw`): what folds, shrinks or stacks at 1279, 959, 900, 720 and 600 px (`src/css/responsive.css`) |
| `src/ui/popover.rs` | the hint icon and the sheet it opens, the cards of the top bar: hover intent, pinning, one at a time, placement (`Corvus.ui.popover`, `infoHint`) |
| `src/home/topbar_cards.rs` | the cards behind GPS, Battery, Altitude, Groundspeed and Vertical speed (`src/js/topbar.js`) |
| `src/home/checklist_window.rs` | the checklist's window on the Home map: ticks kept by item text, drag and fold, the warning on arming with items open (`src/js/checklist.js`) |
| `src/setup/checklist.rs` | the preflight checklists: config block, Setup card and the dialog the lists are written in (`src/js/checklist-editor.js`) |
| `src/setup/schema_form.rs` | the schema-driven form the backend describes (enum, number, sign, bitmask), shared by the Safety and Motors pages (`src/js/setup-shared.js`) |
| `src/ui/pagekit.rs` | page furniture: header, header with its Reload and Check buttons and status, section title, tabs, tile, back button, the armed notice |
| `src/ui/kit.rs` | buttons (`Button`, `Variant`), icon buttons, select, checkbox, switch; the widgets of `components.css` |
| `src/ui/field.rs` | `TextField`: the one text box, on a well, glass or floating surface, with an optional icon |
| `src/ui/menu.rs` | `dropdown` and `row`: every list (select choices, flight modes, elevation models) |
| `src/ui/card.rs` | `card::paint` / `reserve` / `fill`: the panel, page and inset cards |
| `src/ui/segmented.rs` | the segmented switch (`.params-filter`, `.link-kinds`) |
| `src/notify/mod.rs` | the notification board, its read and dismiss rules, the command dedupe and the toasts (`src/js/topbar.js`) |
| `src/notify/overlay.rs` | the NOTIFICATIONS pill, the board and the toast stack |
| `src/ui/frost.rs` | the frosted glass under every `.glass` surface |
| `src/ui/fonts.rs`, `src/ui/icons.rs`, `build.rs` | Inter, JetBrains Mono and the Lucide icons, from the web's own files |

## The 3D map

`walkers` draws a flat map only, so 3D mode (`src/js/map.js` `set3D`) has a
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
  cover more than about 380 px, so the near ground is fine and the horizon
  coarse. Each leaf is a 16 by 16 grid of quads. Heights come from the
  elevation tiles (`terrain.rs`, terrarium or mapbox packing, PNG or WebP,
  256 or 512 px) through `/api/tiles/<dem>/z/x/y.png`, the proxy the web map
  reads, so its cache serves offline. The imagery is the base layer's own
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
- **Slices.** Done: the camera, the ground with imagery and heights, hillshade,
  the vehicle, home and track in 3D, drag to pan, ctrl or right drag to turn
  and tilt, wheel to zoom, the elevation model list with the operator's own
  models (they are listed in `terrain` and served as the same tiles), the
  elevation credit with the height at the centre, the sky and a haze band at
  the horizon (seen from about 70 degrees of tilt), the ground colour where
  imagery has not come, and the buildings.
- **Buildings.** The full 3D (the panel's switch, kept as `three_d_detail`)
  reads the cells of `/api/buildings/15/x/y.json` for the ground in view,
  nearest first, 36 at most, from zoom 14. Each cell becomes one mesh once the
  terrain under it is loaded. A cell the backend has not got yet
  (`pending`, or `unavailable` offline) is asked for again every 3 s. Walls
  and roofs of a cell go into one mesh, 4000 buildings at most, in the web's slate at 85 percent opacity with each wall
  darker toward its foot. The cells are gzipped as the backend stores them,
  which is why `flate2` is a dependency (already in the build through `png`).
  A building sits on the terrain height at its middle.
- **Switching it on.** `Map::set_three_d`; the rail button calls it.

## The UI base

Everything a page draws that is not its own picture comes from `src/ui/`, so
one change reaches every page: the corner radius and border of a card
(`card.rs`), the focus ring and padding of a text box (`field.rs`), the
rows and the glass of a dropdown (`menu.rs`), the buttons and the select
(`kit.rs`), dialogs (`modal.rs`), hints and hover cards (`popover.rs`), and
the tokens under all of it (`style.rs`: colours of the six themes, radii,
fonts). A new page builds from these and does not paint a box, a list or a
field with `rect_filled` of its own; if it needs one that is missing, it is
added there first.

## Looking like the web interface

Every measure is taken from the CSS with one CSS pixel as one egui point,
so a block here is the size, weight and colour of the element it copies.
The fonts are the web's WOFF2 files and the icons are cut out of its
`lucide.min.js`, both at build time, so there is no second copy of either.
The glass panels are frosted as the CSS frosts them (`backdrop-filter:
blur(24px) saturate(180%)`, then 60 % white, 55 % on the map rail): a paint
callback on the OpenGL renderer copies what is already drawn under the
panel, blurs and saturates it inside the panel's rounded corners
(`src/ui/frost.rs`), and the fill paints over it. The copy is taken before the
panel's shadow, because a browser draws a box shadow outside the box only.

The theme is the config's `theme.name`, as in the web interface, and is read
once at start; the palettes are `themes.css` value for value (the dark
themes share one base, blue and pink and orange change the accent family,
blue also the navigation colour). The frost saturates 190 % and brightens
10 % on the dark themes, and the logo is the white cut there.

The window title comes from `GET /api/version`; the crate carries no
version of its own.

## Toolchain

eframe 0.37 needs Rust 1.95 or newer. `rust-toolchain.toml` pins 1.99 for
this folder only, so the default toolchain and `rust/corvus_link` are left
alone; rustup installs it on first use.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Every rule a block of the bar follows has a test with the web interface's
expected text, including `toFixed` and `Math.round` rounding.
