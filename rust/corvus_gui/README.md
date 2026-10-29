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
cargo run --release -- --page motors --scroll 560     # open on a page (setup, parameters, safety, safety:SENSOR, motors), scrolled
# looking at it without a hand on the mouse
CORVUS_GUI_SHOT=out.png cargo run -- --page motors     # save the window's own pixels after 9 s and quit
CORVUS_GUI_SIZE=700,812 cargo run                      # open at that size, below the usual floor too
CORVUS_GUI_HOVER=600,50 cargo run                      # hold the pointer there (a hover card, a hint)
CORVUS_GUI_CLICK=560,310 cargo run -- --page motors    # press there once, at 6 s (an open menu)
CORVUS_GUI_CLICK="178,472;400,200;enter" cargo run    # several presses, half a second apart; enter is the key
CORVUS_GUI_3D=11.5,47.4,13,200 cargo run                 # open the 3D map, here: lng,lat,zoom[,bearing]; 1 alone keeps the view
CORVUS_GUI_3D_PANEL=1 CORVUS_GUI_3D=1 cargo run         # with the 3D panel (buildings, elevation models) open
```

Locally, `serve.py` runs as a child on a free port. Every way out (window
closed, SIGINT, SIGTERM) sends it SIGTERM, which runs its own teardown, and
SIGKILL if it is still there after 8 s. If the window does not close within
10 s of a signal, the child is killed and the process exits.

## What is here

| | |
| --- | --- |
| `src/backend.rs` | starts and stops `serve.py` |
| `src/http.rs` | a small HTTP/1.1 client, with a read timeout per read so a stream reader can stop |
| `src/sse.rs` | server-sent events |
| `src/telemetry.rs` | the `/api/telemetry` reader thread and the latest state |
| `src/units.rs` | display units from the config's `ui.units`, rounded the way the web interface rounds |
| `src/topbar.rs` | the status bar, word for word the web interface's (`src/js/topbar.js`) |
| `src/map.rs` | the map (walkers) on the backend's tile proxy, vehicle and home markers |
| `src/maprail.rs` | the rail and the two buttons beside it: zoom, centre, layers, downloaded areas, measure, the clear track disc |
| `src/regions.rs` | the downloaded areas of `/api/tiles/regions`, merged as the web map merges them (`map.js`) |
| `src/measure.rs` | the distance tool: great circle distance, bearing and the two click rule (`map-measure.js`) |
| `src/map_search.rs`, `src/search_card.rs` | the place search: coordinate notations read offline, area names, `/api/geocode`, the box and its list (`map-search.js`) |
| `src/tiles_model.rs`, `src/tiles_card.rs` | the offline map dialog: tile counts and sizes, the download and its jobs, the areas on this laptop (`tiles.js`) |
| `src/camera3d.rs`, `src/map3d.rs`, `src/terrain.rs`, `src/tile_cache.rs`, `src/threed_panel.rs` | the 3D map: camera maths, the tilted ground of imagery and elevation tiles with hillshade and skirts, the elevation model choice, the tile cache behind both, and the 3D panel (`src/js/map.js` 3D mode) |
| `src/track.rs` | the flown track, recorded and smoothed as `src/js/map.js` does |
| `src/hud.rs` | compass, attitude indicator and readouts (`src/js/instruments.js`) |
| `src/flight.rs` | the flight bar, takeoff card and mode picker (`src/js/app.js`) |
| `src/style.rs` | the design tokens of the six themes (light, light-orange, green, blue, pink, orange) and the type |
| `src/setup.rs` | the Setup page: head, vehicle card, the tile grid and the sub-pages (`src/js/setup.js`) |
| `src/params.rs` | the Parameters page: header actions, download, search, filter, defaults, edit and apply, rows that stack under 721 px (`src/js/setup-parameters.js`) |
| `src/params_dialogs.rs`, `src/params_files.rs` | Export, Import and the reboot confirmation, and the three file formats with the import preview; Import asks for a file path because a native window has no picker without a dependency |
| `src/modal.rs` | the `.modal` card shared by those dialogs |
| `src/term_emu.rs`, `src/term_view.rs` | the terminal: a VT parser (the `vte` crate) feeding a cell grid with colours, scroll regions, the alternate screen and 5000 lines of scrollback, drawn with keyboard, wheel, selection and paste (`src/js/ssh-term.js`) |
| `src/ssh_session.rs`, `src/ssh_window.rs` | one SSH session over `/api/ssh/stream` with ordered keystrokes, and the floating terminal window (`src/js/term-window.js`); the SSH tab holds the same terminal |
| `src/motors.rs` | the Motors page: the airframe drawing, the motor table with output assignment, the motor test, frame size, protocol forms (`src/js/setup-motors.js`) |
| `src/safety.rs` | the Safety & Sensors page: the backend's description in two stacks, the landing preset, the sensor tiles, Check values and the checklist card (`src/js/setup-safety.js`) |
| `src/sensors.rs` | the sensor model and its rules: the write chain of a switch, the presets, the extra parameters kept in `~/.corvus/safety-extra.json` |
| `src/sensor_view.rs` | a sensor page drawn: Status card, Hardware card with the write table, the parameters you add by name |
| `src/check.rs` | Check values: read back, write again what did not stick, the result card (`src/js/setup-shared.js`) |
| `src/geofence.rs` | the geofence area the station keeps, its geometry (corners, crossing, area, framing) and the one shared copy (`src/js/geofence.js`) |
| `src/geofence_card.rs` | the Geofence card: the small map, drawing, dragging and adding corners, upload and removal, the large editor (`src/js/setup-geofence.js`) |
| `src/landing.rs` | the Landing card: the descent as one bar, the fields beside each stretch, the brackets with their lengths (`landingCard` of `src/js/setup-safety.js`) |
| `src/responsive.rs` | the width classes of the web (`data-vw`): what folds, shrinks or stacks at 1279, 959, 900, 720 and 600 px (`src/css/responsive.css`) |
| `src/popover.rs` | the hint icon and the sheet it opens, the cards of the top bar: hover intent, pinning, one at a time, placement (`Corvus.ui.popover`, `infoHint`) |
| `src/topbar_cards.rs` | the cards behind GPS, Battery, Altitude, Groundspeed and Vertical speed (`src/js/topbar.js`) |
| `src/checklist_window.rs` | the checklist's window on the Home map: ticks kept by item text, drag and fold, the warning on arming with items open (`src/js/checklist.js`) |
| `src/checklist.rs` | the preflight checklists: config block, Setup card and the dialog the lists are written in (`src/js/checklist-editor.js`) |
| `src/schema_form.rs` | the schema-driven form the backend describes (enum, number, sign, bitmask), shared by the Safety and Motors pages (`src/js/setup-shared.js`) |
| `src/pagekit.rs` | page furniture: header, section title, tile, back button, segmented control |
| `src/notify.rs` | the notification board, its read and dismiss rules, the command dedupe and the toasts (`src/js/topbar.js`) |
| `src/notify_ui.rs` | the NOTIFICATIONS pill, the board and the toast stack |
| `src/frost.rs` | the frosted glass under every `.glass` surface |
| `src/fonts.rs`, `src/icons.rs`, `build.rs` | Inter, JetBrains Mono and the Lucide icons, from the web's own files |

## The 3D map

`walkers` draws a flat map only, so 3D mode (`src/js/map.js` `set3D`) has a
renderer of its own, drawn with egui meshes and no GPU code of its own.

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
- **Order.** egui has no depth buffer. Leaves are drawn far to near, and the
  quads inside a leaf far to near, which is right for a height field seen from
  above the ground and wrong only for the camera inside a valley wall.
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
  elevation credit with the height at the centre. Still to do: buildings, the
  sky and haze, and the rail button that opens the panel (`maprail.rs`).
- **Switching it on.** `Map::set_three_d`; the rail button calls it.

## Looking like the web interface

Every measure is taken from the CSS with one CSS pixel as one egui point,
so a block here is the size, weight and colour of the element it copies.
The fonts are the web's WOFF2 files and the icons are cut out of its
`lucide.min.js`, both at build time, so there is no second copy of either.
The glass panels are frosted as the CSS frosts them (`backdrop-filter:
blur(24px) saturate(180%)`, then 60 % white, 55 % on the map rail): a paint
callback on the OpenGL renderer copies what is already drawn under the
panel, blurs and saturates it inside the panel's rounded corners
(`src/frost.rs`), and the fill paints over it. The copy is taken before the
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
